//! `try-writes`: resolve Q2/Q3 against the live Sonar.
//!
//! For the current mode, tries every candidate spelling in `routes.rs` for the
//! volume keyword, the master token and the mute keyword, reading the state
//! back after each write to see which ones Sonar actually applies. With
//! `--routing` (Classic only) it also re-sends the channel's *current* device
//! with a raw and a percent-encoded ID to see which form Sonar accepts.
//!
//! Every change is restored afterwards (volumes to the original value rounded
//! to 2 decimals).

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use crate::cli::{fmt_state, resolve_target};
use crate::sonar::client::{HttpSonar, Method};
use crate::sonar::discovery::excerpt;
use crate::sonar::{Channel, Mode, SonarApi, Target, round_volume, routes};

const SETTLE: Duration = Duration::from_millis(300);
/// Hold after a visible change so the user can check the Sonar window (Q2).
const WATCH: Duration = Duration::from_millis(2000);

struct Outcome {
    label: String,
    path: String,
    status: String,
    applied: bool,
}

pub fn run(api: &mut HttpSonar, pos: &[String], routing: bool) -> Result<()> {
    let channel: Channel = match pos.first() {
        Some(s) => s.parse().map_err(|e: String| anyhow!(e))?,
        None => Channel::Game,
    };
    if channel == Channel::Master {
        bail!("pick a non-master channel; master is tested automatically");
    }
    let mode = api.mode()?;
    println!(
        "Sonar {} mode. Testing writes on `{channel}` and `master`{}.",
        mode.label(),
        if mode == Mode::Stream {
            " (monitoring mix)"
        } else {
            ""
        }
    );
    println!(
        "Keep the Sonar window open and watch the {channel} and master faders.\n\
         After each applied change there's a 2 s pause: note whether the fader\n\
         moved straight away (Q2).\n"
    );

    let mut results = Vec::new();
    let target = resolve_target(mode, channel, None);
    let master = resolve_target(mode, Channel::Master, None);
    let (vol_kws, mute_kws) = match mode {
        Mode::Classic => (
            routes::CLASSIC_VOLUME_CANDIDATES,
            routes::CLASSIC_MUTE_CANDIDATES,
        ),
        Mode::Stream => (
            routes::STREAM_VOLUME_CANDIDATES,
            routes::STREAM_MUTE_CANDIDATES,
        ),
    };
    let chan_token = routes::volume_token(channel);

    // A. volume keyword
    let mut working_kw = None;
    for kw in vol_kws {
        let o = try_volume(api, mode, target, chan_token, kw, results.len())?;
        if o.applied && working_kw.is_none() {
            working_kw = Some(*kw);
        }
        results.push(o);
    }
    let vol_kw = working_kw.unwrap_or(vol_kws[0]);

    // B. master token (using the volume keyword that worked)
    for tok in routes::MASTER_TOKEN_CANDIDATES {
        results.push(try_volume(api, mode, master, tok, vol_kw, results.len())?);
    }

    // C. mute keyword
    for kw in mute_kws {
        results.push(try_mute(api, mode, target, chan_token, kw)?);
    }

    // D. device-ID encoding
    if routing {
        if mode == Mode::Classic {
            results.extend(try_routing(api, channel)?);
        } else {
            println!("(--routing skipped: the Streamer device route is still unknown, Q4)");
        }
    }

    println!("\n==== Results ({} mode) ====", mode.label());
    for r in &results {
        println!(
            "{:<4} {:<28} {:<10} {}",
            if r.applied { "OK" } else { "--" },
            r.label,
            r.status,
            r.path
        );
    }
    println!(
        "\nPaste this table back, plus for Q2: did the Sonar faders move on their own\n\
         for each OK volume line, only after reopening the Sonar window, or never?"
    );
    Ok(())
}

