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

/// New York wall-clock time after which the day's regular session counts as completed.
/// The close is 16:00 (13:00 on early-close days); the extra half hour lets the daily
/// bar settle on the delayed feed.
pub const SESSION_SETTLED: (u32, u32) = (16, 30);

/// The first New York date whose session is not yet completed at `now`: today before
/// 16:30 ET, tomorrow from 16:30 ET on. An evening run therefore sees that day's bar.
pub fn first_open_day(now: DateTime<Utc>) -> chrono::NaiveDate {
    use chrono::Timelike;
    let local = now.with_timezone(&New_York);
    let today = local.date_naive();
    if (local.hour(), local.minute()) >= SESSION_SETTLED {
        today.succ_opt().unwrap_or(today)
    } else {
        today
    }
}

/// When the most recent weekday session settled (16:30 ET) at or before `now`. A run that
/// already happened after this instant has seen every completed session there is.
/// Holidays count as sessions here; an extra run on one only finds no new bar.
pub fn last_settled(now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::{Datelike, TimeZone, Weekday};
    let mut day = now.with_timezone(&New_York).date_naive();
    loop {
        let weekday = !matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
        let settled = day
            .and_hms_opt(SESSION_SETTLED.0, SESSION_SETTLED.1, 0)
            .and_then(|t| New_York.from_local_datetime(&t).earliest())
            .map(|t| t.with_timezone(&Utc));
        if let Some(settled) = settled.filter(|t| weekday && *t <= now) {
            return settled;
        }
        day = day.pred_opt().unwrap_or(day);
    }
}

/// When the session `sessions` weekdays before the latest settled one settled: 0 gives
/// [`last_settled`], 4 the start of a five-session window.
pub fn settled_sessions_back(now: DateTime<Utc>, sessions: usize) -> DateTime<Utc> {
    let mut at = last_settled(now);
    for _ in 0..sessions {
        at = last_settled(at - chrono::Duration::seconds(1));
    }
    at
}

/// Turn a Yahoo chart payload into completed daily bars and corporate actions. Bars dated
/// on or after `open_day` (see [`first_open_day`]) are still trading and are dropped.
pub fn parse_history(payload: &Value, open_day: chrono::NaiveDate) -> Result<Quote> {
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
        if day >= open_day {
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
    let meta = &result["meta"];
    let text = |key: &str| meta[key].as_str().map(str::to_string);
    Ok(Quote {
        bars,
        actions,
        name: text("longName").or_else(|| text("shortName")),
        instrument_type: text("instrumentType"),
    })
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
    parse_history(&payload, first_open_day(Utc::now()))
}

/// Scan a rotating slice of the directory, always including account symbols and SPY.
pub fn observe(state: &State, batch_size: usize, extra: &BTreeSet<String>) -> Result<Observation> {
    let listing = universe()?;
    let symbols: Vec<&String> = listing.keys().collect();
    let cursor = state.scan_cursor as usize % symbols.len();
    let count = batch_size.min(symbols.len());
    let mut selected: BTreeSet<String> = (0..count)
        .map(|o| symbols[(cursor + o) % symbols.len()].clone())
        .collect();
    selected.extend(state.positions.keys().cloned());
    selected.extend(state.pending.keys().cloned());
    selected.extend(extra.iter().cloned());
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
        intel: crate::model::Intel::default(),
        securities: BTreeMap::new(),
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

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn session_counts_as_completed_from_half_past_four_new_york() {
        let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
        // Winter (EST, UTC-5): 16:29 ET is still Monday's session, 16:30 ET is not.
        assert_eq!(
            first_open_day(utc("2026-11-09T21:29:00Z")),
            day(2026, 11, 9)
        );
        assert_eq!(
            first_open_day(utc("2026-11-09T21:30:00Z")),
            day(2026, 11, 10)
        );
        // The scheduled triggers (17:37, 19:47 and 22:23 ET) all see Monday's bar.
        assert_eq!(
            first_open_day(utc("2026-11-09T22:37:00Z")),
            day(2026, 11, 10)
        );
        assert_eq!(
            first_open_day(utc("2026-11-10T03:23:00Z")),
            day(2026, 11, 10)
        );
        // After midnight ET nothing changes: the new day is not completed yet.
        assert_eq!(
            first_open_day(utc("2026-11-10T05:30:00Z")),
            day(2026, 11, 10)
        );
        // Summer (EDT, UTC-4): 16:30 ET is 20:30 UTC.
        assert_eq!(first_open_day(utc("2026-07-01T20:29:00Z")), day(2026, 7, 1));
        assert_eq!(first_open_day(utc("2026-07-01T20:30:00Z")), day(2026, 7, 2));
    }

    #[test]
    fn last_settled_is_the_latest_weekday_half_past_four() {
        // Monday evening: Monday's session.
        assert_eq!(
            last_settled(utc("2026-11-09T22:37:00Z")),
            utc("2026-11-09T21:30:00Z")
        );
        // Tuesday 03:00 ET, before Tuesday settles: still Monday's.
        assert_eq!(
            last_settled(utc("2026-11-10T08:00:00Z")),
            utc("2026-11-09T21:30:00Z")
        );
        // Sunday: Friday's.
        assert_eq!(
            last_settled(utc("2026-11-08T18:00:00Z")),
            utc("2026-11-06T21:30:00Z")
        );
        // Exactly at the cutoff counts; summer time is 20:30 UTC.
        assert_eq!(
            last_settled(utc("2026-07-01T20:30:00Z")),
            utc("2026-07-01T20:30:00Z")
        );
    }

    #[test]
    fn five_session_window_skips_the_weekend() {
        // Tuesday evening: Tue, Mon, Fri, Thu, Wed; the window opens Wednesday.
        assert_eq!(
            settled_sessions_back(utc("2026-11-10T23:00:00Z"), 4),
            utc("2026-11-04T21:30:00Z")
        );
        assert_eq!(
            settled_sessions_back(utc("2026-11-10T23:00:00Z"), 0),
            last_settled(utc("2026-11-10T23:00:00Z"))
        );
    }

    #[test]
    fn evening_run_keeps_the_just_closed_session() {
        // Daily bars stamped at the 09:30 ET open, Monday 2026-11-09 last.
        let monday_open = utc("2026-11-09T14:30:00Z").timestamp() as f64;
        let n = 60;
        let stamps: Vec<f64> = (0..n)
            .map(|i| monday_open - 86400.0 * (n - 1 - i) as f64)
            .collect();
        let payload = json!({"chart": {"error": null, "result": [{
            "meta": {"currency": "USD"},
            "timestamp": stamps,
            "indicators": {"quote": [{"open": vec![10.0; n], "close": vec![10.0; n], "volume": vec![5.0; n]}]}
        }]}});
        let evening = parse_history(&payload, first_open_day(utc("2026-11-09T22:37:00Z"))).unwrap();
        assert_eq!(evening.bars.last().unwrap().at, monday_open);
        let midday = parse_history(&payload, first_open_day(utc("2026-11-09T17:00:00Z"))).unwrap();
        assert!(midday.bars.last().unwrap().at < monday_open);
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
