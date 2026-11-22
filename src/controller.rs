//! Pure state machine: mode, active (mix, channel), volumes, devices, coalescing.
//!
//! No Win32 or HTTP here. The controller takes [`Action`]s (hotkeys, tray),
//! [`Event`]s (results from the Sonar worker) and the current time, and returns
//! [`Effect`]s: [`Command`]s for the worker and [`Snapshot`]s for the overlay/tray.
//!
//! Key rules (see CLAUDE.md):
//! - The overlay shows the *local* value at once; Sonar catches up.
//! - Latest-wins writes: per target at most one write in flight; newer values
//!   wait in a single pending slot and only the newest is sent.
//! - Sonar is the source of truth: volumes are re-read on startup, on channel
//!   switch, after idle and periodically, but a read never overwrites a local
//!   change made after the read was requested.
//! - Every failure degrades only the action that failed.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::actions::{Action, Scope};
use crate::sonar::{
    AudioDevice, Channel, ChannelState, DataFlow, Mix, Mode, Routing, SonarError, StreamOutput,
    Target, Volumes, match_device, virtual_device_for,
};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Acceleration {
    pub enabled: bool,
    /// Steps in the same direction within this window count towards `threshold`.
    pub window: Duration,
    pub threshold: usize,
    pub multiplier: f64,
}

