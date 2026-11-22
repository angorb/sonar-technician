//! TOML config: load, validate, defaults (FR-9). Plus the small state file
//! that remembers the active channel/mix across restarts (FR-2.5).
//!
//! The config lives at `%APPDATA%\sonar-technician\config.toml` and is written
//! with commented defaults ([`DEFAULT_CONFIG_TOML`]) on first run. An invalid
//! file is reported with line/column and the previous config stays active.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::actions::{Action, Scope};
use crate::chord::Chord;
use crate::controller::{Acceleration, ControllerConfig};
use crate::sonar::{Channel, Mix};

pub const APP_DIR: &str = "sonar-technician";

/// The file written on first run. Must parse to `Config::default()` (tested).
pub const DEFAULT_CONFIG_TOML: &str = r#"# sonar-technician configuration
#
# Changes are picked up automatically when you save (or use "Reload config"
# in the tray menu). If the file has an error, the previous settings stay
# active and the error is shown.
#
# IMPORTANT: unbind any keys used below from SteelSeries GG's own Sonar
# hotkeys, or both will react.

# Log detail: "error", "warn", "info", "debug" or "trace". Needs a restart.
log_level = "info"

[volume]
# Step for volume_up / volume_down. 0.01 = 1%.
step = 0.01

# How Classic-mode channel volume/mute is written:
#   "windows": set the Windows volume of Sonar's virtual device, so the GG
#              window follows the change (recommended)
#   "rest":    Sonar's API only (changes audio, but the GG window doesn't update)
# Master and Streamer mode always use Sonar's API.
classic_write = "windows"

# Bigger steps when the knob is spun quickly: if `threshold` steps in the same
# direction arrive within `window_ms`, each further step is multiplied.
[volume.acceleration]
enabled = false
window_ms = 80
threshold = 4
multiplier = 3.0

[channels]
# Order of next_channel / prev_channel. Any of:
# "master", "game", "chat", "media", "aux", "mic"
cycle = ["master", "game", "chat", "media", "aux"]

[devices]
# Output devices to rotate through with next/prev_output_device, as
# case-insensitive name fragments, e.g. ["Headphones", "Realtek"].
# Empty = every connected device. Run `sonar-technician devices` for names.
cycle = []

[overlay]
enabled = true
# "top-left", "top-center", "top-right", "bottom-left", "bottom-center", "bottom-right"
position = "bottom-right"
# Pixels from that corner: [x, y]
offset = [24, 24]
timeout_ms = 1500
# 0.0 (invisible) .. 1.0 (opaque)
opacity = 0.9
# Size multiplier on top of the monitor's DPI scaling.
scale = 1.0
# "primary" or "cursor" (the monitor the mouse is on)
monitor = "cursor"

[sonar]
# How often to check for a Classic/Streamer mode change.
mode_poll_ms = 2000
# How often to re-read volumes (picks up changes made in the Sonar window).
volume_poll_ms = 5000
# Re-read volumes this long after the last key press.
idle_resync_ms = 3000

# Key bindings. Keys: F1-F24, A-Z, 0-9, NumPad0-9, Space, Enter, Tab, Esc,
# Insert, Delete, Home, End, PageUp, PageDown, Up, Down, Left, Right, Pause,
# ScrollLock, PrintScreen, VolumeUp, VolumeDown, VolumeMute, MediaNext,
# MediaPrev, MediaStop, MediaPlayPause; with Ctrl/Alt/Shift/Win, e.g. "Ctrl+F17".
#
# Actions and their options:
#   volume_up / volume_down         step = 0.05 (optional, overrides [volume] step)
#   set_volume                      value = 0..100 (percent)
#   toggle_mute
#   next_channel / prev_channel
#   select_channel                  channel = "game"
#   toggle_mix                      (Streamer mode: monitoring <-> streaming)
#   next_output_device / prev_output_device
#                                   scope = "active" (default) or "all"
#   set_output_device               device = "Headphones", scope = "active"|"all"
#   toggle_mode                     (Classic <-> Streamer; disruptive)
#
# scope = "all": in Classic mode, game/chat/media/aux together; in Streamer
# mode, the monitoring mix (what you hear).

# Keychron knob: turn right
[[bindings]]
keys = "F19"
action = "volume_up"

# Knob: turn left
[[bindings]]
keys = "F18"
action = "volume_down"

# Knob: press
[[bindings]]
keys = "F17"
action = "next_channel"

[[bindings]]
keys = "Shift+F17"
action = "toggle_mute"

