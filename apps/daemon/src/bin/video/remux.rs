//! Checking recordings and converting them to MP4. A part is checked by reading it back
//! end to end (and, for `--verify-recording`, decoding it): the proof that a file a crash
//! interrupted still plays. Conversion copies the video and audio into an MP4 without
//! re-encoding, checks the MP4 holds the same frames, and only then removes the Matroska
//! original.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use serde::Serialize;

use jivvy_daemon::log;
use jivvy_daemon::recording::{self, Conversion, RecordingInfo};

use crate::program::Feed;

/// What reading a file back found.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verified {
    pub video_frames: u64,
    pub audio_buffers: u64,
    /// Where the last video frame ends, milliseconds.
    pub duration_ms: u64,
    pub width: i32,
    pub height: i32,
}

/// A read that makes no progress for this long is given up on.
const STALL: Duration = Duration::from_secs(30);

/// Reads `path` to its end through the demuxer and parsers (and decoders, when `decode`).
/// Err if the file can't be read to the end or has no video; a file cut short by a crash
/// passes if everything before the cut reads cleanly. `stop` ends the read early (Err).
pub fn verify(path: &Path, decode: bool, stop: &AtomicBool) -> Result<Verified, String> {
    let mp4 = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    let demux = if mp4 { "qtdemux" } else { "matroskademux" };
    let (video_decoder, audio_decoder) = if decode { ("! avdec_h264 ", "! avdec_aac ") } else { ("", "") };
    let description = format!(
        "filesrc name=src ! {demux} name=d \
         d.video_0 ! queue ! h264parse {video_decoder}! fakesink name=vs sync=false async=false \
         d.audio_0 ! queue ! aacparse {audio_decoder}! fakesink name=as sync=false async=false"
    );
    let pipeline = gst::parse::launch(&description)
        .map_err(|e| e.to_string())?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "not a pipeline")?;
    let get = |n: &str| pipeline.by_name(n).ok_or(format!("no element {n}"));
    get("src")?.set_property("location", path.to_string_lossy().as_ref());

    let video = Arc::new(AtomicU64::new(0));
    let audio = Arc::new(AtomicU64::new(0));
    let end_ms = Arc::new(AtomicU64::new(0));
    let size = Arc::new(Mutex::new((0i32, 0i32)));
    {
        let (video, end_ms, size) = (video.clone(), end_ms.clone(), size.clone());
        get("vs")?.static_pad("sink").ok_or("no video pad")?.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            if let Some(b) = info.buffer() {
                video.fetch_add(1, Ordering::Relaxed);
                if let Some(t) = b.pts().map(|p| p + b.duration().unwrap_or(gst::ClockTime::ZERO)) {
                    end_ms.fetch_max(t.mseconds(), Ordering::Relaxed);
                }
                if let Some(s) = pad.current_caps().and_then(|c| c.structure(0).map(|s| s.to_owned())) {
                    *size.lock().unwrap() = (s.get::<i32>("width").unwrap_or(0), s.get::<i32>("height").unwrap_or(0));
                }
            }
            gst::PadProbeReturn::Ok
        });
        let audio = audio.clone();
        get("as")?.static_pad("sink").ok_or("no audio pad")?.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            audio.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    }

    let finish = |p: &gst::Pipeline| {
        let _ = p.set_state(gst::State::Null);
    };
    if pipeline.set_state(gst::State::Playing).is_err() {
        finish(&pipeline);
        return Err("the file would not open".into());
    }
    let bus = pipeline.bus().ok_or("no bus")?;
    let (mut progress, mut seen_at) = (0u64, Instant::now());
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Err("stopped".to_string());
        }
        match bus.timed_pop(gst::ClockTime::from_mseconds(100)).as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Eos(_)) => break Ok(()),
            Some(gst::MessageView::Error(e)) => {
                let debug = e.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                break Err(format!("{}{debug}", e.error()));
            }
            _ => {}
        }
        let now = video.load(Ordering::Relaxed) + audio.load(Ordering::Relaxed);
        if now != progress {
            (progress, seen_at) = (now, Instant::now());
        } else if seen_at.elapsed() > STALL {
            break Err("reading the file stalled".into());
        }
    };
    finish(&pipeline);
    result?;
    let (video_frames, audio_buffers) = (video.load(Ordering::Relaxed), audio.load(Ordering::Relaxed));
    if video_frames == 0 {
        return Err("the file has no video".into());
    }
    let (width, height) = *size.lock().unwrap();
    Ok(Verified { video_frames, audio_buffers, duration_ms: end_ms.load(Ordering::Relaxed), width, height })
}

