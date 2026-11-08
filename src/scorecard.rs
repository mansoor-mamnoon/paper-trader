//! Scores every decision the analyst makes, not just the trades that happened.
//!
//! A portfolio makes a handful of trades in two weeks, which is too little to tell skill
//! from luck. But every day the desk also picks, vetoes and passes over dozens of stocks,
//! and each of those has a measurable forward return against SPY. Recording them gives
//! hundreds of samples, and a calibration curve for the model's stated conviction.

use crate::ledger::{self, rows};
use crate::model::Observation;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Trading days after a decision at which its return is measured.
pub const HORIZONS: [usize; 4] = [1, 3, 5, 10];
const DAY: f64 = 86400.0;

/// One stock the desk had an opinion on.
#[derive(Clone, Debug, PartialEq)]
pub struct PickRow {
    pub symbol: String,
    /// `pick` (chosen to buy), `veto` (the analyst said no) or `candidate` (passed the
    /// screen but was not chosen, the control group).
    pub kind: &'static str,
    pub rank: i64,
    pub conviction: f64,
    pub price: f64,
}

/// Store today's opinions. Safe to call twice for the same `at`.
pub fn record(db: &Connection, at: f64, spy_price: Option<f64>, picks: &[PickRow]) -> Result<()> {
    for p in picks {
        db.execute(
            "INSERT OR IGNORE INTO picks(at,symbol,kind,rank,conviction,price,spy_price) VALUES(?,?,?,?,?,?,?)",
            params![at, p.symbol, p.kind, p.rank, p.conviction, p.price, spy_price],
        )?;
    }
    Ok(())
}

