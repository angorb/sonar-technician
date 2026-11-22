//! Sonar API access: discovery, HTTP client, routes and models.
//!
//! The rest of the app talks to Sonar only through [`SonarApi`], so the
//! controller can be tested against a fake.

pub mod client;
pub mod discovery;
pub mod models;
pub mod routes;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

pub use discovery::DiscoveryError;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Classic,
    Stream,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Classic => "Classic",
            Mode::Stream => "Streamer",
        }
    }
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "classic" => Ok(Mode::Classic),
            "stream" | "streamer" => Ok(Mode::Stream),
            _ => Err(format!("unknown mode `{s}` (expected classic or stream)")),
        }
    }
}

/// A Sonar channel, named the way the user thinks of it. `routes.rs` maps it
/// to the token each endpoint needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Channel {
    Master,
    Game,
    Chat,
    Media,
    Aux,
    Mic,
}

impl Channel {
    pub const ALL: [Channel; 6] = [
        Channel::Master,
        Channel::Game,
        Channel::Chat,
        Channel::Media,
        Channel::Aux,
        Channel::Mic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Channel::Master => "master",
            Channel::Game => "game",
            Channel::Chat => "chat",
            Channel::Media => "media",
            Channel::Aux => "aux",
            Channel::Mic => "mic",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Channel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Channel::ALL
            .into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| {
                format!("unknown channel `{s}` (expected master, game, chat, media, aux or mic)")
            })
    }
}

/// Streamer-mode mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mix {
    Monitoring,
    Streaming,
}

impl Mix {
    pub const ALL: [Mix; 2] = [Mix::Monitoring, Mix::Streaming];

    pub fn name(self) -> &'static str {
        match self {
            Mix::Monitoring => "monitoring",
            Mix::Streaming => "streaming",
        }
    }
}

impl FromStr for Mix {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Mix::ALL
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown mix `{s}` (expected monitoring or streaming)"))
    }
}

/// What a volume/mute write addresses. The variant encodes the mode, so a
/// write can't silently go to the wrong mode's sliders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Classic(Channel),
    Stream(Mix, Channel),
}

impl Target {
    #[expect(dead_code, reason = "used by the controller in Phase 2")]
    pub fn mode(self) -> Mode {
        match self {
            Target::Classic(_) => Mode::Classic,
            Target::Stream(..) => Mode::Stream,
        }
    }

