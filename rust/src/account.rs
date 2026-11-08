//! Isolated stock paper ledger: whole shares, delayed daily fills and explicit evidence.

use crate::ledger::{self, decision, insert_fill};
use crate::model::{Observation, Order, Position, Quote, State};
use anyhow::{Result, anyhow, bail};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const FEE: f64 = 0.001;
pub const SLIP: f64 = 0.002;
pub const DAY: f64 = 86400.0;

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

/// Update drawdown using only marks available at the execution or observation time.
pub fn update_risk(state: &mut State) -> (f64, f64) {
    let total = equity(state);
    state.peak = state.peak.max(total);
    let drawdown = 1.0 - total / state.peak;
    state.max_drawdown = state.max_drawdown.max(drawdown);
    if drawdown >= 0.10 {
        state.halted = true;
        state.halt_reason = "10% drawdown halt (gaps can exceed limits)".into();
    }
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
        let reason;
        if action.kind == "splits" {
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
            reason = format!("Reported split applied, ratio {ratio}");
        } else if action.kind == "dividends" {
            let amount = state.positions[symbol].quantity * action.amount.unwrap_or(f64::NAN);
            if !amount.is_finite() || amount < 0.0 {
                bail!("Invalid cash distribution");
            }
            state.cash += amount;
            state.realized_pnl += amount;
            reason = format!(
                "Reported distribution credited on ex-date, not payment date: ${amount:.4}"
            );
        } else {
            continue;
        }
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
        let shares = (state.cash.min(capital * 0.20) / (price * (1.0 + FEE))).floor();
        if state.halted || state.positions.len() >= 3 || shares < 1.0 {
            decision(
                db,
                at,
                symbol,
                "REJECT",
                "Halted, three-position limit, or whole share unaffordable",
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
                stop: price * 0.96,
                target: price * 1.08,
                opened_at: bar_at,
                planned_risk: cost - quantity * price * 0.96 * (1.0 - SLIP) * (1.0 - FEE),
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
        if let Some(bar) = quote.bars.iter().filter(|b| b.at > order.at).last() {
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
        fill(&db, &mut state, &symbol, &order, bar.at, bar.open, at)?;
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
            let reason = "Close crossed stop/target or account halted; later observed session open";
            state.pending.insert(
                symbol.clone(),
                Order {
                    side: "SELL".into(),
                    at,
                    reason: reason.into(),
                },
            );
            decision(&db, at, &symbol, "SELL_SIGNAL", reason, json!({}))?;
        }
    }
    let mut candidates: Vec<(f64, &str)> = Vec::new();
    for (symbol, quote) in &usable {
        let bars = &quote.bars;
        let n = bars.len();
        let close = bars[n - 1].close;
        let mean = |k: usize| bars[n - k..].iter().map(|b| b.close).sum::<f64>() / k as f64;
        let (sma20, sma50) = (mean(20), mean(50));
        let momentum = close / bars[n - 21].close - 1.0;
        let dollar_volume = bars[n - 20..]
            .iter()
            .map(|b| b.close * b.volume)
            .sum::<f64>()
            / 20.0;
        let evidence = json!({
            "sma20": sma20, "sma50": sma50, "momentum20d": momentum,
            "average_dollar_volume": dollar_volume, "price_at": bars[n - 1].at,
        });
        let eligible = close >= 5.0
            && dollar_volume >= 1_000_000.0
            && close > sma20
            && sma20 > sma50
            && momentum > 0.02;
        decision(
            &db,
            at,
            symbol,
            if eligible { "CANDIDATE" } else { "PASS" },
            "20/50-day trend, 20-day momentum, price >= $5, liquidity >= $1M/day",
            evidence,
        )?;
        if eligible {
            candidates.push((momentum, symbol));
        }
    }
    let queued_buys = state.pending.values().filter(|o| o.side == "BUY").count() as i64;
    let mut slots = 3 - state.positions.len() as i64 - queued_buys;
    if !state.halted && missing.is_empty() {
        candidates.sort_by(|a, b| b.partial_cmp(a).unwrap());
        for (_, symbol) in candidates {
            if slots <= 0 {
                break;
            }
            if state.positions.contains_key(symbol)
                || state.pending.contains_key(symbol)
                || state.cooldown.get(symbol).copied().unwrap_or(0.0) > at
            {
                continue;
            }
            let reason = "Positive liquid daily trend; whole shares capped at 20% of equity";
            state.pending.insert(
                symbol.to_string(),
                Order {
                    side: "BUY".into(),
                    at,
                    reason: reason.into(),
                },
            );
            decision(&db, at, symbol, "BUY_SIGNAL", reason, json!({}))?;
            slots -= 1;
        }
    }
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
    state.universe = observation.universe.clone();
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

#[allow(dead_code)]
fn _assert_value(_: Value) {}

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
        assert!(state.positions.is_empty());
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
        assert_eq!(position.quantity, 1.0);
        assert!((position.entry - price).abs() < 1e-9);
        assert!((position.stop - price * 0.96).abs() < 1e-9);
        assert!(state.cash < 1000.0 - price);
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
        assert!(state.pending.is_empty());
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
    fn drawdown_halt_triggers_at_ten_percent() {
        let mut state = State::default();
        state.cash = 890.0;
        let (_, drawdown) = update_risk(&mut state);
        assert!(drawdown >= 0.10 && state.halted);
    }
}
