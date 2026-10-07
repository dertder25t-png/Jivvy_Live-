//! `jivvy-video`: camera / capture card and audio input. Normally started by `jivvy-watchdog`.
//! Built only with the `video` feature (it needs GStreamer installed).
//!
//! jivvy-video [--data-dir DIR] [--connect ADDR] [--supervised]
//!
//! Camera and microphone run as separate pipelines, so one device failing never stops the
//! other. A device that is unplugged or fails is retried every second and picked up again
//! when it returns. Audio levels go to the engine about ten times a second
//! (`media.levels`), which relays them to every screen for live meters.
//!
//! This item only captures; compositing the lyric layer, encoding, recording and streaming
//! attach to these pipelines in the next Stage 1 items.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

use jivvy_daemon::media::{self, Choice, DeviceInfo, InputStatus, Kind, RawDevice, Status};
use jivvy_daemon::{DEFAULT_LISTEN, default_data_dir, exit_when_orphaned, heartbeat, log, write_atomic};

const TICK: Duration = Duration::from_millis(250);
/// Devices and config are re-checked, and failed inputs retried, every this many ticks.
const RESCAN_TICKS: u64 = 4;

struct Args {
    data_dir: PathBuf,
    connect: String,
    supervised: bool,
}

fn parse_args() -> Args {
    let mut a = Args { data_dir: default_data_dir(), connect: DEFAULT_LISTEN.into(), supervised: false };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--data-dir" => a.data_dir = val().into(),
            "--connect" => a.connect = val(),
            "--supervised" => a.supervised = true,
            other => fail(&format!("unknown flag {other}")),
        }
    }
    a
}

fn fail(msg: &str) -> ! {
    log("video", msg);
    std::process::exit(2);
}

/// One input (camera or microphone) and its pipeline.
struct Input {
    kind: Kind,
    choice: Choice,
    pipeline: Option<gst::Pipeline>,
    state: &'static str,
    detail: String,
    /// Buffers (camera) or level reports (microphone) since start, for the rate.
    count: Arc<AtomicU64>,
    last_count: u64,
    rate: f64,
}

impl Input {
    fn new(kind: Kind) -> Self {
        Input {
            kind,
            choice: Choice::None,
            pipeline: None,
            state: "off",
            detail: String::new(),
            count: Arc::default(),
            last_count: 0,
            rate: 0.0,
        }
    }

    fn name(&self) -> &'static str {
        if self.kind == Kind::Camera { "camera" } else { "microphone" }
    }

    fn stop(&mut self) {
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
    }

    /// Marks the input as failed; the next rescan retries it.
    fn wait(&mut self, detail: String) {
        self.stop();
        log("video", format!("{}: {detail}; retrying every second", self.name()));
        (self.state, self.detail, self.rate) = ("waiting", detail, 0.0);
    }

    fn status(&self) -> InputStatus {
        InputStatus { choice: self.choice.clone(), state: self.state, detail: self.detail.clone(), rate: self.rate }
    }
}

fn raw_device(d: &gst::Device) -> RawDevice {
    let mut props = std::collections::BTreeMap::new();
    if let Some(s) = d.properties() {
        for (k, v) in s.iter() {
            let text = v
                .get::<String>()
                .ok()
                .or_else(|| v.get::<bool>().ok().map(|b| b.to_string()))
                .or_else(|| v.serialize().ok().map(|g| g.to_string()));
            if let Some(t) = text {
                props.insert(k.to_string(), t);
            }
        }
    }
    RawDevice { name: d.display_name().to_string(), class: d.device_class().to_string(), props }
}

/// The GStreamer device behind a listed device id.
fn find_device(monitor: &gst::DeviceMonitor, id: &str) -> Option<gst::Device> {
    monitor.devices().into_iter().find(|d| {
        let r = raw_device(d);
        media::devices(std::slice::from_ref(&r)).first().is_some_and(|i| i.id == id)
    })
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory).build().map_err(|_| format!("GStreamer element {factory} is missing"))
}

