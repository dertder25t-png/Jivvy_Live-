//! Lower-quality stream encodes (720p, 480p, 360p), for destinations the bandwidth manager
//! has stepped down. Each is its own small pipeline fed the program's composited picture,
//! started only while a destination uses it, so the program and the recording stay at
//! full quality and never notice one failing. See `jivvy_daemon::bandwidth`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::bandwidth::{Quality, Tier};
use jivvy_daemon::log;
use jivvy_daemon::program::{ENCODER_TRIAL, EncoderChoice, ProgramConfig};
use jivvy_daemon::stream::EncodeStatus;

use crate::program::{Feed, encoder_description};
use crate::stream::running_time;

/// A tier nobody has used for this long is stopped.
const IDLE: Duration = Duration::from_secs(10);

struct Input {
    tier: Tier,
    src: gst_app::AppSrc,
    /// The input's caps; a different picture size means the encode is rebuilt.
    caps: Mutex<Option<gst::Caps>>,
    last: Mutex<Option<gst::ClockTime>>,
    /// The program restarted (timestamps from zero again) or changed size: rebuild.
    stale: AtomicBool,
}

/// Hands the program's composited picture to the running lower-quality encodes.
#[derive(Default)]
pub struct Raw {
    inputs: Mutex<Vec<Arc<Input>>>,
}

impl Raw {
    /// True while any lower-quality encode runs (the program opens its valve then).
    pub fn wanted(&self) -> bool {
        !self.inputs.lock().unwrap().is_empty()
    }

    /// One composited frame. Never blocks: each encode's input drops old frames.
    pub fn publish(&self, sample: &gst::Sample) {
        let Some((pts, _)) = running_time(sample) else { return };
        let Some(buf) = sample.buffer() else { return };
        for input in self.inputs.lock().unwrap().iter() {
            let mut caps = input.caps.lock().unwrap();
            match (&*caps, sample.caps()) {
                (None, Some(c)) => {
                    input.src.set_caps(Some(&c.to_owned()));
                    *caps = Some(c.to_owned());
                }
                (Some(have), Some(c)) if !have.is_strictly_equal(c) => {
                    input.stale.store(true, Ordering::Relaxed);
                    continue;
                }
                _ => {}
            }
            let mut last = input.last.lock().unwrap();
            if last.is_some_and(|l| pts + gst::ClockTime::from_seconds(1) < l) {
                input.stale.store(true, Ordering::Relaxed);
                continue;
            }
            *last = Some(pts);
            // Program running time, so these frames line up with the program's audio.
            let mut b = buf.copy();
            if let Some(m) = b.get_mut() {
                m.set_pts(pts);
                m.set_dts(gst::ClockTime::NONE);
            }
            let _ = input.src.push_buffer(b);
        }
    }

    fn add(&self, input: Arc<Input>) {
        self.inputs.lock().unwrap().push(input);
    }

    fn remove(&self, tier: Tier) {
        self.inputs.lock().unwrap().retain(|i| i.tier != tier);
    }
}

struct Encode {
    quality: Quality,
    encoder: EncoderChoice,
    pipeline: gst::Pipeline,
    input: Arc<Input>,
    frames: Arc<AtomicU64>,
    started: Instant,
}

