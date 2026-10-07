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
        assert_eq!(s, Some(LiveState { slide_index: 4, black: true }));
        assert!(state_in(r#"{"v":1,"id":"x","ok":true,"state":{"slideIndex":0,"black":false}}"#).is_some());
        assert_eq!(state_in(r#"{"v":1,"event":"something.new"}"#), None);
        assert_eq!(state_in("garbage"), None);
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
        assert_eq!(next(), Update::State(LiveState { slide_index: 0, black: false }));
        engine.lock().unwrap().handle_line(r#"{"v":1,"id":"r","ts":1,"command":{"type":"slide.goto","index":7}}"#);
        assert_eq!(next(), Update::State(LiveState { slide_index: 7, black: false }));
    }
}
