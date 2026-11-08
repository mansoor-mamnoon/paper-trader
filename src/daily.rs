//! One daily run of the stock accounts: observe the market once, let every strategy
//! (the live champion and its shadows) trade it in its own ledger, then run the
//! improvement step.

use crate::account;
use crate::analyst::{Analyst, review_or_fallback};
use crate::data;
use crate::intel;
use crate::lab::{self, Proposer, Registry};
use crate::ledger;
use crate::model::{Observation, State};
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Options {
    pub db: String,
    pub strategies: PathBuf,
    pub batch_size: usize,
    pub replay: Option<PathBuf>,
}

struct Variant {
    id: String,
    db: String,
    strategy: crate::strategy::Strategy,
}

fn variants(dir: &Path, registry: &Registry, champion_db: &str) -> Vec<Variant> {
    let mut all = vec![Variant {
        id: registry.champion.id.clone(),
        db: champion_db.to_string(),
        strategy: registry.champion.clone(),
    }];
    all.extend(registry.shadows.iter().map(|s| Variant {
        id: s.strategy.id.clone(),
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

/// Run the whole day. Returns the strategy-lab summary for the dashboard.
pub fn run(
    options: &Options,
    mode: &str,
    analyst: Option<&dyn Analyst>,
    proposer: Option<&dyn Proposer>,
) -> Result<Value> {
    account::reserve(&options.db, mode)?;
    let mut registry = Registry::load(&options.strategies)?;
    let all = variants(&options.strategies, &registry, &options.db);

    let mut observation: Observation = if let Some(file) = &options.replay {
        serde_json::from_str(&std::fs::read_to_string(file)?)?
    } else {
        let champion_state = state_of(&options.db);
        let mut extra = BTreeSet::new();
        for v in &all[1..] {
            let s = state_of(&v.db);
            extra.extend(s.positions.keys().cloned());
            extra.extend(s.pending.keys().cloned());
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
        observed.intel = intel::gather(&wanted.into_iter().collect::<Vec<_>>(), today)?;
        observed
    };
    observation.intel.news.retain(|_, v| !v.is_empty());
    let now = observation.at;

    for (index, v) in all.iter().enumerate() {
        let result = (|| -> Result<()> {
            account::reserve(&v.db, mode)?;
            let brief = account::brief(&v.strategy, &observation, &state_of(&v.db));
            let review = review_or_fallback(analyst, &v.strategy, &brief);
            if index == 0 {
                registry.analyst = lab::AnalystStatus {
                    at: now,
                    source: review.source.clone(),
                    model: analyst.map(Analyst::name).unwrap_or_default(),
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

    if options.replay.is_none() {
        for line in lab::improve(
            &options.strategies,
            &mut registry,
            &options.db,
            now,
            proposer,
        )? {
            eprintln!("lab: {line}");
        }
    }
    registry.save(&options.strategies)?;
    Ok(lab::summary(
        &options.strategies,
        &registry,
        &options.db,
        now,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyst::{Brief, Review};
    use crate::model::{Bar, Intel, Quote};
    use crate::strategy::Strategy;
    use std::collections::BTreeMap;

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

    struct Picker(&'static str);
    impl Analyst for Picker {
        fn name(&self) -> String {
            "test".into()
        }
        fn review(&self, _: &Strategy, brief: &Brief) -> Result<Review> {
            Ok(Review {
                picks: brief
                    .candidates
                    .iter()
                    .filter(|c| c.symbol == self.0)
                    .map(|c| crate::analyst::Pick {
                        symbol: c.symbol.clone(),
                        conviction: 0.9,
                        thesis: "test thesis".into(),
                    })
                    .collect(),
                avoid: vec![],
                market_view: "calm".into(),
                source: "claude".into(),
                error: None,
            })
        }
    }

    #[test]
    fn analyst_choice_overrides_momentum_order_and_is_recorded() {
        let dir = std::env::temp_dir().join(format!("pt-daily-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
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
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, serde_json::to_string(&observation).unwrap()).unwrap();
        let options = Options {
            db: dir.join("account.sqlite").to_string_lossy().into_owned(),
            strategies: dir.join("strategies"),
            batch_size: 10,
            replay: Some(file),
        };
        let summary = run(&options, "stocks-replay", Some(&Picker("SLOW")), None).unwrap();
        let state = state_of(&options.db);
        assert_eq!(state.pending.keys().collect::<Vec<_>>(), ["SLOW"]);
        assert!(state.pending["SLOW"].reason.contains("test thesis"));
        assert_eq!(summary["analyst"]["source"], "claude");
        assert_eq!(summary["champion"]["id"], "baseline");
    }
}
