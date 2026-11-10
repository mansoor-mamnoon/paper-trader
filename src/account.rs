//! Isolated stock paper ledger: fractional shares, delayed daily fills and explicit evidence.

use crate::analyst::{Brief, Candidate, Review};
use crate::ledger::{self, decision, insert_fill};
use crate::model::{Observation, Order, Position, Quote, State};
use crate::scorecard;
use crate::strategy::{MAX_POSITIONS, POSITION_CAP, Strategy};
use anyhow::{Result, anyhow, bail};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const FEE: f64 = 0.001;
pub const SLIP: f64 = 0.002;
pub const DAY: f64 = 86400.0;
/// Smallest order, in dollars. Shares are fractional, so this only stops dust trades.
pub const MIN_ORDER: f64 = 1.0;

pub fn reserve(path: &str, mode: &str) -> Result<()> {
    ledger::reserve(
        path,
        mode,
        &["stocks-daily", "stocks-replay"],
        "Stock live/replay and crypto accounts require separate databases",
    )
}

pub fn equity(state: &State) -> f64 {
    state.cash
        + state
            .positions
            .values()
            .map(|p| p.market_value)
            .sum::<f64>()
}

/// Track the peak and the drawdown from it using only marks available at the execution or
/// observation time. The drawdown is reported, never used to stop trading: the account keeps
/// trading until the experiment ends.
pub fn update_risk(state: &mut State) -> (f64, f64) {
    let total = equity(state);
    state.peak = state.peak.max(total);
    let drawdown = 1.0 - total / state.peak;
    state.max_drawdown = state.max_drawdown.max(drawdown);
    (total, drawdown)
}

