//! The live engine: owns the service state and answers protocol commands.
//!
//! Every change is saved to the snapshot store before it is acknowledged, so whatever
//! an operator saw acknowledged survives a crash. Video, outputs and streaming plug in
//! here in later items; until then `stream.start` answers `unavailable`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol::{self, Ack, Command, ErrorCode, StateSnapshot, StreamStatus};
use crate::snapshot::{Snapshot, Store};

/// Longest line accepted on the command channel; protocol messages are tiny.
const MAX_LINE: usize = 16 * 1024;

pub struct Engine {
    state: Snapshot,
    store: Store,
    slide_count: u64,
}

impl Engine {
    pub fn new(store: Store, state: Snapshot, slide_count: u64) -> Self {
        let slide_count = slide_count.max(1);
        let mut state = state;
        // A snapshot from a longer run sheet must not point past the end of this one.
        state.slide_index = state.slide_index.min(slide_count - 1);
        Engine { state, store, slide_count }
    }

    pub fn state(&self) -> &Snapshot {
        &self.state
    }

    pub fn view(&self) -> StateSnapshot {
        StateSnapshot { slide_index: self.state.slide_index, black: self.state.black, stream: StreamStatus::Off }
    }

    /// Handles one raw message and returns the ack to send back.
    pub fn handle_line(&mut self, line: &str) -> Ack {
        let env = match protocol::parse_envelope(line) {
            Ok(e) => e,
            Err(e) => return protocol::nack(e.id.as_deref().unwrap_or(""), e.code, e.message),
        };
        let id = env.id.as_str();
        let mut next = self.state.clone();
        match env.command {
            Command::SlideNext => next.slide_index = (next.slide_index + 1).min(self.slide_count - 1),
            Command::SlidePrev => next.slide_index = next.slide_index.saturating_sub(1),
            Command::SlideGoto { index } => {
                if index >= self.slide_count {
                    return protocol::nack(id, ErrorCode::BadArguments, "slide out of range");
                }
                next.slide_index = index;
            }
            Command::OutputBlack { on } => next.black = on,
            Command::StreamStart | Command::StreamStop => {
                return protocol::nack(id, ErrorCode::Unavailable, "streaming is not built into this daemon yet");
            }
            Command::StateGet => {}
        }
        if next != self.state {
            next.saved_at = crate::now_ms();
            if let Err(e) = self.store.save(&next) {
                // Don't acknowledge a change we couldn't make crash-safe.
                crate::log("engine", format!("snapshot save failed: {e}"));
                return protocol::nack(id, ErrorCode::Unavailable, "could not save state; try again");
            }
            self.state = next;
        }
        protocol::ack(id, Some(self.view()))
    }
}

