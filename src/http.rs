//! Minimal blocking HTTP client shared by the stock and crypto data sources.
//!
//! Every request is also logged to an in-memory list so the run can report which data
//! sources were slow or failing. Requests to the local model server are not logged.

use anyhow::Result;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One request, for the data-source metrics.
#[derive(Clone, Debug)]
pub struct FetchRecord {
    pub source: String,
    pub ok: bool,
    pub ms: u64,
}

static FETCHES: Mutex<Vec<FetchRecord>> = Mutex::new(Vec::new());

/// A short name for the service a URL talks to.
pub fn source_of(url: &str) -> String {
    let host = url
        .split("//")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    match host {
        "www.nasdaqtrader.com" => "nasdaq-directory",
        "api.nasdaq.com" => "nasdaq-earnings",
        "query1.finance.yahoo.com" => "yahoo-bars",
        "feeds.finance.yahoo.com" => "yahoo-news",
        "api.kraken.com" => "kraken",
        "api.anthropic.com" => "anthropic",
        h if h.ends_with("sec.gov") => "sec-edgar",
        h if h.starts_with("127.0.0.1") || h.starts_with("localhost") => "",
        other => other,
    }
    .to_string()
}

fn record(url: &str, started: Instant, ok: bool) {
    let source = source_of(url);
    if source.is_empty() {
        return;
    }
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if let Ok(mut log) = FETCHES.lock() {
        log.push(FetchRecord { source, ok, ms });
    }
}

/// Requests made since the last call.
pub fn drain_fetches() -> Vec<FetchRecord> {
    FETCHES
        .lock()
        .map(|mut log| std::mem::take(&mut *log))
        .unwrap_or_default()
}

/// Fetch a public document as text with a timeout and an identifiable user agent.
pub fn get(url: &str, user_agent: &str, timeout: Duration) -> Result<String> {
    let started = Instant::now();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(user_agent)
        .build()
        .into();
    let result = agent
        .get(url)
        .header("Cache-Control", "no-cache")
        .call()
        .and_then(|mut response| response.body_mut().read_to_string());
    record(url, started, result.is_ok());
    Ok(result?)
}

/// POST a JSON body and return the response text. A non-success status becomes an error
/// that includes the start of the response body.
pub fn post_json(
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    timeout: Duration,
) -> Result<String> {
    let started = Instant::now();
    let result = post_json_inner(url, headers, body, timeout);
    record(url, started, result.is_ok());
    result
}

fn post_json_inner(
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    timeout: Duration,
) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.post(url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send(body)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_named_and_local_traffic_is_ignored() {
        assert_eq!(
            source_of("https://data.sec.gov/submissions/x.json"),
            "sec-edgar"
        );
        assert_eq!(
            source_of("https://api.nasdaq.com/api/calendar/earnings?date=1"),
            "nasdaq-earnings"
        );
        assert_eq!(source_of("http://127.0.0.1:8080/health"), "");
    }

    #[test]
    fn a_failed_request_is_recorded() {
        let _ = drain_fetches();
        let _ = get(
            "https://www.nasdaqtrader.com.invalid/x",
            "t",
            Duration::from_secs(2),
        );
        let logged = drain_fetches();
        assert!(logged.iter().any(|f| !f.ok));
    }
}
