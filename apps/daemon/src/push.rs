//! Alerts to the tech lead by web push (RFC 8030, payload encrypted per RFC 8291, signed with
//! VAPID per RFC 8292), so they arrive even with no Jivvy screen open.
//!
//! The church computer sends the push itself. It needs the internet only to deliver it: each
//! push goes out on its own thread, is retried a few times, and a failure is logged and
//! dropped. Nothing here ever holds up the service.
//!
//! The VAPID key is made once and kept in the data dir (`vapid-key.p8`), like the daemon id;
//! it never goes in a log. Subscriptions are kept per paired device (`push.json`); removing
//! the device drops its subscription. A push carries only the alert's words and a tag.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use web_push_native::{Auth, WebPushBuilder, p256};

/// How long a push service keeps trying to reach the phone: an alert hours late is noise.
const TTL: Duration = Duration::from_secs(15 * 60);
/// Waits before each retry of a push that didn't go through.
const RETRY_AFTER: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)];
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

pub struct Push {
    path: PathBuf,
    subscriptions: Vec<Subscription>,
    /// None if the key couldn't be made or saved: pushes are then unavailable, nothing else.
    vapid: Option<std::sync::Arc<EcdsaKeyPair>>,
    /// Tests send every push here instead of to the push service.
    deliver_to: Option<String>,
}

impl Push {
    /// Reads the subscriptions and the VAPID key in `dir`, making the key the first time.
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
            .map(std::sync::Arc::new);
        Push { path, subscriptions, vapid, deliver_to: None }
    }

    /// The VAPID public key browsers subscribe with: raw P-256, base64url.
    pub fn public_key(&self) -> Option<String> {
        self.vapid.as_ref().map(|k| B64.encode(k.public_key().as_ref()))
    }

    /// Keeps `device`'s subscription, replacing any it had. Saved before it counts.
    pub fn subscribe(&mut self, device: &str, endpoint: &str, p256dh: &str, auth: &str) -> std::io::Result<()> {
        if user_agent_key(p256dh).is_none() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a P-256 key"));
        }
        let mut next: Vec<Subscription> =
            self.subscriptions.iter().filter(|s| s.device_id != device).cloned().collect();
        next.push(Subscription {
            device_id: device.into(),
            endpoint: endpoint.into(),
            p256dh: p256dh.into(),
            auth: auth.into(),
        });
        self.save(next)
    }

    /// Drops `device`'s subscription (the device was removed).
    pub fn forget(&mut self, device: &str) {
        if !self.has(device) {
            return;
        }
        let next = self.subscriptions.iter().filter(|s| s.device_id != device).cloned().collect();
        if let Err(e) = self.save(next) {
            crate::log("engine", format!("push.json save failed: {e}"));
        }
    }

    pub fn has(&self, device: &str) -> bool {
        self.subscriptions.iter().any(|s| s.device_id == device)
    }

    /// Pushes an alert to every subscribed device.
    pub fn alert(&self, message: &str, tag: &str) {
        for s in &self.subscriptions {
            self.send(s, message, tag);
        }
    }

    /// Pushes the test alert to `device`; false if it has no subscription.
    pub fn test(&self, device: &str) -> bool {
        let Some(s) = self.subscriptions.iter().find(|s| s.device_id == device) else { return false };
        self.send(s, crate::protocol::TEST_ALERT_MESSAGE, "test");
        true
    }

    #[cfg(test)]
    pub(crate) fn deliver_to(&mut self, url: &str) {
        self.deliver_to = Some(url.into());
    }

    fn save(&mut self, next: Vec<Subscription>) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&PushFile { version: 1, subscriptions: next.clone() })?;
        crate::devices::save_durably(&self.path, &bytes)?;
        self.subscriptions = next;
        Ok(())
    }

    /// Delivers one push on its own thread, retrying a few times; never blocks the caller.
    fn send(&self, s: &Subscription, message: &str, tag: &str) {
        let Some(key) = self.vapid.clone() else { return };
        let Ok(payload) = serde_json::to_vec(&Payload { message, tag }) else { return };
        let s = s.clone();
        let target = self.deliver_to.clone().unwrap_or_else(|| s.endpoint.clone());
        std::thread::spawn(move || deliver(&key, &s, &payload, &target));
    }
}

/// The push service's host, for logs: the rest of an endpoint is the phone's address.
fn host(endpoint: &str) -> &str {
    let rest = endpoint.split_once("://").map_or(endpoint, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or("")
}

fn deliver(key: &EcdsaKeyPair, s: &Subscription, payload: &[u8], target: &str) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(format!("Jivvy / Jivvy Live / {}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let to = host(&s.endpoint);
    for attempt in 0..=RETRY_AFTER.len() {
        if attempt > 0 {
            std::thread::sleep(RETRY_AFTER[attempt - 1]);
        }
        // Built afresh each time: new encryption keys and a fresh VAPID signature.
        let request = match request(key, s, payload, target) {
            Ok(r) => r,
            Err(e) => {
                crate::log("engine", format!("push to {to} not sent: {e}"));
                return;
            }
        };
        match agent.run(request) {
            Ok(r) if r.status().is_success() => return,
            // The phone unsubscribed or the subscription expired: retrying can't help.
            Ok(r) if matches!(r.status().as_u16(), 404 | 410) => {
                crate::log("engine", format!("push to {to}: subscription gone; turn alerts on again on that device"));
                return;
            }
            Ok(r) if r.status().is_client_error() && r.status().as_u16() != 429 => {
                crate::log("engine", format!("push to {to} refused ({})", r.status().as_u16()));
                return;
            }
            Ok(r) => crate::log("engine", format!("push to {to} failed ({}); will retry", r.status().as_u16())),
            Err(e) => crate::log("engine", format!("push to {to} failed ({e}); will retry")),
        }
    }
    crate::log("engine", format!("push to {to} given up"));
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
    let aud = match uri.port_u16() {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    };
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
