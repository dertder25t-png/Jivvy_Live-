//! The live engine: owns the service state and answers protocol commands.
//!
//! Every change is saved to the snapshot store before it is acknowledged, so whatever
//! an operator saw acknowledged survives a crash. Connections that send `state.subscribe`
//! (output windows, operator screens) get a `state` event after every change. Video and
//! streaming plug in here in later items; until then `stream.start` answers `unavailable`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol::{self, Ack, Command, ErrorCode, StateSnapshot, StreamStatus};
use crate::snapshot::{Snapshot, Store};

/// Longest line accepted on the command channel; protocol messages are tiny.
const MAX_LINE: usize = 16 * 1024;
/// Messages queued for one connection before it counts as stuck and is disconnected.
const OUTBOX_CAPACITY: usize = 256;
/// A stream report older than this means the video process isn't reporting (crashed or
/// restarting): screens see "reconnecting", never a stale "live".
pub const STREAM_REPORT_STALE: Duration = Duration::from_secs(3);

/// Where messages for one connection go. All writes to a socket go through its outbox so
/// acks and events never interleave mid-line.
#[derive(Clone)]
pub struct Outbox {
    tx: SyncSender<Vec<u8>>,
    /// Closed if the client stops reading, so it reconnects and gets fresh state.
    stream: Option<Arc<TcpStream>>,
}

impl Outbox {
    pub fn new(tx: SyncSender<Vec<u8>>, stream: Option<Arc<TcpStream>>) -> Self {
        Outbox { tx, stream }
    }
}

pub struct Engine {
    state: Snapshot,
    store: Store,
    slide_count: u64,
    subscribers: Vec<Outbox>,
    /// The video process's latest report on the stream, and when it came.
    stream_report: Option<(StreamStatus, Instant)>,
    /// The view subscribers last received, so `tick` only sends real changes.
    last_sent: Option<StateSnapshot>,
}

impl Engine {
    pub fn new(store: Store, state: Snapshot, slide_count: u64) -> Self {
        let slide_count = slide_count.max(1);
        let mut state = state;
        // A snapshot from a longer run sheet must not point past the end of this one.
        state.slide_index = state.slide_index.min(slide_count - 1);
        Engine { state, store, slide_count, subscribers: Vec::new(), stream_report: None, last_sent: None }
    }

    pub fn state(&self) -> &Snapshot {
        &self.state
    }

    pub fn view(&self) -> StateSnapshot {
        self.view_at(Instant::now())
    }

    /// The state screens see. The stream is "off" unless asked for; when asked for, it is
    /// "live" only while the video process keeps reporting that it is.
    pub fn view_at(&self, now: Instant) -> StateSnapshot {
        let stream = match (self.state.stream_wanted, self.stream_report) {
            (false, _) => StreamStatus::Off,
            (true, Some((StreamStatus::Live, at))) if now.saturating_duration_since(at) < STREAM_REPORT_STALE => {
                StreamStatus::Live
            }
            (true, _) => StreamStatus::Reconnecting,
        };
        StateSnapshot {
            slide_index: self.state.slide_index,
            black: self.state.black,
            stream,
            stream_wanted: self.state.stream_wanted,
        }
    }

