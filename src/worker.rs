//! Executes controller [`Command`]s against Sonar. Runs on the Sonar worker
//! thread (wired up in Phase 3); `execute` is synchronous so tests can drive
//! the controller end to end against a fake API.

use crate::controller::{Command, DeviceSlot, Event, Via, WriteError};
use crate::sonar::{SonarApi, SonarResult, round_volume};

/// The Windows endpoint-volume path (FR-3.7), behind a trait so tests can fake it.
pub trait EndpointWriter {
    fn set_volume(&mut self, device_id: &str, volume: f32) -> Result<(), String>;
    fn set_mute(&mut self, device_id: &str, muted: bool) -> Result<(), String>;
}

#[cfg(windows)]
impl EndpointWriter for crate::platform::endpoint_volume::EndpointVolume {
    fn set_volume(&mut self, device_id: &str, volume: f32) -> Result<(), String> {
        crate::platform::endpoint_volume::EndpointVolume::set_volume(self, device_id, volume)
            .map_err(|e| e.to_string())
    }

    fn set_mute(&mut self, device_id: &str, muted: bool) -> Result<(), String> {
        crate::platform::endpoint_volume::EndpointVolume::set_mute(self, device_id, muted)
            .map_err(|e| e.to_string())
    }
}

fn devices(
    api: &mut dyn SonarApi,
) -> SonarResult<(Vec<crate::sonar::AudioDevice>, crate::sonar::Routing)> {
    Ok((api.devices()?, api.routing()?))
}

pub fn execute(
    api: &mut dyn SonarApi,
    endpoint: Option<&mut dyn EndpointWriter>,
    cmd: Command,
) -> Event {
    match cmd {
        Command::ReadMode => {
            api.invalidate_mode();
            Event::Mode(api.mode())
        }
        Command::ReadVolumes {
            mode,
            requested_at,
            purpose,
        } => Event::Volumes {
            mode,
            requested_at,
            purpose,
            result: api.volumes(mode),
        },
        Command::ReadDevices => Event::Devices(devices(api)),
        Command::SetVolume {
            target,
            value,
            via,
            generation,
        } => {
            let via_endpoint = matches!(via, Via::Endpoint { .. });
            let result = match via {
                Via::Rest => api.set_volume(target, value).map_err(WriteError::Sonar),
                Via::Endpoint { device_id } => match (endpoint, round_volume(value)) {
                    (Some(ep), Ok(v)) => ep
                        .set_volume(&device_id, v as f32)
                        .map_err(WriteError::Endpoint),
                    (None, _) => Err(WriteError::Endpoint("not available".to_owned())),
                    (_, Err(e)) => Err(WriteError::Sonar(e)),
                },
            };
            Event::VolumeWritten {
                target,
                value,
                via_endpoint,
                generation,
                result,
            }
        }
        Command::SetMute {
            target,
            muted,
            via,
            generation,
        } => {
            let via_endpoint = matches!(via, Via::Endpoint { .. });
            let result = match via {
                Via::Rest => api.set_mute(target, muted).map_err(WriteError::Sonar),
                Via::Endpoint { device_id } => match endpoint {
                    Some(ep) => ep.set_mute(&device_id, muted).map_err(WriteError::Endpoint),
                    None => Err(WriteError::Endpoint("not available".to_owned())),
                },
            };
            Event::MuteWritten {
                target,
                muted,
                via_endpoint,
                generation,
                result,
            }
        }
        Command::SetDevice {
            slot,
            device_id,
            generation,
        } => {
            let result = match slot {
                DeviceSlot::Classic(c) => api.set_classic_device(c, &device_id),
                DeviceSlot::Stream(o) => api.set_stream_device(o, &device_id),
            };
            Event::DeviceSet {
                slot,
                device_id,
                generation,
                result,
            }
        }
        Command::SetMode(m) => Event::ModeSet(api.set_mode(m)),
    }
}
