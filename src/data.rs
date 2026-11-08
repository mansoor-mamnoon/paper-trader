//! Free US exchange directory and unofficial, delayed Yahoo daily price observations.

use crate::ledger;
use crate::model::{Action, Bar, Observation, Quote, State};
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use chrono_tz::America::New_York;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

const DIRECTORY: &str = "https://www.nasdaqtrader.com/dynamic/SymDir/";
pub const SOURCE: &str = "Nasdaq symbol directory; Yahoo Finance unofficial daily chart endpoint";

pub fn fetch(url: &str) -> Result<String> {
    crate::http::get(
        url,
        "Mozilla/5.0 (paper-trader research)",
        Duration::from_secs(20),
    )
}

/// Parse one pipe-delimited Nasdaq directory file into symbol records.
pub fn parse_directory(text: &str, key: &str, into: &mut BTreeMap<String, Value>) -> Result<()> {
    let mut lines = text.lines();
    let header: Vec<&str> = lines
        .next()
        .ok_or_else(|| anyhow!("Invalid Nasdaq directory header"))?
        .split('|')
        .collect();
    let column = |name: &str| header.iter().position(|h| h.trim() == name);
    let key_index = column(key).ok_or_else(|| anyhow!("Invalid Nasdaq directory header"))?;
    for line in lines {
        let cells: Vec<&str> = line.split('|').collect();
        let get = |name: &str| column(name).and_then(|i| cells.get(i)).map(|s| s.trim());
        let symbol = cells.get(key_index).map_or("", |s| s.trim());
        if symbol.is_empty()
            || symbol.starts_with("File Creation")
            || get("Test Issue") != Some("N")
        {
            continue;
        }
        into.insert(
            symbol.to_string(),
            json!({
                "symbol": symbol,
                "name": get("Security Name").unwrap_or(""),
                "exchange": get("Exchange").unwrap_or("Q"),
                "etf": get("ETF") == Some("Y"),
            }),
        );
    }
    Ok(())
}

/// All non-test Nasdaq and other-listed US issues, including ETFs and ADRs.
pub fn universe() -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for (file, key) in [
        ("nasdaqlisted.txt", "Symbol"),
        ("otherlisted.txt", "ACT Symbol"),
    ] {
        parse_directory(&fetch(&format!("{DIRECTORY}{file}"))?, key, &mut result)?;
    }
    if result.len() < 5000 {
        bail!("Incomplete US symbol directory; refusing a truncated universe");
    }
    Ok(result)
}

fn number(item: &Value, key: &str) -> Option<f64> {
    item.get(key).and_then(Value::as_f64)
}

/// Turn a Yahoo chart payload into completed daily bars and corporate actions.
pub fn parse_history(payload: &Value, today: chrono::NaiveDate) -> Result<Quote> {
    let chart = &payload["chart"];
    let result = chart["result"].get(0).filter(|_| chart["error"].is_null());
    let result = result.ok_or_else(|| anyhow!("No daily price history"))?;
    if result["meta"]["currency"] != "USD" {
        bail!("Non-USD security is not eligible");
    }
    let prices = &result["indicators"]["quote"][0];
    let stamps = result["timestamp"].as_array().cloned().unwrap_or_default();
    let mut bars = Vec::new();
    for (i, stamp) in stamps.iter().enumerate() {
        let Some(at) = stamp.as_f64() else { continue };
        let day = DateTime::<Utc>::from_timestamp(at as i64, 0)
            .ok_or_else(|| anyhow!("Bad timestamp"))?
            .with_timezone(&New_York)
            .date_naive();
        if day >= today {
            continue;
        }
        let pick = |k: &str| prices[k].get(i).and_then(Value::as_f64);
        let (Some(open), Some(close), Some(volume)) = (pick("open"), pick("close"), pick("volume"))
        else {
            continue;
        };
        if !(open.is_finite() && close.is_finite() && volume.is_finite())
            || open <= 0.0
            || close <= 0.0
            || volume < 0.0
        {
            continue;
        }
        bars.push(Bar {
            at,
            open,
            close,
            volume,
        });
    }
    if bars.len() < 50 {
        bail!("Fewer than 50 completed daily bars");
    }
    bars.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap());
    let mut actions = Vec::new();
    if let Some(events) = result["events"].as_object() {
        for (kind, records) in events {
            for item in records.as_object().into_iter().flat_map(|m| m.values()) {
                let Some(date) = number(item, "date") else {
                    continue;
                };
                actions.push(Action {
                    kind: kind.clone(),
                    date,
                    amount: number(item, "amount"),
                    numerator: number(item, "numerator"),
                    denominator: number(item, "denominator"),
                });
            }
        }
    }
    Ok(Quote { bars, actions })
}

