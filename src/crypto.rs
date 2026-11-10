//! Hourly crypto paper account on public Kraken data. Entries are queued on one
//! observation and filled on a later fresh one.

use crate::ledger;
use crate::model::{Order, Position, State};
use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

pub const FEE: f64 = 0.004;
pub const SLIP: f64 = 0.001;
const PAIRS: [(&str, &str); 4] = [
    ("BTC", "XBTUSD"),
    ("ETH", "ETHUSD"),
    ("SOL", "SOLUSD"),
    ("ADA", "ADAUSD"),
];
const KEYS: [(&str, &str); 4] = [
    ("BTC", "XXBTZUSD"),
    ("ETH", "XETHZUSD"),
    ("SOL", "SOLUSD"),
    ("ADA", "ADAUSD"),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CryptoQuote {
    pub bid: f64,
    pub ask: f64,
    pub trade_at: f64,
    pub closes: Vec<f64>,
    pub candle_end: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub at: f64,
    pub quotes: BTreeMap<String, CryptoQuote>,
}

type Quotes = BTreeMap<String, CryptoQuote>;

fn public_get(endpoint: &str) -> Result<Value> {
    let url = format!("https://api.kraken.com/0/public/{endpoint}");
    let payload: Value = serde_json::from_str(&crate::http::get(
        &url,
        "PaperTradingExperiment/1.0",
        Duration::from_secs(15),
    )?)?;
    if let Some(errors) = payload["error"].as_array()
        && !errors.is_empty()
    {
        let text: Vec<String> = errors
            .iter()
            .map(|e| e.as_str().unwrap_or("").to_string())
            .collect();
        bail!("Kraken: {}", text.join("; "));
    }
    Ok(payload["result"].clone())
}

fn num(v: &Value) -> Result<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| anyhow!("Malformed market observation"))
}

/// Collect fresh bid/ask, recent trade timestamps and closed hourly candles.
pub fn observe() -> Result<Snapshot> {
    let server_at = num(&public_get("Time")?["unixtime"])?;
    if (ledger::now() - server_at).abs() > 90.0 {
        bail!("Exchange clock is stale or local clock is incorrect");
    }
    let mut quotes = Quotes::new();
    for ((symbol, pair), (_, key)) in PAIRS.iter().zip(KEYS.iter()) {
        let ohlc = public_get(&format!("OHLC?pair={pair}&interval=60"))?;
        let bars = ohlc[*key]
            .as_array()
            .ok_or_else(|| anyhow!("{symbol}: missing candles"))?;
        let trades = public_get(&format!("Trades?pair={pair}&count=1"))?;
        let last = trades[*key]
            .as_array()
            .and_then(|t| t.last())
            .ok_or_else(|| anyhow!("{symbol}: missing trades"))?;
        // Kraken always returns the unfinished current candle last. Discard it.
        let end = bars.len().saturating_sub(1);
        let closed = &bars[end.saturating_sub(60)..end];
        let mut contiguous = closed.len() >= 50;
        for pair_rows in closed.windows(2) {
            if num(&pair_rows[1][0])? as i64 - num(&pair_rows[0][0])? as i64 != 3600 {
                contiguous = false;
            }
        }
        if !contiguous {
            bail!("{symbol}: missing hourly candles");
        }
        let closes = closed
            .iter()
            .map(|row| num(&row[4]))
            .collect::<Result<Vec<_>>>()?;
        quotes.insert(
            symbol.to_string(),
            CryptoQuote {
                bid: 0.0,
                ask: 0.0,
                trade_at: num(&last[2])?,
                closes,
                candle_end: num(&closed[closed.len() - 1][0])? + 3600.0,
            },
        );
    }
    let pairs: Vec<&str> = PAIRS.iter().map(|(_, p)| *p).collect();
    let tickers = public_get(&format!("Ticker?pair={}", pairs.join(",")))?;
    for ((symbol, _), (_, key)) in PAIRS.iter().zip(KEYS.iter()) {
        let ticker = &tickers[*key];
        let quote = quotes.get_mut(*symbol).unwrap();
        quote.bid = num(&ticker["b"][0])?;
        quote.ask = num(&ticker["a"][0])?;
    }
    let snapshot = Snapshot {
        at: ledger::now(),
        quotes,
    };
    validate(&snapshot)?;
    Ok(snapshot)
}

