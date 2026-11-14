//! The local model runtime. It downloads llama.cpp and the pinned model files, keeps one
//! `llama-server` running, swaps the model when the next agent needs a different one, and
//! records every question and answer.

use crate::http;
use crate::models::{Agent, ModelSpec, model_for};
use crate::progress;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The llama.cpp release the runtime uses.
pub const LLAMA_BUILD: &str = "b12211";

/// One question put to a model and what came back.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Call {
    pub at: f64,
    pub agent: String,
    pub model: String,
    /// What the question was about: a sector, a ticker, or `book`.
    pub subject: String,
    pub system: String,
    pub user: String,
    /// The model's answer, or the error text when `ok` is false.
    pub response: String,
    pub ok: bool,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub seconds: f64,
    /// Prompt tokens read per second, as the server reported.
    #[serde(default)]
    pub prompt_per_second: f64,
    /// Answer tokens written per second, as the server reported.
    #[serde(default)]
    pub tokens_per_second: f64,
}

/// A model runtime event worth timing: a download, a server start, a swap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelEvent {
    /// `download`, `load` or `runtime`.
    pub kind: String,
    pub model: String,
    pub seconds: f64,
    pub detail: String,
}

/// Something that answers an agent's question with JSON matching a schema.
pub trait Brain {
    fn ask(
        &self,
        agent: Agent,
        subject: &str,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<Value>;
    /// The display name of the model that answers for `agent`.
    fn model_name(&self, agent: Agent) -> String;
    /// Every call made since the last time this was asked.
    fn take_calls(&self) -> Vec<Call>;
    /// Model downloads and loads since the last time this was asked.
    fn take_events(&self) -> Vec<ModelEvent> {
        Vec::new()
    }
}

/// Find the JSON object in a model's reply.
pub fn extract_json(text: &str) -> Result<Value> {
    let start = text.find('{').ok_or_else(|| anyhow!("no JSON in reply"))?;
    let end = text.rfind('}').ok_or_else(|| anyhow!("no JSON in reply"))?;
    if end < start {
        bail!("no JSON in reply");
    }
    Ok(serde_json::from_str(&text[start..=end])?)
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn cache_dir() -> PathBuf {
    std::env::var("PAPER_TRADER_CACHE").map_or_else(
        |_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".cache/paper-trader-llm")
        },
        PathBuf::from,
    )
}

fn release_asset() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("ubuntu-x64"),
        ("linux", "aarch64") => Ok("ubuntu-arm64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        (os, arch) => bail!("no llama.cpp release for {os}-{arch}"),
    }
}

/// The path to `llama-server`, downloading the pinned llama.cpp release on first use.
/// `PAPER_TRADER_LLAMA_SERVER` names an existing binary instead.
pub fn ensure_server() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("PAPER_TRADER_LLAMA_SERVER")
        && !path.trim().is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    let dir = cache_dir();
    let binary = dir
        .join(format!("llama-{LLAMA_BUILD}"))
        .join("llama-server");
    if binary.exists() {
        return Ok(binary);
    }
    std::fs::create_dir_all(&dir)?;
    let url = format!(
        "https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_BUILD}/llama-{LLAMA_BUILD}-bin-{}.tar.gz",
        release_asset()?
    );
    let _live = progress::begin(&format!("download llama.cpp {LLAMA_BUILD}"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(900)))
        .user_agent("paper-trader")
        .build()
        .into();
    let response = agent.get(&url).call()?;
    let reader = response.into_body().into_reader();
    tar::Archive::new(flate2::read::GzDecoder::new(reader))
        .unpack(&dir)
        .context("could not unpack llama.cpp")?;
    if !binary.exists() {
        bail!("llama.cpp archive did not contain {}", binary.display());
    }
    Ok(binary)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Stream `spec` to `dest`, verifying the SHA-256 before it becomes visible.
fn download(spec: &ModelSpec, dest: &Path) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3 * 3600)))
        .user_agent("paper-trader")
        .build()
        .into();
    let response = agent.get(&spec.url()).call()?;
    let mut reader = response.into_body().into_reader();
    let part = dest.with_extension("part");
    let mut file = File::create(&part)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    let mut total = 0_u64;
    let mut reported = 0_u64;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
        total += n as u64;
        let quarter = total * 4 / spec.bytes.max(1);
        if quarter > reported && quarter < 4 {
            reported = quarter;
            progress::log(&format!("download {} {}%", spec.id, quarter * 25));
        }
    }
    file.flush()?;
    let got = hex(&hasher.finalize());
    if got != spec.sha256 || total != spec.bytes {
        let _ = std::fs::remove_file(&part);
        bail!(
            "{} failed verification: sha256 {got}, {total} bytes",
            spec.id
        );
    }
    std::fs::rename(part, dest)?;
    Ok(())
}

