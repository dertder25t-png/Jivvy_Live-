//! Alerts to the tech lead by web push (RFC 8030, payload encrypted per RFC 8291, signed with
//! VAPID per RFC 8292), so they arrive even with no Jivvy screen open.
//!
//! The church computer sends the push itself. It needs the internet only to deliver it: one
//! worker thread delivers from a bounded queue, retries a few times, and a failure is logged
//! and dropped. Nothing here ever holds up the service: the engine only queues, and saving
//! the subscriptions (flushed to disk) happens on the worker too, off the engine's lock.
//!
//! The VAPID key is made once and kept in the data dir (`vapid-key.p8`), like the daemon id;
//! it never goes in a log. Subscriptions are kept per paired device (`push.json`), one each and
//! `MAX_SUBSCRIPTIONS` in all; removing the device drops its subscription, and so does the push
//! service saying it is gone. Pushes only go to the known push services' hosts
//! (`protocol::is_push_endpoint`), checked when subscribing and again when sending. A push
//! carries only the alert's words and a tag.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use web_push_native::{Auth, WebPushBuilder, p256};

use crate::protocol::is_push_endpoint;

/// Most devices that can get alerts.
pub const MAX_SUBSCRIPTIONS: usize = 20;
/// Pushes waiting for the worker (new and retrying); more are dropped and logged.
const QUEUE: usize = 64;
/// How long a push service keeps trying to reach the phone: an alert hours late is noise.
const TTL: Duration = Duration::from_secs(15 * 60);
/// Waits before each retry of a push that didn't go through.
const RETRY_AFTER: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)];
/// How soon a failed save of `push.json` is tried again.
const SAVE_RETRY: Duration = Duration::from_secs(5);
/// Who sends these pushes, for the push services (VAPID `sub`).
const CONTACT: &str = "https://live.jivvy.org";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub device_id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

#[derive(Serialize, Deserialize)]
struct PushFile {
    version: u32,
    subscriptions: Vec<Subscription>,
}

/// What a push carries: the alert's words and a tag (its kind, or "test").
#[derive(Serialize)]
struct Payload<'a> {
    message: &'a str,
    tag: &'a str,
}

/// One push to deliver.
struct Job {
    sub: Subscription,
    payload: Vec<u8>,
    /// Where the request goes: the subscription's endpoint, except in tests.
    target: String,
    attempt: usize,
}

enum Msg {
    Deliver(Job),
    Save,
}

/// What the engine and the worker share.
struct Shared {
    path: PathBuf,
    subscriptions: Mutex<Vec<Subscription>>,
    /// Set when the list changed; the worker saves it.
    dirty: AtomicBool,
}

impl Shared {
    fn list(&self) -> std::sync::MutexGuard<'_, Vec<Subscription>> {
        self.subscriptions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Saves the list if it changed (temp file, flush, rename). On the worker, never the engine.
    fn save_if_dirty(&self) {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        let list = self.list().clone();
        let saved = serde_json::to_vec_pretty(&PushFile { version: 1, subscriptions: list })
            .map_err(std::io::Error::other)
            .and_then(|b| crate::devices::save_durably(&self.path, &b));
        if let Err(e) = saved {
            // The list in memory is already right (a removed device gets nothing); the file
            // catches up on the next try.
            crate::log("engine", format!("push.json save failed: {e}; trying again shortly"));
            self.dirty.store(true, Ordering::Release);
        }
    }
}

pub struct Push {
    shared: Arc<Shared>,
    /// None if the key couldn't be made or saved: pushes are then unavailable, nothing else.
    vapid: Option<Arc<EcdsaKeyPair>>,
    worker: SyncSender<Msg>,
    /// Tests send every push here instead of to the push service.
    deliver_to: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum SubscribeError {
    /// The browser's key isn't a P-256 key.
    BadKey,
    /// Not a known push service.
    BadEndpoint,
    /// `MAX_SUBSCRIPTIONS` devices already get alerts.
    Full,
}

impl Push {
    /// Reads the subscriptions and the VAPID key in `dir`, making the key the first time, and
    /// starts the delivery worker.
    pub fn load(dir: &Path) -> Push {
        let path = dir.join("push.json");
        let subscriptions = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<PushFile>(&b).ok())
            .filter(|f| f.version == 1)
            .map(|f| f.subscriptions)
            .unwrap_or_default();
        let vapid = vapid_key(dir)
            .map_err(|e| crate::log("engine", format!("alerts by push are off: no VAPID key ({e})")))
            .ok()
            .map(Arc::new);
        let shared = Arc::new(Shared { path, subscriptions: Mutex::new(subscriptions), dirty: AtomicBool::new(false) });
        let (worker, rx) = sync_channel(QUEUE);
        let (s, key) = (shared.clone(), vapid.clone());
        std::thread::spawn(move || work(&s, key.as_deref(), rx));
        Push { shared, vapid, worker, deliver_to: None }
    }

