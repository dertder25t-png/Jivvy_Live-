//! The program pipeline: paced video and audio in, lyric layer on, encoded once, out to a
//! tee that the recording and the stream branch from. See `jivvy_daemon::program`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use jivvy_daemon::client::LiveState;
use jivvy_daemon::log;
use jivvy_daemon::lyrics;
use jivvy_daemon::program::{
    AUDIO_CHANNELS, AUDIO_CHUNK, AUDIO_RATE, AudioFifo, EncoderChoice, FrameSource, ProgramConfig, ProgramStatus,
    encoder_order, frame_source, frames_due, lines_for, next_frame_at,
};

/// How long an encoder gets to produce its first frame before the next one is tried.
const TRIAL: Duration = Duration::from_secs(4);
/// After every encoder failed, wait this long before trying the list again.
const RETRY: Duration = Duration::from_secs(1);

/// What the capture pipelines hand to the program, and what the program reports back.
#[derive(Default)]
pub struct Feed {
    /// Newest camera frame (NV12 at the program size) and when it arrived.
    pub frame: Mutex<Option<(Instant, gst::Buffer)>>,
    pub audio: Mutex<AudioFifo>,
    /// The engine's state, for the lyric layer.
    pub live: Mutex<Option<LiveState>>,
    frames_encoded: AtomicU64,
    bytes_encoded: AtomicU64,
    overlay_renders: AtomicU64,
    slate: AtomicBool,
    overlay: Mutex<Vec<String>>,
}

/// Where the pacers push, swapped whenever the program pipeline is rebuilt.
#[derive(Default)]
struct Sources {
    video: Option<gst_app::AppSrc>,
    audio: Option<gst_app::AppSrc>,
    /// Program start: timestamps count from here.
    start: Option<Instant>,
    generation: u64,
}

enum State {
    Trying { index: usize, since: Instant },
    Running,
    Failed { retry_at: Instant },
}

pub struct Program {
    cfg: ProgramConfig,
    gpu: bool,
    order: Vec<EncoderChoice>,
    state: State,
    pipeline: Option<gst::Pipeline>,
    sources: Arc<Mutex<Sources>>,
    feed: Arc<Feed>,
    font: &'static [u8],
    /// The encoder being tried or running.
    current: Option<EncoderChoice>,
    /// Cleared on drop, which stops this program's pacer threads.
    alive: Arc<AtomicBool>,
    detail: String,
    last_frames: u64,
    last_bytes: u64,
    out_fps: f64,
    kbps: f64,
}

fn has(element: &str) -> bool {
    gst::ElementFactory::find(element).is_some()
}

fn encoder_description(enc: EncoderChoice, cfg: &ProgramConfig, gpu: bool) -> String {
    let (br, gop) = (cfg.bitrate_kbps, cfg.fps * 2);
    // Media Foundation and x264 take system memory, so frames come back from the GPU first.
    let to_system =
        if gpu { "d3d12download ! video/x-raw,format=NV12" } else { "videoconvert ! video/x-raw,format=NV12" };
    match enc {
        EncoderChoice::Mf => {
            format!("{to_system} ! mfh264enc rc-mode=cbr bitrate={br} gop-size={gop} bframes=0 low-latency=true")
        }
        EncoderChoice::Qsv => {
            let input = if gpu { "" } else { "videoconvert ! " };
            format!(
                "{input}qsvh264enc bitrate={br} max-bitrate={br} rate-control=cbr gop-size={gop} b-frames=0 target-usage=7"
            )
        }
        EncoderChoice::X264 | EncoderChoice::Auto => format!(
            "{to_system} ! x264enc bitrate={br} vbv-buf-capacity=1000 key-int-max={gop} bframes=0 \
             speed-preset=veryfast tune=zerolatency"
        ),
    }
}

