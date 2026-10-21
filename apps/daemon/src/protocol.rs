//! Rust port of `packages/protocol` (the TypeScript package is the reference).
//!
//! Must give the same results as the TypeScript `parseEnvelope` for every case in
//! `packages/protocol/fixtures/parse-cases.json`; the tests below run those cases.
//! Rules (CLAUDE.md #4): add fields, never rename or remove them.

use serde::Serialize;
use serde_json::{Map, Value};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MIN_SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum Command {
    #[serde(rename = "slide.next")]
    SlideNext,
    #[serde(rename = "slide.prev")]
    SlidePrev,
    #[serde(rename = "slide.goto")]
    SlideGoto { index: u64 },
    #[serde(rename = "output.black")]
    OutputBlack { on: bool },
    #[serde(rename = "stream.start")]
    StreamStart,
    #[serde(rename = "stream.stop")]
    StreamStop,
    #[serde(rename = "state.get")]
    StateGet,
    #[serde(rename = "state.subscribe")]
    StateSubscribe,
    #[serde(rename = "media.levels")]
    MediaLevels(Levels),
    #[serde(rename = "stream.report")]
    StreamReport {
        status: StreamStatus,
        /// Each platform; left out by older video processes.
        #[serde(skip_serializing_if = "Option::is_none")]
        destinations: Option<Vec<StreamDestination>>,
    },
    #[serde(rename = "recording.start")]
    RecordingStart,
    #[serde(rename = "recording.stop")]
    RecordingStop,
    #[serde(rename = "recording.report")]
    RecordingReport {
        status: RecordingStatus,
        #[serde(rename = "lowDisk")]
        low_disk: bool,
    },
    // Pairing and device authentication: the church computer's local channel is trusted;
    // every device on the network channel pairs once and proves its key on every connection.
    #[serde(rename = "pair.open")]
    PairOpen,
    #[serde(rename = "pair.close")]
    PairClose,
    #[serde(rename = "pair.request")]
    PairRequest {
        #[serde(rename = "publicKey")]
        public_key: String,
        name: String,
    },
    #[serde(rename = "pair.approve")]
    PairApprove {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "pair.deny")]
    PairDeny {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "auth.hello")]
    AuthHello {
        #[serde(rename = "publicKey")]
        public_key: String,
    },
    #[serde(rename = "auth.prove")]
    AuthProve { signature: String },
    #[serde(rename = "device.list")]
    DeviceList,
    /// Measure the upload and say what each platform will get; never while streaming.
    #[serde(rename = "stream.preflight")]
    StreamPreflight,
    #[serde(rename = "device.remove")]
    DeviceRemove {
        #[serde(rename = "deviceId")]
        device_id: String,
    },
    /// Replace the run sheet the screens show; saved before the ack.
    #[serde(rename = "runsheet.set")]
    RunSheetSet {
        #[serde(rename = "runSheet")]
        run_sheet: RunSheet,
    },
    #[serde(rename = "runsheet.get")]
    RunSheetGet,
    /// One piece of an image, by its SHA-256; `data` is base64.
    #[serde(rename = "media.put")]
    MediaPut { id: String, offset: u64, data: String, last: bool },
    /// Which of these images the church computer lacks.
    #[serde(rename = "media.has")]
    MediaHas { ids: Vec<String> },
    /// The library's documents of a kind, without bodies.
    #[serde(rename = "doc.list")]
    DocList { kind: String },
    #[serde(rename = "doc.get")]
    DocGet { kind: String, id: String },
    /// Save a document; with `base_rev`, only if the saved copy is still that revision.
    #[serde(rename = "doc.put")]
    DocPut {
        kind: String,
        id: String,
        title: String,
        body: Map<String, Value>,
        #[serde(rename = "baseRev", skip_serializing_if = "Option::is_none")]
        base_rev: Option<u64>,
    },
    #[serde(rename = "doc.delete")]
    DocDelete {
        kind: String,
        id: String,
        #[serde(rename = "baseRev", skip_serializing_if = "Option::is_none")]
        base_rev: Option<u64>,
    },
    /// The license and trial (see `license.rs`).
    #[serde(rename = "license.get")]
    LicenseGet,
    /// Save a license key; refused unless its signature is good. Never log `key`.
    #[serde(rename = "license.set")]
    LicenseSet { key: LicenseKey },
}

/// A license key as sent; its `Debug` never prints it, so it can't reach a log by accident.
#[derive(Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LicenseKey(pub String);

impl std::fmt::Debug for LicenseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LicenseKey(..)")
    }
}

/// Longest license key, in characters (the TypeScript `MAX_LICENSE_KEY`).
pub const MAX_LICENSE_KEY: usize = 1024;

/// Looks like a license key, `JL1.<payload>.<signature>` (the TypeScript `isLicenseKeyShape`);
/// the signature is checked by `license::verify`.
pub fn is_license_key_shape(key: &str) -> bool {
    let b64 = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let Some(rest) = key.strip_prefix("JL1.") else { return false };
    let Some((payload, sig)) = rest.split_once('.') else { return false };
    key.len() <= MAX_LICENSE_KEY && (1..=900).contains(&payload.len()) && b64(payload) && sig.len() == 86 && b64(sig)
}

