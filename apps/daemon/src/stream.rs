//! Streaming to the platforms over RTMP/RTMPS. Pure logic here; the pipelines live in
//! `jivvy-video`.
//!
//! Each destination gets its own small pipeline fed with the program's already-encoded
//! video and audio, so a network failure only ever tears down and reconnects that
//! destination; the program (and the recording) never notice. A reconnect starts on the
//! next keyframe with timestamps from zero, so the platform sees a clean stream.
//!
//! Destinations and their stream keys live in `stream.json` in the data directory. Keys
//! are secrets: they never appear in logs, status files or error messages.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "stream.json";
pub const CONFIG_VERSION: u32 = 1;
/// No bytes reaching the server for this long (with the connection still open) counts as
/// a dropped connection: tear down and reconnect rather than wait on a dead socket.
pub const STALL: Duration = Duration::from_secs(5);
/// Encoded packets queued for one destination before it counts as stuck.
pub const QUEUE_PACKETS: usize = 600;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Server address without the key, e.g. `rtmps://a.rtmps.youtube.com:443/live2`.
    pub url: String,
    /// Stream key, appended to the URL. Never logged or shown.
    #[serde(default)]
    pub key: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StreamConfig {
    pub version: u32,
    #[serde(default)]
    pub destinations: Vec<Destination>,
}

impl Destination {
    /// The full publish URL, key included. Only ever handed to the RTMP sink.
    pub fn publish_url(&self) -> String {
        let base = self.url.trim().trim_end_matches('/');
        let key = self.key.trim();
        if key.is_empty() { base.to_string() } else { format!("{base}/{key}") }
    }

    /// Scheme, host and app only: safe to show and log.
    pub fn display_server(&self) -> String {
        redact_url(self.url.trim())
    }

    /// Removes the key (and anything after the app path) from text such as an error message.
    pub fn scrub(&self, text: &str) -> String {
        let key = self.key.trim();
        let full = self.publish_url();
        let mut out = text.replace(&full, &format!("{}/<key>", self.display_server()));
        if key.len() >= 4 {
            out = out.replace(key, "<key>");
        }
        out
    }
}

/// `scheme://host[:port]/app` with any further path (where keys go) cut off.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return "<invalid url>".into() };
    let mut parts = rest.splitn(3, '/');
    let host = parts.next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host); // never show user:password@
    match parts.next().filter(|a| !a.is_empty()) {
        Some(app) => format!("{scheme}://{host}/{app}"),
        None => format!("{scheme}://{host}"),
    }
}

/// Problems that keep a destination from being used, in words a tech lead can act on.
pub fn validate(d: &Destination) -> Result<(), String> {
    let url = d.url.trim();
    if !(url.starts_with("rtmp://") || url.starts_with("rtmps://")) {
        return Err(format!("{}: the server address must start with rtmp:// or rtmps://", d.id));
    }
    if redact_url(url).split("://").nth(1).is_none_or(|h| h.is_empty() || h.starts_with('/')) {
        return Err(format!("{}: the server address has no host", d.id));
    }
    if d.key.trim().is_empty() && url.trim_end_matches('/').matches('/').count() < 4 {
        return Err(format!("{}: no stream key", d.id));
    }
    Ok(())
}

/// Reads `stream.json`; missing means no destinations. Invalid is an error and the caller
/// keeps the destinations it has.
pub fn load_config(dir: &Path) -> Result<StreamConfig, String> {
    let bytes = match std::fs::read(dir.join(CONFIG_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StreamConfig { version: CONFIG_VERSION, destinations: vec![] });
        }
        Err(e) => return Err(format!("can't read {CONFIG_FILE}: {e}")),
    };
    // Never echo the file's contents (it holds keys); serde's message points at a position only.
    let c: StreamConfig =
        serde_json::from_slice(&bytes).map_err(|e| format!("{CONFIG_FILE} is not valid (line {})", e.line()))?;
    if c.version != CONFIG_VERSION {
        return Err(format!("{CONFIG_FILE} has version {}, expected {CONFIG_VERSION}", c.version));
    }
    Ok(c)
}

/// A failure in words a tech lead can act on. The raw text (scrubbed of the key) goes to
/// the log; this goes on screen.
pub fn plain_error(raw: &str) -> String {
    let r = raw.to_ascii_lowercase();
    let says = |words: &[&str]| words.iter().any(|w| r.contains(w));
    if says(&["refused", "could not connect", "failed to connect", "no route", "unreachable"]) {
        "Can't reach the streaming server. Check the internet connection and the server address.".into()
    } else if says(&["resolve", "name or service", "no such host", "dns"]) {
        "Can't find the streaming server. Check the server address and the internet connection.".into()
    } else if says(&["no data reached the server", "timed out", "timeout"]) {
        "The streaming server stopped responding. Reconnecting.".into()
    } else if says(&["reset", "forcibly closed", "broken pipe", "error receiving data", "error sending data", "closed"])
    {
        "Lost the connection to the streaming server. Reconnecting.".into()
    } else if says(&["couldn't keep up"]) {
        "The internet upload is too slow for this stream. Reconnecting.".into()
    } else if says(&["program changed"]) {
        "The stream settings changed. Reconnecting.".into()
    } else if says(&["auth", "forbidden", "403", "rejected", "denied", "invalid key", "publish"]) {
        "The platform rejected the stream. Check the stream key and that the event is ready to go live.".into()
    } else {
        "The stream was interrupted. Reconnecting.".into()
    }
}

