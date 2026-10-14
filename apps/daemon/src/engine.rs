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

use crate::protocol::{self, Ack, Command, ErrorCode, RecordingStatus, StateSnapshot, StreamStatus};
use crate::snapshot::{Snapshot, Store};
use crate::tls::TlsFiles;

/// Longest line accepted on the command channel; protocol messages are tiny.
const MAX_LINE: usize = 16 * 1024;
/// Messages queued for one connection before it counts as stuck and is disconnected.
const OUTBOX_CAPACITY: usize = 256;
/// A stream report older than this means the video process isn't reporting (crashed or
/// restarting): screens see "reconnecting", never a stale "live".
pub const STREAM_REPORT_STALE: Duration = Duration::from_secs(3);
/// The same for the recording: a silent video process is never shown as recording.
pub const RECORDING_REPORT_STALE: Duration = Duration::from_secs(3);

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
    /// The video process's latest report on the recording: its status, whether the disk is
    /// running low, and when it came.
    recording_report: Option<(RecordingStatus, bool, Instant)>,
    /// The view subscribers last received, so `tick` only sends real changes.
    last_sent: Option<StateSnapshot>,
}

impl Engine {
    pub fn new(store: Store, state: Snapshot, slide_count: u64) -> Self {
        let slide_count = slide_count.max(1);
        let mut state = state;
        // A snapshot from a longer run sheet must not point past the end of this one.
        state.slide_index = state.slide_index.min(slide_count - 1);
        Engine {
            state,
            store,
            slide_count,
            subscribers: Vec::new(),
            stream_report: None,
            recording_report: None,
            last_sent: None,
        }
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
        // Recording only while the video process keeps saying it is writing; the disk
        // warning needs the same fresh report, and only matters while recording is wanted.
        let fresh =
            self.recording_report.filter(|(_, _, at)| now.saturating_duration_since(*at) < RECORDING_REPORT_STALE);
        let (recording, recording_low_disk) = match (self.state.recording_wanted, fresh) {
            (false, _) => (RecordingStatus::Off, false),
            (true, Some((RecordingStatus::Recording, low, _))) => (RecordingStatus::Recording, low),
            (true, Some((_, low, _))) => (RecordingStatus::Waiting, low),
            (true, None) => (RecordingStatus::Waiting, false),
        };
        StateSnapshot {
            slide_index: self.state.slide_index,
            black: self.state.black,
            stream,
            stream_wanted: self.state.stream_wanted,
            recording,
            recording_wanted: self.state.recording_wanted,
            recording_low_disk,
        }
    }

    /// Called a few times a second: tells subscribers when the view changed by itself
    /// (a stream or recording report going stale).
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
            // Saved like the stream: a crash mid-service resumes the recording by itself.
            Command::RecordingStart => next.recording_wanted = true,
            Command::RecordingStop => next.recording_wanted = false,
            Command::RecordingReport { status, low_disk } => {
                self.recording_report = Some((status, low_disk, Instant::now()));
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

/// How long a WebSocket connection waits for a message before checking its outbox again:
/// the most an event for a WebSocket device can wait behind a quiet socket.
const WS_POLL: Duration = Duration::from_millis(15);
/// A client that hasn't finished the WebSocket handshake by now is dropped.
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Who may open the WebSocket command channel. Programs (no `Origin` header) always may.
/// Browsers send their page's origin, and only listed origins are accepted, so a web page
/// open on a booth computer can't drive the service.
#[derive(Debug, Clone, Default)]
pub struct WsAccess {
    pub origins: Vec<String>,
}

impl WsAccess {
    pub fn allows(&self, origin: Option<&str>) -> bool {
        origin.is_none_or(|o| self.origins.iter().any(|a| a.eq_ignore_ascii_case(o)))
    }
}

/// Serves the same protocol over WebSocket: one envelope per text message in, and the same
/// acks and events out, one per text message (the TCP channel's lines, without the newline).
/// With `tls`, every connection is `wss://` with the church's certificate; there is no plain
/// fallback.
pub fn serve_ws(listener: TcpListener, engine: Arc<Mutex<Engine>>, access: Arc<WsAccess>, tls: Option<Arc<TlsFiles>>) {
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (engine, access, tls) = (engine.clone(), access.clone(), tls.clone());
        std::thread::spawn(move || {
            let _ = serve_ws_connection(stream, &engine, &access, tls.as_deref());
        });
    }
}

fn serve_ws_connection(
    stream: TcpStream,
    engine: &Mutex<Engine>,
    access: &WsAccess,
    tls: Option<&TlsFiles>,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(WS_HANDSHAKE_TIMEOUT))?;
    let shared = Arc::new(stream.try_clone()?);
    let result = match tls {
        None => ws_session(stream, &shared, engine, access),
        // No good certificate yet: refuse rather than serve in plain text.
        Some(tls) => match tls.config() {
            Some(config) => {
                let conn = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
                ws_session(rustls::StreamOwned::new(conn, stream), &shared, engine, access)
            }
            None => Ok(()),
        },
    };
    let _ = shared.shutdown(Shutdown::Both);
    result
}

