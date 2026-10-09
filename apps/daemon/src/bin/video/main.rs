//! `jivvy-video`: camera / capture card and audio input. Normally started by `jivvy-watchdog`.
//! Built only with the `video` feature (it needs GStreamer installed).
//!
//! jivvy-video [--data-dir DIR] [--connect ADDR] [--supervised] [--test-sources]
//! jivvy-video --hardware-check [--if-changed] [--data-dir DIR]
//!
//! `--test-sources` (or `JIVVY_TEST_SOURCES=1`) uses a test pattern and tone for every
//! camera and microphone, whatever `media.json` names: no devices needed.
//!
//! `--hardware-check` measures each picture path and encoder on this machine, writes
//! `hardware.json` with the setting to use and why (see `hwcheck.rs`), prints it and exits:
//! 0 when a setting keeps up, 1 when nothing does, 2 when the report can't be saved.
//! `--if-changed` measures only when there's no report or the machine differs from the one
//! it was made on (a new graphics card or driver, another computer); otherwise it exits 0.
//!
//! Camera and microphone run as separate pipelines, so one device failing never stops the
//! other. A device that is unplugged or fails is retried every second and picked up again
//! when it returns. Audio levels go to the engine about ten times a second
//! (`media.levels`), which relays them to every screen for live meters.
//!
//! Captured frames and audio feed the program pipeline (`program.rs`), which never stops
//! because an input does: a missing camera becomes a slate, a missing microphone silence.
//! It draws the lyric layer, encodes once, and is where recording and streaming attach.

mod hls;
mod hwcheck;
mod program;
mod stream;
mod tiers;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::client::{self, LevelsSender, Update};
use jivvy_daemon::hardware;
use jivvy_daemon::media::{self, Choice, DeviceInfo, InputStatus, Kind, Memory, RawDevice, Status};
use jivvy_daemon::program::{AUDIO_CHANNELS, AUDIO_RATE, EncoderChoice, ProgramConfig};
use jivvy_daemon::{DEFAULT_LISTEN, default_data_dir, exit_when_orphaned, heartbeat, log, write_atomic};
use program::{Feed, Program};
use stream::Streamer;

const TICK: Duration = Duration::from_millis(250);
/// Devices and config are re-checked, and failed inputs retried, every this many ticks.
const RESCAN_TICKS: u64 = 4;

struct Args {
    data_dir: PathBuf,
    connect: String,
    supervised: bool,
    test_sources: bool,
    hardware_check: bool,
    /// With `--hardware-check`: only measure if there's no report or it no longer fits this machine.
    if_changed: bool,
    /// Test hook `JIVVY_TEST_ENCODER=x264|mf`: the encoder to use, whatever `media.json` says.
    /// CI's Windows runners have no GPU, where Media Foundation is a software encoder too slow
    /// for 1080p30.
    test_encoder: Option<EncoderChoice>,
}

fn test_encoder_from_env() -> Option<EncoderChoice> {
    let name = std::env::var("JIVVY_TEST_ENCODER").ok().filter(|v| !v.trim().is_empty())?;
    match serde_json::from_value(serde_json::Value::String(name.trim().to_ascii_lowercase())) {
        Ok(e) => Some(e),
        Err(_) => fail(&format!("JIVVY_TEST_ENCODER must be auto, mf, qsv or x264, not {name:?}")),
    }
}

