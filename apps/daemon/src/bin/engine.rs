//! `jivvy-engine`: the live engine process. Normally started by `jivvy-watchdog`.
//!
//! jivvy-engine [--data-dir DIR] [--listen ADDR] [--slides N] [--supervised]
//!              [--listen-ws ADDR] [--ws-origin ORIGIN]... [--ws-cert PEM --ws-key PEM]
//!              [--upload-test-url URL]
//!
//! `--listen-ws` also serves the protocol over WebSocket (off by default). Browsers may use
//! it only from an origin given with `--ws-origin`; programs always may. With `--ws-cert` and
//! `--ws-key` it is `wss://` only, with that certificate; replacing the files renews it
//! without a restart.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jivvy_daemon::engine::{self, Engine};
use jivvy_daemon::snapshot::Store;
use jivvy_daemon::tls::TlsFiles;
use jivvy_daemon::{DEFAULT_LISTEN, HEARTBEAT_LINE, default_data_dir, log};

fn main() {
    let mut data_dir = default_data_dir();
    let mut listen = DEFAULT_LISTEN.to_string();
    // Until run sheets live in the local database, the slide count comes from here.
    let mut slides: u64 = 100;
    let mut supervised = false;
    let mut listen_ws: Option<String> = None;
    let mut ws_access = engine::WsAccess::default();
    let (mut ws_cert, mut ws_key): (Option<PathBuf>, Option<PathBuf>) = (None, None);
    let mut upload_test_url: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--data-dir" => data_dir = PathBuf::from(val()),
            "--listen" => {
                listen = val();
                // Trusted, so for this computer only; devices use --listen-ws.
                if !engine::is_loopback_only(&listen) {
                    fail(&format!("--listen must be a loopback address such as 127.0.0.1:47800, not {listen}"));
                }
            }
            "--slides" => slides = val().parse().unwrap_or_else(|_| fail("--slides must be a number")),
            "--supervised" => supervised = true,
            "--listen-ws" => listen_ws = Some(val()),
            "--ws-origin" => ws_access.origins.push(val()),
            // The built web app to serve on the WebSocket port (default: `web` beside this program).
            "--web-dir" => ws_access.web_dir = Some(PathBuf::from(val())),
            "--ws-cert" => ws_cert = Some(PathBuf::from(val())),
            "--ws-key" => ws_key = Some(PathBuf::from(val())),
            // Pre-flight's upload test (tests point it at a local stand-in).
            "--upload-test-url" => upload_test_url = Some(val()),
            other => fail(&format!("unknown flag {other}")),
        }
    }

    let ws_tls = match (ws_cert, ws_key) {
        (Some(cert), Some(key)) => Some(Arc::new(TlsFiles::new(cert, key))),
        (None, None) => None,
        _ => fail("--ws-cert and --ws-key go together"),
    };

    let store = Store::new(&data_dir).unwrap_or_else(|e| fail(&format!("data dir {}: {e}", data_dir.display())));
    let (snap, source) = store.load();
    log("engine", format!("resuming slide {} black={} from {source:?} snapshot", snap.slide_index, snap.black));
    let engine = Arc::new(Mutex::new(Engine::new(store, snap, slides)));
    {
        let mut e = engine.lock().unwrap();
        e.set_handle(Arc::downgrade(&engine));
        if let Some(url) = upload_test_url {
            e.set_upload_test_url(url);
        }
    }

    // Keeps the computer awake while a service is on. One thread for the engine's whole life:
    // Windows ties the request to the thread that made it.
    {
        let engine = engine.clone();
        std::thread::spawn(move || {
            let mut on = false;
            loop {
                let now = engine.lock().unwrap_or_else(|p| p.into_inner()).in_service();
                if now != on {
                    on = now;
                    jivvy_daemon::power::keep_awake(on);
                    log(
                        "engine",
                        if on { "service on: keeping the computer awake" } else { "service off: sleep allowed" },
                    );
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

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

    // Without a certificate, pairing codes and proofs would cross the network in plain text:
    // plain ws:// only on this computer. Refused, not fatal: a bad setting never stops a service.
    let listen_ws = listen_ws.filter(|addr| {
        let ok = ws_tls.is_some() || engine::is_loopback_only(addr);
        if !ok {
            log(
                "engine",
                format!("WebSocket channel {addr}: needs --ws-cert and --ws-key off this computer; not started"),
            );
        }
        ok
    });
    if ws_access.web_dir.is_none() {
        let beside = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("web")));
        ws_access.web_dir = beside.filter(|d| d.join("index.html").is_file());
    }
    if let Some(addr) = listen_ws {
        match &ws_access.web_dir {
            Some(dir) => log("engine", format!("serving the web app from {}", dir.display())),
            None => log("engine", "no web app to serve (--web-dir); devices use app.jivvy.org"),
        }
        let (engine, access) = (engine.clone(), Arc::new(ws_access));
        if let Some(tls) = &ws_tls {
            tls.config(); // logs now if the certificate can't be used, not at the first device
        }
        // Its own thread, retrying: a WebSocket port that's busy never stops the engine or
        // its TCP channel.
        std::thread::spawn(move || {
            loop {
                match engine::bind(&addr, Duration::from_secs(2)) {
                    Ok(l) => {
                        let scheme = if ws_tls.is_some() { "wss" } else { "ws" };
                        log("engine", format!("WebSocket channel on {scheme}://{addr}"));
                        engine::serve_ws(l, engine.clone(), access.clone(), ws_tls.clone());
                    }
                    Err(e) => log("engine", format!("WebSocket channel {addr}: {e}; retrying")),
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
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
        let mut guard = guard;
        guard.tick(); // e.g. a stream report going stale reaches the screens
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
