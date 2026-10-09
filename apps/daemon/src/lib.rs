//! Jivvy Live daemon: the church-computer side of the system.
//!
//! Separate processes, so one crashing never takes down another:
//! - `jivvy-watchdog` starts the others, watches their heartbeats, and restarts whichever
//!   exits or hangs.
//! - `jivvy-engine` holds the live state (slide, black screen), saves a snapshot before
//!   acknowledging every change, and resumes from that snapshot after a restart.
//! - `jivvy-outputs` shows the state fullscreen on the projector and other screens. It
//!   follows the engine like any other device, so an engine restart never blanks a screen.

pub mod bandwidth;
pub mod client;
pub mod ctl;
pub mod engine;
pub mod hardware;
pub mod hls;
pub mod lyrics;
pub mod media;
pub mod outputs;
pub mod program;
pub mod protocol;
pub mod snapshot;
pub mod stream;
pub mod watchdog;

use std::path::{Path, PathBuf};

/// Line the engine prints on stdout to tell the watchdog it is alive.
pub const HEARTBEAT_LINE: &str = "hb";
/// Default address for the local command channel (loopback until the secure channel lands).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:47800";

/// Milliseconds since the Unix epoch. Wall clock: for timestamps only, never for measuring
/// how long something took (it can jump when the system clock is corrected; use `Instant`).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Where the daemon keeps its files unless `--data-dir` says otherwise.
pub fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(d).join("JivvyLive");
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("jivvy-live");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/jivvy-live")
}

/// Replaces a file in one step (temp file + rename), so a reader never sees half of it.
/// Not flushed to disk: for status files that are rewritten constantly. Use
/// `snapshot::Store` for state that must survive a power cut.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Tells the watchdog this process is alive (one heartbeat line on stdout). Exits if the
/// watchdog is gone and stdout is closed.
pub fn heartbeat() {
    use std::io::Write;
    let mut out = std::io::stdout();
    if writeln!(out, "{HEARTBEAT_LINE}").and_then(|_| out.flush()).is_err() {
        std::process::exit(0);
    }
}

/// Exits when the watchdog closes our stdin, so a supervised process never outlives it.
pub fn exit_when_orphaned(who: &'static str) {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut sink = [0u8; 64];
        while matches!(std::io::stdin().read(&mut sink), Ok(n) if n > 0) {}
        log(who, "watchdog gone; exiting");
        std::process::exit(0);
    });
}

/// Prints one log line to stderr with a timestamp. Never pass secrets here.
pub fn log(who: &str, msg: impl std::fmt::Display) {
    eprintln!("{} [{who}] {msg}", now_ms());
}
