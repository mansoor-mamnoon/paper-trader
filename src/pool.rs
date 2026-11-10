//! Starts and swaps local model servers so each role runs on its own model.
//!
//! The runner has 16 GB of RAM and 14 GB of disk, which fits one model at a time. The
//! pool keeps one `llama-server` alive, and when a role needs a different model it
//! downloads that file (checking its SHA-256 against the pin in [`crate::models`]),
//! stops the old server and starts a new one. The desk runs its stages in blocks, one
//! model per block, so a day needs about a dozen swaps.
//!
//! Every model gets a determinism check on load: the same prompt twice at temperature 0
//! must give the same text. The result is reported as an event so drift is visible.

use crate::analyst::{Completion, Llm, OpenAiLlm};
use crate::http;
use crate::models::{ModelSpec, Role, model_for};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Something worth recording about a model load, download or check.
#[derive(Clone, Debug)]
pub struct PoolEvent {
    /// `model_load`, `determinism`, `download` or `error`.
    pub kind: String,
    pub subject: String,
    pub detail: String,
    pub ms: u64,
}

/// Routes a decision role to a model.
pub trait Router: std::fmt::Debug {
    fn complete(
        &self,
        role: Role,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> Result<Completion>;
    /// The model id and company that answer for `role`.
    fn describe(&self, role: Role) -> (String, String);
    /// How many candidate stocks a model can read well in one prompt.
    fn max_candidates(&self) -> usize;
    /// One-line description for logs and the dashboard.
    fn label(&self) -> String;
    /// Events since the last call.
    fn take_events(&self) -> Vec<PoolEvent> {
        Vec::new()
    }
}

/// One model for every role, for a single remote or local server.
#[derive(Debug)]
pub struct OneModel<'a>(pub &'a dyn Llm);

impl Router for OneModel<'_> {
    fn complete(
        &self,
        _: Role,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> Result<Completion> {
        self.0.complete_full(system, user, schema)
    }

    fn describe(&self, _: Role) -> (String, String) {
        (self.0.name(), "single model".into())
    }

    fn max_candidates(&self) -> usize {
        self.0.max_candidates()
    }

    fn label(&self) -> String {
        self.0.name()
    }
}

struct Running {
    spec: &'static ModelSpec,
    child: Child,
    llm: OpenAiLlm,
    path: PathBuf,
}

/// A set of pinned models served one at a time.
pub struct Pool {
    dir: PathBuf,
    server: PathBuf,
    port: u16,
    prune: bool,
    state: RefCell<Option<Running>>,
    events: RefCell<Vec<PoolEvent>>,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("dir", &self.dir)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn sha_prefix(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    hex(&digest[..8])
}

impl Pool {
    /// `server` is the `llama-server` binary and `dir` is where model files live. With
    /// `prune`, a model file is deleted after the pool moves on, to bound disk use.
    pub fn new(dir: PathBuf, server: PathBuf, port: u16, prune: bool) -> Self {
        Pool {
            dir,
            server,
            port,
            prune,
            state: RefCell::new(None),
            events: RefCell::new(Vec::new()),
        }
    }

    fn note(&self, kind: &str, subject: &str, detail: impl Into<String>, ms: u64) {
        self.events.borrow_mut().push(PoolEvent {
            kind: kind.into(),
            subject: subject.into(),
            detail: detail.into(),
            ms,
        });
    }

    fn stop(&self) {
        if let Some(mut running) = self.state.borrow_mut().take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
            if self.prune {
                let _ = std::fs::remove_file(&running.path);
            }
        }
    }

