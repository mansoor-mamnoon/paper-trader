//! One daily run: observe the market once, let every strategy (the live champion and its
//! shadows) trade it in its own ledger, learn from closed trades, then run the
//! improvement step.
//!
//! Each strategy runs at its own desk depth. Depth 0 is the momentum ranking with no AI,
//! depth 1 a single-shot analyst, depth 2 a committee, depth 3 the full desk with research
//! agents. The champion sits at the top of a ladder of shadows one rung down each, so the
//! lab measures what every layer of AI adds.

use crate::account;
use crate::analyst::{Llm, LlmAnalyst, review_or_fallback};
use crate::data;
use crate::desk::{self, Budget, ClosedTrade, Desk, Transcript};
use crate::edgar::{self, Edgar};
use crate::intel;
use crate::lab::{self, LlmProposer, Proposer, Registry, Shadow};
use crate::ledger;
use crate::model::{Observation, State};
use crate::scorecard;
use crate::strategy::Strategy;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Options {
    pub db: String,
    pub strategies: PathBuf,
    pub batch_size: usize,
    pub replay: Option<PathBuf>,
    /// Wall-clock allowance for all model calls in one run.
    pub desk_minutes: f64,
}

struct Variant {
    id: String,
    kind: String,
    db: String,
    strategy: Strategy,
}

fn variants(dir: &Path, registry: &Registry, champion_db: &str) -> Vec<Variant> {
    let mut all = vec![Variant {
        id: registry.champion.id.clone(),
        kind: "champion".into(),
        db: champion_db.to_string(),
        strategy: registry.champion.clone(),
    }];
    all.extend(registry.shadows.iter().map(|s| Variant {
        id: s.strategy.id.clone(),
        kind: s.kind.clone(),
        db: Registry::shadow_db(dir, &s.strategy.id),
        strategy: s.strategy.clone(),
    }));
    all
}

fn state_of(path: &str) -> State {
    ledger::connect(path)
        .and_then(|db| ledger::load(&db))
        .unwrap_or_default()
}

/// Keep one shadow at every desk depth below the champion's, so the lab can see what each
/// layer of AI is worth. Idempotent: it only adds the rungs that are missing.
pub fn ensure_ladder(registry: &mut Registry, now: f64) {
    let top = registry.champion.effective_depth();
    for depth in 0..top {
        if registry
            .shadows
            .iter()
            .any(|s| s.strategy.effective_depth() == depth)
        {
            continue;
        }
        let (id, why) = match depth {
            0 => (
                "depth0-no-ai",
                "No AI: the same screen ranked by 20-day gain. If the model cannot beat this, the lab will switch it off.",
            ),
            1 => (
                "depth1-single-shot",
                "One model call that ranks headlines, with no research, debate or reasoning step.",
            ),
            _ => (
                "depth2-committee",
                "Triage, bull and bear debate and a judge, but no research agents or filings.",
            ),
        };
        let strategy = Strategy {
            id: id.into(),
            parent: Some(registry.champion.id.clone()),
            created_at: now,
            rationale: why.into(),
            analyst_enabled: depth > 0,
            desk_depth: depth.max(1),
            ..registry.champion.clone()
        };
        registry.shadows.push(Shadow {
            strategy,
            kind: "ablation".into(),
            started_at: now,
        });
        registry.log.push(lab::LogEntry {
            at: now,
            event: "proposed".into(),
            id: id.into(),
            detail: format!("ablation at desk depth {depth}"),
        });
    }
}

fn theses(db_path: &str, symbols: &[String]) -> BTreeMap<String, String> {
    let Ok(db) = ledger::connect(db_path) else {
        return BTreeMap::new();
    };
    symbols
        .iter()
        .filter_map(|s| {
            let reason: String = db
                .query_row(
                    "SELECT reason FROM decisions WHERE symbol=? AND action='BUY_SIGNAL' ORDER BY id DESC LIMIT 1",
                    [s],
                    |r| r.get(0),
                )
                .ok()?;
            Some((s.clone(), reason))
        })
        .collect()
}