/// Wait before reconnect attempt `attempt` (1-based): quick at first so a short blip costs
/// little, then every 5 s so a long outage doesn't hammer the platform.
pub fn reconnect_delay(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => Duration::from_millis(500),
        2 => Duration::from_secs(1),
        3 => Duration::from_secs(2),
        4 => Duration::from_secs(3),
        _ => Duration::from_secs(5),
    }
}

/// One destination's state, as `media-status.json` and the System screen show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationStatus {
    pub id: String,
    pub name: String,
    pub server: String,
    /// "off", "connecting", "live" or "reconnecting".
    pub state: &'static str,
    pub detail: String,
    pub kbps: f64,
    pub reconnects: u32,
}

/// What screens see: live only when every enabled destination is live.
pub fn overall(wanted: bool, destinations: &[DestinationStatus]) -> &'static str {
    if !wanted {
        "off"
    } else if !destinations.is_empty() && destinations.iter().all(|d| d.state == "live") {
        "live"
    } else {
        "reconnecting"
    }
}

/// `stream` in `media-status.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    pub wanted: bool,
    pub status: &'static str,
    pub destinations: Vec<DestinationStatus>,
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt(key: &str) -> Destination {
        Destination {
            id: "youtube".into(),
            name: "YouTube".into(),
            url: "rtmps://a.rtmps.youtube.com:443/live2/".into(),
            key: key.into(),
            enabled: true,
        }
    }

    #[test]
    fn keys_go_only_into_the_publish_url() {
        let d = yt("abcd-efgh-ijkl-mnop");
        assert_eq!(d.publish_url(), "rtmps://a.rtmps.youtube.com:443/live2/abcd-efgh-ijkl-mnop");
        assert_eq!(d.display_server(), "rtmps://a.rtmps.youtube.com:443/live2");
        let err = format!("Could not connect to {}: refused (key abcd-efgh-ijkl-mnop)", d.publish_url());
        let scrubbed = d.scrub(&err);
        assert!(!scrubbed.contains("abcd"), "{scrubbed}");
        assert!(scrubbed.contains("rtmps://a.rtmps.youtube.com:443/live2/<key>"));
    }

    #[test]
    fn redaction_drops_paths_and_credentials() {
        assert_eq!(redact_url("rtmp://127.0.0.1:1935/live/secret-key"), "rtmp://127.0.0.1:1935/live");
        assert_eq!(redact_url("rtmp://user:pass@host/app/key"), "rtmp://host/app");
        assert_eq!(redact_url("rtmp://host"), "rtmp://host");
        assert_eq!(redact_url("not a url"), "<invalid url>");
    }

    #[test]
    fn validation_catches_what_a_tech_lead_can_fix() {
        assert!(validate(&yt("k")).is_ok());
        assert!(validate(&Destination { url: "http://x/live".into(), ..yt("k") }).unwrap_err().contains("rtmp://"));
        assert!(validate(&yt("")).unwrap_err().contains("no stream key"));
        // A key written straight into the URL is fine too.
        assert!(validate(&Destination { url: "rtmp://host:1935/live/inline-key".into(), ..yt("") }).is_ok());
        assert!(validate(&Destination { url: "rtmp:///live".into(), ..yt("k") }).is_err());
    }

    #[test]
    fn config_loads_without_ever_echoing_keys() {
        let dir = std::env::temp_dir().join(format!("jivvy-stream-cfg-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load_config(&dir).unwrap().destinations.is_empty());
        std::fs::write(
            dir.join(CONFIG_FILE),
            r#"{"version":1,"destinations":[{"id":"yt","url":"rtmp://h/live","key":"k-1"}]}"#,
        )
        .unwrap();
        let c = load_config(&dir).unwrap();
        assert!(c.destinations[0].enabled);
        std::fs::write(dir.join(CONFIG_FILE), r#"{"version":1,"destinations":[{"id":"yt","key":"super-secret-key-9"#)
            .unwrap();
        let e = load_config(&dir).unwrap_err();
        assert!(!e.contains("super-secret"), "{e}");
    }

    #[test]
    fn errors_on_screen_are_plain_english() {
        let cases = [
            ("Could not connect to server: Connection refused", "Can't reach"),
            (
                "Connection error: Error receiving data: An existing connection was forcibly closed",
                "Lost the connection",
            ),
            ("no data reached the server for 5 s", "stopped responding"),
            ("Error resolving 'a.rtmps.youtube.com': No such host is known", "Can't find"),
            ("the connection couldn't keep up", "too slow"),
            ("Internal data stream error.", "interrupted"),
        ];
        for (raw, expect) in cases {
            assert!(plain_error(raw).contains(expect), "{raw} -> {}", plain_error(raw));
        }
    }

    #[test]
    fn reconnects_are_quick_at_first_then_steady() {
        let delays: Vec<u64> = (1..=7).map(|n| reconnect_delay(n).as_millis() as u64).collect();
        assert_eq!(delays, [500, 1000, 2000, 3000, 5000, 5000, 5000]);
        // A 30 s outage is retried at least 7 times.
        let total: Duration = (1..=7).map(reconnect_delay).sum();
        assert!(total < Duration::from_secs(30));
    }

    #[test]
    fn screens_see_live_only_when_every_destination_is() {
        let d = |state| DestinationStatus {
            id: "a".into(),
            name: String::new(),
            server: String::new(),
            state,
            detail: String::new(),
            kbps: 0.0,
            reconnects: 0,
        };
        assert_eq!(overall(false, &[d("live")]), "off");
        assert_eq!(overall(true, &[d("live"), d("live")]), "live");
        assert_eq!(overall(true, &[d("live"), d("reconnecting")]), "reconnecting");
        assert_eq!(overall(true, &[]), "reconnecting", "wanted but nowhere to send it");
    }
}
