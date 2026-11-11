//! Scores every decision the analyst makes, not just the trades that happened.
//!
//! A portfolio makes a handful of trades in two weeks, which is too little to tell skill
//! from luck. But every day the desk also picks, vetoes and passes over dozens of stocks,
//! and each of those has a measurable forward return against SPY. Recording them gives
//! hundreds of samples.
//!
//! What the numbers can and cannot say:
//!
//! - Forward windows overlap. A 10-day return measured on Monday's pick and on Tuesday's
//!   share nine days, and thirty stocks scored on one day share that day's market. So the
//!   count of rows is not the count of independent observations. The summary reports the
//!   number of distinct decision days beside `n` and takes its standard error from the
//!   day averages, not from the rows.
//! - Every screened stock is scored, not only the picks, so the picks are compared with
//!   what was passed over on the same days.
//! - A return is only measured from bars dated after the decision, and a pick's price is
//!   put on the same split basis as the later closes.
//! - A pick whose stock stops trading never gets its later returns. Those are counted as
//!   `unresolved` so the loss of the worst cases is visible, not silent.
//! - The model's `conviction` is free output. Its buckets are shown as score buckets with
//!   their sample sizes, and no bucket is summarised until it has [`MIN_BUCKET`] rows. It
//!   is never called a probability.
//! - Results made under earlier trading rules are counted apart (`legacy_recorded`).

use crate::ledger::{self, rows};
use crate::model::Observation;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Trading days after a decision at which its return is measured.
pub const HORIZONS: [usize; 4] = [1, 3, 5, 10];
/// Rows a score bucket needs before its average is shown.
pub const MIN_BUCKET: usize = 20;
/// Scored picks, and distinct decision days, the judge's feedback needs before it is given.
pub const MIN_FEEDBACK_PICKS: u64 = 20;
pub const MIN_FEEDBACK_DAYS: u64 = 5;
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

/// What to multiply a price recorded at `at` by to put it on the share basis of the bars:
/// the product of old-shares-per-new-share over every split reported after `at`. A
/// 1-for-50 reverse split gives 50.
pub fn split_factor(observation: &Observation, symbol: &str, at: f64) -> f64 {
    observation
        .quotes
        .get(symbol)
        .into_iter()
        .flat_map(|q| &q.actions)
        .filter(|a| a.kind == "splits" && a.date > at)
        .filter_map(|a| {
            let factor = a.denominator? / a.numerator?;
            (factor.is_finite() && factor > 0.0).then_some(factor)
        })
        .product()
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
        // Later closes are on today's share basis. A split after the pick puts the recorded
        // price on a different basis, so it is converted before the two are compared.
        let price = price * split_factor(observation, symbol, at);
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

/// Mean and standard error of the day averages of `(day, value)` rows. Rows from one day
/// share that day's market, so the days are the observations.
fn by_day(rows: &[(i64, f64)]) -> (usize, Option<f64>) {
    let mut days: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for (day, value) in rows {
        days.entry(*day).or_default().push(*value);
    }
    let means: Vec<f64> = days.values().filter_map(|v| mean(v)).collect();
    let n = means.len();
    if n < 2 {
        return (n, None);
    }
    let m = means.iter().sum::<f64>() / n as f64;
    let variance = means.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n as f64 - 1.0);
    (n, Some((variance / n as f64).sqrt()))
}

