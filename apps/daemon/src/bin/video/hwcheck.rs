//! `jivvy-video --hardware-check`: measures how fast each picture path and encoder runs on
//! this machine and writes `hardware.json` with the setting to use (see
//! `jivvy_daemon::hardware`). Each trial runs the program's conversion, lyric-layer and encode
//! chain flat out on a moving test picture; nothing is assumed from what is installed.

use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;

use jivvy_daemon::hardware::{self, Path, QUALITIES, Report, Trial};
use jivvy_daemon::lyrics;
use jivvy_daemon::program::{EncoderChoice, PicturePath, ProgramConfig, bitrate_for, encoder_order};
use jivvy_daemon::{log, now_ms, write_atomic};

use crate::program::{encoder_description, to_composition};

/// Frames per trial: five seconds of video at the target rate.
const SECONDS_PER_TRIAL: u32 = 5;
/// A trial that hasn't finished by now has failed (a hung driver, say).
const TRIAL_TIMEOUT: Duration = Duration::from_secs(30);
/// A two-line lyric on every frame, as on Sunday, so the blending costs what it really does.
const LYRICS: [&str; 2] = ["Amazing grace, how sweet the sound", "That saved a wretch like me"];

/// Encoded frames seen at the end of a trial: the first and last arrival, and how many.
#[derive(Default, Clone, Copy)]
struct Seen {
    first: Option<Instant>,
    last: Option<Instant>,
    count: u64,
}

/// The graphics adapters and their driver versions, one line each; empty if they can't be listed
/// (then the fingerprint just doesn't notice a driver change, and the rest still applies).
fn adapters() -> Vec<String> {
    #[cfg(windows)]
    let lines = tool_output(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name + ' ' + $_.DriverVersion }",
        ],
    );
    #[cfg(target_os = "macos")]
    let lines = tool_output("system_profiler", &["SPDisplaysDataType"])
        .into_iter()
        .filter(|l| l.trim_start().starts_with("Chipset Model:") || l.trim_start().starts_with("Metal"))
        .collect();
    #[cfg(all(unix, not(target_os = "macos")))]
    let lines = {
        let mut lines = Vec::new();
        if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
            for card in cards.flatten() {
                let dir = card.path().join("device");
                let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default().trim().to_string();
                let (vendor, device) = (read("vendor"), read("device"));
                if !vendor.is_empty() {
                    lines.push(format!("{} {vendor}:{device}", card.file_name().to_string_lossy()));
                }
            }
        }
        // The graphics driver ships with the kernel.
        lines.push(std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default());
        lines
    };
    lines
}

/// Runs a tool and returns its output lines; nothing if it can't run or takes over 10 s.
#[cfg(any(windows, target_os = "macos"))]
fn tool_output(program: &str, args: &[&str]) -> Vec<String> {
    let Ok(mut child) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let Some(mut out) = child.stdout.take() else { return Vec::new() };
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return reader.join().unwrap_or_default().lines().map(str::to_string).collect();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    Vec::new()
}

/// What could change the answer on this machine right now (`hardware::fingerprint`).
pub fn fingerprint_now() -> String {
    hardware::fingerprint(
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        &gst::version_string(),
        &adapters(),
    )
}

fn has(element: &str) -> bool {
    gst::ElementFactory::find(element).is_some()
}

/// Runs the check and writes the report; the error says why the report couldn't be saved.
/// The test hooks narrow what is tried, so a machine with a flaky GPU can still be checked
/// safely: `JIVVY_TEST_NO_GPU=1` skips the graphics chip, `JIVVY_TEST_ENCODER` tries only that
/// encoder.
pub fn run(data_dir: &FsPath, only_encoder: Option<EncoderChoice>, font: &[u8]) -> (Report, Result<(), String>) {
    let no_gpu = std::env::var("JIVVY_TEST_NO_GPU").is_ok_and(|v| v.trim() == "1");
    let gpu = !no_gpu && has("d3d12upload") && has("d3d12overlaycompositor") && has("d3d12download");
    let mut encoders = encoder_order(EncoderChoice::Auto, has);
    if let Some(e) = only_encoder {
        encoders.retain(|&x| x == e);
    }
    let order = hardware::candidates(gpu, &encoders);
    let mut trials = Vec::new();
    for &(width, height, fps) in &QUALITIES {
        for &(path, encoder) in &order {
            let t = measure(path, encoder, width, height, fps, font);
            log(
                "video",
                format!(
                    "hardware check: {path:?} {} {height}p{fps}: {}",
                    encoder.name(),
                    t.capacity_fps.map(|c| format!("{c:.1} fps")).unwrap_or_else(|| t.detail.clone())
                ),
            );
            trials.push(t);
        }
        if hardware::choose(&trials, &order).is_some() {
            break; // the best quality that keeps up is found; lower ones aren't needed
        }
    }
    let chosen = hardware::choose(&trials, &order);
    // Taken after the trials: it describes the machine they ran on.
    let fingerprint = fingerprint_now();
    let report = Report {
        version: hardware::REPORT_VERSION,
        checked_at_ms: now_ms(),
        os: std::env::consts::OS.into(),
        cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        reason: hardware::reason(&trials, chosen),
        trials,
        chosen,
        fingerprint,
    };
    log("video", format!("hardware check: {}", report.reason));
    let saved = serde_json::to_vec_pretty(&report)
        .map_err(|e| e.to_string())
        .and_then(|bytes| write_atomic(&data_dir.join(hardware::REPORT_FILE), &bytes).map_err(|e| e.to_string()))
        .map_err(|e| format!("can't save {}: {e}", hardware::REPORT_FILE));
    (report, saved)
}

