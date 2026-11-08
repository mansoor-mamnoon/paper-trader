//! Minimal blocking HTTP client shared by the stock and crypto data sources.

use anyhow::Result;
use std::time::Duration;

/// Fetch a public document as text with a timeout and an identifiable user agent.
pub fn get(url: &str, user_agent: &str, timeout: Duration) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(user_agent)
        .build()
        .into();
    Ok(agent
        .get(url)
        .header("Cache-Control", "no-cache")
        .call()?
        .body_mut()
        .read_to_string()?)
}