/// Mean excess return, hit rate and sample size per decision kind for decisions made at or
/// after `since` (the moment the current trading rules took effect; 0 for everything),
/// with the model's score buckets. Returns are in percent.
pub fn summary(db: &Connection, since: f64) -> Result<Value> {
    let data = rows(
        db,
        &format!(
            "SELECT p.kind AS kind, p.conviction AS conviction, p.at AS at, f.horizon AS horizon, \
             (f.ret - f.spy_ret) AS excess FROM forward f JOIN picks p ON p.id = f.pick_id \
             WHERE p.at >= {since}"
        ),
    )?;
    let mut by_kind: BTreeMap<(String, i64), Vec<(i64, f64)>> = BTreeMap::new();
    let mut buckets: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut scores: BTreeSet<i64> = BTreeSet::new();
    for row in &data {
        let (Some(kind), Some(horizon), Some(excess)) = (
            row["kind"].as_str(),
            row["horizon"].as_i64(),
            row["excess"].as_f64(),
        ) else {
            continue;
        };
        let day = (row["at"].as_f64().unwrap_or(0.0) / DAY) as i64;
        by_kind
            .entry((kind.to_string(), horizon))
            .or_default()
            .push((day, excess * 100.0));
        if kind == "pick" && horizon == 5 {
            let conviction = row["conviction"].as_f64().unwrap_or(0.0);
            scores.insert((conviction * 100.0).round() as i64);
            let bucket = if conviction >= 0.75 {
                "high (0.75 to 1)"
            } else if conviction >= 0.5 {
                "medium (0.5 to 0.75)"
            } else {
                "low (under 0.5)"
            };
            buckets.entry(bucket).or_default().push(excess * 100.0);
        }
    }
    let mut kinds = serde_json::Map::new();
    for kind in ["pick", "veto", "candidate"] {
        let mut horizons = serde_json::Map::new();
        for h in HORIZONS {
            let rows = by_kind
                .get(&(kind.to_string(), h as i64))
                .cloned()
                .unwrap_or_default();
            let values: Vec<f64> = rows.iter().map(|(_, v)| *v).collect();
            let (days, standard_error) = by_day(&rows);
            horizons.insert(
                h.to_string(),
                json!({
                    "n": values.len(),
                    "decision_days": days,
                    // Windows of h days overlap unless the decisions are h days apart.
                    "non_overlapping_windows": days.div_ceil(h),
                    "mean_excess_pct": mean(&values),
                    "standard_error_pct": standard_error,
                    "hit_rate": (!values.is_empty()).then(|| values.iter().filter(|v| **v > 0.0).count() as f64 / values.len() as f64),
                }),
            );
        }
        kinds.insert(kind.to_string(), Value::Object(horizons));
    }
    let score_buckets: Vec<Value> = buckets
        .iter()
        .map(|(name, v)| {
            let enough = v.len() >= MIN_BUCKET;
            json!({"bucket": name, "n": v.len(),
                   "mean_excess_pct": if enough { mean(v) } else { None },
                   "note": if enough { "" } else { "too few rows to summarise" }})
        })
        .collect();
    let total: i64 = db.query_row("SELECT COUNT(*) FROM picks WHERE at >= ?", [since], |r| {
        r.get(0)
    })?;
    let legacy: i64 = db.query_row("SELECT COUNT(*) FROM picks WHERE at < ?", [since], |r| {
        r.get(0)
    })?;
    let newest: f64 = db.query_row("SELECT COALESCE(MAX(at), 0) FROM picks", [], |r| r.get(0))?;
    let unresolved: i64 = db.query_row(
        "SELECT COUNT(*) FROM picks p WHERE p.at >= ?1 AND p.at < ?2 AND \
         (SELECT COUNT(*) FROM forward f WHERE f.pick_id = p.id) < ?3",
        params![since, newest - 21.0 * DAY, HORIZONS.len() as i64],
        |r| r.get(0),
    )?;
    Ok(json!({
        "horizons": HORIZONS, "total_recorded": total, "legacy_recorded": legacy,
        "since": since, "unresolved": unresolved, "kinds": kinds,
        "score_label": crate::analyst::SCORE_LABEL,
        "score_buckets": score_buckets,
        "min_bucket": MIN_BUCKET,
        "distinct_scores": scores.len(),
        "score_note": if scores.len() == 1 {
            "the model gave every pick the same score, so the score carries no information"
        } else {
            ""
        },
    }))
}

