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
//! Captured frames and audio feed the program pipeline (`program.rs`), which never stops
//! because an input does: a missing camera becomes a slate, a missing microphone silence.
//! It draws the lyric layer, encodes once, and is where recording and streaming attach.

mod program;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::client::{self, LevelsSender, Update};
use jivvy_daemon::media::{self, Choice, DeviceInfo, InputStatus, Kind, Memory, RawDevice, Status};
use jivvy_daemon::program::{AUDIO_CHANNELS, AUDIO_RATE, ProgramConfig};
use jivvy_daemon::{DEFAULT_LISTEN, default_data_dir, exit_when_orphaned, heartbeat, log, write_atomic};
use program::{Feed, Program};

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

/// Builds and starts the pipeline for an input's current choice. Its output goes into
/// `feed`: camera frames scaled to the program size, microphone audio as 48 kHz stereo.
fn start(input: &mut Input, monitor: &gst::DeviceMonitor, feed: &Arc<Feed>, cfg: &ProgramConfig) -> Result<(), String> {
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
    let mut chain = vec![source];
    let (count, feed) = (input.count.clone(), feed.clone());
    let sink = if input.kind == Kind::Camera {
        // Media Foundation cameras already deliver NV12 at 1080p, so these pass through;
        // other sizes and formats are converted here, off the program's path.
        chain.push(make("videoconvert")?);
        chain.push(make("videoscale")?);
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "NV12")
            .field("width", cfg.width as i32)
            .field("height", cfg.height as i32)
            .build();
        gst_app::AppSink::builder()
            .caps(&caps)
            .max_buffers(1)
            .drop(true)
            .sync(false)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        if let Some(b) = sample.buffer_owned() {
                            *feed.frame.lock().unwrap() = Some((Instant::now(), b));
                            count.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            )
            .build()
    } else {
        chain.push(make("audioconvert")?);
        chain.push(make("audioresample")?);
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", "S16LE")
            .field("rate", AUDIO_RATE as i32)
            .field("channels", AUDIO_CHANNELS as i32)
            .field("layout", "interleaved")
            .build();
        let filter = make("capsfilter")?;
        filter.set_property("caps", &caps);
        chain.push(filter);
        let level = make("level")?;
        level.set_property("interval", 100_000_000u64); // 100 ms
        level.set_property("post-messages", true);
        chain.push(level);
        gst_app::AppSink::builder()
            .sync(false)
            .callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        if let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) {
                            let samples: Vec<i16> =
                                map.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
                            feed.audio.lock().unwrap().push(&samples);
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            )
            .build()
    };
    chain.push(sink.upcast());
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
    let levels = LevelsSender::start(args.connect.clone());
    let font: &'static [u8] = match jivvy_daemon::lyrics::load_font(None) {
        Ok(f) => Box::leak(f.into_boxed_slice()),
        Err(e) => fail(&format!("lyric layer: {e}")),
    };
    let feed: Arc<Feed> = Arc::default();
    {
        // The lyric layer follows the engine like any screen does.
        let (feed, connect) = (feed.clone(), args.connect.clone());
        std::thread::spawn(move || {
            client::follow(&connect, move |u| {
                if let Update::State(s) = u {
                    *feed.live.lock().unwrap() = Some(s);
                }
            })
        });
    }
    let mut program: Option<Program> = None;
    let mut inputs = [Input::new(Kind::Camera), Input::new(Kind::Microphone)];
    let mut devices: Vec<DeviceInfo> = Vec::new();
    let mut config = media::MediaConfig::default();
    // Auto pins and ambiguous names survive restarts, so a restart never switches cameras.
    let mut memory = Memory::load(&args.data_dir);
    let mut problems: Vec<String> = Vec::new();
    let mut last_peak: Vec<f64> = Vec::new();
    let mut last_written = Vec::new();
    let mut last_rescan: Option<Instant> = None;

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
                                levels.send(peak, rms);
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
            if let Some(p) = program.as_mut() {
                p.tick();
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
            // A new program size or encoder rebuilds the program, and the camera with it
            // (it captures at the program size).
            let program_cfg = config.program.sanitized();
            let resized = program
                .as_ref()
                .is_some_and(|p| (p.config().width, p.config().height) != (program_cfg.width, program_cfg.height));
            if program.as_ref().map(|p| p.config()) != Some(&program_cfg) {
                drop(program.take()); // stops the old one first
                if resized {
                    // Stop capturing at the old size and forget its last frame before the new
                    // program starts, so no old-size frame is pushed under the new size's caps.
                    inputs[0].stop();
                    *feed.frame.lock().unwrap() = None;
                }
                program = Some(Program::new(program_cfg.clone(), feed.clone(), font));
            }
            if let (Some(p), Some(e)) = (program.as_mut(), elapsed) {
                p.measure(e);
            }
            let raw: Vec<RawDevice> = monitor.devices().iter().map(raw_device).collect();
            devices = media::devices(&raw);
            let remembered = memory.clone();
            memory.observe(&devices);
            for input in inputs.iter_mut() {
                let sel = if input.kind == Kind::Camera { &config.camera } else { &config.microphone };
                let choice = memory.choose(sel, input.kind, &devices);
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
                        if let Err(e) = start(input, &monitor, &feed, &program_cfg) {
                            input.wait(e);
                        }
                    }
                }
            }
            if memory != remembered
                && let Err(e) = serde_json::to_vec_pretty(&memory)
                    .map_err(std::io::Error::other)
                    .and_then(|b| write_atomic(&args.data_dir.join(media::MEMORY_FILE), &b))
            {
                log("video", format!("could not save {}: {e}", media::MEMORY_FILE));
            }
            last_rescan = Some(now);
        }
        if inputs[0].pipeline.is_none() {
            *feed.frame.lock().unwrap() = None; // slate right away, not after the stale timeout
        }

        let status = Status {
            devices: devices.clone(),
            camera: inputs[0].status(),
            microphone: inputs[1].status(),
            peak_db: last_peak.clone(),
            connected: levels.delivering(),
            program: program.as_ref().map(|p| p.status()).unwrap_or_default(),
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
