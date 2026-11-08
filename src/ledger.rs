//! Transactional SQLite account and append-only trading evidence. The state is one JSON body
//! and everything else is append-only.

use crate::model::State;
use anyhow::{Result, bail};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

pub fn connect(path: &str) -> Result<Connection> {
    if path != ":memory:"
        && let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(30))?;
    let _: String = db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    db.execute_batch(
        "PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS fills (
            id INTEGER PRIMARY KEY, at REAL, signal_at REAL, symbol TEXT, side TEXT,
            quantity REAL, price REAL, notional REAL, fee REAL, realized_pnl REAL, reason TEXT);
        CREATE TABLE IF NOT EXISTS decisions (
            id INTEGER PRIMARY KEY, at REAL, symbol TEXT, action TEXT, reason TEXT, evidence TEXT);
        CREATE TABLE IF NOT EXISTS equity (
            id INTEGER PRIMARY KEY, at REAL UNIQUE, equity REAL, cash REAL,
            benchmark REAL, drawdown REAL, planned_risk REAL);
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY, at REAL, kind TEXT, message TEXT);
        CREATE TABLE IF NOT EXISTS picks (
            id INTEGER PRIMARY KEY, at REAL, symbol TEXT, kind TEXT, rank INTEGER,
            conviction REAL, price REAL, spy_price REAL, UNIQUE(at, symbol, kind));
        CREATE TABLE IF NOT EXISTS forward (
            pick_id INTEGER, horizon INTEGER, ret REAL, spy_ret REAL,
            PRIMARY KEY (pick_id, horizon));",
    )?;
    db.execute(
        "INSERT OR IGNORE INTO state VALUES (1, ?)",
        params![serde_json::to_string(&State::default())?],
    )?;
    Ok(db)
}

pub fn begin(db: &Connection) -> Result<()> {
    db.execute_batch("BEGIN IMMEDIATE")?;
    Ok(())
}

pub fn commit(db: &Connection) -> Result<()> {
    db.execute_batch("COMMIT")?;
    Ok(())
}

pub fn rollback(db: &Connection) -> Result<()> {
    db.execute_batch("ROLLBACK")?;
    Ok(())
}

pub fn load(db: &Connection) -> Result<State> {
    let body: String = db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
    Ok(serde_json::from_str(&body)?)
}

pub fn save(db: &Connection, state: &State) -> Result<()> {
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        params![serde_json::to_string(state)?],
    )?;
    Ok(())
}