fn put(api: &mut HttpSonar, path: &str) -> Result<String> {
    let (status, body) = api.request_raw(Method::Put, path)?;
    let mut s = status.to_string();
    if body.contains(routes::WRONG_MODE_TEXT) {
        s.push_str(" wrongmode");
    }
    if !(200..300).contains(&status) || !body.is_empty() {
        println!("    -> HTTP {status} {}", excerpt(&body));
    }
    Ok(s)
}

fn read(api: &mut HttpSonar, mode: Mode, t: Target) -> Result<crate::sonar::ChannelState> {
    api.volumes(mode)?
        .get(t)
        .ok_or_else(|| anyhow!("Sonar didn't report {t}"))
}

fn try_volume(
    api: &mut HttpSonar,
    mode: Mode,
    t: Target,
    token: &str,
    kw: &str,
    i: usize,
) -> Result<Outcome> {
    let before = read(api, mode, t)?;
    let original = round_volume(before.volume)?;
    // A value that differs from the current one (Sonar ignores equal writes).
    let delta = 0.02 + 0.01 * (i % 3) as f64;
    let wanted = round_volume(if original >= 0.5 {
        original - delta
    } else {
        original + delta
    })?;
    let path = routes::volume_write(t, token, kw, &routes::format_volume(wanted));
    println!("{t}: PUT {path}");
    let status = put(api, &path)?;
    sleep(SETTLE);
    let after = read(api, mode, t)?;
    let applied = (after.volume - wanted).abs() < 0.005;
    println!(
        "    {} -> {}",
        fmt_state(Some(before)),
        fmt_state(Some(after))
    );
    if applied {
        sleep(WATCH);
        // Restore through the same (working) path.
        let restore = routes::volume_write(t, token, kw, &routes::format_volume(original));
        put(api, &restore)?;
        sleep(SETTLE);
    }
    Ok(Outcome {
        label: format!("volume {token}/{kw}"),
        path,
        status,
        applied,
    })
}

fn try_mute(api: &mut HttpSonar, mode: Mode, t: Target, token: &str, kw: &str) -> Result<Outcome> {
    let before = read(api, mode, t)?;
    let want = !before.muted;
    let path = routes::volume_write(t, token, kw, if want { "true" } else { "false" });
    println!("{t}: PUT {path}");
    let status = put(api, &path)?;
    sleep(SETTLE);
    let after = read(api, mode, t)?;
    let applied = after.muted == want;
    println!(
        "    {} -> {}",
        fmt_state(Some(before)),
        fmt_state(Some(after))
    );
    if applied {
        sleep(WATCH);
        let restore =
            routes::volume_write(t, token, kw, if before.muted { "true" } else { "false" });
        put(api, &restore)?;
        sleep(SETTLE);
    }
    Ok(Outcome {
        label: format!("mute {token}/{kw}"),
        path,
        status,
        applied,
    })
}

/// Re-send the channel's current device, raw and encoded. Success here means
/// "accepted", not "switched" (it's the same device); `route` verifies a real
/// switch.
fn try_routing(api: &mut HttpSonar, channel: Channel) -> Result<Vec<Outcome>> {
    let routing = api.routing()?;
    let id = routing
        .classic
        .get(&channel)
        .ok_or_else(|| anyhow!("no classic redirection for {channel}"))?
        .clone();
    let mut out = Vec::new();
    for encode in [false, true] {
        let Some(path) = routes::set_classic_device_with(channel, &id, encode) else {
            continue;
        };
        println!("{channel}: PUT {path}  (same device, no audible change expected)");
        let status = put(api, &path)?;
        sleep(Duration::from_millis(800));
        let now = api.routing()?;
        let unchanged = now.classic.get(&channel) == Some(&id);
        out.push(Outcome {
            label: format!(
                "route id {}",
                if encode { "percent-encoded" } else { "raw" }
            ),
            applied: status.starts_with('2') && unchanged,
            path,
            status,
        });
    }
    Ok(out)
}
