//! `jivvy-engine`: the live engine process. Normally started by `jivvy-watchdog`.
//!
//! jivvy-engine [--data-dir DIR] [--listen ADDR] [--slides N] [--supervised]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jivvy_daemon::engine::{self, Engine};
use jivvy_daemon::snapshot::Store;
use jivvy_daemon::{DEFAULT_LISTEN, HEARTBEAT_LINE, log};

fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(d).join("JivvyLive");
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("jivvy-live");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/jivvy-live")
}

fn main() {
    let mut data_dir = default_data_dir();
    let mut listen = DEFAULT_LISTEN.to_string();
    // Until run sheets live in the local database, the slide count comes from here.
    let mut slides: u64 = 100;
    let mut supervised = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--data-dir" => data_dir = PathBuf::from(val()),
            "--listen" => listen = val(),
            "--slides" => slides = val().parse().unwrap_or_else(|_| fail("--slides must be a number")),
            "--supervised" => supervised = true,
            other => fail(&format!("unknown flag {other}")),
        }
    }

    let store = Store::new(&data_dir).unwrap_or_else(|e| fail(&format!("data dir {}: {e}", data_dir.display())));
    let (snap, source) = store.load();
    log("engine", format!("resuming slide {} black={} from {source:?} snapshot", snap.slide_index, snap.black));
    let engine = Arc::new(Mutex::new(Engine::new(store, snap, slides)));

    if supervised {
        // The watchdog holds our stdin open; end-of-file means it is gone, so exit
        // rather than keep the command port from the next engine.
        std::thread::spawn(|| {
            let mut sink = [0u8; 64];
            while matches!(std::io::stdin().read(&mut sink), Ok(n) if n > 0) {}
            log("engine", "watchdog gone; exiting");
            std::process::exit(0);
        });
        let (engine, hang_file) = (engine.clone(), data_dir.join("hang-once"));
        std::thread::spawn(move || heartbeat(&engine, &hang_file));
    }

    let listener =
        engine::bind(&listen, Duration::from_secs(2)).unwrap_or_else(|e| fail(&format!("listen {listen}: {e}")));
    log("engine", format!("listening on {listen}"));
    engine::serve(listener, engine);
}

/// Tells the watchdog we're alive four times a second. Taking the engine lock first means
/// a deadlocked engine stops the heartbeat, so the watchdog restarts it.
fn heartbeat(engine: &Mutex<Engine>, hang_file: &std::path::Path) {
    let test_hooks = std::env::var_os("JIVVY_TEST_HOOKS").is_some();
    let mut out = std::io::stdout();
    loop {
        let guard = engine.lock().unwrap_or_else(|p| p.into_inner());
        if test_hooks && std::fs::remove_file(hang_file).is_ok() {
            // Chaos test hook: simulate a deadlock while holding the engine lock.
            log("engine", "test hook: hanging");
            let _hold = guard;
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        drop(guard);
        if writeln!(out, "{HEARTBEAT_LINE}").and_then(|_| out.flush()).is_err() {
            std::process::exit(0); // watchdog closed our stdout
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn fail(msg: &str) -> ! {
    log("engine", msg);
    std::process::exit(2);
}
