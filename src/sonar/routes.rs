//! Every Sonar/GG route string lives here and nowhere else.
//!
//! When GG changes a route, it should be a one-line fix in this file. The
//! `*_KEYWORD`/`*_CANDIDATES` constants below are what `try-writes` tests;
//! once a candidate is confirmed, set the matching constant and update the
//! table in CLAUDE.md.

use super::{Channel, Mix, Mode, StreamOutput, Target};

/// GG's own API (HTTPS, self-signed): lists the sub-apps, including Sonar's web server address.
pub const GG_SUB_APPS: &str = "/subApps";

/// Error text Sonar returns when a route doesn't apply to the current mode.
pub const WRONG_MODE_TEXT: &str = "Cannot be called in current mode";

pub const MODE: &str = "/mode/";
pub const AUDIO_DEVICES: &str = "/audioDevices";
pub const CLASSIC_REDIRECTIONS: &str = "/classicRedirections";
pub const STREAM_REDIRECTIONS: &str = "/streamRedirections";

// --- Write-route keywords (⚠️ until confirmed by `try-writes`) ---------------

/// Classic volume keyword: `/volumeSettings/classic/{ch}/<this>/{v}`.
pub const CLASSIC_VOLUME_KEYWORD: &str = "Volume";
pub const CLASSIC_VOLUME_CANDIDATES: &[&str] = &["Volume", "volume"];
/// Classic mute keyword: `/volumeSettings/classic/{ch}/<this>/{bool}`.
pub const CLASSIC_MUTE_KEYWORD: &str = "Mute";
pub const CLASSIC_MUTE_CANDIDATES: &[&str] = &["Mute", "mute", "isMuted"];
/// Streamer volume keyword: `/volumeSettings/streamer/{mix}/{ch}/<this>/{v}`.
pub const STREAM_VOLUME_KEYWORD: &str = "volume";
pub const STREAM_VOLUME_CANDIDATES: &[&str] = &["volume", "Volume"];
/// Streamer mute keyword: `/volumeSettings/streamer/{mix}/{ch}/<this>/{bool}`.
pub const STREAM_MUTE_KEYWORD: &str = "isMuted";
pub const STREAM_MUTE_CANDIDATES: &[&str] = &["isMuted", "Mute", "mute"];
/// Path token for the master fader.
pub const MASTER_TOKEN: &str = "master";
pub const MASTER_TOKEN_CANDIDATES: &[&str] = &["master", "Master"];
/// Whether device IDs (`{0.0.0.00000000}.{guid}`) are percent-encoded in paths.
pub const ENCODE_DEVICE_ID: bool = true;

// --- Tokens ----------------------------------------------------------------

pub fn mode_token(mode: Mode) -> &'static str {
    match mode {
        Mode::Classic => "classic",
        Mode::Stream => "stream",
    }
}

pub fn parse_mode_token(s: &str) -> Option<Mode> {
    match s {
        "classic" => Some(Mode::Classic),
        "stream" => Some(Mode::Stream),
        _ => None,
    }
}

/// Token used by volume routes and by `role` fields in responses.
pub fn volume_token(channel: Channel) -> &'static str {
    match channel {
        Channel::Master => MASTER_TOKEN,
        Channel::Game => "game",
        Channel::Chat => "chatRender",
        Channel::Media => "media",
        Channel::Aux => "aux",
        Channel::Mic => "chatCapture",
    }
}

/// Inverse of [`volume_token`] for non-master channels (as found in
/// `devices.<token>`, `role`, `status[].role`).
pub fn channel_from_volume_token(token: &str) -> Option<Channel> {
    match token {
        "game" => Some(Channel::Game),
        "chatRender" => Some(Channel::Chat),
        "media" => Some(Channel::Media),
        "aux" => Some(Channel::Aux),
        "chatCapture" => Some(Channel::Mic),
        _ => None,
    }
}

/// Token used by `/classicRedirections`. Master has no redirection.
pub fn redirection_token(channel: Channel) -> Option<&'static str> {
    match channel {
        Channel::Master => None,
        Channel::Game => Some("game"),
        Channel::Chat => Some("chat"),
        Channel::Media => Some("media"),
        Channel::Aux => Some("aux"),
        Channel::Mic => Some("mic"),
    }
}

pub fn channel_from_redirection_token(token: &str) -> Option<Channel> {
    Channel::ALL
        .into_iter()
        .find(|c| redirection_token(*c) == Some(token))
}

pub fn stream_output_from_token(token: &str) -> Option<StreamOutput> {
    match token {
        "monitoring" => Some(StreamOutput::Mix(Mix::Monitoring)),
        "streaming" => Some(StreamOutput::Mix(Mix::Streaming)),
        "mic" => Some(StreamOutput::Mic),
        _ => None,
    }
}

// --- Paths -----------------------------------------------------------------

pub fn set_mode(mode: Mode) -> String {
    format!("/mode/{}", mode_token(mode))
}

pub fn volume_settings(mode: Mode) -> &'static str {
    match mode {
        Mode::Classic => "/volumeSettings/classic",
        Mode::Stream => "/volumeSettings/streamer",
    }
}

/// Format an already-rounded volume for a path (`0.42`, `1.00`).
pub fn format_volume(rounded: f64) -> String {
    format!("{rounded:.2}")
}

/// Build a volume/mute write path from explicit pieces (used by `try-writes`).
pub fn volume_write(target: Target, channel_token: &str, keyword: &str, value: &str) -> String {
    match target {
        Target::Classic(_) => {
            format!("/volumeSettings/classic/{channel_token}/{keyword}/{value}")
        }
        Target::Stream(mix, _) => format!(
            "/volumeSettings/streamer/{}/{channel_token}/{keyword}/{value}",
            mix.name()
        ),
    }
}