    /// Called a few times a second: tells subscribers when the view changed by itself
    /// (a stream report going stale).
    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    pub fn tick_at(&mut self, now: Instant) {
        if self.last_sent.as_ref() != Some(&self.view_at(now)) {
            self.broadcast_at(now);
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Handles one raw message and returns the ack to send back.
    pub fn handle_line(&mut self, line: &str) -> Ack {
        self.handle_line_from(line, None)
    }

    /// Like `handle_line`, for a connection that can receive events through `outbox`.
    pub fn handle_line_from(&mut self, line: &str, outbox: Option<&Outbox>) -> Ack {
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
            // Saved like the slide, so a crash mid-service resumes streaming by itself.
            Command::StreamStart => next.stream_wanted = true,
            Command::StreamStop => next.stream_wanted = false,
            Command::StreamReport { status } => {
                // Live status, not saved: it only means anything while it keeps coming.
                self.stream_report = Some((status, Instant::now()));
                self.tick();
                return protocol::ack(id, None);
            }
            Command::StateGet => {}
            Command::MediaLevels(levels) => {
                // Live meter data: relayed, never saved, never a state change.
                if let Ok(line) = serde_json::to_vec(&protocol::levels_event(&levels)) {
                    self.send_to_subscribers(line);
                }
                return protocol::ack(id, None);
            }
            Command::StateSubscribe => match outbox {
                Some(o) => self.subscribers.push(o.clone()),
                None => return protocol::nack(id, ErrorCode::Unavailable, "this connection can't receive events"),
            },
        }
        if next != self.state {
            next.saved_at = crate::now_ms();
            if let Err(e) = self.store.save(&next) {
                // Don't acknowledge a change we couldn't make crash-safe.
                crate::log("engine", format!("snapshot save failed: {e}"));
                return protocol::nack(id, ErrorCode::Unavailable, "could not save state; try again");
            }
            self.state = next;
            self.broadcast();
        }
        protocol::ack(id, Some(self.view()))
    }

    /// Sends the current state to every subscriber without ever blocking: a subscriber
    /// whose queue is full is disconnected (it reconnects and resubscribes), and one whose
    /// connection is gone is dropped.
    fn broadcast(&mut self) {
        self.broadcast_at(Instant::now());
    }

    fn broadcast_at(&mut self, now: Instant) {
        let view = self.view_at(now);
        if let Ok(line) = serde_json::to_vec(&protocol::state_event(view.clone())) {
            self.send_to_subscribers(line);
        }
        self.last_sent = Some(view);
    }

    fn send_to_subscribers(&mut self, mut line: Vec<u8>) {
        line.push(b'\n');
        self.subscribers.retain(|s| match s.tx.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                crate::log("engine", "subscriber stopped reading; disconnecting it");
                if let Some(stream) = &s.stream {
                    let _ = stream.shutdown(Shutdown::Both);
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
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
    let shared = Arc::new(stream.try_clone()?);
    let (tx, rx) = sync_channel::<Vec<u8>>(OUTBOX_CAPACITY);
    let outbox = Outbox::new(tx, Some(shared.clone()));
    let mut out = stream.try_clone()?;
    std::thread::spawn(move || {
        for msg in rx {
            if out.write_all(&msg).is_err() {
                break;
            }
        }
    });
    let result = read_commands(stream, engine, &outbox);
    // Unblocks the writer and tells the client we're done with this connection.
    let _ = shared.shutdown(Shutdown::Both);
    result
}

fn read_commands(stream: TcpStream, engine: &Mutex<Engine>, outbox: &Outbox) -> std::io::Result<()> {
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
            e.handle_line_from(msg, Some(outbox))
        };
        let mut bytes = serde_json::to_vec(&ack)?;
        bytes.push(b'\n');
        // Blocking here only stalls this connection, never the engine (the lock is released).
        if outbox.tx.send(bytes).is_err() {
            return Ok(());
        }
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
        assert_eq!(r["state"], json!({ "slideIndex": 7, "black": false, "stream": "off", "streamWanted": false }));
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
    fn the_stream_is_live_only_while_reports_keep_coming_and_wanted_survives_a_restart() {
        let (mut e, dir) = engine("stream", 3);
        let stream = |e: &mut Engine| send(e, json!({ "type": "state.get" }))["state"].clone();
        assert_eq!(stream(&mut e)["stream"], json!("off"));
        send(&mut e, json!({ "type": "stream.start" }));
        let s = stream(&mut e);
        assert_eq!((s["stream"].clone(), s["streamWanted"].clone()), (json!("reconnecting"), json!(true)));
        send(&mut e, json!({ "type": "stream.report", "status": "live" }));
        assert_eq!(stream(&mut e)["stream"], json!("live"));
        let later = Instant::now() + STREAM_REPORT_STALE + Duration::from_millis(1);
        assert_eq!(e.view_at(later).stream, StreamStatus::Reconnecting, "a silent video process is never shown live");
        send(&mut e, json!({ "type": "stream.report", "status": "reconnecting" }));
        assert_eq!(stream(&mut e)["stream"], json!("reconnecting"));

        // Wanted is saved; the live report is not.
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut restarted = Engine::new(store, snap, 3);
        assert!(restarted.view().stream_wanted);
        assert_eq!(restarted.view().stream, StreamStatus::Reconnecting);
        send(&mut restarted, json!({ "type": "stream.stop" }));
        assert_eq!(restarted.view().stream, StreamStatus::Off);
    }

    #[test]
    fn subscribers_hear_when_a_stream_report_goes_stale() {
        let (mut e, _) = engine("stale", 3);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        e.handle_line_from(
            r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#,
            Some(&Outbox::new(tx, None)),
        );
        send(&mut e, json!({ "type": "stream.start" }));
        send(&mut e, json!({ "type": "stream.report", "status": "live" }));
        let events: Vec<Value> = rx.try_iter().map(|b| serde_json::from_slice(&b).unwrap()).collect();
        assert_eq!(events.last().unwrap()["state"]["stream"], json!("live"));
        e.tick(); // nothing changed: nothing sent
        assert!(rx.try_recv().is_err());
        e.tick_at(Instant::now() + STREAM_REPORT_STALE + Duration::from_millis(1));
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(ev["state"]["stream"], json!("reconnecting"));
    }

    #[test]
    fn bad_input_is_nacked() {
        let (mut e, _) = engine("bad", 3);
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
    fn subscribers_get_every_change_and_stuck_or_gone_ones_never_block_the_engine() {
        let (mut e, _) = engine("subs", 1000);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        let sub = |e: &mut Engine, outbox: &Outbox| {
            e.handle_line_from(r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#, Some(outbox))
        };
        assert_eq!(serde_json::to_value(sub(&mut e, &Outbox::new(tx, None))).unwrap()["ok"], json!(true));
        send(&mut e, json!({ "type": "slide.goto", "index": 5 }));
        send(&mut e, json!({ "type": "state.get" })); // no change, no event
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            ev,
            json!({ "v": 1, "event": "state", "state": { "slideIndex": 5, "black": false, "stream": "off", "streamWanted": false } })
        );
        assert!(rx.try_recv().is_err());

        // Never reads: more changes than its queue holds, and the engine carries on.
        for i in 0..(OUTBOX_CAPACITY as u64 + 10) {
            send(&mut e, json!({ "type": "slide.goto", "index": i % 900 }));
        }
        assert_eq!(e.subscriber_count(), 0);
        drop(rx);

        // A subscriber whose connection is gone is dropped on the next change.
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        sub(&mut e, &Outbox::new(tx, None));
        drop(rx);
        send(&mut e, json!({ "type": "slide.next" }));
        assert_eq!(e.subscriber_count(), 0);

        // Without a connection there's nowhere to send events.
        assert_eq!(send(&mut e, json!({ "type": "state.subscribe" }))["code"], json!("unavailable"));
    }

    #[test]
    fn levels_are_relayed_to_subscribers_without_touching_the_saved_state() {
        let (mut e, dir) = engine("levels", 10);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        e.handle_line_from(
            r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#,
            Some(&Outbox::new(tx, None)),
        );
        let r = send(&mut e, json!({ "type": "media.levels", "peakDb": [-6.0, -7.5], "rmsDb": [-18.0, -20.0] }));
        assert_eq!(r["ok"], json!(true));
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            ev,
            json!({ "v": 1, "event": "levels", "levels": { "peakDb": [-6.0, -7.5], "rmsDb": [-18.0, -20.0] } })
        );
        assert!(!dir.join("state.json").exists(), "levels must never be written to disk");
    }

    #[test]
    fn a_subscribed_connection_receives_changes_made_by_another_device() {
        let (e, _) = engine("tcp-subs", 10);
        let listener = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || serve(listener, Arc::new(Mutex::new(e))));
        let mut screen = TcpStream::connect(addr).unwrap();
        screen.write_all(b"{\"v\":1,\"id\":\"s\",\"ts\":1,\"command\":{\"type\":\"state.subscribe\"}}\n").unwrap();
        let mut screen = BufReader::new(screen);
        let mut line = String::new();
        screen.read_line(&mut line).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], json!("s"));

        let mut remote = TcpStream::connect(addr).unwrap();
        remote
            .write_all(b"{\"v\":1,\"id\":\"r\",\"ts\":1,\"command\":{\"type\":\"slide.goto\",\"index\":3}}\n")
            .unwrap();
        line.clear();
        screen.read_line(&mut line).unwrap();
        let ev: Value = serde_json::from_str(&line).unwrap();
        assert_eq!((ev["event"].clone(), ev["state"]["slideIndex"].clone()), (json!("state"), json!(3)));
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