/// The whole program pipeline as a launch description.
fn description(enc: EncoderChoice, cfg: &ProgramConfig, gpu: bool) -> String {
    let (w, h, fps) = (cfg.width, cfg.height, cfg.fps);
    // On the GPU, overlaycomposition only attaches the lyric layer as metadata and
    // d3d12overlaycompositor blends it there; without a GPU it blends on the CPU.
    let overlay = if gpu {
        "d3d12upload ! overlaycomposition name=lyrics ! d3d12overlaycompositor"
    } else {
        "overlaycomposition name=lyrics"
    };
    let mut d = format!(
        "appsrc name=vsrc is-live=true format=time do-timestamp=false block=false \
           caps=video/x-raw,format=NV12,width={w},height={h},framerate={fps}/1 \
         ! {overlay} ! queue max-size-buffers=3 leaky=downstream ! {enc} \
         ! h264parse config-interval=-1 ! video/x-h264,stream-format=avc,alignment=au ! tee name=vt allow-not-linked=true \
         vt. ! queue ! fakesink name=vmon sync=false async=false \
         appsrc name=asrc is-live=true format=time do-timestamp=false block=false \
           caps=audio/x-raw,format=S16LE,rate={AUDIO_RATE},channels={AUDIO_CHANNELS},layout=interleaved \
         ! audioconvert ! avenc_aac bitrate=128000 ! aacparse ! tee name=at allow-not-linked=true \
         at. ! queue ! fakesink sync=false async=false",
        enc = encoder_description(enc, cfg, gpu),
    );
    // Developer aid until the recording item lands: also write the program to a file.
    if let Ok(path) = std::env::var("JIVVY_DEBUG_PROGRAM_FILE") {
        d.push_str(&format!(
            " matroskamux name=dbg streamable=true ! filesink location=\"{}\" vt. ! queue ! dbg. at. ! queue ! dbg.",
            path.replace('\\', "/")
        ));
    }
    d
}

/// A black NV12 frame for when there is no camera picture.
fn slate(cfg: &ProgramConfig) -> gst::Buffer {
    let (w, h) = (cfg.width as usize, cfg.height as usize);
    let mut data = vec![16u8; w * h]; // Y: video black
    data.resize(w * h * 3 / 2, 128); // UV: neutral
    gst::Buffer::from_mut_slice(data)
}

fn to_composition(l: lyrics::Layer) -> Option<gst_video::VideoOverlayComposition> {
    let mut buf = gst::Buffer::from_mut_slice(l.bgra);
    gst_video::VideoMeta::add(
        buf.get_mut()?,
        gst_video::VideoFrameFlags::empty(),
        gst_video::VideoFormat::Bgra,
        l.width,
        l.height,
    )
    .ok()?;
    let rect = gst_video::VideoOverlayRectangle::new_raw(
        &buf,
        l.x,
        l.y,
        l.width,
        l.height,
        gst_video::VideoOverlayFormatFlags::PREMULTIPLIED_ALPHA,
    );
    gst_video::VideoOverlayComposition::new(Some(&rect)).ok()
}

impl Program {
    pub fn new(cfg: ProgramConfig, feed: Arc<Feed>, font: &'static [u8]) -> Program {
        let gpu = has("d3d12upload") && has("d3d12overlaycompositor") && has("d3d12download");
        let order = encoder_order(cfg.encoder, has);
        let sources: Arc<Mutex<Sources>> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        start_pacers(&cfg, sources.clone(), feed.clone(), alive.clone());
        let mut p = Program {
            cfg,
            gpu,
            order,
            state: State::Failed { retry_at: Instant::now() },
            pipeline: None,
            sources,
            feed,
            font,
            current: None,
            alive,
            detail: String::new(),
            last_frames: 0,
            last_bytes: 0,
            out_fps: 0.0,
            kbps: 0.0,
        };
        if p.order.is_empty() {
            p.detail = "no H.264 encoder is installed".into();
        }
        p
    }

    pub fn config(&self) -> &ProgramConfig {
        &self.cfg
    }

    /// The program's tee elements, for the recording and stream branches to attach to.
    #[allow(dead_code)] // used by the recording and streaming items
    pub fn pipeline(&self) -> Option<&gst::Pipeline> {
        self.pipeline.as_ref()
    }

    fn stop(&mut self) {
        {
            let mut s = self.sources.lock().unwrap();
            (s.video, s.audio, s.start) = (None, None, None);
        }
        if let Some(p) = self.pipeline.take() {
            let _ = p.set_state(gst::State::Null);
        }
    }

    fn try_encoder(&mut self, index: usize) {
        self.stop();
        let Some(&enc) = self.order.get(index) else {
            self.detail = format!("no encoder worked ({}); retrying", self.detail);
            log("video", format!("program: {}", self.detail));
            self.state = State::Failed { retry_at: Instant::now() + RETRY };
            return;
        };
        match self.build(enc) {
            Ok(()) => {
                log("video", format!("program: trying encoder {}", enc.name()));
                self.current = Some(enc);
                self.state = State::Trying { index, since: Instant::now() };
            }
            Err(e) => {
                self.detail = format!("{}: {e}", enc.name());
                log("video", format!("program: {}", self.detail));
                self.try_encoder(index + 1);
            }
        }
    }

