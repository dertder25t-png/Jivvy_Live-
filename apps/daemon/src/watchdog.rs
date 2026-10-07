//! Supervises the daemon's processes (engine, outputs): restarts each one independently
//! when it exits, crashes or stops sending heartbeats. Each resumes from its own state, so
//! the watchdog holds none.
//!
//! The watchdog keeps every child's stdin open; if the watchdog dies, the children see
//! end-of-file and exit, so a stale engine never holds the command port.
//!
//! Event lines on stdout are stable and machine-readable (`engine started pid=N`,
//! `outputs stopped pid=N reason=...`); the chaos tests and, later, the System screen read them.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// One supervised process.
pub struct ChildSpec {
    /// Short name used in event lines and logs, e.g. `engine`.
    pub name: &'static str,
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// No heartbeat for this long means the process is hung and gets killed.
    pub hang_timeout: Duration,
    /// Allowed silence before the first heartbeat (start-up can be slower than steady state).
    pub startup_grace: Duration,
}

/// Delay before the next start. Restarts are immediate unless the process keeps dying
/// right after starting; then back off a little, never past 1 s (the plan's target is
/// back on the same slide in under 3 s).
pub fn next_delay(previous: Duration, ran_for: Duration) -> Duration {
    if ran_for >= Duration::from_secs(2) {
        Duration::ZERO
    } else {
        (previous * 2).clamp(Duration::from_millis(100), Duration::from_secs(1))
    }
}

/// Time since a process's last heartbeat, on the monotonic clock: a wall-clock
/// correction (common on church PCs) must never hide a hung process.
#[derive(Clone)]
pub struct Heartbeat {
    start: Instant,
    /// Milliseconds after `start` of the last beat.
    last: Arc<AtomicU64>,
    beaten: Arc<AtomicBool>,
}

impl Heartbeat {
    /// Starts counting as if a beat just arrived.
    pub fn new() -> Self {
        Heartbeat { start: Instant::now(), last: Arc::new(AtomicU64::new(0)), beaten: Arc::default() }
    }

    pub fn beat(&self) {
        self.last.store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.beaten.store(true, Ordering::Relaxed);
    }

    /// True once at least one heartbeat has arrived.
    pub fn has_beaten(&self) -> bool {
        self.beaten.load(Ordering::Relaxed)
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

fn spawn(spec: &ChildSpec) -> std::io::Result<(Child, Heartbeat)> {
    let mut child = Command::new(&spec.exe)
        .args(&spec.args)
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

/// Supervises every child, each on its own thread. Runs forever.
pub fn run(children: Vec<ChildSpec>) -> ! {
    for spec in children {
        std::thread::spawn(move || supervise(spec));
    }
    loop {
        std::thread::park();
    }
}

/// Keeps one child running forever.
pub fn supervise(spec: ChildSpec) -> ! {
    let name = spec.name;
    let mut delay = Duration::ZERO;
    loop {
        std::thread::sleep(delay);
        let started = Instant::now();
        let (mut child, heartbeat) = match spawn(&spec) {
            Ok(c) => c,
            Err(e) => {
                crate::log("watchdog", format!("could not start {name} {}: {e}", spec.exe.display()));
                delay = next_delay(delay, Duration::ZERO);
                continue;
            }
        };
        println!("{name} started pid={}", child.id());
        let reason = loop {
            std::thread::sleep(Duration::from_millis(50));
            match child.try_wait() {
                Ok(Some(status)) => break format!("exited ({status})"),
                Ok(None) => {}
                Err(e) => break format!("wait failed ({e})"),
            }
            let silent = heartbeat.silence();
            let allowed = if heartbeat.has_beaten() { spec.hang_timeout } else { spec.startup_grace };
            if silent > allowed {
                let _ = child.kill();
                let _ = child.wait();
                break format!("hung (no heartbeat for {} ms); killed", silent.as_millis());
            }
        };
        println!("{name} stopped pid={} reason={reason}", child.id());
        crate::log("watchdog", format!("{name} {reason}; restarting"));
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
        assert!(hb.has_beaten() && !Heartbeat::new().has_beaten());
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