/// Copies one Matroska part into an MP4 (no re-encoding). Returns when the MP4 is written
/// to `to`; Err (and no file) if it couldn't be, or `stop` was set.
fn remux(from: &Path, to: &Path, stop: &AtomicBool) -> Result<(), String> {
    let pipeline = gst::parse::launch(
        "filesrc name=src ! matroskademux name=d \
         d.video_0 ! queue ! h264parse ! video/x-h264,stream-format=avc,alignment=au ! mux.video_0 \
         d.audio_0 ! queue ! aacparse ! mux.audio_0 \
         mp4mux name=mux faststart=true ! filesink name=out sync=false async=false",
    )
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "not a pipeline")?;
    let get = |n: &str| pipeline.by_name(n).ok_or(format!("no element {n}"));
    get("src")?.set_property("location", from.to_string_lossy().as_ref());
    get("out")?.set_property("location", to.to_string_lossy().as_ref());
    pipeline.set_state(gst::State::Playing).map_err(|_| "the converter would not start")?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Err("stopped".to_string());
        }
        match bus.timed_pop(gst::ClockTime::from_mseconds(100)).as_ref().map(|m| m.view()) {
            Some(gst::MessageView::Eos(_)) => break Ok(()),
            Some(gst::MessageView::Error(e)) => break Err(e.error().to_string()),
            _ => {}
        }
    };
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// The MP4 holds what the Matroska part held.
fn same_content(mkv: &Verified, mp4: &Verified) -> Result<(), String> {
    if mkv.video_frames != mp4.video_frames {
        return Err(format!("the MP4 has {} video frames, the original {}", mp4.video_frames, mkv.video_frames));
    }
    if mkv.audio_buffers.abs_diff(mp4.audio_buffers) > 2 {
        return Err(format!("the MP4 has {} audio buffers, the original {}", mp4.audio_buffers, mkv.audio_buffers));
    }
    Ok(())
}

