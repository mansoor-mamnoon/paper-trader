//! One daily run: observe the market once, let every strategy (the live champion and its
//! shadows) trade it in its own ledger, learn from closed trades, then run the
//! improvement step.
//!
//! Model calls run in blocks so the pool swaps models as little as possible: shared
//! research first (triage, research agents, bull cases, bear cases), then every judge,
//! then every single-shot analyst, then every doctor. Each strategy runs at its own desk
//! depth. Depth 0 is the momentum ranking with no AI, depth 1 a single-shot analyst,
//! depth 2 a committee, depth 3 the full desk. The champion sits at the top of a ladder
//! of shadows one rung down each, so the lab measures what every layer of AI adds.

use crate::account;
use crate::analyst::{Completion, LlmAnalyst, Review, review_or_fallback};
use crate::data;
use crate::desk::{self, Budget, ClosedTrade, Desk, Prepared, Transcript};
use crate::edgar::{self, Edgar};
use crate::http;
use crate::intel;
use crate::lab::{self, LlmProposer, Proposer, Registry, Shadow};
use crate::ledger;
use crate::model::{Observation, State};
use crate::models::{MODELS, Role};
use crate::pool::{PoolEvent, Router};
use crate::scorecard;
use crate::strategy::Strategy;
use crate::telemetry::{CallRecord, Telemetry, short_hash};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

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
                "No AI: the same screen ranked by 20-day gain. If the models cannot beat this, the lab will switch them off.",
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

/// A router that times every call and reports it to the telemetry store, and turns the
/// pool's load, download and determinism events into telemetry too.
#[derive(Debug)]
struct Metered<'a> {
    inner: &'a dyn Router,
    telemetry: &'a Telemetry,
}

impl Metered<'_> {
    fn flush(&self) {
        for PoolEvent {
            kind,
            subject,
            detail,
            ms,
        } in self.inner.take_events()
        {
            if kind == "determinism" {
                let company = MODELS
                    .iter()
                    .find(|m| m.id == subject)
                    .map_or("", |m| m.company);
                let hash = detail.split(' ').nth(1).unwrap_or("").to_string();
                self.telemetry.call(&CallRecord {
                    stage: "canary".into(),
                    role: "canary".into(),
                    model_id: subject.clone(),
                    company: company.into(),
                    prompt_chars: 0,
                    prompt_hash: String::new(),
                    output_chars: 0,
                    output_hash: hash,
                    latency_ms: ms,
                    prompt_tokens: None,
                    completion_tokens: None,
                    prompt_tps: None,
                    gen_tps: None,
                    ok: !detail.contains("UNSTABLE"),
                    error: None,
                });
            }
            self.telemetry
                .event(&kind, &subject, &format!("{detail} ({ms} ms)"));
        }
    }
}

impl Router for Metered<'_> {
    fn complete(
        &self,
        role: Role,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> Result<Completion> {
        let started = Instant::now();
        let result = self.inner.complete(role, system, user, schema);
        self.flush();
        let (model_id, company) = self.inner.describe(role);
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (output, ok, error) = match &result {
            Ok(c) => (c.text.as_str(), true, None),
            Err(e) => ("", false, Some(e.to_string().chars().take(300).collect())),
        };
        let (pt, ct, pps, gps) = result.as_ref().map_or((None, None, None, None), |c| {
            (
                c.prompt_tokens,
                c.completion_tokens,
                c.prompt_tps,
                c.gen_tps,
            )
        });
        self.telemetry.call(&CallRecord {
            stage: role.name().into(),
            role: role.name().into(),
            model_id,
            company,
            prompt_chars: system.len() + user.len(),
            prompt_hash: short_hash(&format!("{system}\n{user}")),
            output_chars: output.len(),
            output_hash: short_hash(output),
            latency_ms,
            prompt_tokens: pt,
            completion_tokens: ct,
            prompt_tps: pps,
            gen_tps: gps,
            ok,
            error,
        });
        result
    }

    fn describe(&self, role: Role) -> (String, String) {
        self.inner.describe(role)
    }

    fn max_candidates(&self) -> usize {
        self.inner.max_candidates()
    }

    fn label(&self) -> String {
        self.inner.label()
    }
}

/// Everything decided for one strategy before its cycle runs.
struct Plan {
    state: State,
    brief: crate::analyst::Brief,
    depth: u8,
    feedback: Option<String>,
    prepared: Option<Prepared>,
    review: Option<Review>,
}

fn record_fetches(telemetry: &Telemetry) {
    for f in http::drain_fetches() {
        telemetry.fetch(&f.source, f.ok, f.ms);
    }
}