/// Builds and starts the pipeline for an input's current choice.
fn start(input: &mut Input, monitor: &gst::DeviceMonitor) -> Result<(), String> {
    let source = match &input.choice {
        Choice::None => return Ok(()),
        Choice::Missing { name } => return Err(format!("{name} is not connected")),
        Choice::Test if input.kind == Kind::Camera => {
            let s = make("videotestsrc")?;
            s.set_property("is-live", true);
            s.set_property_from_str("pattern", "ball");
            s
        }
        Choice::Test => {
            let s = make("audiotestsrc")?;
            s.set_property("is-live", true);
            s.set_property("volume", 0.5f64);
            s
        }
        Choice::Device { id, name } => find_device(monitor, id)
            .ok_or_else(|| format!("{name} disappeared"))?
            .create_element(None)
            .map_err(|e| format!("can't open {name}: {e}"))?,
    };
    let pipeline = gst::Pipeline::new();
    let sink = make("fakesink")?;
    sink.set_property("sync", false);
    let mut chain = vec![source];
    if input.kind == Kind::Camera {
        let q = make("queue")?;
        q.set_property_from_str("leaky", "downstream");
        q.set_property("max-size-buffers", 2u32);
        chain.push(q);
        let count = input.count.clone();
        sink.static_pad("sink").ok_or("fakesink has no pad")?.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    } else {
        chain.push(make("audioconvert")?);
        let level = make("level")?;
        level.set_property("interval", 100_000_000u64); // 100 ms
        level.set_property("post-messages", true);
        chain.push(level);
    }
    chain.push(sink);
    pipeline.add_many(&chain).map_err(|e| e.to_string())?;
    gst::Element::link_many(&chain).map_err(|e| format!("can't link the {} pipeline: {e}", input.name()))?;
    pipeline.set_state(gst::State::Playing).map_err(|_| format!("{} would not start", input.name()))?;
    input.pipeline = Some(pipeline);
    (input.state, input.detail) = ("running", String::new());
    log("video", format!("{}: started {:?}", input.name(), input.choice));
    Ok(())
}

/// Peak and RMS (dB) per channel from a `level` element message.
fn levels_from(s: &gst::StructureRef) -> Option<(Vec<f64>, Vec<f64>)> {
    #[allow(deprecated)] // `level` still posts GValueArray in GStreamer 1.28
    let list = |field: &str| -> Option<Vec<f64>> {
        let arr = s.get::<gst::glib::ValueArray>(field).ok()?;
        Some(arr.iter().map(|v| media::meter_db(v.get::<f64>().unwrap_or(f64::NEG_INFINITY))).collect())
    };
    Some((list("peak")?, list("rms")?))
}

/// Sends level reports to the engine on its own thread, reconnecting as needed. Reports
/// are dropped (never queued up) while the engine is away: old levels are useless.
fn levels_sender(addr: String) -> SyncSender<(Vec<f64>, Vec<f64>)> {
    let (tx, rx) = sync_channel::<(Vec<f64>, Vec<f64>)>(4);
    std::thread::spawn(move || send_levels(&addr, rx));
    tx
}

fn send_levels(addr: &str, rx: Receiver<(Vec<f64>, Vec<f64>)>) {
    use std::io::{BufRead, BufReader, Write};
    let mut conn: Option<(std::net::TcpStream, BufReader<std::net::TcpStream>)> = None;
    for (peak, rms) in rx {
        if conn.is_none() {
            conn = std::net::TcpStream::connect(addr)
                .and_then(|s| {
                    s.set_read_timeout(Some(Duration::from_secs(1)))?;
                    Ok((s.try_clone()?, BufReader::new(s)))
                })
                .ok();
        }
        let Some((w, r)) = conn.as_mut() else { continue };
        let msg = serde_json::json!({ "v": 1, "id": "lv", "ts": 0, "command": { "type": "media.levels", "peakDb": peak, "rmsDb": rms } });
        let mut ack = String::new();
        // Request/response keeps the engine's per-connection queue from ever filling up.
        if writeln!(w, "{msg}").is_err() || r.read_line(&mut ack).unwrap_or(0) == 0 {
            conn = None;
        }
    }
}

