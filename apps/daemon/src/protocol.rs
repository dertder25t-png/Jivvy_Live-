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
    StreamReport { status: StreamStatus },
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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSnapshot {
    pub slide_index: u64,
    pub black: bool,
    pub stream: StreamStatus,
    /// The operator asked for the stream to be on (it may be reconnecting).
    pub stream_wanted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Ack {
    Ok {
        v: u32,
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<StateSnapshot>,
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
    Ack::Ok { v: PROTOCOL_VERSION, id: id.into(), ok: true, state }
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
        "stream.report" => match obj.get("status").and_then(Value::as_str) {
            Some("off") => Command::StreamReport { status: StreamStatus::Off },
            Some("live") => Command::StreamReport { status: StreamStatus::Live },
            Some("reconnecting") => Command::StreamReport { status: StreamStatus::Reconnecting },
            _ => return Err((ErrorCode::BadArguments, "stream.report needs status off, live or reconnecting")),
        },
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
    fn acks_serialize_like_the_typescript_ones() {
        let state = StateSnapshot { slide_index: 2, black: false, stream: StreamStatus::Off, stream_wanted: false };
        assert_eq!(
            serde_json::to_string(&ack("x", Some(state))).unwrap(),
            r#"{"v":1,"id":"x","ok":true,"state":{"slideIndex":2,"black":false,"stream":"off","streamWanted":false}}"#
        );
        assert_eq!(serde_json::to_string(&ack("x", None)).unwrap(), r#"{"v":1,"id":"x","ok":true}"#);
        let state = StateSnapshot { slide_index: 1, black: true, stream: StreamStatus::Live, stream_wanted: true };
        assert_eq!(
            serde_json::to_string(&state_event(state)).unwrap(),
            r#"{"v":1,"event":"state","state":{"slideIndex":1,"black":true,"stream":"live","streamWanted":true}}"#
        );
        assert_eq!(
            serde_json::to_string(&nack("x", ErrorCode::Unavailable, "no")).unwrap(),
            r#"{"v":1,"id":"x","ok":false,"code":"unavailable","message":"no"}"#
        );
    }
}
