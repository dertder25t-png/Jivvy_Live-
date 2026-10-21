//! The license key and the 30-day trial, checked on this computer with no network.
//!
//! Same format as `packages/protocol/src/license.ts` (fixtures/license.json): `JL1.<payload>.<sig>`,
//! an Ed25519 signature by live.jivvy.org over what the church bought. Only the public key is
//! here. Nothing in this file ever stops a service or phones home: a missing or expired key only
//! gives a calm `notice`, and only outside a service (`Engine::in_service`).
//!
//! Files in the data dir: `license-trial.json` (when this computer's trial started, written on
//! first run) and `license.key` (the key as pasted). Never log the key.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// The public half of the test key in packages/protocol/fixtures/license.json, whose private
/// half is published: debug builds and tests only.
pub const TEST_PUBLIC_KEY: &str = "wByFzMtLdt_jsvriieoeMsBjJzQfr6GZUo-NXPwDhyI";
/// What the signature is made over, before the payload text (the TypeScript `LICENSE_SIGNING_PREFIX`).
const SIGNING_PREFIX: &str = "jivvy-live license v1\n";
/// How long the trial lasts (the TypeScript `TRIAL_MS`).
pub const TRIAL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// The trial notice starts this many days before the end.
const TRIAL_NOTICE_DAYS: u64 = 7;

/// The key that checks licenses: `JIVVY_LICENSE_PUBLIC_KEY` at build time (release builds), else
/// the test key in debug builds. A release build without one takes no keys; it still runs every
/// service, on the trial's terms.
pub fn public_key() -> Option<&'static str> {
    match option_env!("JIVVY_LICENSE_PUBLIC_KEY") {
        Some(k) => Some(k),
        None if cfg!(debug_assertions) => Some(TEST_PUBLIC_KEY),
        None => None,
    }
}

/// What a key says (the TypeScript `LicensePayload`); times are ms since the epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub church: String,
    pub plan: String,
    pub updates_until: u64,
    pub plus_until: u64,
    pub issued: u64,
}

/// The key's payload if the signature is good for `public_key` (raw 32 bytes, base64url).
pub fn verify(key: &str, public_key: &str) -> Option<Payload> {
    if !crate::protocol::is_license_key_shape(key) {
        return None;
    }
    let (body, sig) = key.strip_prefix("JL1.")?.split_once('.')?;
    let public = URL_SAFE_NO_PAD.decode(public_key).ok()?;
    let sig = URL_SAFE_NO_PAD.decode(sig).ok()?;
    let message = format!("{SIGNING_PREFIX}{body}");
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public).verify(message.as_bytes(), &sig).ok()?;
    let payload: Payload = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    let church_ok = (1..=64).contains(&payload.church.len())
        && payload.church.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let plan_ok = (1..=32).contains(&payload.plan.len())
        && payload.plan.starts_with(|c: char| c.is_ascii_lowercase())
        && payload.plan.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    // JavaScript numbers: the site can't have meant anything past 2^53.
    let max = (1u64 << 53) - 1;
    let times_ok = [payload.updates_until, payload.plus_until, payload.issued].iter().all(|t| *t <= max);
    (church_ok && plan_ok && times_ok).then_some(payload)
}

/// What license.get and license.set answer (the TypeScript `LicenseStatus`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    /// "trial", "trial_ended" or "licensed".
    pub state: &'static str,
    pub trial_started_at: u64,
    pub trial_ends_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plus_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Trial {
    started_at: u64,
}

pub enum SetError {
    /// The signature isn't good (or this build has no key to check it with).
    Invalid,
    Save(std::io::Error),
}

/// The license on this computer.
pub struct License {
    dir: PathBuf,
    public_key: Option<String>,
    trial_started_at: u64,
    key: Option<(String, Payload)>,
}

impl License {
    /// Reads the trial and key from `dir`, starting the trial on first run.
    pub fn load(dir: &Path, public_key: Option<&str>, now_ms: u64) -> License {
        let trial_path = dir.join("license-trial.json");
        let saved = std::fs::read(&trial_path).ok().and_then(|b| serde_json::from_slice::<Trial>(&b).ok());
        let trial_started_at = match saved {
            Some(t) => t.started_at,
            None => {
                let bytes = serde_json::to_vec(&Trial { started_at: now_ms }).unwrap_or_default();
                if let Err(e) = crate::write_atomic(&trial_path, &bytes) {
                    crate::log("license", format!("can't save the trial start ({e}); it starts again next run"));
                }
                now_ms
            }
        };
        let mut license =
            License { dir: dir.to_path_buf(), public_key: public_key.map(String::from), trial_started_at, key: None };
        if let Ok(text) = std::fs::read_to_string(dir.join("license.key")) {
            let text = text.trim().to_string();
            match license.check(&text) {
                Some(p) => license.key = Some((text, p)),
                None => crate::log("license", "the saved license key doesn't check out with this build; ignoring it"),
            }
        }
        license
    }