/// Symbols whose forward returns are still being measured. The daily scan always
/// includes them so their later prices are available.
pub fn open_symbols(db: &Connection, now: f64) -> Result<BTreeSet<String>> {
    let mut stmt = db.prepare(
        "SELECT DISTINCT symbol FROM picks p WHERE at >= ?1 AND \
         (SELECT COUNT(*) FROM forward f WHERE f.pick_id = p.id) < ?2",
    )?;
    let found = stmt
        .query_map(params![now - 16.0 * DAY, HORIZONS.len() as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(found)
}

fn closes_after(observation: &Observation, symbol: &str, at: f64) -> Vec<f64> {
    observation
        .quotes
        .get(symbol)
        .map(|q| {
            q.bars
                .iter()
                .filter(|b| b.at > at)
                .map(|b| b.close)
                .collect()
        })
        .unwrap_or_default()
}

/// Fill in any forward returns that the latest bars now make measurable.
pub fn update(db: &Connection, observation: &Observation) -> Result<usize> {
    let pending = rows(
        db,
        "SELECT id, at, symbol, price, spy_price FROM picks p WHERE \
         (SELECT COUNT(*) FROM forward f WHERE f.pick_id = p.id) < 4 ORDER BY at",
    )?;
    let mut written = 0;
    for pick in pending {
        let (Some(id), Some(at), Some(symbol), Some(price)) = (
            pick["id"].as_i64(),
            pick["at"].as_f64(),
            pick["symbol"].as_str(),
            pick["price"].as_f64().filter(|p| *p > 0.0),
        ) else {
            continue;
        };
        let closes = closes_after(observation, symbol, at);
        let spy_closes = closes_after(observation, "SPY", at);
        let spy_price = pick["spy_price"].as_f64().filter(|p| *p > 0.0);
        for horizon in HORIZONS {
            let (Some(close), Some(spy_close)) =
                (closes.get(horizon - 1), spy_closes.get(horizon - 1))
            else {
                continue;
            };
            let spy_ret = spy_price.map_or(0.0, |p| spy_close / p - 1.0);
            let changed = db.execute(
                "INSERT OR IGNORE INTO forward(pick_id,horizon,ret,spy_ret) VALUES(?,?,?,?)",
                params![id, horizon as i64, close / price - 1.0, spy_ret],
            )?;
            written += changed;
        }
    }
    Ok(written)
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Mean excess return, hit rate and sample size per decision kind, plus calibration of
/// the model's conviction on its picks. Returns are in percent.
pub fn summary(db: &Connection) -> Result<Value> {
    let data = rows(
        db,
        "SELECT p.kind AS kind, p.conviction AS conviction, f.horizon AS horizon, \
         (f.ret - f.spy_ret) AS excess FROM forward f JOIN picks p ON p.id = f.pick_id",
    )?;
    let mut by_kind: BTreeMap<(String, i64), Vec<f64>> = BTreeMap::new();
    let mut calibration: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    for row in &data {
        let (Some(kind), Some(horizon), Some(excess)) = (
            row["kind"].as_str(),
            row["horizon"].as_i64(),
            row["excess"].as_f64(),
        ) else {
            continue;
        };
        by_kind
            .entry((kind.to_string(), horizon))
            .or_default()
            .push(excess * 100.0);
        if kind == "pick" && horizon == 5 {
            let conviction = row["conviction"].as_f64().unwrap_or(0.0);
            let bucket = if conviction >= 0.75 {
                "high (0.75 to 1)"
            } else if conviction >= 0.5 {
                "medium (0.5 to 0.75)"
            } else {
                "low (under 0.5)"
            };
            calibration.entry(bucket).or_default().push(excess * 100.0);
        }
    }
    let mut kinds = serde_json::Map::new();
    for kind in ["pick", "veto", "candidate"] {
        let mut horizons = serde_json::Map::new();
        for h in HORIZONS {
            let values = by_kind
                .get(&(kind.to_string(), h as i64))
                .cloned()
                .unwrap_or_default();
            horizons.insert(
                h.to_string(),
                json!({
                    "n": values.len(),
                    "mean_excess_pct": mean(&values),
                    "hit_rate": (!values.is_empty()).then(|| values.iter().filter(|v| **v > 0.0).count() as f64 / values.len() as f64),
                }),
            );
        }
        kinds.insert(kind.to_string(), Value::Object(horizons));
    }
    let buckets: Vec<Value> = calibration
        .iter()
        .map(|(name, v)| json!({"bucket": name, "n": v.len(), "mean_excess_pct": mean(v)}))
        .collect();
    let total: i64 = db.query_row("SELECT COUNT(*) FROM picks", [], |r| r.get(0))?;
    Ok(
        json!({"horizons": HORIZONS, "total_recorded": total, "kinds": kinds, "calibration": buckets}),
    )
}

/// A few plain sentences the judge reads about its own track record.
pub fn feedback(db: &Connection) -> Result<Option<String>> {
    let s = summary(db)?;
    let pick = &s["kinds"]["pick"]["5"];
    let control = &s["kinds"]["candidate"]["5"];
    let (Some(n), Some(mean_pick)) = (
        pick["n"].as_u64().filter(|n| *n >= 5),
        pick["mean_excess_pct"].as_f64(),
    ) else {
        return Ok(None);
    };
    let mut text = format!(
        "Your past picks (n={n}) beat SPY by {mean_pick:+.2}% on average after 5 trading days, hit rate {:.0}%.",
        pick["hit_rate"].as_f64().unwrap_or(0.0) * 100.0
    );
    if let (Some(c), Some(cn)) = (control["mean_excess_pct"].as_f64(), control["n"].as_u64()) {
        let _ = write!(
            text,
            " Stocks that passed the screen but you did not pick (n={cn}) returned {c:+.2}%."
        );
    }
    for b in s["calibration"].as_array().into_iter().flatten() {
        if let (Some(name), Some(n), Some(m)) = (
            b["bucket"].as_str(),
            b["n"].as_u64(),
            b["mean_excess_pct"].as_f64(),
        ) {
            let _ = write!(text, " Conviction {name}: n={n}, {m:+.2}%.");
        }
    }
    Ok(Some(text))
}

/// Convenience for the daily run: refresh and summarise a ledger by path.
pub fn refresh(path: &str, observation: &Observation) -> Result<Value> {
    let db = ledger::connect(path)?;
    update(&db, observation)?;
    summary(&db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, Intel, Quote};

    const T0: f64 = 1_700_000_000.0;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, c)| Bar {
                at: T0 + DAY * (i as f64 + 1.0),
                open: *c,
                close: *c,
                volume: 1.0,
            })
            .collect()
    }

    fn observation(stock: &[f64], spy: &[f64]) -> Observation {
        Observation {
            at: T0 + DAY * 20.0,
            universe: vec![],
            quotes: BTreeMap::from([
                (
                    "AAA".to_string(),
                    Quote {
                        bars: bars(stock),
                        actions: vec![],
                    },
                ),
                (
                    "SPY".to_string(),
                    Quote {
                        bars: bars(spy),
                        actions: vec![],
                    },
                ),
            ]),
            errors: BTreeMap::new(),
            next_cursor: 0,
            coverage: serde_json::Map::new(),
            intel: Intel::default(),
        }
    }

    fn pick(kind: &'static str, conviction: f64) -> PickRow {
        PickRow {
            symbol: "AAA".into(),
            kind,
            rank: 1,
            conviction,
            price: 100.0,
        }
    }

    #[test]
    fn forward_returns_are_measured_against_spy() {
        let db = ledger::connect(":memory:").unwrap();
        record(&db, T0, Some(400.0), &[pick("pick", 0.9)]).unwrap();
        let obs = observation(
            &[102.0, 103.0, 104.0, 105.0, 110.0],
            &[404.0, 404.0, 404.0, 404.0, 408.0],
        );
        assert!(update(&db, &obs).unwrap() >= 3);
        let s = summary(&db).unwrap();
        // After 1 day: stock +2%, SPY +1%, so +1 point of excess.
        let h1 = &s["kinds"]["pick"]["1"];
        assert!((h1["mean_excess_pct"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        // After 5 days: stock +10%, SPY +2%, so +8 points.
        assert!((s["kinds"]["pick"]["5"]["mean_excess_pct"].as_f64().unwrap() - 8.0).abs() < 1e-9);
        assert_eq!(s["kinds"]["pick"]["10"]["n"], 0);
        assert_eq!(s["calibration"][0]["bucket"], "high (0.75 to 1)");
    }

    #[test]
    fn recording_twice_and_updating_twice_is_inert() {
        let db = ledger::connect(":memory:").unwrap();
        record(&db, T0, Some(400.0), &[pick("pick", 0.5)]).unwrap();
        record(&db, T0, Some(400.0), &[pick("pick", 0.5)]).unwrap();
        let obs = observation(&[101.0; 5], &[400.0; 5]);
        update(&db, &obs).unwrap();
        assert_eq!(update(&db, &obs).unwrap(), 0);
        assert_eq!(summary(&db).unwrap()["total_recorded"], 1);
    }

    #[test]
    fn open_symbols_lists_unfinished_picks_only() {
        let db = ledger::connect(":memory:").unwrap();
        record(&db, T0, Some(400.0), &[pick("pick", 0.5)]).unwrap();
        assert!(open_symbols(&db, T0 + DAY).unwrap().contains("AAA"));
        let long = observation(&[101.0; 12], &[400.0; 12]);
        update(&db, &long).unwrap();
        assert_eq!(open_symbols(&db, T0 + DAY).unwrap().len(), 0);
    }

    #[test]
    fn feedback_needs_enough_samples() {
        let db = ledger::connect(":memory:").unwrap();
        assert!(feedback(&db).unwrap().is_none());
    }
}
