//! Serde types for Sonar responses, built from `docs/api-samples/*` (GG 124.0.0),
//! plus conversion into the domain types in `sonar/mod.rs`.
//!
//! Unknown fields are ignored so new GG fields don't break parsing.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

use super::routes;
use super::{
    AudioDevice, Channel, ChannelState, DataFlow, Mix, Mode, StreamOutput, StreamRoute, Target,
    Volumes,
};

/// Parse failures carry a short message; the client adds URL and body excerpt.
pub type ParseResult<T> = Result<T, String>;

fn json<'a, T: Deserialize<'a>>(raw: &'a str) -> ParseResult<T> {
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

// --- /mode/ ------------------------------------------------------------------

pub fn parse_mode(raw: &str) -> ParseResult<Mode> {
    let s: String = json(raw)?;
    routes::parse_mode_token(&s).ok_or_else(|| format!("unknown mode `{s}`"))
}

// --- /volumeSettings/{classic,streamer} -------------------------------------

#[derive(Debug, Deserialize)]
struct VolumeSettingsRaw {
    masters: SliderRaw,
    devices: HashMap<String, SliderRaw>,
}

/// `{"classic": {volume, muted} | {}, "stream": {"streaming": ..., "monitoring": ...} | {}}`
#[derive(Debug, Deserialize)]
struct SliderRaw {
    #[serde(default)]
    classic: VolMuteRaw,
    #[serde(default)]
    stream: StreamSliderRaw,
}

#[derive(Debug, Default, Deserialize)]
struct StreamSliderRaw {
    streaming: Option<VolMuteRaw>,
    monitoring: Option<VolMuteRaw>,
}

#[derive(Debug, Default, Deserialize)]
struct VolMuteRaw {
    volume: Option<f64>,
    muted: Option<bool>,
}

impl VolMuteRaw {
    fn state(&self, what: &str) -> ParseResult<ChannelState> {
        match (self.volume, self.muted) {
            (Some(volume), Some(muted)) => Ok(ChannelState {
                volume: volume.clamp(0.0, 1.0),
                muted,
            }),
            _ => Err(format!("missing volume/muted for {what}")),
        }
    }
}

/// Parse `/volumeSettings/classic` (mode = Classic) or `/volumeSettings/streamer`
/// (mode = Stream). Only the half matching `mode` is read; the other half holds
/// placeholders.
pub fn parse_volumes(raw: &str, mode: Mode) -> ParseResult<Volumes> {
    let v: VolumeSettingsRaw = json(raw)?;
    let mut out = BTreeMap::new();
    let mut add = |channel: Channel, slider: &SliderRaw| -> ParseResult<()> {
        match mode {
            Mode::Classic => {
                out.insert(
                    Target::Classic(channel),
                    slider.classic.state(channel.name())?,
                );
            }
            Mode::Stream => {
                for mix in Mix::ALL {
                    let part = match mix {
                        Mix::Streaming => &slider.stream.streaming,
                        Mix::Monitoring => &slider.stream.monitoring,
                    };
                    let what = format!("{}/{}", mix.name(), channel.name());
                    let part = part.as_ref().ok_or_else(|| format!("missing {what}"))?;
                    out.insert(Target::Stream(mix, channel), part.state(&what)?);
                }
            }
        }
        Ok(())
    };
    add(Channel::Master, &v.masters)?;
    for (token, slider) in &v.devices {
        // Unknown tokens (a future GG channel) are skipped, not fatal.
        if let Some(channel) = routes::channel_from_volume_token(token) {
            add(channel, slider)?;
        }
    }
    Ok(Volumes(out))
}

// --- /audioDevices -------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioDeviceRaw {
    id: String,
    friendly_name: String,
    data_flow: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    is_vad: bool,
}

pub fn parse_devices(raw: &str) -> ParseResult<Vec<AudioDevice>> {
    let list: Vec<AudioDeviceRaw> = json(raw)?;
    Ok(list
        .into_iter()
        .filter_map(|d| {
            let flow = match d.data_flow.as_str() {
                "render" => DataFlow::Render,
                "capture" => DataFlow::Capture,
                _ => return None,
            };
            Some(AudioDevice {
                id: d.id,
                name: d.friendly_name,
                flow,
                role: d.role,
                is_virtual: d.is_vad,
                active: d.state == "active",
            })
        })
        .collect())
}

