//! CLI commands for driving Sonar by hand (FR-10.3). Each command is a thin
//! wrapper over [`SonarApi`] that also reads state back, so every write path
//! can be verified on a real machine.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::sonar::client::{HttpSonar, Method};
use crate::sonar::{
    AudioDevice, Channel, ChannelState, DataFlow, Mix, Mode, SonarApi, StreamOutput, Target,
    Volumes, match_device, round_volume, virtual_device_for, virtual_device_for_flow,
};

/// Time to let Sonar settle before reading a write back.
const SETTLE: Duration = Duration::from_millis(200);

pub const USAGE: &str = "  mode [classic|stream]          show the mode, or switch it (disruptive)
  get                            show volumes and routing for the current mode
  devices [--all]                list output/input devices (--all: include Sonar's virtual ones)
  set <channel> <0-100> [--mix monitoring|streaming] [--via rest|endpoint]
                                 set a volume and read it back. --via endpoint (Classic,
                                 not master) sets the Windows volume of Sonar's virtual device
  mute <channel> [on|off|toggle] [--mix monitoring|streaming] [--via rest|endpoint]
                                 --endpoint-flow render|capture picks which Sonar virtual
                                 device --via endpoint uses (mic exists as both)
  route <target> <device-name-substring>
                                 Classic: target = game|chat|media|aux|mic|all
                                 Streamer: target = monitoring|streaming|mic
  try-writes [channel] [--routing]
                                 try every candidate write-route spelling and
                                 read back which ones Sonar applies (Q2/Q3)
  raw-get <path>                 GET any Sonar path and print status + body
  raw-put <path>                 PUT any Sonar path (no body) and print status + body

channels: master, game, chat, media, aux, mic. In Streamer mode --mix defaults to monitoring.";

/// Returns `Ok(false)` if `cmd` isn't a CLI command.
pub fn run(cmd: &str, args: &[String]) -> Result<bool> {
    let (pos, flags) = split_flags(args)?;
    let mut api = HttpSonar::new();
    match cmd {
        "mode" => mode(&mut api, &pos)?,
        "get" => get(&mut api)?,
        "devices" => devices(&mut api, flags.all)?,
        "set" => set(&mut api, &pos, &flags)?,
        "mute" => mute(&mut api, &pos, &flags)?,
        "route" => route(&mut api, &pos)?,
        "raw-get" => raw(&mut api, Method::Get, &pos)?,
        "raw-put" => raw(&mut api, Method::Put, &pos)?,
        "try-writes" => crate::try_writes::run(&mut api, &pos, flags.routing)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// Which write path `set`/`mute` use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Via {
    /// Sonar's REST API (changes audio, not the GG UI).
    #[default]
    Rest,
    /// Windows endpoint volume of Sonar's virtual device (FR-3.7).
    Endpoint,
}

/// Find the virtual device for `target` (Classic, non-master only).
fn endpoint_device(
    api: &mut HttpSonar,
    mode: Mode,
    target: Target,
    flow: Option<DataFlow>,
) -> Result<String> {
    if mode != Mode::Classic {
        bail!("--via endpoint only works in Classic mode");
    }
    let devices = api.devices()?;
    let d = match flow {
        Some(f) => virtual_device_for_flow(&devices, target.channel(), f),
        None => virtual_device_for(&devices, target.channel()),
    }
    .ok_or_else(|| {
        anyhow!(
            "{} has no Sonar virtual device (master always uses REST)",
            target.channel()
        )
    })?;
    println!(
        "via Windows endpoint volume of \"{}\" ({:?}, {})",
        d.name, d.flow, d.id
    );
    Ok(d.id.clone())
}

/// Endpoint-volume writes, with the Windows scalar printed before/after.
#[cfg(windows)]
mod endpoint {
    use crate::platform::endpoint_volume::EndpointVolume;
    use anyhow::Result;

    pub fn set_volume(id: &str, v: f64) -> Result<()> {
        let ep = EndpointVolume::new()?;
        let before = ep.volume(id)?;
        ep.set_volume(id, v as f32)?;
        println!("  Windows scalar: {before:.4} -> {:.4}", ep.volume(id)?);
        Ok(())
    }

    pub fn set_mute(id: &str, muted: bool) -> Result<()> {
        let ep = EndpointVolume::new()?;
        let before = ep.muted(id)?;
        ep.set_mute(id, muted)?;
        println!("  Windows mute: {before} -> {}", ep.muted(id)?);
        Ok(())
    }
}

#[cfg(not(windows))]
mod endpoint {
    use anyhow::{Result, bail};

    pub fn set_volume(_id: &str, _v: f64) -> Result<()> {
        bail!("--via endpoint is Windows-only")
    }

    pub fn set_mute(_id: &str, _muted: bool) -> Result<()> {
        bail!("--via endpoint is Windows-only")
    }
}

#[derive(Default)]
struct Flags {
    mix: Option<Mix>,
    via: Via,
    endpoint_flow: Option<DataFlow>,
    all: bool,
    routing: bool,
}

fn split_flags(args: &[String]) -> Result<(Vec<String>, Flags)> {
    let mut pos = Vec::new();
    let mut flags = Flags::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mix" => {
                let m = it.next().ok_or_else(|| anyhow!("--mix needs a value"))?;
                flags.mix = Some(m.parse().map_err(|e: String| anyhow!(e))?);
            }
            "--all" => flags.all = true,
            "--routing" => flags.routing = true,
            "--endpoint-flow" => {
                flags.endpoint_flow = match it.next().map(String::as_str) {
                    Some("render") => Some(DataFlow::Render),
                    Some("capture") => Some(DataFlow::Capture),
                    _ => bail!("--endpoint-flow needs `render` or `capture`"),
                }
            }
            "--via" => {
                flags.via = match it.next().map(String::as_str) {
                    Some("rest") => Via::Rest,
                    Some("endpoint") => Via::Endpoint,
                    _ => bail!("--via needs `rest` or `endpoint`"),
                }
            }
            f if f.starts_with("--") => bail!("unknown flag `{f}`"),
            _ => pos.push(a.clone()),
        }
    }
    Ok((pos, flags))
}