    /// The VAPID public key browsers subscribe with: raw P-256, base64url.
    pub fn public_key(&self) -> Option<String> {
        self.vapid.as_ref().map(|k| B64.encode(k.public_key().as_ref()))
    }

    /// Keeps `device`'s subscription, replacing any it had; saved by the worker.
    pub fn subscribe(&mut self, device: &str, endpoint: &str, p256dh: &str, auth: &str) -> Result<(), SubscribeError> {
        if !is_push_endpoint(endpoint) {
            return Err(SubscribeError::BadEndpoint);
        }
        if user_agent_key(p256dh).is_none() {
            return Err(SubscribeError::BadKey);
        }
        let mut list = self.shared.list();
        list.retain(|s| s.device_id != device);
        if list.len() >= MAX_SUBSCRIPTIONS {
            return Err(SubscribeError::Full);
        }
        list.push(Subscription {
            device_id: device.into(),
            endpoint: endpoint.into(),
            p256dh: p256dh.into(),
            auth: auth.into(),
        });
        drop(list);
        self.changed();
        Ok(())
    }

    /// Drops `device`'s subscription (the device was removed).
    pub fn forget(&mut self, device: &str) {
        let mut list = self.shared.list();
        let before = list.len();
        list.retain(|s| s.device_id != device);
        let changed = list.len() != before;
        drop(list);
        if changed {
            self.changed();
        }
    }

    /// Keeps only the subscriptions whose device `paired` says is still paired: one removed
    /// while `push.json` couldn't be saved never gets alerts again, even after a restart.
    pub fn keep_only(&mut self, paired: impl Fn(&str) -> bool) {
        let mut list = self.shared.list();
        let before = list.len();
        list.retain(|s| paired(&s.device_id));
        let changed = list.len() != before;
        drop(list);
        if changed {
            self.changed();
        }
    }

    pub fn has(&self, device: &str) -> bool {
        self.shared.list().iter().any(|s| s.device_id == device)
    }

    /// Queues an alert for every subscribed device.
    pub fn alert(&self, message: &str, tag: &str) {
        let subs = self.shared.list().clone();
        for s in subs {
            self.send(s, message, tag);
        }
    }

    /// Queues the test alert for `device`; false if it has no subscription.
    pub fn test(&self, device: &str) -> bool {
        let found = self.shared.list().iter().find(|s| s.device_id == device).cloned();
        let Some(s) = found else { return false };
        self.send(s, crate::protocol::TEST_ALERT_MESSAGE, "test");
        true
    }

    #[cfg(test)]
    pub(crate) fn deliver_to(&mut self, url: &str) {
        self.deliver_to = Some(url.into());
    }

    fn changed(&self) {
        self.shared.dirty.store(true, Ordering::Release);
        // A full queue still wakes the worker, which saves after whatever it handles next.
        let _ = self.worker.try_send(Msg::Save);
    }

    fn send(&self, sub: Subscription, message: &str, tag: &str) {
        if self.vapid.is_none() {
            return;
        }
        let Ok(payload) = serde_json::to_vec(&Payload { message, tag }) else { return };
        let target = self.deliver_to.clone().unwrap_or_else(|| sub.endpoint.clone());
        let to = host(&sub.endpoint).to_string();
        match self.worker.try_send(Msg::Deliver(Job { sub, payload, target, attempt: 0 })) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => crate::log("engine", format!("push to {to} dropped: too many waiting")),
            Err(TrySendError::Disconnected(_)) => crate::log("engine", format!("push to {to} dropped: no worker")),
        }
    }
}

