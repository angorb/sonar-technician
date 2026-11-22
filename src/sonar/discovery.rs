//! Sonar discovery: `coreProps.json` -> GG `/subApps` -> Sonar `webServerAddress`.
//!
//! The Sonar port changes on every GG launch, so this runs at startup and
//! again after any transport error.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use ureq::Agent;
use ureq::tls::TlsConfig;

use super::routes;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Max bytes of a response body quoted in an error message.
const EXCERPT_LEN: usize = 300;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("%ProgramData% is not set, so coreProps.json can't be located")]
    ProgramDataMissing,

    #[error("coreProps.json not found (tried: {}). Is SteelSeries GG installed?", display_paths(.tried))]
    CorePropsNotFound { tried: Vec<PathBuf> },

    #[error("failed to read {}: {source}", .path.display())]
    CorePropsRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse {} as JSON: {source}", .path.display())]
    CorePropsParse {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("{} has no `ggEncryptedAddress` key", .path.display())]
    MissingGgAddress { path: PathBuf },

    #[error("refusing non-loopback address `{address}` (only 127.0.0.1/localhost/::1 are allowed)")]
    NotLoopback { address: String },

    #[error("malformed address `{address}`: {reason}")]
    BadAddress {
        address: String,
        reason: &'static str,
    },

    #[error("GET {url} failed: {source}. Is SteelSeries GG running?")]
    SubAppsRequest { url: String, source: ureq::Error },

    #[error("GET {url} returned HTTP {status}: {excerpt}")]
    SubAppsStatus {
        url: String,
        status: u16,
        excerpt: String,
    },

    #[error("GG /subApps response is not the expected JSON ({source}): {excerpt}")]
    SubAppsParse {
        source: serde_json::Error,
        excerpt: String,
    },

    #[error("GG /subApps has no `sonar` entry. Is Sonar installed/enabled in GG?")]
    SonarMissing,

    #[error(
        "Sonar is not available: isEnabled={enabled:?}, isReady={ready:?}, isRunning={running:?}"
    )]
    SonarNotReady {
        enabled: Option<bool>,
        ready: Option<bool>,
        running: Option<bool>,
    },

    #[error("Sonar entry in /subApps has no `metadata.webServerAddress`")]
    NoWebServerAddress,
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Truncate a body for logs/errors without splitting a UTF-8 char.
pub fn excerpt(body: &str) -> String {
    if body.len() <= EXCERPT_LEN {
        return body.to_owned();
    }
    let mut end = EXCERPT_LEN;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &body[..end], body.len())
}

// ---------------------------------------------------------------------------
// coreProps.json
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CoreProps {
    #[serde(rename = "ggEncryptedAddress")]
    gg_encrypted_address: Option<String>,
}

/// Candidate `coreProps.json` locations, in priority order.
pub fn core_props_candidates(program_data: &Path) -> Vec<PathBuf> {
    vec![
        program_data.join(r"SteelSeries\GG\coreProps.json"),
        program_data.join(r"SteelSeries\SteelSeries Engine 3\coreProps.json"),
    ]
}

/// The located `coreProps.json` and its raw contents.
#[derive(Debug, Clone)]
pub struct CorePropsFile {
    pub path: PathBuf,
    pub raw: String,
    /// `ggEncryptedAddress`, already checked to be loopback `host:port`.
    pub gg_address: String,
}

pub fn read_core_props() -> Result<CorePropsFile, DiscoveryError> {
    let program_data = std::env::var_os("ProgramData").ok_or(DiscoveryError::ProgramDataMissing)?;
    let candidates = core_props_candidates(Path::new(&program_data));
    let path = candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| DiscoveryError::CorePropsNotFound {
            tried: candidates.clone(),
        })?;
    let raw = std::fs::read_to_string(&path).map_err(|source| DiscoveryError::CorePropsRead {
        path: path.clone(),
        source,
    })?;
    let gg_address = parse_core_props(&path, &raw)?;
    Ok(CorePropsFile {
        path,
        raw,
        gg_address,
    })
}