fn parse_channel(s: Option<&String>) -> Result<Channel> {
    s.ok_or_else(|| anyhow!("missing <channel>"))?
        .parse()
        .map_err(|e: String| anyhow!(e))
}

/// Resolve a channel (+ optional mix) to a target in Sonar's current mode.
pub fn resolve_target(mode: Mode, channel: Channel, mix: Option<Mix>) -> Target {
    match mode {
        Mode::Classic => {
            if mix.is_some() {
                eprintln!("note: --mix ignored in Classic mode");
            }
            Target::Classic(channel)
        }
        Mode::Stream => Target::Stream(mix.unwrap_or(Mix::Monitoring), channel),
    }
}

pub fn fmt_state(s: Option<ChannelState>) -> String {
    match s {
        Some(s) => format!(
            "{:>4.0}%{}  ({:.6})",
            s.volume * 100.0,
            if s.muted { " MUTED" } else { "      " },
            s.volume
        ),
        None => "   (n/a)".to_owned(),
    }
}

fn device_name(devices: &[AudioDevice], id: &str) -> String {
    devices
        .iter()
        .find(|d| d.id == id)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| format!("<unknown device {id}>"))
}

// --- commands ----------------------------------------------------------------

fn mode(api: &mut HttpSonar, pos: &[String]) -> Result<()> {
    if let Some(m) = pos.first() {
        let m: Mode = m.parse().map_err(|e: String| anyhow!(e))?;
        api.set_mode(m)?;
        sleep(Duration::from_millis(500));
    }
    println!("{}", api.mode()?.label());
    Ok(())
}

