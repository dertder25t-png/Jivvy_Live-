//! Alerts by email: the same stream-down and no-sound alerts that go out by web push (and
//! their "back"), also emailed to the church account's own address by live.jivvy.org.
//!
//! The church computer POSTs the alert's kind and its plain words, with its license key as
//! proof of which church it is, to `ENDPOINT`. The site checks the key's signature and church
//! id and emails that church's account address only; it also keeps the limits (one email per
//! kind per 10 minutes, 20 a day, a "back" only after a "down" was emailed).
//!
//! Like `push.rs`, nothing here ever holds up the service: the engine only queues, and one
//! worker thread posts from a bounded queue, retries a few times, and a failure is logged and
//! dropped. It is off by default (the setting is `alert-email.json` in the data dir) and needs
//! a license key; without one nothing is sent. The key is never logged; the logs name the host.
//! A trial or unlicensed computer is not an account, so it cannot send.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Where alerts are posted.
pub const ENDPOINT: &str = "https://live.jivvy.org/api/alert";
/// The alert kinds that are emailed (`ALERT_EMAIL_KINDS` in the TypeScript protocol).
pub const KINDS: [&str; 4] = ["stream.down", "stream.up", "audio.silent", "audio.back"];
/// Posts waiting for the worker (new and retrying); more are dropped and logged.
const QUEUE: usize = 16;
/// Waits before each retry of a post that didn't go through.
const RETRY_AFTER: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(30), Duration::from_secs(120)];

/// What alert.email.get and alert.email.set answer (the TypeScript `AlertEmailStatus`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// The church turned alert emails on. Off by default.
    pub enabled: bool,
    /// A license key is saved, so the site can tell which church to email.
    pub available: bool,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    enabled: bool,
}

/// What is posted: the key (proof of the church), the kind and the plain words. Nothing else.
#[derive(Serialize)]
struct Body<'a> {
    key: &'a str,
    kind: &'a str,
    message: &'a str,
}

struct Job {
    body: Vec<u8>,
    attempt: usize,
}

pub struct AlertEmail {
    path: PathBuf,
    enabled: AtomicBool,
    worker: SyncSender<Job>,
    /// Tests post to a local stand-in instead of the site.
    #[cfg_attr(not(test), allow(dead_code))]
    url: Arc<std::sync::Mutex<String>>,
}

impl AlertEmail {
    /// Reads the setting in `dir` (off if there is none) and starts the worker.
    pub fn load(dir: &Path) -> AlertEmail {
        let path = dir.join("alert-email.json");
        let enabled = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<File>(&b).ok())
            .is_some_and(|f| f.version == 1 && f.enabled);
        let (worker, rx) = sync_channel::<Job>(QUEUE);
        let url = Arc::new(std::sync::Mutex::new(ENDPOINT.to_string()));
        let target = url.clone();
        std::thread::spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                .timeout_global(Some(Duration::from_secs(15)))
                .user_agent(format!("Jivvy / Jivvy Live / {}", env!("CARGO_PKG_VERSION")))
                .build()
                .into();
            let mut retries: Vec<(Instant, Job)> = Vec::new();
            loop {
                let wait = retries.iter().map(|(due, _)| due.saturating_duration_since(Instant::now())).min();
                let msg = match wait {
                    Some(w) => rx.recv_timeout(w),
                    None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                match msg {
                    Ok(job) => retries.push((Instant::now(), job)),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                let now = Instant::now();
                let (due, later): (Vec<_>, Vec<_>) = retries.drain(..).partition(|(at, _)| *at <= now);
                retries = later;
                for (_, job) in due {
                    let url = target.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    if let Some(next) = post(&agent, &url, job) {
                        if retries.len() >= QUEUE {
                            crate::log("engine", "alert email dropped: too many waiting");
                            continue;
                        }
                        retries.push((Instant::now() + RETRY_AFTER[next.attempt - 1], next));
                    }
                }
            }
        });
        AlertEmail { path, enabled: AtomicBool::new(enabled), worker, url }
    }

    pub fn status(&self, has_license: bool) -> Status {
        Status { enabled: self.enabled.load(Ordering::Acquire), available: has_license }
    }

    /// Turns alert emails on or off, saved for the next run.
    pub fn set(&self, enabled: bool) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(&File { version: 1, enabled }).map_err(std::io::Error::other)?;
        crate::write_atomic(&self.path, &bytes)?;
        self.enabled.store(enabled, Ordering::Release);
        Ok(())
    }

    /// Queues the alert for the site if emails are on, the kind is one that is emailed, and
    /// there is a license `key`. Never waits: a full queue drops it.
    pub fn alert(&self, key: Option<&str>, kind: &str, message: &str) {
        let Some(key) = key else { return };
        if !self.enabled.load(Ordering::Acquire) || !KINDS.contains(&kind) {
            return;
        }
        let Ok(body) = serde_json::to_vec(&Body { key, kind, message }) else { return };
        match self.worker.try_send(Job { body, attempt: 0 }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => crate::log("engine", "alert email dropped: too many waiting"),
            Err(TrySendError::Disconnected(_)) => crate::log("engine", "alert email dropped: no worker"),
        }
    }

    #[cfg(test)]
    pub(crate) fn post_to(&self, url: &str) {
        *self.url.lock().unwrap() = url.into();
    }
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("")
}

/// Posts one alert; the job back if it should be tried again.
fn post(agent: &ureq::Agent, url: &str, mut job: Job) -> Option<Job> {
    let to = host(url).to_string();
    let failed = match agent.post(url).header("content-type", "application/json").send(&job.body[..]) {
        Ok(r) if r.status().is_success() => return None,
        // The site refused it (not a known key, bad request): trying again can't help.
        Ok(r) if r.status().is_client_error() && r.status().as_u16() != 429 => {
            crate::log("engine", format!("alert email to {to} refused ({})", r.status().as_u16()));
            return None;
        }
        Ok(r) => r.status().as_u16().to_string(),
        // ureq's errors never carry the request body, so never the key.
        Err(e) => e.to_string(),
    };
    job.attempt += 1;
    if job.attempt > RETRY_AFTER.len() {
        crate::log("engine", format!("alert email to {to} failed ({failed}); given up"));
        return None;
    }
    crate::log("engine", format!("alert email to {to} failed ({failed}); will retry"));
    Some(job)
}
