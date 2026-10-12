//! The local recording: the program's encoded video and audio, written to Matroska as a
//! stream, in a worker of its own. Nothing here can slow or stop the program or a stream
//! destination: like them, it is handed copies of the packets and is dropped if it can't
//! keep up (it then carries on in a new file).
//!
//! Crash safety, in layers:
//! - Matroska is written cluster by cluster with nothing left to finish at the end, and
//!   every write goes straight to the file, so a killed process leaves a file that plays up
//!   to its last second.
//! - Every start writes a new numbered part in the same set (`jivvy_daemon::recording`), so
//!   a crash never touches what was already recorded, and the next process knows which set
//!   to carry on in because the engine saved that a recording was wanted.
//! - A failure of the disk, the muxer or the encoder ends the part and starts the next one;
//!   it never ends the recording.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::bandwidth::Tier;
use jivvy_daemon::recording::{self, Config, RecordingInfo};
use jivvy_daemon::stream::reconnect_delay;
use jivvy_daemon::{log, now_ms};

use crate::program::Feed;
use crate::stream::{Packet, restarted, retimed, running_time};

/// If this much video and audio is waiting for the disk, the disk has stalled: the part is
/// given up on (the program never waits for it) and the next one starts.
const MAX_BACKLOG_BYTES: u64 = 32 * 1024 * 1024;
/// How long to wait for the program's first keyframe and the audio's format.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a part gets to close cleanly when the recording is stopped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Recorder {
    info: Arc<Mutex<RecordingInfo>>,
}

impl Recorder {
    /// Starts the worker (and the converter to MP4). It idles until the engine says a
    /// recording is wanted.
    pub fn start(data_dir: PathBuf, feed: Arc<Feed>) -> Recorder {
        let info = Arc::new(Mutex::new(RecordingInfo { status: "off", ..Default::default() }));
        let (d, f, i) = (data_dir.clone(), feed.clone(), info.clone());
        std::thread::spawn(move || run(&d, &f, &i));
        crate::remux::start_converter(data_dir, feed, info.clone());
        Recorder { info }
    }

    pub fn info(&self) -> RecordingInfo {
        self.info.lock().unwrap().clone()
    }
}

/// The engine's "recording wanted" flag: None until the engine has said (right after a
/// restart). Until then nothing is started and nothing is closed, so a restarted process
/// carries on in the set it was recording.
fn wanted(feed: &Feed) -> Option<bool> {
    feed.live.lock().unwrap().map(|s| s.recording_wanted)
}

/// `recording.json`, re-read once a second, and the disk it points at.
struct Settings {
    data_dir: PathBuf,
    config: Config,
    folder: PathBuf,
    warn_below: u64,
    problem: Option<String>,
    low: bool,
    read_at: Option<Instant>,
}

impl Settings {
    fn new(data_dir: &Path) -> Settings {
        let config = Config::default();
        Settings {
            data_dir: data_dir.to_path_buf(),
            folder: config.folder(data_dir),
            warn_below: config.warn_free_bytes(),
            config,
            problem: None,
            low: false,
            read_at: None,
        }
    }

