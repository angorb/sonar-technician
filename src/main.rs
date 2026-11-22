// No console window in release builds (FR-8.4). Debug builds keep the
// console so logs and CLI output are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod actions;
#[cfg(windows)]
mod app;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod chord;
mod cli;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod config;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod controller;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod icon;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod logging;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod overlay_view;
#[cfg(windows)]
mod platform;
mod probe;
mod sonar;
mod try_writes;
#[cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows tray app uses this")
)]
mod worker;

use anyhow::{Result, bail};

const USAGE_TEMPLATE: &str = "\
usage: sonar-technician [command]

commands:
  (none)                         run the tray app
  probe                          discover Sonar and dump every GET endpoint to ./probe-<timestamp>/
  help                           show this message
{CLI}

Set RUST_LOG=debug for request-level logging.";

fn usage() -> String {
    USAGE_TEMPLATE.replace("{CLI}", cli::USAGE)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        #[cfg(windows)]
        return app::run();
        #[cfg(not(windows))]
        bail!("the tray app only runs on Windows.\n\n{}", usage());
    }

    // Release builds have no console of their own; borrow the parent's so CLI
    // output shows up in the terminal that started us.
    #[cfg(all(windows, not(debug_assertions)))]
    {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: plain Win32 call; failure just means no console output.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    logging::init_console("info");
    match args[0].as_str() {
        "probe" => {
            probe::run(&std::env::current_dir()?)?;
            Ok(())
        }
        "help" | "-h" | "--help" => {
            println!("{}", usage());
            Ok(())
        }
        cmd => {
            if cli::run(cmd, &args[1..])? {
                Ok(())
            } else {
                bail!("unknown command `{cmd}`\n\n{}", usage())
            }
        }
    }
}
