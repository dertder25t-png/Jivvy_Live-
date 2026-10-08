//! Streaming to the platforms over RTMP/RTMPS, or to YouTube by HLS segment upload
//! (`hls`). Pure logic here; the pipelines live in `jivvy-video`.
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
    /// Server address without the key, e.g. `rtmps://a.rtmps.youtube.com:443/live2`, or
    /// YouTube's HLS ingestion address (`https://a.upload.youtube.com/http_upload_hls?...`).
    pub url: String,
    /// Stream key: appended to an RTMP URL, or the `cid` of an HLS one. Never logged or shown.
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
    /// HTTP(S) addresses get YouTube's HLS segment upload; everything else RTMP.
    pub fn is_hls(&self) -> bool {
        let url = self.url.trim();
        url.starts_with("https://") || url.starts_with("http://")
    }

    /// The upload address for one HLS file (playlist or segment), key included as `cid`.
    /// Only ever handed to the HTTP client. A key in the `key` field wins over one already
    /// in the address; `copy` (0 primary, 1 backup) is kept, defaulting to 0.
    pub fn hls_url(&self, file: &str) -> String {
        let url = self.url.trim();
        let base = url.split('?').next().unwrap_or(url);
        let key = match self.key.trim() {
            "" => self.url_param("cid"),
            k => k,
        };
        let copy = Some(self.url_param("copy")).filter(|c| !c.is_empty()).unwrap_or("0");
        format!("{base}?cid={key}&copy={copy}&file={file}")
    }

    /// A query parameter of the address (e.g. an HLS key written in as `cid=...`), or "".
    fn url_param(&self, name: &str) -> &str {
        let query = self.url.trim().split_once('?').map(|(_, q)| q).unwrap_or("");
        query.split('&').find_map(|kv| kv.strip_prefix(name).and_then(|v| v.strip_prefix('='))).unwrap_or("")
    }

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
        let full = self.publish_url();
        let mut out = text.replace(&full, &format!("{}/<key>", self.display_server()));
        for key in [self.key.trim(), self.url_param("cid")] {
            if key.len() >= 4 {
                out = out.replace(key, "<key>");
            }
        }
        out
    }
}