/// The socket under a WebSocket's stream, plain or TLS.
trait OverTcp: Read + Write {
    fn tcp(&self) -> &TcpStream;
}

impl OverTcp for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl OverTcp for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn tcp(&self) -> &TcpStream {
        self.get_ref()
    }
}

/// One WebSocket connection over `stream` (a plain socket, or TLS over it); `socket` is a
/// clone of that socket, for the outbox to cut a stuck connection.
fn ws_session<S: OverTcp>(
    stream: S,
    socket: &Arc<TcpStream>,
    engine: &Mutex<Engine>,
    access: &WsAccess,
) -> std::io::Result<()> {
    use tungstenite::{Error, Message, handshake::server::ErrorResponse, http::StatusCode};

    // The error type is tungstenite's handshake `Callback` contract, so it can't be boxed.
    #[allow(clippy::result_large_err)]
    let check_origin = |req: &tungstenite::handshake::server::Request, resp| {
        let origin = req.headers().get("origin").map(|o| o.to_str().unwrap_or("?"));
        if access.allows(origin) {
            return Ok(resp);
        }
        crate::log("engine", format!("refused a WebSocket from origin {}", origin.unwrap_or("")));
        let mut no = ErrorResponse::new(Some("this origin may not control the service".into()));
        *no.status_mut() = StatusCode::FORBIDDEN;
        Err(no)
    };
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_LINE))
        .max_frame_size(Some(MAX_LINE));
    let Ok(mut ws) = tungstenite::accept_hdr_with_config(stream, check_origin, Some(config)) else {
        return Ok(());
    };
    // On the handle being read: on Windows a clone's timeout doesn't apply to it.
    ws.get_ref().tcp().set_read_timeout(Some(WS_POLL))?;

    let (tx, rx) = sync_channel::<Vec<u8>>(OUTBOX_CAPACITY);
    let outbox = Outbox::new(tx, Some(socket.clone()));
    let as_message = |mut bytes: Vec<u8>| {
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        Message::text(String::from_utf8_lossy(&bytes).into_owned())
    };
    loop {
        let ack = match ws.read() {
            Ok(Message::Text(text)) if text.trim().is_empty() => None,
            Ok(Message::Text(text)) => {
                // A panic in one handler must not poison the engine for every other device.
                let mut e = engine.lock().unwrap_or_else(|p| p.into_inner());
                Some(e.handle_line_from(text.trim(), Some(&outbox)))
            }
            Ok(Message::Binary(_)) => Some(protocol::nack("", ErrorCode::BadMessage, "send JSON as text messages")),
            // tungstenite has queued the close reply; the flush below sends it, and the next
            // read reports the connection closed, so the client sees a clean close.
            Ok(Message::Close(_)) => None,
            Ok(_) => None, // ping, pong: tungstenite answers pings itself
            Err(Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                None
            }
            Err(Error::Capacity(_)) => {
                let _ = ws.send(as_message(serde_json::to_vec(&protocol::nack(
                    "",
                    ErrorCode::BadMessage,
                    "message too long",
                ))?));
                break Ok(());
            }
            Err(_) => break Ok(()),
        };
        // Events first, then the ack: the same order the TCP channel writes them in.
        let mut sent = Ok(());
        while let Ok(event) = rx.try_recv() {
            sent = sent.and_then(|_| ws.write(as_message(event)));
        }
        if let Some(ack) = ack {
            let bytes = serde_json::to_vec(&ack)?;
            sent = sent.and_then(|_| ws.write(as_message(bytes)));
        }
        // Flush even if a write failed (e.g. an event after the client closed), so a queued
        // close reply still goes out.
        let flushed = ws.flush();
        match sent.and(flushed) {
            Ok(()) => {}
            Err(Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break Ok(()),
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
        assert_eq!(
            r["state"],
            json!({
                "slideIndex": 7, "black": false, "stream": "off", "streamWanted": false,
                "recording": "off", "recordingWanted": false, "recordingLowDisk": false
            })
        );
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
    fn the_recording_is_shown_only_while_reports_keep_coming_and_wanted_survives_a_restart() {
        let (mut e, dir) = engine("recording", 3);
        let view = |e: &mut Engine| send(e, json!({ "type": "state.get" }))["state"].clone();
        let report = |e: &mut Engine, status: &str, low: bool| {
            send(e, json!({ "type": "recording.report", "status": status, "lowDisk": low }))
        };
        assert_eq!(view(&mut e)["recording"], json!("off"));
        // A report with nothing asked for changes nothing: no recording, no warning.
        report(&mut e, "recording", true);
        let v = view(&mut e);
        assert_eq!((v["recording"].clone(), v["recordingLowDisk"].clone()), (json!("off"), json!(false)));

        send(&mut e, json!({ "type": "recording.start" }));
        let v = view(&mut e);
        assert_eq!((v["recording"].clone(), v["recordingWanted"].clone()), (json!("recording"), json!(true)));
        let later = Instant::now() + RECORDING_REPORT_STALE + Duration::from_millis(1);
        let silent = e.view_at(later);
        assert_eq!(silent.recording, RecordingStatus::Waiting, "a silent video process is never shown recording");
        assert!(!silent.recording_low_disk, "and its old warning goes with it");
        report(&mut e, "waiting", false);
        assert_eq!(view(&mut e)["recording"], json!("waiting"));
        report(&mut e, "recording", false);
        assert_eq!(view(&mut e)["recordingLowDisk"], json!(false));

        // Wanted is saved; the live report is not.
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let mut restarted = Engine::new(store, snap, 3);
        assert!(restarted.view().recording_wanted);
        assert_eq!(restarted.view().recording, RecordingStatus::Waiting, "until the video process says otherwise");
        send(&mut restarted, json!({ "type": "recording.stop" }));
        assert_eq!(restarted.view().recording, RecordingStatus::Off);
        assert!(!restarted.view().recording_wanted);
    }

    #[test]
    fn a_state_saved_by_an_older_daemon_still_loads_with_the_recording_off() {
        let (mut e, dir) = engine("old-state", 3);
        send(&mut e, json!({ "type": "slide.goto", "index": 2 }));
        let store = Store::new(&dir).unwrap();
        let mut old: Value = serde_json::from_slice(&std::fs::read(store.current_path()).unwrap()).unwrap();
        assert!(old.as_object_mut().unwrap().remove("recordingWanted").is_some());
        std::fs::write(store.current_path(), old.to_string()).unwrap();
        let (snap, source) = store.load();
        assert_eq!((source, snap.slide_index, snap.recording_wanted), (crate::snapshot::Source::Current, 2, false));
    }

    #[test]
    fn the_low_disk_warning_reaches_subscribers_and_clears_when_space_returns() {
        let (mut e, _) = engine("low-disk", 3);
        let (tx, rx) = sync_channel(OUTBOX_CAPACITY);
        e.handle_line_from(
            r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#,
            Some(&Outbox::new(tx, None)),
        );
        send(&mut e, json!({ "type": "recording.start" }));
        send(&mut e, json!({ "type": "recording.report", "status": "recording", "lowDisk": true }));
        let events: Vec<Value> = rx.try_iter().map(|b| serde_json::from_slice(&b).unwrap()).collect();
        let last = &events.last().unwrap()["state"];
        assert_eq!((last["recording"].clone(), last["recordingLowDisk"].clone()), (json!("recording"), json!(true)));
        send(&mut e, json!({ "type": "recording.report", "status": "recording", "lowDisk": false }));
        let ev: Value = serde_json::from_slice(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(ev["state"]["recordingLowDisk"], json!(false));
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
            json!({ "v": 1, "event": "state", "state": {
                "slideIndex": 5, "black": false, "stream": "off", "streamWanted": false,
                "recording": "off", "recordingWanted": false, "recordingLowDisk": false
            } })
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

    type Ws = tungstenite::WebSocket<TcpStream>;

    /// One engine on both channels: (TCP address, WebSocket address).
    fn serve_both(name: &str, origins: &[&str]) -> (std::net::SocketAddr, std::net::SocketAddr) {
        let (e, _) = engine(name, 10);
        let engine = Arc::new(Mutex::new(e));
        let tcp = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let ws = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addrs = (tcp.local_addr().unwrap(), ws.local_addr().unwrap());
        let access = Arc::new(WsAccess { origins: origins.iter().map(|o| o.to_string()).collect() });
        let e2 = engine.clone();
        std::thread::spawn(move || serve(tcp, e2));
        std::thread::spawn(move || serve_ws(ws, engine, access, None));
        addrs
    }

    fn ws_connect(addr: std::net::SocketAddr, origin: Option<&str>) -> Result<Ws, tungstenite::Error> {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        match tungstenite::client(req, stream) {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::HandshakeError::Failure(e)) => Err(e),
            Err(e) => panic!("handshake: {e}"),
        }
    }

    fn ws_send(ws: &mut Ws, text: &str) {
        ws.send(tungstenite::Message::text(text.to_string())).unwrap();
    }

    fn ws_next(ws: &mut Ws) -> Value {
        loop {
            match ws.read().unwrap() {
                tungstenite::Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                _ => continue,
            }
        }
    }

    fn tcp_ask(addr: std::net::SocketAddr, line: &str) -> Value {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut l = String::new();
        BufReader::new(s).read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }

    #[test]
    fn websocket_carries_the_same_envelopes_acks_and_events_as_tcp() {
        let (tcp, ws_addr) = serve_both("ws-same", &[]);
        let mut ws = ws_connect(ws_addr, None).unwrap();
        let goto = r#"{"v":1,"id":"g","ts":1,"command":{"type":"slide.goto","index":3}}"#;
        ws_send(&mut ws, goto);
        let ws_ack = ws_next(&mut ws);
        let get = r#"{"v":1,"id":"g","ts":1,"command":{"type":"state.get"}}"#;
        assert_eq!(ws_ack, tcp_ask(tcp, get), "the same ack, byte for byte, on either channel");

        // Subscribed over WebSocket, it hears a change made over TCP.
        ws_send(&mut ws, r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#);
        assert_eq!(ws_next(&mut ws)["id"], "s");
        tcp_ask(tcp, r#"{"v":1,"id":"n","ts":1,"command":{"type":"slide.next"}}"#);
        let event = ws_next(&mut ws);
        assert_eq!((event["event"].clone(), event["state"]["slideIndex"].clone()), (json!("state"), json!(4)));

        // A bad envelope is nacked the same way, and the connection stays up.
        ws_send(&mut ws, "{not json");
        assert_eq!(ws_next(&mut ws), tcp_ask(tcp, "{not json"));
        ws.send(tungstenite::Message::binary(vec![1u8, 2, 3])).unwrap();
        assert_eq!(ws_next(&mut ws)["code"], "bad_message");
        ws_send(&mut ws, get);
        assert_eq!(ws_next(&mut ws)["state"]["slideIndex"], 4);
    }

    #[test]
    fn browsers_may_connect_only_from_listed_origins() {
        let (_, ws_addr) = serve_both("ws-origin", &["https://app.jivvy.org"]);
        assert!(ws_connect(ws_addr, None).is_ok(), "programs send no Origin");
        assert!(ws_connect(ws_addr, Some("https://app.jivvy.org")).is_ok());
        match ws_connect(ws_addr, Some("https://evil.example")) {
            Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
            other => panic!("an unlisted origin is refused, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn a_client_that_closes_gets_a_clean_close_back() {
        let (_, ws_addr) = serve_both("ws-close", &[]);
        let mut ws = ws_connect(ws_addr, None).unwrap();
        ws_send(&mut ws, r#"{"v":1,"id":"s","ts":1,"command":{"type":"state.subscribe"}}"#);
        assert_eq!(ws_next(&mut ws)["id"], "s");
        ws.close(None).unwrap();
        // The server's close reply ends it cleanly (ConnectionClosed), not a dropped socket.
        let end = loop {
            match ws.read() {
                Ok(_) => continue,
                Err(e) => break e,
            }
        };
        assert!(matches!(end, tungstenite::Error::ConnectionClosed), "closed uncleanly: {end}");
    }

    #[test]
    fn an_oversized_websocket_message_is_refused() {
        let (_, ws_addr) = serve_both("ws-big", &[]);
        let mut ws = ws_connect(ws_addr, None).unwrap();
        ws_send(&mut ws, &"x".repeat(MAX_LINE + 10));
        assert_eq!(ws_next(&mut ws)["message"], "message too long");
    }

    type Wss = tungstenite::WebSocket<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>;
    const CHURCH: &str = "church.d.jivvy.org";

    /// An engine serving `wss://` with whatever is in `cert` and `key`.
    fn serve_tls(name: &str, cert: &std::path::Path, key: &std::path::Path) -> std::net::SocketAddr {
        let (e, _) = engine(name, 10);
        let ws = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = ws.local_addr().unwrap();
        let tls = Some(Arc::new(TlsFiles::new(cert, key)));
        std::thread::spawn(move || serve_ws(ws, Arc::new(Mutex::new(e)), Arc::new(WsAccess::default()), tls));
        addr
    }

    /// Connects trusting only `ca_pem`, as a browser trusts only real certificate authorities.
    fn wss_connect(addr: std::net::SocketAddr, ca_pem: &str) -> Result<Wss, String> {
        use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap()).unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let conn = rustls::ClientConnection::new(Arc::new(config), ServerName::try_from(CHURCH).unwrap()).unwrap();
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tungstenite::client(format!("wss://{CHURCH}/"), rustls::StreamOwned::new(conn, stream))
            .map(|(ws, _)| ws)
            .map_err(|e| e.to_string())
    }

    fn wss_ask(ws: &mut Wss, command: &str) -> Value {
        let msg = format!(r#"{{"v":1,"id":"q","ts":1,"command":{{"type":"{command}"}}}}"#);
        ws.send(tungstenite::Message::text(msg)).unwrap();
        loop {
            if let tungstenite::Message::Text(t) = ws.read().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }

    fn write_pair(dir: &std::path::Path, cert: &str, key: &str) {
        std::fs::write(dir.join("cert.pem"), cert).unwrap();
        std::fs::write(dir.join("key.pem"), key).unwrap();
    }

    #[test]
    fn wss_carries_the_protocol_and_never_falls_back_to_plain_ws() {
        let dir = crate::tls::tests::temp("wss");
        let (ca, cert, key) = crate::tls::tests::test_pair(CHURCH);
        write_pair(&dir, &cert, &key);
        let addr = serve_tls("wss", &dir.join("cert.pem"), &dir.join("key.pem"));

        let mut ws = wss_connect(addr, &ca).unwrap();
        assert_eq!(wss_ask(&mut ws, "slide.next")["state"]["slideIndex"], 1);
        // Events reach a quiet subscriber over TLS too.
        assert_eq!(wss_ask(&mut ws, "state.subscribe")["ok"], true);
        let mut other = wss_connect(addr, &ca).unwrap();
        let sent = Instant::now();
        for _ in 0..20 {
            wss_ask(&mut other, "slide.next");
        }
        for want in 2..=11 {
            let event = loop {
                if let tungstenite::Message::Text(t) = ws.read().unwrap() {
                    break serde_json::from_str::<Value>(&t).unwrap();
                }
            };
            assert_eq!(
                (event["event"].clone(), event["state"]["slideIndex"].clone()),
                (json!("state"), json!(want.min(9)))
            );
            if want == 9 {
                break; // clamped at the last slide: no more changes, so no more events
            }
        }
        // WS_POLL bounds the wait; seconds would mean the read timeout isn't on the TLS socket.
        assert!(sent.elapsed() < Duration::from_secs(2), "events took {:?}", sent.elapsed());
        assert!(ws_connect(addr, None).is_err(), "plain ws:// is refused on a wss channel");
        let (other_ca, _, _) = crate::tls::tests::test_pair(CHURCH);
        assert!(wss_connect(addr, &other_ca).is_err(), "a client that doesn't trust the certificate can't connect");
    }

    #[test]
    fn a_renewed_certificate_is_used_without_a_restart_and_open_connections_stay() {
        let dir = crate::tls::tests::temp("wss-renew");
        let (ca, cert, key) = crate::tls::tests::test_pair(CHURCH);
        write_pair(&dir, &cert, &key);
        let addr = serve_tls("wss-renew", &dir.join("cert.pem"), &dir.join("key.pem"));
        let mut open = wss_connect(addr, &ca).unwrap();

        let (new_ca, new_cert, new_key) = crate::tls::tests::test_pair(CHURCH);
        write_pair(&dir, &new_cert, &new_key);
        let mut renewed = wss_connect(addr, &new_ca).expect("new connections get the renewed certificate");
        assert!(wss_connect(addr, &ca).is_err(), "the old certificate is no longer served");
        assert_eq!(wss_ask(&mut renewed, "slide.next")["state"]["slideIndex"], 1);
        assert_eq!(wss_ask(&mut open, "state.get")["state"]["slideIndex"], 1, "the open connection carries on");
    }

    #[test]
    fn with_no_certificate_yet_connections_are_refused_until_it_arrives() {
        let dir = crate::tls::tests::temp("wss-later");
        let addr = serve_tls("wss-later", &dir.join("cert.pem"), &dir.join("key.pem"));
        let (ca, cert, key) = crate::tls::tests::test_pair(CHURCH);
        assert!(wss_connect(addr, &ca).is_err());
        assert!(ws_connect(addr, None).is_err(), "no certificate never means plain text");
        write_pair(&dir, &cert, &key);
        let mut ws = wss_connect(addr, &ca).expect("the certificate is picked up when it arrives");
        assert_eq!(wss_ask(&mut ws, "state.get")["ok"], true);
    }
}