fn parse_core_props(path: &Path, raw: &str) -> Result<String, DiscoveryError> {
    // GG has been seen writing a UTF-8 BOM in some config files; tolerate it.
    let raw = raw.trim_start_matches('\u{feff}');
    let props: CoreProps =
        serde_json::from_str(raw).map_err(|source| DiscoveryError::CorePropsParse {
            path: path.to_owned(),
            source,
        })?;
    let address = props
        .gg_encrypted_address
        .filter(|a| !a.trim().is_empty())
        .ok_or_else(|| DiscoveryError::MissingGgAddress {
            path: path.to_owned(),
        })?;
    let address = address.trim().to_owned();
    check_loopback_authority(&address)?;
    Ok(address)
}

// ---------------------------------------------------------------------------
// Loopback validation
// ---------------------------------------------------------------------------

/// Validate a `host:port` authority and require a loopback host.
pub fn check_loopback_authority(authority: &str) -> Result<(), DiscoveryError> {
    let bad = |reason| DiscoveryError::BadAddress {
        address: authority.to_owned(),
        reason,
    };
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        // [::1]:1234
        let (host, after) = rest.split_once(']').ok_or_else(|| bad("unclosed `[`"))?;
        let port = after.strip_prefix(':').ok_or_else(|| bad("missing port"))?;
        (host, port)
    } else {
        authority
            .rsplit_once(':')
            .ok_or_else(|| bad("missing port"))?
    };
    port.parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or_else(|| bad("invalid port"))?;
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if is_loopback {
        Ok(())
    } else {
        Err(DiscoveryError::NotLoopback {
            address: authority.to_owned(),
        })
    }
}

/// Validate Sonar's `webServerAddress` (`http://127.0.0.1:PORT`, maybe with a
/// trailing slash) and return it normalised without the trailing slash.
pub fn check_web_server_address(address: &str) -> Result<String, DiscoveryError> {
    let trimmed = address.trim().trim_end_matches('/');
    let authority = trimmed
        .strip_prefix("http://")
        .ok_or_else(|| DiscoveryError::BadAddress {
            address: address.to_owned(),
            reason: "expected an http:// URL",
        })?;
    if authority.contains('/') {
        return Err(DiscoveryError::BadAddress {
            address: address.to_owned(),
            reason: "unexpected path after host:port",
        });
    }
    check_loopback_authority(authority)?;
    Ok(trimmed.to_owned())
}

// ---------------------------------------------------------------------------
// GG /subApps
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubAppsResponse {
    #[serde(rename = "subApps")]
    sub_apps: Option<SubApps>,
}

#[derive(Debug, Deserialize)]
struct SubApps {
    sonar: Option<SonarSubApp>,
}

/// The parts of `subApps.sonar` we rely on. Everything is optional so a
/// missing field produces a clear error instead of a parse failure.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SonarSubApp {
    pub is_enabled: Option<bool>,
    pub is_ready: Option<bool>,
    pub is_running: Option<bool>,
    #[serde(default)]
    pub metadata: SonarMetadata,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SonarMetadata {
    pub web_server_address: Option<String>,
}

/// Raw result of asking GG about Sonar, before checking readiness.
#[derive(Debug, Clone)]
pub struct GgSubApps {
    pub raw: String,
    pub sonar: SonarSubApp,
}

/// Dedicated agent for the GG discovery call ONLY.
///
/// GG serves `/subApps` over HTTPS with a self-signed certificate it
/// regenerates, so verification is disabled here. This agent must never be
/// used for anything else, and callers must check the address is loopback
/// first (`fetch_sub_apps` does).
fn gg_discovery_agent() -> Agent {
    Agent::config_builder()
        .tls_config(TlsConfig::builder().disable_verification(true).build())
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(DISCOVERY_TIMEOUT))
        .build()
        .new_agent()
}