    fn build(&mut self, enc: EncoderChoice) -> Result<(), String> {
        let pipeline = gst::parse::launch(&description(enc, &self.cfg, self.gpu))
            .map_err(|e| e.to_string())?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        let get = |name: &str| pipeline.by_name(name).ok_or(format!("no element {name}"));
        let vsrc = get("vsrc")?.downcast::<gst_app::AppSrc>().map_err(|_| "vsrc is not an appsrc")?;
        let asrc = get("asrc")?.downcast::<gst_app::AppSrc>().map_err(|_| "asrc is not an appsrc")?;

        // Count encoded frames and bytes where they leave the encoder.
        let feed = self.feed.clone();
        get("vmon")?.static_pad("sink").ok_or("vmon has no pad")?.add_probe(
            gst::PadProbeType::BUFFER,
            move |_, info| {
                if let Some(b) = info.buffer() {
                    feed.frames_encoded.fetch_add(1, Ordering::Relaxed);
                    feed.bytes_encoded.fetch_add(b.size() as u64, Ordering::Relaxed);
                }
                gst::PadProbeReturn::Ok
            },
        );

        // Lyric layer: drawn only when the text changes, then reused for every frame.
        let (feed, font, w, h) = (self.feed.clone(), self.font, self.cfg.width, self.cfg.height);
        let cache: Mutex<(Option<Vec<String>>, Option<gst_video::VideoOverlayComposition>)> = Mutex::default();
        let face = ab_glyph::FontRef::try_from_slice(font).map_err(|e| format!("font: {e}"))?;
        get("lyrics")?.connect("draw", false, move |_| {
            let lines = lines_for(*feed.live.lock().unwrap());
            let mut c = cache.lock().unwrap();
            if c.0.as_ref() != Some(&lines) {
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                c.1 = lyrics::render(&face, &refs, w, h, &lyrics::Style::default()).and_then(to_composition);
                c.0 = Some(lines.clone());
                *feed.overlay.lock().unwrap() = lines;
                feed.overlay_renders.fetch_add(1, Ordering::Relaxed);
            }
            Some(c.1.clone().to_value())
        });

        pipeline.set_state(gst::State::Playing).map_err(|_| "would not start")?;
        {
            let mut s = self.sources.lock().unwrap();
            (s.video, s.audio, s.start) = (Some(vsrc), Some(asrc), Some(Instant::now()));
            s.generation += 1;
        }
        self.pipeline = Some(pipeline);
        Ok(())
    }

    /// Call every tick: handles pipeline messages and moves the encoder trial along.
    pub fn tick(&mut self) {
        let mut error = None;
        if let Some(bus) = self.pipeline.as_ref().and_then(|p| p.bus()) {
            while let Some(msg) = bus.pop() {
                if let gst::MessageView::Error(e) = msg.view() {
                    error =
                        Some(format!("{} ({})", e.error(), e.src().map(|s| s.name().to_string()).unwrap_or_default()));
                }
            }
        }
        let encoded = self.feed.frames_encoded.load(Ordering::Relaxed);
        match self.state {
            State::Trying { index, since } => {
                let enc = self.order[index];
                if let Some(e) = error {
                    self.detail = format!("{}: {e}", enc.name());
                    log("video", format!("program: {}", self.detail));
                    self.try_encoder(index + 1);
                } else if encoded > self.last_frames {
                    log("video", format!("program: running on {}", enc.name()));
                    self.detail.clear();
                    self.state = State::Running;
                } else if since.elapsed() > TRIAL {
                    self.detail = format!("{} produced no frames", enc.name());
                    log("video", format!("program: {}", self.detail));
                    self.try_encoder(index + 1);
                }
            }
            State::Running => {
                if let Some(e) = error {
                    // Start over from the preferred encoder: a one-off failure shouldn't
                    // demote the rest of the service to a worse one.
                    self.detail = format!("program failed: {e}; restarting");
                    log("video", &self.detail);
                    self.try_encoder(0);
                }
            }
            State::Failed { retry_at } => {
                if Instant::now() >= retry_at && !self.order.is_empty() {
                    self.try_encoder(0);
                }
            }
        }
    }

    /// Call once a second with the time since the last call: updates the rates.
    pub fn measure(&mut self, elapsed: Duration) {
        let frames = self.feed.frames_encoded.load(Ordering::Relaxed);
        let bytes = self.feed.bytes_encoded.load(Ordering::Relaxed);
        let secs = elapsed.as_secs_f64().max(0.001);
        self.out_fps = (frames - self.last_frames) as f64 / secs;
        self.kbps = (bytes - self.last_bytes) as f64 * 8.0 / 1000.0 / secs;
        (self.last_frames, self.last_bytes) = (frames, bytes);
    }

