# sonar-technician

A small Windows tray utility for fine-grained keyboard / knob control of
**SteelSeries GG Sonar**:

- **1% volume steps** (configurable) instead of Sonar's fixed 5%
- hotkeys to **choose which channel** the knob controls (master, game, chat,
  media, aux, mic)
- hotkeys to **switch output devices** (one channel, or headphones ↔ speakers
  for everything at once)
- works in Sonar's **Classic** and **Streamer** modes, and picks up mode
  changes automatically
- a small **on-screen overlay** that shows every change and never steals focus

It drives Sonar's local HTTP API, the one Sonar's own window uses. That API
is **undocumented and unofficial**; see [Known limitations](#known-limitations).

> Not affiliated with or endorsed by SteelSeries.

## Requirements

- Windows 10 (1809 or later) or Windows 11, x64
- SteelSeries GG with Sonar enabled
- To build: Rust stable with the MSVC toolchain (`x86_64-pc-windows-msvc`)

## Install

```powershell
cargo build --release
```

This produces a single self-contained `target\release\sonar-technician.exe`.
Copy it anywhere, for example `%LOCALAPPDATA%\Programs\sonar-technician\`,
and run it. An icon appears in the system tray (orange when connected to
Sonar, grey when Sonar isn't available).

To start it with Windows, right-click the tray icon and tick
**Start with Windows**. No admin rights are needed.

### Important: unbind Sonar's own hotkeys

SteelSeries GG has its own Sonar hotkeys. If GG also uses a key this tool
binds (by default **F17, F18 and F19**, plus Shift/Ctrl/Alt+F17), both react
to it. In GG, open the Sonar hotkey / shortcut settings and clear any
shortcut that uses those keys.

If another app owns one of the keys, the tool shows a Windows notification
naming it when it starts.

## Default controls

The defaults are set up for a Keychron V1 Max whose knob is remapped (in
Keychron Launcher) to send **F18** (turn left), **F19** (turn right) and
**F17** (press):

| Key | Action |
|---|---|
| F19 (knob right) | Volume up 1% |
| F18 (knob left) | Volume down 1% |
| F17 (knob press) | Next channel (master → game → chat → media → aux) |
| Shift + F17 | Mute / unmute the active channel |
| Ctrl + F17 | Next output device for all channels (headphones ↔ speakers) |
| Alt + F17 | Streamer mode: switch between the Monitoring and Streaming mix |

Holding a key repeats it. Everything is configurable; see below.

## Overlay

Every change shows a small panel (bottom-right by default) with the mode,
mix (Streamer), channel, a volume bar with percentage, mute state, and the
new device after a switch. It updates in place while you spin the knob and
fades out 1.5 s after the last change. Clicks go straight through it and it
never takes keyboard focus.

**Left-click** the tray icon to show the current state at any time.

## Tray menu

Right-click the tray icon:

- **Channel** / **Mix** (Streamer mode): pick what the knob controls
- **Output device** (or **Input device** for mic): route the active
  channel or mix
- **Show overlay**: turn the overlay on/off (until the config is next reloaded)
- **Open config**, **Reload config**, **Open log folder**
- **Start with Windows**
- **Quit**

## Configuration

`%APPDATA%\sonar-technician\config.toml` is created on first run, with every
option explained in comments. Changes apply as soon as you save. If the
file has a mistake, the previous settings stay active and a notification
shows the error with its line and column.

| Section | Options |
|---|---|
| top level | `log_level` (`error`…`trace`, needs a restart) |
| `[volume]` | `step` (0.01 = 1%), `classic_write` (`"windows"` or `"rest"`, see [How volume changes are applied](#how-volume-changes-are-applied)) |
| `[volume.acceleration]` | `enabled`, `window_ms`, `threshold`, `multiplier`: bigger steps when spinning fast |
| `[channels]` | `cycle`: which channels next/prev channel rotate through, in order |
| `[devices]` | `cycle`: output devices to rotate through, as name fragments (`["Headphones", "Realtek"]`); empty = all |
| `[overlay]` | `enabled`, `position` (`top-left` … `bottom-right`, `top-center`, `bottom-center`), `offset`, `timeout_ms`, `opacity`, `scale`, `monitor` (`"primary"` or `"cursor"`) |
| `[sonar]` | `mode_poll_ms`, `volume_poll_ms`, `idle_resync_ms` |
| `[[bindings]]` | `keys` + `action` (+ options), see below |

### Bindings

```toml
[[bindings]]
keys = "Ctrl+Alt+H"            # F1-F24, A-Z, 0-9, NumPad0-9, PageUp, VolumeUp, ...
action = "set_output_device"   #   with Ctrl / Alt / Shift / Win
device = "Headphones"
scope = "all"
```

| Action | Options |
|---|---|
| `volume_up`, `volume_down` | `step = 0.05` (optional) |
| `set_volume` | `value = 0..100` |
| `toggle_mute` | |
| `next_channel`, `prev_channel` | |
| `select_channel` | `channel = "game"` (`master`, `game`, `chat`, `media`, `aux`, `mic`) |
| `toggle_mix` | Streamer mode only |
| `next_output_device`, `prev_output_device` | `scope = "active"` (default) or `"all"` |
| `set_output_device` | `device = "name fragment"`, `scope` |
| `toggle_mode` | Classic ↔ Streamer (disruptive; not bound by default) |

`scope = "all"` means game, chat, media and aux together in Classic mode, and
the Monitoring mix (what you hear) in Streamer mode. With the **mic** channel
active, the device actions switch the input device.

Run `sonar-technician devices` to see the exact device names.

Other files:

- `%LOCALAPPDATA%\sonar-technician\state.json`: the last active channel/mix
- `%LOCALAPPDATA%\sonar-technician\logs\sonar-technician.log`: the log
  (rotated at 1 MB, three old files kept)

## Command line

The same executable has commands for scripting and troubleshooting. They
work while the tray app is running.

```text
sonar-technician probe                  dump every Sonar GET endpoint to .\probe-<timestamp>\
sonar-technician mode [classic|stream]  show (or switch) the mode
sonar-technician get                    volumes and routing for the current mode
sonar-technician devices [--all]        list devices (--all includes Sonar's virtual ones)
sonar-technician set <channel> <0-100> [--mix monitoring|streaming] [--via rest|endpoint]
sonar-technician mute <channel> [on|off|toggle] [--mix ...] [--via ...]
sonar-technician route <channel|all|monitoring|streaming|mic> <device name fragment>
sonar-technician try-writes [channel] [--routing]   check which write routes Sonar accepts
sonar-technician raw-get <path> | raw-put <path>    talk to the API directly
```

## How volume changes are applied

- **Classic mode, game/chat/media/aux/mic:** by default the tool sets the
  Windows volume of Sonar's own virtual device for that channel
  ("SteelSeries Sonar - Gaming", …). Sonar mirrors it into its slider, so
  the GG window stays in sync. If that ever fails, it falls back to the API.
- **Master, and everything in Streamer mode:** through Sonar's API. The audio
  changes immediately, but **the GG window doesn't show it** until it
  refreshes (see below).

The tool re-reads Sonar after you stop turning the knob (and every few
seconds), so changes made in the Sonar window are picked up and never
overwritten by stale values.

## Known limitations

- **Unofficial API.** Sonar's local API isn't documented and can change with
  any GG update. The tool finds Sonar's port automatically on every start
  and reconnects after GG restarts. If an update breaks one endpoint, only
  that feature stops working: the overlay says so and the log has details.
  Running `sonar-technician probe` shows what changed. Verified with
  **GG 124.0.0**.
- **The GG window doesn't always refresh.** Changes to master, anything in
  Streamer mode, Streamer device switches, and sometimes the mute button
  only show in the GG window after you navigate away and back. The audio is
  correct either way.
- **Exclusive-fullscreen games** draw over every window, so the overlay can't
  appear on top of them. Borderless and windowed modes work. The keys still
  work in fullscreen.
- **Mode switching** (`toggle_mode`) is disruptive to audio, so it isn't
  bound by default.

## Troubleshooting

- **Nothing happens on a key press:** check that the tray icon is orange.
  Hover it for status. Check whether GG still has the same key bound (see
  above), and whether a "hotkeys unavailable" notification appeared at start.
- **"Sonar unavailable":** GG isn't running or Sonar is disabled in GG. The
  tool keeps retrying (every 2 s, slowing to 30 s) and reconnects by itself,
  including after sleep.
- **Something stopped working after a GG update:** look in the log (tray →
  Open log folder) for lines with the failing URL and status, and run
  `sonar-technician probe`.
- **More detail:** set the environment variable `RUST_LOG=sonar_technician=debug`
  before starting the tool to log every request.

## Development

See [`CLAUDE.md`](CLAUDE.md) for the architecture, API notes and conventions,
and [`REQUIREMENTS.md`](REQUIREMENTS.md) for the spec.

```powershell
cargo test                                   # no Sonar needed
cargo clippy --all-targets -- -D warnings
cargo run                                    # tray app with a console for logs
cargo run -- probe
```
