//! What a hotkey or tray item asks the controller to do.

use crate::sonar::Channel;

/// Which channels a device action applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// The active channel (Classic) or active mix / mic (Streamer).
    #[default]
    Active,
    /// Classic: game, chat, media and aux together. Streamer: the monitoring
    /// mix (what you hear), i.e. the "headphones <-> speakers" switch.
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Step the active target up. `None` uses the configured step.
    VolumeUp(Option<f64>),
    VolumeDown(Option<f64>),
    /// Absolute volume, 0.0..=1.0.
    SetVolume(f64),
    ToggleMute,
    NextChannel,
    PrevChannel,
    SelectChannel(Channel),
    /// Streamer only: monitoring <-> streaming.
    ToggleMix,
    NextOutputDevice(Scope),
    PrevOutputDevice(Scope),
    /// Case-insensitive name substring, resolved at runtime.
    SetOutputDevice {
        pattern: String,
        scope: Scope,
    },
    /// Classic <-> Streamer. Disruptive; the bindings gate it.
    ToggleMode,
}
