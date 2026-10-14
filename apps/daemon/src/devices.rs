//! Paired devices: the phones, tablets and computers allowed on the network channel.
//!
//! A device is its P-256 public key; there are no passwords, and nothing here would let
//! anyone who reads the file act as a device. The list is saved durably (temp file, flush,
//! rename), like the state snapshot, so a crash or power cut never loses a pairing. A file
//! that can't be read is moved aside and no device is trusted until devices pair again:
//! a broken file never lets anyone in.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub device_id: String,
    pub name: String,
    /// Raw uncompressed P-256 key, base64url.
    pub public_key: String,
    /// ms since epoch
    pub paired_at: u64,
}

#[derive(Serialize, Deserialize)]
struct DevicesFile {
    version: u32,
    devices: Vec<Device>,
}

pub struct Devices {
    path: PathBuf,
    list: Vec<Device>,
}

impl Devices {
    /// Reads `devices.json` in `dir`; none yet means no devices.
    pub fn load(dir: &Path) -> Devices {
        let path = dir.join("devices.json");
        let list = match fs::read(&path) {
            Err(_) => Vec::new(),
            Ok(bytes) => match serde_json::from_slice::<DevicesFile>(&bytes) {
                Ok(f) if f.version == 1 => f.devices,
                _ => {
                    let aside = dir.join(format!("devices.json.unreadable-{}", crate::now_ms()));
                    let _ = fs::rename(&path, &aside);
                    crate::log(
                        "engine",
                        format!("devices.json unreadable; moved to {}; pair devices again", aside.display()),
                    );
                    Vec::new()
                }
            },
        };
        Devices { path, list }
    }

    pub fn list(&self) -> &[Device] {
        &self.list
    }

    pub fn by_key(&self, public_key: &str) -> Option<&Device> {
        self.list.iter().find(|d| d.public_key == public_key)
    }

    pub fn contains_id(&self, device_id: &str) -> bool {
        self.list.iter().any(|d| d.device_id == device_id)
    }

    /// Pairs a key (pairing the same key again renames it and keeps its id). Saved before it
    /// counts: if the save fails nothing changes.
    pub fn add(&mut self, name: &str, public_key: &str, now_ms: u64) -> std::io::Result<String> {
        let device_id = self.by_key(public_key).map(|d| d.device_id.clone()).unwrap_or_else(|| random_id(12));
        let mut next: Vec<Device> = self.list.iter().filter(|d| d.public_key != public_key).cloned().collect();
        next.push(Device {
            device_id: device_id.clone(),
            name: name.into(),
            public_key: public_key.into(),
            paired_at: now_ms,
        });
        self.save(&next)?;
        self.list = next;
        Ok(device_id)
    }

    /// Removes a device; false if there was none with that id.
    pub fn remove(&mut self, device_id: &str) -> std::io::Result<bool> {
        if !self.contains_id(device_id) {
            return Ok(false);
        }
        let next: Vec<Device> = self.list.iter().filter(|d| d.device_id != device_id).cloned().collect();
        self.save(&next)?;
        self.list = next;
        Ok(true)
    }

    fn save(&self, list: &[Device]) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&DevicesFile { version: 1, devices: list.to_vec() })?;
        save_durably(&self.path, &bytes)
    }
}

/// Writes a temp file, flushes it to disk and renames it into place.
fn save_durably(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(d) = path.parent().and_then(|p| File::open(p).ok()) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// This church computer's id, made once and kept in `dir`. Devices sign it into every
/// proof, so a proof for one church computer is useless on another.
pub fn daemon_id(dir: &Path) -> std::io::Result<String> {
    let path = dir.join("daemon-id");
    if let Ok(id) = fs::read_to_string(&path) {
        let id = id.trim();
        if id.len() >= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Ok(id.to_string());
        }
    }
    let id = random_id(16);
    save_durably(&path, id.as_bytes())?;
    Ok(id)
}

/// `bytes` random bytes, base64url.
pub fn random_id(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buf).expect("the system random number generator failed");
    B64.encode(buf)
}

/// 6 digits, every code equally likely.
pub fn random_code() -> String {
    let rng = SystemRandom::new();
    loop {
        let mut b = [0u8; 4];
        rng.fill(&mut b).expect("the system random number generator failed");
        let n = u32::from_le_bytes(b);
        if n < 4_294_000_000 {
            // a multiple of 1,000,000
            return format!("{:06}", n % 1_000_000);
        }
    }
}

/// ECDSA P-256 / SHA-256 (r || s) over `message`; false for a bad key or signature.
pub fn verify(public_key: &str, message: &str, signature: &str) -> bool {
    let (Ok(key), Ok(sig)) = (B64.decode(public_key), B64.decode(signature)) else { return false };
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, key)
        .verify(message.as_bytes(), &sig)
        .is_ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};

    /// A device key as the web app makes one: (public key base64url, signer).
    pub(crate) struct TestKey(EcdsaKeyPair);

    impl TestKey {
        pub(crate) fn new() -> TestKey {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            TestKey(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap())
        }
        pub(crate) fn public(&self) -> String {
            B64.encode(self.0.public_key().as_ref())
        }
        pub(crate) fn sign(&self, message: &str) -> String {
            B64.encode(self.0.sign(&SystemRandom::new(), message.as_bytes()).unwrap().as_ref())
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jivvy-devices-{name}-{}-{}", std::process::id(), crate::now_ms()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn signatures_verify_only_with_the_right_key_and_message() {
        let (key, other) = (TestKey::new(), TestKey::new());
        assert_eq!(key.public().len(), 87, "the protocol's key shape");
        let sig = key.sign("hello");
        assert_eq!(sig.len(), 86, "the protocol's signature shape");
        assert!(verify(&key.public(), "hello", &sig));
        assert!(!verify(&key.public(), "hellp", &sig));
        assert!(!verify(&other.public(), "hello", &sig));
        assert!(!verify(&("B".to_string() + &"A".repeat(86)), "hello", &sig), "not a point on the curve");
        assert!(!verify("!!", "hello", &sig));
    }

    #[test]
    fn devices_survive_a_reload_and_pairing_again_keeps_the_id() {
        let dir = temp("reload");
        let key = TestKey::new();
        let mut d = Devices::load(&dir);
        let id = d.add("Phone", &key.public(), 5).unwrap();
        assert_eq!(d.add("Sam's phone", &key.public(), 6).unwrap(), id);
        let d = Devices::load(&dir);
        assert_eq!(d.list().len(), 1);
        assert_eq!(d.by_key(&key.public()).unwrap().name, "Sam's phone");
        let mut d = d;
        assert!(d.remove(&id).unwrap());
        assert!(!d.remove(&id).unwrap());
        assert!(Devices::load(&dir).list().is_empty());
    }

    #[test]
    fn an_unreadable_file_trusts_nobody_and_is_kept_aside() {
        let dir = temp("corrupt");
        fs::write(dir.join("devices.json"), b"{ not json").unwrap();
        assert!(Devices::load(&dir).list().is_empty());
        let aside = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("devices.json.unreadable-"));
        assert!(aside, "the broken file is kept for a person to look at");
    }

    #[test]
    fn the_daemon_id_is_made_once_and_kept() {
        let dir = temp("id");
        let id = daemon_id(&dir).unwrap();
        assert!(id.len() >= 16);
        assert_eq!(daemon_id(&dir).unwrap(), id);
    }

    #[test]
    fn codes_are_six_digits() {
        for _ in 0..200 {
            let c = random_code();
            assert!(c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit()), "{c}");
        }
    }
}