    pub fn status(&self) -> ProgramStatus {
        let (state, encoder) = match self.state {
            State::Trying { .. } => ("starting", self.current.map(|e| e.name()).unwrap_or("")),
            State::Running => ("running", self.current.map(|e| e.name()).unwrap_or("")),
            State::Failed { .. } => ("failed", ""),
        };
        ProgramStatus {
            state,
            encoder: encoder.into(),
            width: self.cfg.width,
            height: self.cfg.height,
            fps: self.cfg.fps,
            out_fps: self.out_fps,
            kbps: self.kbps,
            frames_encoded: self.feed.frames_encoded.load(Ordering::Relaxed),
            slate: self.feed.slate.load(Ordering::Relaxed),
            overlay: self.feed.overlay.lock().unwrap().clone(),
            overlay_renders: self.feed.overlay_renders.load(Ordering::Relaxed),
            detail: self.detail.clone(),
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        self.stop();
    }
}

/// Starts the two pacer threads. They run until the `Program` is dropped and push into
/// whatever pipeline it currently has, so an encoder restart picks up seamlessly.
fn start_pacers(cfg: &ProgramConfig, sources: Arc<Mutex<Sources>>, feed: Arc<Feed>, alive: Arc<AtomicBool>) {
    let (fps, frame) = (cfg.fps, cfg.frame_duration());
    let slate = slate(cfg);
    let (s, f, a) = (sources.clone(), feed.clone(), alive.clone());
    std::thread::spawn(move || {
        let mut sent = 0u64;
        let mut generation = 0u64;
        while a.load(Ordering::Relaxed) {
            let (src, start) = {
                let g = s.lock().unwrap();
                if g.generation != generation {
                    (generation, sent) = (g.generation, 0);
                }
                (g.video.clone(), g.start)
            };
            let (Some(src), Some(start)) = (src, start) else {
                std::thread::sleep(frame);
                continue;
            };
            let due = frames_due(start.elapsed(), fps);
            if due > sent + 3 {
                sent = due - 1; // fell behind (e.g. the machine stalled): skip, don't rush
            }
            if due > sent {
                let latest = f.frame.lock().unwrap().clone();
                let source = frame_source(latest.as_ref().map(|(at, _)| at.elapsed()));
                f.slate.store(source == FrameSource::Slate, Ordering::Relaxed);
                let base = match (source, latest) {
                    (FrameSource::Camera, Some((_, b))) => b,
                    _ => slate.clone(),
                };
                // A shallow copy shares the pixels; only the timestamps are new.
                let mut b = base.copy();
                if let Some(m) = b.get_mut() {
                    m.set_pts(gst::ClockTime::from_nseconds(sent * frame.as_nanos() as u64));
                    m.set_duration(gst::ClockTime::from_nseconds(frame.as_nanos() as u64));
                }
                let _ = src.push_buffer(b);
                sent += 1;
            }
            let next = start + next_frame_at(sent, fps);
            std::thread::sleep(next.saturating_duration_since(Instant::now()).min(frame));
        }
    });
    std::thread::spawn(move || {
        let mut sent = 0u64; // audio frames (samples per channel)
        let mut generation = 0u64;
        while alive.load(Ordering::Relaxed) {
            std::thread::sleep(AUDIO_CHUNK);
            let (src, start) = {
                let g = sources.lock().unwrap();
                if g.generation != generation {
                    (generation, sent) = (g.generation, 0);
                }
                (g.audio.clone(), g.start)
            };
            let (Some(src), Some(start)) = (src, start) else { continue };
            let due = (start.elapsed().as_nanos() * AUDIO_RATE as u128 / 1_000_000_000) as u64;
            if due <= sent {
                continue;
            }
            let frames = (due - sent) as usize;
            let samples = feed.audio.lock().unwrap().take(frames);
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut b = gst::Buffer::from_mut_slice(bytes);
            if let Some(m) = b.get_mut() {
                m.set_pts(gst::ClockTime::from_nseconds(sent * 1_000_000_000 / AUDIO_RATE as u64));
                m.set_duration(gst::ClockTime::from_nseconds(frames as u64 * 1_000_000_000 / AUDIO_RATE as u64));
            }
            let _ = src.push_buffer(b);
            sent = due;
        }
    });
}