// --- /classicRedirections, /streamRedirections -----------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClassicRedirectionRaw {
    id: String,
    device_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamRedirectionRaw {
    stream_redirection_id: String,
    device_id: String,
    #[serde(default)]
    status: Vec<StreamStatusRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamStatusRaw {
    role: String,
    is_enabled: bool,
}

pub fn parse_classic_routing(raw: &str) -> ParseResult<BTreeMap<Channel, String>> {
    let list: Vec<ClassicRedirectionRaw> = json(raw)?;
    Ok(list
        .into_iter()
        .filter_map(|r| routes::channel_from_redirection_token(&r.id).map(|c| (c, r.device_id)))
        .collect())
}

pub fn parse_stream_routing(raw: &str) -> ParseResult<BTreeMap<StreamOutput, StreamRoute>> {
    let list: Vec<StreamRedirectionRaw> = json(raw)?;
    Ok(list
        .into_iter()
        .filter_map(|r| {
            let out = routes::stream_output_from_token(&r.stream_redirection_id)?;
            let enabled = r
                .status
                .iter()
                .filter(|s| s.is_enabled)
                .filter_map(|s| routes::channel_from_volume_token(&s.role))
                .collect();
            Some((
                out,
                StreamRoute {
                    device_id: r.device_id,
                    enabled,
                },
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sonar::Routing;

    macro_rules! sample {
        ($mode:literal, $name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/docs/api-samples/",
                $mode,
                "/",
                $name,
                ".json"
            ))
        };
    }

    #[test]
    fn mode_samples() {
        assert_eq!(
            parse_mode(sample!("classic", "mode")).unwrap(),
            Mode::Classic
        );
        assert_eq!(parse_mode(sample!("stream", "mode")).unwrap(), Mode::Stream);
        assert!(parse_mode("\"party\"").is_err());
    }

    #[test]
    fn classic_volumes_sample() {
        let v = parse_volumes(sample!("classic", "volumeSettings-classic"), Mode::Classic).unwrap();
        assert_eq!(v.0.len(), 6);
        let game = v.get(Target::Classic(Channel::Game)).unwrap();
        assert!((game.volume - 0.101858).abs() < 1e-9);
        assert!(!game.muted);
        let master = v.get(Target::Classic(Channel::Master)).unwrap();
        assert!((master.volume - 0.71249974).abs() < 1e-9);
        assert_eq!(v.get(Target::Classic(Channel::Mic)).unwrap().volume, 1.0);
    }

    #[test]
    fn stream_volumes_sample() {
        let v = parse_volumes(sample!("stream", "volumeSettings-streamer"), Mode::Stream).unwrap();
        assert_eq!(v.0.len(), 12);
        let mon_master = v
            .get(Target::Stream(Mix::Monitoring, Channel::Master))
            .unwrap();
        assert!((mon_master.volume - 0.05).abs() < 1e-6);
        let str_mic = v.get(Target::Stream(Mix::Streaming, Channel::Mic)).unwrap();
        assert_eq!(str_mic.volume, 0.0);
    }

    #[test]
    fn classic_endpoint_has_no_stream_half() {
        // /volumeSettings/classic has `"stream": {}`, so it can't be read as Stream.
        assert!(parse_volumes(sample!("classic", "volumeSettings-classic"), Mode::Stream).is_err());
    }

    #[test]
    fn devices_sample() {
        let d = parse_devices(sample!("classic", "audioDevices")).unwrap();
        assert_eq!(d.len(), 16);
        let real_outputs: Vec<_> = d
            .iter()
            .filter(|d| !d.is_virtual && d.flow == DataFlow::Render)
            .map(|d| d.name.as_str())
            .collect();
        assert!(real_outputs.contains(&"Headphones (SteelSeries Arctis Nova 5)"));
        assert!(real_outputs.contains(&"Speakers (Realtek(R) Audio)"));
        assert!(
            !real_outputs
                .iter()
                .any(|n| n.starts_with("SteelSeries Sonar"))
        );
        let gaming_vad = d
            .iter()
            .find(|d| d.is_virtual && d.role == "game" && d.flow == DataFlow::Render)
            .unwrap();
        assert!(gaming_vad.name.contains("Gaming"));
    }

    #[test]
    fn routing_samples() {
        let r = Routing {
            classic: parse_classic_routing(sample!("classic", "classicRedirections")).unwrap(),
            stream: parse_stream_routing(sample!("classic", "streamRedirections")).unwrap(),
        };
        assert_eq!(r.classic.len(), 5);
        assert_eq!(
            r.classic[&Channel::Mic],
            "{0.0.1.00000000}.{d9c877e3-a447-4c5c-81ca-ff069a1935c5}"
        );
        let streaming = &r.stream[&StreamOutput::Mix(Mix::Streaming)];
        assert_eq!(streaming.enabled, vec![Channel::Chat, Channel::Aux]);
        assert!(r.stream[&StreamOutput::Mic].enabled.is_empty());
    }
}
