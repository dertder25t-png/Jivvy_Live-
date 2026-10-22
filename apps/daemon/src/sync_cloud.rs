//! The real cloud for sync: the church's rows in Supabase (`apps/site/supabase/0001_sync.sql`),
//! reached with PostgREST over HTTPS. Plus only, and only from the sync thread, which pauses
//! during a service and never holds the engine's lock across a call (`sync::spawn`).
//!
//! Signing in: church accounts live on live.jivvy.org, not in Supabase. This computer has its
//! own P-256 device key (`sync-device-key.p8` in the data dir, made once). To get a token it
//! asks the site for a single-use nonce, signs it, and trades the signature for a Supabase JWT
//! that lasts about an hour and names its church (`app_metadata.church_id`, which RLS reads).
//! The first time, it also sends the license key, which registers the device key to the
//! license's church; after that the device key alone is enough. Nothing that crosses the
//! network works for more than an hour, or more than once.
//!
//! The site's answer also says where the Supabase project is and its (public) anon key, so
//! nothing about Supabase is built in here. The token lives in memory only and is never
//! logged; errors are plain words, never a URL, a header or a token.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::sync::{FieldChange, SyncError, Transport};

/// Where church computers sign in for sync.
pub const DEFAULT_SITE: &str = "https://live.jivvy.org";
/// What the device key signs, before the nonce (the site's `SYNC_SIGNING_PREFIX`).
pub const SIGNING_PREFIX: &str = "jivvy-live sync token v1\n";
/// A token this close to its end is replaced before the next request.
const REFRESH_BEFORE_MS: u64 = 5 * 60 * 1000;
/// The largest answer read: a pull of 200 fields of up to 512 KB each.
const MAX_ANSWER: u64 = 128 * 1024 * 1024;

/// The current token, where it works, and when it ends. Not `Debug`, so it can't be logged.
struct Token {
    jwt: String,
    expires_at: u64,
    /// The Supabase project's URL, without a trailing slash.
    url: String,
    anon_key: String,
}

/// Supabase through PostgREST: `rpc/sync_push` and a select on `sync_fields` from a cursor.
pub struct SupabaseTransport {
    site: String,
    data_dir: PathBuf,
    key: EcdsaKeyPair,
    public_key: String,
    agent: ureq::Agent,
    token: Option<Token>,
}

/// https, or plain http to this computer only (local test servers).
fn usable(url: &str) -> bool {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://").filter(|r| is_loopback(r)));
    rest.is_some_and(|r| !r.is_empty() && !r.contains(['?', '#', ' ']))
}

fn is_loopback(host_and_path: &str) -> bool {
    let host = host_and_path.split(['/', ':']).next().unwrap_or_default();
    host == "127.0.0.1" || host == "localhost"
}

