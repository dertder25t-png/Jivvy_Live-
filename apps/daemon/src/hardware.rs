//! The hardware check: which picture path and encoder this machine uses, and at what quality,
//! decided from measured trials (`jivvy-video --hardware-check`), never just from what is
//! installed. A PC without a working GPU still has the D3D12 and Media Foundation plugins,
//! which then run in software at a few frames a second. Pure logic, tested without a GPU.

use serde::{Deserialize, Serialize};

use crate::program::{EncoderChoice, PicturePath, ProgramConfig, ProgramSet, bitrate_for};

pub const REPORT_FILE: &str = "hardware.json";
pub const REPORT_VERSION: u32 = 1;
/// A setting must sustain this many times the frame rate flat out: room for the
/// lower-quality stream encodes, the camera, and whatever else the computer is doing.
pub const HEADROOM: f64 = 1.5;
/// Qualities tried, best first: width, height, frames per second.
pub const QUALITIES: [(u32, u32, u32); 3] = [(1920, 1080, 30), (1280, 720, 30), (854, 480, 30)];

/// Where the picture is converted and the lyric layer blended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Path {
    /// The graphics chip (D3D12 on Windows).
    Gpu,
    /// The computer's processor.
    Cpu,
}

/// One measured setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trial {
    pub path: Path,
    pub encoder: EncoderChoice,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Frames per second it sustained flat out; `None` if it failed or was skipped.
    pub capacity_fps: Option<f64>,
    /// Why it failed or was skipped; empty when it ran.
    pub detail: String,
}

impl Trial {
    pub fn keeps_up(&self) -> bool {
        self.capacity_fps.is_some_and(|c| c >= self.fps as f64 * HEADROOM)
    }
}

/// The setting the program should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chosen {
    pub path: Path,
    pub encoder: EncoderChoice,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// `hardware.json`, written by the check and shown on the System screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub version: u32,
    pub checked_at_ms: u64,
    pub os: String,
    pub cpus: usize,
    pub trials: Vec<Trial>,
    pub chosen: Option<Chosen>,
    /// The choice in plain words, for the tech lead.
    pub reason: String,
    /// What could change the answer (see `fingerprint`), so a new graphics card or driver, or a
    /// different computer, is noticed. Empty in reports written before it existed.
    #[serde(default)]
    pub fingerprint: String,
}

/// Virtual and remote display adapters (Parsec, Remote Desktop) come and go without any change
/// to the graphics hardware, so they're left out of the fingerprint.
fn is_real_adapter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    !(n.contains("virtual display") || n.contains("remote display"))
}

/// What the fingerprint is made of.
#[derive(Debug, Clone, Copy)]
pub struct Machine<'a> {
    pub os: &'a str,
    pub arch: &'a str,
    /// The processor model, e.g. "Intel(R) Core(TM) i5-8250U"; empty if unknown.
    pub cpu: &'a str,
    pub cores: usize,
    /// GStreamer's core version.
    pub gstreamer: &'a str,
    /// `element=plugin version` (or `element=missing`) for each element the check measures with.
    pub plugins: &'a [String],
    /// Graphics adapters with their driver versions.
    pub adapters: &'a [String],
}

/// Everything that could change which setting keeps up: the OS and architecture, the processor
/// model and core count, GStreamer's version and the version of each plugin the check measures
/// with (installing, removing or updating one changes what can run without changing GStreamer's
/// core), and the graphics adapters with their driver versions. Lists are compared in any
/// order. Equal fingerprints mean the old measurement still applies; a different one means
/// measure again.
pub fn fingerprint(m: &Machine) -> String {
    let sorted = |items: &[String], keep: fn(&str) -> bool| {
        let mut v: Vec<&str> = items.iter().map(|a| a.trim()).filter(|a| !a.is_empty() && keep(a)).collect();
        v.sort_unstable();
        v.join(" | ")
    };
    format!(
        "{}/{}; {} ({} cores); {}; plugins: {}; graphics: {}",
        m.os,
        m.arch,
        m.cpu.trim(),
        m.cores,
        m.gstreamer,
        sorted(m.plugins, |_| true),
        sorted(m.adapters, is_real_adapter)
    )
}

