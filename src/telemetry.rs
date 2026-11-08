//! Logs and metrics for every stage and model call of a run.
//!
//! One small SQLite file (`telemetry.sqlite`) sits next to the strategy registry and is
//! committed with it. Each run records its stages as timed spans, every model call with
//! latency and token rates, data-source requests, and notable events. The dashboard and the
//! GitHub step summary read aggregates from here to show where a run spends its time and
//! which model or source misbehaves. Recording never fails the caller: write errors are
//! swallowed because telemetry must not be able to break a trading run.

use crate::ledger;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

/// How many runs are kept on disk.
const KEEP_RUNS: i64 = 120;

/// Facts about the machine and trigger of one run.
#[derive(Clone, Debug, Default)]
pub struct RunMeta {
    /// `schedule`, `manual`, `webhook` or `local`.
    pub trigger: String,
    pub git_sha: String,
    pub cpu: String,
    pub cores: usize,
    pub ram_mb: u64,
    pub desk_minutes: f64,
}

/// One model call.
#[derive(Clone, Debug, Default)]
pub struct CallRecord {
    /// `triage`, `research`, `findings`, `bull`, `bear`, `judge`, `doctor`, `post_mortem`,
    /// `proposer`, `single_shot` or `canary`.
    pub stage: String,
    pub role: String,
    pub model_id: String,
    pub company: String,
    pub prompt_chars: usize,
    pub prompt_hash: String,
    pub output_chars: usize,
    pub output_hash: String,
    pub latency_ms: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub prompt_tps: Option<f64>,
    pub gen_tps: Option<f64>,
    pub ok: bool,
    /// First 300 characters of the error.
    pub error: Option<String>,
}

/// How a run ended.
#[derive(Clone, Debug, Default)]
pub struct RunOutcome {
    /// `ok`, `degraded` or `failed`.
    pub status: String,
    pub minutes_used: f64,
    pub minutes_budget: f64,
    pub degraded: Vec<String>,
    pub notes: String,
}

/// The telemetry store for one run.
#[derive(Debug)]
pub struct Telemetry {
    conn: Mutex<Connection>,
    run_id: String,
}

/// Times one stage. The row is written when the guard is dropped, as a success unless
/// [`SpanGuard::fail`] was called.
#[derive(Debug)]
pub struct SpanGuard<'a> {
    telemetry: &'a Telemetry,
    name: String,
    started_at: f64,
    clock: Instant,
    outcome: Option<(bool, String)>,
}

impl SpanGuard<'_> {
    /// End the span as a success.
    pub fn ok(mut self) {
        self.outcome = Some((true, String::new()));
    }

    /// End the span as a failure with a short note.
    pub fn fail(mut self, note: &str) {
        self.outcome = Some((false, note.chars().take(300).collect()));
    }
}

impl Drop for SpanGuard<'_> {
    fn drop(&mut self) {
        let (ok, note) = self.outcome.take().unwrap_or((true, String::new()));
        let ms = i64::try_from(self.clock.elapsed().as_millis()).unwrap_or(i64::MAX);
        let conn = self.telemetry.lock();
        let _ = conn.execute(
            "INSERT INTO stages(run_id,name,started_at,ms,ok,note) VALUES(?,?,?,?,?,?)",
            params![
                self.telemetry.run_id,
                self.name,
                self.started_at,
                ms,
                ok,
                note
            ],
        );
    }
}

