//! `jivvy-outputs`: fullscreen output windows (projector, TVs). Normally started by
//! `jivvy-watchdog`.
//!
//! jivvy-outputs [--data-dir DIR] [--connect ADDR] [--supervised] [--headless]
//!
//! Follows the engine over the command channel and shows its state on every planned monitor
//! (see `jivvy_daemon::outputs`). Re-checks monitors and `outputs.json` every second, so a
//! projector plugged in mid-service appears without anyone touching the computer. While
//! the engine restarts, windows keep showing the last state.
//!
//! `--headless` runs everything except the windows, reading monitors from
//! `test-displays.json` in the data directory; the chaos tests use it.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use jivvy_daemon::client::{self, LiveState, Update};
use jivvy_daemon::outputs::{Model, MonitorInfo, Placement};
use jivvy_daemon::{DEFAULT_LISTEN, HEARTBEAT_LINE, default_data_dir, log};

const OUTPUT_HTML: &str = include_str!("output.html");
const TICK: Duration = Duration::from_millis(250);
/// Monitors and config are re-checked every this many ticks.
const RESCAN_TICKS: u64 = 4;

struct Args {
    data_dir: PathBuf,
    connect: String,
    supervised: bool,
    headless: bool,
}

fn parse_args() -> Args {
    let mut a =
        Args { data_dir: default_data_dir(), connect: DEFAULT_LISTEN.into(), supervised: false, headless: false };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--data-dir" => a.data_dir = val().into(),
            "--connect" => a.connect = val(),
            "--supervised" => a.supervised = true,
            "--headless" => a.headless = true,
            other => fail(&format!("unknown flag {other}")),
        }
    }
    a
}

fn fail(msg: &str) -> ! {
    log("outputs", msg);
    std::process::exit(2);
}

/// Tells the watchdog this process is alive; exits if the watchdog is gone.
fn heartbeat() {
    let mut out = std::io::stdout();
    if writeln!(out, "{HEARTBEAT_LINE}").and_then(|_| out.flush()).is_err() {
        std::process::exit(0);
    }
}

/// Exits when the watchdog closes our stdin, so a stale process never lingers.
fn exit_when_orphaned() {
    std::thread::spawn(|| {
        let mut sink = [0u8; 64];
        while matches!(std::io::stdin().read(&mut sink), Ok(n) if n > 0) {}
        log("outputs", "watchdog gone; exiting");
        std::process::exit(0);
    });
}

fn apply(model: &mut Model, update: Update) -> Option<LiveState> {
    match update {
        Update::Connected => model.connected = true,
        Update::Disconnected => model.connected = false,
        Update::State(s) => {
            model.shown = Some(s);
            return Some(s);
        }
    }
    None
}

fn write_status(model: &mut Model) {
    if let Err(e) = model.write_status() {
        log("outputs", format!("could not write status: {e}"));
    }
}

fn main() {
    let args = parse_args();
    if let Err(e) = std::fs::create_dir_all(&args.data_dir) {
        fail(&format!("data dir {}: {e}", args.data_dir.display()));
    }
    if args.supervised {
        exit_when_orphaned();
    }
    if args.headless { run_headless(args) } else { ui::run(args) }
}