/// Sells after `after_id`, paired with the buy and thesis that opened them.
fn closed_trades(db_path: &str, after_id: i64) -> Result<Vec<(i64, ClosedTrade)>> {
    let db = ledger::connect(db_path)?;
    let sells = ledger::rows(
        &db,
        &format!(
            "SELECT id, at, symbol, price, reason FROM fills WHERE side='SELL' AND id > {after_id} ORDER BY id"
        ),
    )?;
    let mut out = Vec::new();
    for sell in sells {
        let (Some(id), Some(at), Some(symbol), Some(price)) = (
            sell["id"].as_i64(),
            sell["at"].as_f64(),
            sell["symbol"].as_str(),
            sell["price"].as_f64(),
        ) else {
            continue;
        };
        let buy: Option<(f64, f64)> = db
            .query_row(
                "SELECT at, price FROM fills WHERE symbol=? AND side='BUY' AND id < ? ORDER BY id DESC LIMIT 1",
                rusqlite::params![symbol, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((bought_at, entry)) = buy else {
            continue;
        };
        let thesis = theses(db_path, &[symbol.to_string()])
            .remove(symbol)
            .unwrap_or_default();
        out.push((
            id,
            ClosedTrade {
                symbol: symbol.to_string(),
                thesis,
                entry,
                exit: price,
                pnl_pct: (price / entry - 1.0) * 100.0,
                days_held: (at - bought_at) / 86400.0,
                exit_reason: sell["reason"].as_str().unwrap_or("").to_string(),
            },
        ));
    }
    Ok(out)
}

/// Gather filings for the symbols the desk may look at.
fn attach_filings(observation: &mut Observation, symbols: &[String], edgar: &Edgar) {
    let today = chrono::DateTime::from_timestamp(observation.at as i64, 0)
        .unwrap_or_default()
        .date_naive();
    observation.intel.filings = edgar::gather_filings(edgar, symbols, today, 45, 6);
}

/// Run the whole day. Returns the dashboard summary.
pub fn run(
    options: &Options,
    mode: &str,
    llm: Option<&dyn Llm>,
    edgar: Option<&Edgar>,
) -> Result<Value> {
    account::reserve(&options.db, mode)?;
    let mut registry = Registry::load(&options.strategies)?;
    let first_at = if options.replay.is_some() {
        observation_time(options)
    } else {
        ledger::now()
    };
    if llm.is_some() {
        ensure_ladder(&mut registry, first_at);
    }
    let all = variants(&options.strategies, &registry, &options.db);

    let mut observation: Observation = if let Some(file) = &options.replay {
        serde_json::from_str(&std::fs::read_to_string(file)?)?
    } else {
        let champion_state = state_of(&options.db);
        let mut extra = BTreeSet::new();
        for v in &all {
            let s = state_of(&v.db);
            extra.extend(s.positions.keys().cloned());
            extra.extend(s.pending.keys().cloned());
            if let Ok(db) = ledger::connect(&v.db) {
                extra.extend(scorecard::open_symbols(&db, ledger::now()).unwrap_or_default());
            }
        }
        let mut observed = data::observe(&champion_state, options.batch_size, &extra)?;
        let mut wanted = BTreeSet::new();
        for v in &all {
            let state = state_of(&v.db);
            wanted.extend(
                account::brief(&v.strategy, &observed, &state)
                    .candidates
                    .into_iter()
                    .map(|c| c.symbol),
            );
            wanted.extend(state.positions.keys().cloned());
        }
        let today = chrono::DateTime::from_timestamp(observed.at as i64, 0)
            .unwrap_or_default()
            .date_naive();
        let wanted: Vec<String> = wanted.into_iter().collect();
        observed.intel = intel::gather(&wanted, today)?;
        if let Some(edgar) = edgar {
            attach_filings(&mut observed, &wanted, edgar);
        }
        observed
    };
    observation.intel.news.retain(|_, v| !v.is_empty());
    let now = observation.at;

    let budget = Budget::new(options.desk_minutes);
    let mut desk_state = llm.map(|llm| Desk::new(llm, edgar, &observation, &budget));
    let mut memo: BTreeMap<String, crate::analyst::Review> = BTreeMap::new();
    let mut champion_transcript = Transcript::default();
    let mut doctor_notes: Vec<String> = Vec::new();

    for (index, v) in all.iter().enumerate() {
        let result = (|| -> Result<()> {
            account::reserve(&v.db, mode)?;
            let state = state_of(&v.db);
            let brief = account::brief(&v.strategy, &observation, &state);
            let depth = v.strategy.effective_depth();
            let feedback = if v.strategy.use_memory {
                ledger::connect(&v.db)
                    .ok()
                    .and_then(|db| scorecard::feedback(&db).ok().flatten())
            } else {
                None
            };
            let mut review = match (&mut desk_state, llm) {
                (Some(desk), Some(_)) if depth >= 2 => {
                    let (review, transcript) =
                        desk.review(&v.strategy, &brief, feedback.as_deref(), &registry.lessons);
                    if index == 0 {
                        champion_transcript = transcript;
                    }
                    review
                }
                (_, Some(llm)) if depth == 1 => {
                    // Strategies that show the model the same stocks under the same playbook
                    // share one answer instead of paying for a second call.
                    let key = format!(
                        "{}|{}|{:?}",
                        v.strategy.playbook,
                        brief.open_slots,
                        brief
                            .candidates
                            .iter()
                            .map(|c| c.symbol.as_str())
                            .collect::<Vec<_>>()
                    );
                    let analyst = LlmAnalyst { llm };
                    memo.entry(key)
                        .or_insert_with(|| review_or_fallback(Some(&analyst), &v.strategy, &brief))
                        .clone()
                }
                _ => review_or_fallback(None, &v.strategy, &brief),
            };
            if v.strategy.use_doctor
                && depth >= 1
                && !state.positions.is_empty()
                && let Some(desk) = &mut desk_state
            {
                let symbols: Vec<String> = state.positions.keys().cloned().collect();
                let held = desk::held_views(&state, &observation, &theses(&v.db, &symbols));
                let (calls, shown) = desk.doctor(&held, &mut doctor_notes);
                review.doctor = calls;
                if index == 0 {
                    champion_transcript.doctor = shown;
                }
            }
            if index == 0 {
                registry.analyst = lab::AnalystStatus {
                    at: now,
                    source: review.source.clone(),
                    model: llm.map(Llm::name).unwrap_or_default(),
                    market_view: review.market_view.clone(),
                    error: review.error.clone(),
                };
            }
            account::cycle_with(&v.db, &observation, mode, &v.strategy, Some(&review))
        })();
        match result {
            Ok(()) => {}
            Err(error) if index == 0 => return Err(error),
            Err(error) => {
                let message = format!("shadow cycle failed: {error}");
                registry.log.push(lab::LogEntry {
                    at: now,
                    event: "error".into(),
                    id: v.id.clone(),
                    detail: message,
                });
            }
        }
    }
    champion_transcript.degraded.extend(doctor_notes);

    // Learn from trades that closed since the last run.
    if let Some(llm) = llm.filter(|_| options.replay.is_none()) {
        for (id, trade) in closed_trades(&options.db, registry.lessons_cursor)? {
            registry.lessons_cursor = registry.lessons_cursor.max(id);
            if !budget.has(60) {
                continue;
            }
            if let Ok(lesson) = desk::post_mortem(llm, &trade, now)
                && !lesson.lesson.to_lowercase().contains("no clear lesson")
            {
                registry.lessons.push(lesson);
            }
        }
        let excess = registry.lessons.len().saturating_sub(40);
        registry.lessons.drain(..excess);
    }

    if options.replay.is_none() {
        let proposer = llm.map(|llm| LlmProposer { llm });
        for line in lab::improve(
            &options.strategies,
            &mut registry,
            &options.db,
            now,
            proposer.as_ref().map(|p| p as &dyn Proposer),
        )? {
            eprintln!("lab: {line}");
        }
    }
    registry.save(&options.strategies)?;

    let mut summary = lab::summary(&options.strategies, &registry, &options.db, now);
    let scorecards: Vec<Value> = variants(&options.strategies, &registry, &options.db)
        .iter()
        .filter_map(|v| {
            let db = ledger::connect(&v.db).ok()?;
            Some(json!({"id": v.id, "kind": v.kind, "depth": v.strategy.effective_depth(), "scorecard": scorecard::summary(&db).ok()?}))
        })
        .collect();
    summary["scorecards"] = json!(scorecards);
    summary["lessons"] = json!(registry.lessons.iter().rev().take(12).collect::<Vec<_>>());
    summary["desk"] = serde_json::to_value(&champion_transcript)?;
    Ok(summary)
}

fn observation_time(options: &Options) -> f64 {
    options
        .replay
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["at"].as_f64())
        .unwrap_or_else(ledger::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Bar, Intel, Quote};

    const T0: f64 = 1_700_000_000.0;
    const DAY: f64 = 86400.0;

    fn bars(count: usize, start: f64, step: f64) -> Vec<Bar> {
        (0..count)
            .map(|i| {
                let close = start + step * i as f64;
                Bar {
                    at: T0 + DAY * i as f64,
                    open: close,
                    close,
                    volume: 1_000_000.0,
                }
            })
            .collect()
    }

    /// Always answers with a review that picks SLOW, whichever stage asks.
    #[derive(Debug)]
    struct Picker;
    impl Llm for Picker {
        fn name(&self) -> String {
            "test".into()
        }
        fn complete(&self, _: &str, _: &str, _: Option<&Value>) -> Result<String> {
            Ok(r#"{"market_view":"calm","regime":"neutral","picks":[{"symbol":"SLOW","conviction":0.9,"size_fraction":1,"thesis":"test thesis","key_risk":"","invalidation":""}],"avoid":[]}"#.into())
        }
    }

    fn setup(name: &str, depth: u8) -> (Options, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pt-daily-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let at = T0 + DAY * 59.5;
        let observation = Observation {
            at,
            universe: vec![],
            quotes: BTreeMap::from([
                (
                    "FAST".to_string(),
                    Quote {
                        bars: bars(60, 50.0, 2.0),
                        actions: vec![],
                    },
                ),
                (
                    "SLOW".to_string(),
                    Quote {
                        bars: bars(60, 50.0, 1.0),
                        actions: vec![],
                    },
                ),
            ]),
            errors: BTreeMap::new(),
            next_cursor: 0,
            coverage: serde_json::Map::new(),
            intel: Intel::default(),
        };
        let file = dir.join("obs.json");
        std::fs::write(&file, serde_json::to_string(&observation).unwrap()).unwrap();
        let strategies = dir.join("strategies");
        let mut registry = Registry::load(&strategies).unwrap();
        registry.champion.desk_depth = depth;
        registry.save(&strategies).unwrap();
        (
            Options {
                db: dir.join("account.sqlite").to_string_lossy().into_owned(),
                strategies,
                batch_size: 10,
                replay: Some(file),
                desk_minutes: 5.0,
            },
            dir,
        )
    }

    #[test]
    fn a_single_shot_analyst_overrides_momentum_order_and_is_recorded() {
        let (options, _dir) = setup("single", 1);
        let summary = run(&options, "stocks-replay", Some(&Picker), None).unwrap();
        let state = state_of(&options.db);
        assert_eq!(state.pending.keys().collect::<Vec<_>>(), ["SLOW"]);
        assert!(state.pending["SLOW"].reason.contains("test thesis"));
        assert_eq!(summary["analyst"]["source"], "llm");
        assert_eq!(summary["champion"]["id"], "baseline");
    }

    #[test]
    fn the_full_desk_runs_and_leaves_a_transcript_and_a_depth_ladder() {
        let (options, _dir) = setup("desk", 3);
        let summary = run(&options, "stocks-replay", Some(&Picker), None).unwrap();
        // Three rungs below depth 3: no AI, single shot, committee.
        let ids: Vec<&str> = summary["shadows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            ["depth0-no-ai", "depth1-single-shot", "depth2-committee"]
        );
        assert_eq!(summary["desk"]["depth"], 3);
        assert!(summary["scorecards"].as_array().unwrap().len() >= 4);
    }

    #[test]
    fn without_a_model_nothing_is_added_and_the_momentum_rank_trades() {
        let (options, _dir) = setup("none", 3);
        let summary = run(&options, "stocks-replay", None, None).unwrap();
        assert_eq!(summary["shadows"].as_array().unwrap().len(), 0);
        let state = state_of(&options.db);
        assert_eq!(state.pending.keys().next().unwrap(), "FAST");
    }

    #[test]
    fn the_ladder_is_idempotent_and_skips_rungs_that_already_exist() {
        let mut registry = Registry::load(&std::env::temp_dir().join("pt-ladder-none")).unwrap();
        ensure_ladder(&mut registry, 1.0);
        ensure_ladder(&mut registry, 2.0);
        assert_eq!(registry.shadows.len(), 3);
        registry.champion.desk_depth = 1;
        registry.shadows.clear();
        ensure_ladder(&mut registry, 3.0);
        assert_eq!(registry.shadows.len(), 1);
        assert!(!registry.shadows[0].strategy.analyst_enabled);
    }

    #[test]
    fn closed_trades_pair_a_sell_with_its_buy_and_thesis() {
        let dir = std::env::temp_dir().join(format!("pt-closed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("a.sqlite").to_string_lossy().into_owned();
        let db = ledger::connect(&path).unwrap();
        ledger::insert_fill(&db, 100.0, 90.0, "XYZ", "BUY", 1.0, 10.0, 0.1, 0.0, "r").unwrap();
        ledger::insert_fill(
            &db,
            100.0 + 3.0 * DAY,
            100.0,
            "XYZ",
            "SELL",
            1.0,
            9.0,
            0.1,
            -1.0,
            "stop",
        )
        .unwrap();
        ledger::decision(
            &db,
            90.0,
            "XYZ",
            "BUY_SIGNAL",
            "Analyst: guidance up",
            json!({}),
        )
        .unwrap();
        let trades = closed_trades(&path, 0).unwrap();
        assert_eq!(trades.len(), 1);
        assert!((trades[0].1.pnl_pct + 10.0).abs() < 1e-9);
        assert_eq!(trades[0].1.thesis, "Analyst: guidance up");
        assert!((trades[0].1.days_held - 3.0).abs() < 1e-9);
        assert_eq!(closed_trades(&path, trades[0].0).unwrap().len(), 0);
    }
}
