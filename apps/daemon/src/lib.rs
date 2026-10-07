//! Jivvy Live daemon: the church-computer side of the system.
//!
//! Two processes, so a crash in the live engine never takes down its supervisor:
//! - `jivvy-watchdog` starts the engine, watches its heartbeat, and restarts it when it
//!   exits or hangs.
//! - `jivvy-engine` holds the live state (slide, black screen), saves a snapshot before
//!   acknowledging every change, and resumes from that snapshot after a restart.

pub mod engine;
pub mod protocol;
pub mod snapshot;
pub mod watchdog;

/// Line the engine prints on stdout to tell the watchdog it is alive.
pub const HEARTBEAT_LINE: &str = "hb";
/// Default address for the local command channel (loopback until the secure channel lands).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:47800";

/// Milliseconds since the Unix epoch. Wall clock: for timestamps only, never for measuring
/// how long something took (it can jump when the system clock is corrected; use `Instant`).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Prints one log line to stderr with a timestamp. Never pass secrets here.
pub fn log(who: &str, msg: impl std::fmt::Display) {
    eprintln!("{} [{who}] {msg}", now_ms());
}