    /// Once a second: re-reads the config and the free space into `info`.
    fn refresh(&mut self, info: &Mutex<RecordingInfo>) {
        if self.read_at.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.read_at = Some(Instant::now());
        match recording::load_config(&self.data_dir) {
            Ok(c) => (self.config, self.problem) = (c, None),
            Err(e) => self.problem = Some(format!("{e}; keeping the last good settings")),
        }
        self.folder = self.config.folder(&self.data_dir);
        self.warn_below = self.config.warn_free_bytes();
        let free = recording::free_space(&self.folder);
        self.low = free.is_some_and(|f| recording::is_low(f, self.warn_below, self.low));
        let mut i = info.lock().unwrap();
        i.folder = self.folder.display().to_string();
        i.free_gb = free.map(|f| (f as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0);
        i.low_disk = self.low;
        i.problems = self.problem.iter().cloned().collect();
    }
}

fn update(info: &Mutex<RecordingInfo>, f: impl FnOnce(&mut RecordingInfo)) {
    f(&mut info.lock().unwrap());
}

fn run(data_dir: &Path, feed: &Arc<Feed>, info: &Arc<Mutex<RecordingInfo>>) {
    let mut settings = Settings::new(data_dir);
    let mut attempt = 0u32;
    loop {
        settings.refresh(info);
        match wanted(feed) {
            None => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Some(false) => {
                // Over: the next recording starts a new set, and this one can be converted.
                recording::finish(data_dir);
                update(info, |i| {
                    (i.wanted, i.status, i.set, i.file, i.parts, i.bytes) =
                        (false, "off", String::new(), String::new(), 0, 0);
                    i.detail.clear();
                });
                attempt = 0;
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Some(true) => update(info, |i| i.wanted = true),
        }
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| session(data_dir, feed, info, &mut settings)))
            .unwrap_or_else(|_| Err("the recorder crashed".into()));
        match result {
            Ok(_) => attempt = 0,
            Err(e) => {
                attempt += 1;
                log("video", format!("recording: {e}; carrying on in a new file"));
                update(info, |i| (i.status, i.detail) = ("waiting", recording::plain_error(&e)));
                // A retry shortly, but never a busy loop on a disk that is full or missing.
                let until = Instant::now() + reconnect_delay(attempt).min(Duration::from_secs(5));
                while Instant::now() < until && wanted(feed) == Some(true) {
                    std::thread::sleep(Duration::from_millis(50));
                    settings.refresh(info);
                }
            }
        }
    }
}

/// Why a part ended without a failure.
enum End {
    /// The recording was stopped.
    Stopped,
    /// The program changed under the recording (another size or encoder, or a restart of
    /// its pipeline): this part is closed and the next one carries on.
    Changed,
}

/// Closes a pipeline without ever blocking the worker: a disk that has stalled can hold
/// `set_state(Null)` up indefinitely.
fn drop_pipeline(p: gst::Pipeline) {
    std::thread::spawn(move || {
        let _ = p.set_state(gst::State::Null);
    });
}