    /// Download `spec` if it is missing, checking the pinned SHA-256.
    fn fetch_model(&self, spec: &ModelSpec) -> Result<PathBuf> {
        let path = self.dir.join(spec.file);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == spec.bytes) {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.dir)?;
        let started = Instant::now();
        let mut last_error = anyhow!("no attempt made");
        let _live = crate::progress::begin(&format!(
            "download {} ({} MB)",
            spec.id,
            spec.bytes / 1_000_000
        ));
        for attempt in 1..=3 {
            match download(spec, &path) {
                Ok(()) => {
                    crate::progress::log(&format!(
                        "downloaded {} in {}, sha256 verified",
                        spec.id,
                        crate::progress::human(started.elapsed())
                    ));
                    self.note(
                        "download",
                        spec.id,
                        format!(
                            "{} MB, sha256 verified, attempt {attempt}",
                            spec.bytes / 1_000_000
                        ),
                        elapsed_ms(started),
                    );
                    return Ok(path);
                }
                Err(error) => {
                    crate::progress::log(&format!(
                        "download attempt {attempt} of {} failed: {error}",
                        spec.id
                    ));
                    self.note(
                        "error",
                        spec.id,
                        format!("download attempt {attempt} failed: {error}"),
                        elapsed_ms(started),
                    );
                    last_error = error;
                    std::thread::sleep(Duration::from_secs(5 * attempt));
                }
            }
        }
        Err(last_error.context(format!("could not fetch {}", spec.id)))
    }

    fn start(&self, spec: &'static ModelSpec) -> Result<()> {
        self.stop();
        let path = self.fetch_model(spec)?;
        let _live = crate::progress::begin(&format!("load {} ({})", spec.id, spec.company));
        let started = Instant::now();
        let log = File::create(self.dir.join("server.log"))?;
        let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        let mut child = Command::new(&self.server)
            .args(["-m"])
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
            ])
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .with_context(|| format!("could not start {}", self.server.display()))?;
        let base = format!("http://127.0.0.1:{}", self.port);
        let ready = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("{} exited early with {status}; see server.log", spec.id);
            }
            if http::get(
                &format!("{base}/health"),
                "paper-trader",
                Duration::from_secs(3),
            )
            .is_ok()
            {
                break;
            }
            if ready.elapsed() > Duration::from_secs(600) {
                let _ = child.kill();
                bail!("{} did not become ready in 10 minutes", spec.id);
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
        let llm = OpenAiLlm::local(&base, spec.id);
        crate::progress::log(&format!(
            "{} ready in {}",
            spec.id,
            crate::progress::human(started.elapsed())
        ));
        self.note(
            "model_load",
            spec.id,
            format!("{} ({}) ready", spec.name, spec.company),
            elapsed_ms(started),
        );
        *self.state.borrow_mut() = Some(Running {
            spec,
            child,
            llm,
            path,
        });
        self.canary(spec);
        Ok(())
    }

    /// The same prompt twice at temperature 0 must give the same text.
    fn canary(&self, spec: &ModelSpec) {
        let state = self.state.borrow();
        let Some(running) = state.as_ref() else {
            return;
        };
        let schema = json!({"type":"object","properties":{"primes":{"type":"array","maxItems":5,"items":{"type":"integer"}}},"required":["primes"]});
        let ask = || {
            running.llm.complete(
                "You answer in JSON.",
                "List the first five prime numbers.",
                Some(&schema),
            )
        };
        let started = Instant::now();
        match (ask(), ask()) {
            (Ok(a), Ok(b)) => {
                let (ha, hb) = (sha_prefix(&a), sha_prefix(&b));
                let stable = ha == hb;
                self.note(
                    "determinism",
                    spec.id,
                    format!(
                        "hash {ha} {}",
                        if stable {
                            "stable"
                        } else {
                            "UNSTABLE: second run gave a different answer"
                        }
                    ),
                    elapsed_ms(started),
                );
            }
            (Err(e), _) | (_, Err(e)) => self.note(
                "error",
                spec.id,
                format!("canary failed: {e}"),
                elapsed_ms(started),
            ),
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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
        let tenth = total * 10 / spec.bytes.max(1);
        if tenth > reported && tenth < 10 {
            reported = tenth;
            crate::progress::log(&format!("download {} {}0%", spec.id, tenth));
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

impl Router for Pool {
    fn complete(
        &self,
        role: Role,
        system: &str,
        user: &str,
        schema: Option<&Value>,
    ) -> Result<Completion> {
        let spec = model_for(role);
        let needs_start = self
            .state
            .borrow()
            .as_ref()
            .is_none_or(|r| r.spec.id != spec.id);
        if needs_start {
            self.start(spec)?;
        }
        let state = self.state.borrow();
        let running = state.as_ref().ok_or_else(|| anyhow!("no model running"))?;
        running.llm.complete_full(system, user, schema)
    }

    fn describe(&self, role: Role) -> (String, String) {
        let spec = model_for(role);
        (spec.id.to_string(), spec.company.to_string())
    }

    fn max_candidates(&self) -> usize {
        12
    }

    fn label(&self) -> String {
        format!(
            "{} open-weights models from {} companies",
            crate::models::MODELS.len(),
            crate::models::MODELS.len()
        )
    }

    fn take_events(&self) -> Vec<PoolEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Fixed;
    impl Llm for Fixed {
        fn name(&self) -> String {
            "fixed".into()
        }
        fn complete(&self, _: &str, _: &str, _: Option<&Value>) -> Result<String> {
            Ok("hello".into())
        }
    }

    #[test]
    fn one_model_answers_every_role() {
        let llm = Fixed;
        let router = OneModel(&llm);
        for role in Role::ALL {
            assert_eq!(router.complete(role, "s", "u", None).unwrap().text, "hello");
        }
        assert_eq!(router.describe(Role::Judge).0, "fixed");
    }

    #[test]
    fn sha_prefix_is_stable_and_distinguishes_text() {
        assert_eq!(sha_prefix("a"), sha_prefix("a"));
        assert_ne!(sha_prefix("a"), sha_prefix("b"));
        assert_eq!(sha_prefix("a").len(), 16);
    }

    #[test]
    fn describe_names_a_different_company_for_each_role() {
        let pool = Pool::new(
            PathBuf::from("/nonexistent"),
            PathBuf::from("/nonexistent/llama-server"),
            1,
            false,
        );
        let companies: std::collections::BTreeSet<String> =
            Role::ALL.iter().map(|r| pool.describe(*r).1).collect();
        assert_eq!(companies.len(), Role::ALL.len());
    }

    #[test]
    fn a_missing_server_binary_is_a_clear_error() {
        let dir = std::env::temp_dir().join(format!("pt-pool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = Pool::new(dir.clone(), dir.join("missing-llama-server"), 18999, false);
        // The model file does not exist and the download would be huge, so seed a file of the right size.
        let spec = model_for(Role::Triage);
        std::fs::create_dir_all(&dir).unwrap();
        let f = File::create(dir.join(spec.file)).unwrap();
        f.set_len(spec.bytes).unwrap();
        let error = pool
            .complete(Role::Triage, "s", "u", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("could not start"), "{error}");
    }
}