/// Whether the saved report no longer fits this machine (or predates fingerprints).
pub fn needs_recheck(report: &Report, current: &str) -> bool {
    report.fingerprint != current
}

/// The settings to try at each quality, in order of preference: the graphics chip before the
/// processor (it leaves the CPU free), hardware encoders before x264. Quick Sync is never
/// picked automatically (it crashes on some drivers; see `encoder_order`).
pub fn candidates(gpu_available: bool, encoders: &[EncoderChoice]) -> Vec<(Path, EncoderChoice)> {
    let encoders: Vec<EncoderChoice> =
        [EncoderChoice::Mf, EncoderChoice::X264].into_iter().filter(|e| encoders.contains(e)).collect();
    let paths: &[Path] = if gpu_available { &[Path::Gpu, Path::Cpu] } else { &[Path::Cpu] };
    paths.iter().flat_map(|&p| encoders.iter().map(move |&e| (p, e))).collect()
}

/// The first setting, in order of preference, that keeps up at the best quality any does.
pub fn choose(trials: &[Trial], order: &[(Path, EncoderChoice)]) -> Option<Chosen> {
    QUALITIES.iter().find_map(|&(width, height, fps)| {
        order.iter().find_map(|&(path, encoder)| {
            trials
                .iter()
                .any(|t| {
                    (t.path, t.encoder, t.width, t.height, t.fps) == (path, encoder, width, height, fps) && t.keeps_up()
                })
                .then_some(Chosen { path, encoder, width, height, fps })
        })
    })
}

/// Reads `hardware.json`; missing, unreadable or from another version means no check yet.
pub fn load(dir: &std::path::Path) -> Option<Report> {
    let r: Report = serde_json::from_slice(&std::fs::read(dir.join(REPORT_FILE)).ok()?).ok()?;
    (r.version == REPORT_VERSION).then_some(r)
}

/// The program settings to run with: whatever `media.json` sets itself (the tech lead's
/// override), and the hardware check's choice for the rest. Encoder and path follow the check
/// when they're `auto`; size and frame rate when `media.json` leaves them out, with the
/// bitrate scaled to the size unless that's set too. With no check yet, `media.json` as is.
pub fn resolve(cfg: &ProgramConfig, set: ProgramSet, chosen: Option<Chosen>) -> ProgramConfig {
    let Some(c) = chosen else { return cfg.clone() };
    let mut r = cfg.clone();
    if r.encoder == EncoderChoice::Auto {
        r.encoder = c.encoder;
    }
    if r.path == PicturePath::Auto {
        r.path = match c.path {
            Path::Gpu => PicturePath::Gpu,
            Path::Cpu => PicturePath::Cpu,
        };
    }
    if !set.size {
        (r.width, r.height) = (c.width, c.height);
    }
    if !set.fps {
        r.fps = c.fps;
    }
    if !set.bitrate {
        // For the size the program will really use: a typo in media.json is clamped first.
        let size = r.sanitized();
        r.bitrate_kbps = bitrate_for(size.width, size.height);
    }
    r
}

fn path_words(p: Path) -> &'static str {
    match p {
        Path::Gpu => "the graphics chip",
        Path::Cpu => "the computer's processor",
    }
}

fn encoder_words(e: EncoderChoice) -> &'static str {
    match e {
        EncoderChoice::Mf => "the Windows video encoder",
        EncoderChoice::Qsv => "Intel Quick Sync",
        EncoderChoice::X264 | EncoderChoice::Auto => "the software encoder (x264)",
    }
}