/// A few plain sentences the judge reads about its own track record. Nothing is said until
/// there are enough picks over enough separate days, and what is said carries its
/// uncertainty. The score is never described as a probability.
pub fn feedback(db: &Connection, since: f64) -> Result<Option<String>> {
    let s = summary(db, since)?;
    let pick = &s["kinds"]["pick"]["5"];
    let control = &s["kinds"]["candidate"]["5"];
    let (Some(n), Some(days), Some(mean_pick)) = (
        pick["n"].as_u64().filter(|n| *n >= MIN_FEEDBACK_PICKS),
        pick["decision_days"]
            .as_u64()
            .filter(|d| *d >= MIN_FEEDBACK_DAYS),
        pick["mean_excess_pct"].as_f64(),
    ) else {
        return Ok(None);
    };
    let mut text = format!(
        "Your past picks ({n} over {days} decision days) returned {mean_pick:+.2}% against SPY after 5 trading days",
    );
    match pick["standard_error_pct"].as_f64() {
        Some(se) => {
            let _ = write!(
                text,
                ", give or take {:.2}% (two standard errors across days){}.",
                2.0 * se,
                if mean_pick.abs() < 2.0 * se {
                    "; that is not distinguishable from zero"
                } else {
                    ""
                }
            );
        }
        None => text.push_str("; too few days to say how uncertain that is."),
    }
    if let (Some(c), Some(cn)) = (control["mean_excess_pct"].as_f64(), control["n"].as_u64()) {
        let _ = write!(
            text,
            " Stocks that passed the screen but you did not pick (n={cn}) returned {c:+.2}%."
        );
    }
    if s["distinct_scores"].as_u64() == Some(1) {
        text.push_str(" You gave every pick the same score, so your score has told nothing apart.");
    }
    for b in s["score_buckets"].as_array().into_iter().flatten() {
        if let (Some(name), Some(n), Some(m)) = (
            b["bucket"].as_str(),
            b["n"].as_u64(),
            b["mean_excess_pct"].as_f64(),
        ) {
            let _ = write!(text, " Uncalibrated score {name}: n={n}, {m:+.2}%.");
        }
    }
    Ok(Some(text))
}