/// Run the whole day. Returns the dashboard summary.
#[allow(clippy::too_many_lines)]
pub fn run(
    options: &Options,
    mode: &str,
    llm: Option<&dyn Router>,
    edgar: Option<&Edgar>,
    telemetry: &Telemetry,
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
        let mut observed = {
            let _span = telemetry.span("scan");
            data::observe(&champion_state, options.batch_size, &extra)?
        };
        record_fetches(telemetry);
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
        {
            let _span = telemetry.span("intel");
            observed.intel = intel::gather(&wanted, today)?;
        }
        record_fetches(telemetry);
        if let Some(edgar) = edgar {
            let _span = telemetry.span("edgar");
            observed.intel.filings = edgar::gather_filings(edgar, &wanted, today, 45, 6);
            record_fetches(telemetry);
        }
        observed
    };
    observation.intel.news.retain(|_, v| !v.is_empty());
    let now = observation.at;

    let budget = Budget::new(options.desk_minutes);
    let metered = llm.map(|inner| Metered { inner, telemetry });
    let router: Option<&dyn Router> = metered.as_ref().map(|m| m as &dyn Router);
    let mut desk_state = router.map(|r| Desk::new(r, edgar, &observation, &budget));
    let mut champion_transcript = Transcript::default();
    let mut notes: Vec<String> = Vec::new();

    // Plan every strategy: its state, its brief, and the depth it runs at.
    let mut plans: Vec<Option<Plan>> = Vec::new();
    for v in &all {
        if let Err(error) = account::reserve(&v.db, mode) {
            registry.log.push(lab::LogEntry {
                at: now,
                event: "error".into(),
                id: v.id.clone(),
                detail: format!("could not open ledger: {error}"),
            });
            plans.push(None);
            continue;
        }
        let state = state_of(&v.db);
        let brief = account::brief(&v.strategy, &observation, &state);
        let feedback = if v.strategy.use_memory {
            ledger::connect(&v.db)
                .ok()
                .and_then(|db| scorecard::feedback(&db).ok().flatten())
        } else {
            None
        };
        plans.push(Some(Plan {
            state,
            brief,
            depth: if router.is_some() {
                v.strategy.effective_depth()
            } else {
                0
            },
            feedback,
            prepared: None,
            review: None,
        }));
    }

    // Block 1: the shared research, which every deep strategy reuses.
    if let Some(desk) = &mut desk_state {
        let _span = telemetry.span("desk.prepare");
        for (v, plan) in all.iter().zip(plans.iter_mut()) {
            if let Some(plan) = plan.as_mut().filter(|p| p.depth >= 2) {
                plan.prepared = Some(desk.prepare(&v.strategy, &plan.brief));
            }
        }
    }
    // Block 2: every judge, on the judge's model.
    if let Some(desk) = &mut desk_state {
        let _span = telemetry.span("desk.judge");
        for (index, (v, plan)) in all.iter().zip(plans.iter_mut()).enumerate() {
            let Some(plan) = plan.as_mut() else { continue };
            if let Some(prepared) = &plan.prepared {
                let (review, transcript) = desk.judge(
                    &v.strategy,
                    &plan.brief,
                    prepared,
                    plan.feedback.as_deref(),
                    &registry.lessons,
                );
                plan.review = Some(review);
                if index == 0 {
                    champion_transcript = transcript;
                }
            }
        }
    }
    // Block 3: the single-shot analysts (depth 1), sharing answers between strategies.
    if let Some(router) = router {
        let _span = telemetry.span("desk.single_shot");
        let mut memo: BTreeMap<String, Review> = BTreeMap::new();
        for (v, plan) in all.iter().zip(plans.iter_mut()) {
            let Some(plan) = plan.as_mut().filter(|p| p.depth == 1) else {
                continue;
            };
            let key = format!(
                "{}|{}|{:?}",
                v.strategy.playbook,
                plan.brief.open_slots,
                plan.brief
                    .candidates
                    .iter()
                    .map(|c| c.symbol.as_str())
                    .collect::<Vec<_>>()
            );
            let analyst = LlmAnalyst { llm: router };
            plan.review = Some(
                memo.entry(key)
                    .or_insert_with(|| review_or_fallback(Some(&analyst), &v.strategy, &plan.brief))
                    .clone(),
            );
        }
    }
    for plan in plans.iter_mut().flatten() {
        if plan.review.is_none() {
            plan.review = Some(review_or_fallback(None, &Strategy::default(), &plan.brief));
        }
    }
    // Block 4: the position doctor for every strategy that holds something.
    if let Some(desk) = &mut desk_state {
        let _span = telemetry.span("desk.doctor");
        for (index, (v, plan)) in all.iter().zip(plans.iter_mut()).enumerate() {
            let Some(plan) = plan.as_mut() else { continue };
            if v.strategy.use_doctor && plan.depth >= 1 && !plan.state.positions.is_empty() {
                let symbols: Vec<String> = plan.state.positions.keys().cloned().collect();
                let held = desk::held_views(&plan.state, &observation, &theses(&v.db, &symbols));
                let (calls, shown) = desk.doctor(&held, &mut notes);
                if let Some(review) = plan.review.as_mut() {
                    review.doctor = calls;
                }
                if index == 0 {
                    champion_transcript.doctor = shown;
                }
            }
        }
    }
    if let Some(Some(plan)) = plans.first()
        && let Some(review) = &plan.review
    {
        registry.analyst = lab::AnalystStatus {
            at: now,
            source: review.source.clone(),
            model: router.map(Router::label).unwrap_or_default(),
            market_view: review.market_view.clone(),
            error: review.error.clone(),
        };
    }

    // Block 5: every strategy trades its day.
    {
        let _span = telemetry.span("cycles");
        for (index, (v, plan)) in all.iter().zip(plans.iter()).enumerate() {
            let Some(plan) = plan else { continue };
            let _one = telemetry.span(&format!("cycle:{}", v.id));
            let result =
                account::cycle_with(&v.db, &observation, mode, &v.strategy, plan.review.as_ref());
            match result {
                Ok(()) => {}
                Err(error) if index == 0 => return Err(error),
                Err(error) => {
                    telemetry.event("error", &v.id, &format!("shadow cycle failed: {error}"));
                    registry.log.push(lab::LogEntry {
                        at: now,
                        event: "error".into(),
                        id: v.id.clone(),
                        detail: format!("shadow cycle failed: {error}"),
                    });
                }
            }
        }
    }
    champion_transcript.degraded.extend(notes);
    for d in &champion_transcript.degraded {
        telemetry.event("degraded", "desk", d);
    }

    // Learn from trades that closed since the last run.
    if let Some(router) = router.filter(|_| options.replay.is_none()) {
        let _span = telemetry.span("post_mortem");
        for (id, trade) in closed_trades(&options.db, registry.lessons_cursor)? {
            registry.lessons_cursor = registry.lessons_cursor.max(id);
            if !budget.has(60) {
                continue;
            }
            if let Ok(lesson) = desk::post_mortem(router, &trade, now)
                && !lesson.lesson.to_lowercase().contains("no clear lesson")
            {
                registry.lessons.push(lesson);
            }
        }
        let excess = registry.lessons.len().saturating_sub(40);
        registry.lessons.drain(..excess);
    }

    if options.replay.is_none() {
        let _span = telemetry.span("lab");
        let proposer = router.map(|llm| LlmProposer { llm });
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
    if let Some(m) = &metered {
        m.flush();
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
    summary["minutes_left"] = json!(budget.minutes_left());
    summary["roster"] = json!(
        Role::ALL
            .iter()
            .map(|role| {
                let m = crate::models::model_for(*role);
                json!({
                    "role": role.name(), "model_id": m.id, "company": m.company, "name": m.name,
                    "license": m.license, "size_mb": m.bytes / 1_000_000,
                })
            })
            .collect::<Vec<_>>()
    );
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
    use crate::analyst::Llm;
    use crate::model::{Bar, Intel, Quote};
    use crate::pool::OneModel;
    use crate::telemetry::RunMeta;

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

    fn telemetry() -> Telemetry {
        let meta = RunMeta {
            trigger: "test".into(),
            git_sha: "x".into(),
            cpu: "test".into(),
            cores: 1,
            ram_mb: 1,
            desk_minutes: 5.0,
        };
        Telemetry::open_memory("t", &meta, 0.0).unwrap()
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
        let llm = Picker;
        let router = OneModel(&llm);
        let t = telemetry();
        let summary = run(&options, "stocks-replay", Some(&router), None, &t).unwrap();
        let state = state_of(&options.db);
        assert_eq!(state.pending.keys().collect::<Vec<_>>(), ["SLOW"]);
        assert!(state.pending["SLOW"].reason.contains("test thesis"));
        assert_eq!(summary["analyst"]["source"], "llm");
        assert_eq!(summary["champion"]["id"], "baseline");
        // Every model call was metered.
        assert!(t.summary_json()["totals"]["calls"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn the_full_desk_runs_and_leaves_a_transcript_and_a_depth_ladder() {
        let (options, _dir) = setup("desk", 3);
        let llm = Picker;
        let router = OneModel(&llm);
        let t = telemetry();
        let summary = run(&options, "stocks-replay", Some(&router), None, &t).unwrap();
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
        let stage_names: Vec<String> = t.summary_json()["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
        for expected in ["desk.prepare", "desk.judge", "cycles"] {
            assert!(stage_names.iter().any(|n| n == expected), "{stage_names:?}");
        }
    }

    #[test]
    fn without_a_model_nothing_is_added_and_the_momentum_rank_trades() {
        let (options, _dir) = setup("none", 3);
        let t = telemetry();
        let summary = run(&options, "stocks-replay", None, None, &t).unwrap();
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