/// Test monitors for `--headless`: `test-displays.json`, re-read on every scan so tests
/// can plug and unplug monitors.
fn test_monitors(dir: &Path) -> Vec<MonitorInfo> {
    std::fs::read(dir.join("test-displays.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn run_headless(args: Args) -> ! {
    let (tx, rx) = mpsc::channel();
    let connect = args.connect.clone();
    std::thread::spawn(move || {
        client::follow(&connect, move |u| {
            let _ = tx.send(u);
        })
    });
    let mut model = Model::new(&args.data_dir);
    let mut ticks = 0u64;
    loop {
        while let Ok(u) = rx.recv_timeout(TICK) {
            apply(&mut model, u);
        }
        if args.supervised {
            heartbeat();
        }
        if ticks.is_multiple_of(RESCAN_TICKS) {
            model.refresh(test_monitors(&args.data_dir));
        }
        write_status(&mut model);
        ticks += 1;
    }
}

/// The real windows: borderless fullscreen on each planned monitor, one WebView each.
mod ui {
    use super::*;
    use tao::dpi::{PhysicalPosition, PhysicalSize};
    use tao::event::{Event, StartCause, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
    use tao::monitor::MonitorHandle;
    use tao::window::{Fullscreen, Window, WindowBuilder};
    use wry::{WebView, WebViewBuilder};

    #[derive(Debug)]
    enum UserEvent {
        Tick,
        Update(Update),
        /// An output's page finished loading and can be drawn.
        Ready(String),
    }

    struct OutputWindow {
        placement: Placement,
        webview: WebView,
        // Dropped after the webview, which closes the window.
        _window: Window,
    }

    fn monitor_info(m: &MonitorHandle, primary: Option<&str>) -> MonitorInfo {
        let (pos, size) = (m.position(), m.size());
        let name = m.name().unwrap_or_default();
        MonitorInfo {
            primary: Some(name.as_str()) == primary,
            name,
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
            scale: m.scale_factor(),
        }
    }

    fn render(w: &OutputWindow, state: LiveState) {
        let json = serde_json::to_string(&state).unwrap_or_default();
        if let Err(e) = w.webview.evaluate_script(&format!("window.jivvy && jivvy.render({json})")) {
            log("outputs", format!("output {}: render failed: {e}", w.placement.id));
        }
    }

    fn open(
        target: &EventLoopWindowTarget<UserEvent>,
        p: &Placement,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> Result<OutputWindow, String> {
        let monitor = target
            .available_monitors()
            .find(|m| m.name().as_deref() == Some(p.monitor.name.as_str()))
            .ok_or("monitor disappeared")?;
        let title = if p.label.is_empty() { format!("Jivvy Live output {}", p.id) } else { p.label.clone() };
        #[allow(unused_mut)]
        let mut builder = WindowBuilder::new()
            .with_title(title)
            .with_decorations(false)
            .with_always_on_top(true)
            .with_focused(false)
            // Physical pixels: correct with per-monitor DPI and monitors left of or above the primary.
            .with_position(PhysicalPosition::new(p.monitor.x, p.monitor.y))
            .with_inner_size(PhysicalSize::new(p.monitor.width, p.monitor.height))
            .with_fullscreen(Some(Fullscreen::Borderless(Some(monitor))));
        #[cfg(windows)]
        {
            use tao::platform::windows::WindowBuilderExtWindows;
            builder = builder.with_skip_taskbar(true);
        }
        let window = builder.build(target).map_err(|e| format!("window: {e}"))?;
        let canvas = serde_json::json!({ "w": p.canvas_width, "h": p.canvas_height, "label": p.label });
        let (id, proxy) = (p.id.clone(), proxy.clone());
        let webview = WebViewBuilder::new()
            .with_background_color((0, 0, 0, 255))
            .with_initialization_script(format!("window.__jivvyCanvas = {canvas};"))
            .with_html(OUTPUT_HTML)
            .with_ipc_handler(move |req| {
                if req.body() == "ready" {
                    let _ = proxy.send_event(UserEvent::Ready(id.clone()));
                }
            })
            .build(&window)
            .map_err(|e| format!("webview: {e}"))?;
        window.set_cursor_visible(false);
        Ok(OutputWindow { placement: p.clone(), webview, _window: window })
    }

    /// Re-plans and makes the windows match: closes ones no longer planned (or planned
    /// differently) and opens missing ones.
    fn reconcile(
        target: &EventLoopWindowTarget<UserEvent>,
        model: &mut Model,
        windows: &mut HashMap<String, OutputWindow>,
        proxy: &EventLoopProxy<UserEvent>,
    ) {
        let primary = target.primary_monitor().and_then(|m| m.name());
        let monitors = target.available_monitors().map(|m| monitor_info(&m, primary.as_deref())).collect();
        model.refresh(monitors);
        windows.retain(|id, w| {
            let keep = model.placements.iter().any(|p| p.id == *id && *p == w.placement);
            if !keep {
                log("outputs", format!("closing output {id}"));
            }
            keep
        });
        for p in model.placements.clone() {
            if windows.contains_key(&p.id) {
                continue;
            }
            match open(target, &p, proxy) {
                Ok(w) => {
                    log(
                        "outputs",
                        format!("output {} on {} at {}×{}", p.id, p.monitor.name, p.canvas_width, p.canvas_height),
                    );
                    windows.insert(p.id.clone(), w);
                }
                Err(e) => log("outputs", format!("output {} failed to open: {e}", p.id)),
            }
        }
        write_status(model);
    }

    pub fn run(args: Args) -> ! {
        let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        let proxy = event_loop.create_proxy();
        let p = proxy.clone();
        // Ticks go through the event loop, so a hung UI thread stops the heartbeat.
        std::thread::spawn(move || {
            while p.send_event(UserEvent::Tick).is_ok() {
                std::thread::sleep(TICK);
            }
        });
        let p = proxy.clone();
        let connect = args.connect.clone();
        std::thread::spawn(move || {
            client::follow(&connect, move |u| {
                let _ = p.send_event(UserEvent::Update(u));
            })
        });

        let mut model = Model::new(&args.data_dir);
        let mut windows: HashMap<String, OutputWindow> = HashMap::new();
        let mut ticks = 0u64;
        event_loop.run(move |event, target, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                Event::NewEvents(StartCause::Init) => reconcile(target, &mut model, &mut windows, &proxy),
                Event::UserEvent(UserEvent::Tick) => {
                    if args.supervised {
                        heartbeat();
                    }
                    ticks += 1;
                    if ticks.is_multiple_of(RESCAN_TICKS) {
                        reconcile(target, &mut model, &mut windows, &proxy);
                    }
                }
                Event::UserEvent(UserEvent::Update(u)) => {
                    if let Some(state) = apply(&mut model, u) {
                        windows.values().for_each(|w| render(w, state));
                    }
                    write_status(&mut model);
                }
                Event::UserEvent(UserEvent::Ready(id)) => {
                    if let (Some(w), Some(state)) = (windows.get(&id), model.shown) {
                        render(w, state);
                    }
                }
                // Outputs close only when unplugged or reconfigured, never by accident.
                Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {}
                _ => {}
            }
        })
    }
}