/// Converts one part: remux to a `.converting.mp4` file, read it back, and only if it matches the
/// original rename it into place and remove the original. A part that can't be read at all
/// is left alone.
pub fn convert(job: &Conversion, stop: &AtomicBool) -> Result<(), String> {
    let partial = job.mp4.with_extension("converting.mp4");
    let _ = std::fs::remove_file(&partial);
    let result = (|| {
        let original = verify(&job.mkv, false, stop).map_err(|e| format!("the original can't be read: {e}"))?;
        remux(&job.mkv, &partial, stop)?;
        let copy = verify(&partial, false, stop).map_err(|e| format!("the converted file can't be read: {e}"))?;
        same_content(&original, &copy)?;
        std::fs::rename(&partial, &job.mp4).map_err(|e| e.to_string())?;
        // Only now that the MP4 is whole and complete is the original no longer needed.
        std::fs::remove_file(&job.mkv).map_err(|e| format!("converted, but couldn't remove the original: {e}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// Converts finished recordings in the background, whenever neither the recording nor the
/// stream is wanted, and stops the moment either is. A part that fails is left as it is and
/// not tried again until the video process next starts.
pub fn start_converter(data_dir: PathBuf, feed: Arc<Feed>, status: Arc<Mutex<RecordingInfo>>) {
    std::thread::spawn(move || run(&data_dir, &feed, &status));
}

/// True while nothing is live: the recording and the stream are both off, as far as the
/// engine has said.
fn idle(feed: &Feed) -> bool {
    feed.live.lock().unwrap().is_some_and(|s| !s.recording_wanted && !s.stream_wanted)
}

fn run(data_dir: &Path, feed: &Arc<Feed>, status: &Mutex<RecordingInfo>) {
    let mut failed: Vec<PathBuf> = Vec::new();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if !idle(feed) {
            continue;
        }
        let folder = recording::load_config(data_dir).unwrap_or_default().folder(data_dir);
        let Some(job) = recording::to_convert(data_dir, &folder).into_iter().find(|j| !failed.contains(&j.mkv)) else {
            continue;
        };
        // Stops as soon as a recording or stream is asked for.
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (stop, feed) = (stop.clone(), feed.clone());
            let done = Arc::new(AtomicBool::new(false));
            let d = done.clone();
            let handle = std::thread::spawn(move || {
                while !d.load(Ordering::Relaxed) {
                    if !idle(&feed) {
                        stop.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
            (done, handle)
        };
        let name = job.mkv.display().to_string();
        status.lock().unwrap().converting =
            format!("Converting {} to MP4", job.mkv.file_name().unwrap_or_default().to_string_lossy());
        let result = convert(&job, &stop);
        watcher.0.store(true, Ordering::Relaxed);
        let _ = watcher.1.join();
        let mut s = status.lock().unwrap();
        s.converting.clear();
        match result {
            Ok(()) => {
                s.converted += 1;
                log("video", format!("recording: converted {name} to MP4"));
            }
            Err(e) if stop.load(Ordering::Relaxed) => {
                log("video", format!("recording: conversion of {name} paused ({e})"))
            }
            Err(e) => {
                log("video", format!("recording: could not convert {name}: {e}; leaving it as it is"));
                failed.push(job.mkv);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("jivvy-remux-{name}-{}-{}", std::process::id(), jivvy_daemon::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A short Matroska file written the way the recorder writes it (streamed, with a
    /// keyframe every second), from test sources.
    fn record(path: &Path, seconds: u32) {
        gst::init().unwrap();
        let p = gst::parse::launch(&format!(
            "videotestsrc num-buffers={} ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! x264enc key-int-max=30 bframes=0 tune=zerolatency ! h264parse ! mux.video_0 \
             audiotestsrc num-buffers={} samplesperbuffer=1024 ! audioconvert ! avenc_aac ! aacparse ! mux.audio_0 \
             matroskamux name=mux streamable=true ! filesink name=out",
            seconds * 30,
            seconds * 47
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        p.by_name("out").unwrap().set_property("location", path.to_string_lossy().as_ref());
        p.set_state(gst::State::Playing).unwrap();
        let bus = p.bus().unwrap();
        let msg =
            bus.timed_pop_filtered(gst::ClockTime::from_seconds(60), &[gst::MessageType::Eos, gst::MessageType::Error]);
        assert!(matches!(msg.as_ref().map(|m| m.view()), Some(gst::MessageView::Eos(_))), "{msg:?}");
        p.set_state(gst::State::Null).unwrap();
    }

    static NEVER: AtomicBool = AtomicBool::new(false);

    #[test]
    fn a_whole_file_and_one_cut_off_mid_write_both_play_but_garbage_does_not() {
        let d = dir("verify");
        let whole = d.join("whole.mkv");
        record(&whole, 4);
        let all = verify(&whole, true, &NEVER).unwrap();
        assert_eq!((all.video_frames, all.width, all.height), (120, 320, 240));
        assert!(all.audio_buffers > 100 && (3_900..=4_200).contains(&all.duration_ms), "{all:?}");

        // What a crash leaves: the file cut off at some point, even inside a cluster.
        let bytes = std::fs::read(&whole).unwrap();
        for part in [bytes.len() / 2, bytes.len() * 9 / 10, bytes.len() - 7] {
            let cut = d.join(format!("cut-{part}.mkv"));
            std::fs::write(&cut, &bytes[..part]).unwrap();
            let found = verify(&cut, true, &NEVER).unwrap_or_else(|e| panic!("cut at {part} of {}: {e}", bytes.len()));
            assert!(found.video_frames > 0 && found.video_frames <= all.video_frames, "{found:?}");
        }

        std::fs::write(d.join("garbage.mkv"), vec![7u8; 10_000]).unwrap();
        assert!(verify(&d.join("garbage.mkv"), true, &NEVER).is_err());
        std::fs::write(d.join("empty.mkv"), b"").unwrap();
        assert!(verify(&d.join("empty.mkv"), true, &NEVER).is_err());
        assert!(verify(&d.join("missing.mkv"), true, &NEVER).is_err());
    }

    #[test]
    fn converting_keeps_every_frame_and_only_then_removes_the_original() {
        let d = dir("convert");
        let job = Conversion { mkv: d.join("part-001.mkv"), mp4: d.join("part-001.mp4") };
        record(&job.mkv, 3);
        let before = verify(&job.mkv, false, &NEVER).unwrap();
        convert(&job, &NEVER).unwrap();
        assert!(!job.mkv.exists(), "the original goes once the MP4 is checked");
        assert!(!d.join("part-001.converting.mp4").exists());
        let after = verify(&job.mp4, true, &NEVER).unwrap();
        assert_eq!(after.video_frames, before.video_frames);
        assert!(after.audio_buffers.abs_diff(before.audio_buffers) <= 2, "{before:?} {after:?}");
    }

    #[test]
    fn a_part_that_cannot_be_read_is_left_alone_and_leaves_nothing_half_done() {
        gst::init().unwrap();
        let d = dir("bad-convert");
        let job = Conversion { mkv: d.join("part-001.mkv"), mp4: d.join("part-001.mp4") };
        std::fs::write(&job.mkv, vec![7u8; 10_000]).unwrap();
        assert!(convert(&job, &NEVER).is_err());
        assert!(job.mkv.exists(), "never throw away what couldn't be converted");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no MP4, no half-converted file");
    }

    #[test]
    fn stopping_ends_a_conversion_without_touching_the_original() {
        let d = dir("stop-convert");
        let job = Conversion { mkv: d.join("part-001.mkv"), mp4: d.join("part-001.mp4") };
        record(&job.mkv, 2);
        let stop = AtomicBool::new(true);
        assert!(convert(&job, &stop).is_err());
        assert!(job.mkv.exists());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
    }

    #[test]
    fn the_converted_copy_must_hold_the_same_frames() {
        let v =
            |video_frames, audio_buffers| Verified { video_frames, audio_buffers, duration_ms: 0, width: 0, height: 0 };
        assert!(same_content(&v(100, 150), &v(100, 151)).is_ok(), "one audio buffer either way is rounding");
        assert!(same_content(&v(100, 150), &v(99, 150)).unwrap_err().contains("video frames"));
        assert!(same_content(&v(100, 150), &v(100, 120)).unwrap_err().contains("audio"));
    }
}