/// Reject malformed, incomplete, stale or nonfinite market observations.
pub fn validate(snapshot: &Snapshot) -> Result<()> {
    let at = snapshot.at;
    let expected: Vec<&str> = {
        let mut names: Vec<&str> = PAIRS.iter().map(|(s, _)| *s).collect();
        names.sort_unstable();
        names
    };
    let actual: Vec<&str> = snapshot
        .quotes
        .keys()
        .map(std::string::String::as_str)
        .collect();
    if !at.is_finite() || at <= 0.0 || actual != expected {
        bail!("Invalid observation time or incomplete universe");
    }
    for (symbol, quote) in &snapshot.quotes {
        let prices = [quote.bid, quote.ask]
            .into_iter()
            .chain(quote.closes.iter().copied());
        if prices.into_iter().any(|v| !v.is_finite() || v <= 0.0) {
            bail!("{symbol}: invalid price");
        }
        if quote.bid > quote.ask || quote.ask / quote.bid > 1.01 {
            bail!("{symbol}: crossed or wider than 1% spread");
        }
        if !quote.trade_at.is_finite() || !quote.candle_end.is_finite() {
            bail!("{symbol}: invalid price");
        }
        let age = at - quote.trade_at;
        let candle_age = at - quote.candle_end;
        if !(0.0..=180.0).contains(&age) || !(0.0..=3900.0).contains(&candle_age) {
            bail!("{symbol}: stale or future market data");
        }
        if quote.closes.len() < 50 {
            bail!("{symbol}: fewer than 50 closed candles");
        }
    }
    Ok(())
}

/// Require the 20-hour mean above the 50-hour mean and positive 24-hour momentum.
pub fn signal(quote: &CryptoQuote) -> (bool, Value) {
    let closes = &quote.closes;
    if closes.len() < 50 {
        return (false, json!({}));
    }
    let n = closes.len();
    let fast = closes[n - 20..].iter().sum::<f64>() / 20.0;
    let slow = closes[n - 50..].iter().sum::<f64>() / 50.0;
    let momentum = closes[n - 1] / closes[n - 25] - 1.0;
    (
        fast > slow && momentum > 0.01,
        json!({
            "sma20": fast,
            "sma50": slow,
            "momentum24h": momentum,
            "candle_end": quote.candle_end,
        }),
    )
}

/// Adverse sell fill at the bid less ten basis points.
fn sale_price(quote: &CryptoQuote) -> f64 {
    quote.bid * (1.0 - SLIP)
}

/// Mark positions at conservative net liquidation proceeds.
fn value(state: &State, quotes: &Quotes) -> f64 {
    state.cash
        + state
            .positions
            .iter()
            .map(|(s, p)| p.quantity * sale_price(&quotes[s]) * (1.0 - FEE))
            .sum::<f64>()
}

/// Sum of original entry-to-stop losses, including simulated execution costs.
fn planned_risk(state: &State) -> f64 {
    state.positions.values().map(|p| p.planned_risk).sum()
}

fn sell(
    db: &Connection,
    state: &mut State,
    at: f64,
    symbol: &str,
    quote: &CryptoQuote,
    reason: &str,
    signal_at: f64,
) -> Result<()> {
    let pos = state
        .positions
        .remove(symbol)
        .ok_or_else(|| anyhow!("No open position in {symbol}"))?;
    let price = sale_price(quote);
    let notional = pos.quantity * price;
    let fee = notional * FEE;
    let pnl = notional - fee - pos.cost;
    state.cash += notional - fee;
    state.fees += fee;
    state.realized_pnl += pnl;
    state.cooldown.insert(symbol.to_string(), at + 86400.0);
    state.pending.remove(symbol);
    ledger::insert_fill(
        db,
        at,
        signal_at,
        symbol,
        "SELL",
        pos.quantity,
        price,
        fee,
        pnl,
        reason,
    )?;
    ledger::decision(
        db,
        at,
        symbol,
        "SELL_FILLED",
        reason,
        json!({"bid": quote.bid, "price": price}),
    )
}