pub fn fetch_sub_apps(gg_address: &str) -> Result<GgSubApps, DiscoveryError> {
    check_loopback_authority(gg_address)?;
    let url = format!("https://{gg_address}{}", routes::GG_SUB_APPS);
    tracing::debug!(%url, "GET (GG discovery)");
    let mut resp =
        gg_discovery_agent()
            .get(&url)
            .call()
            .map_err(|source| DiscoveryError::SubAppsRequest {
                url: url.clone(),
                source,
            })?;
    let status = resp.status().as_u16();
    let raw =
        resp.body_mut()
            .read_to_string()
            .map_err(|source| DiscoveryError::SubAppsRequest {
                url: url.clone(),
                source,
            })?;
    if !(200..300).contains(&status) {
        return Err(DiscoveryError::SubAppsStatus {
            url,
            status,
            excerpt: excerpt(&raw),
        });
    }
    let sonar = parse_sub_apps(&raw)?;
    Ok(GgSubApps { raw, sonar })
}

fn parse_sub_apps(raw: &str) -> Result<SonarSubApp, DiscoveryError> {
    let parsed: SubAppsResponse =
        serde_json::from_str(raw).map_err(|source| DiscoveryError::SubAppsParse {
            source,
            excerpt: excerpt(raw),
        })?;
    parsed
        .sub_apps
        .and_then(|s| s.sonar)
        .ok_or(DiscoveryError::SonarMissing)
}

impl SonarSubApp {
    /// Check readiness and return the validated base URL (no trailing slash).
    pub fn base_url(&self) -> Result<String, DiscoveryError> {
        if self.is_enabled != Some(true)
            || self.is_ready != Some(true)
            || self.is_running != Some(true)
        {
            return Err(DiscoveryError::SonarNotReady {
                enabled: self.is_enabled,
                ready: self.is_ready,
                running: self.is_running,
            });
        }
        let address = self
            .metadata
            .web_server_address
            .as_deref()
            .ok_or(DiscoveryError::NoWebServerAddress)?;
        check_web_server_address(address)
    }
}

/// Result of a full discovery.
#[expect(dead_code, reason = "used by the Sonar client in Phase 1")]
#[derive(Debug, Clone)]
pub struct Discovery {
    pub core_props: CorePropsFile,
    pub sub_apps: GgSubApps,
    /// Sonar's base URL, e.g. `http://127.0.0.1:50453` (no trailing slash).
    pub base_url: String,
}