/// `scheme://host[:port]/app` with any further path (where RTMP keys go) and any query
/// (where HLS keys go) cut off.
pub fn redact_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or("");
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
    if !(url.starts_with("rtmp://") || url.starts_with("rtmps://") || d.is_hls()) {
        return Err(format!("{}: the server address must start with rtmps://, rtmp:// or https://", d.id));
    }
    let redacted = redact_url(url);
    let host = redacted.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
    if host.is_empty() {
        return Err(format!("{}: the server address has no host", d.id));
    }
    if d.is_hls() {
        // The key travels in the address: plain http only to this computer (test servers).
        let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
        if url.starts_with("http://") && !["127.0.0.1", "localhost", "[::1]"].contains(&name) {
            return Err(format!("{}: the upload address must start with https://", d.id));
        }
        let (key, url_key) = (d.key.trim(), d.url_param("cid"));
        if key.is_empty() && (url_key.is_empty() || url_key.starts_with('$')) {
            return Err(format!("{}: no stream key", d.id));
        }
        // It goes into a query string unescaped, as YouTube's own examples do.
        if !key.chars().chain(url_key.chars()).all(|c| c.is_ascii_alphanumeric() || "-_.$".contains(c)) {
            return Err(format!("{}: the stream key has characters YouTube never uses", d.id));
        }
        return Ok(());
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
    if says(&["refused", "could not connect", "failed to connect", "no route", "unreachable", "never answered"]) {
        "Can't reach the streaming server. Check the internet connection and the server address.".into()
    } else if says(&["resolve", "name or service", "no such host", "host not found", "dns"]) {
        "Can't find the streaming server. Check the server address and the internet connection.".into()
    } else if says(&["no data reached the server", "stopped confirming", "timed out", "timeout"]) {
        "The streaming server stopped responding. Reconnecting.".into()
    } else if says(&[
        "reset",
        "forcibly closed",
        "broken pipe",
        "error receiving data",
        "error sending data",
        "closed",
        "unexpected eof",
    ]) {
        "Lost the connection to the streaming server. Reconnecting.".into()
    } else if says(&["couldn't keep up", "upload is too slow"]) {
        "The internet upload is too slow for this stream. Reconnecting.".into()
    } else if says(&["program changed"]) {
        "The stream settings changed. Reconnecting.".into()
    } else if says(&["auth", "forbidden", "401", "403", "rejected", "denied", "invalid key", "publish"]) {
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

/// A server that hasn't answered the RTMP handshake in this long is unreachable.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The RTMP sink's own connection counters (its `stats` property).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RtmpStats {
    /// Bytes the server sent us: above zero once it has answered the handshake and publish.
    pub in_bytes: u64,
    /// Bytes the socket accepted from us.
    pub out_bytes: u64,
    /// Bytes the server has confirmed receiving (it confirms once per `window`).
    pub acked: u64,
    /// The server's acknowledgement window; 0 if it hasn't said.
    pub window: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The server hasn't answered yet.
    Connecting,
    /// Answered and data is moving, not yet long enough to call it live.
    Starting,
    Live,
    /// Give up on this connection and reconnect.
    Failed(&'static str),
}

/// Decides from the sink's counters whether data is really reaching the server. Buffers
/// arriving at the sink prove nothing (they're accepted before the connection exists);
/// only the server's answer, the socket taking our bytes, and the server's
/// acknowledgements do.
#[derive(Debug)]
pub struct DeliveryTracker {
    started: std::time::Instant,
    connected_at: Option<std::time::Instant>,
    last_out: u64,
    last_out_at: Option<std::time::Instant>,
    flowing_since: Option<std::time::Instant>,
}

impl DeliveryTracker {
    pub fn new(now: std::time::Instant) -> Self {
        DeliveryTracker { started: now, connected_at: None, last_out: 0, last_out_at: None, flowing_since: None }
    }

    pub fn update(&mut self, s: RtmpStats, now: std::time::Instant) -> Delivery {
        if s.in_bytes == 0 {
            return if now.duration_since(self.started) > CONNECT_TIMEOUT {
                Delivery::Failed("the server never answered")
            } else {
                Delivery::Connecting
            };
        }
        let connected_at = *self.connected_at.get_or_insert(now);
        if s.out_bytes > self.last_out {
            (self.last_out, self.last_out_at) = (s.out_bytes, Some(now));
            self.flowing_since.get_or_insert(now);
        } else if now.duration_since(self.last_out_at.unwrap_or(connected_at)) > STALL {
            return Delivery::Failed("no data reached the server for 5 s");
        }
        let unacked = s.out_bytes.saturating_sub(s.acked);
        if s.window > 0 && unacked > 3 * s.window {
            return Delivery::Failed("the server stopped confirming what it received");
        }
        // Before the first window completes no confirmation is possible yet; after that,
        // confirmations must keep up for the stream to count as live.
        let confirmed = s.window == 0 || unacked <= 2 * s.window;
        // Writes go out about 30 times a second; two seconds without one isn't "live" any
        // more, even before it counts as a stall.
        let recent = self.last_out_at.is_some_and(|t| now.duration_since(t) <= Duration::from_secs(2));
        match self.flowing_since {
            Some(t) if confirmed && recent && now.duration_since(t) >= Duration::from_secs(1) => Delivery::Live,
            _ => Delivery::Starting,
        }
    }
}

/// One destination's state, as `media-status.json` and the System screen show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationStatus {
    pub id: String,
    pub name: String,
    pub server: String,
    /// "off", "connecting", "live", "reconnecting", or "paused" (by the bandwidth manager:
    /// the upload can't carry this platform too).
    pub state: &'static str,
    pub detail: String,
    pub kbps: f64,
    pub reconnects: u32,
    /// What it sends: "1080p", "720p", ... (the bandwidth manager's choice), or "" when off.
    pub quality: String,
}

/// What screens see: live only when every destination is live, apart from ones the
/// bandwidth manager paused (as long as one is still live).
pub fn overall(wanted: bool, destinations: &[DestinationStatus]) -> &'static str {
    let sending: Vec<&DestinationStatus> = destinations.iter().filter(|d| d.state != "paused").collect();
    if !wanted {
        "off"
    } else if !sending.is_empty() && sending.iter().all(|d| d.state == "live") {
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
    /// The upload speed the bandwidth manager is working with (kbps); None until the
    /// connection has shown a limit.
    pub upload_kbps: Option<f64>,
    /// Lower-quality encodes running for stepped-down destinations.
    pub encodes: Vec<EncodeStatus>,
}

/// One lower-quality encode.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeStatus {
    pub quality: String,
    pub encoder: String,
    pub frames_encoded: u64,
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
        assert_eq!(
            redact_url("https://a.upload.youtube.com/http_upload_hls?cid=secret&copy=0&file="),
            "https://a.upload.youtube.com/http_upload_hls"
        );
    }

    fn hls(url: &str, key: &str) -> Destination {
        Destination { url: url.into(), ..yt(key) }
    }

    #[test]
    fn hls_addresses_carry_the_key_as_cid_and_nowhere_else() {
        let base = "https://a.upload.youtube.com/http_upload_hls";
        // As YouTube Studio shows it, with the key in its own field.
        let d = hls(&format!("{base}?cid=$STREAM_KEY&copy=0&file="), "abcd-efgh-ijkl-mnop");
        assert!(d.is_hls() && !yt("k").is_hls());
        assert_eq!(d.hls_url("live.m3u8"), format!("{base}?cid=abcd-efgh-ijkl-mnop&copy=0&file=live.m3u8"));
        assert_eq!(d.display_server(), base);
        // The key already in the address; the backup ingest keeps copy=1.
        let d = hls("https://b.upload.youtube.com/http_upload_hls?cid=wxyz-1234&copy=1&file=", "");
        assert_eq!(
            d.hls_url("s1-00000.ts"),
            "https://b.upload.youtube.com/http_upload_hls?cid=wxyz-1234&copy=1&file=s1-00000.ts"
        );
        let scrubbed = d.scrub(&format!("error sending to {}", d.hls_url("x.ts")));
        assert!(!scrubbed.contains("wxyz-1234"), "{scrubbed}");
        // Just the address and a key.
        assert_eq!(hls(base, "k-1").hls_url("a.ts"), format!("{base}?cid=k-1&copy=0&file=a.ts"));
    }

    #[test]
    fn hls_validation_catches_what_a_tech_lead_can_fix() {
        let base = "https://a.upload.youtube.com/http_upload_hls";
        assert!(validate(&hls(base, "abcd-efgh")).is_ok());
        assert!(validate(&hls(&format!("{base}?cid=abcd&copy=0&file="), "")).is_ok());
        let placeholder = hls(&format!("{base}?cid=$STREAM_KEY&copy=0&file="), "");
        assert!(validate(&placeholder).unwrap_err().contains("no stream key"));
        let plain = hls("http://a.upload.youtube.com/http_upload_hls", "k");
        assert!(validate(&plain).unwrap_err().contains("https://"));
        assert!(validate(&hls("http://127.0.0.1:8080/http_upload_hls", "k")).is_ok(), "local test servers");
        assert!(validate(&hls(base, "a&copy=1")).unwrap_err().contains("characters"));
        assert!(validate(&hls("https:///x", "k")).is_err());
    }

    #[test]
    fn validation_catches_what_a_tech_lead_can_fix() {
        assert!(validate(&yt("k")).is_ok());
        assert!(validate(&Destination { url: "ftp://x/live".into(), ..yt("k") }).unwrap_err().contains("rtmp://"));
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
            ("the server answered 401", "rejected"),
            ("Internal data stream error.", "interrupted"),
        ];
        for (raw, expect) in cases {
            assert!(plain_error(raw).contains(expect), "{raw} -> {}", plain_error(raw));
        }
    }

    fn at(t0: std::time::Instant, ms: u64) -> std::time::Instant {
        t0 + Duration::from_millis(ms)
    }

    fn stats(in_bytes: u64, out_bytes: u64, acked: u64) -> RtmpStats {
        RtmpStats { in_bytes, out_bytes, acked, window: 2_500_000 }
    }

    #[test]
    fn a_black_holed_server_is_never_live() {
        let t0 = std::time::Instant::now();
        let mut d = DeliveryTracker::new(t0);
        // Buffers pile into the sink but the server never answers.
        for ms in (0..=10_000).step_by(500) {
            assert_eq!(d.update(RtmpStats::default(), at(t0, ms)), Delivery::Connecting);
        }
        assert_eq!(d.update(RtmpStats::default(), at(t0, 10_001)), Delivery::Failed("the server never answered"));
    }

    #[test]
    fn live_only_after_the_server_answers_and_a_second_of_real_writes() {
        let t0 = std::time::Instant::now();
        let mut d = DeliveryTracker::new(t0);
        assert_eq!(d.update(stats(394, 0, 0), at(t0, 100)), Delivery::Starting, "answered, nothing written yet");
        assert_eq!(d.update(stats(394, 700_000, 0), at(t0, 200)), Delivery::Starting);
        assert_eq!(d.update(stats(394, 1_400_000, 0), at(t0, 1_000)), Delivery::Starting);
        assert_eq!(
            d.update(stats(394, 2_200_000, 0), at(t0, 1_250)),
            Delivery::Live,
            "first window not done: writes count"
        );
        assert_eq!(d.update(stats(406, 3_000_000, 2_500_000), at(t0, 2_000)), Delivery::Live);
    }

    #[test]
    fn a_server_that_stops_confirming_or_a_blocked_socket_is_dropped() {
        let t0 = std::time::Instant::now();
        let mut d = DeliveryTracker::new(t0);
        d.update(stats(394, 1_000_000, 0), at(t0, 0));
        // The socket keeps accepting (big buffers) but the server never confirms.
        assert_eq!(
            d.update(stats(394, 6_000_000, 0), at(t0, 6_000)),
            Delivery::Starting,
            "too far behind to call live"
        );
        assert_eq!(
            d.update(stats(394, 7_600_000, 0), at(t0, 8_000)),
            Delivery::Failed("the server stopped confirming what it received")
        );
        // The socket stops taking bytes at all.
        let mut d = DeliveryTracker::new(t0);
        d.update(stats(394, 1_000_000, 0), at(t0, 0));
        assert_eq!(d.update(stats(394, 1_000_000, 0), at(t0, 4_900)), Delivery::Starting);
        assert_eq!(
            d.update(stats(394, 1_000_000, 0), at(t0, 5_100)),
            Delivery::Failed("no data reached the server for 5 s")
        );
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
            quality: String::new(),
        };
        assert_eq!(overall(false, &[d("live")]), "off");
        assert_eq!(overall(true, &[d("live"), d("paused")]), "live", "paused for bandwidth");
        assert_eq!(overall(true, &[d("paused")]), "reconnecting", "nothing going out");
        assert_eq!(overall(true, &[d("live"), d("live")]), "live");
        assert_eq!(overall(true, &[d("live"), d("reconnecting")]), "reconnecting");
        assert_eq!(overall(true, &[]), "reconnecting", "wanted but nowhere to send it");
    }
}