/// A stable 16-character hash of `text` (FNV-1a, 64 bit), used to compare prompts and
/// outputs across runs without storing them.
pub fn short_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS runs (
        run_id TEXT PRIMARY KEY, started_at REAL, ended_at REAL, trigger TEXT, git_sha TEXT,
        cpu TEXT, cores INTEGER, ram_mb INTEGER, desk_minutes REAL, status TEXT,
        minutes_used REAL, minutes_budget REAL, degraded TEXT, notes TEXT);
    CREATE TABLE IF NOT EXISTS stages (
        id INTEGER PRIMARY KEY, run_id TEXT, name TEXT, started_at REAL, ms INTEGER,
        ok INTEGER, note TEXT);
    CREATE TABLE IF NOT EXISTS llm_calls (
        id INTEGER PRIMARY KEY, run_id TEXT, at REAL, stage TEXT, role TEXT, model_id TEXT,
        company TEXT, prompt_chars INTEGER, prompt_hash TEXT, output_chars INTEGER,
        output_hash TEXT, latency_ms INTEGER, prompt_tokens INTEGER, completion_tokens INTEGER,
        prompt_tps REAL, gen_tps REAL, ok INTEGER, error TEXT);
    CREATE TABLE IF NOT EXISTS events (
        id INTEGER PRIMARY KEY, run_id TEXT, at REAL, kind TEXT, subject TEXT, detail TEXT);
    CREATE TABLE IF NOT EXISTS fetches (
        id INTEGER PRIMARY KEY, run_id TEXT, at REAL, source TEXT, ok INTEGER, ms INTEGER);
    CREATE INDEX IF NOT EXISTS stages_run ON stages(run_id);
    CREATE INDEX IF NOT EXISTS calls_run ON llm_calls(run_id);
    CREATE INDEX IF NOT EXISTS events_run ON events(run_id);
    CREATE INDEX IF NOT EXISTS fetches_run ON fetches(run_id);";