/// The delivery worker: one push at a time, retries waiting their turn in a bounded list.
fn work(shared: &Shared, key: Option<&EcdsaKeyPair>, rx: Receiver<Msg>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(format!("Jivvy / Jivvy Live / {}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut retries: Vec<(Instant, Job)> = Vec::new();
    loop {
        let save_retry = shared.dirty.load(Ordering::Acquire).then_some(SAVE_RETRY);
        let wait = retries.iter().map(|(due, _)| due.saturating_duration_since(Instant::now())).chain(save_retry).min();
        let msg = match wait {
            Some(w) => rx.recv_timeout(w),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(Msg::Deliver(job)) => retries.push((Instant::now(), job)),
            Ok(Msg::Save) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                shared.save_if_dirty();
                return;
            }
        }
        let now = Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = retries.drain(..).partition(|(at, _)| *at <= now);
        retries = later;
        for (_, job) in due {
            let Some(key) = key else { continue };
            if let Some(next) = deliver(&agent, key, shared, job) {
                if retries.len() >= QUEUE {
                    crate::log("engine", format!("push to {} dropped: too many waiting", host(&next.sub.endpoint)));
                    continue;
                }
                retries.push((Instant::now() + RETRY_AFTER[next.attempt - 1], next));
            }
        }
        shared.save_if_dirty();
    }
}

/// The push service's host, for logs: the rest of an endpoint is the phone's address.
fn host(endpoint: &str) -> &str {
    let rest = endpoint.split_once("://").map_or(endpoint, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("")
}

/// Sends one push; the job back if it should be tried again.
fn deliver(agent: &ureq::Agent, key: &EcdsaKeyPair, shared: &Shared, mut job: Job) -> Option<Job> {
    let to = host(&job.sub.endpoint).to_string();
    // Checked again here: push.json is a file, and only push services get our requests.
    if !is_push_endpoint(&job.sub.endpoint) {
        crate::log("engine", format!("push to {to} not sent: not a known push service"));
        return None;
    }
    // Built afresh each time: new encryption keys and a fresh VAPID signature.
    let request = match request(key, &job.sub, &job.payload, &job.target) {
        Ok(r) => r,
        Err(e) => {
            crate::log("engine", format!("push to {to} not sent: {e}"));
            return None;
        }
    };
    let failed = match agent.run(request) {
        Ok(r) if r.status().is_success() => return None,
        // The phone unsubscribed or the subscription expired: retrying can't help.
        Ok(r) if matches!(r.status().as_u16(), 404 | 410) => {
            crate::log(
                "engine",
                format!("push to {to}: subscription gone; dropped (turn alerts on again on that device)"),
            );
            let mut list = shared.list();
            let before = list.len();
            list.retain(|s| s.endpoint != job.sub.endpoint);
            if list.len() != before {
                shared.dirty.store(true, Ordering::Release);
            }
            return None;
        }
        Ok(r) if r.status().is_client_error() && r.status().as_u16() != 429 => {
            crate::log("engine", format!("push to {to} refused ({})", r.status().as_u16()));
            return None;
        }
        Ok(r) => r.status().as_u16().to_string(),
        Err(e) => e.to_string(),
    };
    job.attempt += 1;
    if job.attempt > RETRY_AFTER.len() {
        crate::log("engine", format!("push to {to} failed ({failed}); given up"));
        return None;
    }
    crate::log("engine", format!("push to {to} failed ({failed}); will retry"));
    Some(job)
}

fn user_agent_key(p256dh: &str) -> Option<p256::PublicKey> {
    p256::PublicKey::from_sec1_bytes(&B64.decode(p256dh).ok()?).ok()
}

/// The encrypted push request for `s`, sent to `target` (its endpoint, except in tests).
fn request(
    key: &EcdsaKeyPair,
    s: &Subscription,
    payload: &[u8],
    target: &str,
) -> Result<ureq::http::Request<Vec<u8>>, String> {
    let ua_key = user_agent_key(&s.p256dh).ok_or("bad p256dh")?;
    let auth = B64.decode(&s.auth).ok().filter(|a| a.len() == 16).ok_or("bad auth")?;
    let uri: ureq::http::Uri = target.parse().map_err(|_| "bad endpoint")?;
    let mut request = WebPushBuilder::new(uri, ua_key, Auth::clone_from_slice(&auth))
        .with_valid_duration(TTL)
        .build(payload)
        .map_err(|e| e.to_string())?;
    let vapid = vapid_header(key, &s.endpoint)?;
    let headers = request.headers_mut();
    headers.insert("authorization", vapid.parse().map_err(|_| "bad VAPID header")?);
    headers.insert("urgency", ureq::http::HeaderValue::from_static("high"));
    Ok(request)
}

/// `vapid t=<JWT>, k=<public key>` for a push to `endpoint` (RFC 8292): ES256 over the
/// push service's origin, valid for 12 hours.
fn vapid_header(key: &EcdsaKeyPair, endpoint: &str) -> Result<String, String> {
    let uri: ureq::http::Uri = endpoint.parse().map_err(|_| "bad endpoint")?;
    let (Some(scheme), Some(host)) = (uri.scheme_str(), uri.host()) else { return Err("bad endpoint".into()) };
    let aud = format!("{scheme}://{host}");
    let exp = crate::now_ms() / 1000 + 12 * 60 * 60;
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = B64.encode(serde_json::json!({ "aud": aud, "exp": exp, "sub": CONTACT }).to_string());
    let signed = format!("{header}.{claims}");
    let sig = key.sign(&SystemRandom::new(), signed.as_bytes()).map_err(|_| "signing failed")?;
    Ok(format!("vapid t={signed}.{}, k={}", B64.encode(sig.as_ref()), B64.encode(key.public_key().as_ref())))
}

/// This church computer's VAPID key (PKCS#8), made once and kept in `dir`.
fn vapid_key(dir: &Path) -> std::io::Result<EcdsaKeyPair> {
    let path = dir.join("vapid-key.p8");
    let rng = SystemRandom::new();
    if let Ok(bytes) = fs::read(&path)
        && let Ok(key) = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &bytes, &rng)
    {
        return Ok(key);
    }
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|_| std::io::Error::other("can't make a key"))?;
    crate::devices::save_durably(&path, pkcs8.as_ref())?;
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| std::io::Error::other("can't read the key just made"))
}