fn parse_args() -> Args {
    let mut a = Args {
        data_dir: default_data_dir(),
        connect: DEFAULT_LISTEN.into(),
        supervised: false,
        test_sources: media::test_sources_from_env(),
        hardware_check: false,
        if_changed: false,
        test_encoder: test_encoder_from_env(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--data-dir" => a.data_dir = val().into(),
            "--connect" => a.connect = val(),
            "--supervised" => a.supervised = true,
            "--test-sources" => a.test_sources = true,
            "--hardware-check" => a.hardware_check = true,
            "--if-changed" => a.if_changed = true,
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

/// Devices as last listed by the scanner thread, with the GStreamer device for each.
type DeviceCache = Arc<Mutex<Vec<(RawDevice, gst::Device)>>>;

fn list_devices(monitor: &gst::DeviceMonitor) -> Vec<(RawDevice, gst::Device)> {
    monitor.devices().into_iter().map(|d| (raw_device(&d), d)).collect()
}

/// Starts the device monitor and fills `cache` with the first list, before the main loop
/// runs (this loads GStreamer's device plugins, which holds up other GStreamer calls for a
/// few seconds; it belongs in the start-up grace period). After that the list is refreshed
/// once a second on its own thread: listing can take a second or more (longer with a
/// misbehaving USB driver), and the main loop must keep its heartbeat going.
fn start_device_scanner(cache: &DeviceCache) {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    monitor.add_filter(Some("Audio/Source"), None);
    if monitor.start().is_err() {
        log("video", "device monitor did not start; only test sources will work");
    }
    *cache.lock().unwrap() = list_devices(&monitor);
    let c = cache.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(TICK * RESCAN_TICKS as u32);
            let list = list_devices(&monitor);
            *c.lock().unwrap() = list;
        }
    });
}

/// The GStreamer device behind a listed device id.
fn find_device(cache: &DeviceCache, id: &str) -> Option<gst::Device> {
    let list = cache.lock().unwrap();
    list.iter()
        .find(|(raw, _)| media::devices(std::slice::from_ref(raw)).first().is_some_and(|i| i.id == id))
        .map(|(_, d)| d.clone())
}

fn make(factory: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory).build().map_err(|_| format!("GStreamer element {factory} is missing"))
}