/// Largest document body (as JSON), the same as the TypeScript `MAX_DOC_BYTES`.
pub const MAX_DOC_BYTES: usize = 512 * 1024;

/// A document kind: 1-32 of a-z 0-9 -, starting with a letter.
fn doc_kind(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let mut chars = s.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && s.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    ok.then(|| s.to_string())
}

/// An optional baseRev: Ok(None) when left out, Err when it isn't a whole number from 0.
fn base_rev(v: Option<&Value>) -> Result<Option<u64>, ()> {
    match v {
        None => Ok(None),
        Some(v) => {
            js_integer(v).filter(|f| *f >= 0.0 && *f <= 9_007_199_254_740_991.0).map(|f| Some(f as u64)).ok_or(())
        }
    }
}

/// A document in the library, without its body.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub rev: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Doc {
    #[serde(flatten)]
    pub info: DocInfo,
    pub body: Map<String, Value>,
}

/// A library document was saved or deleted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocChangedEvent<'a> {
    pub v: u32,
    pub event: &'static str,
    pub doc: &'a DocInfo,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

pub fn doc_changed_event(doc: &DocInfo, deleted: bool) -> DocChangedEvent<'_> {
    DocChangedEvent { v: PROTOCOL_VERSION, event: "doc.changed", doc, deleted }
}

/// One slide as the screens show it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slide {
    pub lines: Vec<String>,
    /// A countdown slide counts down to this time (ms since epoch) under its lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown_to: Option<u64>,
    /// An image slide: the image's id (its SHA-256), sent first with `media.put`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Which part of its item this is ("Verse 1"), for the operator only; never on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One item in the service (a song, a reading, a countdown...), already turned into slides
/// by the UI; the daemon only shows slides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSheetItem {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub slides: Vec<Slide>,
    /// While one of its slides shows, move to the next every this many seconds, wrapping
    /// from the last to the first (the pre-service loop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RunSheet {
    pub id: String,
    pub title: String,
    pub items: Vec<RunSheetItem>,
}

impl RunSheet {
    /// Every slide in order: what `slideIndex` counts.
    pub fn slides(&self) -> impl Iterator<Item = &Slide> {
        self.items.iter().flat_map(|i| i.slides.iter())
    }

    /// The item slide `index` belongs to, and the index of that item's first slide.
    pub fn item_at(&self, index: u64) -> Option<(&RunSheetItem, u64)> {
        let mut start = 0;
        for item in &self.items {
            let len = item.slides.len() as u64;
            if index < start + len {
                return Some((item, start));
            }
            start += len;
        }
        None
    }
}

/// Run sheet limits, the same as the TypeScript package's.
pub const MAX_RUNSHEET_ITEMS: usize = 200;
pub const MAX_RUNSHEET_SLIDES: usize = 2000;
pub const MAX_SLIDE_LINES: usize = 16;
pub const MAX_SLIDE_LINE: usize = 200;
pub const MAX_TITLE: usize = 200;
pub const MAX_SLIDE_LABEL: usize = 40;
pub const MAX_ADVANCE_SECONDS: u64 = 3600;

/// At most `max` characters (code points, as JavaScript's `[...s]` counts), no control characters.
fn plain_text(v: Option<&Value>, max: usize) -> Option<String> {
    let s = v?.as_str()?;
    let control = s.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f);
    (s.chars().count() <= max && !control).then(|| s.to_string())
}

fn slide(v: &Value) -> Option<Slide> {
    let obj = v.as_object()?;
    let lines = obj.get("lines")?.as_array()?;
    if lines.len() > MAX_SLIDE_LINES {
        return None;
    }
    let lines = lines.iter().map(|l| plain_text(Some(l), MAX_SLIDE_LINE)).collect::<Option<Vec<_>>>()?;
    let countdown_to = match obj.get("countdownTo") {
        None => None,
        Some(t) => Some(js_integer(t).filter(|f| *f >= 0.0)? as u64),
    };
    let image = match obj.get("image") {
        None => None,
        Some(i) => Some(media_id(Some(i))?),
    };
    let label = match obj.get("label") {
        None => None,
        Some(l) => Some(plain_text(Some(l), MAX_SLIDE_LABEL)?),
    };
    Some(Slide { lines, countdown_to, image, label })
}

/// Largest image the church computer keeps, and largest piece of one per `media.put`.
pub const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;
pub const MEDIA_CHUNK_BYTES: usize = 256 * 1024;

/// An image's id: the SHA-256 of its bytes, 64 lowercase hex.
pub fn media_id(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then(|| s.to_string())
}

/// Standard base64 of at most one chunk, checked the same way as the TypeScript (decoded later).
fn base64_chunk(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    let max = MEDIA_CHUNK_BYTES.div_ceil(3) * 4;
    let body = s.trim_end_matches('=');
    let ok = s.len() <= max
        && s.len() - body.len() <= 2
        && body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    ok.then(|| s.to_string())
}