/// This computer's sync device key (PKCS#8), made once and kept in `dir`.
fn device_key(dir: &Path) -> std::io::Result<EcdsaKeyPair> {
    let path = dir.join("sync-device-key.p8");
    let rng = SystemRandom::new();
    if let Ok(bytes) = std::fs::read(&path)
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

/// A row of `sync_fields` as PostgREST sends it.
#[derive(Deserialize)]
struct Row {
    seq: u64,
    kind: String,
    doc_id: String,
    field: String,
    value: Value,
    hlc: String,
}

/// The site's `error`, if it is short plain words (it never holds a secret, but stay careful).
fn site_error(body: &Value) -> String {
    let e = body["error"].as_str().unwrap_or("no reason given");
    if e.len() <= 200 && e.chars().all(|c| !c.is_control()) { e.to_string() } else { "no reason given".to_string() }
}

impl SupabaseTransport {
    /// `site`: where to sign in (`DEFAULT_SITE`, or a local stand-in in tests).
    pub fn new(site: &str, data_dir: &Path) -> std::io::Result<Self> {
        let site = site.trim_end_matches('/').to_string();
        if !usable(&site) {
            return Err(std::io::Error::other("the sync sign-in address must be https"));
        }
        let key = device_key(data_dir)?;
        let public_key = B64.encode(key.public_key().as_ref());
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(format!("Jivvy / Jivvy Live / {}", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(SupabaseTransport { site, data_dir: data_dir.to_path_buf(), key, public_key, agent, token: None })
    }

    /// Sends one request (POST with a JSON body, else GET) and reads its JSON answer. Any failure to connect or read is `Offline`;
    /// the URL and headers never reach an error.
    fn send(&self, url: &str, headers: &[(&str, &str)], body: Option<&Value>) -> Result<(u16, Value), SyncError> {
        let result = match body {
            Some(b) => {
                let bytes = serde_json::to_vec(b).map_err(|e| SyncError::Local(e.to_string()))?;
                let mut req = self.agent.post(url).header("Content-Type", "application/json");
                for (k, v) in headers {
                    req = req.header(*k, *v);
                }
                req.send(&bytes[..])
            }
            None => {
                let mut req = self.agent.get(url);
                for (k, v) in headers {
                    req = req.header(*k, *v);
                }
                req.call()
            }
        };
        let resp = result.map_err(|_| SyncError::Offline)?;
        let status = resp.status().as_u16();
        let bytes = resp.into_body().with_config().limit(MAX_ANSWER).read_to_vec().map_err(|_| SyncError::Offline)?;
        let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
        Ok((status, value))
    }

    /// Signs in at the site: a fresh nonce, signed with the device key, for a token. The
    /// license key goes along only when the site doesn't know this device yet.
    fn sign_in(&mut self) -> Result<(), SyncError> {
        let mut license: Option<String> = None;
        loop {
            let (status, answer) = self.send(&format!("{}/api/sync/nonce", self.site), &[], Some(&json!({})))?;
            let nonce = match answer["nonce"].as_str() {
                Some(n) if status == 200 => n.to_string(),
                _ => return Err(SyncError::Cloud(format!("live.jivvy.org refused to sign in ({status})"))),
            };
            let message = format!("{SIGNING_PREFIX}{nonce}\n{}", self.public_key);
            let signature = self
                .key
                .sign(&SystemRandom::new(), message.as_bytes())
                .map_err(|_| SyncError::Local("can't sign with the device key".into()))?;
            let mut body =
                json!({ "nonce": nonce, "publicKey": self.public_key, "signature": B64.encode(signature.as_ref()) });
            if let Some(l) = &license {
                body["licenseKey"] = Value::String(l.clone());
            }
            let (status, answer) = self.send(&format!("{}/api/sync/token", self.site), &[], Some(&body))?;
            if status == 200 {
                let token = (|| {
                    let url = answer["supabaseUrl"].as_str()?.trim_end_matches('/').to_string();
                    usable(&url).then_some(())?;
                    Some(Token {
                        jwt: answer["token"].as_str()?.to_string(),
                        expires_at: answer["expiresAt"].as_u64()?,
                        url,
                        anon_key: answer["supabaseAnonKey"].as_str()?.to_string(),
                    })
                })();
                return match token {
                    Some(t) => {
                        self.token = Some(t);
                        Ok(())
                    }
                    None => Err(SyncError::Cloud("live.jivvy.org sent a sign-in this computer can't use".into())),
                };
            }
            let why = site_error(&answer);
            if why == "unknown_device" && license.is_none() {
                // First sign-in from this computer: register it with the church's license key.
                let saved = std::fs::read_to_string(self.data_dir.join("license.key")).unwrap_or_default();
                let key = saved.trim();
                if key.is_empty() {
                    return Err(SyncError::Cloud("there's no license key on this computer to sync with".into()));
                }
                license = Some(key.to_string());
                continue;
            }
            return Err(SyncError::Cloud(if why == "no_plus" {
                "Plus isn't active for this church at live.jivvy.org".to_string()
            } else {
                format!("live.jivvy.org refused to sign in: {why}")
            }));
        }
    }

    /// A PostgREST request with a current token, signing in again first if it ends soon, and
    /// once more if Supabase turns the token down.
    fn rest(&mut self, path: &str, body: Option<&Value>) -> Result<Value, SyncError> {
        for attempt in 0..2 {
            if self.token.as_ref().is_none_or(|t| t.expires_at <= crate::now_ms() + REFRESH_BEFORE_MS) {
                self.token = None;
                self.sign_in()?;
            }
            let Some(t) = &self.token else { return Err(SyncError::Offline) };
            let url = format!("{}/rest/v1/{path}", t.url);
            let auth = format!("Bearer {}", t.jwt);
            let headers =
                [("apikey", t.anon_key.as_str()), ("Authorization", auth.as_str()), ("Accept", "application/json")];
            let (status, answer) = self.send(&url, &headers, body)?;
            match status {
                200..=299 => return Ok(answer),
                401 | 403 if attempt == 0 => self.token = None,
                401 | 403 => return Err(SyncError::Cloud("the cloud refused this computer's sign-in".into())),
                s => return Err(SyncError::Cloud(format!("the cloud answered {s}"))),
            }
        }
        Err(SyncError::Cloud("the cloud refused this computer's sign-in".into()))
    }
}

impl Transport for SupabaseTransport {
    fn push(&mut self, changes: &[FieldChange]) -> Result<Vec<FieldChange>, SyncError> {
        let answer = self.rest("rpc/sync_push", Some(&json!({ "changes": changes })))?;
        serde_json::from_value(answer)
            .map_err(|_| SyncError::Cloud("the cloud's answer to a push didn't make sense".into()))
    }

    fn pull(&mut self, cursor: u64, limit: usize) -> Result<Vec<(u64, FieldChange)>, SyncError> {
        let path =
            format!("sync_fields?select=seq,kind,doc_id,field,value,hlc&seq=gt.{cursor}&order=seq.asc&limit={limit}");
        let rows: Vec<Row> = serde_json::from_value(self.rest(&path, None)?)
            .map_err(|_| SyncError::Cloud("the cloud's answer to a pull didn't make sense".into()))?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.seq,
                    FieldChange { kind: r.kind, id: r.doc_id, field: r.field, value: r.value, hlc: r.hlc, base: None },
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use crate::sync::{MemoryCloud, Syncer};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// What the stand-in saw, to check the sign-in rules.
    #[derive(Default)]
    struct Seen {
        nonces: Vec<String>,
        devices: Vec<(String, String)>,
        license_sent: usize,
        tokens: Vec<String>,
    }

    /// A local stand-in for live.jivvy.org's two sync endpoints and Supabase's two PostgREST
    /// ones, with a MemoryCloud behind them (the same rules as 0001_sync.sql).
    fn stand_in(cloud: MemoryCloud, seen: Arc<Mutex<Seen>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let supabase = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
                let (mut length, mut auth, mut apikey) = (0usize, String::new(), String::new());
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    match k.trim().to_ascii_lowercase().as_str() {
                        "content-length" => length = v.trim().parse().unwrap(),
                        "authorization" => auth = v.trim().to_string(),
                        "apikey" => apikey = v.trim().to_string(),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let mut s = seen.lock().unwrap();
                let signed_in = apikey == "anon-test" && s.tokens.iter().any(|t| auth == format!("Bearer {t}"));
                let mut cloud = cloud.clone();
                let (status, answer) = match (method.as_str(), target.split('?').next().unwrap()) {
                    ("POST", "/api/sync/nonce") => {
                        let nonce = format!("n{}", s.nonces.len());
                        s.nonces.push(nonce);
                        (200, json!({ "nonce": s.nonces.last() }))
                    }
                    ("POST", "/api/sync/token") => {
                        let (nonce, key, sig) = (
                            body["nonce"].as_str().unwrap_or(""),
                            body["publicKey"].as_str().unwrap_or(""),
                            body["signature"].as_str().unwrap_or(""),
                        );
                        let fresh = s.nonces.last().map(String::as_str) == Some(nonce);
                        let good = crate::devices::verify(key, &format!("{SIGNING_PREFIX}{nonce}\n{key}"), sig);
                        let known = s.devices.iter().any(|(k, _)| k == key);
                        let license = body["licenseKey"].as_str();
                        s.license_sent += usize::from(license.is_some());
                        if !(fresh && good) {
                            (401, json!({ "error": "bad signature" }))
                        } else if !known && license != Some("JL1.test-license") {
                            (401, json!({ "error": "unknown_device" }))
                        } else {
                            if !known {
                                s.devices.push((key.to_string(), "ch_grace".into()));
                            }
                            let token = format!("jwt-{}", s.tokens.len());
                            s.tokens.push(token.clone());
                            let answer = json!({ "token": token, "expiresAt": crate::now_ms() + 3_600_000,
                                "supabaseUrl": supabase, "supabaseAnonKey": "anon-test" });
                            (200, answer)
                        }
                    }
                    (_, "/rest/v1/rpc/sync_push" | "/rest/v1/sync_fields") if !signed_in => (401, json!({})),
                    ("POST", "/rest/v1/rpc/sync_push") => {
                        let changes: Vec<FieldChange> = serde_json::from_value(body["changes"].clone()).unwrap();
                        (200, serde_json::to_value(cloud.push(&changes).unwrap()).unwrap())
                    }
                    ("GET", "/rest/v1/sync_fields") => {
                        let q = target.split_once('?').unwrap().1;
                        let arg = |name: &str| {
                            q.split('&').find_map(|p| p.strip_prefix(name)).unwrap().parse::<u64>().unwrap()
                        };
                        let rows: Vec<Value> = cloud
                            .pull(arg("seq=gt."), arg("limit=") as usize)
                            .unwrap()
                            .into_iter()
                            .map(|(seq, c)| json!({ "seq": seq, "kind": c.kind, "doc_id": c.id, "field": c.field, "value": c.value, "hlc": c.hlc }))
                            .collect();
                        (200, Value::Array(rows))
                    }
                    _ => (404, json!({ "error": "not found" })),
                };
                drop(s);
                let text = answer.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        base
    }

    #[test]
    fn two_church_computers_sync_a_song_through_supabase_after_signing_in_with_the_license() {
        let cloud = MemoryCloud::default();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let site = stand_in(cloud, seen.clone());
        let dir = |n: &str| {
            let d =
                std::env::temp_dir().join(format!("jivvy-sync-cloud-{n}-{}-{}", std::process::id(), crate::now_ms()));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("license.key"), "JL1.test-license\n").unwrap();
            d
        };
        let (da, db) = (dir("a"), dir("b"));
        let open =
            |d: &Path| Syncer::new(Library::open(d).unwrap(), Box::new(SupabaseTransport::new(&site, d).unwrap()));
        let (mut a, mut b) = (open(&da), open(&db));
        let go = || true;
        let song =
            json!({ "sections": [{ "id": "v1", "label": "Verse 1", "text": "Amazing grace" }], "arrangement": ["v1"] });
        a.library().put("song", "grace", "Amazing Grace", song.as_object().unwrap(), Some(0), 1_000).unwrap();
        assert!(a.round(1_100, &go).unwrap().finished);
        assert!(b.round(1_200, &go).unwrap().finished);
        assert_eq!(b.library().get("song", "grace").unwrap().unwrap().body, *song.as_object().unwrap());
        // A second round reuses each computer's token: no new sign-in, no license key sent again.
        b.library().put("song", "grace", "Amazing Grace!", song.as_object().unwrap(), None, 2_000).unwrap();
        b.round(2_100, &go).unwrap();
        a.round(2_200, &go).unwrap();
        assert_eq!(a.library().get("song", "grace").unwrap().unwrap().info.title, "Amazing Grace!");
        let s = seen.lock().unwrap();
        assert_eq!(
            (s.devices.len(), s.license_sent, s.tokens.len()),
            (2, 2, 2),
            "the license key went once per computer"
        );
        drop((a, b));
        let _ = (std::fs::remove_dir_all(&da), std::fs::remove_dir_all(&db));
    }
}
