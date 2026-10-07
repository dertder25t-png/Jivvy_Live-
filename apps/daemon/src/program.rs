//! The program feed: what the stream and the recording carry. Pure logic here; the
//! GStreamer pipeline lives in `jivvy-video`.
//!
//! The program never stops because an input does. A clock-driven pacer pushes exactly
//! `fps` frames a second (the newest camera frame, or a slate when the camera is missing or
//! stalled) and a steady run of audio (the microphone, padded with silence). The lyric
//! layer is drawn only when the slide changes and composited on the GPU where available.
//! It is encoded once; the recording and the stream branch off that one encode.

use std::collections::VecDeque;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::client::LiveState;

pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u32 = 2;
/// Audio is pushed in chunks of this length.
pub const AUDIO_CHUNK: Duration = Duration::from_millis(20);
/// A camera frame older than this is "stalled": the slate is shown instead.
pub const CAMERA_STALE: Duration = Duration::from_millis(500);
/// Microphone audio buffered beyond this is dropped, so a hiccup never adds lasting delay.
pub const AUDIO_MAX_BACKLOG: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EncoderChoice {
    /// The best encoder known to work on this kind of machine, falling back in order.
    #[default]
    Auto,
    /// Windows Media Foundation (the vendor's hardware encoder).
    Mf,
    /// Intel Quick Sync through GStreamer's plugin (crashes on some drivers; see the spike).
    Qsv,
    /// Software (x264). Works everywhere; about one core at 1080p30.
    X264,
}

impl EncoderChoice {
    pub fn name(self) -> &'static str {
        match self {
            EncoderChoice::Auto => "auto",
            EncoderChoice::Mf => "mf",
            EncoderChoice::Qsv => "qsv",
            EncoderChoice::X264 => "x264",
        }
    }

    /// The GStreamer element this encoder needs.
    pub fn element(self) -> &'static str {
        match self {
            EncoderChoice::Mf => "mfh264enc",
            EncoderChoice::Qsv => "qsvh264enc",
            EncoderChoice::X264 | EncoderChoice::Auto => "x264enc",
        }
    }
}

/// `program` in `media.json`. Every field has a default, so older files keep working.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProgramConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub encoder: EncoderChoice,
}

impl Default for ProgramConfig {
    fn default() -> Self {
        ProgramConfig { width: 1920, height: 1080, fps: 30, bitrate_kbps: 6000, encoder: EncoderChoice::Auto }
    }
}

impl ProgramConfig {
    /// Clamps nonsense to something that streams: even sizes 320–3840 × 180–2160,
    /// 15–60 fps, 500–20,000 kbps.
    pub fn sanitized(&self) -> ProgramConfig {
        let even = |v: u32, lo: u32, hi: u32| v.clamp(lo, hi) & !1;
        ProgramConfig {
            width: even(self.width, 320, 3840),
            height: even(self.height, 180, 2160),
            fps: self.fps.clamp(15, 60),
            bitrate_kbps: self.bitrate_kbps.clamp(500, 20_000),
            encoder: self.encoder,
        }
    }

    pub fn frame_duration(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / self.fps.max(1) as u64)
    }
}

/// Encoders to try, in order. A configured one comes first, but the others follow, so a
/// broken driver never leaves a church without a stream. Quick Sync is never chosen
/// automatically (it crashed on the founder's laptop); only when asked for by name.
pub fn encoder_order(choice: EncoderChoice, available: impl Fn(&str) -> bool) -> Vec<EncoderChoice> {
    let mut order = match choice {
        EncoderChoice::Auto => vec![],
        c => vec![c],
    };
    for c in [EncoderChoice::Mf, EncoderChoice::X264] {
        if !order.contains(&c) {
            order.push(c);
        }
    }
    order.retain(|c| available(c.element()));
    order
}

/// Which picture the pacer sends for this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSource {
    Camera,
    Slate,
}

/// The newest camera frame if it is fresh, else the slate.
pub fn frame_source(camera_frame_age: Option<Duration>) -> FrameSource {
    match camera_frame_age {
        Some(age) if age < CAMERA_STALE => FrameSource::Camera,
        _ => FrameSource::Slate,
    }
}

/// How many frames the pacer should have pushed after `elapsed`, so it never drifts and,
/// after a stall, skips ahead instead of rushing frames out.
pub fn frames_due(elapsed: Duration, fps: u32) -> u64 {
    (elapsed.as_nanos() * fps as u128 / 1_000_000_000) as u64
}

/// When (after the program start) the pacer should wake to push the next frame, once
/// `sent` frames are out: the moment `frames_due` reaches `sent + 1`. Waking earlier than
/// this finds nothing due and would spin.
pub fn next_frame_at(sent: u64, fps: u32) -> Duration {
    // Round up so frames_due is guaranteed to have moved on by then.
    Duration::from_nanos(((sent as u128 + 1) * 1_000_000_000).div_ceil(fps.max(1) as u128) as u64)
}

/// Microphone samples waiting to go into the program (interleaved S16, stereo).
#[derive(Debug, Default)]
pub struct AudioFifo {
    samples: VecDeque<i16>,
}

impl AudioFifo {
    fn max_len() -> usize {
        (AUDIO_MAX_BACKLOG.as_millis() as usize) * AUDIO_RATE as usize / 1000 * AUDIO_CHANNELS as usize
    }