const LAST_RUNS: &str = "SELECT run_id FROM runs ORDER BY started_at DESC LIMIT 30";

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Nearest-rank 95th percentile.
fn p95(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (0.95 * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

fn rate(ok: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| ok as f64 / total as f64)
}

fn human_ms(ms: f64) -> String {
    if ms >= 120_000.0 {
        format!("{:.1} min", ms / 60_000.0)
    } else if ms >= 1_000.0 {
        format!("{:.1} s", ms / 1_000.0)
    } else {
        format!("{ms:.0} ms")
    }
}

fn csv_cell(value: Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::String(s) => s,
        other => other.to_string(),
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

impl Telemetry {
    /// Open (or create) the store at `path` and record the start of run `run_id`.
    pub fn open(path: &str, run_id: &str, meta: &RunMeta, now: f64) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        Self::start(conn, run_id, meta, now)
    }

    /// An in-memory store, for tests.
    pub fn open_memory(run_id: &str, meta: &RunMeta, now: f64) -> Result<Self> {
        Self::start(Connection::open_in_memory()?, run_id, meta, now)
    }

    fn start(conn: Connection, run_id: &str, meta: &RunMeta, now: f64) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR REPLACE INTO runs(run_id,started_at,trigger,git_sha,cpu,cores,ram_mb,desk_minutes,status,degraded,notes) \
             VALUES(?,?,?,?,?,?,?,?,'running','[]','')",
            params![
                run_id,
                now,
                meta.trigger,
                meta.git_sha,
                meta.cpu,
                i64::try_from(meta.cores).unwrap_or(0),
                i64::try_from(meta.ram_mb).unwrap_or(0),
                meta.desk_minutes
            ],
        )?;
        prune(&conn)?;
        Ok(Telemetry {
            conn: Mutex::new(conn),
            run_id: run_id.to_string(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn insert_call(&self, run_id: &str, at: f64, c: &CallRecord) {
        let conn = self.lock();
        let error: Option<String> = c.error.as_ref().map(|e| e.chars().take(300).collect());
        let _ = conn.execute(
            "INSERT INTO llm_calls(run_id,at,stage,role,model_id,company,prompt_chars,prompt_hash,output_chars,output_hash,\
             latency_ms,prompt_tokens,completion_tokens,prompt_tps,gen_tps,ok,error) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                run_id,
                at,
                c.stage,
                c.role,
                c.model_id,
                c.company,
                i64::try_from(c.prompt_chars).unwrap_or(0),
                c.prompt_hash,
                i64::try_from(c.output_chars).unwrap_or(0),
                c.output_hash,
                i64::try_from(c.latency_ms).unwrap_or(i64::MAX),
                c.prompt_tokens.and_then(|v| i64::try_from(v).ok()),
                c.completion_tokens.and_then(|v| i64::try_from(v).ok()),
                c.prompt_tps,
                c.gen_tps,
                c.ok,
                error
            ],
        );
    }

    /// Record one model call. Never fails the caller.
    pub fn call(&self, c: &CallRecord) {
        self.insert_call(&self.run_id, ledger::now(), c);
    }

    /// Start timing a stage.
    pub fn span(&self, name: &str) -> SpanGuard<'_> {
        SpanGuard {
            telemetry: self,
            name: name.to_string(),
            started_at: ledger::now(),
            clock: Instant::now(),
            outcome: None,
        }
    }

    /// Record a notable event. `kind` is `error`, `degraded`, `determinism`, `model_load`,
    /// `http` or `info`.
    pub fn event(&self, kind: &str, subject: &str, detail: &str) {
        let conn = self.lock();
        let _ = conn.execute(
            "INSERT INTO events(run_id,at,kind,subject,detail) VALUES(?,?,?,?,?)",
            params![
                self.run_id,
                ledger::now(),
                kind,
                subject,
                detail.chars().take(500).collect::<String>()
            ],
        );
    }

    /// Record one data-source request.
    pub fn fetch(&self, source: &str, ok: bool, ms: u64) {
        let conn = self.lock();
        let _ = conn.execute(
            "INSERT INTO fetches(run_id,at,source,ok,ms) VALUES(?,?,?,?,?)",
            params![
                self.run_id,
                ledger::now(),
                source,
                ok,
                i64::try_from(ms).unwrap_or(i64::MAX)
            ],
        );
    }

    /// Close the run row with its outcome.
    pub fn finish(&self, outcome: &RunOutcome, now: f64) -> Result<()> {
        self.lock().execute(
            "UPDATE runs SET ended_at=?, status=?, minutes_used=?, minutes_budget=?, degraded=?, notes=? WHERE run_id=?",
            params![
                now,
                outcome.status,
                outcome.minutes_used,
                outcome.minutes_budget,
                serde_json::to_string(&outcome.degraded)?,
                outcome.notes,
                self.run_id
            ],
        )?;
        Ok(())
    }

    fn rows(conn: &Connection, sql: &str) -> Vec<Value> {
        ledger::rows(conn, sql).unwrap_or_default()
    }

    /// Aggregates for the dashboard.
    pub fn summary_json(&self) -> Value {
        let conn = self.lock();
        let current = Self::rows(
            &conn,
            &format!(
                "SELECT run_id,status,minutes_used,minutes_budget,started_at,ended_at,trigger,cpu,cores,ram_mb,git_sha \
                 FROM runs WHERE run_id='{}'",
                self.run_id.replace('\'', "''")
            ),
        )
        .into_iter()
        .next()
        .unwrap_or(Value::Null);

        let runs: Vec<Value> = Self::rows(
            &conn,
            "SELECT r.run_id, r.started_at, r.status, r.minutes_used, r.minutes_budget, r.degraded, \
             (SELECT COUNT(*) FROM llm_calls c WHERE c.run_id=r.run_id AND c.stage<>'canary') AS calls, \
             (SELECT COUNT(*) FROM llm_calls c WHERE c.run_id=r.run_id AND c.stage<>'canary' AND c.ok=0) + \
             (SELECT COUNT(*) FROM events e WHERE e.run_id=r.run_id AND e.kind='error') AS errors \
             FROM runs r ORDER BY r.started_at DESC LIMIT 30",
        )
        .into_iter()
        .map(|mut row| {
            let degraded: Value = row["degraded"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!([]));
            row["degraded"] = degraded;
            row
        })
        .collect();

        let stages = Self::rows(
            &conn,
            &format!(
                "SELECT name, ms, ok, note FROM stages WHERE run_id='{}' ORDER BY started_at, id",
                self.run_id.replace('\'', "''")
            ),
        )
        .into_iter()
        .map(|mut row| {
            row["ok"] = json!(row["ok"].as_i64().unwrap_or(1) != 0);
            row
        })
        .collect::<Vec<_>>();

        let mut by_stage: BTreeMap<String, (Vec<f64>, usize, std::collections::BTreeSet<String>)> =
            BTreeMap::new();
        for row in Self::rows(
            &conn,
            &format!("SELECT run_id, name, ms, ok FROM stages WHERE run_id IN ({LAST_RUNS})"),
        ) {
            let entry = by_stage
                .entry(row["name"].as_str().unwrap_or("").to_string())
                .or_default();
            entry.0.push(row["ms"].as_f64().unwrap_or(0.0));
            if row["ok"].as_i64().unwrap_or(1) == 0 {
                entry.1 += 1;
            }
            entry
                .2
                .insert(row["run_id"].as_str().unwrap_or("").to_string());
        }
        let mut stage_history: Vec<Value> = by_stage
            .into_iter()
            .map(|(name, (values, failed, runs))| {
                json!({
                    "name": name, "runs": runs.len(), "mean_ms": mean(&values), "p95_ms": p95(&values),
                    "max_ms": values.iter().copied().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |x| x.max(v)))),
                    "fail_rate": rate(failed, values.len()),
                })
            })
            .collect();
        stage_history.sort_by(|a, b| {
            b["mean_ms"]
                .as_f64()
                .partial_cmp(&a["mean_ms"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let calls = Self::rows(
            &conn,
            &format!(
                "SELECT role, model_id, company, latency_ms, completion_tokens, gen_tps, prompt_tps, ok \
                 FROM llm_calls WHERE stage<>'canary' AND run_id IN ({LAST_RUNS})"
            ),
        );
        let mut by_model: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        let mut by_role: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        for call in &calls {
            by_model
                .entry(call["model_id"].as_str().unwrap_or("").to_string())
                .or_default()
                .push(call);
            by_role
                .entry(call["role"].as_str().unwrap_or("").to_string())
                .or_default()
                .push(call);
        }
        let field = |rows: &[&Value], key: &str| -> Vec<f64> {
            rows.iter().filter_map(|r| r[key].as_f64()).collect()
        };
        let ok_count = |rows: &[&Value]| {
            rows.iter()
                .filter(|r| r["ok"].as_i64().unwrap_or(1) != 0)
                .count()
        };
        let first_str = |rows: &[&Value], key: &str| {
            rows.first()
                .and_then(|r| r[key].as_str())
                .unwrap_or("")
                .to_string()
        };

        let mut models: Vec<Value> = by_model
            .iter()
            .map(|(id, rows)| {
                let latency = field(rows, "latency_ms");
                let roles: std::collections::BTreeSet<String> =
                    rows.iter().filter_map(|r| r["role"].as_str().map(str::to_string)).collect();
                json!({
                    "model_id": id, "company": first_str(rows, "company"), "roles": roles,
                    "calls": rows.len(), "ok_rate": rate(ok_count(rows), rows.len()),
                    "mean_latency_ms": mean(&latency), "p95_latency_ms": p95(&latency),
                    "mean_gen_tps": mean(&field(rows, "gen_tps")), "mean_prompt_tps": mean(&field(rows, "prompt_tps")),
                    "mean_completion_tokens": mean(&field(rows, "completion_tokens")),
                    "errors": rows.len() - ok_count(rows),
                })
            })
            .collect();
        models.sort_by(|a, b| {
            b["mean_latency_ms"]
                .as_f64()
                .partial_cmp(&a["mean_latency_ms"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let roles: Vec<Value> = by_role
            .iter()
            .map(|(role, rows)| {
                let latency = field(rows, "latency_ms");
                json!({
                    "role": role, "model_id": first_str(rows, "model_id"), "company": first_str(rows, "company"),
                    "calls": rows.len(), "ok_rate": rate(ok_count(rows), rows.len()),
                    "mean_latency_ms": mean(&latency), "total_minutes": latency.iter().sum::<f64>() / 60_000.0,
                })
            })
            .collect();

        let mut by_source: BTreeMap<String, (usize, Vec<f64>)> = BTreeMap::new();
        for row in Self::rows(
            &conn,
            &format!("SELECT source, ok, ms FROM fetches WHERE run_id IN ({LAST_RUNS})"),
        ) {
            let entry = by_source
                .entry(row["source"].as_str().unwrap_or("").to_string())
                .or_default();
            if row["ok"].as_i64().unwrap_or(1) != 0 {
                entry.0 += 1;
            }
            entry.1.push(row["ms"].as_f64().unwrap_or(0.0));
        }
        let fetches: Vec<Value> = by_source
            .into_iter()
            .map(|(source, (ok, ms))| {
                json!({"source": source, "requests": ms.len(), "ok_rate": rate(ok, ms.len()), "mean_ms": mean(&ms), "p95_ms": p95(&ms)})
            })
            .collect();

        let events = Self::rows(
            &conn,
            "SELECT at, kind, subject, detail FROM events ORDER BY id DESC LIMIT 40",
        );

        let mut canaries: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for row in Self::rows(
            &conn,
            "SELECT c.model_id AS model_id, c.run_id AS run_id, c.output_hash AS hash FROM llm_calls c \
             JOIN runs r ON r.run_id=c.run_id WHERE c.stage='canary' ORDER BY r.started_at DESC, c.id DESC",
        ) {
            let id = row["model_id"].as_str().unwrap_or("").to_string();
            let run = row["run_id"].as_str().unwrap_or("").to_string();
            let hash = row["hash"].as_str().unwrap_or("").to_string();
            let seen = canaries.entry(id).or_default();
            if !seen.iter().any(|(r, _)| *r == run) {
                seen.push((run, hash));
            }
        }
        let determinism: Vec<Value> = canaries
            .into_iter()
            .map(|(model_id, seen)| {
                let latest = seen.first().map(|s| s.1.clone());
                let previous = seen.get(1).map(|s| s.1.clone());
                let stable = previous.as_ref().zip(latest.as_ref()).map(|(p, l)| p == l);
                json!({"model_id": model_id, "canary_hash_latest": latest, "canary_hash_previous": previous, "stable": stable})
            })
            .collect();

        let totals = Self::rows(
            &conn,
            "SELECT (SELECT COUNT(*) FROM runs) AS runs, \
             (SELECT COUNT(*) FROM llm_calls WHERE stage<>'canary') AS calls, \
             (SELECT COUNT(*) FROM llm_calls WHERE stage<>'canary' AND ok=0) + (SELECT COUNT(*) FROM events WHERE kind='error') AS errors, \
             (SELECT COALESCE(SUM(minutes_used),0) FROM runs) AS minutes_total",
        )
        .into_iter()
        .next()
        .unwrap_or(Value::Null);

        json!({
            "current": current, "runs": runs, "stages": stages, "stage_history": stage_history,
            "models": models, "roles": roles, "fetches": fetches, "events": events,
            "determinism": determinism, "totals": totals,
        })
    }

    /// A GitHub step-summary report of this run.
    pub fn markdown_summary(&self) -> String {
        let summary = self.summary_json();
        let current = &summary["current"];
        let mut out = String::new();
        let _ = writeln!(
            out,
            "### Run {}: {}\n",
            current["run_id"].as_str().unwrap_or(&self.run_id),
            current["status"].as_str().unwrap_or("running")
        );
        let _ = writeln!(
            out,
            "Used {:.1} of {:.1} model minutes on {} ({} cores, {} MB). Trigger: {}.\n",
            current["minutes_used"].as_f64().unwrap_or(0.0),
            current["minutes_budget"].as_f64().unwrap_or(0.0),
            current["cpu"]
                .as_str()
                .filter(|c| !c.is_empty())
                .unwrap_or("unknown CPU"),
            current["cores"].as_i64().unwrap_or(0),
            current["ram_mb"].as_i64().unwrap_or(0),
            current["trigger"].as_str().unwrap_or("unknown")
        );
        let mut stages: Vec<&Value> = summary["stages"].as_array().into_iter().flatten().collect();
        stages.sort_by(|a, b| {
            b["ms"]
                .as_f64()
                .partial_cmp(&a["ms"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let slowest = stages
            .first()
            .and_then(|s| s["ms"].as_f64())
            .unwrap_or(1.0)
            .max(1.0);
        out.push_str("#### Where the time went\n\n| Stage | Time | |\n|---|---:|---|\n");
        for s in stages.iter().take(15) {
            let ms = s["ms"].as_f64().unwrap_or(0.0);
            let bar = "█".repeat(((ms / slowest) * 20.0).round() as usize);
            let flag = if s["ok"].as_bool().unwrap_or(true) {
                ""
            } else {
                " (failed)"
            };
            let _ = writeln!(
                out,
                "| {}{flag} | {} | {bar} |",
                s["name"].as_str().unwrap_or(""),
                human_ms(ms)
            );
        }
        out.push_str("\n#### Models\n\n| Model | Company | Calls | Mean latency | Tokens/s | Errors |\n|---|---|---:|---:|---:|---:|\n");
        let current_models = self.models_for_current_run();
        for m in &current_models {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} |",
                m["model_id"].as_str().unwrap_or(""),
                m["company"].as_str().unwrap_or(""),
                m["calls"],
                m["mean_latency_ms"]
                    .as_f64()
                    .map_or("-".to_string(), human_ms),
                m["mean_gen_tps"]
                    .as_f64()
                    .map_or("-".to_string(), |v| format!("{v:.1}")),
                m["errors"]
            );
        }
        let fails = self.fetch_failures_for_current_run();
        if !fails.is_empty() {
            out.push_str("\n#### Data-fetch failures\n\n");
            for (source, failed, total) in fails {
                let _ = writeln!(out, "- {source}: {failed} of {total} requests failed");
            }
        }
        let notable: Vec<&Value> = summary["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| {
                matches!(
                    e["kind"].as_str(),
                    Some("error" | "degraded" | "determinism")
                )
            })
            .take(10)
            .collect();
        if !notable.is_empty() {
            out.push_str("\n#### Notable events\n\n");
            for e in notable {
                let _ = writeln!(
                    out,
                    "- {} {}: {}",
                    e["kind"].as_str().unwrap_or(""),
                    e["subject"].as_str().unwrap_or(""),
                    e["detail"].as_str().unwrap_or("")
                );
            }
        }
        out
    }

    fn models_for_current_run(&self) -> Vec<Value> {
        let conn = self.lock();
        let rows = Self::rows(
            &conn,
            &format!(
                "SELECT model_id, company, latency_ms, gen_tps, ok FROM llm_calls WHERE run_id='{}' AND stage<>'canary'",
                self.run_id.replace('\'', "''")
            ),
        );
        let mut grouped: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        for r in &rows {
            grouped
                .entry(r["model_id"].as_str().unwrap_or("").to_string())
                .or_default()
                .push(r);
        }
        let mut out: Vec<Value> = grouped
            .iter()
            .map(|(id, calls)| {
                let latency: Vec<f64> = calls
                    .iter()
                    .filter_map(|c| c["latency_ms"].as_f64())
                    .collect();
                let tps: Vec<f64> = calls.iter().filter_map(|c| c["gen_tps"].as_f64()).collect();
                json!({
                    "model_id": id, "company": calls[0]["company"], "calls": calls.len(),
                    "mean_latency_ms": mean(&latency), "mean_gen_tps": mean(&tps),
                    "errors": calls.iter().filter(|c| c["ok"].as_i64().unwrap_or(1) == 0).count(),
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b["mean_latency_ms"]
                .as_f64()
                .partial_cmp(&a["mean_latency_ms"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    fn fetch_failures_for_current_run(&self) -> Vec<(String, usize, usize)> {
        let conn = self.lock();
        let rows = Self::rows(
            &conn,
            &format!(
                "SELECT source, ok FROM fetches WHERE run_id='{}'",
                self.run_id.replace('\'', "''")
            ),
        );
        let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for r in rows {
            let entry = counts
                .entry(r["source"].as_str().unwrap_or("").to_string())
                .or_default();
            entry.1 += 1;
            if r["ok"].as_i64().unwrap_or(1) == 0 {
                entry.0 += 1;
            }
        }
        counts
            .into_iter()
            .filter(|(_, (failed, _))| *failed > 0)
            .map(|(s, (f, t))| (s, f, t))
            .collect()
    }

    /// Write `runs.csv`, `stages.csv`, `llm_calls.csv`, `events.csv` and `fetches.csv`.
    pub fn export_csv(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let conn = self.lock();
        for table in ["runs", "stages", "llm_calls", "events", "fetches"] {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
            let names: Vec<String> = stmt.column_names().map(str::to_string).collect();
            let mut text = names.join(",");
            text.push('\n');
            let mut cursor = stmt.query([])?;
            while let Some(row) = cursor.next()? {
                let cells: Vec<String> = (0..names.len())
                    .map(|i| row.get_ref(i).map(|v| csv_cell(ledger::value_ref(v))))
                    .collect::<Result<_, _>>()?;
                text.push_str(&cells.join(","));
                text.push('\n');
            }
            std::fs::write(dir.join(format!("{table}.csv")), text)?;
        }
        Ok(())
    }
}

/// Keep only the newest runs and the rows that belong to them.
fn prune(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM runs WHERE run_id NOT IN (SELECT run_id FROM runs ORDER BY started_at DESC LIMIT ?)",
        params![KEEP_RUNS],
    )?;
    for table in ["stages", "llm_calls", "events", "fetches"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE run_id NOT IN (SELECT run_id FROM runs)"),
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> RunMeta {
        RunMeta {
            trigger: "local".into(),
            git_sha: "abc123".into(),
            cpu: "Test CPU".into(),
            cores: 4,
            ram_mb: 16_000,
            desk_minutes: 120.0,
        }
    }

    fn call(stage: &str, role: &str, model: &str, latency: u64, ok: bool) -> CallRecord {
        CallRecord {
            stage: stage.into(),
            role: role.into(),
            model_id: model.into(),
            company: format!("{model}-co"),
            prompt_chars: 100,
            prompt_hash: short_hash("p"),
            output_chars: 50,
            output_hash: short_hash("o"),
            latency_ms: latency,
            prompt_tokens: Some(30),
            completion_tokens: Some(20),
            prompt_tps: Some(100.0),
            gen_tps: Some(8.0),
            ok,
            error: (!ok).then(|| "boom".to_string()),
        }
    }

    fn add_run(t: &Telemetry, id: &str, started: f64) {
        t.lock()
            .execute("INSERT INTO runs(run_id,started_at,status,degraded,notes) VALUES(?,?, 'ok','[]','')", params![id, started])
            .unwrap();
    }

    #[test]
    fn short_hash_is_stable_and_distinguishes_text() {
        assert_eq!(short_hash("hello"), short_hash("hello"));
        assert_ne!(short_hash("hello"), short_hash("hellp"));
        assert_eq!(short_hash("hello").len(), 16);
    }

    #[test]
    fn p95_uses_nearest_rank() {
        let values: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(p95(&values), Some(19.0));
        assert_eq!(p95(&[5.0]), Some(5.0));
        assert_eq!(p95(&[]), None);
    }

    #[test]
    fn summary_has_the_documented_shape_and_math() {
        let t = Telemetry::open_memory("r1", &meta(), 1000.0).unwrap();
        for ms in 1..=20_u64 {
            t.call(&call("judge", "judge", "qwen", ms * 100, true));
        }
        t.call(&call("bull", "bull", "llama", 500, false));
        t.call(&call("canary", "canary", "qwen", 50, true));
        t.span("scan").ok();
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.span("desk.judge").ok();
        t.fetch("yahoo", true, 100);
        t.fetch("yahoo", false, 900);
        t.event("error", "bull", "model failed");
        t.finish(
            &RunOutcome {
                status: "degraded".into(),
                minutes_used: 12.5,
                minutes_budget: 120.0,
                degraded: vec!["bull".into()],
                notes: String::new(),
            },
            2000.0,
        )
        .unwrap();
        let s = t.summary_json();
        assert_eq!(s["current"]["status"], "degraded");
        assert_eq!(s["current"]["cores"], 4);
        assert_eq!(s["runs"][0]["calls"], 21);
        assert_eq!(s["runs"][0]["errors"], 2);
        assert_eq!(s["runs"][0]["degraded"][0], "bull");
        assert_eq!(s["stages"].as_array().unwrap().len(), 2);
        assert_eq!(s["stages"][0]["name"], "scan");
        let qwen = s["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["model_id"] == "qwen")
            .unwrap();
        assert_eq!(qwen["calls"], 20);
        assert_eq!(qwen["p95_latency_ms"], 1900.0);
        assert_eq!(qwen["company"], "qwen-co");
        let llama = s["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["model_id"] == "llama")
            .unwrap();
        assert_eq!(llama["errors"], 1);
        assert_eq!(llama["ok_rate"], 0.0);
        let yahoo = &s["fetches"][0];
        assert_eq!(yahoo["requests"], 2);
        assert_eq!(yahoo["ok_rate"], 0.5);
        assert_eq!(s["events"][0]["kind"], "error");
        assert_eq!(s["totals"]["calls"], 21);
        assert_eq!(s["totals"]["errors"], 2);
        assert!((s["totals"]["minutes_total"].as_f64().unwrap() - 12.5).abs() < 1e-9);
        assert!(
            s["roles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["role"] == "judge")
        );
        assert_ne!(s["stage_history"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn determinism_compares_the_latest_two_runs() {
        let t = Telemetry::open_memory("today", &meta(), 3000.0).unwrap();
        add_run(&t, "yesterday", 2000.0);
        let canary = |hash: &str| CallRecord {
            output_hash: hash.into(),
            ..call("canary", "canary", "qwen", 10, true)
        };
        t.insert_call("yesterday", 2001.0, &canary("aaaa"));
        t.insert_call("today", 3001.0, &canary("aaaa"));
        let s = t.summary_json();
        assert_eq!(s["determinism"][0]["stable"], true);
        t.insert_call("today", 3002.0, &canary("bbbb"));
        let s = t.summary_json();
        assert_eq!(s["determinism"][0]["canary_hash_latest"], "bbbb");
        assert_eq!(s["determinism"][0]["canary_hash_previous"], "aaaa");
        assert_eq!(s["determinism"][0]["stable"], false);
    }

    #[test]
    fn determinism_is_null_with_a_single_run() {
        let t = Telemetry::open_memory("only", &meta(), 1.0).unwrap();
        t.insert_call("only", 2.0, &call("canary", "canary", "qwen", 10, true));
        assert!(t.summary_json()["determinism"][0]["stable"].is_null());
    }

    #[test]
    fn opening_prunes_to_the_newest_120_runs() {
        let dir = std::env::temp_dir().join(format!("pt-telemetry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("telemetry.sqlite").to_string_lossy().into_owned();
        let first = Telemetry::open(&path, "seed", &meta(), 1.0).unwrap();
        for i in 0..130 {
            add_run(&first, &format!("old{i}"), 10.0 + f64::from(i));
            first.insert_call(
                &format!("old{i}"),
                10.0,
                &call("judge", "judge", "qwen", 1, true),
            );
        }
        drop(first);
        let second = Telemetry::open(&path, "latest", &meta(), 9999.0).unwrap();
        let conn = second.lock();
        let runs: i64 = conn
            .query_row("SELECT COUNT(*) FROM runs", [], |r| r.get(0))
            .unwrap();
        let calls: i64 = conn
            .query_row("SELECT COUNT(*) FROM llm_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, 120);
        assert_eq!(calls, 119);
        let newest: i64 = conn
            .query_row("SELECT COUNT(*) FROM runs WHERE run_id='latest'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(newest, 1);
    }

    #[test]
    fn spans_record_failures_and_drops() {
        let t = Telemetry::open_memory("r", &meta(), 1.0).unwrap();
        t.span("export").fail("disk full");
        {
            let _guard = t.span("scan");
        }
        t.span("intel").ok();
        let s = t.summary_json();
        let stages = s["stages"].as_array().unwrap();
        assert_eq!(stages.len(), 3);
        let export = stages.iter().find(|x| x["name"] == "export").unwrap();
        assert_eq!(export["ok"], false);
        assert_eq!(export["note"], "disk full");
        let scan = stages.iter().find(|x| x["name"] == "scan").unwrap();
        assert_eq!(scan["ok"], true);
    }

    #[test]
    fn markdown_names_the_slowest_stage_and_the_failures() {
        let t = Telemetry::open_memory("md", &meta(), 1.0).unwrap();
        {
            let _slow = t.span("desk.research");
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        t.span("export").ok();
        t.call(&call("judge", "judge", "qwen", 2000, true));
        t.fetch("edgar", false, 50);
        t.event("degraded", "research", "ran out of time");
        let text = t.markdown_summary();
        let slowest = text.find("desk.research").unwrap();
        let fast = text.find("| export").unwrap();
        assert!(slowest < fast, "slowest stage should come first:\n{text}");
        assert!(text.contains("qwen"));
        assert!(text.contains("edgar: 1 of 1 requests failed"));
        assert!(text.contains("ran out of time"));
        assert!(text.lines().count() < 60);
    }

    #[test]
    fn export_writes_five_csv_files_with_headers() {
        let t = Telemetry::open_memory("csv", &meta(), 1.0).unwrap();
        t.call(&call("judge", "judge", "qwen", 10, false));
        t.span("scan").ok();
        t.event("info", "x", "has, comma");
        t.fetch("yahoo", true, 5);
        let dir = std::env::temp_dir().join(format!("pt-telemetry-csv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        t.export_csv(&dir).unwrap();
        for (name, header) in [
            ("runs", "run_id,started_at"),
            ("stages", "id,run_id,name"),
            ("llm_calls", "id,run_id,at,stage"),
            ("events", "id,run_id,at,kind"),
            ("fetches", "id,run_id,at,source"),
        ] {
            let text = std::fs::read_to_string(dir.join(format!("{name}.csv"))).unwrap();
            assert!(text.starts_with(header), "{name}: {text}");
            assert!(text.lines().count() >= 2, "{name} has no rows");
        }
        let events = std::fs::read_to_string(dir.join("events.csv")).unwrap();
        assert!(events.contains("\"has, comma\""));
    }
}
