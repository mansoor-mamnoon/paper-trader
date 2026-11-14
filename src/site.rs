//! The dashboard's data: one JSON file with the account, the latest desk report, the
//! newsbook, the journal and the trades, plus one transcript file per day.

use crate::agents::CATALYSTS;
use crate::config::{
    FEE, MAX_POSITIONS, MIN_DOLLAR_VOLUME, MIN_MARKET_CAP, MIN_PRICE, SLIP, START_CASH,
};
use crate::models::{Agent, MODELS, model_for};
use crate::sectors::SECTORS;
use crate::store::{Store, days_before};
use anyhow::Result;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

fn write_atomic(target: &Path, body: &str) -> Result<()> {
    let temporary = target.with_extension("tmp");
    fs::write(&temporary, body)?;
    fs::rename(temporary, target)?;
    Ok(())
}

/// Write `api/state.json` and today's `api/calls/<day>.json` under `output`.
pub fn export(store: &Store, output: &str) -> Result<Value> {
    let root = Path::new(output);
    fs::create_dir_all(root.join("api/calls"))?;
    let state = store.load()?;
    let reports = store.reports(30)?;
    let latest: Value = reports
        .first()
        .and_then(|(_, body)| serde_json::from_str(body).ok())
        .unwrap_or(Value::Null);
    let day = latest["day"].as_str().unwrap_or("").to_string();
    let since = days_before(if day.is_empty() { "2026-01-01" } else { &day }, 30);
    let equity = state.equity();
    let benchmark = state.benchmark_value.unwrap_or(START_CASH);
    let days: Vec<Value> = reports
        .iter()
        .filter_map(|(day, body)| {
            let report: Value = serde_json::from_str(body).ok()?;
            Some(json!({
                "day": day,
                "focus": report["focus"],
                "note": report["note"],
                "decisions": report["decisions"],
                "funnel": report["funnel"],
                "problems": report["problems"],
                "metrics": report["metrics"],
            }))
        })
        .collect();
    let body = json!({
        "generated_at": crate::store::now(),
        "account": {
            "equity": equity,
            "cash": state.cash,
            "start": START_CASH,
            "return_pct": (equity / START_CASH - 1.0) * 100.0,
            "benchmark": benchmark,
            "benchmark_return_pct": (benchmark / START_CASH - 1.0) * 100.0,
            "realized_pnl": state.realized_pnl,
            "fees": state.fees,
            "peak": state.peak,
            "max_drawdown": state.max_drawdown,
            "started_at": state.started_at,
            "last_at": state.last_at,
            "health": state.health,
            "engine": state.engine,
        },
        "positions": state.positions.values().collect::<Vec<_>>(),
        "pending": state.pending,
        "equity": store.equity_curve()?,
        "latest": latest,
        "days": days,
        "newsbook": store.newsbook(&since)?,
        "journal": store.journal_since(&since)?,
        "trades": store.trades(100)?,
        "record": store.catalyst_record()?,
        "fills": store.fills(200)?,
        "events": store.events(30)?,
        "desk": {
            "agents": Agent::ALL.iter().map(|a| json!({
                "name": a.name(), "job": a.job(), "model": model_for(*a).name,
            })).collect::<Vec<_>>(),
            "models": MODELS,
            "sectors": SECTORS.iter().map(|s| json!({"id": s.id, "name": s.name})).collect::<Vec<_>>(),
            "catalysts": CATALYSTS.iter().map(|(l, m)| json!({"label": l, "meaning": m})).collect::<Vec<_>>(),
        },
        "rules": {
            "max_positions": MAX_POSITIONS, "fee": FEE, "slippage": SLIP,
            "min_market_cap": MIN_MARKET_CAP, "min_price": MIN_PRICE,
            "min_dollar_volume": MIN_DOLLAR_VOLUME, "start_cash": START_CASH,
        },
    });
    write_atomic(&root.join("api/state.json"), &serde_json::to_string(&body)?)?;
    if !day.is_empty() {
        let calls = store.calls(&day)?;
        if !calls.is_empty() {
            write_atomic(
                &root.join(format!("api/calls/{day}.json")),
                &serde_json::to_string(&calls)?,
            )?;
        }
    }
    Ok(json!({"equity": equity, "cash": state.cash, "day": day,
              "positions": state.positions.len(), "pending": state.pending.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_account_exports() {
        let store = Store::open(":memory:").unwrap();
        let dir = std::env::temp_dir().join(format!("pt-site-{}", std::process::id()));
        let summary = export(&store, dir.to_str().unwrap()).unwrap();
        assert_eq!(summary["positions"], 0);
        let body: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("api/state.json")).unwrap()).unwrap();
        assert_eq!(body["account"]["equity"], START_CASH);
        assert_eq!(
            body["desk"]["agents"].as_array().unwrap().len(),
            Agent::ALL.len()
        );
        let _ = fs::remove_dir_all(dir);
    }
}
