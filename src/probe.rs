//! `sonar-technician probe`: discover Sonar and dump every known GET endpoint
//! to `./probe-<UTC timestamp>/`. Used to confirm JSON shapes and to debug
//! after a GG update. A failing endpoint is recorded and the probe carries on.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::sonar::{client, discovery, routes};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of one GET.
struct Fetched {
    status: Result<u16, String>,
    body: String,
    elapsed: Duration,
}

pub fn run(out_root: &Path) -> Result<PathBuf> {
    let now = SystemTime::now();
    let stamp = utc_stamp(now);
    let dir = out_root.join(format!("probe-{stamp}"));
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    println!("Writing probe output to {}", dir.display());

    let mut summary = String::new();
    let _ = writeln!(
        summary,
        "sonar-technician {} probe",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(summary, "time (UTC): {stamp}");

    // Step 1: coreProps.json
    let core = match discovery::read_core_props() {
        Ok(c) => c,
        Err(e) => return fail(&dir, summary, "coreProps.json", &e),
    };
    write(&dir, "coreProps.json", &core.raw)?;
    let _ = writeln!(summary, "coreProps: {}", core.path.display());
    let _ = writeln!(summary, "ggEncryptedAddress: {}", core.gg_address);

    // Step 2: GG /subApps
    let sub_apps = match discovery::fetch_sub_apps(&core.gg_address) {
        Ok(s) => s,
        Err(e) => return fail(&dir, summary, "GG /subApps", &e),
    };
    write(&dir, "subApps.json", &sub_apps.raw)?;
    let s = &sub_apps.sonar;
    let _ = writeln!(
        summary,
        "sonar: isEnabled={:?} isReady={:?} isRunning={:?} webServerAddress={:?}",
        s.is_enabled, s.is_ready, s.is_running, s.metadata.web_server_address
    );

    // Step 3: Sonar base URL
    let base_url = match s.base_url() {
        Ok(u) => u,
        Err(e) => return fail(&dir, summary, "Sonar readiness", &e),
    };
    let _ = writeln!(summary, "base URL: {base_url}\n");
    let _ = writeln!(
        summary,
        "{:<46} {:<8} {:>8} {:>7}  path",
        "name", "status", "bytes", "ms"
    );

    // Step 4: every GET endpoint
    let agent = client::loopback_agent(REQUEST_TIMEOUT);
    let mut failures = 0;
    for ep in routes::PROBE_ENDPOINTS {
        let url = format!("{base_url}{}", ep.path);
        let f = get(&agent, &url);
        let status_txt = match &f.status {
            Ok(code) => code.to_string(),
            Err(_) => "ERR".to_owned(),
        };
        if !matches!(f.status, Ok(200..=299)) {
            failures += 1;
        }
        let _ = writeln!(
            summary,
            "{:<46} {:<8} {:>8} {:>7}  {}",
            ep.name,
            status_txt,
            f.body.len(),
            f.elapsed.as_millis(),
            ep.path
        );
        println!("  {:<46} {status_txt}", ep.name);

        let mut meta = format!("GET {url}\n");
        match &f.status {
            Ok(code) => {
                let _ = writeln!(meta, "status: {code}");
            }
            Err(e) => {
                let _ = writeln!(meta, "transport error: {e}");
            }
        }
        let _ = writeln!(meta, "elapsed_ms: {}", f.elapsed.as_millis());
        let _ = writeln!(meta, "body_bytes: {}", f.body.len());
        let _ = writeln!(
            meta,
            "body_is_json: {}",
            serde_json::from_str::<serde_json::Value>(&f.body).is_ok()
        );
        write(&dir, &format!("{}.meta.txt", ep.name), &meta)?;
        // Raw body, byte-for-byte as received (even for error statuses).
        write(&dir, &format!("{}.json", ep.name), &f.body)?;
    }

    let _ = writeln!(
        summary,
        "\n{} endpoints, {failures} non-2xx/failed",
        routes::PROBE_ENDPOINTS.len()
    );
    write(&dir, "summary.txt", &summary)?;
    println!("\n{summary}");
    Ok(dir)
}

fn get(agent: &ureq::Agent, url: &str) -> Fetched {
    tracing::debug!(%url, "GET (probe)");
    let start = Instant::now();
    let result = agent.get(url).call().and_then(|mut resp| {
        let status = resp.status().as_u16();
        resp.body_mut().read_to_string().map(|body| (status, body))
    });
    let elapsed = start.elapsed();
    match result {
        Ok((status, body)) => Fetched {
            status: Ok(status),
            body,
            elapsed,
        },
        Err(e) => Fetched {
            status: Err(e.to_string()),
            body: String::new(),
            elapsed,
        },
    }
}

/// Record a discovery failure in summary.txt and return it as the error.
fn fail(
    dir: &Path,
    mut summary: String,
    step: &str,
    err: &dyn std::error::Error,
) -> Result<PathBuf> {
    let _ = writeln!(summary, "\nFAILED at {step}: {err}");
    write(dir, "summary.txt", &summary)?;
    println!("\n{summary}");
    anyhow::bail!("probe stopped at {step}: {err}")
}

fn write(dir: &Path, name: &str, contents: &str) -> Result<()> {
    let path = dir.join(name);
    fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))
}

/// `YYYYMMDDTHHMMSSZ` in UTC, without pulling in a date crate.
fn utc_stamp(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_formats() {
        assert_eq!(utc_stamp(UNIX_EPOCH), "19700101T000000Z");
        // 2026-11-21 19:04:05 UTC
        let t = UNIX_EPOCH + Duration::from_secs(1_795_287_845);
        assert_eq!(utc_stamp(t), "20261121T190405Z");
        // Leap day 2024-02-29 00:00:00 UTC
        let t = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(utc_stamp(t), "20240229T000000Z");
    }
}