/// Convenience for the daily run: refresh and summarise a ledger by path.
pub fn refresh(path: &str, observation: &Observation) -> Result<Value> {
    let db = ledger::connect(path)?;
    update(&db, observation)?;
    summary(&db, 0.0)
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
                        ..Quote::default()
                    },
                ),
                (
                    "SPY".to_string(),
                    Quote {
                        bars: bars(spy),
                        actions: vec![],
                        ..Quote::default()
                    },
                ),
            ]),
            errors: BTreeMap::new(),
            next_cursor: 0,
            coverage: serde_json::Map::new(),
            intel: Intel::default(),
            securities: BTreeMap::new(),
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
        let s = summary(&db, 0.0).unwrap();
        // After 1 day: stock +2%, SPY +1%, so +1 point of excess.
        let h1 = &s["kinds"]["pick"]["1"];
        assert!((h1["mean_excess_pct"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        // After 5 days: stock +10%, SPY +2%, so +8 points.
        assert!((s["kinds"]["pick"]["5"]["mean_excess_pct"].as_f64().unwrap() - 8.0).abs() < 1e-9);
        assert_eq!(s["kinds"]["pick"]["10"]["n"], 0);
        assert_eq!(s["score_buckets"][0]["bucket"], "high (0.75 to 1)");
        // One row is not a calibration curve: the bucket is listed and not summarised.
        assert_eq!(s["score_buckets"][0]["mean_excess_pct"], Value::Null);
        assert_eq!(
            s["score_label"],
            "uncalibrated model score, not a probability"
        );
        assert_eq!(s["kinds"]["pick"]["5"]["decision_days"], 1);
        assert_eq!(s["kinds"]["pick"]["5"]["standard_error_pct"], Value::Null);
    }

    #[test]
    fn recording_twice_and_updating_twice_is_inert() {
        let db = ledger::connect(":memory:").unwrap();
        record(&db, T0, Some(400.0), &[pick("pick", 0.5)]).unwrap();
        record(&db, T0, Some(400.0), &[pick("pick", 0.5)]).unwrap();
        let obs = observation(&[101.0; 5], &[400.0; 5]);
        update(&db, &obs).unwrap();
        assert_eq!(update(&db, &obs).unwrap(), 0);
        assert_eq!(summary(&db, 0.0).unwrap()["total_recorded"], 1);
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
        assert!(feedback(&db, 0.0).unwrap().is_none());
    }

    #[test]
    fn a_pick_made_before_a_reverse_split_is_scored_on_the_same_share_basis() {
        // OMH's 1-for-50 reverse split: a pick at $0.07 before it, bars at $3.50 after it,
        // all of them on the post-split basis. Unadjusted, that reads as +4,900%.
        let db = ledger::connect(":memory:").unwrap();
        record(
            &db,
            T0,
            Some(400.0),
            &[PickRow {
                price: 0.07,
                ..pick("pick", 0.5)
            }],
        )
        .unwrap();
        let mut obs = observation(&[3.57, 3.5, 3.5, 3.5, 3.85], &[400.0; 5]);
        obs.quotes
            .get_mut("AAA")
            .unwrap()
            .actions
            .push(crate::model::Action {
                kind: "splits".into(),
                date: T0 + DAY * 1.0,
                amount: None,
                numerator: Some(1.0),
                denominator: Some(50.0),
            });
        assert!((split_factor(&obs, "AAA", T0) - 50.0).abs() < 1e-9);
        assert!((split_factor(&obs, "AAA", T0 + DAY * 2.0) - 1.0).abs() < 1e-9);
        update(&db, &obs).unwrap();
        let s = summary(&db, 0.0).unwrap();
        // $0.07 is $3.50 on the new basis: +2% after one day, +10% after five.
        assert!((s["kinds"]["pick"]["1"]["mean_excess_pct"].as_f64().unwrap() - 2.0).abs() < 1e-6);
        assert!((s["kinds"]["pick"]["5"]["mean_excess_pct"].as_f64().unwrap() - 10.0).abs() < 1e-6);
    }

    #[test]
    fn rows_from_one_day_are_one_observation_and_old_rules_are_counted_apart() {
        let db = ledger::connect(":memory:").unwrap();
        // Thirty stocks on each of two days: sixty rows, two observations.
        for day in 0..2 {
            let at = T0 + DAY * f64::from(day);
            for i in 0..30 {
                db.execute(
                    "INSERT INTO picks(at,symbol,kind,rank,conviction,price,spy_price) VALUES(?,?,?,?,?,?,?)",
                    params![at, format!("S{i}"), "pick", 1, 0.9, 100.0, 400.0],
                )
                .unwrap();
                let id = db.last_insert_rowid();
                let ret = if day == 0 { 0.04 } else { -0.02 };
                db.execute(
                    "INSERT INTO forward(pick_id,horizon,ret,spy_ret) VALUES(?,?,?,?)",
                    params![id, 5, ret, 0.0],
                )
                .unwrap();
            }
        }
        let s = summary(&db, 0.0).unwrap();
        let five = &s["kinds"]["pick"]["5"];
        assert_eq!(five["n"], 60);
        assert_eq!(five["decision_days"], 2);
        assert_eq!(five["non_overlapping_windows"], 1);
        // Day means +4 and -2: mean +1, standard error 3. Sixty rows would have claimed 0.4.
        assert!((five["mean_excess_pct"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert!((five["standard_error_pct"].as_f64().unwrap() - 3.0).abs() < 1e-9);
        assert_eq!(s["distinct_scores"], 1);
        assert!(s["score_note"].as_str().unwrap().contains("no information"));
        assert_eq!(s["score_buckets"][0]["n"], 60);
        assert!((s["score_buckets"][0]["mean_excess_pct"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        // Two days is not enough to tell the judge anything.
        assert_eq!(feedback(&db, 0.0).unwrap(), None);
        // Under rules that began on the second day, the first day is legacy.
        let later = summary(&db, T0 + DAY).unwrap();
        assert_eq!(later["total_recorded"], 30);
        assert_eq!(later["legacy_recorded"], 30);
        assert_eq!(later["kinds"]["pick"]["5"]["n"], 30);
    }

    #[test]
    fn feedback_states_its_uncertainty_and_never_calls_the_score_a_probability() {
        let db = ledger::connect(":memory:").unwrap();
        for day in 0..6 {
            for i in 0..4 {
                db.execute(
                    "INSERT INTO picks(at,symbol,kind,rank,conviction,price,spy_price) VALUES(?,?,?,?,?,?,?)",
                    params![T0 + DAY * f64::from(day), format!("S{i}"), "pick", 1, 0.9, 100.0, 400.0],
                )
                .unwrap();
                let id = db.last_insert_rowid();
                let ret = if day % 2 == 0 { 0.03 } else { -0.02 };
                db.execute(
                    "INSERT INTO forward(pick_id,horizon,ret,spy_ret) VALUES(?,?,?,?)",
                    params![id, 5, ret, 0.0],
                )
                .unwrap();
            }
        }
        let text = feedback(&db, 0.0).unwrap().unwrap();
        assert!(
            text.starts_with("Your past picks (24 over 6 decision days) returned +0.50%"),
            "{text}"
        );
        assert!(text.contains("not distinguishable from zero"), "{text}");
        assert!(text.contains("same score"), "{text}");
        assert!(text.contains("Uncalibrated score high"), "{text}");
        for word in [
            "probab",
            "calibration",
            "conviction",
            "chance",
            "likel",
            "hit rate",
        ] {
            assert!(!text.to_lowercase().contains(word), "{word}: {text}");
        }
    }
}
