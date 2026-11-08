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

/// POST a JSON body and return the response text. A non-success status becomes an error
/// that includes the start of the response body.
pub fn post_json(
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
