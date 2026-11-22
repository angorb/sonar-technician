// No console window in release builds (FR-8.4). Debug builds keep the
// console so logs and CLI output are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the tray app in Phase 3")
)]
mod actions;
mod cli;
mod config;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the tray app in Phase 3")
)]
mod controller;
#[cfg(windows)]
mod platform;
mod probe;
mod sonar;
mod try_writes;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the tray app in Phase 3")
)]
mod worker;

use anyhow::{Result, bail};
use tracing_subscriber::EnvFilter;

const USAGE_TEMPLATE: &str = "\
usage: sonar-technician [command]

commands:
  (none)                         run the tray app (not implemented yet)
  probe                          discover Sonar and dump every GET endpoint to ./probe-<timestamp>/
  help                           show this message
{CLI}

Set RUST_LOG=debug for request-level logging.";

fn usage() -> String {
    USAGE_TEMPLATE.replace("{CLI}", cli::USAGE)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => bail!(
            "the tray app isn't implemented yet (Phase 3). Try `probe`.\n\n{}",
            usage()
        ),
        Some("probe") => {
            probe::run(&std::env::current_dir()?)?;
            Ok(())
        }
        Some("help" | "-h" | "--help") => {
            println!("{}", usage());
            Ok(())
        }
        Some(cmd) => {
            if cli::run(cmd, &args[1..])? {
                Ok(())
            } else {
                bail!("unknown command `{cmd}`\n\n{}", usage())
            }
        }
    }
}
