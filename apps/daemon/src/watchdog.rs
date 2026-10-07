//! Supervises the engine process: restarts it when it exits, crashes or stops sending
//! heartbeats. The engine resumes from its own snapshot, so the watchdog holds no state.
//!
//! The watchdog keeps the engine's stdin open; if the watchdog dies, the engine sees
//! end-of-file and exits, so a stale engine never holds the command port.
//!
//! Event lines on stdout are stable and machine-readable (`engine started pid=N`, ...);
//! the chaos tests and, later, the System screen read them.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Config {
    pub engine: PathBuf,
    pub engine_args: Vec<String>,
    /// No heartbeat for this long means the engine is hung and gets killed.
    pub hang_timeout: Duration,
}

/// Delay before the next start. Restarts are immediate unless the engine keeps dying
/// right after starting; then back off a little, never past 1 s (the plan's target is
/// back on the same slide in under 3 s).
pub fn next_delay(previous: Duration, ran_for: Duration) -> Duration {
    if ran_for >= Duration::from_secs(2) {
        Duration::ZERO
    } else {
        (previous * 2).clamp(Duration::from_millis(100), Duration::from_secs(1))
    }
}

/// Time since the engine's last heartbeat, on the monotonic clock: a wall-clock
/// correction (common on church PCs) must never hide a hung engine.
#[derive(Clone)]
pub struct Heartbeat {
    start: Instant,
    /// Milliseconds after `start` of the last beat.
    last: Arc<AtomicU64>,
}

impl Heartbeat {
    /// Starts counting as if a beat just arrived.
    pub fn new() -> Self {
        Heartbeat { start: Instant::now(), last: Arc::new(AtomicU64::new(0)) }
    }

    pub fn beat(&self) {
        self.last.store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    pub fn silence(&self) -> Duration {
        self.start.elapsed().saturating_sub(Duration::from_millis(self.last.load(Ordering::Relaxed)))
    }
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn(cfg: &Config) -> std::io::Result<(Child, Heartbeat)> {
    let mut child = Command::new(&cfg.engine)
        .args(&cfg.engine_args)
        .arg("--supervised")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let heartbeat = Heartbeat::new();
    let stdout = child.stdout.take().expect("piped stdout");
    let beat = heartbeat.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line.trim() == crate::HEARTBEAT_LINE {
                beat.beat();
            }
        }
    });
    Ok((child, heartbeat))
}

/// Runs forever.
pub fn run(cfg: Config) -> ! {
    let mut delay = Duration::ZERO;
    loop {
        std::thread::sleep(delay);
        let started = Instant::now();
        let (mut child, heartbeat) = match spawn(&cfg) {
            Ok(c) => c,
            Err(e) => {
                crate::log("watchdog", format!("could not start engine {}: {e}", cfg.engine.display()));
                delay = next_delay(delay, Duration::ZERO);
                continue;
            }
        };
        println!("engine started pid={}", child.id());
        let reason = loop {
            std::thread::sleep(Duration::from_millis(50));
            match child.try_wait() {
                Ok(Some(status)) => break format!("exited ({status})"),
                Ok(None) => {}
                Err(e) => break format!("wait failed ({e})"),
            }
            let silent = heartbeat.silence();
            if silent > cfg.hang_timeout {
                let _ = child.kill();
                let _ = child.wait();
                break format!("hung (no heartbeat for {} ms); killed", silent.as_millis());
            }
        };
        println!("engine stopped pid={} reason={reason}", child.id());
        crate::log("watchdog", format!("engine {reason}; restarting"));
        delay = next_delay(delay, started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_silence_grows_until_the_next_beat() {
        let hb = Heartbeat::new();
        std::thread::sleep(Duration::from_millis(60));
        assert!(hb.silence() >= Duration::from_millis(60));
        let other_thread = hb.clone();
        std::thread::spawn(move || other_thread.beat()).join().unwrap();
        assert!(hb.silence() < Duration::from_millis(60));
    }

    #[test]
    fn restarts_immediately_after_a_long_run_and_backs_off_on_a_crash_loop() {
        let ms = Duration::from_millis;
        assert_eq!(next_delay(ms(800), Duration::from_secs(60)), Duration::ZERO);
        assert_eq!(next_delay(Duration::ZERO, ms(10)), ms(100));
        assert_eq!(next_delay(ms(100), ms(10)), ms(200));
        assert_eq!(next_delay(ms(800), ms(10)), Duration::from_secs(1));
        assert_eq!(next_delay(Duration::from_secs(1), ms(10)), Duration::from_secs(1));
    }
}
