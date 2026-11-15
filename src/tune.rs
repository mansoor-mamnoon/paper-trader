//! Measure provider goodput on identical desk questions before selecting concurrency.

use crate::{bench, parallel, progress, remote::Remote};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize, Deserialize)]
pub struct Sample {
    pub question: String,
    pub seconds: f64,
    pub correct: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Round {
    pub workers: usize,
    pub seconds: f64,
    pub samples: Vec<Sample>,
}

impl Round {
    fn reliable(&self) -> bool {
        !self.samples.is_empty() && self.samples.iter().all(|s| s.correct)
    }
    fn goodput(&self) -> f64 {
        self.samples.iter().filter(|s| s.correct).count() as f64 / self.seconds.max(0.001)
    }
}

/// Prefer fewer workers unless a fully correct round improves goodput by at least 15%.
/// The last observation of a level wins, so the repeated serial baseline detects drift.
pub fn select(rounds: &[Round]) -> Option<usize> {
    let mut latest = BTreeMap::new();
    for round in rounds {
        latest.insert(round.workers, round);
    }
    let mut best: Option<&Round> = None;
    for round in latest.values().filter(|r| r.reliable()) {
        if best.is_none_or(|b| round.goodput() > b.goodput() * 1.15) {
            best = Some(round);
        }
    }
    best.map(|r| r.workers)
}

#[derive(Debug, Serialize)]
struct Report<'a> {
    model: &'a str,
    timeout_seconds: u64,
    retries: usize,
    rounds: &'a [Round],
    selected_workers: Option<usize>,
}

/// Run real hosted requests without retries or local fallback, saving every completed
/// round even if the provider never produces a usable concurrency level.
pub fn run(levels: &str, samples: usize, timeout: u64, output: &str) -> Result<()> {
    let levels: Vec<usize> = levels
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    if samples < 4
        || levels.is_empty()
        || levels.iter().any(|n| *n == 0 || *n > samples || *n > 32)
        || !(1..=900).contains(&timeout)
    {
        bail!(
            "use at least four samples, worker counts from 1 to min(samples,32), and a 1–900 second timeout"
        );
    }
    let mut remote =
        Remote::from_env().ok_or_else(|| anyhow!("PAPER_TRADER_API_KEY is required for tuning"))?;
    remote.timeout = Duration::from_secs(timeout);
    let questions = bench::questions();
    let jobs: Vec<_> = (0..samples).collect();
    let mut rounds = Vec::new();
    for (index, workers) in levels.into_iter().enumerate() {
        progress::log(&format!(
            "benchmark round {}: {workers} workers, {samples} questions, {timeout}s request timeout, no retries",
            index + 1
        ));
        let start = Instant::now();
        let answers = parallel::map(&jobs, workers, |job| {
            let (name, system, user, schema, check) = &questions[job % questions.len()];
            let user = format!(
                "{user}\nBenchmark sample {}-{}; use the same decision rules.",
                index + 1,
                job + 1
            );
            let started = Instant::now();
            let (correct, error) = match remote.probe(system, &user, schema) {
                Ok((value, _, _)) => (check(&value) >= 1.0, None),
                Err(error) => (false, Some(format!("{error:#}"))),
            };
            let sample = Sample {
                question: (*name).into(),
                seconds: started.elapsed().as_secs_f64(),
                correct,
                error,
            };
            progress::log(&format!(
                "benchmark round {} sample {} {name}: correct={correct}, {:.1}s, error={:?}",
                index + 1,
                job + 1,
                sample.seconds,
                sample.error
            ));
            sample
        });
        let round = Round {
            workers,
            seconds: start.elapsed().as_secs_f64(),
            samples: answers,
        };
        println!("{}", serde_json::to_string(&round)?);
        rounds.push(round);
        let report = Report {
            model: &remote.model,
            timeout_seconds: timeout,
            retries: 0,
            selected_workers: select(&rounds),
            rounds: &rounds,
        };
        std::fs::write(output, serde_json::to_string_pretty(&report)?)?;
    }
    let selected = select(&rounds).ok_or_else(|| anyhow!("no concurrency level answered every question correctly within the timeout; no replay will be dispatched"))?;
    progress::log(&format!(
        "benchmark selected {selected} workers; current-provider sample, not a permanent capacity guarantee"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn round(workers: usize, seconds: f64, correct: bool) -> Round {
        Round {
            workers,
            seconds,
            samples: (0..4)
                .map(|_| Sample {
                    question: "fixture".into(),
                    seconds: seconds / 4.0,
                    correct,
                    error: None,
                })
                .collect(),
        }
    }
    #[test]
    fn errors_do_not_beat_reliable_results_and_small_gains_do_not_add_workers() {
        assert_eq!(
            select(&[
                round(1, 100.0, true),
                round(2, 95.0, true),
                round(4, 10.0, false)
            ]),
            Some(1)
        );
        assert_eq!(
            select(&[
                round(1, 100.0, true),
                round(2, 60.0, true),
                round(4, 20.0, false)
            ]),
            Some(2)
        );
        assert_eq!(select(&[round(1, 100.0, false)]), None);
    }
    #[test]
    fn repeated_baseline_replaces_the_earlier_observation() {
        assert_eq!(
            select(&[
                round(1, 100.0, true),
                round(2, 60.0, true),
                round(1, 30.0, true)
            ]),
            Some(1)
        );
    }
}