/// A valid run sheet with unknown fields dropped, as the TypeScript `parseRunSheet`.
pub fn run_sheet(v: Option<&Value>) -> Option<RunSheet> {
    let obj = v?.as_object()?;
    let id = id_field(obj.get("id"))?;
    let title = plain_text(obj.get("title"), MAX_TITLE)?;
    let raw_items = obj.get("items")?.as_array()?;
    if raw_items.len() > MAX_RUNSHEET_ITEMS {
        return None;
    }
    let mut total = 0;
    let mut items = Vec::with_capacity(raw_items.len());
    for it in raw_items {
        let it = it.as_object()?;
        let id = id_field(it.get("id"))?;
        let kind = it.get("kind")?.as_str()?;
        let kind_ok = (1..=32).contains(&kind.len())
            && kind.starts_with(|c: char| c.is_ascii_lowercase())
            && kind.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !kind_ok {
            return None;
        }
        let title = plain_text(it.get("title"), MAX_TITLE)?;
        let raw_slides = it.get("slides")?.as_array()?;
        total += raw_slides.len();
        if total > MAX_RUNSHEET_SLIDES {
            return None;
        }
        let slides = raw_slides.iter().map(slide).collect::<Option<Vec<_>>>()?;
        let advance_seconds = match it.get("advanceSeconds") {
            None => None,
            Some(v) => Some(js_integer(v).filter(|f| (1.0..=MAX_ADVANCE_SECONDS as f64).contains(f))? as u64),
        };
        items.push(RunSheetItem { id, kind: kind.into(), title, slides, advance_seconds });
    }
    Some(RunSheet { id, title, items })
}

/// One streaming platform, as the video process reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamDestination {
    pub id: String,
    pub name: String,
    /// "off", "connecting", "live", "reconnecting" or "paused" (by the bandwidth manager).
    pub state: String,
    /// "1080p", "720p", ..., "audio" (audio only), or "" when not sending.
    pub quality: String,
    /// The best it may send (its own top quality in Advanced mode): sending that is never
    /// "lowered". Older video processes leave it out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<String>,
}

/// Most destinations a stream report may list.
pub const MAX_DESTINATIONS: usize = 16;

fn destination_list(v: &Value) -> Option<Vec<StreamDestination>> {
    let list = v.as_array()?;
    if list.len() > MAX_DESTINATIONS {
        return None;
    }
    list.iter()
        .map(|d| {
            let d = d.as_object()?;
            let id = id_field(d.get("id"))?;
            let name = d.get("name")?.as_str()?;
            let state = d.get("state")?.as_str()?;
            let quality = d.get("quality")?.as_str()?;
            let valid = |q: &str| {
                q.is_empty()
                    || q == "audio"
                    || (q.ends_with('p')
                        && (3..=4).contains(&(q.len() - 1))
                        && q[..q.len() - 1].bytes().all(|b| b.is_ascii_digit()))
            };
            let quality_ok = valid(quality);
            let top = match d.get("top") {
                None => None,
                Some(t) => Some(t.as_str().filter(|t| valid(t))?.to_string()),
            };
            let state_ok = ["off", "connecting", "live", "reconnecting", "paused"].contains(&state);
            (name.chars().count() <= MAX_DEVICE_NAME && state_ok && quality_ok).then(|| StreamDestination {
                id,
                name: name.into(),
                state: state.into(),
                quality: quality.into(),
                top,
            })
        })
        .collect()
}

/// Something the tech lead should know about, in plain words.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    /// "stream.lowered", "stream.paused" or "stream.resumed".
    pub kind: &'static str,
    pub destination_id: String,
    pub name: String,
    /// What it sends now; "" when paused.
    pub quality: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlertEvent<'a> {
    pub v: u32,
    pub event: &'static str,
    pub alert: &'a Alert,
}

pub fn alert_event(alert: &Alert) -> AlertEvent<'_> {
    AlertEvent { v: PROTOCOL_VERSION, event: "alert", alert }
}

/// Height of a quality label ("720p" -> 720); audio only ranks below every picture; 0 for none.
fn height(q: &str) -> u32 {
    if q == "audio" { 1 } else { q.trim_end_matches('p').parse().unwrap_or(0) }
}

/// A quality as people say it.
fn spoken(q: &str) -> &str {
    if q == "audio" { "audio only" } else { q }
}

/// The alerts between two reports of the same stream, word for word as the TypeScript
/// `streamAlerts` (packages/protocol/fixtures/stream-alerts.json). Only platforms in both
/// reports count: one starting or reconnecting isn't the bandwidth manager's doing.
pub fn stream_alerts(before: &[StreamDestination], after: &[StreamDestination]) -> Vec<Alert> {
    let mut out = Vec::new();
    for now in after {
        let Some(was) = before.iter().find(|d| d.id == now.id) else { continue };
        let name = if now.name.is_empty() { now.id.clone() } else { now.name.clone() };
        let quality = if now.state == "paused" { String::new() } else { now.quality.clone() };
        let alert = |kind, message| Alert {
            kind,
            destination_id: now.id.clone(),
            name: name.clone(),
            quality: quality.clone(),
            message,
        };
        if now.state == "paused" && was.state != "paused" {
            out.push(alert(
                "stream.paused",
                format!("{name} is paused: the internet upload can't carry it right now. It comes back by itself when the upload recovers."),
            ));
        } else if was.state == "paused" && now.state != "paused" && !now.quality.is_empty() {
            let how = if now.quality == "audio" { "audio only".to_string() } else { format!("at {}", now.quality) };
            out.push(alert("stream.resumed", format!("{name} is streaming again, {how}.")));
        } else if was.state != "paused"
            && now.state != "paused"
            && height(&now.quality) > 0
            && height(&now.quality) < height(&was.quality)
            // At its own top quality: the church lowered the top, the upload didn't.
            && now.top.as_deref() != Some(now.quality.as_str())
        {
            out.push(alert(
                "stream.lowered",
                format!(
                    "{name} lowered to {}: the internet upload is slow. It goes back up by itself.",
                    spoken(&now.quality)
                ),
            ));
        }
    }
    out
}

