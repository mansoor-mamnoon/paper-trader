//! A large hosted model, reached over the chat-completions API that NVIDIA, Groq,
//! OpenRouter, Google and most other providers share. Free tiers are rate-limited, so the
//! client paces itself, checks every answer against the schema, and gives up cleanly so the
//! local model can take over.

use crate::http;
use crate::llm::{Stats, extract_json};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// After this many failed questions in a row the hosted model is left alone for the run.
const MAX_FAILURES: u32 = 3;

/// A hosted model and its limits.
#[derive(Debug)]
pub struct Remote {
    /// Base URL ending in `/v1` or the provider's equivalent.
    pub url: String,
    key: String,
    pub model: String,
    /// Requests allowed per minute.
    pub rpm: usize,
    /// Tokens allowed per minute, prompt and answer together.
    pub tpm: u64,
    /// Whether the provider accepts a JSON schema in `response_format`.
    strict: Cell<bool>,
    recent: RefCell<VecDeque<(Instant, u64)>>,
    failures: Cell<u32>,
}

/// Whether `value` has the shape `schema` asks for: required keys, enum members, number
/// ranges and array lengths. String lengths are not checked.
pub fn conforms(value: &Value, schema: &Value) -> bool {
    match schema["type"].as_str() {
        Some("object") => {
            let Some(object) = value.as_object() else {
                return false;
            };
            schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .all(|key| {
                    object
                        .get(key)
                        .is_some_and(|v| conforms(v, &schema["properties"][key]))
                })
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                return false;
            };
            let n = items.len() as u64;
            schema["minItems"].as_u64().is_none_or(|min| n >= min)
                && schema["maxItems"].as_u64().is_none_or(|max| n <= max)
                && items.iter().all(|item| conforms(item, &schema["items"]))
        }
        Some("string") => value.as_str().is_some_and(|text| {
            schema["enum"]
                .as_array()
                .is_none_or(|allowed| allowed.iter().any(|a| a == text))
        }),
        Some("integer") => value.as_i64().is_some_and(|n| {
            schema["minimum"].as_i64().is_none_or(|min| n >= min)
                && schema["maximum"].as_i64().is_none_or(|max| n <= max)
        }),
        Some("boolean") => value.is_boolean(),
        _ => true,
    }
}

/// A rough token count: about three and a half characters each.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64 * 2).div_ceil(7)
}

