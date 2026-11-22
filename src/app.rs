//! The tray app: a hidden window on the UI thread (hotkeys, tray, timers,
//! later the overlay) and the Sonar worker thread.
//!
//! ```text
//! UI thread:  WM_HOTKEY -> Action -> Controller -> Effects
//!                                     |  Command (mpsc)        ^ Event (mpsc + WM_APP_EVENT)
//! worker:                             +-> worker::execute(HttpSonar, EndpointVolume)
//! ```
//!
//! The UI thread never blocks on HTTP: all Sonar access happens on the worker,
//! which wakes the UI thread with `WM_APP_EVENT` when a result is ready.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Instant, SystemTime};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{NIN_SELECT, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetMessageW, HMENU, KillTimer, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SW_SHOWNORMAL, SetForegroundWindow, SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_NULL,
    WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::actions::{Action, Scope};
use crate::config::{self, Config, UiState};
use crate::controller::{Command, Controller, Effect, Event, Snapshot};
use crate::logging;
use crate::platform::endpoint_volume::EndpointVolume;
use crate::platform::hotkeys::Hotkeys;
use crate::platform::tray::{Tray, cursor_pos};
use crate::platform::{autostart, single_instance};
use crate::sonar::client::HttpSonar;
use crate::sonar::{Channel, Mix, Mode};
use crate::worker::{EndpointWriter, execute};

const CLASS: PCWSTR = w!("SonarTechnicianWindow");
const WM_APP_EVENT: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SECOND_INSTANCE: u32 = WM_APP + 3;
/// `NIN_SELECT | NINF_KEY` (keyboard activation of the tray icon).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TIMER_TICK: usize = 1;
const TIMER_CONFIG: usize = 2;
const CONFIG_POLL_MS: u32 = 1000;

// Tray menu command ids.
const ID_CHANNEL: usize = 100; // + index into Channel::ALL
const ID_MIX: usize = 120; // + index into Mix::ALL
const ID_DEVICE: usize = 200; // + index into App::menu_devices
const ID_OVERLAY: usize = 300;
const ID_OPEN_CONFIG: usize = 301;
const ID_RELOAD_CONFIG: usize = 302;
const ID_OPEN_LOGS: usize = 303;
const ID_AUTOSTART: usize = 304;
const ID_QUIT: usize = 399;

/// `TaskbarCreated` (Explorer restarted): the tray icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

struct App {
    hwnd: HWND,
    controller: Controller,
    cmd_tx: Sender<Command>,
    evt_rx: Receiver<Event>,
    tray: Tray,
    hotkeys: Hotkeys,
    config: Config,
    config_path: PathBuf,
    config_mtime: Option<SystemTime>,
    state_path: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    saved_active: (Channel, Mix),
    menu_devices: Vec<String>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app state. `None` if there's no app yet, or if we're being
/// re-entered (e.g. a message dispatched from inside a modal loop), in which
/// case the message is dropped rather than panicking.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn shell_open(target: &Path) -> bool {
    let target = HSTRING::from(target.as_os_str());
    // SAFETY: plain ShellExecute; > 32 means success.
    let r = unsafe { ShellExecuteW(None, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    r.0 as isize > 32
}

impl App {
    fn apply(&mut self, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Sonar(cmd) => {
                    if self.cmd_tx.send(cmd).is_err() {
                        tracing::error!("Sonar worker has stopped");
                    }
                }
                Effect::Ui {
                    snapshot,
                    show_overlay,
                } => self.show(&snapshot, show_overlay),
            }
        }
        self.schedule_tick();
    }

    fn show(&mut self, s: &Snapshot, show_overlay: bool) {
        self.tray.update(&s.tooltip(), s.connected);
        if show_overlay && self.config.overlay.enabled {
            // Phase 4: the overlay window. Until then, the tray tooltip and
            // the debug log show the state.
            tracing::debug!(
                "overlay: {:?} {:?}/{:?} vol={:?} muted={:?} dev={:?} msg={:?}",
                s.mode,
                s.mix,
                s.channel,
                s.volume,
                s.muted,
                s.device,
                s.message
            );
        }
        let active = self.controller.active();
        if active != self.saved_active {
            self.saved_active = active;
            if let Some(p) = &self.state_path {
                UiState::new(active.0, active.1).save(p);
            }
        }
    }