[[bindings]]
keys = "Ctrl+F17"
action = "next_output_device"
scope = "all"

[[bindings]]
keys = "Alt+F17"
action = "toggle_mix"
"#;

// ---------------------------------------------------------------------------
// Validated config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorChoice {
    Primary,
    Cursor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub position: Corner,
    pub offset: (i32, i32),
    pub timeout: Duration,
    pub opacity: f32,
    pub scale: f32,
    pub monitor: MonitorChoice,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub chord: Chord,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub log_level: String,
    pub controller: ControllerConfig,
    pub overlay: OverlayConfig,
    pub bindings: Vec<Binding>,
}

impl Default for Config {
    fn default() -> Self {
        // Can't fail: DEFAULT_CONFIG_TOML is a constant and
        // `default_file_parses_to_expected_defaults` checks it parses.
        parse(DEFAULT_CONFIG_TOML).expect("built-in default config must parse")
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("can't read {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("can't write {}: {source}", .path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("config line {line}, column {column}: {message}")]
    Syntax {
        line: usize,
        column: usize,
        message: String,
    },
    #[error("config error: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

// ---------------------------------------------------------------------------
// Raw (serde) shape
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default = "d_log_level")]
    log_level: String,
    #[serde(default)]
    volume: RawVolume,
    #[serde(default)]
    channels: RawChannels,
    #[serde(default)]
    devices: RawDevices,
    #[serde(default)]
    overlay: RawOverlay,
    #[serde(default)]
    sonar: RawSonar,
    #[serde(default)]
    bindings: Vec<RawBinding>,
}