/// Pushes a moving test picture through the path's conversion and lyric-layer blending and the
/// encoder as fast as they go, and counts what comes out.
fn measure(path: Path, encoder: EncoderChoice, width: u32, height: u32, fps: u32, font: &[u8]) -> Trial {
    let mut trial = Trial { path, encoder, width, height, fps, capacity_fps: None, detail: String::new() };
    let gpu = path == Path::Gpu;
    let cfg = ProgramConfig {
        width,
        height,
        fps,
        bitrate_kbps: bitrate_for(width, height), // what the program would use at this size
        encoder,
        path: if gpu { PicturePath::Gpu } else { PicturePath::Cpu },
    };
    let overlay = if gpu {
        "d3d12upload ! overlaycomposition name=lyrics ! d3d12overlaycompositor"
    } else {
        "overlaycomposition name=lyrics"
    };
    let frames = fps * SECONDS_PER_TRIAL;
    let desc = format!(
        "videotestsrc num-buffers={frames} is-live=false pattern=ball \
         ! video/x-raw,format=NV12,width={width},height={height},framerate={fps}/1 \
         ! {overlay} ! queue max-size-buffers=3 ! {enc} ! fakesink name=sink sync=false async=false",
        enc = encoder_description(encoder, &cfg, gpu),
    );
    let pipeline = match gst::parse::launch(&desc).map(|e| e.downcast::<gst::Pipeline>()) {
        Ok(Ok(p)) => p,
        Ok(Err(_)) => {
            trial.detail = "not a pipeline".into();
            return trial;
        }
        Err(e) => {
            trial.detail = format!("can't build it: {e}");
            return trial;
        }
    };
    // The lyric layer, drawn once and attached to every frame, as the program does.
    let layer = ab_glyph::FontRef::try_from_slice(font)
        .ok()
        .and_then(|face| lyrics::render(&face, &LYRICS, width, height, &lyrics::Style::default()))
        .and_then(to_composition);
    let Some(layer) = layer else {
        trial.detail = "couldn't draw the lyric layer".into();
        return trial;
    };
    if let Some(el) = pipeline.by_name("lyrics") {
        el.connect("draw", false, move |_| Some(Some(layer.clone()).to_value()));
    }
    // First and last encoded frame, and how many: the rate excludes start-up (loading the
    // encoder, opening the GPU device).
    let seen: Arc<Mutex<Seen>> = Arc::default();
    if let Some(pad) = pipeline.by_name("sink").and_then(|s| s.static_pad("sink")) {
        let seen = seen.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            let mut s = seen.lock().unwrap();
            let now = Instant::now();
            s.first.get_or_insert(now);
            s.last = Some(now);
            s.count += 1;
            gst::PadProbeReturn::Ok
        });
    }
    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        trial.detail = format!("won't start: {e}");
        let _ = pipeline.set_state(gst::State::Null);
        return trial;
    }
    let bus = pipeline.bus().expect("a pipeline has a bus");
    let ended = bus.timed_pop_filtered(
        gst::ClockTime::from_mseconds(TRIAL_TIMEOUT.as_millis() as u64),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    let _ = pipeline.set_state(gst::State::Null);
    match ended.as_ref().map(|m| m.view()) {
        Some(gst::MessageView::Eos(_)) => {}
        Some(gst::MessageView::Error(e)) => {
            trial.detail = format!("failed: {}", e.error());
            return trial;
        }
        _ => {
            trial.detail = format!("didn't finish in {} s", TRIAL_TIMEOUT.as_secs());
            return trial;
        }
    }
    let Seen { first, last, count } = *seen.lock().unwrap();
    match (first, last) {
        (Some(a), Some(b)) if count >= 10 && b > a => {
            trial.capacity_fps = Some((count - 1) as f64 / b.duration_since(a).as_secs_f64());
        }
        _ => trial.detail = format!("only {count} frames came out"),
    }
    trial
}