/// Longest device name, in characters (code points, as JavaScript's `[...name]` counts).
pub const MAX_DEVICE_NAME: usize = 64;
/// How long pairing stays open after `pair.open`.
pub const PAIRING_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);
/// Most pairing requests waiting at once.
pub const MAX_PENDING_PAIRINGS: usize = 5;

fn is_b64url(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A P-256 public key, raw uncompressed (65 bytes), base64url without padding.
fn public_key(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (s.len() == 87 && s.starts_with('B') && is_b64url(s)).then(|| s.to_string())
}

/// A 64-byte signature, base64url without padding.
fn signature(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (s.len() == 86 && is_b64url(s)).then(|| s.to_string())
}

/// Request and device ids.
fn id_field(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty() && s.len() <= 64 && is_b64url(s)).then(|| s.to_string())
}

/// A device name, trimmed like JavaScript's `trim()`: 1-64 characters, no control characters.
fn device_name(v: Option<&Value>) -> Option<String> {
    let name = v?.as_str()?.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let count = name.chars().count();
    let control = name.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f);
    ((1..=MAX_DEVICE_NAME).contains(&count) && !control).then(|| name.to_string())
}

/// Audio input levels in dBFS, one entry per channel.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Levels {
    pub peak_db: Vec<f64>,
    pub rms_db: Vec<f64>,
}

/// Most audio channels a levels report may carry (same as the TypeScript package).
pub const MAX_CHANNELS: usize = 32;

fn db_list(v: Option<&Value>) -> Option<Vec<f64>> {
    let list = v?.as_array()?;
    if list.is_empty() || list.len() > MAX_CHANNELS {
        return None;
    }
    list.iter().map(|n| n.as_f64().filter(|f| f.is_finite())).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadMessage,
    UnsupportedVersion,
    UnknownCommand,
    BadArguments,
    Unavailable,
    /// This connection may not do that: an unpaired or unproven device, a command only the
    /// church computer may send, or pairing that isn't open.
    Unauthorized,
    /// doc.put / doc.delete with a `baseRev` that is no longer the saved revision.
    Conflict,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub v: u32,
    pub id: String,
    pub ts: f64,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub code: ErrorCode,
    pub message: String,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamStatus {
    Off,
    Live,
    Reconnecting,
}

/// `Waiting`: a recording was asked for but nothing is being written yet (starting, or
/// resuming after a crash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingStatus {
    Off,
    Recording,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSnapshot {
    pub slide_index: u64,
    pub black: bool,
    pub stream: StreamStatus,
    /// The operator asked for the stream to be on (it may be reconnecting).
    pub stream_wanted: bool,
    pub recording: RecordingStatus,
    /// The operator asked for the recording to be on (it may be resuming).
    pub recording_wanted: bool,
    /// The disk the recording goes to has under 10 GB free.
    pub recording_low_disk: bool,
    /// Each platform's state and quality while streaming; left out when there's nothing to show.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stream_destinations: Vec<StreamDestination>,
    /// The run sheet on the daemon; these four are left out when none is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_sheet_id: Option<String>,
    /// Goes up by one every time a run sheet is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_sheet_rev: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide_count: Option<u64>,
    /// The slide at `slide_index`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide: Option<Slide>,
    /// The active theme's body, as the library keeps it; left out when none is picked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<Value>,
}

/// A device waiting for approval, as the church computer sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequestInfo {
    pub request_id: String,
    pub name: String,
    /// 6 random digits, also shown on the device.
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingStatus {
    pub open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closes_in_ms: Option<u64>,
    pub requests: Vec<PairRequestInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestAck {
    pub request_id: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub device_id: String,
    pub name: String,
    pub paired_at: u64,
}

/// Fields an ok ack may carry besides `state`; only some commands send them.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AckExtras {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pairing: Option<PairingStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestAck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<DeviceInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preflight: Option<Preflight>,
    /// runsheet.get
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_sheet: Option<RunSheet>,
    /// media.put
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaProgress>,
    /// media.has
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<String>>,
    /// doc.list
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs: Option<Vec<DocInfo>>,
    /// doc.get
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Doc>,
    /// doc.put, doc.delete
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<DocInfo>,
    /// license.get, license.set
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<crate::license::LicenseStatus>,
}

/// How much of an image the church computer has.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaProgress {
    pub id: String,
    pub received: u64,
    pub complete: bool,
}

/// One platform in the pre-flight summary.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct PreflightDestination {
    pub id: String,
    pub name: String,
    /// What it will get ("1080p", ..., "audio"), or "" if it won't fit and would be paused.
    pub quality: String,
}