fn d_log_level() -> String {
    "info".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawVolume {
    step: f64,
    classic_write: String,
    acceleration: RawAccel,
}

impl Default for RawVolume {
    fn default() -> Self {
        Self {
            step: 0.01,
            classic_write: "windows".into(),
            acceleration: RawAccel::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawAccel {
    enabled: bool,
    window_ms: u64,
    threshold: usize,
    multiplier: f64,
}

impl Default for RawAccel {
    fn default() -> Self {
        Self {
            enabled: false,
            window_ms: 80,
            threshold: 4,
            multiplier: 3.0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawChannels {
    cycle: Vec<String>,
}

impl Default for RawChannels {
    fn default() -> Self {
        Self {
            cycle: ["master", "game", "chat", "media", "aux"]
                .map(String::from)
                .to_vec(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawDevices {
    cycle: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawOverlay {
    enabled: bool,
    position: String,
    offset: [i32; 2],
    timeout_ms: u64,
    opacity: f32,
    scale: f32,
    monitor: String,
}

impl Default for RawOverlay {
    fn default() -> Self {
        Self {
            enabled: true,
            position: "bottom-right".into(),
            offset: [24, 24],
            timeout_ms: 1500,
            opacity: 0.9,
            scale: 1.0,
            monitor: "cursor".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawSonar {
    mode_poll_ms: u64,
    volume_poll_ms: u64,
    idle_resync_ms: u64,
}

impl Default for RawSonar {
    fn default() -> Self {
        Self {
            mode_poll_ms: 2000,
            volume_poll_ms: 5000,
            idle_resync_ms: 3000,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBinding {
    keys: String,
    action: String,
    step: Option<f64>,
    value: Option<f64>,
    channel: Option<String>,
    device: Option<String>,
    scope: Option<String>,
}

// ---------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------

fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let raw: Raw = toml::from_str(text).map_err(|e| {
        let (line, column) = e.span().map_or((0, 0), |s| line_col(text, s.start));
        ConfigError::Syntax {
            line,
            column,
            message: e.message().trim().to_owned(),
        }
    })?;
    validate(raw)
}

fn parse_scope(s: Option<&str>) -> Result<Scope, String> {
    match s {
        None | Some("active") => Ok(Scope::Active),
        Some("all") => Ok(Scope::All),
        Some(o) => Err(format!("scope `{o}` must be \"active\" or \"all\"")),
    }
}

fn binding_action(b: &RawBinding) -> Result<Action, String> {
    // Which options each action accepts.
    let allowed: &[&str] = match b.action.as_str() {
        "volume_up" | "volume_down" => &["step"],
        "set_volume" => &["value"],
        "select_channel" => &["channel"],
        "next_output_device" | "prev_output_device" => &["scope"],
        "set_output_device" => &["device", "scope"],
        _ => &[],
    };
    for (name, present) in [
        ("step", b.step.is_some()),
        ("value", b.value.is_some()),
        ("channel", b.channel.is_some()),
        ("device", b.device.is_some()),
        ("scope", b.scope.is_some()),
    ] {
        if present && !allowed.contains(&name) {
            return Err(format!("`{name}` doesn't apply to action `{}`", b.action));
        }
    }
    let step = match b.step {
        Some(s) if !(s > 0.0 && s <= 1.0) => {
            return Err(format!("step {s} must be > 0 and <= 1 (0.01 = 1%)"));
        }
        s => s,
    };
    Ok(match b.action.as_str() {
        "volume_up" => Action::VolumeUp(step),
        "volume_down" => Action::VolumeDown(step),
        "set_volume" => {
            let v = b.value.ok_or("set_volume needs `value = 0..100`")?;
            if !(0.0..=100.0).contains(&v) {
                return Err(format!("value {v} must be 0..100"));
            }
            Action::SetVolume(v / 100.0)
        }
        "toggle_mute" => Action::ToggleMute,
        "next_channel" => Action::NextChannel,
        "prev_channel" => Action::PrevChannel,
        "select_channel" => Action::SelectChannel(
            b.channel
                .as_deref()
                .ok_or("select_channel needs `channel = \"...\"`")?
                .parse()?,
        ),
        "toggle_mix" => Action::ToggleMix,
        "next_output_device" => Action::NextOutputDevice(parse_scope(b.scope.as_deref())?),
        "prev_output_device" => Action::PrevOutputDevice(parse_scope(b.scope.as_deref())?),
        "set_output_device" => Action::SetOutputDevice {
            pattern: b
                .device
                .clone()
                .filter(|d| !d.trim().is_empty())
                .ok_or("set_output_device needs `device = \"name fragment\"`")?,
            scope: parse_scope(b.scope.as_deref())?,
        },
        "toggle_mode" => Action::ToggleMode,
        other => return Err(format!("unknown action `{other}`")),
    })
}

fn validate(raw: Raw) -> Result<Config, ConfigError> {
    let mut errs: Vec<String> = Vec::new();

    if !["error", "warn", "info", "debug", "trace"].contains(&raw.log_level.as_str()) {
        errs.push(format!(
            "log_level `{}` is not error/warn/info/debug/trace",
            raw.log_level
        ));
    }

    let v = &raw.volume;
    if !(v.step > 0.0 && v.step <= 1.0) {
        errs.push(format!(
            "[volume] step {} must be > 0 and <= 1 (0.01 = 1%)",
            v.step
        ));
    }
    let use_endpoint_volume = match v.classic_write.as_str() {
        "windows" => true,
        "rest" => false,
        o => {
            errs.push(format!(
                "[volume] classic_write `{o}` must be \"windows\" or \"rest\""
            ));
            true
        }
    };
    let a = &v.acceleration;
    if a.threshold < 2 || !(a.multiplier >= 1.0 && a.multiplier <= 20.0) || a.window_ms == 0 {
        errs.push(
            "[volume.acceleration] needs threshold >= 2, multiplier 1..20, window_ms > 0".into(),
        );
    }

    let mut cycle = Vec::new();
    for c in &raw.channels.cycle {
        match c.parse::<Channel>() {
            Ok(ch) if cycle.contains(&ch) => errs.push(format!("[channels] `{c}` listed twice")),
            Ok(ch) => cycle.push(ch),
            Err(e) => errs.push(format!("[channels] {e}")),
        }
    }
    if cycle.is_empty() {
        errs.push("[channels] cycle needs at least one channel".into());
    }

    let o = &raw.overlay;
    let position = match o.position.as_str() {
        "top-left" => Corner::TopLeft,
        "top-center" => Corner::TopCenter,
        "top-right" => Corner::TopRight,
        "bottom-left" => Corner::BottomLeft,
        "bottom-center" => Corner::BottomCenter,
        "bottom-right" => Corner::BottomRight,
        p => {
            errs.push(format!("[overlay] position `{p}` is not a known corner"));
            Corner::BottomRight
        }
    };
    let monitor = match o.monitor.as_str() {
        "primary" => MonitorChoice::Primary,
        "cursor" => MonitorChoice::Cursor,
        m => {
            errs.push(format!(
                "[overlay] monitor `{m}` must be \"primary\" or \"cursor\""
            ));
            MonitorChoice::Cursor
        }
    };
    if !(0.0..=1.0).contains(&o.opacity) {
        errs.push(format!("[overlay] opacity {} must be 0..1", o.opacity));
    }
    if !(0.25..=4.0).contains(&o.scale) {
        errs.push(format!("[overlay] scale {} must be 0.25..4", o.scale));
    }

    let s = &raw.sonar;
    if s.mode_poll_ms < 250 || s.volume_poll_ms < 250 || s.idle_resync_ms < 250 {
        errs.push("[sonar] intervals must be at least 250 ms".into());
    }

    let mut bindings: Vec<Binding> = Vec::new();
    let mut seen: Vec<(Chord, usize)> = Vec::new();
    for (i, b) in raw.bindings.iter().enumerate() {
        let at = format!("binding #{} (`{}`)", i + 1, b.keys);
        let chord = match b.keys.parse::<Chord>() {
            Ok(c) => c,
            Err(e) => {
                errs.push(format!("binding #{}: {e}", i + 1));
                continue;
            }
        };
        if let Some((_, prev)) = seen.iter().find(|(c, _)| *c == chord) {
            errs.push(format!("{at}: {chord} is already bound by binding #{prev}"));
            continue;
        }
        seen.push((chord, i + 1));
        match binding_action(b) {
            Ok(action) => bindings.push(Binding { chord, action }),
            Err(e) => errs.push(format!("{at}: {e}")),
        }
    }

    if !errs.is_empty() {
        return Err(ConfigError::Invalid(errs));
    }
    let ms = Duration::from_millis;
    Ok(Config {
        log_level: raw.log_level,
        controller: ControllerConfig {
            step: v.step,
            acceleration: Acceleration {
                enabled: a.enabled,
                window: ms(a.window_ms),
                threshold: a.threshold,
                multiplier: a.multiplier,
            },
            channel_cycle: cycle,
            device_cycle: raw.devices.cycle,
            use_endpoint_volume,
            mode_poll: ms(s.mode_poll_ms),
            volume_poll: ms(s.volume_poll_ms),
            idle_resync: ms(s.idle_resync_ms),
            ..ControllerConfig::default()
        },
        overlay: OverlayConfig {
            enabled: o.enabled,
            position,
            offset: (o.offset[0], o.offset[1]),
            timeout: ms(o.timeout_ms),
            opacity: o.opacity,
            scale: o.scale,
            monitor,
        },
        bindings,
    })
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// `%APPDATA%\sonar-technician`
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join(APP_DIR))
}

/// `%LOCALAPPDATA%\sonar-technician`
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(APP_DIR))
}

/// Load `path`, writing the commented default first if it doesn't exist.
pub fn load_or_create(path: &Path) -> Result<Config, ConfigError> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| ConfigError::Write {
                path: dir.to_owned(),
                source,
            })?;
        }
        std::fs::write(path, DEFAULT_CONFIG_TOML).map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
        tracing::info!("wrote default config to {}", path.display());
    }
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    parse(&text)
}

/// Active channel/mix, persisted across restarts (FR-2.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiState {
    channel: String,
    mix: String,
}

impl UiState {
    pub fn new(channel: Channel, mix: Mix) -> Self {
        Self {
            channel: channel.name().to_owned(),
            mix: mix.name().to_owned(),
        }
    }

    /// Falls back to game / monitoring on anything unreadable.
    pub fn load(path: &Path) -> (Channel, Mix) {
        let s: Option<UiState> = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        (
            s.as_ref()
                .and_then(|s| s.channel.parse().ok())
                .unwrap_or(Channel::Game),
            s.as_ref()
                .and_then(|s| s.mix.parse().ok())
                .unwrap_or(Mix::Monitoring),
        )
    }

    pub fn save(&self, path: &Path) {
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, serde_json::to_string(self).unwrap_or_default()));
        if let Err(e) = result {
            tracing::warn!("couldn't save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::{MOD_ALT, MOD_CONTROL, MOD_SHIFT};

    #[test]
    fn default_file_parses_to_expected_defaults() {
        let c = parse(DEFAULT_CONFIG_TOML).unwrap();
        assert_eq!(c.controller, ControllerConfig::default());
        assert_eq!(c.log_level, "info");
        assert!(c.overlay.enabled);
        assert_eq!(c.overlay.position, Corner::BottomRight);
        let f = |n: u32| 0x70 + n - 1;
        let expect = [
            (0, f(19), Action::VolumeUp(None)),
            (0, f(18), Action::VolumeDown(None)),
            (0, f(17), Action::NextChannel),
            (MOD_SHIFT, f(17), Action::ToggleMute),
            (MOD_CONTROL, f(17), Action::NextOutputDevice(Scope::All)),
            (MOD_ALT, f(17), Action::ToggleMix),
        ];
        assert_eq!(c.bindings.len(), expect.len());
        for (b, (mods, vk, action)) in c.bindings.iter().zip(expect) {
            assert_eq!(b.chord, Chord { mods, vk });
            assert_eq!(b.action, action);
        }
    }

    #[test]
    fn empty_file_gets_defaults_without_bindings() {
        let c = parse("").unwrap();
        assert_eq!(c.controller, ControllerConfig::default());
        assert!(c.bindings.is_empty());
    }

    #[test]
    fn syntax_error_reports_line_and_column() {
        let err = parse("log_level = \"info\"\n[volume]\nstep = = 1\n").unwrap_err();
        match err {
            ConfigError::Syntax { line, column, .. } => {
                assert_eq!(line, 3);
                assert!(column > 1);
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn unknown_keys_are_errors() {
        let err = parse("[volume]\nsteps = 0.02\n").unwrap_err();
        assert!(err.to_string().contains("steps"), "{err}");
    }

    #[test]
    fn binding_validation() {
        let text = r#"
[[bindings]]
keys = "F13"
action = "set_volume"
value = 150

[[bindings]]
keys = "F14"
action = "select_channel"
channel = "chatRender"

[[bindings]]
keys = "F15"
action = "toggle_mute"
step = 0.1

[[bindings]]
keys = "F16"
action = "set_output_device"
device = "Headphones"
scope = "everything"

[[bindings]]
keys = "f13"
action = "next_channel"

[[bindings]]
keys = "Hyper+F1"
action = "next_channel"

[[bindings]]
keys = "F20"
action = "dance"
"#;
        let ConfigError::Invalid(errs) = parse(text).unwrap_err() else {
            panic!()
        };
        let all = errs.join("\n");
        for needle in [
            "value 150",
            "unknown channel `chatRender`",
            "`step` doesn't apply",
            "scope `everything`",
            "F13 is already bound by binding #1",
            "unknown key `Hyper`",
            "unknown action `dance`",
        ] {
            assert!(all.contains(needle), "missing `{needle}` in:\n{all}");
        }
    }

    #[test]
    fn full_binding_options() {
        let text = r#"
[[bindings]]
keys = "Ctrl+Alt+H"
action = "set_output_device"
device = "Headphones"
scope = "all"

[[bindings]]
keys = "F20"
action = "set_volume"
value = 35

[[bindings]]
keys = "F21"
action = "volume_up"
step = 0.05

[[bindings]]
keys = "F22"
action = "select_channel"
channel = "mic"
"#;
        let c = parse(text).unwrap();
        assert_eq!(
            c.bindings[0].action,
            Action::SetOutputDevice {
                pattern: "Headphones".into(),
                scope: Scope::All
            }
        );
        assert_eq!(c.bindings[1].action, Action::SetVolume(0.35));
        assert_eq!(c.bindings[2].action, Action::VolumeUp(Some(0.05)));
        assert_eq!(c.bindings[3].action, Action::SelectChannel(Channel::Mic));
    }

    #[test]
    fn settings_validation() {
        let err = parse(
            "[volume]\nstep = 0\nclassic_write = \"magic\"\n[channels]\ncycle = []\n\
             [overlay]\nposition = \"middle\"\nopacity = 2.0\n",
        )
        .unwrap_err();
        let s = err.to_string();
        for needle in [
            "step 0",
            "classic_write",
            "at least one channel",
            "position",
            "opacity",
        ] {
            assert!(s.contains(needle), "missing `{needle}` in {s}");
        }
        let c = parse("[volume]\nclassic_write = \"rest\"\n").unwrap();
        assert!(!c.controller.use_endpoint_volume);
    }

    #[test]
    fn load_or_create_writes_default() {
        let dir = std::env::temp_dir().join(format!("st-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = std::fs::remove_dir_all(&dir);
        let c = load_or_create(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG_TOML);
        assert_eq!(c, Config::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_state_round_trip_and_fallback() {
        let dir = std::env::temp_dir().join(format!("st-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(UiState::load(&path), (Channel::Game, Mix::Monitoring));
        UiState::new(Channel::Media, Mix::Streaming).save(&path);
        assert_eq!(UiState::load(&path), (Channel::Media, Mix::Streaming));
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(UiState::load(&path), (Channel::Game, Mix::Monitoring));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