impl Drop for Encode {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// The lower-quality encodes, kept in step with what destinations use.
#[derive(Default)]
pub struct Tiers {
    running: HashMap<Tier, Encode>,
    last_used: HashMap<Tier, Instant>,
    /// The encoder to try next per tier, after one failed.
    next_encoder: HashMap<Tier, usize>,
}

impl Tiers {
    /// Once a second: starts encodes destinations use, stops ones unused for a while, and
    /// rebuilds any that failed or whose input changed.
    pub fn reconcile(
        &mut self,
        ladder: &[Quality],
        program: &ProgramConfig,
        encoders: &[EncoderChoice],
        feed: &Arc<Feed>,
    ) {
        let now = Instant::now();
        let used = feed.taps.tiers_used();
        for t in &used {
            self.last_used.insert(*t, now);
        }
        let mut stop: Vec<Tier> = Vec::new();
        for (tier, e) in &self.running {
            let quality = ladder.iter().find(|q| q.tier == *tier);
            let idle = self.last_used.get(tier).is_none_or(|t| now.duration_since(*t) > IDLE);
            let silent = e.frames.load(Ordering::Relaxed) == 0 && e.started.elapsed() > ENCODER_TRIAL;
            let failed = e.pipeline.bus().is_some_and(|b| {
                let mut failed = false;
                while let Some(m) = b.pop() {
                    if let gst::MessageView::Error(err) = m.view() {
                        log("video", format!("{} encode failed: {}", e.quality.label(), err.error()));
                        failed = true;
                    }
                }
                failed
            });
            if failed || silent {
                let n = self.next_encoder.entry(*tier).or_insert(0);
                *n = (*n + 1) % encoders.len().max(1);
            }
            if idle || failed || silent || quality != Some(&e.quality) || e.input.stale.load(Ordering::Relaxed) {
                stop.push(*tier);
            }
        }
        for tier in stop {
            feed.raw.remove(tier);
            self.running.remove(&tier);
        }
        for q in ladder.iter().filter(|q| q.tier != Tier::Full && used.contains(&q.tier)) {
            if self.running.contains_key(&q.tier) || encoders.is_empty() {
                continue;
            }
            let encoder = encoders[self.next_encoder.get(&q.tier).copied().unwrap_or(0) % encoders.len()];
            match start(q, encoder, program, feed) {
                Ok(e) => {
                    log("video", format!("{} encode started on {}", q.label(), encoder.name()));
                    feed.raw.add(e.input.clone());
                    self.running.insert(q.tier, e);
                }
                Err(err) => {
                    log("video", format!("{} encode on {}: {err}", q.label(), encoder.name()));
                    let n = self.next_encoder.entry(q.tier).or_insert(0);
                    *n = (*n + 1) % encoders.len();
                }
            }
        }
    }

    pub fn status(&self) -> Vec<EncodeStatus> {
        let mut out: Vec<(Tier, EncodeStatus)> = self
            .running
            .iter()
            .map(|(t, e)| {
                let s = EncodeStatus {
                    quality: e.quality.label(),
                    encoder: e.encoder.name().into(),
                    frames_encoded: e.frames.load(Ordering::Relaxed),
                };
                (*t, s)
            })
            .collect();
        out.sort_by_key(|(t, _)| *t);
        out.into_iter().map(|(_, s)| s).collect()
    }
}

/// One lower-quality encode: scaled program picture in, H.264 out to the taps.
fn start(q: &Quality, encoder: EncoderChoice, program: &ProgramConfig, feed: &Arc<Feed>) -> Result<Encode, String> {
    let cfg = ProgramConfig { width: q.width, height: q.height, bitrate_kbps: q.video_kbps, ..program.clone() };
    // Not live: frames carry the program's running time, unrelated to this pipeline's clock.
    let description = format!(
        "appsrc name=src format=time leaky-type=downstream max-buffers=3 \
         ! videoscale ! video/x-raw,width={w},height={h} ! {enc} \
         ! h264parse config-interval=-1 ! video/x-h264,stream-format=avc,alignment=au \
         ! appsink name=out sync=false async=false max-buffers=32 drop=true",
        w = q.width,
        h = q.height,
        enc = encoder_description(encoder, &cfg, false),
    );
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| e.to_string())?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline")?;
    let get = |n: &str| pipeline.by_name(n).ok_or(format!("no element {n}"));
    let src = get("src")?.downcast::<gst_app::AppSrc>().map_err(|_| "src is not an appsrc")?;
    let frames = Arc::new(AtomicU64::new(0));
    let (taps, tier, count) = (feed.taps.clone(), q.tier, frames.clone());
    get("out")?.downcast::<gst_app::AppSink>().map_err(|_| "out is not an appsink")?.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                count.fetch_add(1, Ordering::Relaxed);
                taps.publish_tier(tier, sample);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    pipeline.set_state(gst::State::Playing).map_err(|_| "would not start")?;
    let input = Arc::new(Input {
        tier: q.tier,
        src,
        caps: Mutex::new(None),
        last: Mutex::new(None),
        stale: AtomicBool::new(false),
    });
    Ok(Encode { quality: *q, encoder, pipeline, input, frames, started: Instant::now() })
}