    fn schedule_tick(&self) {
        let Some(deadline) = self.controller.next_deadline() else {
            // SAFETY: killing our own timer.
            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_TICK) };
            return;
        };
        let ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 60_000) as u32;
        // SAFETY: (re)arming our own timer; same id replaces the old one.
        unsafe { SetTimer(Some(self.hwnd), TIMER_TICK, ms, None) };
    }

    fn act(&mut self, action: Action) {
        let fx = self.controller.handle_action(Instant::now(), action);
        self.apply(fx);
    }

    fn on_hotkey(&mut self, id: usize) {
        let Some(b) = Hotkeys::index_of(id).and_then(|i| self.config.bindings.get(i)) else {
            return;
        };
        tracing::debug!("hotkey {} -> {:?}", b.chord, b.action);
        let action = b.action.clone();
        self.act(action);
    }

    fn on_events(&mut self) {
        while let Ok(ev) = self.evt_rx.try_recv() {
            let fx = self.controller.handle_event(Instant::now(), ev);
            self.apply(fx);
        }
    }

    fn on_tick(&mut self) {
        let fx = self.controller.tick(Instant::now());
        self.apply(fx);
    }

    fn register_hotkeys(&mut self) {
        let chords: Vec<_> = self.config.bindings.iter().map(|b| b.chord).collect();
        let failed = self.hotkeys.register_all(&chords);
        if failed.is_empty() {
            tracing::info!("registered {} hotkeys", chords.len());
            return;
        }
        let list: Vec<String> = failed.iter().map(|(c, _)| c.to_string()).collect();
        for (c, e) in &failed {
            tracing::warn!("hotkey {c} unavailable: {e}");
        }
        self.tray.notify(
            "Some hotkeys are unavailable",
            &format!(
                "{} already in use by another app (e.g. SteelSeries GG). Unbind it there.",
                list.join(", ")
            ),
            true,
        );
    }

    fn check_config(&mut self) {
        let m = mtime(&self.config_path);
        if m != self.config_mtime {
            self.config_mtime = m;
            self.reload_config(false);
        }
    }

    fn reload_config(&mut self, from_menu: bool) {
        match config::load_or_create(&self.config_path) {
            Ok(c) => {
                let bindings_changed = c.bindings != self.config.bindings;
                self.controller.set_config(c.controller.clone());
                self.config = c;
                if bindings_changed || from_menu {
                    self.register_hotkeys();
                }
                tracing::info!("config reloaded");
                if from_menu {
                    self.tray.notify("Config reloaded", "Settings applied.", false);
                }
            }
            Err(e) => {
                tracing::warn!("{e}");
                self.tray.notify(
                    "Config error: keeping previous settings",
                    &e.to_string(),
                    true,
                );
            }
        }
    }

    fn build_menu(&mut self) -> Option<HMENU> {
        let s = self.controller.snapshot(None);
        // SAFETY: building a menu we own; it's destroyed by `show_menu`.
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            let item = |m: HMENU, id: usize, text: &str, checked: bool| {
                let flags = if checked {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
            };

            let channels = CreatePopupMenu().ok()?;
            for (i, c) in Channel::ALL.iter().enumerate() {
                item(channels, ID_CHANNEL + i, c.label(), *c == s.channel);
            }
            let _ = AppendMenuW(menu, MF_POPUP, channels.0 as usize, w!("Channel"));

            if s.mode == Some(Mode::Stream) {
                let mixes = CreatePopupMenu().ok()?;
                for (i, m) in Mix::ALL.iter().enumerate() {
                    item(mixes, ID_MIX + i, m.label(), *m == s.mix);
                }
                let _ = AppendMenuW(menu, MF_POPUP, mixes.0 as usize, w!("Mix"));
            }

            let devices = CreatePopupMenu().ok()?;
            let choices = self.controller.device_choices();
            if choices.is_empty() {
                let _ = AppendMenuW(
                    devices,
                    MF_STRING | MF_GRAYED,
                    0,
                    w!("(Sonar not connected)"),
                );
            }
            self.menu_devices.clear();
            for (i, (name, current)) in choices.into_iter().enumerate() {
                item(devices, ID_DEVICE + i, &name, current);
                self.menu_devices.push(name);
            }
            let label = if s.channel == Channel::Mic {
                "Input device"
            } else {
                "Output device"
            };
            let _ = AppendMenuW(menu, MF_POPUP, devices.0 as usize, &HSTRING::from(label));

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            item(
                menu,
                ID_OVERLAY,
                "Show overlay",
                self.config.overlay.enabled,
            );
            item(menu, ID_OPEN_CONFIG, "Open config", false);
            item(menu, ID_RELOAD_CONFIG, "Reload config", false);
            item(menu, ID_OPEN_LOGS, "Open log folder", false);
            item(
                menu,
                ID_AUTOSTART,
                "Start with Windows",
                autostart::is_enabled(),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            item(menu, ID_QUIT, "Quit", false);
            Some(menu)
        }
    }

    fn menu_command(&mut self, id: usize) {
        match id {
            _ if (ID_CHANNEL..ID_CHANNEL + Channel::ALL.len()).contains(&id) => {
                self.act(Action::SelectChannel(Channel::ALL[id - ID_CHANNEL]));
            }
            _ if (ID_MIX..ID_MIX + Mix::ALL.len()).contains(&id) => {
                if self.controller.active().1 != Mix::ALL[id - ID_MIX] {
                    self.act(Action::ToggleMix);
                }
            }
            _ if (ID_DEVICE..ID_DEVICE + self.menu_devices.len()).contains(&id) => {
                let pattern = self.menu_devices[id - ID_DEVICE].clone();
                self.act(Action::SetOutputDevice {
                    pattern,
                    scope: Scope::Active,
                });
            }
            ID_OVERLAY => {
                // In-memory only; the config file's value wins on next reload.
                self.config.overlay.enabled = !self.config.overlay.enabled;
            }
            ID_OPEN_CONFIG => {
                if !shell_open(&self.config_path) {
                    let path = HSTRING::from(self.config_path.as_os_str());
                    // SAFETY: plain ShellExecute fallback to Notepad.
                    unsafe {
                        ShellExecuteW(
                            None,
                            w!("open"),
                            w!("notepad.exe"),
                            &path,
                            None,
                            SW_SHOWNORMAL,
                        )
                    };
                }
            }
            ID_RELOAD_CONFIG => self.reload_config(true),
            ID_OPEN_LOGS => {
                if let Some(d) = &self.log_dir {
                    shell_open(d);
                }
            }
            ID_AUTOSTART => {
                let want = !autostart::is_enabled();
                match autostart::set(want) {
                    Ok(()) => tracing::info!("start with Windows: {want}"),
                    Err(e) => {
                        tracing::warn!("{e}");
                        self.tray.notify("Start with Windows", &e, true);
                    }
                }
            }
            ID_QUIT => {
                // SAFETY: destroying our own window ends the message loop.
                let _ = unsafe { DestroyWindow(self.hwnd) };
            }
            _ => {}
        }
    }
}

