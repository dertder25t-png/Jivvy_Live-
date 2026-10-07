//! Stage 0 tech spike: lyrics composited over video, hardware-encoded once,
//! sent to a crash-safe recording and (optionally) a YouTube stream, with CPU
//! and frame-rate measurements. See README.md for how to run it.

mod lyrics;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video as gst_video;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 30;

/// Public-domain hymn (John Newton, 1779) used as sample slides.
const SLIDES: &[&[&str]] = &[
    &["Amazing grace, how sweet the sound", "That saved a wretch like me"],
    &["I once was lost, but now am found", "Was blind, but now I see"],
    &["'Twas grace that taught my heart to fear", "And grace my fears relieved"],
    &["How precious did that grace appear", "The hour I first believed"],
    &[],
];

struct Args {
    source: String,
    seconds: u64,
    slide_seconds: u64,
    bitrate: u32,
    out: String,
    font: Option<String>,
    encoder: String,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        source: "test".into(),
        seconds: 60,
        slide_seconds: 4,
        bitrate: 6000,
        out: "recording.mkv".into(),
        font: None,
        encoder: "qsv".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--source" => a.source = val()?,
            "--seconds" => a.seconds = val()?.parse()?,
            "--slide-seconds" => a.slide_seconds = val()?.parse::<u64>()?.max(1),
            "--bitrate" => a.bitrate = val()?.parse()?,
            "--out" => a.out = val()?,
            "--font" => a.font = Some(val()?),
            "--encoder" => a.encoder = val()?,
            "-h" | "--help" => {
                println!("lyrics-stream [--source test|webcam] [--seconds N] [--slide-seconds N] [--bitrate KBPS] [--out FILE] [--font TTF] [--encoder qsv|mf|x264]");
                println!("Set JIVVY_YT_STREAM_KEY to also stream to YouTube.");
                std::process::exit(0);
            }
            other => bail!("unknown flag {other}"),
        }
    }
    if a.source != "test" && a.source != "webcam" {
        bail!("--source must be test or webcam");
    }
    if !["qsv", "mf", "x264"].contains(&a.encoder.as_str()) {
        bail!("--encoder must be qsv, mf or x264");
    }
    Ok(a)
}

