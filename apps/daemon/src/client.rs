//! Follows the engine's state over the command channel, for processes that show it
//! (output windows today). Reconnects forever; while the engine restarts, callers keep
//! showing the last state they got rather than going blank.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// The part of the engine's state a screen needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    pub slide_index: u64,
    pub black: bool,
    /// The operator asked for the stream to be on. Older engines don't send it.
    #[serde(default)]
    pub stream_wanted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    Connected,
    Disconnected,
    State(LiveState),
}

const SUBSCRIBE: &[u8] = b"{\"v\":1,\"id\":\"outputs\",\"ts\":0,\"command\":{\"type\":\"state.subscribe\"}}\n";

/// The state carried by an ack or a `state` event, if any.
pub fn state_in(line: &str) -> Option<LiveState> {
    let v: Value = serde_json::from_str(line).ok()?;
    serde_json::from_value(v.get("state")?.clone()).ok()
}

/// One connection's lifetime. Sets `connected` once the engine has answered.
fn follow_once(addr: &str, on: &mut impl FnMut(Update), connected: &mut bool) -> std::io::Result<()> {
    let target = addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::other("no address"))?;
    let mut stream = TcpStream::connect_timeout(&target, Duration::from_millis(500))?;
    stream.set_nodelay(true)?;
    stream.write_all(SUBSCRIBE)?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if !*connected {
            *connected = true;
            on(Update::Connected);
        }
        if let Some(state) = state_in(&line) {
            on(Update::State(state));
        }
    }
    Ok(())
}