fn print_volumes(mode: Mode, v: &Volumes) {
    match mode {
        Mode::Classic => {
            for c in Channel::ALL {
                println!("  {:<7} {}", c.name(), fmt_state(v.get(Target::Classic(c))));
            }
        }
        Mode::Stream => {
            println!("  {:<7} {:<26} streaming", "", "monitoring");
            for c in Channel::ALL {
                println!(
                    "  {:<7} {:<26} {}",
                    c.name(),
                    fmt_state(v.get(Target::Stream(Mix::Monitoring, c))),
                    fmt_state(v.get(Target::Stream(Mix::Streaming, c)))
                );
            }
        }
    }
}

fn get(api: &mut HttpSonar) -> Result<()> {
    let mode = api.mode()?;
    println!("Sonar: {}  mode: {}", api.base_url()?, mode.label());
    println!("\nVolumes:");
    print_volumes(mode, &api.volumes(mode)?);
    let devices = api.devices()?;
    let routing = api.routing()?;
    println!("\nRouting:");
    match mode {
        Mode::Classic => {
            for (c, id) in &routing.classic {
                println!("  {:<7} -> {}", c.name(), device_name(&devices, id));
            }
        }
        Mode::Stream => {
            for (out, r) in &routing.stream {
                let chans: Vec<_> = r.enabled.iter().map(|c| c.name()).collect();
                println!(
                    "  {:<10} -> {}{}",
                    out.name(),
                    device_name(&devices, &r.device_id),
                    if chans.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", chans.join(", "))
                    }
                );
            }
        }
    }
    Ok(())
}

fn devices(api: &mut HttpSonar, all: bool) -> Result<()> {
    let devices = api.devices()?;
    for (title, flow) in [("Outputs", DataFlow::Render), ("Inputs", DataFlow::Capture)] {
        println!("{title}:");
        for d in devices
            .iter()
            .filter(|d| d.flow == flow && (all || !d.is_virtual))
        {
            let mut tags = Vec::new();
            if d.is_virtual {
                tags.push(format!("Sonar virtual, role={}", d.role));
            }
            if !d.active {
                tags.push("inactive".to_owned());
            }
            let tags = if tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", tags.join(", "))
            };
            println!("  {}{tags}\n      {}", d.name, d.id);
        }
    }
    Ok(())
}

fn set(api: &mut HttpSonar, pos: &[String], flags: &Flags) -> Result<()> {
    let channel = parse_channel(pos.first())?;
    let pct: f64 = pos
        .get(1)
        .ok_or_else(|| anyhow!("missing <0-100>"))?
        .parse()
        .context("volume must be a number 0-100")?;
    if !(0.0..=100.0).contains(&pct) {
        bail!("volume must be 0-100");
    }
    let mode = api.mode()?;
    let target = resolve_target(mode, channel, flags.mix);
    let before = api.volumes(mode)?.get(target);
    match flags.via {
        Via::Rest => api.set_volume(target, pct / 100.0)?,
        Via::Endpoint => {
            let id = endpoint_device(api, mode, target, flags.endpoint_flow)?;
            endpoint::set_volume(&id, round_volume(pct / 100.0)?)?;
        }
    }
    sleep(SETTLE);
    let after = api.volumes(mode)?.get(target);
    println!("{target}: {} -> {}", fmt_state(before), fmt_state(after));
    if let Some(a) = after
        && (a.volume - pct / 100.0).abs() > 0.005
    {
        println!(
            "WARNING: Sonar reports {:.4}, not {:.2}. The write was accepted but not applied.",
            a.volume,
            pct / 100.0
        );
    }
    Ok(())
}