    /// Adds captured samples, dropping the oldest beyond the backlog limit.
    pub fn push(&mut self, samples: &[i16]) {
        self.samples.extend(samples);
        let excess = self.samples.len().saturating_sub(Self::max_len());
        // Drop whole frames (both channels) so left and right never swap.
        let excess = excess + excess % AUDIO_CHANNELS as usize;
        self.samples.drain(..excess.min(self.samples.len()));
    }

    /// Takes exactly `frames` stereo frames, padding with silence when the mic is short.
    pub fn take(&mut self, frames: usize) -> Vec<i16> {
        let n = frames * AUDIO_CHANNELS as usize;
        let mut out: Vec<i16> = self.samples.drain(..n.min(self.samples.len())).collect();
        out.resize(n, 0);
        out
    }

    pub fn len_frames(&self) -> usize {
        self.samples.len() / AUDIO_CHANNELS as usize
    }
}

/// Text on the stream for the engine's state: nothing when black. Shows "Slide N" until
/// run sheets reach the engine, like the output windows.
pub fn lines_for(state: Option<LiveState>) -> Vec<String> {
    match state {
        Some(s) if !s.black => vec![format!("Slide {}", s.slide_index + 1)],
        _ => vec![],
    }
}

/// `program` in `media-status.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatus {
    /// "running", "starting" or "failed" (retried every second).
    pub state: &'static str,
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Encoded frames per second over the last second.
    pub out_fps: f64,
    /// Encoded video bitrate over the last second.
    pub kbps: f64,
    pub frames_encoded: u64,
    /// The slate is on because the camera is missing or stalled.
    pub slate: bool,
    /// Lyric text on the stream right now.
    pub overlay: Vec<String>,
    /// Times the lyric layer was drawn (once per slide change).
    pub overlay_renders: u64,
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_order_falls_back_and_never_picks_quick_sync_by_itself() {
        let all = |_: &str| true;
        assert_eq!(encoder_order(EncoderChoice::Auto, all), [EncoderChoice::Mf, EncoderChoice::X264]);
        assert_eq!(
            encoder_order(EncoderChoice::Qsv, all),
            [EncoderChoice::Qsv, EncoderChoice::Mf, EncoderChoice::X264]
        );
        assert_eq!(encoder_order(EncoderChoice::X264, all), [EncoderChoice::X264, EncoderChoice::Mf]);
        let linux = |e: &str| e == "x264enc";
        assert_eq!(encoder_order(EncoderChoice::Mf, linux), [EncoderChoice::X264]);
        assert!(encoder_order(EncoderChoice::Auto, |_| false).is_empty());
    }

    #[test]
    fn config_defaults_and_clamps() {
        let c: ProgramConfig = serde_json::from_str(r#"{"bitrateKbps": 4500}"#).unwrap();
        assert_eq!((c.width, c.height, c.fps, c.bitrate_kbps, c.encoder), (1920, 1080, 30, 4500, EncoderChoice::Auto));
        let odd = ProgramConfig { width: 1281, height: 9999, fps: 240, bitrate_kbps: 1, encoder: EncoderChoice::X264 };
        let s = odd.sanitized();
        assert_eq!((s.width, s.height, s.fps, s.bitrate_kbps), (1280, 2160, 60, 500));
        assert_eq!(ProgramConfig::default().frame_duration(), Duration::from_nanos(33_333_333));
    }

    #[test]
    fn a_stalled_or_missing_camera_shows_the_slate() {
        assert_eq!(frame_source(Some(Duration::from_millis(30))), FrameSource::Camera);
        assert_eq!(frame_source(Some(Duration::from_millis(600))), FrameSource::Slate);
        assert_eq!(frame_source(None), FrameSource::Slate);
    }

    #[test]
    fn pacing_follows_the_clock_and_skips_ahead_after_a_stall() {
        assert_eq!(frames_due(Duration::from_millis(1000), 30), 30);
        assert_eq!(frames_due(Duration::from_millis(1033), 30), 30);
        assert_eq!(frames_due(Duration::from_secs(3600), 30), 108_000);
    }

    #[test]
    fn the_pacer_wakes_exactly_when_the_next_frame_is_due() {
        for fps in [24, 25, 30, 50, 60] {
            for sent in [0u64, 1, 29, 1000, 107_999] {
                let at = next_frame_at(sent, fps);
                assert_eq!(frames_due(at, fps), sent + 1, "{fps} fps, {sent} sent: due at the wake-up");
                assert_eq!(frames_due(at - Duration::from_nanos(1), fps), sent, "not before it (no busy loop)");
            }
        }
    }

    #[test]
    fn audio_is_padded_with_silence_and_never_builds_up_delay() {
        let mut f = AudioFifo::default();
        f.push(&[1, 2, 3, 4]);
        assert_eq!(f.take(3), [1, 2, 3, 4, 0, 0], "short microphone audio is padded with silence");
        assert_eq!(f.take(2), [0, 0, 0, 0], "no microphone at all is silence");
        // A second of audio arrives at once: only the last 200 ms is kept.
        let second: Vec<i16> = (0..AUDIO_RATE * AUDIO_CHANNELS).map(|i| (i % 1000) as i16).collect();
        f.push(&second);
        assert_eq!(f.len_frames(), (AUDIO_RATE / 5) as usize);
        let first = f.take(1);
        assert_eq!(first[0] % 2, 0, "whole stereo frames are dropped, so channels never swap");
    }

    #[test]
    fn stream_text_follows_the_slide_and_black() {
        assert_eq!(lines_for(Some(LiveState { slide_index: 2, black: false })), ["Slide 3"]);
        assert!(lines_for(Some(LiveState { slide_index: 2, black: true })).is_empty());
        assert!(lines_for(None).is_empty());
    }
}
