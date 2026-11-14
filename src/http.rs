//! Minimal blocking HTTP client for the public data sources and the local model server.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The user agent sent to public data sources.
pub const AGENT: &str = "Mozilla/5.0 (paper-trader research)";

/// Requests to one data source during a run, for the ops page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceStats {
    pub source: String,
    pub requests: u64,
    pub failures: u64,
    pub bytes: u64,
    pub total_ms: u64,
    pub max_ms: u64,
}

static FETCHES: Mutex<BTreeMap<String, SourceStats>> = Mutex::new(BTreeMap::new());

/// A short name for the service a URL talks to. Local traffic (the model server) is
/// measured separately and gets an empty name.
pub fn source_of(url: &str) -> String {
    let host = url
        .split("//")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    match host {
        "api.nasdaq.com" => "Nasdaq screener",
        "www.nasdaqtrader.com" => "Nasdaq fund directory",
        "query1.finance.yahoo.com" if url.contains("/trending/") => "Yahoo trending",
        "query1.finance.yahoo.com" => "Yahoo daily bars",
        "feeds.finance.yahoo.com" => "Yahoo stock headlines",
        "news.google.com" => "Google News",
        "www.bing.com" => "Bing News",
        "apewisdom.io" => "Reddit (ApeWisdom)",
        "api.stocktwits.com" => "StockTwits",
        "huggingface.co" => "Hugging Face",
        "github.com" => "GitHub releases",
        h if h.starts_with("127.0.0.1") || h.starts_with("localhost") => "",
        other => other,
    }
    .to_string()
}

fn record(url: &str, started: Instant, ok: bool, bytes: usize) {
    let source = source_of(url);
    if source.is_empty() {
        return;
    }
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Ok(mut log) = FETCHES.lock() {
        let entry = log.entry(source.clone()).or_insert_with(|| SourceStats {
            source,
            ..SourceStats::default()
        });
        entry.requests += 1;
        entry.failures += u64::from(!ok);
        entry.bytes += bytes as u64;
        entry.total_ms += ms;
        entry.max_ms = entry.max_ms.max(ms);
    }
}

/// Request statistics since the last call, by source.
pub fn drain_fetches() -> Vec<SourceStats> {
    FETCHES
        .lock()
        .map(|mut log| std::mem::take(&mut *log).into_values().collect())
        .unwrap_or_default()
}

/// Fetch a public document as text.
pub fn get(url: &str, timeout: Duration) -> Result<String> {
    let started = Instant::now();
    let result = get_inner(url, timeout);
    record(
        url,
        started,
        result.is_ok(),
        result.as_ref().map_or(0, String::len),
    );
    result
}

fn get_inner(url: &str, timeout: Duration) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(AGENT)
        .build()
        .into();
    let text = agent
        .get(url)
        .header("Cache-Control", "no-cache")
        .call()?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()?;
    Ok(text)
}

/// Fetch with up to `tries` attempts, pausing between them.
pub fn get_retry(url: &str, timeout: Duration, tries: u32) -> Result<String> {
    let mut last = None;
    for attempt in 0..tries.max(1) {
        match get(url, timeout) {
            Ok(text) => return Ok(text),
            Err(error) => {
                last = Some(error);
                std::thread::sleep(Duration::from_millis(800 * u64::from(attempt + 1)));
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempt made")))
}

/// POST a JSON body and return the response text. A non-success status becomes an error
/// that includes the start of the response body.
pub fn post_json(url: &str, body: &str, timeout: Duration) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("content-type", "application/json")
        .send(body)?;
    let status = response.status();
    let text = response.body_mut().read_to_string()?;
    if !status.is_success() {
        anyhow::bail!(
            "HTTP {status}: {}",
            text.chars().take(300).collect::<String>()
        );
    }
    Ok(text)
}

/// Percent-encode a query value.
pub fn encode(text: &str) -> String {
    use std::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, b| {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(
            encode("AI chips \"stocks\" when:1d"),
            "AI+chips+%22stocks%22+when%3A1d"
        );
    }

    #[test]
    fn sources_are_named_and_local_traffic_is_ignored() {
        assert_eq!(
            source_of("https://news.google.com/rss/search?q=x"),
            "Google News"
        );
        assert_eq!(
            source_of("https://query1.finance.yahoo.com/v1/finance/trending/US"),
            "Yahoo trending"
        );
        assert_eq!(source_of("http://127.0.0.1:8091/health"), "");
    }

    #[test]
    fn a_failed_request_is_an_error() {
        assert!(
            get(
                "https://www.nasdaqtrader.com.invalid/x",
                Duration::from_secs(2)
            )
            .is_err()
        );
    }
}