/// One part, from its first keyframe until the recording stops, the program changes or
/// something fails.
fn session(
    data_dir: &Path,
    feed: &Arc<Feed>,
    info: &Mutex<RecordingInfo>,
    settings: &mut Settings,
) -> Result<End, String> {
    let sub = feed.taps.subscribe(Tier::Full);
    // Start on a keyframe (the program sends one every 2 s) and know the audio's format.
    let deadline = Instant::now() + START_TIMEOUT;
    let (mut first, mut audio_caps) = (None, None);
    while first.is_none() || audio_caps.is_none() {
        if wanted(feed) != Some(true) {
            return Ok(End::Stopped);
        }
        match sub.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Packet::Video(s))
                if first.is_none() && s.buffer().is_some_and(|b| !b.flags().contains(gst::BufferFlags::DELTA_UNIT)) =>
            {
                first = Some(s);
            }
            Ok(Packet::Audio(s)) if audio_caps.is_none() => audio_caps = s.caps_owned(),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) if Instant::now() > deadline => {
                return Err("the program isn't producing video".into());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("program taps closed".into()),
        }
    }
    let (first, audio_caps) = (first.unwrap(), audio_caps.unwrap());
    let base = running_time(&first).ok_or("the first keyframe has no timestamp")?.0;
    let video_caps = first.caps_owned().ok_or("no video caps")?;

    let opened = recording::open_part(data_dir, &settings.folder, now_ms())
        .map_err(|e| format!("can't use the recordings folder: {e}"))?;
    log(
        "video",
        format!(
            "recording: {} {} in set {}",
            if opened.resumed { "carrying on with part" } else { "starting part" },
            opened.part,
            opened.set
        ),
    );
    update(info, |i| {
        (i.set, i.file, i.parts, i.bytes) = (opened.set.clone(), opened.file.display().to_string(), opened.part, 0);
    });

    let pipeline = gst::Pipeline::new();
    let make = |factory: &str| {
        gst::ElementFactory::make(factory).build().map_err(|_| format!("GStreamer element {factory} is missing"))
    };
    let mux = make("matroskamux")?;
    // Written as a stream: no index and no duration to fix up at the end, so there is
    // nothing a crash can leave half done.
    mux.set_property("streamable", true);
    mux.set_property("writing-app", "Jivvy Live");
    let out = make("filesink")?;
    out.set_property("location", opened.file.to_string_lossy().as_ref());
    // Every write goes straight to the file: a killed process loses no data in a buffer.
    out.set_property_from_str("buffer-mode", "unbuffered");
    out.set_property("sync", false);
    out.set_property("async", false);
    let source = |caps: &gst::Caps| {
        gst_app::AppSrc::builder().caps(caps).format(gst::Format::Time).is_live(false).block(false).build()
    };
    let (vsrc, asrc) = (source(&video_caps), source(&audio_caps));
    pipeline
        .add_many([&mux, &out, vsrc.upcast_ref::<gst::Element>(), asrc.upcast_ref::<gst::Element>()])
        .map_err(|e| e.to_string())?;
    mux.link(&out).map_err(|e| e.to_string())?;
    for (src, pad) in [(&vsrc, "video_%u"), (&asrc, "audio_%u")] {
        let sink = mux.request_pad_simple(pad).ok_or("the muxer has no pad for the stream")?;
        src.static_pad("src")
            .ok_or("no source pad")?
            .link(&sink)
            .map_err(|e| format!("can't link the recording: {e}"))?;
    }
    pipeline.set_state(gst::State::Playing).map_err(|_| "the recording pipeline would not start")?;

    let push = |src: &gst_app::AppSrc, sample: &gst::Sample| {
        if let Some(b) = retimed(sample, base, gst::ClockTime::ZERO) {
            let _ = src.push_buffer(b);
        }
    };
    let mut last_video = running_time(&first).map(|(t, _)| t);
    push(&vsrc, &first);
    let mut last_report = Instant::now();
    let result = loop {
        if wanted(feed) != Some(true) {
            break Ok(End::Stopped);
        }
        if sub.overflowed.load(Ordering::Relaxed) {
            break Err("the disk couldn't keep up".to_string());
        }
        if vsrc.current_level_bytes() + asrc.current_level_bytes() > MAX_BACKLOG_BYTES {
            break Err("the disk couldn't keep up".into());
        }
        match sub.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Packet::Video(s)) => {
                if s.caps().is_some_and(|c| !c.is_strictly_equal(&video_caps)) || restarted(&mut last_video, &s) {
                    break Ok(End::Changed);
                }
                push(&vsrc, &s);
            }
            Ok(Packet::Audio(s)) => push(&asrc, &s),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Err("program taps closed".into()),
        }
        if let Some(bus) = pipeline.bus() {
            let mut error = None;
            while let Some(msg) = bus.pop() {
                // The first error is the cause; elements upstream then report a generic one.
                if let gst::MessageView::Error(e) = msg.view()
                    && error.is_none()
                {
                    let debug = e.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                    error = Some(format!("{}{debug}", e.error()));
                }
            }
            if let Some(e) = error {
                break Err(e);
            }
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            settings.refresh(info);
            // Recording means bytes are really in the file, not just handed to the muxer.
            let on_disk = std::fs::metadata(&opened.file).map_or(0, |m| m.len());
            update(info, |i| {
                i.bytes = on_disk;
                if on_disk > 0 {
                    (i.status, i.detail) = ("recording", String::new());
                }
            });
        }
    };
    match &result {
        // A clean end: the muxer finishes the file, then it is closed.
        Ok(_) => close(pipeline, &vsrc, &asrc),
        // A failure: whatever is in the file already stays; a stalled disk can't hold us up.
        Err(_) => drop_pipeline(pipeline),
    }
    result
}

/// Ends both streams and waits (briefly) for the muxer to finish the file.
fn close(pipeline: gst::Pipeline, vsrc: &gst_app::AppSrc, asrc: &gst_app::AppSrc) {
    let _ = vsrc.end_of_stream();
    let _ = asrc.end_of_stream();
    if let Some(bus) = pipeline.bus() {
        let _ = bus.timed_pop_filtered(
            gst::ClockTime::from_nseconds(CLOSE_TIMEOUT.as_nanos() as u64),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
    }
    drop_pipeline(pipeline);
}
