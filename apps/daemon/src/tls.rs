//! TLS for the WebSocket command channel (`wss://`): the church's certificate and private key,
//! read from PEM files.
//!
//! Renewing the certificate just replaces the two files. Each new connection checks them and
//! picks up a new pair, so the engine never restarts for a certificate and open connections
//! are untouched. A pair that can't be used (a file half-written mid-renewal, a key that
//! doesn't match the certificate) leaves the last good pair in use. With no good pair at all,
//! connections are refused: the channel is never served in plain text instead.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub struct TlsFiles {
    cert: PathBuf,
    key: PathBuf,
    loaded: Mutex<Loaded>,
}

#[derive(Default)]
struct Loaded {
    /// The file contents (cert, key) last tried, good or not, so unchanged files aren't
    /// parsed again. `None` until the files have been read once, so even empty files get
    /// checked (and their problem logged) the first time.
    tried: Option<(Vec<u8>, Vec<u8>)>,
    /// The last good pair.
    config: Option<Arc<ServerConfig>>,
    /// The last problem logged, so a bad pair is logged once, not on every connection.
    problem: Option<String>,
}

impl TlsFiles {
    pub fn new(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        TlsFiles { cert: cert.into(), key: key.into(), loaded: Mutex::new(Loaded::default()) }
    }

    /// The TLS settings for a new connection, re-read if either file changed. `None` until a
    /// good pair has been read.
    pub fn config(&self) -> Option<Arc<ServerConfig>> {
        let mut loaded = self.loaded.lock().unwrap_or_else(|p| p.into_inner());
        let files = std::fs::read(&self.cert).and_then(|c| Ok((c, std::fs::read(&self.key)?)));
        let problem = match files {
            Ok(files) if loaded.tried.as_ref() == Some(&files) => return loaded.config.clone(),
            Ok((cert, key)) => {
                let result = server_config(&cert, &key);
                loaded.tried = Some((cert, key));
                match result {
                    Ok(config) => {
                        crate::log("engine", format!("WebSocket certificate loaded from {}", self.cert.display()));
                        loaded.config = Some(Arc::new(config));
                        loaded.problem = None;
                        return loaded.config.clone();
                    }
                    Err(e) => e,
                }
            }
            Err(e) => format!("can't read the certificate or key: {e}"),
        };
        if loaded.problem.as_deref() != Some(problem.as_str()) {
            let keeping = if loaded.config.is_some() { "keeping the previous one" } else { "refusing connections" };
            crate::log("engine", format!("WebSocket certificate {}: {problem}; {keeping}", self.cert.display()));
            loaded.problem = Some(problem);
        }
        loaded.config.clone()
    }

    pub fn cert_path(&self) -> &Path {
        &self.cert
    }
}

/// Never puts key bytes in the error: errors go to the log.
fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<ServerConfig, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|_| "the certificate file isn't valid PEM".to_string())?;
    if certs.is_empty() {
        return Err("the certificate file has no certificate in it".into());
    }
    let key =
        PrivateKeyDer::from_pem_slice(key_pem).map_err(|_| "the key file has no usable private key".to_string())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let signing_key = provider
        .key_provider
        .load_private_key(key.clone_key())
        .map_err(|_| "the private key can't be used".to_string())?;
    rustls::sign::CertifiedKey::new(certs.clone(), signing_key)
        .keys_match()
        .map_err(|_| "the private key doesn't match the certificate".to_string())?;
    ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|_| "the certificate and key can't be used together".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A certificate authority and a certificate it signed for `name`: (CA PEM, cert PEM, key PEM).
    pub(crate) fn test_pair(name: &str) -> (String, String, String) {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec![name.to_string()]).unwrap().signed_by(&key, &ca, &ca_key).unwrap();
        (ca.pem(), cert.pem(), key.serialize_pem())
    }

    pub(crate) fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jivvy-tls-{name}-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_a_pair_and_keeps_it_until_the_files_change() {
        let dir = temp("load");
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        let tls = TlsFiles::new(&cert, &key);
        assert!(tls.config().is_none(), "no files yet: nothing to serve with");

        let (_, c, k) = test_pair("church.d.jivvy.org");
        std::fs::write(&cert, &c).unwrap();
        std::fs::write(&key, &k).unwrap();
        let first = tls.config().expect("a good pair loads");
        assert!(Arc::ptr_eq(&first, &tls.config().unwrap()), "unchanged files aren't loaded again");

        let (_, c2, k2) = test_pair("church.d.jivvy.org");
        std::fs::write(&cert, &c2).unwrap();
        std::fs::write(&key, &k2).unwrap();
        assert!(!Arc::ptr_eq(&first, &tls.config().unwrap()), "a renewed pair is picked up");
    }

    #[test]
    fn empty_files_are_checked_not_mistaken_for_files_already_tried() {
        let dir = temp("empty");
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        std::fs::write(&cert, "").unwrap();
        std::fs::write(&key, "").unwrap();
        let tls = TlsFiles::new(&cert, &key);
        assert!(tls.config().is_none());
        let problem = tls.loaded.lock().unwrap().problem.clone();
        assert!(problem.is_some_and(|p| p.contains("no certificate")), "the empty pair is diagnosed");
    }

    #[test]
    fn a_bad_pair_keeps_the_last_good_one() {
        let dir = temp("bad");
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        let (_, c, k) = test_pair("church.d.jivvy.org");
        std::fs::write(&cert, &c).unwrap();
        std::fs::write(&key, &k).unwrap();
        let tls = TlsFiles::new(&cert, &key);
        let good = tls.config().unwrap();

        // Renewal caught halfway: the new certificate with the old key.
        let (_, c2, _) = test_pair("church.d.jivvy.org");
        std::fs::write(&cert, &c2).unwrap();
        assert!(Arc::ptr_eq(&good, &tls.config().unwrap()), "a mismatched key keeps the last good pair");
        std::fs::write(&cert, "not a certificate").unwrap();
        assert!(Arc::ptr_eq(&good, &tls.config().unwrap()), "a garbled file keeps the last good pair");
        std::fs::remove_file(&key).unwrap();
        assert!(Arc::ptr_eq(&good, &tls.config().unwrap()), "a missing file keeps the last good pair");
    }

    #[test]
    fn errors_never_contain_the_key() {
        let (_, c, k) = test_pair("church.d.jivvy.org");
        let (_, c2, _) = test_pair("church.d.jivvy.org");
        let body: String = k.lines().filter(|l| !l.starts_with("-----")).collect();
        for err in [server_config(c2.as_bytes(), k.as_bytes()), server_config(c.as_bytes(), b"junk")] {
            let msg = err.expect_err("refused");
            assert!(!msg.contains(&body[..20]), "{msg}");
        }
    }
}