/// Size and execute a delayed entry only within all current risk budgets.
fn buy(
    db: &Connection,
    state: &mut State,
    at: f64,
    symbol: &str,
    quote: &CryptoQuote,
    pending_at: f64,
    equity: f64,
) -> Result<()> {
    let price = quote.ask * (1.0 + SLIP);
    let stop = price * 0.96;
    let unit_cost = price * (1.0 + FEE);
    let unit_risk = unit_cost - stop * (1.0 - SLIP) * (1.0 - FEE);
    // Five basis points of equity buffer avoids exhausting the aggregate limit
    // through execution fees and the conservative liquidation mark alone.
    let budget = equity * 0.0095;
    if planned_risk(state) + budget > equity * 0.03 {
        return ledger::decision(
            db,
            at,
            symbol,
            "REJECT",
            "Aggregate planned-risk limit",
            json!({}),
        );
    }
    let quantity = (equity * 0.25 / unit_cost)
        .min(budget / unit_risk)
        .min(state.cash / unit_cost);
    if quantity * unit_cost < 10.0 {
        return ledger::decision(
            db,
            at,
            symbol,
            "REJECT",
            "Less than $10 available under risk limits",
            json!({}),
        );
    }
    let notional = quantity * price;
    let fee = notional * FEE;
    let cost = notional + fee;
    state.cash -= cost;
    state.fees += fee;
    state.positions.insert(
        symbol.to_string(),
        Position {
            symbol: symbol.to_string(),
            quantity,
            entry: price,
            cost,
            stop,
            target: price * 1.08,
            opened_at: at,
            planned_risk: quantity * unit_risk,
            ..Default::default()
        },
    );
    ledger::insert_fill(
        db,
        at,
        pending_at,
        symbol,
        "BUY",
        quantity,
        price,
        fee,
        0.0,
        "Prior closed-candle signal; later fresh observation",
    )?;
    ledger::decision(
        db,
        at,
        symbol,
        "BUY_FILLED",
        "Deferred entry passed risk checks",
        json!({"ask": quote.ask, "trade_at": quote.trade_at, "planned_risk": budget}),
    )
}

fn queue(state: &mut State, symbol: &str, at: f64, side: &str) {
    state.pending.insert(
        symbol.to_string(),
        Order {
            side: side.to_string(),
            at,
            reason: String::new(),
            size_fraction: 1.0,
            industry: String::new(),
        },
    );
}