/// Adjust held quantities for splits and credit cash distributions once.
fn corporate_actions(
    db: &Connection,
    state: &mut State,
    symbol: &str,
    quote: &Quote,
    at: f64,
) -> Result<()> {
    let mut actions = quote.actions.clone();
    // Splits apply before per-post-split-share distributions on the same ex-date.
    actions.sort_by(|a, b| {
        (a.date, a.kind != "splits", &a.kind)
            .partial_cmp(&(b.date, b.kind != "splits", &b.kind))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for action in actions {
        let key = format!("{}:{}:{}", symbol, action.kind, action.date);
        let opened_at = state.positions[symbol].opened_at;
        if state.corporate_actions.contains(&key) || !(opened_at < action.date && action.date <= at)
        {
            continue;
        }

        let reason = if action.kind == "splits" {
            let ratio =
                action.numerator.unwrap_or(f64::NAN) / action.denominator.unwrap_or(f64::NAN);
            if !ratio.is_finite() || ratio <= 0.0 {
                bail!("Invalid split ratio");
            }
            let position = state.positions.get_mut(symbol).unwrap();
            position.quantity *= ratio;
            position.entry /= ratio;
            position.stop /= ratio;
            position.target /= ratio;
            format!("Reported split applied, ratio {ratio}")
        } else if action.kind == "dividends" {
            let amount = state.positions[symbol].quantity * action.amount.unwrap_or(f64::NAN);
            if !amount.is_finite() || amount < 0.0 {
                bail!("Invalid cash distribution");
            }
            state.cash += amount;
            state.realized_pnl += amount;
            format!("Reported distribution credited on ex-date, not payment date: ${amount:.4}")
        } else {
            continue;
        };
        state.corporate_actions.push(key);
        decision(
            db,
            at,
            symbol,
            "CORPORATE_ACTION",
            &reason,
            serde_json::to_value(&action)?,
        )?;
    }
    Ok(())
}

/// Execute a prior decision at a later completed bar open, with adverse costs.
fn fill(
    db: &Connection,
    state: &mut State,
    symbol: &str,
    order: &Order,
    bar_at: f64,
    bar_open: f64,
    at: f64,
    strategy: &Strategy,
) -> Result<()> {
    let side = order.side.as_str();
    let price = bar_open
        * if side == "BUY" {
            1.0 + SLIP
        } else {
            1.0 - SLIP
        };
    let quantity;
    let pnl;
    if side == "BUY" {
        let capital = equity(state);
        let budget = state
            .cash
            .min(capital * POSITION_CAP * order.size_fraction.clamp(0.25, 1.0));
        let shares = budget / (price * (1.0 + FEE));
        if state.halted || state.positions.len() >= MAX_POSITIONS || budget < MIN_ORDER {
            decision(
                db,
                at,
                symbol,
                "REJECT",
                "Experiment ended, position limit reached, or less than $1 of cash available",
                json!({}),
            )?;
            return Ok(());
        }
        quantity = shares;
        let cost = quantity * price * (1.0 + FEE);
        state.cash -= cost;
        state.positions.insert(
            symbol.to_string(),
            Position {
                symbol: symbol.to_string(),
                quantity,
                entry: price,
                cost,
                stop: price * (1.0 - strategy.stop_pct),
                target: price * (1.0 + strategy.target_pct),
                opened_at: bar_at,
                planned_risk: cost
                    - quantity * price * (1.0 - strategy.stop_pct) * (1.0 - SLIP) * (1.0 - FEE),
                market_value: quantity * price * (1.0 - SLIP) * (1.0 - FEE),
                unrealized_pnl: 0.0,
                price_at: None,
                mark: None,
            },
        );
        pnl = 0.0;
    } else {
        let position = state
            .positions
            .remove(symbol)
            .ok_or_else(|| anyhow!("Sell order for a symbol that is not held"))?;
        quantity = position.quantity;
        let proceeds = quantity * price * (1.0 - FEE);
        pnl = proceeds - position.cost;
        state.cash += proceeds;
        state.realized_pnl += pnl;
        state.cooldown.insert(symbol.to_string(), at + 5.0 * DAY);
    }
    let fee = quantity * price * FEE;
    state.fees += fee;
    insert_fill(
        db,
        bar_at,
        order.at,
        symbol,
        side,
        quantity,
        price,
        fee,
        pnl,
        &order.reason,
    )?;
    decision(
        db,
        at,
        symbol,
        &format!("{side}_FILLED"),
        "Daily simulation: latest completed session open after signal, plus adverse costs; \
         missed runs skip sessions",
        json!({"bar_at": bar_at, "price": price, "signal_at": order.at}),
    )
}

fn validate_bars(quote: &Quote, at: f64) -> Result<()> {
    let bars = &quote.bars;
    let bad = bars.len() < 50
        || bars.iter().any(|b| {
            ![b.at, b.open, b.close, b.volume]
                .iter()
                .all(|v| v.is_finite())
                || b.open.min(b.close) <= 0.0
                || b.volume < 0.0
                || b.at >= at
        })
        || bars.windows(2).any(|w| w[0].at >= w[1].at);
    if bad {
        bail!("Invalid daily bars");
    }
    Ok(())
}

/// Apply one recorded observation atomically; retries cannot duplicate orders or fills.
pub fn cycle(path: &str, observation: &Observation, mode: &str) -> Result<()> {
    cycle_with(path, observation, mode, &Strategy::default(), None)
}

/// Apply one observation under `strategy`. With a `review`, buys follow the analyst's
/// ranking (restricted to stocks that passed the screen); without one they follow
/// 20-day momentum.
pub fn cycle_with(
    path: &str,
    observation: &Observation,
    mode: &str,
    strategy: &Strategy,
    review: Option<&Review>,
) -> Result<()> {
    reserve(path, mode)?;
    let at = observation.at;
    if !at.is_finite() || at <= 0.0 {
        bail!("Invalid observation timestamp");
    }
    let db = ledger::connect(path)?;
    ledger::begin(&db)?;
    let mut state = ledger::load(&db)?;
    if at <= state.last_at {
        return ledger::rollback(&db);
    }
    let mut usable: BTreeMap<&str, &Quote> = BTreeMap::new();
    for (symbol, quote) in &observation.quotes {
        validate_bars(quote, at)?;
        if at - quote.bars.last().unwrap().at > 5.0 * DAY {
            decision(
                &db,
                at,
                symbol,
                "SKIP",
                "Daily bar older than five days",
                json!({}),
            )?;
            continue;
        }
        usable.insert(symbol, quote);
    }
    if state.started_at.is_none() {
        state.started_at = Some(at);
    }
    if at - state.started_at.unwrap() >= 60.0 * DAY {
        state.halted = true;
        state.halt_reason = "60-day experiment ended".into();
    }
    // Missing held marks freeze entries; absent or delisted stocks are not valued at zero.
    let missing: BTreeSet<String> = state
        .positions
        .keys()
        .filter(|s| !usable.contains_key(s.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        decision(
            &db,
            at,
            "ACCOUNT",
            "FREEZE",
            "Held symbols unavailable; retained last marks",
            json!({"symbols": missing}),
        )?;
    }
    let mut executions = Vec::new();
    for (symbol, order) in state.pending.clone() {
        let Some(quote) = usable.get(symbol.as_str()) else {
            continue;
        };
        // The first session after the signal, so a missed run does not move the fill to a
        // later open than the one the order was queued for.
        if let Some(bar) = quote.bars.iter().find(|b| b.at > order.at) {
            executions.push((bar.at, symbol, order, bar.clone()));
        }
    }
    executions.sort_by(|a, b| (a.0, &a.1).partial_cmp(&(b.0, &b.1)).unwrap());
    for (execution_at, symbol, order, bar) in executions {
        state.pending.remove(&symbol);
        let mut unavailable = BTreeSet::new();
        for held in state.positions.keys().cloned().collect::<Vec<_>>() {
            let opening = usable
                .get(held.as_str())
                .and_then(|q| q.bars.iter().find(|b| b.at == execution_at));
            let Some(opening) = opening else {
                unavailable.insert(held);
                continue;
            };
            corporate_actions(&db, &mut state, &held, usable[held.as_str()], execution_at)?;
            let position = state.positions.get_mut(&held).unwrap();
            position.market_value = position.quantity * opening.open * (1.0 - SLIP) * (1.0 - FEE);
        }
        update_risk(&mut state);
        if at - order.at > 7.0 * DAY || (order.side == "BUY" && !unavailable.is_empty()) {
            decision(
                &db,
                at,
                &symbol,
                "EXPIRED",
                "Order stale or held prices unavailable",
                json!({}),
            )?;
            continue;
        }
        fill(
            &db, &mut state, &symbol, &order, bar.at, bar.open, at, strategy,
        )?;
    }
    for symbol in state.positions.keys().cloned().collect::<Vec<_>>() {
        let Some(quote) = usable.get(symbol.as_str()) else {
            continue;
        };
        let last = quote.bars.last().unwrap();
        corporate_actions(&db, &mut state, &symbol, quote, last.at)?;
        let position = state.positions.get_mut(&symbol).unwrap();
        position.market_value = position.quantity * last.close * (1.0 - SLIP) * (1.0 - FEE);
        position.unrealized_pnl = position.market_value - position.cost;
        position.price_at = Some(last.at);
    }
    let (total, drawdown) = update_risk(&mut state);
    for (symbol, position) in state.positions.clone() {
        let Some(quote) = usable.get(symbol.as_str()) else {
            continue;
        };
        if state.pending.contains_key(&symbol) {
            continue;
        }
        let close = quote.bars.last().unwrap().close;
        if state.halted || close <= position.stop || close >= position.target {
            let reason =
                "Close crossed stop/target or the experiment ended; later observed session open";
            state.pending.insert(
                symbol.clone(),
                Order {
                    side: "SELL".into(),
                    at,
                    reason: reason.into(),
                    size_fraction: 1.0,
                },
            );
            decision(&db, at, &symbol, "SELL_SIGNAL", reason, json!({}))?;
        }
    }
    if let Some(review) = review {
        for call in &review.doctor {
            let Some(position) = state.positions.get(&call.symbol).cloned() else {
                continue;
            };
            let Some(quote) = usable.get(call.symbol.as_str()) else {
                continue;
            };
            let close = quote.bars.last().unwrap().close;
            let evidence = json!({"assessment": call.assessment, "stop_pct": call.stop_pct});
            match call.action.as_str() {
                "exit" if !state.pending.contains_key(&call.symbol) => {
                    let reason = format!("Position doctor: {}", call.assessment);
                    state.pending.insert(
                        call.symbol.clone(),
                        Order {
                            side: "SELL".into(),
                            at,
                            reason: reason.clone(),
                            size_fraction: 1.0,
                        },
                    );
                    decision(&db, at, &call.symbol, "DOCTOR_EXIT", &reason, evidence)?;
                }
                "tighten" => {
                    // The doctor may only raise a stop toward the price, never widen it.
                    let proposed = close * (1.0 - call.stop_pct.clamp(0.01, 0.10));
                    if proposed > position.stop && proposed < close * 0.995 {
                        if let Some(held) = state.positions.get_mut(&call.symbol) {
                            held.stop = proposed;
                        }
                        decision(
                            &db,
                            at,
                            &call.symbol,
                            "DOCTOR_TIGHTEN",
                            &call.assessment,
                            evidence,
                        )?;
                    } else {
                        decision(
                            &db,
                            at,
                            &call.symbol,
                            "DOCTOR_HOLD",
                            &call.assessment,
                            evidence,
                        )?;
                    }
                }
                _ => decision(
                    &db,
                    at,
                    &call.symbol,
                    "DOCTOR_HOLD",
                    &call.assessment,
                    evidence,
                )?,
            }
        }
    }
    let screened = screen(strategy, observation, at);
    for row in &screened {
        decision(
            &db,
            at,
            &row.candidate.symbol,
            if row.eligible { "CANDIDATE" } else { "PASS" },
            &row.reason,
            row.evidence.clone(),
        )?;
    }
    let eligible: BTreeMap<&str, &Screened> = screened
        .iter()
        .filter(|r| r.eligible)
        .map(|r| (r.candidate.symbol.as_str(), r))
        .collect();
    let queued_buys = state.pending.values().filter(|o| o.side == "BUY").count();
    let mut slots = MAX_POSITIONS.saturating_sub(state.positions.len() + queued_buys);
    // A risk-off call can only reduce exposure: at most one new position.
    if review.and_then(|r| r.regime.as_deref()) == Some("risk_off") {
        slots = slots.min(1);
    }
    let mut ordered: Vec<scorecard::PickRow> = Vec::new();
    if !state.halted && missing.is_empty() {
        // (symbol, conviction, thesis) in the order buys are attempted.
        let ranked: Vec<(String, f64, String)> = if let Some(review) = review {
            for avoided in &review.avoid {
                decision(
                    &db,
                    at,
                    &avoided.symbol,
                    "AVOID",
                    &avoided.reason,
                    json!({"source": review.source}),
                )?;
            }
            review
                .picks
                .iter()
                .filter(|p| {
                    eligible.contains_key(p.symbol.as_str())
                        && !review.avoid.iter().any(|a| a.symbol == p.symbol)
                })
                .map(|p| (p.symbol.clone(), p.conviction, p.thesis.clone()))
                .collect()
        } else {
            let mut by_momentum: Vec<(f64, &str)> = eligible
                .values()
                .map(|r| (r.candidate.momentum_20d, r.candidate.symbol.as_str()))
                .collect();
            by_momentum.sort_by(|a, b| b.partial_cmp(a).unwrap());
            by_momentum
                .into_iter()
                .map(|(_, s)| (s.to_string(), 0.0, String::new()))
                .collect()
        };
        for (rank, (symbol, conviction, thesis)) in ranked.into_iter().enumerate() {
            if slots == 0 {
                break;
            }
            if state.positions.contains_key(&symbol)
                || state.pending.contains_key(&symbol)
                || state.cooldown.get(&symbol).copied().unwrap_or(0.0) > at
            {
                continue;
            }
            let risk_off = review.and_then(|r| r.regime.as_deref()) == Some("risk_off");
            // Slots are equal. A risk-off call is the only thing that shrinks a position.
            let size_fraction = if risk_off { 0.5 } else { 1.0 };
            let reason = if thesis.is_empty() {
                "Positive liquid daily trend; position capped at 20% of equity".to_string()
            } else {
                format!("Analyst conviction {conviction:.2}: {thesis}")
            };
            let candidate = &eligible[symbol.as_str()].candidate;
            let evidence = json!({
                "source": review.map_or("quant", |r| r.source.as_str()),
                "conviction": conviction,
                "thesis": thesis,
                "earnings_date": candidate.earnings_date,
                "headlines": candidate.headlines.iter().map(|h| h.title.clone()).collect::<Vec<_>>(),
            });
            state.pending.insert(
                symbol.clone(),
                Order {
                    side: "BUY".into(),
                    at,
                    reason: reason.clone(),
                    size_fraction,
                },
            );
            decision(&db, at, &symbol, "BUY_SIGNAL", &reason, evidence)?;
            ordered.push(scorecard::PickRow {
                symbol: symbol.clone(),
                kind: "pick",
                rank: rank as i64 + 1,
                conviction,
                price: candidate.close,
            });
            slots -= 1;
        }
    }
    if let Some(review) = review {
        let spy_price = usable
            .get("SPY")
            .and_then(|q| q.bars.last())
            .map(|b| b.close);
        let mut rows = ordered.clone();
        for avoided in &review.avoid {
            if let Some(r) = eligible.get(avoided.symbol.as_str()) {
                rows.push(scorecard::PickRow {
                    symbol: avoided.symbol.clone(),
                    kind: "veto",
                    rank: 0,
                    conviction: 0.0,
                    price: r.candidate.close,
                });
            }
        }
        let taken: BTreeSet<String> = rows.iter().map(|r| r.symbol.clone()).collect();
        let mut rest: Vec<&Screened> = eligible
            .values()
            .copied()
            .filter(|r| !taken.contains(&r.candidate.symbol))
            .collect();
        rest.sort_by(|a, b| {
            b.candidate
                .momentum_20d
                .partial_cmp(&a.candidate.momentum_20d)
                .unwrap()
        });
        rows.extend(rest.into_iter().take(40).map(|r| scorecard::PickRow {
            symbol: r.candidate.symbol.clone(),
            kind: "candidate",
            rank: 0,
            conviction: 0.0,
            price: r.candidate.close,
        }));
        scorecard::record(&db, at, spy_price, &rows)?;
    }
    scorecard::update(&db, observation)?;
    let mut benchmark = 1000.0;
    if let Some(spy) = usable.get("SPY") {
        let price = spy.bars.last().unwrap().close;
        if state.benchmark_quantity == 0.0 {
            state.benchmark_quantity = 1000.0 / price;
        }
        benchmark = state.benchmark_quantity * price;
    } else if let Some(value) = state.benchmark_value.filter(|v| *v != 0.0) {
        benchmark = value;
    }
    state.benchmark_value = Some(benchmark);
    let risk: f64 = state.positions.values().map(|p| p.planned_risk).sum();
    db.execute(
        "INSERT INTO equity(at,equity,cash,benchmark,drawdown,planned_risk) VALUES(?,?,?,?,?,?)",
        rusqlite::params![at, total, state.cash, benchmark, drawdown, risk],
    )?;
    state.last_at = at;
    state.scan_cursor = observation.next_cursor;
    let mut coverage = observation.coverage.clone();
    for symbol in observation.quotes.keys().chain(observation.errors.keys()) {
        state.scan_seen.insert(symbol.clone(), at);
    }
    let listed: BTreeSet<&str> = observation
        .universe
        .iter()
        .filter_map(|r| r["symbol"].as_str())
        .collect();
    let attempted_ever = listed
        .iter()
        .filter(|s| state.scan_seen.contains_key(**s))
        .count();
    coverage.insert("attempted_ever".into(), json!(attempted_ever));
    coverage.insert(
        "never_attempted".into(),
        json!(listed.len() - attempted_ever),
    );
    coverage.insert("usable".into(), json!(usable.len()));
    state.coverage = coverage;
    state.universe.clone_from(&observation.universe);
    state.data_errors = observation.errors.clone();
    let status = if usable.is_empty() {
        "error"
    } else if !missing.is_empty() {
        "degraded"
    } else {
        "delayed"
    };
    state.health = json!({
        "status": status,
        "at": at,
        "message": format!(
            "Daily-price simulation, not live quotes. Prior completed sessions only. {}{} usable prices; {} provider errors.",
            if missing.is_empty() { "" } else { "Some holdings lack current marks. " },
            usable.len(),
            observation.errors.len()
        ),
    });
    ledger::event(
        &db,
        at,
        "stock_scan",
        &serde_json::to_string(&state.coverage)?,
    )?;
    ledger::save(&db, &state)?;
    ledger::commit(&db)
}

/// One stock's result against a strategy's screen.
#[derive(Clone, Debug)]
pub struct Screened {
    pub candidate: Candidate,
    pub eligible: bool,
    pub reason: String,
    pub evidence: Value,
}

fn utc_date(at: f64) -> chrono::NaiveDate {
    chrono::DateTime::from_timestamp(at as i64, 0)
        .unwrap_or_default()
        .date_naive()
}

/// Apply `strategy`'s screen to every stock with a recent, complete history.
pub fn screen(strategy: &Strategy, observation: &Observation, at: f64) -> Vec<Screened> {
    let today = utc_date(at);
    let mut rows = Vec::new();
    for (symbol, quote) in &observation.quotes {
        let bars = &quote.bars;
        let n = bars.len();
        if n < 50 || at - bars[n - 1].at > 5.0 * DAY {
            continue;
        }
        let close = bars[n - 1].close;
        let mean = |k: usize| bars[n - k..].iter().map(|b| b.close).sum::<f64>() / k as f64;
        let (sma20, sma50) = (mean(20), mean(50));
        let momentum = close / bars[n - 21].close - 1.0;
        let dollar_volume = bars[n - 20..]
            .iter()
            .map(|b| b.close * b.volume)
            .sum::<f64>()
            / 20.0;
        let momentum_5d = close / bars[n - 6].close - 1.0;
        let returns: Vec<f64> = bars[n - 21..]
            .windows(2)
            .map(|w| w[1].close / w[0].close - 1.0)
            .collect();
        let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
        let volatility = (returns
            .iter()
            .map(|r| (r - mean_return).powi(2))
            .sum::<f64>()
            / returns.len() as f64)
            .sqrt();
        let max_gap = bars[n - 20..]
            .iter()
            .zip(&bars[n - 21..])
            .map(|(today, yesterday)| (today.open / yesterday.close - 1.0).abs())
            .fold(0.0, f64::max);
        let earnings_date = observation.intel.earnings.get(symbol).cloned();
        let blackout = earnings_date.as_deref().filter(|date| {
            let days = date
                .parse::<chrono::NaiveDate>()
                .map(|d| (d - today).num_days());
            strategy.earnings_blackout_days > 0
                && days.is_ok_and(|d| (0..=i64::from(strategy.earnings_blackout_days)).contains(&d))
        });
        let trend = !strategy.require_trend || (close > sma20 && sma20 > sma50);
        let eligible = close >= strategy.min_price
            && dollar_volume >= strategy.min_dollar_volume
            && momentum > strategy.min_momentum
            && trend
            && blackout.is_none();
        let reason = match blackout {
            Some(date) => format!(
                "Earnings report on {date} falls inside the {}-day blackout",
                strategy.earnings_blackout_days
            ),
            None => format!(
                "{}20-day momentum above {:.0}%, price >= ${}, liquidity >= ${}M/day",
                if strategy.require_trend {
                    "20/50-day trend, "
                } else {
                    ""
                },
                strategy.min_momentum * 100.0,
                strategy.min_price,
                strategy.min_dollar_volume / 1e6
            ),
        };
        rows.push(Screened {
            candidate: Candidate {
                symbol: symbol.clone(),
                close,
                sma20,
                sma50,
                momentum_20d: momentum,
                avg_dollar_volume: dollar_volume,
                earnings_date: earnings_date.clone(),
                headlines: observation
                    .intel
                    .news
                    .get(symbol)
                    .cloned()
                    .unwrap_or_default(),
                momentum_5d,
                volatility_20d: volatility,
                max_gap_20d: max_gap,
            },
            eligible,
            reason,
            evidence: json!({
                "sma20": sma20, "sma50": sma50, "momentum20d": momentum,
                "average_dollar_volume": dollar_volume, "price_at": bars[n - 1].at,
                "earnings_date": earnings_date,
            }),
        });
    }
    rows
}

/// What the analyst is shown: the best screened stocks this account could still buy.
pub fn brief(strategy: &Strategy, observation: &Observation, state: &State) -> Brief {
    let at = observation.at;
    let mut candidates: Vec<Candidate> = screen(strategy, observation, at)
        .into_iter()
        .filter(|r| {
            let symbol = r.candidate.symbol.as_str();
            r.eligible
                && !state.positions.contains_key(symbol)
                && !state.pending.contains_key(symbol)
                && state.cooldown.get(symbol).copied().unwrap_or(0.0) <= at
        })
        .map(|r| r.candidate)
        .collect();
    candidates.sort_by(|a, b| b.momentum_20d.partial_cmp(&a.momentum_20d).unwrap());
    candidates.truncate(strategy.shortlist_size);
    let queued_buys = state.pending.values().filter(|o| o.side == "BUY").count();
    Brief {
        date: utc_date(at).to_string(),
        equity: equity(state),
        cash: state.cash,
        open_slots: if state.halted {
            0
        } else {
            MAX_POSITIONS.saturating_sub(state.positions.len() + queued_buys)
        },
        held: state.positions.keys().cloned().collect(),
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, Quote};

    const T0: f64 = 1_700_000_000.0;

    fn bars(count: usize, start: f64, step: f64, last_open: Option<f64>) -> Vec<Bar> {
        (0..count)
            .map(|i| {
                let close = start + step * i as f64;
                let open = if i == count - 1 {
                    last_open.unwrap_or(close)
                } else {
                    close
                };
                Bar {
                    at: T0 + DAY * i as f64,
                    open,
                    close,
                    volume: 1_000_000.0,
                }
            })
            .collect()
    }

    fn observation(at: f64, quotes: Vec<(&str, Vec<Bar>)>) -> Observation {
        Observation {
            at,
            universe: quotes.iter().map(|(s, _)| json!({"symbol": s})).collect(),
            quotes: quotes
                .into_iter()
                .map(|(s, b)| {
                    (
                        s.to_string(),
                        Quote {
                            bars: b,
                            actions: vec![],
                        },
                    )
                })
                .collect(),
            errors: BTreeMap::new(),
            next_cursor: 0,
            coverage: serde_json::Map::new(),
            intel: crate::model::Intel::default(),
        }
    }

    fn temp_db(name: &str) -> String {
        let path = std::env::temp_dir().join(format!("pt-{name}-{}.sqlite", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
        path.to_string_lossy().into_owned()
    }

    fn snapshot(path: &str) -> State {
        let db = ledger::connect(path).unwrap();
        ledger::load(&db).unwrap()
    }

    #[test]
    fn rising_stock_is_queued_then_filled_at_next_open() {
        let path = temp_db("fill");
        let series = bars(60, 50.0, 1.0, None);
        let at = series.last().unwrap().at + DAY * 0.5;
        cycle(
            &path,
            &observation(at, vec![("UP", series.clone())]),
            "stocks-daily",
        )
        .unwrap();
        let state = snapshot(&path);
        assert_eq!(state.pending["UP"].side, "BUY");
        assert_eq!(state.positions.len(), 0);
        // The next session opens at 120.
        let mut longer = series;
        longer.push(Bar {
            at: T0 + DAY * 60.0,
            open: 120.0,
            close: 121.0,
            volume: 1_000_000.0,
        });
        cycle(
            &path,
            &observation(T0 + DAY * 61.5, vec![("UP", longer)]),
            "stocks-daily",
        )
        .unwrap();
        let state = snapshot(&path);
        let position = &state.positions["UP"];
        let price = 120.0 * (1.0 + SLIP);
        // Fractional shares: the whole 20% budget is invested, not a rounded-down share count.
        assert!(
            (position.cost - 200.0).abs() < 1e-6,
            "cost {}",
            position.cost
        );
        assert!(position.quantity.fract() > 0.0);
        assert!((position.entry - price).abs() < 1e-9);
        assert!((position.stop - price * 0.96).abs() < 1e-9);
        assert!(state.cash < 1000.0 - price);
    }

    #[test]
    fn missed_run_still_fills_at_the_first_open_after_the_signal() {
        let path = temp_db("fill-late");
        let series = bars(60, 50.0, 1.0, None);
        let at = series.last().unwrap().at + DAY * 0.5;
        cycle(
            &path,
            &observation(at, vec![("UP", series.clone())]),
            "stocks-daily",
        )
        .unwrap();
        // The next run arrives two sessions later: it must fill at 120, not at 125.
        let mut longer = series;
        for (i, open) in [(60.0, 120.0), (61.0, 125.0)] {
            longer.push(Bar {
                at: T0 + DAY * i,
                open,
                close: open + 1.0,
                volume: 1_000_000.0,
            });
        }
        cycle(
            &path,
            &observation(T0 + DAY * 62.5, vec![("UP", longer)]),
            "stocks-daily",
        )
        .unwrap();
        let state = snapshot(&path);
        let entry = state.positions["UP"].entry;
        assert!((entry - 120.0 * (1.0 + SLIP)).abs() < 1e-9, "entry {entry}");
    }

    #[test]
    fn five_slots_of_twenty_percent_invest_the_whole_account() {
        let path = temp_db("full");
        let names = ["AAA", "BBB", "CCC", "DDD", "EEE", "FFF"];
        let series = bars(60, 50.0, 1.0, None);
        let at = series.last().unwrap().at + DAY * 0.5;
        let quotes = |extra: Option<Bar>| -> Vec<(&str, Vec<Bar>)> {
            names
                .iter()
                .map(|n| {
                    let mut b = series.clone();
                    b.extend(extra.clone());
                    (*n, b)
                })
                .collect()
        };
        cycle(&path, &observation(at, quotes(None)), "stocks-daily").unwrap();
        assert_eq!(snapshot(&path).pending.len(), MAX_POSITIONS);
        let next = Bar {
            at: T0 + DAY * 60.0,
            open: 120.0,
            close: 121.0,
            volume: 1_000_000.0,
        };
        cycle(
            &path,
            &observation(T0 + DAY * 61.5, quotes(Some(next))),
            "stocks-daily",
        )
        .unwrap();
        let state = snapshot(&path);
        assert_eq!(state.positions.len(), MAX_POSITIONS);
        let invested = 1.0 - state.cash / equity(&state);
        assert!(invested > 0.98, "only {invested:.3} invested");
    }

    #[test]
    fn falling_stock_is_never_a_candidate() {
        let path = temp_db("pass");
        let series = bars(60, 150.0, -1.0, None);
        let at = series.last().unwrap().at + DAY * 0.5;
        cycle(
            &path,
            &observation(at, vec![("DOWN", series)]),
            "stocks-daily",
        )
        .unwrap();
        let state = snapshot(&path);
        assert_eq!(state.pending.len(), 0);
        let db = ledger::connect(&path).unwrap();
        let action: String = db
            .query_row("SELECT action FROM decisions LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(action, "PASS");
    }

    #[test]
    fn repeated_observation_time_is_inert() {
        let path = temp_db("dup");
        let series = bars(60, 50.0, 1.0, None);
        let at = series.last().unwrap().at + DAY * 0.5;
        let obs = observation(at, vec![("UP", series)]);
        cycle(&path, &obs, "stocks-daily").unwrap();
        let db = ledger::connect(&path).unwrap();
        let count = |db: &Connection| -> i64 {
            db.query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0))
                .unwrap()
        };
        let before = count(&db);
        cycle(&path, &obs, "stocks-daily").unwrap();
        assert_eq!(before, count(&db));
    }

    #[test]
    fn stop_crossing_queues_a_sell() {
        let path = temp_db("stop");
        let series = bars(60, 50.0, 1.0, None);
        cycle(
            &path,
            &observation(
                series.last().unwrap().at + DAY * 0.5,
                vec![("UP", series.clone())],
            ),
            "stocks-daily",
        )
        .unwrap();
        let mut next = series;
        next.push(Bar {
            at: T0 + DAY * 60.0,
            open: 100.0,
            close: 101.0,
            volume: 1_000_000.0,
        });
        cycle(
            &path,
            &observation(T0 + DAY * 61.5, vec![("UP", next.clone())]),
            "stocks-daily",
        )
        .unwrap();
        next.push(Bar {
            at: T0 + DAY * 61.0,
            open: 90.0,
            close: 80.0,
            volume: 1_000_000.0,
        });
        cycle(
            &path,
            &observation(T0 + DAY * 62.5, vec![("UP", next)]),
            "stocks-daily",
        )
        .unwrap();
        let state = snapshot(&path);
        assert_eq!(state.pending["UP"].side, "SELL");
    }

    #[test]
    fn rejects_malformed_bars() {
        let path = temp_db("bad");
        let mut series = bars(60, 50.0, 1.0, None);
        series[10].close = -1.0;
        let at = series.last().unwrap().at + DAY * 0.5;
        assert!(
            cycle(
                &path,
                &observation(at, vec![("BAD", series)]),
                "stocks-daily"
            )
            .is_err()
        );
    }

    #[test]
    fn drawdown_is_reported_but_never_halts_trading() {
        let mut state = State {
            cash: 1000.0,
            ..State::default()
        };
        update_risk(&mut state);
        state.cash = 700.0;
        let (_, drawdown) = update_risk(&mut state);
        assert!((drawdown - 0.30).abs() < 1e-9);
        assert!(!state.halted);
        assert!((state.max_drawdown - 0.30).abs() < 1e-9);
    }

    fn held_position(path: &str) -> (Vec<Bar>, f64) {
        let series = bars(60, 50.0, 1.0, None);
        cycle(
            path,
            &observation(
                series.last().unwrap().at + DAY * 0.5,
                vec![("UP", series.clone())],
            ),
            "stocks-daily",
        )
        .unwrap();
        let mut next = series;
        next.push(Bar {
            at: T0 + DAY * 60.0,
            open: 100.0,
            close: 105.0,
            volume: 1_000_000.0,
        });
        let at = T0 + DAY * 61.5;
        cycle(
            path,
            &observation(at, vec![("UP", next.clone())]),
            "stocks-daily",
        )
        .unwrap();
        assert!(snapshot(path).positions.contains_key("UP"));
        (next, at)
    }

    fn doctor(action: &str, stop_pct: f64) -> Review {
        Review {
            doctor: vec![crate::analyst::DoctorCall {
                symbol: "UP".into(),
                action: action.into(),
                assessment: "test".into(),
                stop_pct,
            }],
            source: "llm".into(),
            ..Review::default()
        }
    }

    #[test]
    fn doctor_tightens_a_stop_but_never_widens_it() {
        let path = temp_db("doctor-tighten");
        let (mut series, at) = held_position(&path);
        let original = snapshot(&path).positions["UP"].stop;
        series.push(Bar {
            at: T0 + DAY * 61.0,
            open: 106.0,
            close: 106.0,
            volume: 1_000_000.0,
        });
        let next_at = at + DAY;
        cycle_with(
            &path,
            &observation(next_at, vec![("UP", series.clone())]),
            "stocks-daily",
            &Strategy::default(),
            Some(&doctor("tighten", 0.03)),
        )
        .unwrap();
        let tightened = snapshot(&path).positions["UP"].stop;
        assert!(tightened > original, "{tightened} vs {original}");
        series.push(Bar {
            at: T0 + DAY * 62.0,
            open: 106.0,
            close: 106.0,
            volume: 1_000_000.0,
        });
        cycle_with(
            &path,
            &observation(next_at + DAY, vec![("UP", series)]),
            "stocks-daily",
            &Strategy::default(),
            Some(&doctor("tighten", 0.10)),
        )
        .unwrap();
        assert!((snapshot(&path).positions["UP"].stop - tightened).abs() < 1e-9);
    }

    #[test]
    fn doctor_exit_queues_a_sell() {
        let path = temp_db("doctor-exit");
        let (mut series, at) = held_position(&path);
        series.push(Bar {
            at: T0 + DAY * 61.0,
            open: 106.0,
            close: 106.0,
            volume: 1_000_000.0,
        });
        cycle_with(
            &path,
            &observation(at + DAY, vec![("UP", series)]),
            "stocks-daily",
            &Strategy::default(),
            Some(&doctor("exit", 0.0)),
        )
        .unwrap();
        let state = snapshot(&path);
        assert_eq!(state.pending["UP"].side, "SELL");
        assert!(state.pending["UP"].reason.starts_with("Position doctor"));
    }

    #[test]
    fn risk_off_allows_one_new_position_at_half_size() {
        let path = temp_db("risk-off");
        let quotes: Vec<(&str, Vec<Bar>)> = vec![
            ("AAA", bars(60, 50.0, 1.0, None)),
            ("BBB", bars(60, 40.0, 1.0, None)),
            ("CCC", bars(60, 30.0, 1.0, None)),
        ];
        let at = quotes[0].1.last().unwrap().at + DAY * 0.5;
        let pick = |s: &str, c: f64| crate::analyst::Pick {
            symbol: s.into(),
            conviction: c,
            ..crate::analyst::Pick::default()
        };
        let review = Review {
            picks: vec![pick("AAA", 0.9), pick("BBB", 0.8), pick("CCC", 0.7)],
            regime: Some("risk_off".into()),
            source: "llm".into(),
            ..Review::default()
        };
        cycle_with(
            &path,
            &observation(at, quotes),
            "stocks-daily",
            &Strategy::default(),
            Some(&review),
        )
        .unwrap();
        let state = snapshot(&path);
        assert_eq!(state.pending.len(), 1);
        assert!((state.pending["AAA"].size_fraction - 0.5).abs() < 1e-9);
    }

    #[test]
    fn every_opinion_is_recorded_for_the_scorecard() {
        let path = temp_db("scorecard");
        let quotes: Vec<(&str, Vec<Bar>)> = vec![
            ("AAA", bars(60, 50.0, 1.0, None)),
            ("BBB", bars(60, 40.0, 1.0, None)),
            ("CCC", bars(60, 30.0, 1.0, None)),
        ];
        let at = quotes[0].1.last().unwrap().at + DAY * 0.5;
        let review = Review {
            picks: vec![crate::analyst::Pick {
                symbol: "AAA".into(),
                conviction: 0.9,
                ..crate::analyst::Pick::default()
            }],
            avoid: vec![crate::analyst::Avoid {
                symbol: "BBB".into(),
                reason: "weak".into(),
            }],
            source: "llm".into(),
            ..Review::default()
        };
        cycle_with(
            &path,
            &observation(at, quotes),
            "stocks-daily",
            &Strategy::default(),
            Some(&review),
        )
        .unwrap();
        let db = ledger::connect(&path).unwrap();
        let kinds: Vec<(String, String)> = {
            let mut stmt = db
                .prepare("SELECT symbol, kind FROM picks ORDER BY symbol")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            kinds,
            vec![
                ("AAA".into(), "pick".into()),
                ("BBB".into(), "veto".into()),
                ("CCC".into(), "candidate".into())
            ]
        );
    }
}