/// Show the tray menu at the cursor. The app borrow is released while the
/// menu's modal loop runs (it dispatches other messages).
fn show_menu(hwnd: HWND) {
    let Some(Some(menu)) = with_app(App::build_menu) else {
        return;
    };
    let pt = cursor_pos();
    // SAFETY: standard tray-menu sequence on our own window and menu.
    let cmd = unsafe {
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    };
    if cmd != 0 {
        with_app(|a| a.menu_command(cmd));
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            with_app(|a| a.on_hotkey(wparam.0));
        }
        WM_APP_EVENT => {
            with_app(App::on_events);
        }
        WM_TIMER => match wparam.0 {
            TIMER_TICK => {
                with_app(App::on_tick);
            }
            TIMER_CONFIG => {
                with_app(App::check_config);
            }
            _ => {}
        },
        WM_APP_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_KEYSELECT {
                show_menu(hwnd);
            }
        }
        WM_APP_SECOND_INSTANCE => {
            with_app(|a| {
                a.tray.notify(
                    "Sonar Technician is already running",
                    "It's in the system tray.",
                    false,
                )
            });
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            with_app(|a| a.tray.add());
        }
        // SAFETY: default handling for everything else.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

fn create_window() -> Result<HWND> {
    // SAFETY: standard window-class registration and creation; the window is
    // never shown (it exists for hotkeys, tray callbacks and timers).
    unsafe {
        let hinstance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            anyhow::bail!("RegisterClassW failed");
        }
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS,
            w!("Sonar Technician"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .context("CreateWindowExW")
    }
}