/// Apply one validated observation atomically; duplicate timestamps are inert.
pub fn cycle(path: &str, snapshot: &Snapshot, mode: &str) -> Result<()> {
    if mode != "live" && mode != "replay" {
        bail!("Unknown account mode");
    }
    validate(snapshot)?;
    let at = snapshot.at;
    let quotes = &snapshot.quotes;
    let db = ledger::connect(path)?;
    ledger::begin(&db)?;
    let mut state = ledger::load(&db)?;
    if state.mode.as_deref().is_some_and(|m| m != mode) {
        bail!("Live and replay data must use separate databases");
    }
    if mode == "live" && !(0.0..=180.0).contains(&(ledger::now() - at)) {
        bail!("Live observation is stale or ahead of the local clock");
    }
    if at <= state.last_at {
        return ledger::rollback(&db);
    }
    state.mode = Some(mode.to_string());
    let started_at = if let Some(started) = state.started_at {
        started
    } else {
        state.started_at = Some(at);
        state.benchmark_quantity = 1000.0 / (quotes["BTC"].ask * (1.0 + SLIP) * (1.0 + FEE));
        ledger::event(
            &db,
            at,
            "start",
            "60-day paper experiment started with $1,000",
        )?;
        at
    };
    let mut equity = value(&state, quotes);
    state.peak = state.peak.max(equity);
    let drawdown = 1.0 - equity / state.peak;
    state.max_drawdown = state.max_drawdown.max(drawdown);
    if !state.halted && (drawdown >= 0.10 || at >= started_at + 60.0 * 86400.0) {
        state.halted = true;
        state.halt_reason = if drawdown >= 0.10 {
            "10% drawdown halt"
        } else {
            "60-day experiment ended"
        }
        .into();
        ledger::event(&db, at, "halt", &state.halt_reason.clone())?;
    }
    let mut exited = false;
    let held: Vec<String> = state.positions.keys().cloned().collect();
    for symbol in held {
        let pos = state.positions[&symbol].clone();
        let quote = &quotes[&symbol];
        let reason = if state.halted {
            state.halt_reason.clone()
        } else if quote.bid <= pos.stop {
            "4% stop crossed; filled at observed bid, not historical stop".into()
        } else if quote.bid >= pos.target {
            "8% take-profit threshold".into()
        } else if at - pos.opened_at >= 7.0 * 86400.0 {
            "Seven-day maximum holding period".into()
        } else if pos.quantity * quote.bid > equity * 0.25 {
            "25% position exposure rebalance".into()
        } else if planned_risk(&state) > equity * 0.03 {
            "3% aggregate planned-risk rebalance".into()
        } else {
            String::new()
        };
        if !reason.is_empty() {
            sell(&db, &mut state, at, &symbol, quote, &reason, pos.opened_at)?;
            exited = true;
            equity = value(&state, quotes);
        }
    }
    let queued: Vec<(String, Order)> = state
        .pending
        .iter()
        .map(|(s, o)| (s.clone(), o.clone()))
        .collect();
    for (symbol, pending) in queued {
        let quote = &quotes[&symbol];
        if state.halted || at - pending.at > 3600.0 {
            state.pending.remove(&symbol);
            ledger::decision(
                &db,
                at,
                &symbol,
                "CANCEL",
                "Halted or signal expired after one hour",
                json!({}),
            )?;
            continue;
        }
        if at - pending.at < 60.0 || quote.trade_at <= pending.at {
            continue;
        }
        state.pending.remove(&symbol);
        if pending.side == "BUY" && exited {
            ledger::decision(
                &db,
                at,
                &symbol,
                "CANCEL",
                "Portfolio exit invalidated queued entry",
                json!({}),
            )?;
        } else if pending.side == "SELL" {
            sell(
                &db,
                &mut state,
                at,
                &symbol,
                quote,
                "Deferred trend exit",
                pending.at,
            )?;
        } else if signal(quote).0 {
            let current = value(&state, quotes);
            buy(&db, &mut state, at, &symbol, quote, pending.at, current)?;
        } else {
            ledger::decision(
                &db,
                at,
                &symbol,
                "CANCEL",
                "Trend no longer qualifies",
                json!({}),
            )?;
        }
    }
    for (symbol, quote) in quotes {
        let holding = state.positions.contains_key(symbol);
        let (qualifies, mut evidence) = signal(quote);
        evidence["bid"] = json!(quote.bid);
        evidence["ask"] = json!(quote.ask);
        evidence["trade_at"] = json!(quote.trade_at);
        if state.halted {
            continue;
        }
        let (action, reason) = if state.pending.contains_key(symbol) {
            ("WAIT", "Waiting for a later fresh observation")
        } else if holding && !qualifies {
            queue(&mut state, symbol, at, "SELL");
            ("SELL", "Trend weakened; queue next-observation exit")
        } else if holding {
            ("HOLD", "Trend intact; standing stop and target active")
        } else if at < state.cooldown.get(symbol).copied().unwrap_or(0.0) {
            ("WAIT", "24-hour cooldown after exit")
        } else if qualifies {
            queue(&mut state, symbol, at, "BUY");
            ("BUY", "20h mean > 50h mean and 24h return > 1%; queue only")
        } else {
            ("WAIT", "Trend filter not satisfied; cash is a position")
        };
        ledger::decision(&db, at, symbol, action, reason, evidence)?;
    }
    equity = value(&state, quotes);
    state.peak = state.peak.max(equity);
    let drawdown = (1.0 - equity / state.peak).max(0.0);
    state.max_drawdown = state.max_drawdown.max(drawdown);
    state.last_at = at;
    state.health = json!({
        "status": if mode == "live" { "ok" } else { "replay" },
        "at": at,
        "message": if mode == "live" {
            "Fresh public Kraken observations"
        } else {
            "Historical replay scenario; not live trading performance"
        },
    });
    for (symbol, pos) in &mut state.positions {
        let quote = &quotes[symbol];
        pos.mark = Some(quote.bid);
        pos.market_value = pos.quantity * sale_price(quote) * (1.0 - FEE);
        pos.unrealized_pnl = pos.market_value - pos.cost;
    }
    let benchmark = state.benchmark_quantity * sale_price(&quotes["BTC"]) * (1.0 - FEE);
    db.execute(
        "INSERT INTO equity(at,equity,cash,benchmark,drawdown,planned_risk) VALUES(?,?,?,?,?,?)",
        params![
            at,
            equity,
            state.cash,
            benchmark,
            drawdown,
            planned_risk(&state)
        ],
    )?;
    ledger::save(&db, &state)?;
    ledger::commit(&db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(at: f64, trend: f64) -> CryptoQuote {
        CryptoQuote {
            bid: 100.0,
            ask: 100.1,
            trade_at: at - 10.0,
            closes: (0..60).map(|i| 100.0 + trend * f64::from(i)).collect(),
            candle_end: at - 100.0,
        }
    }

    fn snapshot(at: f64, trend: f64) -> Snapshot {
        let quotes = PAIRS
            .iter()
            .map(|(s, _)| (s.to_string(), quote(at, trend)))
            .collect();
        Snapshot { at, quotes }
    }

    fn temp_db(name: &str) -> String {
        let path =
            std::env::temp_dir().join(format!("pt-crypto-{name}-{}.sqlite", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path.to_string_lossy().into_owned()
    }

    fn count(path: &str, table: &str) -> i64 {
        let db = ledger::connect(path).unwrap();
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn signal_follows_trend() {
        let (up, evidence) = signal(&quote(1e6, 1.0));
        assert!(up);
        assert!(evidence["momentum24h"].as_f64().unwrap() > 0.01);
        assert!(!signal(&quote(1e6, -1.0)).0);
        assert!(!signal(&quote(1e6, 0.0)).0);
    }

    #[test]
    fn validate_rejects_bad_data() {
        let good = snapshot(1e6, 1.0);
        assert!(validate(&good).is_ok());
        let mut stale = good.clone();
        stale.quotes.get_mut("BTC").unwrap().trade_at = 1e6 - 500.0;
        assert!(validate(&stale).is_err());
        let mut crossed = good.clone();
        crossed.quotes.get_mut("ETH").unwrap().bid = 101.0;
        assert!(validate(&crossed).is_err());
        let mut wide = good.clone();
        wide.quotes.get_mut("ETH").unwrap().ask = 102.0;
        assert!(validate(&wide).is_err());
        let mut missing = good.clone();
        missing.quotes.remove("SOL");
        assert!(validate(&missing).is_err());
        let mut short = good;
        short.quotes.get_mut("ADA").unwrap().closes.truncate(49);
        assert!(validate(&short).is_err());
    }

    #[test]
    fn queued_buy_fills_on_later_observation() {
        let path = temp_db("fill");
        cycle(&path, &snapshot(1_000_000.0, 1.0), "replay").unwrap();
        let db = ledger::connect(&path).unwrap();
        let state = ledger::load(&db).unwrap();
        assert_eq!(state.positions.len(), 0);
        assert_eq!(state.pending["BTC"].side, "BUY");
        assert_eq!(count(&path, "fills"), 0);
        drop(db);
        cycle(&path, &snapshot(1_000_120.0, 1.0), "replay").unwrap();
        let state = ledger::load(&ledger::connect(&path).unwrap()).unwrap();
        assert!(state.positions.contains_key("ADA") || state.positions.contains_key("BTC"));
        assert!(count(&path, "fills") >= 1);
        assert!(state.cash < 1000.0);
        assert_eq!(count(&path, "equity"), 2);
        for pos in state.positions.values() {
            assert!(pos.market_value > 0.0 && pos.mark == Some(100.0));
        }
    }

    #[test]
    fn duplicate_timestamp_is_inert() {
        let path = temp_db("dup");
        let snap = snapshot(1_000_000.0, 1.0);
        cycle(&path, &snap, "replay").unwrap();
        let (decisions, equity) = (count(&path, "decisions"), count(&path, "equity"));
        cycle(&path, &snap, "replay").unwrap();
        assert_eq!(count(&path, "decisions"), decisions);
        assert_eq!(count(&path, "equity"), equity);
    }

    #[test]
    fn modes_cannot_share_a_database() {
        let path = temp_db("mode");
        cycle(&path, &snapshot(1_000_000.0, 1.0), "replay").unwrap();
        let at = ledger::now();
        assert!(cycle(&path, &snapshot(at, 1.0), "live").is_err());
    }
}