pub fn history(symbol: &str) -> Result<Quote> {
    let ticker: String = symbol
        .replace('.', "-")
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?range=3mo&interval=1d&events=div%2Csplits"
    );
    let payload: Value = serde_json::from_str(&fetch(&url)?)?;
    parse_history(&payload, Utc::now().with_timezone(&New_York).date_naive())
}

/// Scan a rotating slice of the directory, always including account symbols and SPY.
pub fn observe(state: &State, batch_size: usize) -> Result<Observation> {
    let listing = universe()?;
    let symbols: Vec<&String> = listing.keys().collect();
    let cursor = state.scan_cursor as usize % symbols.len();
    let count = batch_size.min(symbols.len());
    let mut selected: BTreeSet<String> = (0..count)
        .map(|o| symbols[(cursor + o) % symbols.len()].clone())
        .collect();
    selected.extend(state.positions.keys().cloned());
    selected.extend(state.pending.keys().cloned());
    selected.insert("SPY".into());
    let queue = Mutex::new(selected.iter().cloned().collect::<VecDeque<_>>());
    let found = Mutex::new((BTreeMap::new(), BTreeMap::new()));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let Some(symbol) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let outcome = history(&symbol);
                    let mut guard = found.lock().unwrap();
                    match outcome {
                        Ok(quote) => {
                            guard.0.insert(symbol, quote);
                        }
                        Err(error) => {
                            guard.1.insert(symbol, error.to_string());
                        }
                    }
                }
            });
        }
    });
    let (quotes, errors) = found.into_inner().unwrap();
    let mut coverage = Map::new();
    coverage.insert("universe_count".into(), json!(symbols.len()));
    coverage.insert("attempted".into(), json!(selected.len()));
    coverage.insert("succeeded".into(), json!(quotes.len()));
    coverage.insert("failed".into(), json!(errors.len()));
    coverage.insert(
        "remaining".into(),
        json!(symbols.len().saturating_sub(selected.len())),
    );
    coverage.insert("batch_size".into(), json!(count));
    coverage.insert("scan_start_index".into(), json!(cursor));
    coverage.insert(
        "rotation_cycles".into(),
        json!(symbols.len().div_ceil(count.max(1))),
    );
    coverage.insert(
        "scope".into(),
        json!("US exchange-listed issues; not worldwide or OTC"),
    );
    coverage.insert(
        "method".into(),
        json!("Rotating alphabetical scan; positions, pending orders and SPY every run"),
    );
    Ok(Observation {
        at: ledger::now(),
        universe: listing.values().cloned().collect(),
        quotes,
        errors,
        next_cursor: ((cursor + count) % symbols.len()) as u64,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_skips_test_issues_and_footer() {
        let text = "Symbol|Security Name|Test Issue|ETF\nAAA|Alpha Inc|N|N\nTST|Test Co|Y|N\nSPY|SPDR|N|Y\nFile Creation Time: 1|||\n";
        let mut out = BTreeMap::new();
        parse_directory(text, "Symbol", &mut out).unwrap();
        assert_eq!(out.keys().collect::<Vec<_>>(), ["AAA", "SPY"]);
        assert_eq!(out["SPY"]["etf"], true);
        assert_eq!(out["AAA"]["exchange"], "Q");
    }

    #[test]
    fn history_drops_today_and_bad_rows() {
        let day0 = 1_700_000_000.0_f64;
        let n = 60;
        let stamps: Vec<f64> = (0..n).map(|i| day0 + 86400.0 * i as f64).collect();
        let mut open: Vec<Value> = (0..n).map(|_| json!(10.0)).collect();
        open[3] = Value::Null;
        let payload = json!({"chart": {"error": null, "result": [{
            "meta": {"currency": "USD"},
            "timestamp": stamps,
            "indicators": {"quote": [{"open": open, "close": vec![10.0; n], "volume": vec![5.0; n]}]},
            "events": {"splits": {"1": {"date": day0 + 5.0 * 86400.0, "numerator": 2.0, "denominator": 1.0}}}
        }]}});
        let today = DateTime::<Utc>::from_timestamp((day0 + 86400.0 * 59.0) as i64, 0)
            .unwrap()
            .with_timezone(&New_York)
            .date_naive();
        let quote = parse_history(&payload, today).unwrap();
        assert_eq!(quote.bars.len(), 58);
        assert_eq!(quote.actions[0].kind, "splits");
    }

    #[test]
    fn non_usd_is_rejected() {
        let payload = json!({"chart": {"error": null, "result": [{"meta": {"currency": "EUR"}}]}});
        assert!(
            parse_history(
                &payload,
                chrono::NaiveDate::from_ymd_opt(2030, 1, 1).unwrap()
            )
            .is_err()
        );
    }
}