pub fn event(db: &Connection, at: f64, kind: &str, message: &str) -> Result<()> {
    db.execute(
        "INSERT INTO events(at,kind,message) VALUES(?,?,?)",
        params![at, kind, message],
    )?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
pub fn decision(
    db: &Connection,
    at: f64,
    symbol: &str,
    action: &str,
    reason: &str,
    evidence: Value,
) -> Result<()> {
    db.execute(
        "INSERT INTO decisions(at,symbol,action,reason,evidence) VALUES(?,?,?,?,?)",
        params![
            at,
            symbol,
            action,
            reason,
            serde_json::to_string(&evidence)?
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn insert_fill(
    db: &Connection,
    at: f64,
    signal_at: f64,
    symbol: &str,
    side: &str,
    quantity: f64,
    price: f64,
    fee: f64,
    pnl: f64,
    reason: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO fills(at,signal_at,symbol,side,quantity,price,notional,fee,realized_pnl,reason) \
         VALUES(?,?,?,?,?,?,?,?,?,?)",
        params![at, signal_at, symbol, side, quantity, price, quantity * price, fee, pnl, reason],
    )?;
    Ok(())
}

/// Reserve a database for one data source before recording any state.
pub fn reserve(path: &str, mode: &str, allowed: &[&str], message: &str) -> Result<()> {
    if !allowed.contains(&mode) {
        bail!("Unknown account mode");
    }
    let db = connect(path)?;
    begin(&db)?;
    let mut state = load(&db)?;
    if state.mode.as_deref().is_some_and(|m| m != mode) {
        bail!("{message}");
    }
    state.mode = Some(mode.to_string());
    save(&db, &state)?;
    commit(&db)
}

/// Record a failed cycle while keeping cash, positions and valuation.
pub fn failure(path: &str, message: &str) -> Result<()> {
    let db = connect(path)?;
    begin(&db)?;
    let mut state = load(&db)?;
    let at = now();
    state.health = json!({"status": "error", "at": at, "message": message});
    event(&db, at, "error", message)?;
    save(&db, &state)?;
    commit(&db)
}

/// Run a query and return each row as a JSON object keyed by column name.
pub fn rows(db: &Connection, sql: &str) -> Result<Vec<Value>> {
    let mut stmt = db.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .map(std::string::ToString::to_string)
        .collect();
    let mut out = Vec::new();
    let mut cursor = stmt.query([])?;
    while let Some(row) = cursor.next()? {
        let mut object = Map::new();
        for (i, name) in names.iter().enumerate() {
            object.insert(name.clone(), value_ref(row.get_ref(i)?));
        }
        out.push(Value::Object(object));
    }
    Ok(out)
}

pub fn value_ref(v: ValueRef) -> Value {
    match v {
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
    }
}

/// Counts of symbols that passed or failed the entry screen in the latest scan.
fn last_scan(db: &Connection) -> Result<Value> {
    let counts = db.query_row(
        "SELECT COALESCE(SUM(action='CANDIDATE'),0), COALESCE(SUM(action='PASS'),0) \
         FROM decisions WHERE at = (SELECT MAX(at) FROM decisions \
         WHERE action IN ('PASS','CANDIDATE'))",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )?;
    Ok(json!({"candidates": counts.0, "rejected": counts.1}))
}

/// Consistent read-only dashboard view and lifetime performance.
pub fn report(path: &str) -> Result<Value> {
    let db = connect(path)?;
    db.execute_batch("BEGIN")?;
    let state = load(&db)?;
    let history = rows(&db, "SELECT * FROM equity ORDER BY id")?;
    let latest = history.last().cloned().unwrap_or_else(|| {
        json!({"equity": 1000, "cash": 1000, "benchmark": 1000, "drawdown": 0, "planned_risk": 0})
    });
    let mut account = Map::new();
    let state_value = serde_json::to_value(&state)?;
    for key in [
        "cash",
        "started_at",
        "last_at",
        "peak",
        "max_drawdown",
        "halted",
        "halt_reason",
        "realized_pnl",
        "fees",
        "mode",
    ] {
        account.insert(key.into(), state_value[key].clone());
    }
    for (k, v) in latest.as_object().unwrap() {
        account.insert(k.clone(), v.clone());
    }
    let equity = latest["equity"].as_f64().unwrap_or(1000.0);
    let benchmark = latest["benchmark"].as_f64().unwrap_or(1000.0);
    account.insert("return_pct".into(), json!((equity / 1000.0 - 1.0) * 100.0));
    account.insert(
        "benchmark_return_pct".into(),
        json!((benchmark / 1000.0 - 1.0) * 100.0),
    );
    account.insert(
        "unrealized_pnl".into(),
        json!(equity - 1000.0 - state.realized_pnl),
    );
    account.insert(
        "end_at".into(),
        json!(state.started_at.unwrap_or_else(now) + 60.0 * 86400.0),
    );
    let mut health = state.health.clone();
    let age = (now() - state.last_at).max(0.0);
    health["age_seconds"] = json!(age);
    if health["status"] == "ok" && age > 900.0 {
        health["status"] = json!("stale");
    }
    let step = (history.len() / 600).max(1);
    let mut sampled: Vec<Value> = history.iter().step_by(step).cloned().collect();
    if let (Some(last), Some(tail)) = (history.last(), sampled.last())
        && last != tail
    {
        sampled.push(last.clone());
    }
    let report = json!({
        "account": account,
        "positions": state.positions.values().collect::<Vec<_>>(),
        "pending": state.pending,
        "health": health,
        "history": sampled,
        "fills": rows(&db, "SELECT * FROM fills ORDER BY id DESC LIMIT 200")?,
        "decisions": rows(&db, "SELECT * FROM decisions ORDER BY id DESC LIMIT 120")?,
        "events": rows(&db, "SELECT * FROM events ORDER BY id DESC LIMIT 30")?,
        "activity": rows(
            &db,
            "SELECT * FROM decisions WHERE action NOT IN ('PASS','CANDIDATE','SKIP') \
             ORDER BY id DESC LIMIT 100",
        )?,
        "last_scan": last_scan(&db)?,
        "rules": {
            "starting_cash": 1000, "position_cap": 0.25, "trade_risk": 0.01,
            "aggregate_risk": 0.03, "drawdown_halt": 0.10, "fee": 0.004, "slippage": 0.001,
            "stop": 0.04, "take_profit": 0.08, "cycle_seconds": 300, "duration_days": 60,
        },
    });
    db.execute_batch("COMMIT")?;
    Ok(report)
}