    pub fn channel(self) -> Channel {
        match self {
            Target::Classic(c) | Target::Stream(_, c) => c,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Classic(c) => write!(f, "{c}"),
            Target::Stream(m, c) => write!(f, "{}/{c}", m.name()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelState {
    /// 0.0..=1.0, as stored by Sonar (not rounded).
    pub volume: f64,
    pub muted: bool,
}

/// Volumes for every target of one mode.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Volumes(pub BTreeMap<Target, ChannelState>);

impl Volumes {
    pub fn get(&self, t: Target) -> Option<ChannelState> {
        self.0.get(&t).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFlow {
    Render,
    Capture,
}

/// An audio device as reported by `/audioDevices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub flow: DataFlow,
    /// For Sonar virtual devices: the volume token (`game`, `chatRender`, ...).
    /// For real devices: `"none"`.
    pub role: String,
    pub is_virtual: bool,
    pub active: bool,
}

/// Where Sonar sends a Classic channel, or a Streamer mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    pub classic: BTreeMap<Channel, String>,
    pub stream: BTreeMap<StreamOutput, StreamRoute>,
}

/// A Streamer-mode redirection: one device per mix, plus the mic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StreamOutput {
    Mix(Mix),
    Mic,
}

impl StreamOutput {
    pub fn name(self) -> &'static str {
        match self {
            StreamOutput::Mix(m) => m.name(),
            StreamOutput::Mic => "mic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRoute {
    pub device_id: String,
    /// Channels included in this mix (empty for the mic).
    pub enabled: Vec<Channel>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SonarError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    #[error("{method} {url}: {message}")]
    Transport {
        method: &'static str,
        url: String,
        message: String,
    },

    #[error("{method} {url} returned HTTP {status}: {excerpt}")]
    Status {
        method: &'static str,
        url: String,
        status: u16,
        excerpt: String,
    },

    /// Sonar said "Cannot be called in current mode". The mode cache has been
    /// refreshed; `current` is what Sonar is in now.
    #[error("{url}: not available in {} mode", .current.label())]
    WrongMode { url: String, current: Mode },

    #[error("unexpected response from {url} ({message}): {excerpt}")]
    Parse {
        url: String,
        message: String,
        excerpt: String,
    },

    #[error("invalid volume {0} (must be 0.0..=1.0)")]
    InvalidVolume(f64),

    #[error("{0}")]
    Unsupported(&'static str),
}

pub type SonarResult<T> = Result<T, SonarError>;

// ---------------------------------------------------------------------------
// The API
// ---------------------------------------------------------------------------

/// Everything the app needs from Sonar. Owned by the worker thread.
pub trait SonarApi {
    /// Current mode (may be cached for a short time).
    fn mode(&mut self) -> SonarResult<Mode>;
    /// Drop any cached mode so the next `mode()` asks Sonar.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by the controller in Phase 2")
    )]
    fn invalidate_mode(&mut self);
    fn set_mode(&mut self, mode: Mode) -> SonarResult<()>;

    /// Volumes for `mode`. Only meaningful when Sonar is in that mode: the
    /// other mode's endpoint returns placeholder values.
    fn volumes(&mut self, mode: Mode) -> SonarResult<Volumes>;
    /// `volume` must be finite and in 0..=1; it's rounded to 2 decimals.
    fn set_volume(&mut self, target: Target, volume: f64) -> SonarResult<()>;
    fn set_mute(&mut self, target: Target, muted: bool) -> SonarResult<()>;

    fn devices(&mut self) -> SonarResult<Vec<AudioDevice>>;
    fn routing(&mut self) -> SonarResult<Routing>;
    /// Classic: route one channel (not master) to a device.
    fn set_classic_device(&mut self, channel: Channel, device_id: &str) -> SonarResult<()>;
    /// Streamer: route a mix or the mic to a device.
    fn set_stream_device(&mut self, output: StreamOutput, device_id: &str) -> SonarResult<()>;
}

/// Round to 2 decimals, rejecting NaN/inf and out-of-range values.
pub fn round_volume(v: f64) -> SonarResult<f64> {
    if !v.is_finite() || !(0.0..=1.0).contains(&v) {
        return Err(SonarError::InvalidVolume(v));
    }
    Ok((v * 100.0).round() / 100.0)
}

/// Sonar's own virtual device for a channel (used for the Core Audio
/// endpoint-volume path). Matched by `role`, which holds the volume token.
/// `None` for master, which has no virtual device.
pub fn virtual_device_for(devices: &[AudioDevice], channel: Channel) -> Option<&AudioDevice> {
    let flow = if channel == Channel::Mic {
        DataFlow::Capture
    } else {
        DataFlow::Render
    };
    virtual_device_for_flow(devices, channel, flow)
}

/// Like [`virtual_device_for`] with an explicit flow. Sonar exposes the
/// Microphone virtual device as both a render and a capture endpoint.
pub fn virtual_device_for_flow(
    devices: &[AudioDevice],
    channel: Channel,
    flow: DataFlow,
) -> Option<&AudioDevice> {
    if channel == Channel::Master {
        return None;
    }
    let token = routes::volume_token(channel);
    devices
        .iter()
        .find(|d| d.is_virtual && d.flow == flow && d.role == token)
}

/// Pick a real (non-virtual, active) device of `flow` by name: an exact
/// case-insensitive match wins, otherwise a unique substring match.
pub fn match_device<'a>(
    devices: &'a [AudioDevice],
    flow: DataFlow,
    pattern: &str,
) -> Result<&'a AudioDevice, String> {
    let candidates: Vec<&AudioDevice> = devices
        .iter()
        .filter(|d| !d.is_virtual && d.active && d.flow == flow)
        .collect();
    let needle = pattern.to_lowercase();
    if let Some(d) = candidates.iter().find(|d| d.name.to_lowercase() == needle) {
        return Ok(d);
    }
    let hits: Vec<&AudioDevice> = candidates
        .iter()
        .copied()
        .filter(|d| d.name.to_lowercase().contains(&needle))
        .collect();
    let list = |ds: &[&AudioDevice]| {
        ds.iter()
            .map(|d| format!("  - {}", d.name))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "no device matches `{pattern}`. Candidates:\n{}",
            list(&candidates)
        )),
        many => Err(format!(
            "`{pattern}` matches {} devices; be more specific:\n{}",
            many.len(),
            list(many)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_volume_rules() {
        assert_eq!(round_volume(0.101858).unwrap(), 0.10);
        assert_eq!(round_volume(0.71249974).unwrap(), 0.71);
        assert_eq!(round_volume(0.005).unwrap(), 0.01);
        assert_eq!(round_volume(1.0).unwrap(), 1.0);
        assert!(round_volume(f64::NAN).is_err());
        assert!(round_volume(f64::INFINITY).is_err());
        assert!(round_volume(1.01).is_err());
        assert!(round_volume(-0.01).is_err());
    }

    #[test]
    fn parse_names() {
        assert_eq!("Game".parse::<Channel>().unwrap(), Channel::Game);
        assert!("chatRender".parse::<Channel>().is_err());
        assert_eq!("streamer".parse::<Mode>().unwrap(), Mode::Stream);
        assert_eq!("MONITORING".parse::<Mix>().unwrap(), Mix::Monitoring);
    }

    fn dev(name: &str, flow: DataFlow, is_virtual: bool) -> AudioDevice {
        AudioDevice {
            id: name.to_owned(),
            name: name.to_owned(),
            flow,
            role: "none".to_owned(),
            is_virtual,
            active: true,
        }
    }

    #[test]
    fn virtual_devices_from_sample() {
        let ds = models::parse_devices(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/api-samples/classic/audioDevices.json"
        )))
        .unwrap();
        let name = |c| virtual_device_for(&ds, c).map(|d| d.name.as_str());
        assert!(name(Channel::Game).unwrap().contains("Sonar - Gaming"));
        assert!(name(Channel::Chat).unwrap().contains("Sonar - Chat"));
        assert!(name(Channel::Media).unwrap().contains("Sonar - Media"));
        assert!(name(Channel::Aux).unwrap().contains("Sonar - Aux"));
        let mic = virtual_device_for(&ds, Channel::Mic).unwrap();
        assert!(mic.name.contains("Sonar - Microphone"));
        assert_eq!(mic.flow, DataFlow::Capture);
        assert!(virtual_device_for(&ds, Channel::Master).is_none());
    }