/// Binds the command channel, retrying briefly in case the previous engine's socket is
/// still being released by the OS after a crash.
pub fn bind(addr: &str, wait: Duration) -> std::io::Result<TcpListener> {
    let start = Instant::now();
    loop {
        match TcpListener::bind(addr) {
            Ok(l) => return Ok(l),
            Err(e) if start.elapsed() < wait => {
                crate::log("engine", format!("bind {addr} failed ({e}); retrying"));
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Accepts connections forever; one thread per connected device.
pub fn serve(listener: TcpListener, engine: Arc<Mutex<Engine>>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let engine = engine.clone();
        std::thread::spawn(move || {
            let _ = serve_connection(stream, &engine);
        });
    }
}

/// Skips input up to and including the next newline (or end of stream) without keeping
/// any of it, so a client that never sends a newline can't grow our memory: only the
/// reader's own fixed-size buffer is ever used. Returns how many bytes were skipped.
fn discard_line(reader: &mut impl BufRead) -> std::io::Result<u64> {
    let mut skipped = 0u64;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(skipped);
        }
        let (used, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        reader.consume(used);
        skipped += used as u64;
        if done {
            return Ok(skipped);
        }
    }
}

fn serve_connection(stream: TcpStream, engine: &Mutex<Engine>) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        let ack = if n > MAX_LINE && buf.last() != Some(&b'\n') {
            discard_line(&mut reader)?;
            protocol::nack("", ErrorCode::BadMessage, "message too long")
        } else {
            // Invalid UTF-8 becomes a bad_message nack, never a dropped connection.
            let line = String::from_utf8_lossy(&buf);
            let msg = line.trim();
            if msg.is_empty() {
                continue;
            }
            // A panic in one handler must not poison the engine for every other device.
            let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
            e.handle_line(msg)
        };
        let mut bytes = serde_json::to_vec(&ack)?;
        bytes.push(b'\n');
        out.write_all(&bytes)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn engine(name: &str, slides: u64) -> (Engine, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("jivvy-engine-{name}-{}-{}", std::process::id(), crate::now_ms()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        (Engine::new(store, snap, slides), dir)
    }

    fn send(e: &mut Engine, command: Value) -> Value {
        let msg = json!({ "v": 1, "id": "t", "ts": 1, "command": command }).to_string();
        serde_json::to_value(e.handle_line(&msg)).unwrap()
    }

    #[test]
    fn navigates_and_clamps_like_the_simulated_daemon() {
        let (mut e, _) = engine("nav", 5);
        send(&mut e, json!({ "type": "slide.prev" }));
        assert_eq!(e.state().slide_index, 0);
        for _ in 0..10 {
            send(&mut e, json!({ "type": "slide.next" }));
        }
        assert_eq!(e.state().slide_index, 4);
        let r = send(&mut e, json!({ "type": "slide.goto", "index": 9 }));
        assert_eq!((r["ok"].clone(), r["code"].clone()), (json!(false), json!("bad_arguments")));
    }

    #[test]
    fn every_ack_reports_state_and_changes_are_saved_before_the_ack() {
        let (mut e, dir) = engine("save", 10);
        let r = send(&mut e, json!({ "type": "slide.goto", "index": 7 }));
        assert_eq!(r["state"], json!({ "slideIndex": 7, "black": false, "stream": "off" }));
        send(&mut e, json!({ "type": "output.black", "on": true }));
        let (snap, _) = Store::new(&dir).unwrap().load();
        assert_eq!((snap.slide_index, snap.black), (7, true));
        let r = send(&mut e, json!({ "type": "state.get" }));
        assert_eq!(r["state"]["black"], json!(true));
    }

    #[test]
    fn a_new_engine_resumes_from_the_saved_snapshot() {
        let (mut e, dir) = engine("resume", 10);
        send(&mut e, json!({ "type": "slide.goto", "index": 6 }));
        drop(e);
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        assert_eq!(Engine::new(store, snap, 10).state().slide_index, 6);
    }

    #[test]
    fn a_snapshot_past_the_end_of_a_shorter_run_sheet_is_clamped() {
        let (mut e, dir) = engine("clamp", 10);
        send(&mut e, json!({ "type": "slide.goto", "index": 9 }));
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        assert_eq!(Engine::new(store, snap, 4).state().slide_index, 3);
    }

    #[test]
    fn streaming_answers_unavailable_and_bad_input_is_nacked() {
        let (mut e, _) = engine("stream", 3);
        assert_eq!(send(&mut e, json!({ "type": "stream.start" }))["code"], json!("unavailable"));
        assert_eq!(serde_json::to_value(e.handle_line("{")).unwrap()["code"], json!("bad_message"));
        let r =
            serde_json::to_value(e.handle_line(r#"{"v":9,"id":"z","ts":1,"command":{"type":"slide.next"}}"#)).unwrap();
        assert_eq!((r["code"].clone(), r["id"].clone()), (json!("unsupported_version"), json!("z")));
    }

    /// Yields `len` bytes of 'x' without allocating them, then `tail`.
    struct Flood {
        len: u64,
        tail: std::io::Cursor<Vec<u8>>,
    }

    impl Read for Flood {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.len == 0 {
                return self.tail.read(out);
            }
            let n = out.len().min(self.len as usize);
            out[..n].fill(b'x');
            self.len -= n as u64;
            Ok(n)
        }
    }

    #[test]
    fn discarding_a_huge_line_keeps_nothing_and_the_next_message_still_reads() {
        let flood = 256 * 1024 * 1024; // would be a 256 MiB allocation if it were retained
        let tail = b"\n{\"next\":1}\n".to_vec();
        let mut r = BufReader::with_capacity(8 * 1024, Flood { len: flood, tail: std::io::Cursor::new(tail) });
        assert_eq!(discard_line(&mut r).unwrap(), flood + 1);
        let mut next = String::new();
        r.read_line(&mut next).unwrap();
        assert_eq!(next, "{\"next\":1}\n");
        // A stream that ends without a newline just stops.
        let mut r = BufReader::new(Flood { len: 100_000, tail: std::io::Cursor::new(Vec::new()) });
        assert_eq!(discard_line(&mut r).unwrap(), 100_000);
    }

    #[test]
    fn serves_newline_delimited_json_over_tcp() {
        let (e, _) = engine("tcp", 5);
        let listener = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Mutex::new(e));
        std::thread::spawn(move || serve(listener, engine));
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"{\"v\":1,\"id\":\"a\",\"ts\":1,\"command\":{\"type\":\"slide.next\"}}\n\n").unwrap();
        s.write_all(&vec![b'x'; MAX_LINE + 10]).unwrap();
        s.write_all(b"\n{\"v\":1,\"id\":\"b\",\"ts\":1,\"command\":{\"type\":\"state.get\"}}\n").unwrap();
        let mut r = BufReader::new(s);
        let mut lines = Vec::new();
        for _ in 0..3 {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            lines.push(serde_json::from_str::<Value>(&l).unwrap());
        }
        assert_eq!(lines[0]["state"]["slideIndex"], json!(1));
        assert_eq!(lines[1]["message"], json!("message too long"));
        assert_eq!((lines[2]["id"].clone(), lines[2]["state"]["slideIndex"].clone()), (json!("b"), json!(1)));
    }
}
