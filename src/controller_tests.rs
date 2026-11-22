//! Controller tests: the real controller + `worker::execute`, against a fake
//! Sonar (`World`) shared by a fake `SonarApi` and a fake `EndpointWriter`.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::*;
use crate::sonar::{SonarApi, SonarResult, models, routes};
use crate::worker::{EndpointWriter, execute};

// ---------------------------------------------------------------------------
// Fake Sonar
// ---------------------------------------------------------------------------

struct World {
    up: bool,
    mode: Mode,
    vols: BTreeMap<Target, ChannelState>,
    devices: Vec<AudioDevice>,
    routing: Routing,
    /// Every write, as "<via> <target> <value>".
    log: Vec<String>,
    /// Simulates Sonar dropping a volume during a device switch.
    reset_on_switch: Option<(Target, f64)>,
    fail_endpoint: bool,
}

type W = Rc<RefCell<World>>;

fn transport() -> SonarError {
    SonarError::Transport {
        method: "GET",
        url: "http://127.0.0.1:1/".into(),
        message: "connection refused".into(),
    }
}

fn st(volume: f64) -> ChannelState {
    ChannelState {
        volume,
        muted: false,
    }
}

impl World {
    fn new() -> W {
        let mut vols = BTreeMap::new();
        for (c, v) in [
            (Channel::Master, 0.71),
            (Channel::Game, 0.101858),
            (Channel::Chat, 0.10),
            (Channel::Media, 0.22),
            (Channel::Aux, 0.71),
            (Channel::Mic, 1.0),
        ] {
            vols.insert(Target::Classic(c), st(v));
            for m in Mix::ALL {
                vols.insert(Target::Stream(m, c), st(0.5));
            }
        }
        let sample = |name: &str| {
            std::fs::read_to_string(format!(
                "{}/docs/api-samples/stream/{name}.json",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        Rc::new(RefCell::new(World {
            up: true,
            mode: Mode::Classic,
            vols,
            devices: models::parse_devices(&sample("audioDevices")).unwrap(),
            routing: Routing {
                classic: models::parse_classic_routing(&sample("classicRedirections")).unwrap(),
                stream: models::parse_stream_routing(&sample("streamRedirections")).unwrap(),
            },
            log: Vec::new(),
            reset_on_switch: None,
            fail_endpoint: false,
        }))
    }
}

struct FakeApi(W);
struct FakeEp(W);

impl FakeApi {
    fn check(&self, target: Option<Target>) -> SonarResult<()> {
        let w = self.0.borrow();
        if !w.up {
            return Err(transport());
        }
        if let Some(t) = target
            && t.mode() != w.mode
        {
            return Err(SonarError::WrongMode {
                url: "x".into(),
                current: w.mode,
            });
        }
        Ok(())
    }

    fn switched(&self) {
        let mut w = self.0.borrow_mut();
        if let Some((t, v)) = w.reset_on_switch {
            w.vols.get_mut(&t).unwrap().volume = v;
        }
    }
}

impl SonarApi for FakeApi {
    fn mode(&mut self) -> SonarResult<Mode> {
        self.check(None)?;
        Ok(self.0.borrow().mode)
    }
    fn invalidate_mode(&mut self) {}
    fn set_mode(&mut self, mode: Mode) -> SonarResult<()> {
        self.check(None)?;
        self.0.borrow_mut().mode = mode;
        Ok(())
    }
    fn volumes(&mut self, mode: Mode) -> SonarResult<Volumes> {
        self.check(None)?;
        let w = self.0.borrow();
        Ok(Volumes(
            w.vols
                .iter()
                .filter(|(t, _)| t.mode() == mode)
                .map(|(t, s)| (*t, *s))
                .collect(),
        ))
    }
    fn set_volume(&mut self, target: Target, volume: f64) -> SonarResult<()> {
        self.check(Some(target))?;
        let v = crate::sonar::round_volume(volume)?;
        let mut w = self.0.borrow_mut();
        w.vols.get_mut(&target).unwrap().volume = v;
        w.log.push(format!("rest {target} {v:.2}"));
        Ok(())
    }
    fn set_mute(&mut self, target: Target, muted: bool) -> SonarResult<()> {
        self.check(Some(target))?;
        let mut w = self.0.borrow_mut();
        w.vols.get_mut(&target).unwrap().muted = muted;
        w.log.push(format!("rest {target} mute={muted}"));
        Ok(())
    }
    fn devices(&mut self) -> SonarResult<Vec<AudioDevice>> {
        self.check(None)?;
        Ok(self.0.borrow().devices.clone())
    }
    fn routing(&mut self) -> SonarResult<Routing> {
        self.check(None)?;
        Ok(self.0.borrow().routing.clone())
    }
    fn set_classic_device(&mut self, channel: Channel, device_id: &str) -> SonarResult<()> {
        self.check(None)?;
        {
            let mut w = self.0.borrow_mut();
            w.routing.classic.insert(channel, device_id.to_owned());
            w.log.push(format!("route {channel} {device_id}"));
        }
        self.switched();
        Ok(())
    }
    fn set_stream_device(&mut self, output: StreamOutput, device_id: &str) -> SonarResult<()> {
        self.check(None)?;
        {
            let mut w = self.0.borrow_mut();
            w.routing.stream.get_mut(&output).unwrap().device_id = device_id.to_owned();
            w.log.push(format!("route {} {device_id}", output.name()));
        }
        self.switched();
        Ok(())
    }
}

impl FakeEp {
    fn target(&self, device_id: &str) -> Result<Target, String> {
        let w = self.0.borrow();
        if w.fail_endpoint {
            return Err("E_FAIL".into());
        }
        let d = w
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("no such device")?;
        let c = routes::channel_from_volume_token(&d.role).ok_or("not a Sonar device")?;
        Ok(Target::Classic(c))
    }
}

impl EndpointWriter for FakeEp {
    fn set_volume(&mut self, device_id: &str, volume: f32) -> Result<(), String> {
        let t = self.target(device_id)?;
        let mut w = self.0.borrow_mut();
        w.vols.get_mut(&t).unwrap().volume = f64::from(volume);
        w.log.push(format!("ep {t} {volume:.2}"));
        Ok(())
    }
    fn set_mute(&mut self, device_id: &str, muted: bool) -> Result<(), String> {
        let t = self.target(device_id)?;
        let mut w = self.0.borrow_mut();
        w.vols.get_mut(&t).unwrap().muted = muted;
        w.log.push(format!("ep {t} mute={muted}"));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct H {
    c: Controller,
    w: W,
    api: FakeApi,
    ep: FakeEp,
    q: VecDeque<Command>,
    ui: Vec<(Snapshot, bool)>,
    now: Instant,
}

impl H {
    fn new(w: W) -> H {
        Self::with(w, ControllerConfig::default(), Channel::Game)
    }

    fn with(w: W, cfg: ControllerConfig, channel: Channel) -> H {
        let now = Instant::now();
        let mut h = H {
            c: Controller::new(cfg, channel, Mix::Monitoring),
            api: FakeApi(w.clone()),
            ep: FakeEp(w.clone()),
            w,
            q: VecDeque::new(),
            ui: Vec::new(),
            now,
        };
        let fx = h.c.start(now);
        h.absorb(fx);
        h.run();
        h
    }

    fn absorb(&mut self, fx: Vec<Effect>) {
        for e in fx {
            match e {
                Effect::Sonar(c) => self.q.push_back(c),
                Effect::Ui {
                    snapshot,
                    show_overlay,
                } => self.ui.push((snapshot, show_overlay)),
            }
        }
    }

    fn act(&mut self, a: Action) {
        let fx = self.c.handle_action(self.now, a);
        self.absorb(fx);
    }

    /// Execute one queued command and feed its event back.
    fn step(&mut self) -> bool {
        let Some(cmd) = self.q.pop_front() else {
            return false;
        };
        let ev = execute(&mut self.api, Some(&mut self.ep), cmd);
        let fx = self.c.handle_event(self.now, ev);
        self.absorb(fx);
        true
    }

    fn run(&mut self) {
        for _ in 0..1000 {
            if !self.step() {
                return;
            }
        }
        panic!("command loop didn't settle");
    }

    fn advance(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        let fx = self.c.tick(self.now);
        self.absorb(fx);
        self.run();
    }

    fn last(&self) -> &Snapshot {
        &self.ui.last().expect("no UI update").0
    }

    fn vol(&self, t: Target) -> f64 {
        self.w.borrow().vols[&t].volume
    }

    fn log(&self) -> Vec<String> {
        self.w.borrow().log.clone()
    }

    fn clear_log(&mut self) {
        self.w.borrow_mut().log.clear();
    }

    fn queued_writes(&self) -> usize {
        self.q
            .iter()
            .filter(|c| matches!(c, Command::SetVolume { .. }))
            .count()
    }
}

const GAME: Target = Target::Classic(Channel::Game);
const HEADPHONES: &str = "{0.0.0.00000000}.{d016b81c-e9b0-4d42-aa2b-8bfc2fbab6b4}";
const REALTEK: &str = "{0.0.0.00000000}.{1ea774ba-44fc-437f-8575-6b0b4f87b4c4}";

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn startup_reads_mode_volumes_and_devices() {
    let h = H::new(World::new());
    let s = h.last();
    assert!(s.connected);
    assert_eq!(s.mode, Some(Mode::Classic));
    assert_eq!(s.channel, Channel::Game);
    assert!(close(s.volume.unwrap(), 0.101858));
    assert_eq!(s.device.as_deref(), Some("Speakers (Realtek(R) Audio)"));
    // Startup is silent: no overlay.
    assert!(h.ui.iter().all(|(_, show)| !show));
}

#[test]
fn step_lands_on_whole_percent_and_uses_endpoint_for_classic_channels() {
    let mut h = H::new(World::new());
    h.act(Action::VolumeUp(None));
    assert!(close(h.last().volume.unwrap(), 0.11));
    assert!(h.ui.last().unwrap().1, "overlay shown");
    h.run();
    assert!(close(h.vol(GAME), 0.11));
    assert_eq!(h.log(), ["ep game 0.11"]);
}

#[test]
fn master_always_goes_through_rest() {
    let mut h = H::new(World::new());
    h.act(Action::SelectChannel(Channel::Master));
    h.run();
    h.act(Action::VolumeDown(None));
    h.run();
    assert_eq!(h.log(), ["rest master 0.70"]);
}

#[test]
fn fast_knob_spin_coalesces_latest_wins() {
    let mut h = H::new(World::new());
    h.act(Action::SetVolume(0.20));
    h.run();
    h.clear_log();
    for _ in 0..60 {
        h.act(Action::VolumeUp(None));
    }
    // Overlay is already at the final value; only one write went out.
    assert!(close(h.last().volume.unwrap(), 0.80));
    assert_eq!(h.queued_writes(), 1);
    h.run();
    assert!(close(h.vol(GAME), 0.80));
    assert_eq!(h.log(), ["ep game 0.21", "ep game 0.80"]);
}

#[test]
fn volume_clamps_at_bounds_without_extra_writes() {
    let mut h = H::new(World::new());
    h.act(Action::SetVolume(1.0));
    h.run();
    h.clear_log();
    h.act(Action::VolumeUp(None));
    h.run();
    assert!(close(h.last().volume.unwrap(), 1.0));
    assert!(h.log().is_empty());
}

#[test]
fn acceleration_multiplies_fast_steps() {
    let cfg = ControllerConfig {
        acceleration: Acceleration {
            enabled: true,
            window: Duration::from_millis(80),
            threshold: 3,
            multiplier: 3.0,
        },
        ..Default::default()
    };
    let mut h = H::with(World::new(), cfg, Channel::Game);
    for _ in 0..4 {
        h.act(Action::VolumeUp(None));
    }
    // 0.10 -> 0.11 -> 0.12 -> 0.15 -> 0.18
    assert!(close(h.last().volume.unwrap(), 0.18));
    h.run();
    h.now += Duration::from_millis(200);
    h.act(Action::VolumeUp(None));
    assert!(close(h.last().volume.unwrap(), 0.19), "slow step is 1%");
    // Changing direction resets acceleration.
    h.act(Action::VolumeDown(None));
    h.act(Action::VolumeDown(None));
    assert!(close(h.last().volume.unwrap(), 0.17));
}

#[test]
fn channel_cycle_wraps_and_rereads() {
    let mut h = H::with(World::new(), ControllerConfig::default(), Channel::Aux);
    h.act(Action::NextChannel);
    assert_eq!(h.last().channel, Channel::Master);
    assert!(
        h.q.iter().any(|c| matches!(c, Command::ReadVolumes { .. })),
        "channel switch re-reads Sonar"
    );
    h.run();
    h.act(Action::PrevChannel);
    assert_eq!(h.last().channel, Channel::Aux);
    // A channel outside the cycle continues from the start.
    h.act(Action::SelectChannel(Channel::Mic));
    h.act(Action::NextChannel);
    assert_eq!(h.last().channel, Channel::Master);
}

#[test]
fn mute_toggles_via_endpoint() {
    let mut h = H::new(World::new());
    h.act(Action::ToggleMute);
    assert_eq!(h.last().muted, Some(true));
    h.run();
    h.act(Action::ToggleMute);
    h.run();
    assert_eq!(h.log(), ["ep game mute=true", "ep game mute=false"]);
    assert!(!h.w.borrow().vols[&GAME].muted);
}

#[test]
fn endpoint_failure_falls_back_to_rest() {
    let mut h = H::new(World::new());
    h.w.borrow_mut().fail_endpoint = true;
    h.act(Action::VolumeUp(None));
    h.run();
    assert!(close(h.vol(GAME), 0.11));
    h.act(Action::VolumeUp(None));
    h.run();
    assert_eq!(h.log(), ["rest game 0.11", "rest game 0.12"]);
}

#[test]
fn toggle_mix_only_in_streamer_mode() {
    let mut h = H::new(World::new());
    h.act(Action::ToggleMix);
    assert_eq!(
        h.last().message.as_deref(),
        Some("Mix toggle is only available in Streamer mode")
    );

    let w = World::new();
    w.borrow_mut().mode = Mode::Stream;
    let mut h = H::new(w);
    assert_eq!(h.last().mix, Mix::Monitoring);
    h.act(Action::ToggleMix);
    assert_eq!(h.last().mix, Mix::Streaming);
    h.run();
    h.act(Action::VolumeUp(None));
    h.run();
    assert_eq!(h.log(), ["rest streaming/game 0.51"]);
}

#[test]
fn device_cycle_all_switches_every_output_channel() {
    let cfg = ControllerConfig {
        device_cycle: vec!["Headphones".into(), "Realtek".into()],
        ..Default::default()
    };
    let mut h = H::with(World::new(), cfg, Channel::Game);
    let mic_before = h.w.borrow().routing.classic[&Channel::Mic].clone();

    h.act(Action::NextOutputDevice(Scope::All));
    assert_eq!(
        h.last().message.as_deref(),
        Some("→ Headphones (SteelSeries Arctis Nova 5)")
    );
    h.run();
    {
        let w = h.w.borrow();
        for c in [Channel::Game, Channel::Chat, Channel::Media, Channel::Aux] {
            assert_eq!(w.routing.classic[&c], HEADPHONES, "{c}");
        }
        assert_eq!(w.routing.classic[&Channel::Mic], mic_before);
    }
    h.act(Action::NextOutputDevice(Scope::All));
    h.run();
    assert_eq!(h.w.borrow().routing.classic[&Channel::Game], REALTEK);
}

#[test]
fn device_switch_that_drops_a_volume_is_repaired() {
    let mut h = H::new(World::new());
    h.act(Action::SetVolume(0.30));
    h.run();
    h.w.borrow_mut().reset_on_switch = Some((GAME, 1.0));
    h.act(Action::SetOutputDevice {
        pattern: "headphones".into(),
        scope: Scope::Active,
    });
    h.run();
    assert!(close(h.vol(GAME), 1.0), "Sonar dropped the volume");
    h.advance(400);
    assert!(close(h.vol(GAME), 0.30), "re-applied after the switch");
    // Overlay never showed the dropped value.
    assert!(
        h.ui.iter()
            .all(|(s, _)| s.volume.is_none_or(|v| !close(v, 1.0)))
    );
}

#[test]
fn set_output_device_active_scope_only_touches_active_channel() {
    let mut h = H::new(World::new());
    h.act(Action::SetOutputDevice {
        pattern: "arctis".into(),
        scope: Scope::Active,
    });
    h.run();
    let w = h.w.borrow();
    assert_eq!(w.routing.classic[&Channel::Game], HEADPHONES);
    assert_eq!(w.routing.classic[&Channel::Chat], REALTEK);
}

#[test]
fn ambiguous_device_name_shows_a_message() {
    let mut h = H::new(World::new());
    h.act(Action::SetOutputDevice {
        pattern: "speakers".into(),
        scope: Scope::Active,
    });
    assert_eq!(
        h.last().message.as_deref(),
        Some("No unique device matches \"speakers\"")
    );
    assert!(h.q.is_empty());
}

#[test]
fn streamer_device_cycle_targets_the_mix() {
    let w = World::new();
    w.borrow_mut().mode = Mode::Stream;
    let mut h = H::new(w);
    h.act(Action::SetOutputDevice {
        pattern: "headphones".into(),
        scope: Scope::All,
    });
    h.run();
    assert_eq!(h.log(), [format!("route monitoring {HEADPHONES}")]);
    h.act(Action::SelectChannel(Channel::Mic));
    h.run();
    h.act(Action::SetOutputDevice {
        pattern: "headset microphone".into(),
        scope: Scope::Active,
    });
    h.run();
    assert!(h.log()[1].starts_with("route mic {0.0.1."));
}

#[test]
fn idle_resync_picks_up_changes_made_in_sonar() {
    let mut h = H::new(World::new());
    h.act(Action::VolumeUp(None));
    h.run();
    h.w.borrow_mut().vols.get_mut(&GAME).unwrap().volume = 0.55;
    h.advance(1000);
    assert!(close(h.last().volume.unwrap(), 0.11), "not yet idle");
    h.advance(2000);
    let (s, show) = h.ui.last().unwrap();
    assert!(close(s.volume.unwrap(), 0.55));
    assert!(!show, "background resync doesn't pop the overlay");
}

#[test]
fn read_never_clobbers_a_newer_local_change() {
    let mut h = H::new(World::new());
    h.advance(10);
    h.act(Action::SelectChannel(Channel::Game)); // queues a read
    h.now += Duration::from_millis(1);
    h.act(Action::VolumeUp(None)); // local 0.11, newer than the read
    h.step(); // the read returns the old 0.101858
    assert!(close(h.last().volume.unwrap(), 0.11));
    h.run();
    assert!(close(h.vol(GAME), 0.11));
}

#[test]
fn mode_change_mid_session_is_picked_up_by_polling() {
    let mut h = H::new(World::new());
    h.w.borrow_mut().mode = Mode::Stream;
    h.advance(2000);
    let s = h.last();
    assert_eq!(s.mode, Some(Mode::Stream));
    assert!(
        h.ui.iter()
            .any(|(s, show)| *show && s.message.as_deref() == Some("Streamer mode"))
    );
    h.act(Action::VolumeUp(None));
    h.run();
    assert_eq!(h.log(), ["rest monitoring/game 0.51"]);
}

#[test]
fn write_in_wrong_mode_refreshes_mode_and_is_dropped() {
    let mut h = H::new(World::new());
    h.w.borrow_mut().mode = Mode::Stream;
    h.w.borrow_mut().fail_endpoint = true; // force REST so Sonar can say "wrong mode"
    h.act(Action::VolumeUp(None));
    h.run();
    assert_eq!(h.last().mode, Some(Mode::Stream));
    assert!(h.log().is_empty(), "the Classic write was not replayed");
    assert!(close(
        h.vol(Target::Stream(Mix::Monitoring, Channel::Game)),
        0.5
    ));
}

#[test]
fn gg_restart_disconnects_backs_off_and_recovers() {
    let mut h = H::new(World::new());
    h.w.borrow_mut().up = false;
    h.w.borrow_mut().fail_endpoint = true;
    h.act(Action::VolumeUp(None));
    h.run();
    let s = h.last();
    assert!(!s.connected);
    assert_eq!(s.message.as_deref(), Some("Sonar unavailable"));

    // Actions while down only show a message.
    h.act(Action::VolumeUp(None));
    assert!(h.q.is_empty());

    h.advance(2000); // retry #1 fails -> back off to 4 s
    assert!(!h.last().connected);
    let deadline = h.c.next_deadline().unwrap();
    assert_eq!(deadline.duration_since(h.now), Duration::from_secs(4));
    h.advance(2000);
    assert!(h.q.is_empty(), "still backing off");

    h.w.borrow_mut().up = true; // GG is back (on a new port: the client rediscovers)
    h.w.borrow_mut().vols.get_mut(&GAME).unwrap().volume = 0.42;
    h.advance(2000);
    let s = h.last();
    assert!(s.connected);
    assert!(close(s.volume.unwrap(), 0.42), "resynced from Sonar");
}

#[test]
fn starts_disconnected_when_sonar_is_down() {
    let w = World::new();
    w.borrow_mut().up = false;
    let mut h = H::new(w);
    let (s, show) = h.ui.last().unwrap();
    assert!(
        !s.connected && !show,
        "tray shows unavailable, overlay stays hidden"
    );
    h.act(Action::NextChannel);
    assert_eq!(h.last().message.as_deref(), Some("Sonar unavailable"));
    h.w.borrow_mut().up = true;
    h.advance(2000);
    assert!(h.last().connected);
}

#[test]
fn polling_deadlines() {
    let mut h = H::new(World::new());
    let d = h.c.next_deadline().unwrap();
    assert_eq!(d.duration_since(h.now), Duration::from_secs(2), "mode poll");
    h.act(Action::VolumeUp(None));
    h.run();
    h.advance(3000);
    // After idle resync, nothing earlier than the next poll is pending.
    assert!(h.c.next_deadline().unwrap() > h.now);
}

#[test]
fn prev_output_device_cycles_backwards() {
    let cfg = ControllerConfig {
        device_cycle: vec![
            "Headphones".into(),
            "CABLE In 16ch".into(),
            "Realtek".into(),
        ],
        ..Default::default()
    };
    let mut h = H::with(World::new(), cfg, Channel::Game);
    h.act(Action::PrevOutputDevice(Scope::Active));
    h.run();
    assert_eq!(
        h.last().device.as_deref(),
        Some("CABLE In 16ch (VB-Audio Virtual Cable)")
    );
}

#[test]
fn toggle_mode_switches_and_resyncs() {
    let mut h = H::new(World::new());
    h.act(Action::ToggleMode);
    h.run();
    assert_eq!(h.w.borrow().mode, Mode::Stream);
    assert_eq!(h.last().mode, Some(Mode::Stream));
    assert!(close(h.last().volume.unwrap(), 0.5));
}

#[test]
fn config_reload_and_active_target() {
    let mut h = H::new(World::new());
    h.act(Action::SelectChannel(Channel::Media));
    assert_eq!(h.c.active(), (Channel::Media, Mix::Monitoring));
    h.c.set_config(ControllerConfig {
        step: 0.05,
        ..Default::default()
    });
    h.run();
    h.act(Action::VolumeUp(None));
    assert!(close(h.last().volume.unwrap(), 0.27));
}