/// The pre-flight streaming summary (same shape as the TypeScript `Preflight`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preflight {
    /// "measuring", "done" or "failed".
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_kbps: Option<f64>,
    /// True if measured just now; false if this is the last speed measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measured_at: Option<u64>,
    pub destinations: Vec<PreflightDestination>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Added in v1: the church computer's other checks (disk, streaming settings, video check,
    /// screens, camera and microphone). Left out while measuring and when the test was stopped.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<PreflightCheck>,
    /// Added in v1: `Some(true)` when the stream starting stopped the test; left out otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<bool>,
}

/// One pre-flight check (same shape as the TypeScript `PreflightCheck`).
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct PreflightCheck {
    pub id: String,
    /// "ok", "warn" or "bad".
    pub level: String,
    pub label: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreflightEvent<'a> {
    pub v: u32,
    pub event: &'static str,
    pub preflight: &'a Preflight,
}

pub fn preflight_event(preflight: &Preflight) -> PreflightEvent<'_> {
    PreflightEvent { v: PROTOCOL_VERSION, event: "preflight", preflight }
}

/// The pre-flight line for these platforms, word for word as the TypeScript
/// `preflightSummary` (packages/protocol/fixtures/preflight-summary.json).
pub fn preflight_summary(destinations: &[PreflightDestination]) -> String {
    if destinations.is_empty() {
        return "No streaming platforms are set up.".into();
    }
    let name = |d: &PreflightDestination| if d.name.is_empty() { d.id.clone() } else { d.name.clone() };
    let fits: Vec<String> = destinations
        .iter()
        .filter(|d| !d.quality.is_empty())
        .map(|d| format!("{} {}", name(d), if d.quality == "audio" { "audio only" } else { &d.quality }))
        .collect();
    let paused: Vec<String> = destinations.iter().filter(|d| d.quality.is_empty()).map(name).collect();
    let paused_line =
        if paused.is_empty() { String::new() } else { format!("{} won't fit and would be paused.", paused.join(", ")) };
    match (fits.is_empty(), paused.is_empty()) {
        (true, _) => paused_line,
        (false, true) => fits.join(" + "),
        (false, false) => format!("{}. {paused_line}", fits.join(" + ")),
    }
}

// Built once per command and sent straight away; boxing the ok case would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Ack {
    Ok {
        v: u32,
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<StateSnapshot>,
        #[serde(flatten)]
        extras: AckExtras,
    },
    Err {
        v: u32,
        id: String,
        ok: bool,
        code: ErrorCode,
        message: String,
    },
}

/// Unsolicited message to connections that sent `state.subscribe`; it has no `id`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateEvent {
    pub v: u32,
    pub event: &'static str,
    pub state: StateSnapshot,
}

pub fn state_event(state: StateSnapshot) -> StateEvent {
    StateEvent { v: PROTOCOL_VERSION, event: "state", state }
}

/// Live audio levels, relayed to subscribers for meters.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LevelsEvent<'a> {
    pub v: u32,
    pub event: &'static str,
    pub levels: &'a Levels,
}

pub fn levels_event(levels: &Levels) -> LevelsEvent<'_> {
    LevelsEvent { v: PROTOCOL_VERSION, event: "levels", levels }
}

pub fn ack(id: &str, state: Option<StateSnapshot>) -> Ack {
    ack_with(id, state, AckExtras::default())
}

pub fn ack_with(id: &str, state: Option<StateSnapshot>, extras: AckExtras) -> Ack {
    Ack::Ok { v: PROTOCOL_VERSION, id: id.into(), ok: true, state, extras }
}

/// To the church computer's subscribers whenever pairing opens, closes or gets a request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairingEvent {
    pub v: u32,
    pub event: &'static str,
    pub pairing: PairingStatus,
}

pub fn pairing_event(pairing: PairingStatus) -> PairingEvent {
    PairingEvent { v: PROTOCOL_VERSION, event: "pairing", pairing }
}

/// To the device that asked to pair, once the church computer answers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResultEvent {
    pub v: u32,
    pub event: &'static str,
    pub approved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_id: Option<String>,
}

pub fn pair_result_event(approved: Option<(&str, &str)>) -> PairResultEvent {
    PairResultEvent {
        v: PROTOCOL_VERSION,
        event: "pair.result",
        approved: approved.is_some(),
        device_id: approved.map(|(device, _)| device.to_string()),
        daemon_id: approved.map(|(_, daemon)| daemon.to_string()),
    }
}

/// What a device signs to prove it holds its key (same as the TypeScript `authMessage`).
pub fn auth_message(daemon_id: &str, challenge: &str) -> String {
    format!("jivvy-live auth v1\n{daemon_id}\n{challenge}")
}

pub fn nack(id: &str, code: ErrorCode, message: impl Into<String>) -> Ack {
    Ack::Err { v: PROTOCOL_VERSION, id: id.into(), ok: false, code, message: message.into() }
}

/// A JSON number that JavaScript's `Number.isInteger` would accept, as u64 when non-negative.
fn js_integer(v: &Value) -> Option<f64> {
    let f = v.as_f64()?;
    (f.is_finite() && f.fract() == 0.0).then_some(f)
}

