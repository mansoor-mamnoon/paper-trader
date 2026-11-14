//! What each run measured about itself, for the ops page: how long every stage took, how
//! each agent and data source performed, model downloads and loads, the machine it ran
//! on, and how much the book traded.

use crate::http::SourceStats;
use crate::llm::{Call, ModelEvent};
use crate::progress;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Instant;

/// One timed stage of a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub name: String,
    pub seconds: f64,
}

/// A stage being timed. It shows in the progress log while it runs.
#[derive(Debug)]
pub struct Lap {
    name: String,
    started: Instant,
    _live: progress::Activity,
}

impl Lap {
    /// Start a stage and print it.
    pub fn start(name: &str) -> Self {
        Lap {
            name: name.into(),
            started: Instant::now(),
            _live: progress::begin(name),
        }
    }

    /// Start a sub-step without printing it.
    pub fn quiet(name: &str) -> Self {
        Lap {
            name: name.into(),
            started: Instant::now(),
            _live: progress::track(name),
        }
    }

    pub fn end(self) -> Stage {
        Stage {
            seconds: self.started.elapsed().as_secs_f64(),
            name: self.name,
        }
    }
}

/// One agent's model calls in a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentStats {
    pub agent: String,
    pub model: String,
    pub calls: u64,
    pub failures: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub seconds: f64,
    pub max_seconds: f64,
    /// Prompt tokens read per second over all calls.
    pub prompt_per_second: f64,
    /// Answer tokens written per second over all calls.
    pub tokens_per_second: f64,
}

/// One slow model call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SlowCall {
    pub agent: String,
    pub subject: String,
    pub seconds: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Where and why the run happened.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub os: String,
    pub cpu: String,
    pub cores: usize,
    pub ram_mb: u64,
    pub trigger: String,
    pub run_id: String,
    pub git_sha: String,
}

/// What the books did in the run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Trading {
    pub fills: u64,
    pub bought: f64,
    pub sold: f64,
    pub fees: f64,
    /// Dollars traded as a share of the account, in percent.
    pub turnover_pct: f64,
    pub orders_queued: u64,
}

/// Everything a run measured.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    pub started_at: f64,
    pub finished_at: f64,
    pub seconds: f64,
    pub stages: Vec<Stage>,
    pub agents: Vec<AgentStats>,
    pub slowest: Vec<SlowCall>,
    pub sources: Vec<SourceStats>,
    pub model: Vec<ModelEvent>,
    pub machine: Machine,
    pub trading: Trading,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Seconds spent waiting on the model, summed over calls.
    pub model_seconds: f64,
    pub database_bytes: u64,
}

/// Sum the calls by agent, in the order agents first appear.
pub fn agent_stats(calls: &[Call]) -> Vec<AgentStats> {
    let mut order: Vec<String> = Vec::new();
    let mut by: BTreeMap<String, AgentStats> = BTreeMap::new();
    let mut read_seconds: BTreeMap<String, f64> = BTreeMap::new();
    let mut write_seconds: BTreeMap<String, f64> = BTreeMap::new();
    for call in calls {
        if !by.contains_key(&call.agent) {
            order.push(call.agent.clone());
        }
        let stats = by.entry(call.agent.clone()).or_insert_with(|| AgentStats {
            agent: call.agent.clone(),
            model: call.model.clone(),
            ..AgentStats::default()
        });
        stats.calls += 1;
        stats.failures += u64::from(!call.ok);
        stats.prompt_tokens += call.prompt_tokens;
        stats.completion_tokens += call.completion_tokens;
        stats.seconds += call.seconds;
        stats.max_seconds = stats.max_seconds.max(call.seconds);
        if call.prompt_per_second > 0.0 {
            *read_seconds.entry(call.agent.clone()).or_default() +=
                call.prompt_tokens as f64 / call.prompt_per_second;
        }
        if call.tokens_per_second > 0.0 {
            *write_seconds.entry(call.agent.clone()).or_default() +=
                call.completion_tokens as f64 / call.tokens_per_second;
        }
    }
    order
        .into_iter()
        .filter_map(|agent| {
            let mut stats = by.remove(&agent)?;
            let read = read_seconds.get(&agent).copied().unwrap_or(0.0);
            let write = write_seconds.get(&agent).copied().unwrap_or(0.0);
            if read > 0.0 {
                stats.prompt_per_second = stats.prompt_tokens as f64 / read;
            }
            if write > 0.0 {
                stats.tokens_per_second = stats.completion_tokens as f64 / write;
            }
            Some(stats)
        })
        .collect()
}