impl Default for Acceleration {
    fn default() -> Self {
        Self {
            enabled: false,
            window: Duration::from_millis(80),
            threshold: 4,
            multiplier: 3.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControllerConfig {
    /// Volume step (0.01 = 1%).
    pub step: f64,
    pub acceleration: Acceleration,
    /// Channels `next_channel`/`prev_channel` rotate through, in order.
    pub channel_cycle: Vec<Channel>,
    /// Device-name substrings to cycle through; empty = every real device.
    pub device_cycle: Vec<String>,
    /// Classic channels (not master) via the Windows endpoint volume of
    /// Sonar's virtual device, so the GG UI follows (FR-3.7).
    pub use_endpoint_volume: bool,
    pub mode_poll: Duration,
    pub volume_poll: Duration,
    pub idle_resync: Duration,
    /// When to re-read and repair volumes after a device switch.
    pub verify_after_switch: Vec<Duration>,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            step: 0.01,
            acceleration: Acceleration::default(),
            channel_cycle: vec![
                Channel::Master,
                Channel::Game,
                Channel::Chat,
                Channel::Media,
                Channel::Aux,
            ],
            device_cycle: Vec::new(),
            use_endpoint_volume: true,
            mode_poll: Duration::from_secs(2),
            volume_poll: Duration::from_secs(5),
            idle_resync: Duration::from_secs(3),
            verify_after_switch: vec![Duration::from_millis(400), Duration::from_millis(1500)],
            reconnect_min: Duration::from_secs(2),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands, events, effects
// ---------------------------------------------------------------------------

/// How a volume/mute write reaches Sonar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    Rest,
    /// Windows endpoint volume of this Sonar virtual device.
    Endpoint {
        device_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPurpose {
    /// Adopt Sonar's values (unless a local change is newer).
    Sync,
    /// After a device switch: Sonar may have dropped writes; re-send local
    /// values that don't match.
    Verify,
}

/// A device redirection: a Classic channel or a Streamer mix/mic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceSlot {
    Classic(Channel),
    Stream(StreamOutput),
}

/// Work for the Sonar worker. `gen` lets the controller ignore results of
/// writes issued before a mode change or reconnect.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    ReadMode,
    ReadVolumes {
        mode: Mode,
        requested_at: Instant,
        purpose: ReadPurpose,
    },
    /// `/audioDevices` plus both redirection lists.
    ReadDevices,
    SetVolume {
        target: Target,
        value: f64,
        via: Via,
        generation: u64,
    },
    SetMute {
        target: Target,
        muted: bool,
        via: Via,
        generation: u64,
    },
    SetDevice {
        slot: DeviceSlot,
        device_id: String,
        generation: u64,
    },
    SetMode(Mode),
}

#[derive(Debug)]
pub enum WriteError {
    Sonar(SonarError),
    /// The Windows endpoint-volume path failed.
    Endpoint(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Sonar(e) => write!(f, "{e}"),
            WriteError::Endpoint(e) => write!(f, "Windows volume: {e}"),
        }
    }
}

/// Result of a [`Command`], sent back by the worker.
#[derive(Debug)]
pub enum Event {
    Mode(Result<Mode, SonarError>),
    Volumes {
        mode: Mode,
        requested_at: Instant,
        purpose: ReadPurpose,
        result: Result<Volumes, SonarError>,
    },
    Devices(Result<(Vec<AudioDevice>, Routing), SonarError>),
    VolumeWritten {
        target: Target,
        value: f64,
        via_endpoint: bool,
        generation: u64,
        result: Result<(), WriteError>,
    },
    MuteWritten {
        target: Target,
        muted: bool,
        via_endpoint: bool,
        generation: u64,
        result: Result<(), WriteError>,
    },
    DeviceSet {
        slot: DeviceSlot,
        device_id: String,
        generation: u64,
        result: Result<(), SonarError>,
    },
    ModeSet(Result<(), SonarError>),
}

/// What the overlay and tray show.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub connected: bool,
    pub mode: Option<Mode>,
    pub channel: Channel,
    /// Only meaningful in Streamer mode.
    pub mix: Mix,
    pub volume: Option<f64>,
    pub muted: Option<bool>,
    /// Output device of the active channel/mix, when known.
    pub device: Option<String>,
    /// Status or error text for this update.
    pub message: Option<String>,
}

impl Snapshot {
    /// Tray tooltip text (Windows truncates at 127 chars).
    pub fn tooltip(&self) -> String {
        let mut t = String::from("Sonar Technician");
        if !self.connected {
            t.push_str("\nSonar unavailable");
            return t;
        }
        if let Some(mode) = self.mode {
            t.push('\n');
            t.push_str(mode.label());
            t.push_str(" · ");
            if mode == Mode::Stream {
                t.push_str(self.mix.label());
                t.push(' ');
            }
            t.push_str(self.channel.label());
            if let Some(v) = self.volume {
                t.push_str(&format!(" {:.0}%", v * 100.0));
            }
            if self.muted == Some(true) {
                t.push_str(" (muted)");
            }
        }
        if let Some(d) = &self.device {
            t.push_str("\n→ ");
            t.push_str(d);
        }
        t
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Sonar(Command),
    /// `show_overlay`: the user did something (or something they should see
    /// happened); otherwise just refresh the tray tooltip.
    Ui {
        snapshot: Snapshot,
        show_overlay: bool,
    },
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Latest-wins write slot: one in flight, at most one waiting.
#[derive(Debug, Clone, Default)]
struct Slot<T> {
    in_flight: Option<T>,
    pending: Option<T>,
}

impl<T> Slot<T> {
    fn busy(&self) -> bool {
        self.in_flight.is_some() || self.pending.is_some()
    }

    /// Returns the value to send now, if nothing is in flight.
    fn offer(&mut self, v: T) -> Option<T>
    where
        T: Clone,
    {
        if self.in_flight.is_some() {
            self.pending = Some(v);
            None
        } else {
            self.in_flight = Some(v.clone());
            Some(v)
        }
    }

    /// The in-flight write finished; returns the next value to send.
    fn complete(&mut self) -> Option<T>
    where
        T: Clone,
    {
        self.in_flight = None;
        let next = self.pending.take()?;
        self.in_flight = Some(next.clone());
        Some(next)
    }
}

#[derive(Debug, Default)]
struct TargetState {
    local: Option<ChannelState>,
    last_change: Option<Instant>,
    volume: Slot<f64>,
    mute: Slot<bool>,
}

impl TargetState {
    fn busy(&self) -> bool {
        self.volume.busy() || self.mute.busy()
    }
}

pub struct Controller {
    cfg: ControllerConfig,
    connected: bool,
    mode: Option<Mode>,
    channel: Channel,
    mix: Mix,
    /// Bumped on mode change / disconnect; stale write results are ignored.
    generation: u64,
    targets: BTreeMap<Target, TargetState>,
    devices: Vec<AudioDevice>,
    routing: Option<Routing>,
    device_slots: BTreeMap<DeviceSlot, Slot<String>>,
    /// Cleared if the Windows endpoint path fails; REST is used instead.
    endpoint_ok: bool,

    mode_in_flight: bool,
    last_mode_req: Option<Instant>,
    volumes_in_flight: bool,
    last_volumes_req: Option<Instant>,
    devices_in_flight: bool,
    next_reconnect: Option<Instant>,
    reconnect_backoff: Duration,
    last_input: Option<Instant>,
    idle_resync_done: bool,
    verify_at: Vec<Instant>,
    recent_steps: VecDeque<(Instant, i8)>,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn is_connection_error(e: &SonarError) -> bool {
    matches!(e, SonarError::Transport { .. } | SonarError::Discovery(_))
}

impl Controller {
    pub fn new(cfg: ControllerConfig, channel: Channel, mix: Mix) -> Self {
        let reconnect_backoff = cfg.reconnect_min;
        Self {
            cfg,
            connected: false,
            mode: None,
            channel,
            mix,
            generation: 0,
            targets: BTreeMap::new(),
            devices: Vec::new(),
            routing: None,
            device_slots: BTreeMap::new(),
            endpoint_ok: true,
            mode_in_flight: false,
            last_mode_req: None,
            volumes_in_flight: false,
            last_volumes_req: None,
            devices_in_flight: false,
            next_reconnect: None,
            reconnect_backoff,
            last_input: None,
            idle_resync_done: true,
            verify_at: Vec::new(),
            recent_steps: VecDeque::new(),
        }
    }

    pub fn set_config(&mut self, cfg: ControllerConfig) {
        self.cfg = cfg;
    }

    /// The active (channel, mix), for persisting across restarts.
    pub fn active(&self) -> (Channel, Mix) {
        (self.channel, self.mix)
    }

    /// First commands after launch.
    pub fn start(&mut self, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.read_mode(now, &mut fx);
        fx
    }

    pub fn snapshot(&self, message: Option<String>) -> Snapshot {
        let target = self.active_target();
        let local = target
            .and_then(|t| self.targets.get(&t))
            .and_then(|s| s.local);
        Snapshot {
            connected: self.connected,
            mode: self.mode,
            channel: self.channel,
            mix: self.mix,
            volume: local.map(|s| s.volume),
            muted: local.map(|s| s.muted),
            device: self.active_device_name(),
            message,
        }
    }

    fn active_target(&self) -> Option<Target> {
        Some(match self.mode? {
            Mode::Classic => Target::Classic(self.channel),
            Mode::Stream => Target::Stream(self.mix, self.channel),
        })
    }

    fn ui(&self, fx: &mut Vec<Effect>, show_overlay: bool, message: Option<String>) {
        fx.push(Effect::Ui {
            snapshot: self.snapshot(message),
            show_overlay,
        });
    }

    // --- reads ---------------------------------------------------------------

    fn read_mode(&mut self, now: Instant, fx: &mut Vec<Effect>) {
        if !self.mode_in_flight {
            self.mode_in_flight = true;
            self.last_mode_req = Some(now);
            fx.push(Effect::Sonar(Command::ReadMode));
        }
    }

    /// Returns false if a volume read is already in flight.
    fn read_volumes(&mut self, now: Instant, purpose: ReadPurpose, fx: &mut Vec<Effect>) -> bool {
        let Some(mode) = self.mode else { return false };
        if self.volumes_in_flight {
            return false;
        }
        self.volumes_in_flight = true;
        self.last_volumes_req = Some(now);
        fx.push(Effect::Sonar(Command::ReadVolumes {
            mode,
            requested_at: now,
            purpose,
        }));
        true
    }

    fn read_devices(&mut self, fx: &mut Vec<Effect>) {
        if !self.devices_in_flight {
            self.devices_in_flight = true;
            fx.push(Effect::Sonar(Command::ReadDevices));
        }
    }

    // --- writes ----------------------------------------------------------------

    fn via_for(&self, target: Target) -> Via {
        if let Target::Classic(ch) = target
            && self.cfg.use_endpoint_volume
            && self.endpoint_ok
            && let Some(d) = virtual_device_for(&self.devices, ch)
        {
            return Via::Endpoint {
                device_id: d.id.clone(),
            };
        }
        Via::Rest
    }

    fn send_volume(&self, target: Target, value: f64, fx: &mut Vec<Effect>) {
        fx.push(Effect::Sonar(Command::SetVolume {
            target,
            value,
            via: self.via_for(target),
            generation: self.generation,
        }));
    }

    fn send_mute(&self, target: Target, muted: bool, fx: &mut Vec<Effect>) {
        fx.push(Effect::Sonar(Command::SetMute {
            target,
            muted,
            via: self.via_for(target),
            generation: self.generation,
        }));
    }

    fn queue_volume(&mut self, target: Target, value: f64, fx: &mut Vec<Effect>) {
        let st = self.targets.entry(target).or_default();
        if let Some(v) = st.volume.offer(value) {
            self.send_volume(target, v, fx);
        }
    }

    fn queue_mute(&mut self, target: Target, muted: bool, fx: &mut Vec<Effect>) {
        let st = self.targets.entry(target).or_default();
        if let Some(m) = st.mute.offer(muted) {
            self.send_mute(target, m, fx);
        }
    }

    fn queue_device(&mut self, slot: DeviceSlot, device_id: String, fx: &mut Vec<Effect>) {
        let s = self.device_slots.entry(slot).or_default();
        if let Some(id) = s.offer(device_id) {
            fx.push(Effect::Sonar(Command::SetDevice {
                slot,
                device_id: id,
                generation: self.generation,
            }));
        }
    }

    // --- connection / mode -------------------------------------------------------

    /// Forget everything tied to the old connection or mode.
    fn reset_state(&mut self) {
        self.generation += 1;
        self.targets.clear();
        self.device_slots.clear();
        self.verify_at.clear();
    }

    fn disconnect(&mut self, now: Instant, why: &SonarError, fx: &mut Vec<Effect>) {
        if self.connected {
            tracing::warn!("Sonar unavailable: {why}");
            self.connected = false;
            self.reset_state();
            self.reconnect_backoff = self.cfg.reconnect_min;
            self.ui(fx, true, Some("Sonar unavailable".to_owned()));
        } else {
            tracing::debug!("Sonar still unavailable: {why}");
            // Tray tooltip only; don't pop the overlay on every retry.
            self.ui(fx, false, Some("Sonar unavailable".to_owned()));
        }
        // Wait the current backoff, then double it for next time (2 s .. 30 s).
        self.next_reconnect = Some(now + self.reconnect_backoff);
        self.reconnect_backoff = (self.reconnect_backoff * 2).min(self.cfg.reconnect_max);
    }

    fn on_mode(&mut self, now: Instant, mode: Mode, fx: &mut Vec<Effect>) {
        let reconnected = !self.connected;
        if reconnected {
            tracing::info!("connected to Sonar ({} mode)", mode.label());
            self.connected = true;
            self.next_reconnect = None;
            self.reconnect_backoff = self.cfg.reconnect_min;
        }
        if self.mode == Some(mode) && !reconnected {
            return;
        }
        let changed = self.mode.is_some_and(|m| m != mode);
        if changed {
            tracing::info!("Sonar mode is now {}", mode.label());
        }
        self.mode = Some(mode);
        self.reset_state();
        self.volumes_in_flight = false;
        self.read_volumes(now, ReadPurpose::Sync, fx);
        self.read_devices(fx);
        let msg = if changed {
            Some(format!("{} mode", mode.label()))
        } else if reconnected && self.last_input.is_some() {
            Some("Sonar connected".to_owned())
        } else {
            None
        };
        let show = msg.is_some();
        self.ui(fx, show, msg);
    }

    // --- actions ------------------------------------------------------------------

    pub fn handle_action(&mut self, now: Instant, action: Action) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.last_input = Some(now);
        self.idle_resync_done = false;
        if !self.connected || self.mode.is_none() {
            self.ui(&mut fx, true, Some("Sonar unavailable".to_owned()));
            return fx;
        }
        match action {
            Action::VolumeUp(step) => self.step(now, 1, step, &mut fx),
            Action::VolumeDown(step) => self.step(now, -1, step, &mut fx),
            Action::SetVolume(v) => self.set_volume(now, v, &mut fx),
            Action::ToggleMute => self.toggle_mute(now, &mut fx),
            Action::NextChannel => self.cycle_channel(now, 1, &mut fx),
            Action::PrevChannel => self.cycle_channel(now, -1, &mut fx),
            Action::SelectChannel(c) => self.select_channel(now, c, &mut fx),
            Action::ToggleMix => self.toggle_mix(now, &mut fx),
            Action::NextOutputDevice(scope) => self.cycle_device(now, scope, 1, &mut fx),
            Action::PrevOutputDevice(scope) => self.cycle_device(now, scope, -1, &mut fx),
            Action::SetOutputDevice { pattern, scope } => {
                self.set_device_by_name(now, scope, &pattern, &mut fx)
            }
            Action::ToggleMode => {
                let to = match self.mode {
                    Some(Mode::Classic) => Mode::Stream,
                    _ => Mode::Classic,
                };
                fx.push(Effect::Sonar(Command::SetMode(to)));
                self.ui(&mut fx, true, Some(format!("Switching to {}…", to.label())));
            }
        }
        fx
    }

    /// Local state of the active target, or a "reading" hint + a read.
    fn active_local(
        &mut self,
        now: Instant,
        fx: &mut Vec<Effect>,
    ) -> Option<(Target, ChannelState)> {
        let t = self.active_target()?;
        match self.targets.get(&t).and_then(|s| s.local) {
            Some(s) => Some((t, s)),
            None => {
                self.read_volumes(now, ReadPurpose::Sync, fx);
                self.ui(fx, true, Some("Reading Sonar…".to_owned()));
                None
            }
        }
    }

    fn accel_multiplier(&mut self, now: Instant, dir: i8) -> f64 {
        let a = &self.cfg.acceleration;
        if self.recent_steps.back().is_some_and(|(_, d)| *d != dir) {
            self.recent_steps.clear();
        }
        while self
            .recent_steps
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > a.window)
        {
            self.recent_steps.pop_front();
        }
        self.recent_steps.push_back((now, dir));
        if a.enabled && self.recent_steps.len() >= a.threshold {
            a.multiplier
        } else {
            1.0
        }
    }

    fn step(&mut self, now: Instant, dir: i8, step: Option<f64>, fx: &mut Vec<Effect>) {
        let mult = self.accel_multiplier(now, dir);
        let Some((t, local)) = self.active_local(now, fx) else {
            return;
        };
        let step = step.unwrap_or(self.cfg.step) * mult;
        // Step from the *rounded* value so stored values like 0.101858 land on
        // whole percentages.
        let new = round2((round2(local.volume) + f64::from(dir) * step).clamp(0.0, 1.0));
        self.apply_volume(now, t, local, new, fx);
    }

    fn set_volume(&mut self, now: Instant, v: f64, fx: &mut Vec<Effect>) {
        if !v.is_finite() {
            return;
        }
        let Some((t, local)) = self.active_local(now, fx) else {
            return;
        };
        self.apply_volume(now, t, local, round2(v.clamp(0.0, 1.0)), fx);
    }

    fn apply_volume(
        &mut self,
        now: Instant,
        t: Target,
        local: ChannelState,
        new: f64,
        fx: &mut Vec<Effect>,
    ) {
        if (new - local.volume).abs() > f64::EPSILON {
            let st = self.targets.entry(t).or_default();
            st.local = Some(ChannelState {
                volume: new,
                ..local
            });
            st.last_change = Some(now);
            self.queue_volume(t, new, fx);
        }
        self.ui(fx, true, None);
    }

    fn toggle_mute(&mut self, now: Instant, fx: &mut Vec<Effect>) {
        let Some((t, local)) = self.active_local(now, fx) else {
            return;
        };
        let muted = !local.muted;
        let st = self.targets.entry(t).or_default();
        st.local = Some(ChannelState { muted, ..local });
        st.last_change = Some(now);
        self.queue_mute(t, muted, fx);
        self.ui(fx, true, None);
    }

    fn cycle_channel(&mut self, now: Instant, dir: i32, fx: &mut Vec<Effect>) {
        let cycle = &self.cfg.channel_cycle;
        if cycle.is_empty() {
            return;
        }
        let next = match cycle.iter().position(|c| *c == self.channel) {
            Some(i) => {
                let n = cycle.len() as i32;
                cycle[((i as i32 + dir).rem_euclid(n)) as usize]
            }
            None => cycle[0],
        };
        self.select_channel(now, next, fx);
    }

    fn select_channel(&mut self, now: Instant, channel: Channel, fx: &mut Vec<Effect>) {
        self.channel = channel;
        self.recent_steps.clear();
        self.read_volumes(now, ReadPurpose::Sync, fx);
        self.ui(fx, true, None);
    }

    fn toggle_mix(&mut self, now: Instant, fx: &mut Vec<Effect>) {
        if self.mode != Some(Mode::Stream) {
            self.ui(
                fx,
                true,
                Some("Mix toggle is only available in Streamer mode".to_owned()),
            );
            return;
        }
        self.mix = match self.mix {
            Mix::Monitoring => Mix::Streaming,
            Mix::Streaming => Mix::Monitoring,
        };
        self.recent_steps.clear();
        self.read_volumes(now, ReadPurpose::Sync, fx);
        self.ui(fx, true, None);
    }

    // --- devices ------------------------------------------------------------------

    /// Redirections a device action touches, and the data flow they need.
    fn device_slots_for(&self, scope: Scope) -> Option<(Vec<DeviceSlot>, DataFlow)> {
        let mode = self.mode?;
        let mic = self.channel == Channel::Mic;
        Some(match mode {
            Mode::Classic if mic => (vec![DeviceSlot::Classic(Channel::Mic)], DataFlow::Capture),
            Mode::Classic if scope == Scope::All || self.channel == Channel::Master => (
                [Channel::Game, Channel::Chat, Channel::Media, Channel::Aux]
                    .into_iter()
                    .map(DeviceSlot::Classic)
                    .collect(),
                DataFlow::Render,
            ),
            Mode::Classic => (vec![DeviceSlot::Classic(self.channel)], DataFlow::Render),
            Mode::Stream if mic => (
                vec![DeviceSlot::Stream(StreamOutput::Mic)],
                DataFlow::Capture,
            ),
            Mode::Stream if scope == Scope::All => (
                vec![DeviceSlot::Stream(StreamOutput::Mix(Mix::Monitoring))],
                DataFlow::Render,
            ),
            Mode::Stream => (
                vec![DeviceSlot::Stream(StreamOutput::Mix(self.mix))],
                DataFlow::Render,
            ),
        })
    }

    fn routed_device(&self, slot: DeviceSlot) -> Option<&str> {
        let r = self.routing.as_ref()?;
        match slot {
            DeviceSlot::Classic(c) => r.classic.get(&c).map(String::as_str),
            DeviceSlot::Stream(o) => r.stream.get(&o).map(|s| s.device_id.as_str()),
        }
    }

    fn set_routed_device(&mut self, slot: DeviceSlot, id: &str) {
        let Some(r) = self.routing.as_mut() else {
            return;
        };
        match slot {
            DeviceSlot::Classic(c) => {
                r.classic.insert(c, id.to_owned());
            }
            DeviceSlot::Stream(o) => {
                if let Some(s) = r.stream.get_mut(&o) {
                    s.device_id = id.to_owned();
                }
            }
        }
    }

    fn active_device_name(&self) -> Option<String> {
        let (slots, _) = self.device_slots_for(Scope::Active)?;
        let id = self.routed_device(*slots.first()?)?;
        self.devices
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
    }

    /// Devices the active channel/mix can be routed to (for the tray menu):
    /// every real active device of the right direction, with the current one
    /// flagged. Empty until devices have been read.
    pub fn device_choices(&self) -> Vec<(String, bool)> {
        let Some((slots, flow)) = self.device_slots_for(Scope::Active) else {
            return Vec::new();
        };
        let current = slots.first().and_then(|s| self.routed_device(*s));
        self.devices
            .iter()
            .filter(|d| !d.is_virtual && d.active && d.flow == flow)
            .map(|d| (d.name.clone(), Some(d.id.as_str()) == current))
            .collect()
    }

    /// Devices to cycle through, in order.
    fn device_cycle(&self, flow: DataFlow) -> Vec<&AudioDevice> {
        if self.cfg.device_cycle.is_empty() {
            return self
                .devices
                .iter()
                .filter(|d| !d.is_virtual && d.active && d.flow == flow)
                .collect();
        }
        let mut out: Vec<&AudioDevice> = Vec::new();
        for pat in &self.cfg.device_cycle {
            match match_device(&self.devices, flow, pat) {
                Ok(d) if !out.iter().any(|o| o.id == d.id) => out.push(d),
                Ok(_) => {}
                Err(e) => tracing::debug!("device cycle entry `{pat}` skipped: {e}"),
            }
        }
        out
    }

    fn devices_ready(&mut self, fx: &mut Vec<Effect>) -> bool {
        if self.routing.is_some() && !self.devices.is_empty() {
            return true;
        }
        self.read_devices(fx);
        self.ui(fx, true, Some("Reading devices…".to_owned()));
        false
    }

    fn cycle_device(&mut self, now: Instant, scope: Scope, dir: i32, fx: &mut Vec<Effect>) {
        if !self.devices_ready(fx) {
            return;
        }
        let Some((slots, flow)) = self.device_slots_for(scope) else {
            return;
        };
        let cycle = self.device_cycle(flow);
        if cycle.is_empty() {
            self.ui(fx, true, Some("No devices to switch to".to_owned()));
            return;
        }
        let current = slots.first().and_then(|s| self.routed_device(*s));
        let next = match current.and_then(|id| cycle.iter().position(|d| d.id == id)) {
            Some(i) => {
                let n = cycle.len() as i32;
                cycle[((i as i32 + dir).rem_euclid(n)) as usize]
            }
            None => cycle[0],
        };
        let (id, name) = (next.id.clone(), next.name.clone());
        self.switch_device(now, &slots, id, &name, fx);
    }

    fn set_device_by_name(
        &mut self,
        now: Instant,
        scope: Scope,
        pattern: &str,
        fx: &mut Vec<Effect>,
    ) {
        if !self.devices_ready(fx) {
            return;
        }
        let Some((slots, flow)) = self.device_slots_for(scope) else {
            return;
        };
        match match_device(&self.devices, flow, pattern) {
            Ok(d) => {
                let (id, name) = (d.id.clone(), d.name.clone());
                self.switch_device(now, &slots, id, &name, fx);
            }
            Err(e) => {
                tracing::warn!("set_output_device: {e}");
                self.ui(
                    fx,
                    true,
                    Some(format!("No unique device matches \"{pattern}\"")),
                );
            }
        }
    }

    fn switch_device(
        &mut self,
        _now: Instant,
        slots: &[DeviceSlot],
        id: String,
        name: &str,
        fx: &mut Vec<Effect>,
    ) {
        for slot in slots {
            if self.routed_device(*slot) == Some(id.as_str()) {
                continue;
            }
            // Optimistic, so the next press continues from here.
            self.set_routed_device(*slot, &id);
            self.queue_device(*slot, id.clone(), fx);
        }
        self.ui(fx, true, Some(format!("→ {name}")));
    }

    // --- events -------------------------------------------------------------------

    pub fn handle_event(&mut self, now: Instant, event: Event) -> Vec<Effect> {
        let mut fx = Vec::new();
        match event {
            Event::Mode(r) => {
                self.mode_in_flight = false;
                match r {
                    Ok(m) => self.on_mode(now, m, &mut fx),
                    Err(e) if is_connection_error(&e) => self.disconnect(now, &e, &mut fx),
                    Err(e) => tracing::warn!("reading mode failed: {e}"),
                }
            }
            Event::Volumes {
                mode,
                requested_at,
                purpose,
                result,
            } => {
                self.volumes_in_flight = false;
                match result {
                    Ok(v) if Some(mode) == self.mode => {
                        self.apply_read(requested_at, purpose, v, &mut fx)
                    }
                    Ok(_) => {} // mode changed meanwhile
                    Err(e) if is_connection_error(&e) => self.disconnect(now, &e, &mut fx),
                    Err(e) => tracing::warn!("reading volumes failed: {e}"),
                }
            }
            Event::Devices(r) => {
                self.devices_in_flight = false;
                match r {
                    Ok((devices, routing)) => {
                        self.devices = devices;
                        // Keep optimistic routing for slots with writes pending.
                        let keep: Vec<(DeviceSlot, String)> = self
                            .device_slots
                            .iter()
                            .filter(|(_, s)| s.busy())
                            .filter_map(|(slot, _)| {
                                self.routed_device(*slot).map(|id| (*slot, id.to_owned()))
                            })
                            .collect();
                        self.routing = Some(routing);
                        for (slot, id) in keep {
                            self.set_routed_device(slot, &id);
                        }
                        self.ui(&mut fx, false, None);
                    }
                    Err(e) if is_connection_error(&e) => self.disconnect(now, &e, &mut fx),
                    Err(e) => tracing::warn!("reading devices failed: {e}"),
                }
            }
            Event::VolumeWritten {
                target,
                value,
                via_endpoint,
                generation,
                result,
            } => {
                if generation == self.generation {
                    self.on_volume_written(now, target, value, via_endpoint, result, &mut fx);
                }
            }
            Event::MuteWritten {
                target,
                muted,
                via_endpoint,
                generation,
                result,
            } => {
                if generation == self.generation {
                    self.on_mute_written(now, target, muted, via_endpoint, result, &mut fx);
                }
            }
            Event::DeviceSet {
                slot,
                device_id,
                generation,
                result,
            } => {
                if generation == self.generation {
                    self.on_device_set(now, slot, device_id, result, &mut fx);
                }
            }
            Event::ModeSet(r) => match r {
                Ok(()) => {
                    self.mode_in_flight = false;
                    self.read_mode(now, &mut fx);
                }
                Err(e) if is_connection_error(&e) => self.disconnect(now, &e, &mut fx),
                Err(e) => self.ui(&mut fx, true, Some(format!("Mode switch failed: {e}"))),
            },
        }
        fx
    }

    fn apply_read(
        &mut self,
        requested_at: Instant,
        purpose: ReadPurpose,
        v: Volumes,
        fx: &mut Vec<Effect>,
    ) {
        let active = self.active_target();
        let before = active
            .and_then(|t| self.targets.get(&t))
            .and_then(|s| s.local);
        let mut resend = Vec::new();
        for (t, read) in v.0 {
            let st = self.targets.entry(t).or_default();
            let newer_local = st.last_change.is_some_and(|c| c >= requested_at);
            if st.busy() || newer_local {
                continue;
            }
            match (purpose, st.local) {
                (ReadPurpose::Verify, Some(local)) => {
                    if (local.volume - read.volume).abs() > 0.005 {
                        resend.push((t, Some(local.volume), None));
                    }
                    if local.muted != read.muted {
                        resend.push((t, None, Some(local.muted)));
                    }
                }
                _ => st.local = Some(read),
            }
        }
        for (t, vol, mute) in resend {
            tracing::info!("re-applying {t} after device switch");
            if let Some(v) = vol {
                self.queue_volume(t, v, fx);
            }
            if let Some(m) = mute {
                self.queue_mute(t, m, fx);
            }
        }
        let after = active
            .and_then(|t| self.targets.get(&t))
            .and_then(|s| s.local);
        if before != after {
            self.ui(fx, false, None);
        }
    }

    fn on_volume_written(
        &mut self,
        now: Instant,
        target: Target,
        value: f64,
        via_endpoint: bool,
        result: Result<(), WriteError>,
        fx: &mut Vec<Effect>,
    ) {
        let Some(st) = self.targets.get_mut(&target) else {
            return;
        };
        let next = st.volume.complete();
        match result {
            Ok(()) => {}
            Err(WriteError::Endpoint(e)) if via_endpoint => {
                tracing::warn!(
                    "Windows volume path failed for {target} ({e}); using REST from now on"
                );
                self.endpoint_ok = false;
                if next.is_none() {
                    // Re-send this value via REST.
                    if let Some(st) = self.targets.get_mut(&target) {
                        st.volume.in_flight = Some(value);
                    }
                    self.send_volume(target, value, fx);
                    return;
                }
            }
            Err(WriteError::Sonar(e)) => {
                if self.write_failed(now, target, &e, "Volume", fx) {
                    return;
                }
            }
            Err(e) => tracing::warn!("volume write for {target} failed: {e}"),
        }
        if let Some(v) = next {
            self.send_volume(target, v, fx);
        }
    }

    fn on_mute_written(
        &mut self,
        now: Instant,
        target: Target,
        muted: bool,
        via_endpoint: bool,
        result: Result<(), WriteError>,
        fx: &mut Vec<Effect>,
    ) {
        let Some(st) = self.targets.get_mut(&target) else {
            return;
        };
        let next = st.mute.complete();
        match result {
            Ok(()) => {}
            Err(WriteError::Endpoint(e)) if via_endpoint => {
                tracing::warn!(
                    "Windows mute path failed for {target} ({e}); using REST from now on"
                );
                self.endpoint_ok = false;
                if next.is_none() {
                    if let Some(st) = self.targets.get_mut(&target) {
                        st.mute.in_flight = Some(muted);
                    }
                    self.send_mute(target, muted, fx);
                    return;
                }
            }
            Err(WriteError::Sonar(e)) => {
                if self.write_failed(now, target, &e, "Mute", fx) {
                    return;
                }
            }
            Err(e) => tracing::warn!("mute write for {target} failed: {e}"),
        }
        if let Some(m) = next {
            self.send_mute(target, m, fx);
        }
    }

    /// Handle a failed Sonar write. Returns true if state was reset (stop).
    fn write_failed(
        &mut self,
        now: Instant,
        target: Target,
        e: &SonarError,
        what: &str,
        fx: &mut Vec<Effect>,
    ) -> bool {
        match e {
            SonarError::WrongMode { current, .. } => {
                self.on_mode(now, *current, fx);
                true
            }
            e if is_connection_error(e) => {
                self.disconnect(now, e, fx);
                true
            }
            e => {
                tracing::warn!("{what} write for {target} failed: {e}");
                if let Some(st) = self.targets.get_mut(&target) {
                    // Let the next read restore Sonar's real value.
                    st.last_change = None;
                }
                self.read_volumes(now, ReadPurpose::Sync, fx);
                self.ui(fx, true, Some(format!("{what} change failed")));
                false
            }
        }
    }

    fn on_device_set(
        &mut self,
        now: Instant,
        slot: DeviceSlot,
        device_id: String,
        result: Result<(), SonarError>,
        fx: &mut Vec<Effect>,
    ) {
        let next = self.device_slots.entry(slot).or_default().complete();
        match result {
            Ok(()) => {
                tracing::info!("switched {slot:?} to {device_id}");
                for d in &self.cfg.verify_after_switch {
                    self.verify_at.push(now + *d);
                }
            }
            Err(SonarError::WrongMode { current, .. }) => {
                self.on_mode(now, current, fx);
                return;
            }
            Err(e) if is_connection_error(&e) => {
                self.disconnect(now, &e, fx);
                return;
            }
            Err(e) => {
                tracing::warn!("device switch for {slot:?} failed: {e}");
                self.read_devices(fx);
                self.ui(fx, true, Some("Device switch failed".to_owned()));
            }
        }
        if let Some(id) = next {
            fx.push(Effect::Sonar(Command::SetDevice {
                slot,
                device_id: id,
                generation: self.generation,
            }));
        }
    }

    // --- timers -------------------------------------------------------------------

    /// When `tick` next has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        if !self.connected {
            return self.next_reconnect;
        }
        let mut d: Vec<Instant> = Vec::new();
        if let Some(t) = self.last_mode_req {
            d.push(t + self.cfg.mode_poll);
        }
        if let Some(t) = self.last_volumes_req {
            d.push(t + self.cfg.volume_poll);
        }
        if !self.idle_resync_done
            && let Some(t) = self.last_input
        {
            d.push(t + self.cfg.idle_resync);
        }
        d.extend(self.verify_at.iter().copied());
        d.into_iter().min()
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();
        if !self.connected {
            if self.next_reconnect.is_some_and(|t| t <= now) {
                self.next_reconnect = None;
                self.read_mode(now, &mut fx);
            }
            return fx;
        }
        if self
            .last_mode_req
            .is_none_or(|t| now.duration_since(t) >= self.cfg.mode_poll)
        {
            self.read_mode(now, &mut fx);
        }
        if self.verify_at.iter().any(|t| *t <= now)
            && self.read_volumes(now, ReadPurpose::Verify, &mut fx)
        {
            self.verify_at.retain(|t| *t > now);
        }
        if !self.idle_resync_done
            && self
                .last_input
                .is_some_and(|t| now.duration_since(t) >= self.cfg.idle_resync)
            && !self.targets.values().any(TargetState::busy)
            && self.read_volumes(now, ReadPurpose::Sync, &mut fx)
        {
            self.idle_resync_done = true;
        }
        if self
            .last_volumes_req
            .is_none_or(|t| now.duration_since(t) >= self.cfg.volume_poll)
            && !self.targets.values().any(TargetState::busy)
        {
            self.read_volumes(now, ReadPurpose::Sync, &mut fx);
        }
        fx
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