/// Sonar worker: owns the HTTP client and the Core Audio writer (COM objects
/// must stay on the thread that created them).
fn spawn_worker(hwnd: HWND, rx: Receiver<Command>, tx: Sender<Event>) -> Result<()> {
    // HWND isn't Send; pass the raw value. Posting to a destroyed window is harmless.
    let raw = hwnd.0 as isize;
    std::thread::Builder::new()
        .name("sonar-worker".into())
        .spawn(move || {
            let hwnd = HWND(raw as *mut _);
            let mut api = HttpSonar::new();
            let mut endpoint = match EndpointVolume::new() {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!("Windows volume path unavailable: {e}");
                    None
                }
            };
            for cmd in rx {
                tracing::debug!("worker: {cmd:?}");
                let ep = endpoint.as_mut().map(|e| e as &mut dyn EndpointWriter);
                let ev = execute(&mut api, ep, cmd);
                if tx.send(ev).is_err() {
                    break;
                }
                // SAFETY: wake the UI thread; failure just means it's gone.
                let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_EVENT, WPARAM(0), LPARAM(0)) };
            }
        })
        .context("spawning the Sonar worker")?;
    Ok(())
}

pub fn run() -> Result<()> {
    let Some(_instance) = single_instance::acquire() else {
        single_instance::notify_existing(CLASS, WM_APP_SECOND_INSTANCE);
        return Ok(());
    };

    let config_path = config::config_dir()
        .context("%APPDATA% is not set")?
        .join("config.toml");
    let data_dir = config::data_dir();
    let log_dir = data_dir.as_ref().map(|d| d.join("logs"));
    let state_path = data_dir.as_ref().map(|d| d.join("state.json"));

    let (config, config_err) = match config::load_or_create(&config_path) {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e)),
    };
    if let Some(e) = logging::init_app(
        &config.log_level,
        log_dir.as_deref(),
        cfg!(debug_assertions),
    ) {
        eprintln!("{e}");
    }
    tracing::info!(
        "sonar-technician {} starting; config {}",
        env!("CARGO_PKG_VERSION"),
        config_path.display()
    );

    let hwnd = create_window()?;
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    spawn_worker(hwnd, cmd_rx, evt_tx)?;

    let (channel, mix) = state_path
        .as_deref()
        .map_or((Channel::Game, Mix::Monitoring), UiState::load);
    let tray = Tray::new(hwnd, WM_APP_TRAY);
    tray.add();
    let mut app = App {
        hwnd,
        controller: Controller::new(config.controller.clone(), channel, mix),
        cmd_tx,
        evt_rx,
        tray,
        hotkeys: Hotkeys::new(hwnd),
        config,
        config_mtime: mtime(&config_path),
        config_path,
        state_path,
        log_dir,
        saved_active: (channel, mix),
        menu_devices: Vec::new(),
    };
    if let Some(e) = config_err {
        tracing::warn!("{e}");
        app.tray.notify(
            "Config error: using built-in defaults",
            &e.to_string(),
            true,
        );
    }
    app.register_hotkeys();
    let fx = app.controller.start(Instant::now());
    app.apply(fx);
    APP.with(|cell| *cell.borrow_mut() = Some(app));

    // SAFETY: arming our own timer.
    unsafe { SetTimer(Some(hwnd), TIMER_CONFIG, CONFIG_POLL_MS, None) };

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Drop the app (removes the tray icon, unregisters hotkeys, closes the
    // command channel so the worker exits).
    APP.with(|cell| cell.borrow_mut().take());
    tracing::info!("exiting");
    Ok(())
}