/// Builds and starts the pipeline for an input's current choice. Its output goes into
/// `feed`: camera frames scaled to the program size, microphone audio as 48 kHz stereo.
fn start(input: &mut Input, devices: &DeviceCache, feed: &Arc<Feed>, cfg: &ProgramConfig) -> Result<(), String> {
    let source = match &input.choice {
        Choice::None => return Ok(()),
        Choice::Missing { name } => return Err(format!("{name} is not connected")),
        Choice::Test if input.kind == Kind::Camera => {
            let s = make("videotestsrc")?;
            s.set_property("is-live", true);
            // Tests can ask for a noisier picture (e.g. "snow") so an encoder really spends
            // its bitrate, as it does on a camera.
            let pattern = std::env::var("JIVVY_TEST_PATTERN").unwrap_or_else(|_| "ball".into());
            s.set_property_from_str("pattern", &pattern);
            s
        }
        Choice::Test => {
            let s = make("audiotestsrc")?;
            s.set_property("is-live", true);
            s.set_property("volume", 0.5f64);
            s
        }
        Choice::Device { id, name } => find_device(devices, id)
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
                                map.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
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

/// Everything the main loop owns.
struct Video {
    args: Args,
    levels: LevelsSender,
    font: &'static [u8],
    feed: Arc<Feed>,
    device_cache: DeviceCache,
    program: Option<Program>,
    streamer: Streamer,
    tiers: tiers::Tiers,
    /// The stream status last reported to the engine, and when.
    stream_reported: Option<(&'static str, Instant)>,
    inputs: [Input; 2],
    devices: Vec<DeviceInfo>,
    config: media::MediaConfig,
    // Auto pins and ambiguous names survive restarts, so a restart never switches cameras.
    memory: Memory,
    /// The hardware check's report (`hardware.json`), read at start.
    hardware: Option<hardware::Report>,
    problems: Vec<String>,
    last_peak: Vec<f64>,
    last_written: Vec<u8>,
    last_rescan: Option<Instant>,
}

impl Video {
    /// Handles queued input pipeline messages and moves the program along. True if there
    /// was anything to handle.
    fn pump(&mut self) -> bool {
        let mut handled = false;
        for input in self.inputs.iter_mut() {
            let Some(bus) = input.pipeline.as_ref().and_then(|p| p.bus()) else { continue };
            while let Some(msg) = bus.pop() {
                handled = true;
                match msg.view() {
                    gst::MessageView::Element(e) => {
                        if let Some(s) = e.structure().filter(|s| s.name() == "level")
                            && let Some((peak, rms)) = levels_from(s)
                        {
                            input.count.fetch_add(1, Ordering::Relaxed);
                            self.last_peak = peak.clone();
                            self.levels.send(peak, rms);
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
        if let Some(p) = self.program.as_mut() {
            p.tick();
        }
        handled
    }

    /// Once a second: re-read config and devices, update rates, (re)start what's needed.
    fn rescan(&mut self, now: Instant) {
        let elapsed = self.last_rescan.map(|t| now.duration_since(t));
        self.problems.clear();
        match media::load_config(&self.args.data_dir) {
            // Test sources are applied when inputs are chosen (`choose_input`), not here, so
            // the remembered auto pins survive.
            Ok(mut c) => {
                if let Some(encoder) = self.args.test_encoder {
                    c.program.encoder = encoder;
                }
                self.config = c;
            }
            Err(e) => self.problems.push(format!("{e}; keeping the last good setup")),
        }
        // A new program size or encoder rebuilds the program, and the camera with it
        // (it captures at the program size).
        // media.json's own settings win; the hardware check fills in the rest.
        let chosen = self.hardware.as_ref().and_then(|r| r.chosen);
        let program_cfg = hardware::resolve(&self.config.program, self.config.program_set, chosen).sanitized();
        let resized = self
            .program
            .as_ref()
            .is_some_and(|p| (p.config().width, p.config().height) != (program_cfg.width, program_cfg.height));
        if self.program.as_ref().map(|p| p.config()) != Some(&program_cfg) {
            drop(self.program.take()); // stops the old one first
            if resized {
                // Stop capturing at the old size and forget its last frame before the new
                // program starts, so no old-size frame is pushed under the new size's caps.
                self.inputs[0].stop();
                *self.feed.frame.lock().unwrap() = None;
            }
            self.program = Some(Program::new(program_cfg.clone(), self.feed.clone(), self.font));
        }
        if let (Some(p), Some(e)) = (self.program.as_mut(), elapsed) {
            p.measure(e);
        }
        self.streamer.reconcile(&self.args.data_dir, &self.feed, &program_cfg);
        let encoders = media_encoders(&program_cfg);
        self.tiers.reconcile(self.streamer.ladder(), &program_cfg, &encoders, &self.feed);
        let raw: Vec<RawDevice> = self.device_cache.lock().unwrap().iter().map(|(r, _)| r.clone()).collect();
        self.devices = media::devices(&raw);
        let remembered = self.memory.clone();
        self.memory.observe(&self.devices);
        for input in self.inputs.iter_mut() {
            let sel = if input.kind == Kind::Camera { &self.config.camera } else { &self.config.microphone };
            let choice = self.memory.choose_input(sel, input.kind, &self.devices, self.args.test_sources);
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
                    if let Err(e) = start(input, &self.device_cache, &self.feed, &program_cfg) {
                        input.wait(e);
                    }
                }
            }
        }
        if self.memory != remembered
            && let Err(e) = serde_json::to_vec_pretty(&self.memory)
                .map_err(std::io::Error::other)
                .and_then(|b| write_atomic(&self.args.data_dir.join(media::MEMORY_FILE), &b))
        {
            log("video", format!("could not save {}: {e}", media::MEMORY_FILE));
        }
        self.last_rescan = Some(now);
    }

    /// Writes `media-status.json` if anything changed.
    fn write_status(&mut self) {
        if self.inputs[0].pipeline.is_none() {
            *self.feed.frame.lock().unwrap() = None; // slate right away, not after the stale timeout
        }
        let wanted = self.feed.live.lock().unwrap().is_some_and(|s| s.stream_wanted);
        let mut stream = self.streamer.status(wanted);
        stream.encodes = self.tiers.status();
        // Tell the engine on every change and once a second (it treats silence as trouble).
        if self.stream_reported.is_none_or(|(s, at)| s != stream.status || at.elapsed() >= Duration::from_secs(1)) {
            self.levels.report_stream(stream.status);
            self.stream_reported = Some((stream.status, Instant::now()));
        }
        let status = Status {
            devices: self.devices.clone(),
            camera: self.inputs[0].status(),
            microphone: self.inputs[1].status(),
            peak_db: self.last_peak.clone(),
            connected: self.levels.delivering(),
            program: self.program.as_ref().map(|p| p.status()).unwrap_or_default(),
            stream,
            problems: self.problems.clone(),
            hardware: self.hardware.as_ref().map(|r| r.reason.clone()).unwrap_or_default(),
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(&status)
            && bytes != self.last_written
        {
            if let Err(e) = write_atomic(&self.args.data_dir.join(media::STATUS_FILE), &bytes) {
                log("video", format!("could not write status: {e}"));
            }
            self.last_written = bytes;
        }
    }
}

/// Encoders for the lower-quality stream encodes, in the order the program uses.
fn media_encoders(cfg: &jivvy_daemon::program::ProgramConfig) -> Vec<jivvy_daemon::program::EncoderChoice> {
    jivvy_daemon::program::encoder_order(cfg.encoder, |e| gst::ElementFactory::find(e).is_some())
}

fn main() {
    let args = parse_args();
    if let Err(e) = gst::init() {
        fail(&format!("GStreamer: {e}"));
    }
    if let Err(e) = std::fs::create_dir_all(&args.data_dir) {
        fail(&format!("data dir {}: {e}", args.data_dir.display()));
    }
    // First, so a hardware check started by the watchdog never outlives it either.
    if args.supervised {
        exit_when_orphaned("video");
    }
    if args.hardware_check {
        if args.if_changed
            && let Some(report) = hardware::load(&args.data_dir)
            && !hardware::needs_recheck(&report, &hwcheck::fingerprint_now())
        {
            log("video", "hardware check: this machine is unchanged since the last one; keeping its report");
            std::process::exit(0);
        }
        let font = jivvy_daemon::lyrics::load_font(None).unwrap_or_else(|e| fail(&format!("lyric layer: {e}")));
        let (report, saved) = hwcheck::run(&args.data_dir, args.test_encoder, &font);
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
        if let Err(e) = saved {
            fail(&format!("hardware check: {e}")); // exits 2: the check isn't done until it's saved
        }
        std::process::exit(if report.chosen.is_some() { 0 } else { 1 });
    }
    if args.test_sources {
        log("video", "test sources: every camera and microphone is a test pattern or tone");
    }
    if let Some(encoder) = args.test_encoder {
        log("video", format!("test hook: encoder {} (JIVVY_TEST_ENCODER)", encoder.name()));
    }
    if std::env::var("JIVVY_TEST_NO_GPU").is_ok_and(|v| v.trim() == "1") {
        log("video", "test hook: lyric layer and conversion on the CPU (JIVVY_TEST_NO_GPU)");
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
    let memory = Memory::load(&args.data_dir);
    let hardware = hardware::load(&args.data_dir);
    match &hardware {
        Some(r) => log("video", format!("hardware check: {}", r.reason)),
        None => log("video", "no hardware check yet; using media.json and the installed plugins"),
    }
    let mut video = Video {
        args,
        levels,
        font,
        feed,
        device_cache: Arc::default(),
        program: None,
        streamer: Streamer::new(),
        tiers: tiers::Tiers::default(),
        stream_reported: None,
        inputs: [Input::new(Kind::Camera), Input::new(Kind::Microphone)],
        devices: Vec::new(),
        config: media::MediaConfig::default(),
        memory,
        hardware,
        problems: Vec::new(),
        last_peak: Vec::new(),
        last_written: Vec::new(),
        last_rescan: None,
    };

    // Get the program going first, so after a restart the stream and recording are fed
    // again within a moment (slate and silence until devices are known; test sources too).
    // Then start the device monitor, which takes a few seconds and holds up other GStreamer
    // calls meanwhile; the program's own threads keep running through it. Both happen
    // before the first heartbeat, inside the watchdog's start-up grace.
    video.rescan(Instant::now());
    video.write_status();
    start_device_scanner(&video.device_cache);

    loop {
        let tick_end = Instant::now() + TICK;
        while Instant::now() < tick_end {
            if !video.pump() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if video.args.supervised {
            heartbeat();
        }
        let now = Instant::now();
        if video.last_rescan.is_none_or(|t| now.duration_since(t) >= TICK * RESCAN_TICKS as u32) {
            video.rescan(now);
        }
        video.write_status();
    }
}