/// Full discovery: coreProps.json -> /subApps -> validated base URL.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the Sonar client in Phase 1")
)]
pub fn discover() -> Result<Discovery, DiscoveryError> {
    let core_props = read_core_props()?;
    let sub_apps = fetch_sub_apps(&core_props.gg_address)?;
    let base_url = sub_apps.sonar.base_url()?;
    tracing::info!(%base_url, "discovered Sonar");
    Ok(Discovery {
        core_props,
        sub_apps,
        base_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_props_ok() {
        let p = Path::new("coreProps.json");
        let raw =
            "\u{feff}{\"address\":\"127.0.0.1:6327\",\"ggEncryptedAddress\":\"127.0.0.1:6328\"}";
        assert_eq!(parse_core_props(p, raw).unwrap(), "127.0.0.1:6328");
    }

    #[test]
    fn core_props_missing_key() {
        let p = Path::new("coreProps.json");
        assert!(matches!(
            parse_core_props(p, r#"{"address":"127.0.0.1:1"}"#),
            Err(DiscoveryError::MissingGgAddress { .. })
        ));
        assert!(matches!(
            parse_core_props(p, r#"{"ggEncryptedAddress":"  "}"#),
            Err(DiscoveryError::MissingGgAddress { .. })
        ));
    }

    #[test]
    fn core_props_rejects_remote() {
        let p = Path::new("coreProps.json");
        assert!(matches!(
            parse_core_props(p, r#"{"ggEncryptedAddress":"192.168.1.5:6327"}"#),
            Err(DiscoveryError::NotLoopback { .. })
        ));
    }

    #[test]
    fn loopback_authorities() {
        for ok in [
            "127.0.0.1:6327",
            "localhost:1",
            "LOCALHOST:80",
            "[::1]:5000",
            "127.1.2.3:9",
        ] {
            assert!(check_loopback_authority(ok).is_ok(), "{ok}");
        }
        for bad in [
            "127.0.0.1",
            "127.0.0.1:0",
            "127.0.0.1:99999",
            "[::1]",
            "[::1:5",
        ] {
            assert!(
                matches!(
                    check_loopback_authority(bad),
                    Err(DiscoveryError::BadAddress { .. })
                ),
                "{bad}"
            );
        }
        for remote in [
            "10.0.0.1:80",
            "example.com:80",
            "localhost.evil.com:80",
            "[::2]:80",
        ] {
            assert!(
                matches!(
                    check_loopback_authority(remote),
                    Err(DiscoveryError::NotLoopback { .. })
                ),
                "{remote}"
            );
        }
    }

    #[test]
    fn web_server_address() {
        assert_eq!(
            check_web_server_address("http://127.0.0.1:50453/").unwrap(),
            "http://127.0.0.1:50453"
        );
        assert!(check_web_server_address("https://127.0.0.1:50453").is_err());
        assert!(check_web_server_address("http://127.0.0.1:50453/x").is_err());
        assert!(check_web_server_address("http://10.0.0.2:50453").is_err());
    }

    #[test]
    fn sub_apps_ready() {
        let raw = r#"{"subApps":{"engine":{},"sonar":{"isEnabled":true,"isReady":true,
            "isRunning":true,"metadata":{"webServerAddress":"http://127.0.0.1:50453"}}}}"#;
        let sonar = parse_sub_apps(raw).unwrap();
        assert_eq!(sonar.base_url().unwrap(), "http://127.0.0.1:50453");
    }

    #[test]
    fn sub_apps_not_ready() {
        let raw = r#"{"subApps":{"sonar":{"isEnabled":true,"isReady":false,"isRunning":true,
            "metadata":{}}}}"#;
        let sonar = parse_sub_apps(raw).unwrap();
        assert!(matches!(
            sonar.base_url(),
            Err(DiscoveryError::SonarNotReady {
                ready: Some(false),
                ..
            })
        ));
    }

    #[test]
    fn sub_apps_missing_sonar_or_address() {
        assert!(matches!(
            parse_sub_apps(r#"{"subApps":{}}"#),
            Err(DiscoveryError::SonarMissing)
        ));
        let raw = r#"{"subApps":{"sonar":{"isEnabled":true,"isReady":true,"isRunning":true}}}"#;
        assert!(matches!(
            parse_sub_apps(raw).unwrap().base_url(),
            Err(DiscoveryError::NoWebServerAddress)
        ));
        assert!(matches!(
            parse_sub_apps("<html>"),
            Err(DiscoveryError::SubAppsParse { .. })
        ));
    }

    #[test]
    fn excerpt_truncates_on_char_boundary() {
        let s = "é".repeat(400);
        let e = excerpt(&s);
        assert!(e.contains("bytes total"));
        assert!(excerpt("short") == "short");
    }

    /// Hits `/subApps` at the address in `SONAR_TECH_GG_ADDR` (e.g. a local
    /// self-signed test server, or GG itself). `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_sub_apps_from_env() {
        let addr = std::env::var("SONAR_TECH_GG_ADDR").expect("set SONAR_TECH_GG_ADDR");
        let s = fetch_sub_apps(&addr).unwrap();
        println!("{:?} -> {:?}", s.sonar, s.sonar.base_url());
    }

    /// Needs GG + Sonar running on this machine: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_discovery() {
        let d = discover().unwrap();
        println!("{}", d.base_url);
    }
}