impl Remote {
    /// Read `PAPER_TRADER_API_KEY`, and optionally `PAPER_TRADER_API_URL`,
    /// `PAPER_TRADER_API_MODEL`, `PAPER_TRADER_API_RPM` and `PAPER_TRADER_API_TPM`. With no
    /// key there is no hosted model.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let key = var("PAPER_TRADER_API_KEY")?;
        Some(Remote {
            url: var("PAPER_TRADER_API_URL")
                .unwrap_or_else(|| "https://integrate.api.nvidia.com/v1".into())
                .trim_end_matches('/')
                .to_string(),
            key: key.trim().to_string(),
            model: var("PAPER_TRADER_API_MODEL")
                .unwrap_or_else(|| "deepseek-ai/deepseek-v4.1-flash".into()),
            rpm: var("PAPER_TRADER_API_RPM")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            tpm: var("PAPER_TRADER_API_TPM")
                .and_then(|v| v.parse().ok())
                .unwrap_or(60_000),
            strict: Cell::new(true),
            recent: RefCell::new(VecDeque::new()),
            failures: Cell::new(0),
        })
    }

    /// Whether the hosted model is still worth asking in this run.
    pub fn usable(&self) -> bool {
        self.failures.get() < MAX_FAILURES
    }

    /// How long to wait before sending `tokens` more, given the last minute's traffic.
    fn wait_for(&self, tokens: u64, now: Instant) -> Duration {
        let mut recent = self.recent.borrow_mut();
        while recent
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(60))
        {
            recent.pop_front();
        }
        let mut used: u64 = recent.iter().map(|(_, t)| *t).sum();
        let mut count = recent.len();
        let mut wait = Duration::ZERO;
        for (at, spent) in recent.iter() {
            if count < self.rpm && used + tokens <= self.tpm {
                break;
            }
            wait = Duration::from_secs(60).saturating_sub(now.duration_since(*at));
            used -= spent;
            count -= 1;
        }
        wait
    }

    fn post(&self, system: &str, user: &str, schema: &Value) -> Result<(u16, String)> {
        let mut body = json!({
            "model": self.model,
            "temperature": 0.2,
            "max_tokens": 2500,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": format!(
                    "{user}\n\nAnswer with one JSON object and nothing else. It must match this JSON schema: {schema}"
                )},
            ],
        });
        if self.model == "deepseek-ai/deepseek-v4.1-flash" {
            // Keep the output budget for the schema answer rather than hidden reasoning.
            body["temperature"] = json!(1.0);
            body["chat_template_kwargs"] = json!({"thinking": false});
        }
        body["response_format"] = if self.strict.get() {
            json!({"type": "json_schema", "json_schema": {"name": "answer", "strict": true, "schema": schema}})
        } else {
            json!({"type": "json_object"})
        };
        http::post_json_auth(
            &format!("{}/chat/completions", self.url),
            &self.key,
            &body.to_string(),
            Duration::from_secs(300),
        )
    }

    fn attempt(&self, system: &str, user: &str, schema: &Value) -> Result<(Value, String, Stats)> {
        let estimate = estimate_tokens(system) + estimate_tokens(user) + 800;
        if estimate > self.tpm {
            bail!(
                "the question is about {estimate} tokens, over the hosted model's {} a minute",
                self.tpm
            );
        }
        for _ in 0..4 {
            let wait = self.wait_for(estimate, Instant::now());
            if !wait.is_zero() {
                std::thread::sleep(wait + Duration::from_millis(500));
            }
            let started = Instant::now();
            let (status, text) = self.post(system, user, schema)?;
            match status {
                200..=299 => {
                    let reply: Value = serde_json::from_str(&text)?;
                    let content = reply["choices"][0]["message"]["content"]
                        .as_str()
                        .filter(|t| !t.trim().is_empty())
                        .ok_or_else(|| anyhow!("empty reply from the hosted model"))?
                        .to_string();
                    let prompt = reply["usage"]["prompt_tokens"].as_u64().unwrap_or(estimate);
                    let written = reply["usage"]["completion_tokens"].as_u64().unwrap_or(0);
                    self.recent
                        .borrow_mut()
                        .push_back((Instant::now(), prompt + written));
                    let value = extract_json(&content)?;
                    if !conforms(&value, schema) {
                        bail!("the hosted model's answer does not match the schema");
                    }
                    let seconds = started.elapsed().as_secs_f64().max(0.001);
                    return Ok((
                        value,
                        content,
                        Stats {
                            prompt_tokens: prompt,
                            completion_tokens: written,
                            prompt_per_second: 0.0,
                            tokens_per_second: written as f64 / seconds,
                        },
                    ));
                }
                429 => {
                    self.recent
                        .borrow_mut()
                        .push_back((Instant::now(), estimate));
                    std::thread::sleep(Duration::from_secs(20));
                }
                500..=599 => std::thread::sleep(Duration::from_secs(5)),
                400 | 422 if self.strict.get() => {
                    // Some providers and models take only plain JSON mode.
                    self.strict.set(false);
                }
                _ => bail!(
                    "HTTP {status}: {}",
                    text.chars().take(240).collect::<String>()
                ),
            }
        }
        bail!("the hosted model kept refusing the request")
    }

    /// Ask the hosted model, trying twice. Three failed questions in a row switch it off
    /// for the rest of the run.
    pub fn ask(&self, system: &str, user: &str, schema: &Value) -> Result<(Value, String, Stats)> {
        let mut last = anyhow!("no attempt made");
        for _ in 0..2 {
            match self.attempt(system, user, schema) {
                Ok(answer) => {
                    self.failures.set(0);
                    return Ok(answer);
                }
                Err(error) => last = error,
            }
        }
        self.failures.set(self.failures.get() + 1);
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(rpm: usize, tpm: u64) -> Remote {
        Remote {
            url: "http://127.0.0.1:1/v1".into(),
            key: "k".into(),
            model: "m".into(),
            rpm,
            tpm,
            strict: Cell::new(true),
            recent: RefCell::new(VecDeque::new()),
            failures: Cell::new(0),
        }
    }

    #[test]
    fn answers_are_checked_against_the_schema() {
        let schema = json!({"type": "object", "required": ["action", "buys"], "properties": {
            "action": {"type": "string", "enum": ["KEEP", "SELL"]},
            "buys": {"type": "array", "minItems": 1, "maxItems": 2, "items": {"type": "object",
                "required": ["weight"], "properties": {"weight": {"type": "integer", "minimum": 1, "maximum": 3}}}}}});
        assert!(conforms(
            &json!({"action": "KEEP", "buys": [{"weight": 2}]}),
            &schema
        ));
        assert!(!conforms(
            &json!({"action": "HOLD", "buys": [{"weight": 2}]}),
            &schema
        ));
        assert!(!conforms(&json!({"action": "KEEP", "buys": []}), &schema));
        assert!(!conforms(
            &json!({"action": "KEEP", "buys": [{"weight": 9}]}),
            &schema
        ));
        assert!(!conforms(&json!({"buys": [{"weight": 1}]}), &schema));
        assert!(!conforms(&json!("KEEP"), &schema));
    }

    #[test]
    fn requests_are_paced_to_the_limits() {
        let r = remote(2, 1000);
        let now = Instant::now();
        assert!(r.wait_for(400, now).is_zero());
        r.recent.borrow_mut().push_back((now, 700));
        // 700 + 400 is over the token limit, so wait for the first request to age out.
        assert!(r.wait_for(400, now) > Duration::from_secs(59));
        assert!(r.wait_for(200, now).is_zero());
        r.recent.borrow_mut().push_back((now, 100));
        // Two requests in the minute is the request limit.
        assert!(r.wait_for(10, now) > Duration::from_secs(59));
        assert_eq!(estimate_tokens("abcdefg"), 2);
    }

    #[test]
    fn an_oversized_question_and_a_dead_server_fail_cleanly() {
        let r = remote(10, 100);
        let schema = json!({"type": "object"});
        let error = r
            .ask("s", &"x".repeat(2000), &schema)
            .unwrap_err()
            .to_string();
        assert!(error.contains("over the hosted model's"), "{error}");
        let r = remote(10, 100_000);
        for _ in 0..MAX_FAILURES {
            assert!(r.usable());
            assert!(r.ask("s", "u", &schema).is_err());
        }
        assert!(!r.usable());
    }
}