    #[test]
    fn device_matching() {
        let ds = vec![
            dev(
                "Headphones (SteelSeries Arctis Nova 5)",
                DataFlow::Render,
                false,
            ),
            dev("Speakers (Realtek(R) Audio)", DataFlow::Render, false),
            dev(
                "Speakers (Steam Streaming Speakers)",
                DataFlow::Render,
                false,
            ),
            dev("SteelSeries Sonar - Gaming", DataFlow::Render, true),
            dev(
                "Headset Microphone (SteelSeries Arctis Nova 5)",
                DataFlow::Capture,
                false,
            ),
        ];
        assert_eq!(
            match_device(&ds, DataFlow::Render, "headphones")
                .unwrap()
                .name,
            "Headphones (SteelSeries Arctis Nova 5)"
        );
        assert_eq!(
            match_device(&ds, DataFlow::Render, "realtek").unwrap().name,
            "Speakers (Realtek(R) Audio)"
        );
        let err = match_device(&ds, DataFlow::Render, "speakers").unwrap_err();
        assert!(err.contains("matches 2 devices"), "{err}");
        assert!(match_device(&ds, DataFlow::Render, "gaming").is_err());
        assert_eq!(
            match_device(&ds, DataFlow::Capture, "arctis").unwrap().flow,
            DataFlow::Capture
        );
    }
}