fn main() {
    let args = parse_args();
    if let Err(e) = gst::init() {
        fail(&format!("GStreamer: {e}"));
    }
    if let Err(e) = std::fs::create_dir_all(&args.data_dir) {
        fail(&format!("data dir {}: {e}", args.data_dir.display()));
    }
    if args.supervised {
        exit_when_orphaned("video");
    }
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    monitor.add_filter(Some("Audio/Source"), None);
    if monitor.start().is_err() {
        log("video", "device monitor did not start; only test sources will work");
    }
    let levels = levels_sender(args.connect.clone());
    let mut inputs = [Input::new(Kind::Camera), Input::new(Kind::Microphone)];
    let mut devices: Vec<DeviceInfo> = Vec::new();
    let mut config = media::MediaConfig::default();
    let mut problems: Vec<String> = Vec::new();
    let mut last_peak: Vec<f64> = Vec::new();
    let mut last_written = Vec::new();
    let mut last_rescan: Option<Instant> = None;
    let mut sent_ok = false;

    loop {
        let tick_end = Instant::now() + TICK;
        // Handle pipeline messages until the end of this tick.
        while Instant::now() < tick_end {
            let mut handled = false;
            for input in inputs.iter_mut() {
                let Some(bus) = input.pipeline.as_ref().and_then(|p| p.bus()) else { continue };
                while let Some(msg) = bus.pop() {
                    handled = true;
                    match msg.view() {
                        gst::MessageView::Element(e) => {
                            if let Some(s) = e.structure().filter(|s| s.name() == "level")
                                && let Some((peak, rms)) = levels_from(s)
                            {
                                input.count.fetch_add(1, Ordering::Relaxed);
                                last_peak = peak.clone();
                                sent_ok = !matches!(levels.try_send((peak, rms)), Err(TrySendError::Disconnected(_)));
                            }
                        }
                        gst::MessageView::Error(err) => {
                            input.wait(format!("{} failed: {}", input.name(), err.error()));
                            break;
                        }
                        gst::MessageView::Eos(_) => {
                            input.wait(format!("{} stopped sending", input.name()));
                            break;
                        }
                        _ => {}
                    }
                }
            }
            if !handled {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if args.supervised {
            heartbeat();
        }

        // Once a second: re-read config and devices, update rates, retry failed inputs.
        let now = Instant::now();
        let elapsed = last_rescan.map(|t| now.duration_since(t));
        if elapsed.is_none_or(|e| e >= TICK * RESCAN_TICKS as u32) {
            problems.clear();
            match media::load_config(&args.data_dir) {
                Ok(c) => config = c,
                Err(e) => problems.push(format!("{e}; keeping the last good setup")),
            }
            let raw: Vec<RawDevice> = monitor.devices().iter().map(raw_device).collect();
            devices = media::devices(&raw);
            for input in inputs.iter_mut() {
                let sel = if input.kind == Kind::Camera { &config.camera } else { &config.microphone };
                let choice = media::choose(sel, input.kind, &devices);
                if let Some(e) = elapsed {
                    let n = input.count.load(Ordering::Relaxed);
                    input.rate = (n - input.last_count) as f64 / e.as_secs_f64();
                    input.last_count = n;
                }
                let changed = choice != input.choice;
                if changed {
                    input.stop();
                    input.choice = choice;
                }
                if changed || input.pipeline.is_none() {
                    if input.choice == Choice::None {
                        (input.state, input.detail) = ("off", String::new());
                    } else {
                        input.state = "starting";
                        if let Err(e) = start(input, &monitor) {
                            input.wait(e);
                        }
                    }
                }
            }
            last_rescan = Some(now);
        }

        let status = Status {
            devices: devices.clone(),
            camera: inputs[0].status(),
            microphone: inputs[1].status(),
            peak_db: last_peak.clone(),
            connected: sent_ok,
            problems: problems.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&status)
            && bytes != last_written
        {
            if let Err(e) = write_atomic(&args.data_dir.join(media::STATUS_FILE), &bytes) {
                log("video", format!("could not write status: {e}"));
            }
            last_written = bytes;
        }
    }
}