fn is_version_supported(v: &Value) -> Option<u32> {
    let f = js_integer(v)?;
    (f >= MIN_SUPPORTED_VERSION as f64 && f <= PROTOCOL_VERSION as f64).then_some(f as u32)
}

fn err(code: ErrorCode, message: &str, id: Option<String>) -> ParseError {
    ParseError { code, message: message.into(), id }
}

fn parse_command(raw: Option<&Value>) -> Result<Command, (ErrorCode, &'static str)> {
    let Some(obj) = raw.and_then(Value::as_object) else {
        return Err((ErrorCode::BadMessage, "command.type missing"));
    };
    let Some(ty) = obj.get("type").and_then(Value::as_str) else {
        return Err((ErrorCode::BadMessage, "command.type missing"));
    };
    Ok(match ty {
        "slide.next" => Command::SlideNext,
        "slide.prev" => Command::SlidePrev,
        "stream.start" => Command::StreamStart,
        "stream.stop" => Command::StreamStop,
        "state.get" => Command::StateGet,
        "state.subscribe" => Command::StateSubscribe,
        "pair.open" => Command::PairOpen,
        "pair.close" => Command::PairClose,
        "device.list" => Command::DeviceList,
        "stream.preflight" => Command::StreamPreflight,
        "runsheet.get" => Command::RunSheetGet,
        "license.get" => Command::LicenseGet,
        "license.set" => match obj.get("key").and_then(Value::as_str).map(str::trim) {
            Some(key) if is_license_key_shape(key) => Command::LicenseSet { key: LicenseKey(key.into()) },
            _ => return Err((ErrorCode::BadArguments, "license.set needs a key: the JL1. text from live.jivvy.org")),
        },
        "media.put" => {
            let offset = obj.get("offset").and_then(js_integer).filter(|f| *f >= 0.0);
            match (media_id(obj.get("id")), offset, base64_chunk(obj.get("data"))) {
                (Some(id), Some(offset), Some(data)) => Command::MediaPut {
                    id,
                    offset: offset as u64,
                    data,
                    last: obj.get("last").and_then(Value::as_bool) == Some(true),
                },
                _ => {
                    return Err((
                        ErrorCode::BadArguments,
                        "media.put needs an id (SHA-256), an offset and base64 data",
                    ));
                }
            }
        }
        "media.has" => {
            let ids = obj.get("ids").and_then(Value::as_array).filter(|a| a.len() <= MAX_RUNSHEET_SLIDES);
            match ids.and_then(|a| a.iter().map(|v| media_id(Some(v))).collect::<Option<Vec<_>>>()) {
                Some(ids) => Command::MediaHas { ids },
                None => return Err((ErrorCode::BadArguments, "media.has needs ids: SHA-256 ids")),
            }
        }
        "doc.list" => match doc_kind(obj.get("kind")) {
            Some(kind) => Command::DocList { kind },
            None => return Err((ErrorCode::BadArguments, "doc commands need a kind: 1-32 of a-z 0-9 -")),
        },
        "doc.get" | "doc.delete" => {
            let (Some(kind), Some(id)) = (doc_kind(obj.get("kind")), id_field(obj.get("id"))) else {
                return Err((ErrorCode::BadArguments, "doc commands need kind and id"));
            };
            if ty == "doc.get" {
                Command::DocGet { kind, id }
            } else {
                let base_rev = base_rev(obj.get("baseRev"))
                    .map_err(|_| (ErrorCode::BadArguments, "baseRev must be a whole number from 0"))?;
                Command::DocDelete { kind, id, base_rev }
            }
        }
        "doc.put" => {
            let kind = doc_kind(obj.get("kind"));
            let id = id_field(obj.get("id"));
            let title = plain_text(obj.get("title"), MAX_TITLE);
            let body = obj.get("body").and_then(Value::as_object);
            let rev = base_rev(obj.get("baseRev"));
            let (Some(kind), Some(id), Some(title), Some(body), Ok(base_rev)) = (kind, id, title, body, rev) else {
                return Err((
                    ErrorCode::BadArguments,
                    "doc.put needs kind, id, title, a body object and an optional baseRev",
                ));
            };
            if serde_json::to_vec(body).map_or(usize::MAX, |b| b.len()) > MAX_DOC_BYTES {
                return Err((ErrorCode::BadArguments, "a document's body can be at most 512 KB"));
            }
            Command::DocPut { kind, id, title, body: body.clone(), base_rev }
        }
        "runsheet.set" => match run_sheet(obj.get("runSheet")) {
            Some(run_sheet) => Command::RunSheetSet { run_sheet },
            None => return Err((ErrorCode::BadArguments, "runsheet.set needs a valid runSheet")),
        },
        "pair.request" => match (public_key(obj.get("publicKey")), device_name(obj.get("name"))) {
            (Some(public_key), Some(name)) => Command::PairRequest { public_key, name },
            _ => {
                return Err((
                    ErrorCode::BadArguments,
                    "pair.request needs a P-256 publicKey and a name of 1-64 characters",
                ));
            }
        },
        "pair.approve" | "pair.deny" => match id_field(obj.get("requestId")) {
            Some(request_id) if ty == "pair.approve" => Command::PairApprove { request_id },
            Some(request_id) => Command::PairDeny { request_id },
            None => return Err((ErrorCode::BadArguments, "needs a requestId")),
        },
        "auth.hello" => match public_key(obj.get("publicKey")) {
            Some(public_key) => Command::AuthHello { public_key },
            None => return Err((ErrorCode::BadArguments, "auth.hello needs a P-256 publicKey")),
        },
        "auth.prove" => match signature(obj.get("signature")) {
            Some(signature) => Command::AuthProve { signature },
            None => return Err((ErrorCode::BadArguments, "auth.prove needs a 64-byte signature")),
        },
        "device.remove" => match id_field(obj.get("deviceId")) {
            Some(device_id) => Command::DeviceRemove { device_id },
            None => return Err((ErrorCode::BadArguments, "device.remove needs a deviceId")),
        },
        "recording.start" => Command::RecordingStart,
        "recording.stop" => Command::RecordingStop,
        "recording.report" => {
            let status = match obj.get("status").and_then(Value::as_str) {
                Some("off") => RecordingStatus::Off,
                Some("recording") => RecordingStatus::Recording,
                Some("waiting") => RecordingStatus::Waiting,
                _ => return Err((ErrorCode::BadArguments, "recording.report needs status off, recording or waiting")),
            };
            Command::RecordingReport { status, low_disk: obj.get("lowDisk").and_then(Value::as_bool) == Some(true) }
        }
        "stream.report" => {
            let status = match obj.get("status").and_then(Value::as_str) {
                Some("off") => StreamStatus::Off,
                Some("live") => StreamStatus::Live,
                Some("reconnecting") => StreamStatus::Reconnecting,
                _ => return Err((ErrorCode::BadArguments, "stream.report needs status off, live or reconnecting")),
            };
            let destinations = match obj.get("destinations") {
                None => None,
                Some(v) => match destination_list(v) {
                    Some(list) => Some(list),
                    None => {
                        return Err((
                            ErrorCode::BadArguments,
                            "stream.report destinations: up to 16 of { id, name, state, quality }",
                        ));
                    }
                },
            };
            Command::StreamReport { status, destinations }
        }
        "media.levels" => match (db_list(obj.get("peakDb")), db_list(obj.get("rmsDb"))) {
            (Some(peak_db), Some(rms_db)) if peak_db.len() == rms_db.len() => {
                Command::MediaLevels(Levels { peak_db, rms_db })
            }
            _ => return Err((ErrorCode::BadArguments, "media.levels needs peakDb and rmsDb of equal length")),
        },
        "slide.goto" => match obj.get("index").and_then(js_integer) {
            Some(i) if i >= 0.0 => Command::SlideGoto { index: i as u64 },
            _ => return Err((ErrorCode::BadArguments, "slide.goto needs a non-negative integer index")),
        },
        "output.black" => match obj.get("on").and_then(Value::as_bool) {
            Some(on) => Command::OutputBlack { on },
            None => return Err((ErrorCode::BadArguments, "output.black needs boolean on")),
        },
        _ => return Err((ErrorCode::UnknownCommand, "unknown command")),
    })
}