/// `rounded` must already be validated and rounded to 2 decimals.
pub fn set_volume(target: Target, rounded: f64) -> String {
    let keyword = match target {
        Target::Classic(_) => CLASSIC_VOLUME_KEYWORD,
        Target::Stream(..) => STREAM_VOLUME_KEYWORD,
    };
    volume_write(
        target,
        volume_token(target.channel()),
        keyword,
        &format_volume(rounded),
    )
}

pub fn set_mute(target: Target, muted: bool) -> String {
    let keyword = match target {
        Target::Classic(_) => CLASSIC_MUTE_KEYWORD,
        Target::Stream(..) => STREAM_MUTE_KEYWORD,
    };
    volume_write(
        target,
        volume_token(target.channel()),
        keyword,
        if muted { "true" } else { "false" },
    )
}

/// Path segment for a device ID, raw or percent-encoded.
pub fn device_id_segment(id: &str, encode: bool) -> String {
    if !encode {
        return id.to_owned();
    }
    let mut out = String::with_capacity(id.len() * 3);
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `None` for master, which has no redirection.
pub fn set_classic_device_with(channel: Channel, device_id: &str, encode: bool) -> Option<String> {
    redirection_token(channel).map(|tok| {
        format!(
            "/classicRedirections/{tok}/deviceId/{}",
            device_id_segment(device_id, encode)
        )
    })
}

pub fn set_classic_device(channel: Channel, device_id: &str) -> Option<String> {
    set_classic_device_with(channel, device_id, ENCODE_DEVICE_ID)
}

/// Streamer device switch. Unknown until Q4 is resolved (traffic capture).
pub fn set_stream_device(_output: StreamOutput, _device_id: &str) -> Option<String> {
    None
}

// --- Probe -----------------------------------------------------------------

/// A read-only GET endpoint that `probe` dumps.
#[derive(Debug, Clone, Copy)]
pub struct ProbeEndpoint {
    /// File-name stem for the dump (`<name>.json`, `<name>.meta.txt`).
    pub name: &'static str,
    /// Path relative to Sonar's base URL.
    pub path: &'static str,
}

/// Sonar GET endpoints that `probe` dumps. All read-only.
pub const PROBE_ENDPOINTS: &[ProbeEndpoint] = &[
    ProbeEndpoint {
        name: "mode",
        path: MODE,
    },
    ProbeEndpoint {
        name: "volumeSettings-classic",
        path: "/volumeSettings/classic",
    },
    ProbeEndpoint {
        name: "volumeSettings-streamer",
        path: "/volumeSettings/streamer",
    },
    ProbeEndpoint {
        name: "audioDevices",
        path: AUDIO_DEVICES,
    },
    ProbeEndpoint {
        name: "classicRedirections",
        path: CLASSIC_REDIRECTIONS,
    },
    ProbeEndpoint {
        name: "streamRedirections",
        path: STREAM_REDIRECTIONS,
    },
    ProbeEndpoint {
        name: "streamRedirections-isStreamMonitoringEnabled",
        path: "/streamRedirections/isStreamMonitoringEnabled",
    },
    ProbeEndpoint {
        name: "configs",
        path: "/configs",
    },
    ProbeEndpoint {
        name: "configs-selected",
        path: "/configs/selected",
    },
    ProbeEndpoint {
        name: "chatMix",
        path: "/chatMix",
    },
    ProbeEndpoint {
        name: "v1-chatMix",
        path: "/v1/chatMix",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_paths() {
        assert_eq!(
            set_volume(Target::Classic(Channel::Game), 0.42),
            "/volumeSettings/classic/game/Volume/0.42"
        );
        assert_eq!(
            set_volume(Target::Classic(Channel::Master), 1.0),
            "/volumeSettings/classic/master/Volume/1.00"
        );
        assert_eq!(
            set_volume(Target::Stream(Mix::Streaming, Channel::Chat), 0.05),
            "/volumeSettings/streamer/streaming/chatRender/volume/0.05"
        );
        assert_eq!(
            set_mute(Target::Classic(Channel::Mic), true),
            "/volumeSettings/classic/chatCapture/Mute/true"
        );
        assert_eq!(
            set_mute(Target::Stream(Mix::Monitoring, Channel::Aux), false),
            "/volumeSettings/streamer/monitoring/aux/isMuted/false"
        );
    }

    #[test]
    fn token_round_trips() {
        for c in Channel::ALL {
            if c != Channel::Master {
                assert_eq!(channel_from_volume_token(volume_token(c)), Some(c));
            }
            if let Some(t) = redirection_token(c) {
                assert_eq!(channel_from_redirection_token(t), Some(c));
            }
        }
        assert_eq!(redirection_token(Channel::Master), None);
    }

    #[test]
    fn device_paths() {
        let id = "{0.0.0.00000000}.{1ea774ba-44fc-437f-8575-6b0b4f87b4c4}";
        assert_eq!(
            set_classic_device_with(Channel::Chat, id, true).unwrap(),
            "/classicRedirections/chat/deviceId/%7B0.0.0.00000000%7D.%7B1ea774ba-44fc-437f-8575-6b0b4f87b4c4%7D"
        );
        assert_eq!(
            set_classic_device_with(Channel::Mic, id, false).unwrap(),
            format!("/classicRedirections/mic/deviceId/{id}")
        );
        assert!(set_classic_device(Channel::Master, id).is_none());
    }
}
