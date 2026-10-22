//! `jivvy-ctl`: one-line commands to a running daemon, for trying features by hand. It
//! speaks the same newline-delimited JSON envelopes (protocol v1) as every other client.

use serde_json::{Value, json};

use crate::DEFAULT_LISTEN;
use crate::protocol::PROTOCOL_VERSION;

pub const USAGE: &str = "\
jivvy-ctl [--connect ADDR] COMMAND      (ADDR defaults to 127.0.0.1:47800)

  next                 next slide
  back                 previous slide
  goto INDEX           go to a slide by index, from 0
  black [on|off]       black the screens (default on), or bring them back
  state                print the engine's state
  watch                print every state change and event until Ctrl+C
  stream start|stop    start or stop streaming
  record start|stop    start or stop the local recording
  pair open            let devices ask to pair for 2 minutes (watch shows their codes)
  pair close           stop accepting pairing requests
  pair approve ID      pair the device whose request has this id, once its code matches
  pair deny ID         turn a pairing request down
  preflight            test the upload and say what each platform will get (about 5 s)
  devices              list paired devices
  devices remove ID    unpair a device; its open connections end at once
  license              show the trial or license on this computer
  license set KEY      save a license key from live.jivvy.org (checked offline)

Prints the engine's answer as one JSON line; exits 1 if the engine refused the command
or can't be reached, 2 for a usage mistake.";

/// What to send, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub addr: String,
    pub command: Value,
    /// Keep printing events after the ack (`watch`).
    pub watch: bool,
    /// Wait for one event with this name after the ack, print it, and stop (`preflight`).
    pub until: Option<&'static str>,
}

/// Reads `jivvy-ctl`'s arguments (without the program name).
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut addr = DEFAULT_LISTEN.to_string();
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--connect" => addr = it.next().ok_or("--connect needs an address")?.clone(),
            "-h" | "--help" | "help" => return Err(USAGE.into()),
            w => words.push(w),
        }
    }
    let command = match words.as_slice() {
        ["next"] => json!({ "type": "slide.next" }),
        ["back"] | ["prev"] => json!({ "type": "slide.prev" }),
        ["goto", n] => {
            let index: u64 = n.parse().map_err(|_| format!("goto needs a slide index from 0, not {n:?}"))?;
            json!({ "type": "slide.goto", "index": index })
        }
        ["black"] | ["black", "on"] => json!({ "type": "output.black", "on": true }),
        ["black", "off"] => json!({ "type": "output.black", "on": false }),
        ["state"] => json!({ "type": "state.get" }),
        ["watch"] => json!({ "type": "state.subscribe" }),
        ["stream", "start"] => json!({ "type": "stream.start" }),
        ["stream", "stop"] => json!({ "type": "stream.stop" }),
        ["record", "start"] => json!({ "type": "recording.start" }),
        ["record", "stop"] => json!({ "type": "recording.stop" }),
        ["pair", "open"] => json!({ "type": "pair.open" }),
        ["pair", "close"] => json!({ "type": "pair.close" }),
        ["pair", "approve", id] => json!({ "type": "pair.approve", "requestId": id }),
        ["pair", "deny", id] => json!({ "type": "pair.deny", "requestId": id }),
        ["devices"] => json!({ "type": "device.list" }),
        ["preflight"] => json!({ "type": "stream.preflight" }),
        ["devices", "remove", id] => json!({ "type": "device.remove", "deviceId": id }),
        ["license"] => json!({ "type": "license.get" }),
        ["license", "set", key] => json!({ "type": "license.set", "key": key }),
        [] => return Err(USAGE.into()),
        other => return Err(format!("unknown command: {}\n\n{USAGE}", other.join(" "))),
    };
    let watch = command["type"] == "state.subscribe";
    let until = (command["type"] == "stream.preflight").then_some("preflight");
    Ok(Invocation { addr, command, watch, until })
}

/// The command wrapped in a protocol envelope, as one line.
pub fn envelope(command: &Value, id: &str, ts: u64) -> String {
    format!("{}\n", json!({ "v": PROTOCOL_VERSION, "id": id, "ts": ts, "command": command }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &str) -> Result<Invocation, String> {
        parse(&words.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn each_command_becomes_its_protocol_command() {
        let cases = [
            ("next", json!({ "type": "slide.next" })),
            ("back", json!({ "type": "slide.prev" })),
            ("goto 4", json!({ "type": "slide.goto", "index": 4 })),
            ("black", json!({ "type": "output.black", "on": true })),
            ("black off", json!({ "type": "output.black", "on": false })),
            ("state", json!({ "type": "state.get" })),
            ("watch", json!({ "type": "state.subscribe" })),
            ("stream start", json!({ "type": "stream.start" })),
            ("stream stop", json!({ "type": "stream.stop" })),
            ("record start", json!({ "type": "recording.start" })),
            ("record stop", json!({ "type": "recording.stop" })),
            ("pair open", json!({ "type": "pair.open" })),
            ("pair close", json!({ "type": "pair.close" })),
            ("license", json!({ "type": "license.get" })),
            ("license set JL1.x.y", json!({ "type": "license.set", "key": "JL1.x.y" })),
            ("pair approve r1", json!({ "type": "pair.approve", "requestId": "r1" })),
            ("pair deny r1", json!({ "type": "pair.deny", "requestId": "r1" })),
            ("devices", json!({ "type": "device.list" })),
            ("preflight", json!({ "type": "stream.preflight" })),
            ("devices remove d1", json!({ "type": "device.remove", "deviceId": "d1" })),
        ];
        for (words, command) in cases {
            let inv = parse_words(words).unwrap();
            assert_eq!(inv.command, command, "{words}");
            assert_eq!(inv.addr, DEFAULT_LISTEN);
            assert_eq!(inv.watch, words == "watch");
        }
    }

    #[test]
    fn connect_works_anywhere_and_mistakes_explain_themselves() {
        assert_eq!(parse_words("next --connect 127.0.0.1:9").unwrap().addr, "127.0.0.1:9");
        assert_eq!(parse_words("--connect 127.0.0.1:9 next").unwrap().addr, "127.0.0.1:9");
        assert!(parse_words("goto two").unwrap_err().contains("goto needs a slide index"));
        assert!(parse_words("stream pause").unwrap_err().contains("unknown command: stream pause"));
        assert!(parse_words("--connect").unwrap_err().contains("needs an address"));
        assert!(parse_words("").unwrap_err().starts_with("jivvy-ctl"));
    }

    #[test]
    fn the_envelope_is_one_protocol_line() {
        let line = envelope(&json!({ "type": "slide.next" }), "ctl-1", 5);
        assert!(line.ends_with('\n') && !line.trim_end().contains('\n'));
        let parsed = crate::protocol::parse_envelope(line.trim_end());
        assert!(parsed.is_ok(), "{parsed:?}");
    }
}