/// Loads a TTF: the given path, else a common system font.
pub fn load_font(path: Option<&str>) -> Result<Vec<u8>> {
    let candidates: Vec<String> = match path {
        Some(p) => vec![p.to_string()],
        None => [
            r"C:\Windows\Fonts\segoeuib.ttf",
            r"C:\Windows\Fonts\arialbd.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        ]
        .map(String::from)
        .to_vec(),
    };
    for c in &candidates {
        if let Ok(b) = std::fs::read(c) {
            return Ok(b);
        }
    }
    bail!("no font found; pass --font path/to/font.ttf")
}

/// Never print a stream URL with its key.
pub fn redact(url: &str) -> String {
    match url.rfind('/') {
        Some(i) if i + 1 < url.len() => format!("{}/<redacted>", &url[..i]),
        _ => url.to_string(),
    }
}

fn pipeline_description(a: &Args, streaming: bool) -> String {
    let src = match a.source.as_str() {
        // Media Foundation already decodes the camera's MJPEG to NV12, which is what
        // the encoder takes, so no conversion is needed on the CPU.
        "webcam" => format!("mfvideosrc ! video/x-raw,format=NV12,width={WIDTH},height={HEIGHT},framerate={FPS}/1"),
        _ => format!("videotestsrc is-live=true pattern=ball ! video/x-raw,format=NV12,width={WIDTH},height={HEIGHT},framerate={FPS}/1"),
    };
    let (br, gop) = (a.bitrate, FPS * 2);
    let enc = match a.encoder.as_str() {
        // Media Foundation's Intel encoder takes system or D3D11 memory, not D3D12.
        "mf" => format!(
            "d3d12download ! video/x-raw,format=NV12 ! mfh264enc name=enc rc-mode=cbr bitrate={br} gop-size={gop} bframes=0 low-latency=true"
        ),
        // Software fallback for when no hardware encoder works. veryfast is the
        // usual live-streaming preset; vbv-buf-capacity (ms) keeps it near CBR.
        "x264" => format!(
            "d3d12download ! video/x-raw,format=NV12 ! x264enc name=enc bitrate={br} vbv-buf-capacity=1000 key-int-max={gop} bframes=0 speed-preset=veryfast tune=zerolatency"
        ),
        _ => format!("qsvh264enc name=enc bitrate={br} max-bitrate={br} rate-control=cbr gop-size={gop} b-frames=0 target-usage=7"),
    };
    let out = a.out.replace('\\', "/");
    let mut d = format!(
        // Frames go to GPU memory first. overlaycomposition then only attaches the
        // lyric layer as metadata and d3d12overlaycompositor blends it on the GPU,
        // so the CPU never touches a full frame (CPU blending into camera buffers
        // throttled the webcam to ~10 fps).
        "{src} ! d3d12upload ! overlaycomposition name=lyrics ! d3d12overlaycompositor \
         ! queue max-size-buffers=3 leaky=downstream ! {enc} \
         ! h264parse name=parse config-interval=-1 ! tee name=vt \
         audiotestsrc is-live=true wave=silence ! audio/x-raw,rate=48000,channels=2 ! audioconvert \
         ! avenc_aac bitrate=128000 ! aacparse ! tee name=at \
         matroskamux name=mkv streamable=true ! filesink location=\"{out}\" \
         vt. ! queue ! mkv. at. ! queue ! mkv.",
    );
    if streaming {
        d.push_str(" flvmux name=fmux streamable=true ! rtmp2sink name=yt vt. ! queue ! fmux. at. ! queue ! fmux.");
    }
    d
}

fn main() -> Result<()> {
    let args = parse_args()?;
    gst::init()?;

    let key = std::env::var("JIVVY_YT_STREAM_KEY").ok().filter(|k| !k.trim().is_empty());
    let pipeline = gst::parse::launch(&pipeline_description(&args, key.is_some()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
    if let Some(k) = &key {
        let url = format!("rtmps://a.rtmps.youtube.com:443/live2/{}", k.trim());
        pipeline.by_name("yt").unwrap().set_property("location", &url);
        println!("Streaming to {}", redact(&url));
    }

    // Lyric layer: re-rendered only when the slide index changes.
    let font_bytes: &'static [u8] = Box::leak(load_font(args.font.as_deref())?.into_boxed_slice());
    let font = ab_glyph::FontRef::try_from_slice(font_bytes)?;
    let cache: Arc<Mutex<(Option<usize>, Option<gst_video::VideoOverlayComposition>)>> = Arc::default();
    let render_us = Arc::new(AtomicU64::new(0));
    let renders = Arc::new(AtomicU64::new(0));
    let slide_ns = args.slide_seconds * 1_000_000_000;
    {
        let (cache, render_us, renders) = (cache.clone(), render_us.clone(), renders.clone());
        pipeline.by_name("lyrics").unwrap().connect("draw", false, move |vals| {
            let sample = vals[1].get::<gst::Sample>().ok()?;
            let pts = sample.buffer().and_then(|b| b.pts()).map(|t| t.nseconds()).unwrap_or(0);
            let idx = (pts / slide_ns) as usize % SLIDES.len();
            let mut c = cache.lock().unwrap();
            if c.0 != Some(idx) {
                let t = Instant::now();
                c.1 = lyrics::render(&font, SLIDES[idx], WIDTH, HEIGHT, &lyrics::Style::default()).and_then(to_composition);
                c.0 = Some(idx);
                render_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                renders.fetch_add(1, Ordering::Relaxed);
            }
            Some(c.1.clone().to_value())
        });
    }

    // Count encoded frames.
    let frames = Arc::new(AtomicU64::new(0));
    {
        let frames = frames.clone();
        pipeline.by_name("parse").unwrap().static_pad("src").unwrap().add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            frames.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
    }

    pipeline.set_state(gst::State::Playing)?;
    let bus = pipeline.bus().unwrap();
    let start = Instant::now();
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::get_current_pid().map_err(anyhow::Error::msg)?;
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
    let (mut cpu_samples, mut last_frames, mut last_tick, mut eos_sent) = (Vec::new(), 0u64, Instant::now(), false);
    let mut worst_fps = f64::MAX;

    println!("source={} encoder={} {WIDTH}x{HEIGHT}@{FPS} bitrate={}kbps seconds={} cores={cores}", args.source, args.encoder, args.bitrate, args.seconds);
    loop {
        if let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(200)) {
            match msg.view() {
                gst::MessageView::Eos(_) => break,
                gst::MessageView::Error(e) => {
                    let _ = pipeline.set_state(gst::State::Null);
                    // Error details can include the RTMP URL; scrub the key before printing.
                    let text = format!("{} ({:?})", e.error(), e.debug());
                    let text = match &key {
                        Some(k) => text.replace(k.trim(), "<redacted>"),
                        None => text,
                    };
                    bail!("pipeline error: {text}");
                }
                _ => {}
            }
        }
        if last_tick.elapsed() >= Duration::from_secs(1) {
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            let cpu = sys.process(pid).map(|p| p.cpu_usage() / cores).unwrap_or(0.0);
            let f = frames.load(Ordering::Relaxed);
            let fps = (f - last_frames) as f64 / last_tick.elapsed().as_secs_f64();
            if start.elapsed() > Duration::from_secs(3) {
                // Skip warm-up seconds.
                cpu_samples.push(cpu);
                worst_fps = worst_fps.min(fps);
            }
            println!("t={:>4}s cpu={cpu:>5.1}% fps={fps:>5.1}", start.elapsed().as_secs());
            last_frames = f;
            last_tick = Instant::now();
        }
        if !eos_sent && start.elapsed() >= Duration::from_secs(args.seconds) {
            pipeline.send_event(gst::event::Eos::new());
            eos_sent = true;
        }
    }
    pipeline.set_state(gst::State::Null)?;

    let total = frames.load(Ordering::Relaxed);
    let expected = args.seconds * FPS as u64;
    let avg = cpu_samples.iter().sum::<f32>() / cpu_samples.len().max(1) as f32;
    let max = cpu_samples.iter().cloned().fold(0.0, f32::max);
    let n = renders.load(Ordering::Relaxed).max(1);
    println!("\n== Summary ==");
    println!("frames encoded: {total} of ~{expected} expected");
    println!("worst 1s fps:   {:.1}", if worst_fps == f64::MAX { 0.0 } else { worst_fps });
    println!("cpu (whole machine %): avg {avg:.1}, max {max:.1}");
    println!("lyric renders:  {n}, avg {:.2} ms each", render_us.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0);
    println!("recording:      {}", args.out);
    Ok(())
}

fn to_composition(l: lyrics::Layer) -> Option<gst_video::VideoOverlayComposition> {
    let mut buf = gst::Buffer::from_mut_slice(l.bgra);
    gst_video::VideoMeta::add(buf.get_mut()?, gst_video::VideoFrameFlags::empty(), gst_video::VideoFormat::Bgra, l.width, l.height).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_hides_the_stream_key() {
        let r = redact("rtmps://a.rtmps.youtube.com:443/live2/abcd-efgh-ijkl");
        assert_eq!(r, "rtmps://a.rtmps.youtube.com:443/live2/<redacted>");
        assert!(!r.contains("abcd"));
    }

    #[test]
    fn pipeline_only_streams_when_a_key_is_set() {
        let a = Args { source: "test".into(), seconds: 1, slide_seconds: 4, bitrate: 6000, out: "x.mkv".into(), font: None, encoder: "qsv".into() };
        assert!(!pipeline_description(&a, false).contains("rtmp2sink"));
        let d = pipeline_description(&a, true);
        assert!(d.contains("rtmp2sink name=yt") && !d.contains("location=rtmp"));
    }

    #[test]
    fn lyrics_are_composited_on_the_gpu_for_every_encoder() {
        let mut a = Args { source: "webcam".into(), seconds: 1, slide_seconds: 4, bitrate: 6000, out: "x.mkv".into(), font: None, encoder: "qsv".into() };
        let qsv = pipeline_description(&a, false);
        assert!(qsv.contains("d3d12upload ! overlaycomposition name=lyrics ! d3d12overlaycompositor"));
        assert!(qsv.contains("qsvh264enc") && !qsv.contains("mfh264enc"));
        a.encoder = "mf".into();
        let mf = pipeline_description(&a, false);
        assert!(mf.contains("d3d12overlaycompositor") && mf.contains("d3d12download ! video/x-raw,format=NV12 ! mfh264enc"));
        a.encoder = "x264".into();
        let x264 = pipeline_description(&a, false);
        assert!(x264.contains("d3d12overlaycompositor") && x264.contains("d3d12download ! video/x-raw,format=NV12 ! x264enc"));
    }
}