struct Running {
    spec: &'static ModelSpec,
    child: Child,
    path: PathBuf,
}

/// The pinned models, served one at a time by a local `llama-server`.
pub struct LocalBrain {
    models: PathBuf,
    server: PathBuf,
    port: u16,
    /// Delete a model file once the desk has moved on, to bound disk use.
    prune: bool,
    running: RefCell<Option<Running>>,
    calls: RefCell<Vec<Call>>,
    events: RefCell<Vec<ModelEvent>>,
}

impl std::fmt::Debug for LocalBrain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalBrain")
            .field("models", &self.models)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Speed and size of one answer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub prompt_per_second: f64,
    pub tokens_per_second: f64,
}

impl LocalBrain {
    /// Set up from the environment: `PAPER_TRADER_MODELS_DIR` for the model files,
    /// `PAPER_TRADER_LLM_PORT` for the port and `PAPER_TRADER_PRUNE_MODELS` to delete each
    /// model file after use.
    pub fn from_env() -> Result<Self> {
        let models = std::env::var("PAPER_TRADER_MODELS_DIR")
            .map_or_else(|_| cache_dir().join("models"), PathBuf::from);
        let port = std::env::var("PAPER_TRADER_LLM_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8091);
        let mut events = Vec::new();
        Ok(LocalBrain {
            models,
            server: {
                let started = Instant::now();
                let server = ensure_server()?;
                events.push(ModelEvent {
                    kind: "runtime".into(),
                    model: format!("llama.cpp {LLAMA_BUILD}"),
                    seconds: started.elapsed().as_secs_f64(),
                    detail: "find or download llama-server".into(),
                });
                server
            },
            port,
            prune: std::env::var("PAPER_TRADER_PRUNE_MODELS").is_ok(),
            running: RefCell::new(None),
            calls: RefCell::new(Vec::new()),
            events: RefCell::new(events),
        })
    }

    /// Stop the server. The next question starts it again.
    pub fn stop(&self) {
        if let Some(mut running) = self.running.borrow_mut().take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
            if self.prune {
                let _ = std::fs::remove_file(&running.path);
            }
        }
    }

    fn fetch(&self, spec: &ModelSpec) -> Result<PathBuf> {
        let path = self.models.join(spec.file);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == spec.bytes) {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.models)?;
        let _live = progress::begin(&format!(
            "download {} ({} MB)",
            spec.id,
            spec.bytes / 1_000_000
        ));
        let mut last = anyhow!("no attempt made");
        let started = Instant::now();
        for attempt in 1..=3 {
            match download(spec, &path) {
                Ok(()) => {
                    self.events.borrow_mut().push(ModelEvent {
                        kind: "download".into(),
                        model: spec.id.into(),
                        seconds: started.elapsed().as_secs_f64(),
                        detail: format!(
                            "{} MB, attempt {attempt}, sha256 verified",
                            spec.bytes / 1_000_000
                        ),
                    });
                    return Ok(path);
                }
                Err(error) => {
                    progress::log(&format!(
                        "download attempt {attempt} of {} failed: {error}",
                        spec.id
                    ));
                    last = error;
                    std::thread::sleep(Duration::from_secs(5 * attempt));
                }
            }
        }
        Err(last.context(format!("could not fetch {}", spec.id)))
    }

    /// Make `spec` the running model.
    pub fn load(&self, spec: &'static ModelSpec) -> Result<()> {
        if self
            .running
            .borrow()
            .as_ref()
            .is_some_and(|r| r.spec.id == spec.id)
        {
            return Ok(());
        }
        self.stop();
        let path = self.fetch(spec)?;
        let _live = progress::begin(&format!("load {} ({})", spec.id, spec.company));
        let log = File::create(self.models.join("server.log"))?;
        let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        let mut child = Command::new(&self.server)
            .arg("-m")
            .arg(&path)
            .args([
                "-c",
                &spec.ctx.to_string(),
                "-np",
                "1",
                "--host",
                "127.0.0.1",
                "--port",
                &self.port.to_string(),
                "-t",
                &threads.to_string(),
                "--reasoning-budget",
                &spec.reasoning_budget.to_string(),
            ])
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .with_context(|| format!("could not start {}", self.server.display()))?;
        let health = format!("http://127.0.0.1:{}/health", self.port);
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("{} exited early with {status}; see server.log", spec.id);
            }
            if http::get(&health, Duration::from_secs(3)).is_ok() {
                break;
            }
            if started.elapsed() > Duration::from_secs(900) {
                let _ = child.kill();
                bail!("{} did not become ready in 15 minutes", spec.id);
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
        self.events.borrow_mut().push(ModelEvent {
            kind: "load".into(),
            model: spec.id.into(),
            seconds: started.elapsed().as_secs_f64(),
            detail: format!("context {} tokens, {threads} threads", spec.ctx),
        });
        *self.running.borrow_mut() = Some(Running { spec, child, path });
        Ok(())
    }

    /// One chat completion on the running model, constrained to `schema`.
    pub fn complete(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
        schema: &Value,
        attempt: u32,
    ) -> Result<(String, Stats)> {
        let mut body = json!({
            "model": spec.id,
            "max_tokens": 1800,
            "temperature": if attempt == 0 { 0.0 } else { 0.4 },
            "seed": 42 + attempt,
            "cache_prompt": false,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "answer", "strict": true, "schema": schema},
            },
        });
        if !spec.template.is_empty() {
            body["chat_template_kwargs"] = serde_json::from_str(spec.template)?;
        }
        let url = format!("http://127.0.0.1:{}/v1/chat/completions", self.port);
        let raw = http::post_json(&url, &body.to_string(), Duration::from_secs(1200))?;
        let reply: Value = serde_json::from_str(&raw)?;
        let text = reply["choices"][0]["message"]["content"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| anyhow!("empty reply from the model server"))?
            .to_string();
        let stats = Stats {
            prompt_tokens: reply["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            completion_tokens: reply["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            prompt_per_second: reply["timings"]["prompt_per_second"]
                .as_f64()
                .unwrap_or(0.0),
            tokens_per_second: reply["timings"]["predicted_per_second"]
                .as_f64()
                .unwrap_or(0.0),
        };
        Ok((text, stats))
    }

    /// Ask `spec` and parse the JSON answer, trying twice.
    pub fn ask_model(
        &self,
        spec: &'static ModelSpec,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<(Value, String, Stats)> {
        self.load(spec)?;
        let mut last = anyhow!("no attempt made");
        for attempt in 0..2 {
            match self
                .complete(spec, system, user, schema, attempt)
                .and_then(|(text, stats)| Ok((extract_json(&text)?, text, stats)))
            {
                Ok(answer) => return Ok(answer),
                Err(error) => last = error,
            }
        }
        Err(last)
    }
}

