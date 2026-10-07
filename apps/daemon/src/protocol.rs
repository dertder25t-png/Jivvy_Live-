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
                    assert_eq!(serde_json::to_value(&e.command).unwrap(), expect["command"], "{name}");
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
        let state = StateSnapshot { slide_index: 2, black: false, stream: StreamStatus::Off };
        assert_eq!(
            serde_json::to_string(&ack("x", Some(state))).unwrap(),
            r#"{"v":1,"id":"x","ok":true,"state":{"slideIndex":2,"black":false,"stream":"off"}}"#
        );
        assert_eq!(serde_json::to_string(&ack("x", None)).unwrap(), r#"{"v":1,"id":"x","ok":true}"#);
        assert_eq!(
            serde_json::to_string(&nack("x", ErrorCode::Unavailable, "no")).unwrap(),
            r#"{"v":1,"id":"x","ok":false,"code":"unavailable","message":"no"}"#
        );
    }
}
