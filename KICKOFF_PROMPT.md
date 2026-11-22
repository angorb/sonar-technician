# Kickoff prompt — sonar-technician

Paste everything below the line into Claude Code (or another coding agent), opened in an empty project folder that contains `REQUIREMENTS.md` and `CLAUDE.md`.

---

We're building **sonar-technician**, a Windows tray utility in Rust that gives fine-grained control over SteelSeries GG Sonar through Sonar's undocumented local HTTP API. Read `REQUIREMENTS.md` (the spec) and `CLAUDE.md` (architecture rules, API notes, conventions) in full before writing any code.

**Context:**
- I'm on Windows with Rust stable (MSVC) installed and GG/Sonar running.
- My Keychron V1 Max knob sends F17/F18/F19.
- Sonar's own hotkeys only step 5%. I want 1% steps, hotkeys to choose which channel the knob controls, hotkeys to switch output devices, support for both Classic and Streamer mode, and an overlay that shows each change.

**Ground rules:**
- Sonar's API is unofficial. Several routes and all response shapes are unverified (see the ⚠️ rows in CLAUDE.md and §7 of REQUIREMENTS.md). **Don't guess JSON shapes.** Build the probe first, have me run it, and build models from the real output.
- Before using any crate, check its actual API for the version you add (`windows`, `tray-icon`, HTTP and TLS crates change between versions). Prefer a small dependency set.
- Stop at the end of each phase. Summarise what was built, what you need me to run or test on my machine, and what you learned about the API. Update CLAUDE.md's API table whenever something is confirmed.

**Phases:**

**Phase 0 — Scaffold and probe**
1. `cargo new`, add dependencies, and set up the module layout from CLAUDE.md. Use `windows_subsystem = "windows"` in release builds only.
2. Implement discovery: `coreProps.json` → `GET https://{ggEncryptedAddress}/subApps` (skip TLS verification for this loopback call only) → `webServerAddress`, with clear errors for each step.
3. Implement `sonar-technician probe`. It dumps the raw responses of `/mode/`, `/volumeSettings/classic`, `/volumeSettings/streamer`, `/audioDevices`, `/classicRedirections`, `/streamRedirections`, `/streamRedirections/isStreamMonitoringEnabled`, `/configs/selected`, `/chatMix` and `/v1/chatMix` into a timestamped folder. A failing endpoint records its status and body and the probe carries on.
4. Ask me to run the probe in Classic mode and again in Streamer mode, then copy the output into `docs/api-samples/`.
5. Give me a short checklist to resolve Q2–Q4 from REQUIREMENTS.md (UI sync after REST writes, master casing, the Streamer device-switch route). For Q4, walk me through capturing the Sonar UI's traffic with Wireshark or Fiddler on the discovered port while I change a device in Streamer mode.

**Phase 1 — Sonar client and CLI**
- Add typed models from the samples, a `SonarApi` trait with an HTTP implementation, retry-once-after-rediscovery, mode caching and the "Cannot be called in current mode" handling.
- Add CLI commands `set <channel> <0-100>`, `get`, `devices`, `route <channel> <device-substring>` and `mode`. These let me verify every write path by hand in both modes.
- If REST writes don't update the GG UI, add the Core Audio endpoint-volume path for Classic channels (FR-3.7) and let me compare the two.

**Phase 2 — Controller**
- Build the pure state machine: active (mix, channel), the channel cycle, volume stepping with latest-wins coalescing and optional acceleration, mute, device cycling (per channel or all), the post-switch volume re-apply, and the idle resync.
- Add thorough unit tests against a fake `SonarApi`, including a fast knob spin, a device switch dropping volumes, a mode change mid-session and GG restarting on a new port.

**Phase 3 — Hotkeys and tray**
- Add `RegisterHotKey`-based bindings from config, with chord parsing for F13–F24 and modifiers.
- Add the tray icon and menu, single-instance, autostart and config hot-reload.
- Default bindings: F18 = volume up 1%, F17 = volume down 1%, F19 = next channel, Shift+F19 = mute, Ctrl+F19 = next output device (all channels), Alt+F19 = toggle mix (Streamer). Confirm the knob's turn directions with me first.

**Phase 4 — Overlay**
- Build the layered, click-through, no-activate, DPI-aware overlay showing the mode, mix, channel, a volume bar with percentage, mute, and the device name on a switch.
- It updates in place during fast spins and fades after the timeout. Position, monitor, opacity and scale are configurable.

**Phase 5 — Hardening and release**
- Test recovery from a GG restart, sleep/resume, an unplugged device and a broken endpoint.
- Make `clippy -D warnings` clean, document the default config with comments, and write a README covering install, unbinding Sonar's own F17–F19 hotkeys, config reference and known limitations (exclusive fullscreen, unofficial API).
- Produce a release build.

Start with Phase 0. Before writing code, restate the plan in a few lines and list any questions for me.