/// Parse and validate one incoming message. Never panics.
pub fn parse_envelope(input: &str) -> Result<Envelope, ParseError> {
    let raw: Value = serde_json::from_str(input).map_err(|_| err(ErrorCode::BadMessage, "invalid JSON", None))?;
    let Some(obj): Option<&Map<String, Value>> = raw.as_object() else {
        return Err(err(ErrorCode::BadMessage, "message must be an object", None));
    };
    let id = obj.get("id").and_then(Value::as_str).map(String::from);
    let ts = obj.get("ts").and_then(Value::as_f64);
    let v = obj.get("v").filter(|v| v.is_number());
    let (Some(id_s), Some(ts), Some(v)) = (id.clone().filter(|s| !s.is_empty()), ts, v) else {
        return Err(err(ErrorCode::BadMessage, "v, id and ts are required", id));
    };
    let Some(v) = is_version_supported(v) else {
        return Err(err(ErrorCode::UnsupportedVersion, "version not supported", Some(id_s)));
    };
    let command = parse_command(obj.get("command")).map_err(|(code, m)| err(code, m, Some(id_s.clone())))?;
    Ok(Envelope { v, id: id_s, ts, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = include_str!("../../../packages/protocol/fixtures/parse-cases.json");

    /// JSON equality as JavaScript sees it: `-6` and `-6.0` are the same number.
    fn js_equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| js_equal(p, q)),
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| js_equal(v, w)))
            }
            _ => a == b,
        }
    }

    #[test]
    fn matches_every_shared_conformance_case() {
        let doc: Value = serde_json::from_str(FIXTURES).unwrap();
        let cases = doc["cases"].as_array().unwrap();
        assert!(cases.len() > 20);
        for c in cases {
            let name = c["name"].as_str().unwrap();
            let expect = &c["expect"];
            match parse_envelope(c["input"].as_str().unwrap()) {
                Ok(e) => {
                    assert!(expect["ok"].as_bool().unwrap(), "{name}: parsed but expected an error");
                    let got = serde_json::to_value(&e.command).unwrap();
                    assert!(js_equal(&got, &expect["command"]), "{name}: got {got}, expected {}", expect["command"]);
                }
                Err(e) => {
                    assert!(!expect["ok"].as_bool().unwrap(), "{name}: expected ok, got {e:?}");
                    assert_eq!(serde_json::to_value(e.code).unwrap(), expect["code"], "{name}");
                    assert_eq!(e.id.as_deref(), expect.get("id").and_then(Value::as_str), "{name}: id");
                }
            }
        }
    }

    #[test]
    fn stream_alerts_match_the_typescript_ones_word_for_word() {
        const ALERTS: &str = include_str!("../../../packages/protocol/fixtures/stream-alerts.json");
        let doc: Value = serde_json::from_str(ALERTS).unwrap();
        let list = |v: &Value| -> Vec<StreamDestination> {
            destination_list(v).unwrap_or_else(|| panic!("a valid list: {v}"))
        };
        let cases = doc["cases"].as_array().unwrap();
        assert!(cases.len() >= 5);
        for c in cases {
            let got = serde_json::to_value(stream_alerts(&list(&c["before"]), &list(&c["after"]))).unwrap();
            assert_eq!(got, c["expect"], "{}", c["name"]);
        }
    }

    #[test]
    fn a_preflight_with_checks_is_sent_exactly_as_the_fixture_says() {
        const EVENT: &str = include_str!("../../../packages/protocol/fixtures/preflight-event.json");
        let doc: Value = serde_json::from_str(EVENT).unwrap();
        let p = &doc["event"]["preflight"];
        let preflight = Preflight {
            status: "done",
            upload_kbps: p["uploadKbps"].as_f64(),
            measured: p["measured"].as_bool(),
            measured_at: p["measuredAt"].as_u64(),
            destinations: serde_json::from_value(p["destinations"].clone()).unwrap(),
            summary: p["summary"].as_str().unwrap().into(),
            message: p["message"].as_str().map(Into::into),
            checks: serde_json::from_value(p["checks"].clone()).unwrap(),
            stopped: None,
        };
        // Stopped by the stream starting: exactly what the daemon sends then.
        let stopped = serde_json::to_value(preflight_event(&crate::preflight::stopped())).unwrap();
        assert_eq!(stopped, doc["stopped"]);
        let mut sent = serde_json::to_value(preflight_event(&preflight)).unwrap();
        let mut expect = doc["event"].clone();
        // A number either way: 12000.0 here, 12000 in the fixture.
        let kbps =
            |v: &mut Value| v["preflight"].as_object_mut().unwrap().remove("uploadKbps").and_then(|k| k.as_f64());
        assert_eq!(kbps(&mut sent), kbps(&mut expect));
        assert_eq!(sent, expect);
    }

    #[test]
    fn preflight_summaries_match_the_typescript_ones_word_for_word() {
        const CASES: &str = include_str!("../../../packages/protocol/fixtures/preflight-summary.json");
        let doc: Value = serde_json::from_str(CASES).unwrap();
        for c in doc["cases"].as_array().unwrap() {
            let list: Vec<PreflightDestination> = serde_json::from_value(c["destinations"].clone()).unwrap();
            assert_eq!(preflight_summary(&list), c["expect"].as_str().unwrap(), "{}", c["name"]);
        }
    }

    #[test]
    fn acks_serialize_like_the_typescript_ones() {
        let state = StateSnapshot {
            slide_index: 2,
            black: false,
            stream: StreamStatus::Off,
            stream_wanted: false,
            recording: RecordingStatus::Off,
            recording_wanted: false,
            recording_low_disk: false,
            stream_destinations: Vec::new(),
            run_sheet_id: None,
            run_sheet_rev: None,
            slide_count: None,
            slide: None,
            theme: None,
        };
        assert_eq!(
            serde_json::to_string(&ack("x", Some(state))).unwrap(),
            r#"{"v":1,"id":"x","ok":true,"state":{"slideIndex":2,"black":false,"stream":"off","streamWanted":false,"recording":"off","recordingWanted":false,"recordingLowDisk":false}}"#
        );
        assert_eq!(serde_json::to_string(&ack("x", None)).unwrap(), r#"{"v":1,"id":"x","ok":true}"#);
        let state = StateSnapshot {
            slide_index: 1,
            black: true,
            stream: StreamStatus::Live,
            stream_wanted: true,
            recording: RecordingStatus::Recording,
            recording_wanted: true,
            recording_low_disk: true,
            stream_destinations: Vec::new(),
            run_sheet_id: None,
            run_sheet_rev: None,
            slide_count: None,
            slide: None,
            theme: None,
        };
        assert_eq!(
            serde_json::to_string(&state_event(state)).unwrap(),
            r#"{"v":1,"event":"state","state":{"slideIndex":1,"black":true,"stream":"live","streamWanted":true,"recording":"recording","recordingWanted":true,"recordingLowDisk":true}}"#
        );
        assert_eq!(
            serde_json::to_string(&nack("x", ErrorCode::Unavailable, "no")).unwrap(),
            r#"{"v":1,"id":"x","ok":false,"code":"unavailable","message":"no"}"#
        );
    }
}
