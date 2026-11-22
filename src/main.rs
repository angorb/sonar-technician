// No console window in release builds (FR-8.4). Debug builds keep the
// console so logs and CLI output are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod config;
mod controller;
#[cfg(windows)]
mod platform;
mod probe;
mod sonar;

use anyhow::{Result, bail};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
usage: sonar-technician [command]

commands:
  (none)      run the tray app (not implemented yet)
  probe       discover Sonar and dump every GET endpoint to ./probe-<timestamp>/
  help        show this message

Set RUST_LOG=debug for request-level logging.";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => bail!("the tray app isn't implemented yet (Phase 3). Try `probe`.\n\n{USAGE}"),
        Some("probe") => {
            probe::run(&std::env::current_dir()?)?;
            Ok(())
        }
        Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}
