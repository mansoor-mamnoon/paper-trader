//! Minimal blocking HTTP client for the public data sources and the local model server.

use anyhow::Result;
use std::time::Duration;

/// The user agent sent to public data sources.
pub const AGENT: &str = "Mozilla/5.0 (paper-trader research)";

/// Fetch a public document as text.
pub fn get(url: &str, timeout: Duration) -> Result<String> {
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