/// The choice in plain words: what was picked, how fast it ran, and what was passed over.
pub fn reason(trials: &[Trial], chosen: Option<Chosen>) -> String {
    let Some(c) = chosen else {
        let (w, h, f) = QUALITIES[QUALITIES.len() - 1];
        return format!(
            "Nothing this computer tried kept up, even at {w}x{h} {f} fps. It may be too slow for live video, \
             or busy with something else; close other programs and run the check again."
        );
    };
    let quality = |w: u32, h: u32| format!("{}p", h.min(w));
    let ran = trials
        .iter()
        .find(|t| (t.path, t.encoder, t.width, t.height, t.fps) == (c.path, c.encoder, c.width, c.height, c.fps))
        .and_then(|t| t.capacity_fps)
        .unwrap_or(0.0);
    let mut s = format!(
        "Using {} with {} at {}{} (it handled {:.0} frames a second; {:.0} are needed).",
        path_words(c.path),
        encoder_words(c.encoder),
        quality(c.width, c.height),
        c.fps,
        ran,
        c.fps as f64 * HEADROOM
    );
    if let Some(gpu) = trials
        .iter()
        .filter(|t| t.path == Path::Gpu && !t.keeps_up())
        .max_by(|a, b| a.capacity_fps.unwrap_or(0.0).total_cmp(&b.capacity_fps.unwrap_or(0.0)))
        && c.path == Path::Cpu
    {
        match gpu.capacity_fps {
            Some(f) => s.push_str(&format!(" The graphics chip managed only {f:.0} frames a second.")),
            None => s.push_str(&format!(" The graphics chip didn't work: {}.", gpu.detail)),
        }
    }
    let (best_w, best_h, _) = QUALITIES[0];
    if (c.width, c.height) != (best_w, best_h) {
        s.push_str(&format!(
            " Nothing kept up at {}, so the stream and recording are lowered to {}.",
            quality(best_w, best_h),
            quality(c.width, c.height)
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use EncoderChoice::{Mf, X264};

    fn trial(path: Path, encoder: EncoderChoice, q: usize, capacity: Option<f64>) -> Trial {
        let (width, height, fps) = QUALITIES[q];
        Trial { path, encoder, width, height, fps, capacity_fps: capacity, detail: String::new() }
    }

    #[test]
    fn the_graphics_chip_and_hardware_encoders_are_preferred_and_quick_sync_never_picked() {
        assert_eq!(
            candidates(true, &[Mf, EncoderChoice::Qsv, X264]),
            [(Path::Gpu, Mf), (Path::Gpu, X264), (Path::Cpu, Mf), (Path::Cpu, X264)]
        );
        assert_eq!(candidates(false, &[X264]), [(Path::Cpu, X264)]);
    }

    #[test]
    fn a_fast_gpu_is_used_at_full_quality() {
        let order = candidates(true, &[Mf, X264]);
        let trials = [trial(Path::Gpu, Mf, 0, Some(240.0)), trial(Path::Cpu, X264, 0, Some(70.0))];
        let c = choose(&trials, &order).unwrap();
        assert_eq!((c.path, c.encoder, c.height), (Path::Gpu, Mf, 1080));
        assert!(
            reason(&trials, Some(c)).starts_with("Using the graphics chip with the Windows video encoder at 1080p30")
        );
    }

    #[test]
    fn a_gpu_that_is_installed_but_too_slow_loses_to_the_processor() {
        // What GitHub's Windows runners do: D3D12 and Media Foundation in software, ~7 fps.
        let order = candidates(true, &[Mf, X264]);
        let trials = [
            trial(Path::Gpu, Mf, 0, Some(7.0)),
            trial(Path::Gpu, X264, 0, Some(7.7)),
            trial(Path::Cpu, Mf, 0, Some(6.0)),
            trial(Path::Cpu, X264, 0, Some(55.0)),
        ];
        let c = choose(&trials, &order).unwrap();
        assert_eq!((c.path, c.encoder, c.height), (Path::Cpu, X264, 1080));
        let r = reason(&trials, Some(c));
        assert!(r.contains("the computer's processor") && r.contains("managed only 8 frames a second"), "{r}");
    }

    #[test]
    fn just_keeping_up_is_not_enough_headroom() {
        let order = candidates(false, &[X264]);
        let trials = [trial(Path::Cpu, X264, 0, Some(40.0)), trial(Path::Cpu, X264, 1, Some(90.0))];
        let c = choose(&trials, &order).unwrap();
        assert_eq!(c.height, 720, "40 fps is under the 45 needed at 1080p30");
        assert!(reason(&trials, Some(c)).contains("lowered to 720p"));
    }

    #[test]
    fn nothing_keeping_up_is_said_plainly() {
        let order = candidates(false, &[X264]);
        let trials: Vec<Trial> = (0..QUALITIES.len()).map(|q| trial(Path::Cpu, X264, q, Some(10.0))).collect();
        assert_eq!(choose(&trials, &order), None);
        assert!(reason(&trials, None).starts_with("Nothing this computer tried kept up"));
    }

    #[test]
    fn a_failed_gpu_trial_is_explained() {
        let order = candidates(true, &[X264]);
        let mut gpu = trial(Path::Gpu, X264, 0, None);
        gpu.detail = "no D3D12 device".into();
        let trials = [gpu, trial(Path::Cpu, X264, 0, Some(60.0))];
        let c = choose(&trials, &order);
        assert!(reason(&trials, c).contains("didn't work: no D3D12 device"));
    }

    #[test]
    fn the_check_fills_in_what_media_json_leaves_out_and_the_tech_lead_wins() {
        let chosen = Some(Chosen { path: Path::Cpu, encoder: X264, width: 1280, height: 720, fps: 30 });
        let defaults = ProgramConfig::default();

        let r = resolve(&defaults, ProgramSet::default(), chosen);
        assert_eq!((r.path, r.encoder, r.width, r.height, r.fps), (PicturePath::Cpu, X264, 1280, 720, 30));
        assert_eq!(r.bitrate_kbps, 2666, "the 1080p bitrate scaled to 720p");

        // The tech lead asked for 1080p on the graphics chip with Media Foundation: kept.
        let mine =
            ProgramConfig { path: PicturePath::Gpu, encoder: Mf, bitrate_kbps: 5000, ..ProgramConfig::default() };
        let set = ProgramSet { size: true, fps: false, bitrate: true };
        let r = resolve(&mine, set, chosen);
        assert_eq!((r.path, r.encoder, r.width, r.height, r.bitrate_kbps), (PicturePath::Gpu, Mf, 1920, 1080, 5000));

        // No check yet: media.json as it is.
        assert_eq!(resolve(&defaults, ProgramSet::default(), None), defaults);

        // A typo'd size with no bitrate: no overflow, and the bitrate is for the clamped size.
        let typo = ProgramConfig { width: 100_000, height: 100_000, ..ProgramConfig::default() };
        let set = ProgramSet { size: true, fps: false, bitrate: false };
        let r = resolve(&typo, set, chosen);
        assert_eq!(r.bitrate_kbps, bitrate_for(3840, 2160), "{r:?}");
    }

    #[test]
    fn a_new_driver_card_or_computer_means_measure_again() {
        let v = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let plugins = v(&["x264enc=1.28.6", "mfh264enc=1.28.6", "d3d12upload=1.28.6"]);
        let adapters = v(&["Intel UHD 32.0.101.8860", "AMD RX 6700 XT 32.0.21045"]);
        let machine =
            |cpu: &'static str, cores, gst: &'static str, plugins: &'static [String], adapters: &'static [String]| {
                fingerprint(&Machine { os: "windows", arch: "x86_64", cpu, cores, gstreamer: gst, plugins, adapters })
            };
        let (plugins, adapters): (&'static [String], &'static [String]) =
            (Box::leak(plugins.into_boxed_slice()), Box::leak(adapters.into_boxed_slice()));
        let base = machine("Intel(R) Core(TM) i5-8250U", 8, "1.28.6", plugins, adapters);

        // The same machine: lists in another order, stray spaces, a virtual display (Parsec,
        // Remote Desktop) appearing or not: none of that is a hardware change.
        let reordered = v(&["d3d12upload=1.28.6", "x264enc=1.28.6 ", "mfh264enc=1.28.6"]);
        let adapters2 = v(&[
            " AMD RX 6700 XT 32.0.21045",
            "Parsec Virtual Display Adapter 0.45.0.0",
            "Intel UHD 32.0.101.8860 ",
            "",
        ]);
        assert_eq!(
            base,
            machine(
                "Intel(R) Core(TM) i5-8250U ",
                8,
                "1.28.6",
                Box::leak(reordered.into_boxed_slice()),
                Box::leak(adapters2.into_boxed_slice())
            )
        );

        let leak = |items: &[&str]| -> &'static [String] { Box::leak(v(items).into_boxed_slice()) };
        for (what, changed) in [
            (
                "another processor with the same core count",
                machine("Intel(R) Core(TM) i3-7100U", 8, "1.28.6", plugins, adapters),
            ),
            ("a different number of cores", machine("Intel(R) Core(TM) i5-8250U", 4, "1.28.6", plugins, adapters)),
            ("GStreamer updated", machine("Intel(R) Core(TM) i5-8250U", 8, "1.29.0", plugins, adapters)),
            (
                "a plugin updated on its own",
                machine(
                    "Intel(R) Core(TM) i5-8250U",
                    8,
                    "1.28.6",
                    leak(&["x264enc=1.28.7", "mfh264enc=1.28.6", "d3d12upload=1.28.6"]),
                    adapters,
                ),
            ),
            (
                "a plugin removed",
                machine(
                    "Intel(R) Core(TM) i5-8250U",
                    8,
                    "1.28.6",
                    leak(&["x264enc=1.28.6", "mfh264enc=missing", "d3d12upload=1.28.6"]),
                    adapters,
                ),
            ),
            (
                "a graphics driver updated",
                machine(
                    "Intel(R) Core(TM) i5-8250U",
                    8,
                    "1.28.6",
                    plugins,
                    leak(&["Intel UHD 32.0.101.9999", "AMD RX 6700 XT 32.0.21045"]),
                ),
            ),
            (
                "a graphics card removed",
                machine("Intel(R) Core(TM) i5-8250U", 8, "1.28.6", plugins, leak(&["Intel UHD 32.0.101.8860"])),
            ),
        ] {
            assert_ne!(base, changed, "{what} must count as a change");
        }
        let mut r = Report {
            version: REPORT_VERSION,
            checked_at_ms: 1,
            os: "windows".into(),
            cpus: 4,
            trials: vec![],
            chosen: None,
            reason: String::new(),
            fingerprint: base.clone(),
        };
        assert!(!needs_recheck(&r, &base));
        assert!(needs_recheck(&r, "another machine"));
        r.fingerprint.clear(); // a report from before fingerprints: measured once more, then it has one
        assert!(needs_recheck(&r, &base));
        let old = r#"{"version":1,"checkedAtMs":1,"os":"windows","cpus":4,"trials":[],"chosen":null,"reason":""}"#;
        assert_eq!(serde_json::from_str::<Report>(old).unwrap().fingerprint, "");
    }

    #[test]
    fn the_report_round_trips_as_json() {
        let trials = vec![trial(Path::Cpu, X264, 0, Some(60.0))];
        let chosen = choose(&trials, &candidates(false, &[X264]));
        let r = Report {
            version: REPORT_VERSION,
            checked_at_ms: 1,
            os: "windows".into(),
            cpus: 4,
            reason: reason(&trials, chosen),
            trials,
            chosen,
            fingerprint: "test".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"capacityFps\":60.0") && json.contains("\"path\":\"cpu\""), "{json}");
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), r);
    }
}