fn mute(api: &mut HttpSonar, pos: &[String], flags: &Flags) -> Result<()> {
    let channel = parse_channel(pos.first())?;
    let mode = api.mode()?;
    let target = resolve_target(mode, channel, flags.mix);
    let before = api
        .volumes(mode)?
        .get(target)
        .ok_or_else(|| anyhow!("Sonar didn't report {target}"))?;
    let want = match pos.get(1).map(String::as_str) {
        None | Some("toggle") => !before.muted,
        Some("on") => true,
        Some("off") => false,
        Some(o) => bail!("expected on, off or toggle, got `{o}`"),
    };
    match flags.via {
        Via::Rest => api.set_mute(target, want)?,
        Via::Endpoint => {
            let id = endpoint_device(api, mode, target, flags.endpoint_flow)?;
            endpoint::set_mute(&id, want)?;
        }
    }
    sleep(SETTLE);
    let after = api.volumes(mode)?.get(target);
    println!(
        "{target}: {} -> {}",
        fmt_state(Some(before)),
        fmt_state(after)
    );
    if after.map(|a| a.muted) != Some(want) {
        println!("WARNING: Sonar didn't apply the mute change.");
    }
    Ok(())
}

fn route(api: &mut HttpSonar, pos: &[String]) -> Result<()> {
    let what = pos.first().ok_or_else(|| anyhow!("missing <target>"))?;
    let pattern = pos
        .get(1)
        .ok_or_else(|| anyhow!("missing <device-name-substring>"))?;
    let mode = api.mode()?;
    let devices = api.devices()?;
    let vols_before = api.volumes(mode)?;

    match mode {
        Mode::Classic => {
            let channels: Vec<Channel> = if what.eq_ignore_ascii_case("all") {
                vec![Channel::Game, Channel::Chat, Channel::Media, Channel::Aux]
            } else {
                let c: Channel = what.parse().map_err(|e: String| anyhow!(e))?;
                if c == Channel::Master {
                    bail!("master has no device of its own; use `route all <device>`");
                }
                vec![c]
            };
            for c in channels {
                let flow = if c == Channel::Mic {
                    DataFlow::Capture
                } else {
                    DataFlow::Render
                };
                let dev = match_device(&devices, flow, pattern).map_err(|e| anyhow!(e))?;
                api.set_classic_device(c, &dev.id)?;
                println!("{c} -> {}", dev.name);
            }
        }
        Mode::Stream => {
            let out = match what.to_ascii_lowercase().as_str() {
                "mic" => StreamOutput::Mic,
                m => StreamOutput::Mix(m.parse().map_err(|e: String| anyhow!(e))?),
            };
            let flow = if out == StreamOutput::Mic {
                DataFlow::Capture
            } else {
                DataFlow::Render
            };
            let dev = match_device(&devices, flow, pattern).map_err(|e| anyhow!(e))?;
            api.set_stream_device(out, &dev.id)?;
            println!("{} -> {}", out.name(), dev.name);
        }
    }

    // Report whether the device switch disturbed any volumes (known quirk:
    // writes during the audio-path rebuild are dropped, and some reports say
    // volumes reset).
    for (wait_ms, delta_ms) in [(500u64, 500u64), (1500, 1000)] {
        sleep(Duration::from_millis(delta_ms));
        let now = api.volumes(mode)?;
        let changed: Vec<String> = now
            .0
            .iter()
            .filter(|(t, s)| vols_before.get(**t) != Some(**s))
            .map(|(t, s)| {
                format!(
                    "{t}: {} -> {}",
                    fmt_state(vols_before.get(*t)),
                    fmt_state(Some(*s))
                )
            })
            .collect();
        if changed.is_empty() {
            println!("+{wait_ms} ms: volumes unchanged");
        } else {
            println!(
                "+{wait_ms} ms: volumes changed:\n  {}",
                changed.join("\n  ")
            );
        }
    }
    Ok(())
}

fn raw(api: &mut HttpSonar, method: Method, pos: &[String]) -> Result<()> {
    let path = pos.first().ok_or_else(|| anyhow!("missing <path>"))?;
    if !path.starts_with('/') {
        bail!("path must start with `/`");
    }
    let (status, body) = api.request_raw(method, path)?;
    println!("HTTP {status}\n{body}");
    Ok(())
}