/// Runs forever on the calling thread, reporting every change through `on`. `Disconnected`
/// comes after every connection that was up, and once at start if the engine isn't there.
pub fn follow(addr: &str, mut on: impl FnMut(Update)) -> ! {
    let mut reported_down = false;
    loop {
        let mut connected = false;
        let _ = follow_once(addr, &mut on, &mut connected);
        if connected || !reported_down {
            on(Update::Disconnected);
            reported_down = true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Sends the video process's reports (audio levels, stream status) to the engine from a
/// background thread, reconnecting as needed, and reports whether the engine is actually
/// acknowledging them. Reports are dropped (never queued up) while the engine is away: old
/// reports are useless, and fresh ones follow within a second.
pub struct LevelsSender {
    tx: std::sync::mpsc::SyncSender<serde_json::Value>,
    delivered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LevelsSender {
    /// While there are no levels to send (e.g. microphone off), the engine is still checked
    /// this often so `delivering` stays truthful.
    pub const KEEPALIVE: Duration = Duration::from_secs(1);

    pub fn start(addr: String) -> LevelsSender {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let delivered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d = delivered.clone();
        std::thread::spawn(move || send_levels(&addr, rx, &d));
        LevelsSender { tx, delivered }
    }

    /// Queues one levels report; dropped if the sender is busy.
    pub fn send(&self, peak_db: Vec<f64>, rms_db: Vec<f64>) {
        self.command(serde_json::json!({ "type": "media.levels", "peakDb": peak_db, "rmsDb": rms_db }));
    }

    /// Queues a stream status report ("off", "live" or "reconnecting").
    pub fn report_stream(&self, status: &str) {
        self.command(serde_json::json!({ "type": "stream.report", "status": status }));
    }

    fn command(&self, command: serde_json::Value) {
        let _ = self.tx.try_send(serde_json::json!({ "v": 1, "id": "vp", "ts": 0, "command": command }));
    }

    /// True when the engine acknowledged the most recent report or check.
    pub fn delivering(&self) -> bool {
        self.delivered.load(std::sync::atomic::Ordering::Relaxed)
    }
}

type Conn = (TcpStream, BufReader<TcpStream>);

fn connect(addr: &str) -> std::io::Result<Conn> {
    let target = addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::other("no address"))?;
    let s = TcpStream::connect_timeout(&target, Duration::from_millis(500))?;
    s.set_read_timeout(Some(Duration::from_secs(1)))?;
    s.set_nodelay(true)?;
    Ok((s.try_clone()?, BufReader::new(s)))
}

/// Sends one message and waits for its ack (request/response, so the engine's queue for
/// this connection never fills). True only if the engine answered ok.
fn exchange(conn: &mut Conn, msg: &serde_json::Value) -> bool {
    let (w, r) = conn;
    let mut ack = String::new();
    if writeln!(w, "{msg}").is_err() || r.read_line(&mut ack).unwrap_or(0) == 0 {
        return false;
    }
    serde_json::from_str::<Value>(&ack).is_ok_and(|v| v["ok"] == true)
}

fn send_levels(
    addr: &str,
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    delivered: &std::sync::atomic::AtomicBool,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut conn: Option<Conn> = None;
    loop {
        let msg = match rx.recv_timeout(LevelsSender::KEEPALIVE) {
            Ok(msg) => msg,
            Err(RecvTimeoutError::Timeout) => {
                serde_json::json!({ "v": 1, "id": "ka", "ts": 0, "command": { "type": "state.get" } })
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if conn.is_none() {
            conn = connect(addr).ok();
        }
        let ok = conn.as_mut().is_some_and(|c| exchange(c, &msg));
        if !ok {
            conn = None;
        }
        delivered.store(ok, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, bind, serve};
    use crate::snapshot::Store;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};

    #[test]
    fn reads_state_from_acks_and_events_and_ignores_the_rest() {
        let s = state_in(r#"{"v":1,"event":"state","state":{"slideIndex":4,"black":true,"stream":"off"}}"#);
        assert_eq!(s, Some(LiveState { slide_index: 4, black: true, stream_wanted: false }));
        assert!(state_in(r#"{"v":1,"id":"x","ok":true,"state":{"slideIndex":0,"black":false}}"#).is_some());
        assert_eq!(state_in(r#"{"v":1,"event":"something.new"}"#), None);
        assert_eq!(state_in("garbage"), None);
    }

    fn wait_until(what: &str, ok: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn levels_sender_reports_delivery_only_when_the_engine_acknowledges() {
        // Nothing listening: reports are accepted locally but never delivered.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap().to_string();
        drop(free);
        let sender = LevelsSender::start(addr.clone());
        for _ in 0..5 {
            sender.send(vec![-6.0], vec![-18.0]);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!sender.delivering(), "nothing reached an engine");

        // The engine comes up on that address: delivery is reported.
        let dir = std::env::temp_dir().join(format!("jivvy-levels-{}-{}", std::process::id(), crate::now_ms()));
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let listener = bind(&addr, Duration::from_secs(1)).unwrap();
        std::thread::spawn(move || serve(listener, Arc::new(Mutex::new(Engine::new(store, snap, 10)))));
        wait_until("delivery via the keepalive, with no levels being sent", || sender.delivering());

        // An engine that answers something other than ok doesn't count.
        let rejecting = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let raddr = rejecting.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for conn in rejecting.incoming().flatten() {
                let mut w = conn.try_clone().unwrap();
                for _ in BufReader::new(conn).lines().map_while(Result::ok) {
                    let _ = w.write_all(br#"{"v":1,"id":"lv","ok":false,"code":"unknown_command","message":"old"}"#);
                    let _ = w.write_all(b"\n");
                }
            }
        });
        let old_engine = LevelsSender::start(raddr);
        old_engine.send(vec![-6.0], vec![-18.0]);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!old_engine.delivering());
    }

    #[test]
    fn follows_the_engine_and_reports_disconnects() {
        let dir = std::env::temp_dir().join(format!("jivvy-client-{}-{}", std::process::id(), crate::now_ms()));
        let store = Store::new(&dir).unwrap();
        let (snap, _) = store.load();
        let engine = Arc::new(Mutex::new(Engine::new(store, snap, 10)));
        let listener = bind("127.0.0.1:0", Duration::from_secs(1)).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let e = engine.clone();
        std::thread::spawn(move || serve(listener, e));

        let (tx, rx) = channel();
        let a = addr.clone();
        std::thread::spawn(move || follow(&a, move |u| tx.send(u).unwrap()));
        let next = || rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next(), Update::Connected);
        assert_eq!(next(), Update::State(LiveState { slide_index: 0, black: false, stream_wanted: false }));
        engine.lock().unwrap().handle_line(r#"{"v":1,"id":"r","ts":1,"command":{"type":"slide.goto","index":7}}"#);
        assert_eq!(next(), Update::State(LiveState { slide_index: 7, black: false, stream_wanted: false }));
    }
}
