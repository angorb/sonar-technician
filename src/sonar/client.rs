//! HTTP access to Sonar's local API.
//!
//! Phase 0 only provides the plain-HTTP loopback agent. The `SonarApi`
//! implementation (retry-once-after-rediscovery, mode caching) comes in Phase 1.

use std::time::Duration;

use ureq::Agent;

/// Plain-HTTP agent for Sonar's web server on loopback.
///
/// - No proxy (env proxies must never see loopback traffic).
/// - No redirects (a redirect could point off-host).
/// - HTTP error statuses are returned as responses so callers can log the body.
pub fn loopback_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}