/// The `n` slowest calls, slowest first.
pub fn slowest(calls: &[Call], n: usize) -> Vec<SlowCall> {
    let mut sorted: Vec<&Call> = calls.iter().collect();
    sorted.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
    sorted
        .into_iter()
        .take(n)
        .map(|c| SlowCall {
            agent: c.agent.clone(),
            subject: c.subject.clone(),
            seconds: c.seconds,
            prompt_tokens: c.prompt_tokens,
            completion_tokens: c.completion_tokens,
        })
        .collect()
}

fn command(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// The machine and the trigger, from the system and the GitHub Actions environment.
pub fn machine() -> Machine {
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let cpu = read("/proc/cpuinfo")
        .lines()
        .find_map(|l| {
            l.strip_prefix("model name")
                .and_then(|r| r.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| command("sysctl", &["-n", "machdep.cpu.brand_string"]));
    let ram_mb = read("/proc/meminfo")
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|r| r.split_whitespace().next()?.parse::<u64>().ok())
        .map_or_else(
            || {
                command("sysctl", &["-n", "hw.memsize"])
                    .parse::<u64>()
                    .map_or(0, |b| b / 1024 / 1024)
            },
            |kb| kb / 1024,
        );
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    Machine {
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        cpu,
        cores: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        ram_mb,
        trigger: match env("GITHUB_EVENT_NAME").as_str() {
            "schedule" => "schedule".into(),
            "workflow_dispatch" => "manual".into(),
            "repository_dispatch" => "webhook".into(),
            _ => "local".into(),
        },
        run_id: env("GITHUB_RUN_ID"),
        git_sha: env("GITHUB_SHA").chars().take(7).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(agent: &str, seconds: f64, prompt: u64, out: u64, ok: bool) -> Call {
        Call {
            agent: agent.into(),
            model: "m".into(),
            subject: format!("{agent}-{seconds}"),
            seconds,
            prompt_tokens: prompt,
            completion_tokens: out,
            prompt_per_second: 100.0,
            tokens_per_second: 10.0,
            ok,
            ..Call::default()
        }
    }

    #[test]
    fn calls_are_summed_by_agent_in_order() {
        let calls = [
            call("scout", 10.0, 1000, 100, true),
            call("manager", 60.0, 8000, 400, true),
            call("scout", 20.0, 1000, 100, false),
        ];
        let stats = agent_stats(&calls);
        assert_eq!(stats[0].agent, "scout");
        assert_eq!((stats[0].calls, stats[0].failures), (2, 1));
        assert_eq!(stats[0].prompt_tokens, 2000);
        assert!((stats[0].seconds - 30.0).abs() < 1e-9);
        assert!((stats[0].max_seconds - 20.0).abs() < 1e-9);
        assert!((stats[0].prompt_per_second - 100.0).abs() < 1e-9);
        assert!((stats[0].tokens_per_second - 10.0).abs() < 1e-9);
        let slow = slowest(&calls, 2);
        assert_eq!(slow[0].agent, "manager");
        assert_eq!(slow.len(), 2);
    }

    #[test]
    fn laps_measure_time_and_the_machine_is_described() {
        let stage = Lap::quiet("test").end();
        assert_eq!(stage.name, "test");
        assert!(stage.seconds >= 0.0);
        let m = machine();
        assert!(m.cores >= 1 && !m.os.is_empty());
    }
}
