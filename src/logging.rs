//! Logging setup (FR-10.1). The tray app logs to
//! `%LOCALAPPDATA%\sonar-technician\logs\sonar-technician.log`, rotated at
//! startup once it passes [`MAX_BYTES`] (keeping [`KEEP`] old files). CLI
//! commands log to stderr only. `RUST_LOG` overrides the configured level.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub const LOG_FILE: &str = "sonar-technician.log";
const MAX_BYTES: u64 = 1024 * 1024;
const KEEP: usize = 3;

fn filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level))
}

/// stderr only (CLI).
pub fn init_console(level: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(filter(level))
        .with_writer(std::io::stderr)
        .init();
}

/// `name` -> `name.1` -> ... -> `name.KEEP` (oldest dropped) when over the limit.
fn rotate(path: &Path) {
    if fs::metadata(path).map_or(true, |m| m.len() < MAX_BYTES) {
        return;
    }
    let numbered = |n: usize| PathBuf::from(format!("{}.{n}", path.display()));
    let _ = fs::remove_file(numbered(KEEP));
    for n in (1..KEEP).rev() {
        let _ = fs::rename(numbered(n), numbered(n + 1));
    }
    let _ = fs::rename(path, numbered(1));
}

fn open_log(dir: &Path) -> std::io::Result<File> {
    fs::create_dir_all(dir)?;
    let path = dir.join(LOG_FILE);
    rotate(&path);
    OpenOptions::new().create(true).append(true).open(path)
}

/// Log file in `dir` (if it can be opened), plus stderr when `console`.
/// Returns an error message if the file couldn't be opened; logging then
/// continues on stderr.
pub fn init_app(level: &str, dir: Option<&Path>, console: bool) -> Option<String> {
    let (file, err) = match dir.map(open_log) {
        Some(Ok(f)) => (Some(f), None),
        Some(Err(e)) => (None, Some(format!("can't open log file: {e}"))),
        None => (
            None,
            Some("no log directory (%LOCALAPPDATA% not set)".to_owned()),
        ),
    };
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(f))
    });
    let console_layer = (console || err.is_some())
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter(level))
        .with(file_layer)
        .with(console_layer)
        .init();
    err
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_three() {
        let dir = std::env::temp_dir().join(format!("st-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        for round in 0..5 {
            fs::write(&path, vec![b'a' + round; MAX_BYTES as usize]).unwrap();
            rotate(&path);
            assert!(!path.exists());
        }
        // Newest rotated file is .1 (round 4 = 'e'); only 3 kept.
        let first = |n: usize| fs::read(format!("{}.{n}", path.display())).unwrap()[0];
        assert_eq!(first(1), b'e');
        assert_eq!(first(2), b'd');
        assert_eq!(first(3), b'c');
        assert!(!PathBuf::from(format!("{}.4", path.display())).exists());
        // Small files are left alone.
        fs::write(&path, b"small").unwrap();
        rotate(&path);
        assert!(path.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