    fn check(&self, key: &str) -> Option<Payload> {
        verify(key, self.public_key.as_deref()?)
    }

    /// Saves `key` if its signature is good. A bad key leaves the saved one as it was.
    pub fn set(&mut self, key: &str) -> Result<&Payload, SetError> {
        let payload = self.check(key).ok_or(SetError::Invalid)?;
        crate::write_atomic(&self.dir.join("license.key"), key.as_bytes()).map_err(SetError::Save)?;
        crate::log("license", format!("license saved for church {} ({})", payload.church, payload.plan));
        Ok(&self.key.insert((key.to_string(), payload)).1)
    }

    /// The license and trial at `now_ms`. `in_service`: no notice at all.
    pub fn status(&self, now_ms: u64, in_service: bool) -> LicenseStatus {
        let trial_ends_at = self.trial_started_at.saturating_add(TRIAL_MS);
        let mut s = LicenseStatus {
            state: "trial",
            trial_started_at: self.trial_started_at,
            trial_ends_at,
            key: None,
            license: None,
            updates_active: None,
            plus_active: None,
            notice: None,
        };
        let notice = match &self.key {
            Some((key, p)) => {
                s.state = "licensed";
                s.key = Some(key.clone());
                s.license = Some(p.clone());
                s.updates_active = Some(now_ms < p.updates_until);
                s.plus_active = Some(now_ms < p.plus_until);
                if now_ms >= p.plus_until {
                    Some("Plus has ended. Everything on this computer keeps working; renew at live.jivvy.org to turn the cloud features back on.".to_string())
                } else if now_ms >= p.updates_until {
                    Some("Your years of new features have ended. Everything you have keeps working; renew at live.jivvy.org for what's new.".to_string())
                } else {
                    None
                }
            }
            None if now_ms >= trial_ends_at => {
                s.state = "trial_ended";
                Some("Your free trial has ended. Services keep running as normal; get a license at live.jivvy.org whenever you're ready.".to_string())
            }
            None => {
                let days = (trial_ends_at - now_ms).div_ceil(DAY_MS);
                (days <= TRIAL_NOTICE_DAYS).then(|| {
                    let unit = if days == 1 { "day" } else { "days" };
                    format!("Your free trial ends in {days} {unit}. Nothing stops during a service; get a license at live.jivvy.org.")
                })
            }
        };
        s.notice = notice.filter(|_| !in_service);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FIXTURE: &str = include_str!("../../../packages/protocol/fixtures/license.json");

    #[test]
    fn a_signed_key_verifies_and_a_tampered_one_fails() {
        let f: Value = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(f["publicKey"], TEST_PUBLIC_KEY);
        let payload = verify(f["key"].as_str().unwrap(), TEST_PUBLIC_KEY).expect("the fixture's key verifies");
        assert_eq!(serde_json::to_value(&payload).unwrap(), f["payload"]);
        for bad in f["bad"].as_array().unwrap() {
            assert_eq!(verify(bad["key"].as_str().unwrap(), TEST_PUBLIC_KEY), None, "{}", bad["name"]);
        }
    }

    #[test]
    fn the_trial_starts_on_first_run_and_a_key_replaces_it() {
        let dir = std::env::temp_dir().join(format!("jivvy-license-{}", crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let f: Value = serde_json::from_str(FIXTURE).unwrap();
        let start = 1_792_540_800_000;
        let lic = License::load(&dir, Some(TEST_PUBLIC_KEY), start);
        assert_eq!(lic.status(start, false).state, "trial");
        // Restarting later keeps the same trial.
        let mut lic = License::load(&dir, Some(TEST_PUBLIC_KEY), start + 40 * DAY_MS);
        let ended = lic.status(start + 40 * DAY_MS, false);
        assert_eq!((ended.state, ended.trial_started_at), ("trial_ended", start));
        assert!(ended.notice.is_some());
        assert_eq!(lic.status(start + 40 * DAY_MS, true).notice, None, "no notice during a service");
        assert!(lic.set(f["bad"][0]["key"].as_str().unwrap()).is_err());
        assert!(lic.set(f["key"].as_str().unwrap()).is_ok());
        let lic = License::load(&dir, Some(TEST_PUBLIC_KEY), start + 40 * DAY_MS);
        let s = lic.status(start + 40 * DAY_MS, false);
        assert_eq!((s.state, s.plus_active, s.notice), ("licensed", Some(true), None));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