impl Brain for LocalBrain {
    fn ask(
        &self,
        agent: Agent,
        subject: &str,
        system: &str,
        user: &str,
        schema: &Value,
    ) -> Result<Value> {
        let spec = model_for(agent);
        let _live = progress::track(&format!("{} on {subject}", agent.name()));
        let started = Instant::now();
        let outcome = self.ask_model(spec, system, user, schema);
        let seconds = started.elapsed().as_secs_f64();
        let mut call = Call {
            at: now(),
            agent: agent.name().into(),
            model: spec.id.into(),
            subject: subject.into(),
            system: system.into(),
            user: user.into(),
            seconds,
            ..Call::default()
        };
        let result = match outcome {
            Ok((value, text, stats)) => {
                call.ok = true;
                call.response = text;
                call.prompt_tokens = stats.prompt_tokens;
                call.completion_tokens = stats.completion_tokens;
                call.prompt_per_second = stats.prompt_per_second;
                call.tokens_per_second = stats.tokens_per_second;
                progress::log(&format!(
                    "{} {subject}: {} in, {} out, {seconds:.0}s ({:.1} tok/s)",
                    agent.name(),
                    stats.prompt_tokens,
                    stats.completion_tokens,
                    stats.tokens_per_second
                ));
                Ok(value)
            }
            Err(error) => {
                call.response = error.to_string();
                progress::log(&format!("{} {subject}: FAILED {error}", agent.name()));
                Err(error)
            }
        };
        self.calls.borrow_mut().push(call);
        result
    }

    fn model_name(&self, agent: Agent) -> String {
        model_for(agent).name.to_string()
    }

    fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }

    fn take_events(&self) -> Vec<ModelEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}

impl Drop for LocalBrain {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_inside_surrounding_text() {
        let value = extract_json("Sure:\n```json\n{\"a\": {\"b\": 1}}\n```").unwrap();
        assert_eq!(value["a"]["b"], 1);
        assert!(extract_json("no braces").is_err());
        assert!(extract_json("} backwards {").is_err());
    }

    #[test]
    fn a_missing_server_binary_is_a_clear_error() {
        let dir = std::env::temp_dir().join(format!("pt-llm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = model_for(Agent::Manager);
        // The download would be huge, so seed a file of the right size.
        File::create(dir.join(spec.file))
            .unwrap()
            .set_len(spec.bytes)
            .unwrap();
        let brain = LocalBrain {
            models: dir.clone(),
            server: dir.join("missing-llama-server"),
            port: 18999,
            prune: false,
            running: RefCell::new(None),
            calls: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
        };
        let error = brain
            .ask(Agent::Manager, "book", "s", "u", &json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("could not start"), "{error}");
        let calls = brain.take_calls();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].ok);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
