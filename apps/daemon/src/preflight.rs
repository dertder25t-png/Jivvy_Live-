//! The pre-flight streaming summary: measure the upload for a few seconds, then say what
//! each platform will get at that speed, e.g. "YouTube 1080p + Facebook 720p".
//!
//! The summary comes from the bandwidth manager's own allocation with the church's settings
//! (`stream.json`), so it promises exactly what streaming will do. The upload test sends to
//! our endpoint (`live.jivvy.org/api/upload-test`), which only counts the bytes; if it can't
//! be reached, the last speed measured is used and the summary says so. It never runs while
//! streaming, and nothing during a service depends on it.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bandwidth::{self, Tier};
use crate::protocol::{Preflight, PreflightCheck, PreflightDestination, preflight_summary};
use crate::{hardware, media, stream};

pub const DEFAULT_URL: &str = "https://live.jivvy.org/api/upload-test";
/// How long the test sends for.
pub const SEND_FOR: Duration = Duration::from_secs(5);
/// Most it sends (the endpoint refuses more): about 5 s at 50 Mbps.
pub const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// The last speed measured, for when the test can't run.
pub const SAVED_FILE: &str = "upload-speed.json";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    pub kbps: f64,
    pub measured_at: u64,
}

/// Zeros for `SEND_FOR`, up to `MAX_BYTES`, or until cancelled (the stream started).
struct Timed {
    start: Instant,
    sent: u64,
    cancel: Arc<AtomicBool>,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("the stream started"));
        }
        if self.start.elapsed() >= SEND_FOR || self.sent >= MAX_BYTES {
            return Ok(0);
        }
        let n = buf.len().min(64 * 1024).min((MAX_BYTES - self.sent) as usize);
        buf[..n].fill(0);
        self.sent += n as u64;
        Ok(n)
    }
}

/// Sends for `SEND_FOR` and times it until the endpoint says how much arrived. kbps.
pub fn measure(url: &str, cancel: Arc<AtomicBool>) -> Result<f64, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(SEND_FOR + Duration::from_secs(15)))
        .user_agent(format!("Jivvy / Jivvy Live / {}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let start = Instant::now();
    let mut body = Timed { start, sent: 0, cancel };
    let resp = agent
        .post(url)
        .header("Content-Type", "application/octet-stream")
        .send(ureq::SendBody::from_reader(&mut body))
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let answer = resp.into_body().with_config().limit(64 * 1024).read_to_vec().map_err(|e| e.to_string())?;
    let elapsed = start.elapsed().as_secs_f64();
    let bytes = serde_json::from_slice::<serde_json::Value>(&answer)
        .ok()
        .and_then(|v| v["bytes"].as_u64())
        .filter(|_| status == 200)
        .ok_or_else(|| format!("the test server answered {status}"))?;
    if bytes == 0 || elapsed <= 0.0 {
        return Err("nothing arrived".into());
    }
    Ok(bytes as f64 * 8.0 / elapsed / 1000.0)
}

pub fn load_saved(dir: &Path) -> Option<Measurement> {
    serde_json::from_slice(&std::fs::read(dir.join(SAVED_FILE)).ok()?).ok()
}

fn save(dir: &Path, m: Measurement) {
    if let Ok(bytes) = serde_json::to_vec_pretty(&m)
        && let Err(e) = crate::write_atomic(&dir.join(SAVED_FILE), &bytes)
    {
        crate::log("engine", format!("could not save {SAVED_FILE}: {e}"));
    }
}

/// What each enabled platform would get at `kbps`, by the bandwidth manager's own rules and
/// the church's settings.
pub fn plan(dir: &Path, kbps: f64) -> Vec<PreflightDestination> {
    let config = stream::load_config(dir).unwrap_or_default();
    let (prefs, _) = stream::prefs(&config);
    let program = match media::load_config(dir) {
        Ok(m) => {
            let chosen = hardware::load(dir).and_then(|r| r.chosen);
            hardware::resolve(&m.program, m.program_set, chosen).sanitized()
        }
        Err(_) => Default::default(),
    };
    let ladder = bandwidth::ladder(&program);
    let audio = bandwidth::audio_quality(&ladder);
    let mut order: Vec<&stream::Destination> = Vec::new();
    for d in config.destinations.iter().filter(|d| d.enabled && stream::validate(d).is_ok()) {
        if !order.iter().any(|o| o.id == d.id) {
            order.push(d);
        }
    }
    let ids: Vec<String> = order.iter().map(|d| d.id.clone()).collect();
    let assigned = bandwidth::allocate_with(&ladder, &ids, &prefs, Some(kbps));
    order
        .iter()
        .map(|d| {
            let tier = assigned.get(&d.id).copied().flatten();
            let quality = match tier {
                None => String::new(),
                Some(Tier::Audio) => audio.map(|q| q.label()).unwrap_or_default(),
                Some(t) => ladder.iter().find(|q| q.tier == t).map(|q| q.label()).unwrap_or_default(),
            };
            PreflightDestination { id: d.id.clone(), name: d.name.clone(), quality }
        })
        .collect()
}

fn check(id: &str, level: &str, label: &str, message: impl Into<String>, fix: Option<&str>) -> PreflightCheck {
    PreflightCheck {
        id: id.into(),
        level: level.into(),
        label: label.into(),
        message: message.into(),
        fix: fix.map(Into::into),
    }
}

/// What the engine knows right now, so a status file left behind by a process that has since
/// stopped is never taken for the truth.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Live {
    /// `jivvy-video` is installed (builds with the `video` feature).
    pub video_installed: bool,
    /// Its reports are arriving (the engine's own freshness of the stream and recording reports).
    pub video_alive: bool,
}

impl Live {
    /// Whether `jivvy-video` sits next to the running program, as the watchdog looks for it.
    pub fn video_installed_here() -> bool {
        let name = format!("jivvy-video{}", std::env::consts::EXE_SUFFIX);
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name))).is_some_and(|p| p.exists())
    }
}

/// Below this much free space recording the service is at risk (red); the warn level is amber.
pub const DISK_FLOOR_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// `outputs-status.json` is rewritten at least this often while `jivvy-outputs` runs
/// (`outputs::STATUS_REFRESH`); older than this, it was left behind.
pub const OUTPUTS_STATUS_STALE: Duration = Duration::from_secs(15);

/// A status file another process writes (`media-status.json`, `outputs-status.json`), with its age.
fn status_file(dir: &Path, name: &str) -> Option<(serde_json::Value, Duration)> {
    let path = dir.join(name);
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?.elapsed().unwrap_or_default();
    Some((serde_json::from_slice(&std::fs::read(path).ok()?).ok()?, age))
}

const RESTART: Option<&str> = Some("Restart Jivvy Live on the church computer.");

/// The church computer's other checks, from the settings, the status files its processes write
/// and what the engine knows live. Nothing here needs the internet, and no stream key is shown.
pub fn checks(dir: &Path, live: Live) -> Vec<PreflightCheck> {
    let mut out = Vec::new();

    // The camera and microphone status file counts only while the video process reports.
    let media = if live.video_alive { status_file(dir, media::STATUS_FILE).map(|(m, _)| m) } else { None };

    // Disk space where recordings go: amber below the warn level, red below the floor. With
    // unreadable settings, the recorder's own folder (it keeps the last good settings), else say so.
    let (folder, warn_below) = match crate::recording::load_config(dir) {
        Ok(rec) => (Some(rec.folder(dir)), rec.warn_free_bytes()),
        Err(e) => {
            let current = media.as_ref().and_then(|m| m["recording"]["folder"].as_str()).filter(|f| !f.is_empty());
            if current.is_none() {
                out.push(check(
                    "disk",
                    "warn",
                    "Disk space",
                    format!("The recording settings can't be read ({e}), so the free space wasn't checked."),
                    Some("Set the recording folder again in the settings."),
                ));
            }
            (current.map(std::path::PathBuf::from), crate::recording::WARN_FREE_BYTES)
        }
    };
    let floor = DISK_FLOOR_BYTES.min(warn_below);
    let gb = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    let space_fix = Some("Delete or move old recordings, or choose a recording folder on a bigger disk.");
    if let Some(folder) = folder {
        out.push(match crate::recording::free_space(&folder) {
            Some(free) if free < floor => check(
                "disk",
                "bad",
                "Disk space",
                format!("Only {:.1} GB free where recordings go: not enough to record the service.", gb(free)),
                space_fix,
            ),
            Some(free) if free < warn_below => check(
                "disk",
                "warn",
                "Disk space",
                format!("{:.1} GB free where recordings go: enough for a short service only.", gb(free)),
                space_fix,
            ),
            Some(free) => check("disk", "ok", "Disk space", format!("{:.0} GB free for recordings.", gb(free)), None),
            None => check(
                "disk",
                "warn",
                "Disk space",
                "Couldn't read the free space where recordings go.",
                Some("Check that the recording folder's disk is connected."),
            ),
        })
    }

    // Streaming settings (never the keys).
    out.push(match stream::load_config(dir) {
        Err(e) => check(
            "stream",
            "bad",
            "Streaming settings",
            format!("The streaming settings can't be used: {e}."),
            Some("Set up the streaming platforms again in the settings."),
        ),
        Ok(c) => {
            let on: Vec<&stream::Destination> = c.destinations.iter().filter(|d| d.enabled).collect();
            let broken: Vec<String> = on.iter().filter_map(|d| stream::validate(d).err()).collect();
            if !broken.is_empty() {
                check(
                    "stream",
                    "bad",
                    "Streaming settings",
                    broken.join("; "),
                    Some("Paste the server address and stream key again from the platform's live dashboard."),
                )
            } else if on.is_empty() {
                check(
                    "stream",
                    "warn",
                    "Streaming settings",
                    "No streaming platforms are set up.",
                    Some("Add YouTube or Facebook in the streaming settings, if this service streams."),
                )
            } else {
                let names: Vec<&str> =
                    on.iter().map(|d| if d.name.is_empty() { &d.id } else { &d.name }).map(String::as_str).collect();
                check("stream", "ok", "Streaming settings", format!("Set up: {}.", names.join(", ")), None)
            }
        }
    });

    // The hardware check: what video this computer can encode. Only with video installed.
    let not_here = "Not part of this setup: camera and streaming aren't installed on this computer.";
    out.push(match hardware::load(dir) {
        _ if !live.video_installed => check("encoder", "ok", "Video check", not_here, None),
        None => check(
            "encoder",
            "warn",
            "Video check",
            "This computer's video check hasn't run yet.",
            Some("Start Jivvy Live with the camera on once; it tests what this computer can stream."),
        ),
        Some(r) if r.chosen.is_none() => check(
            "encoder",
            "bad",
            "Video check",
            r.reason,
            Some("Close other programs and run the video check again, or use a more powerful computer."),
        ),
        Some(r) => check("encoder", "ok", "Video check", r.reason, None),
    });

    // The screens (fullscreen output windows).
    out.push(match status_file(dir, crate::outputs::STATUS_FILE) {
        None => check("screens", "warn", "Screens", "The screen windows aren't running.", RESTART),
        Some((_, age)) if age > OUTPUTS_STATUS_STALE => {
            check("screens", "warn", "Screens", "The screen windows stopped answering.", RESTART)
        }
        Some((s, _)) => {
            let problems: Vec<&str> =
                s["problems"].as_array().into_iter().flatten().filter_map(|p| p["message"].as_str()).collect();
            let shown = s["outputs"].as_array().map_or(0, Vec::len);
            let cables = Some("Check the screen cables and the display settings.");
            if !problems.is_empty() {
                check("screens", "warn", "Screens", problems.join(" "), cables)
            } else if s["connected"] != true {
                check("screens", "warn", "Screens", "The screens aren't following the slides.", RESTART)
            } else if shown == 0 {
                check("screens", "warn", "Screens", "No screen is showing the slides.", cables)
            } else {
                let what = if shown == 1 { "1 screen".to_string() } else { format!("{shown} screens") };
                check("screens", "ok", "Screens", format!("{what} showing the slides."), None)
            }
        }
    });

    // Camera and microphone.
    for (id, label) in [("camera", "Camera"), ("microphone", "Microphone")] {
        if !live.video_installed {
            out.push(check(id, "ok", label, not_here, None));
            continue;
        }
        let Some(m) = &media else {
            out.push(check(id, "warn", label, "The camera and microphone aren't running.", RESTART));
            continue;
        };
        let input = &m[id];
        out.push(match input["state"].as_str().unwrap_or("") {
            "running" => check(id, "ok", label, "Working.", None),
            "off" => check(id, "ok", label, "Not part of this setup: turned off in the settings.", None),
            "starting" => {
                check(id, "warn", label, "Still starting.", Some("Wait a few seconds and run the check again."))
            }
            _ => {
                let detail = input["detail"].as_str().filter(|d| !d.is_empty()).unwrap_or("It isn't working.");
                check(id, "bad", label, detail, Some("Check that it is plugged in and not in use by another program."))
            }
        });
    }

    out.push(restart_row(pending_restart()));
    out
}

/// Which signs of a pending OS restart were found.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PendingRestart {
    pub windows_update: bool,
    pub servicing: bool,
    pub file_renames: bool,
}

/// The "Pending restart" row from what was found; `None` means it couldn't be read, or this
/// isn't Windows. Then the row says so in green, like the "Not part of this setup" rows.
fn restart_row(found: Option<PendingRestart>) -> PreflightCheck {
    let label = "Pending restart";
    match found {
        None => check("restart", "ok", label, "Not checked on this computer.", None),
        Some(f) if f == PendingRestart::default() => check("restart", "ok", label, "No restart is waiting.", None),
        Some(f) => {
            let mut why = Vec::new();
            if f.windows_update {
                why.push("Windows Update");
            }
            if f.servicing {
                why.push("a Windows component");
            }
            if f.file_renames {
                why.push("an installed program");
            }
            check(
                "restart",
                "warn",
                label,
                format!("The computer is waiting to restart ({}).", why.join(", ")),
                Some("Restart the computer before the service, not during it."),
            )
        }
    }
}

/// Reads the Windows registry for a pending restart. Quick: three lookups, no shell.
#[cfg(windows)]
fn pending_restart() -> Option<PendingRestart> {
    type Hkey = isize;
    const HKLM: Hkey = 0x8000_0002u32 as i32 as isize;
    const KEY_READ: u32 = 0x20019;
    const KEY_WOW64_64KEY: u32 = 0x0100;
    const NOT_FOUND: i32 = 2;
    const MORE_DATA: i32 = 234;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(key: Hkey, sub: *const u16, options: u32, access: u32, out: *mut Hkey) -> i32;
        fn RegQueryValueExW(
            key: Hkey,
            name: *const u16,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            len: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    // Some(true) found, Some(false) absent, None unreadable.
    let exists = |sub: &str, value: Option<&str>| -> Option<bool> {
        let mut key: Hkey = 0;
        // SAFETY: valid NUL-terminated UTF-16 strings and out-pointers for the calls' duration.
        unsafe {
            match RegOpenKeyExW(HKLM, wide(sub).as_ptr(), 0, KEY_READ | KEY_WOW64_64KEY, &mut key) {
                0 => {}
                NOT_FOUND => return Some(false),
                _ => return None,
            }
            let found = match value {
                None => Some(true),
                Some(v) => {
                    let mut len = 0u32;
                    match RegQueryValueExW(
                        key,
                        wide(v).as_ptr(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut len,
                    ) {
                        // An empty multi-string (just terminators) isn't a pending rename.
                        0 | MORE_DATA => Some(len > 4),
                        NOT_FOUND => Some(false),
                        _ => None,
                    }
                }
            };
            RegCloseKey(key);
            found
        }
    };
    Some(PendingRestart {
        windows_update: exists(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired",
            None,
        )?,
        servicing: exists(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending", None)?,
        file_renames: exists(r"SYSTEM\CurrentControlSet\Control\Session Manager", Some("PendingFileRenameOperations"))?,
    })
}

#[cfg(not(windows))]
fn pending_restart() -> Option<PendingRestart> {
    None
}

/// Runs the whole check: measure (or fall back to the last speed measured), then plan.
pub fn run(dir: &Path, url: &str, cancel: Arc<AtomicBool>, live: Live) -> Preflight {
    let mut p = run_upload(dir, url, cancel.clone());
    if p.stopped.is_none() {
        p.checks = checks(dir, live);
    }
    p
}

fn run_upload(dir: &Path, url: &str, cancel: Arc<AtomicBool>) -> Preflight {
    let (m, measured, message) = match measure(url, cancel.clone()) {
        Ok(kbps) => {
            let m = Measurement { kbps, measured_at: crate::now_ms() };
            save(dir, m);
            (Some(m), true, None)
        }
        Err(e) if cancel.load(Ordering::Relaxed) => {
            crate::log("engine", format!("upload test stopped: {e}"));
            return stopped();
        }
        Err(e) => {
            crate::log("engine", format!("upload test failed: {e}"));
            let saved = load_saved(dir);
            let message = if saved.is_some() {
                "Couldn't test the upload just now (no internet?); this is the last speed measured."
            } else {
                "Couldn't test the upload: check the internet connection."
            };
            (saved, false, Some(message.to_string()))
        }
    };
    let Some(m) = m else { return failed(message.as_deref().unwrap_or_default()) };
    let destinations = plan(dir, m.kbps);
    Preflight {
        status: "done",
        upload_kbps: Some(m.kbps.round()),
        measured: Some(measured),
        measured_at: Some(m.measured_at),
        summary: preflight_summary(&destinations),
        destinations,
        message,
        checks: Vec::new(),
        stopped: None,
    }
}

fn failed(message: &str) -> Preflight {
    Preflight {
        status: "failed",
        upload_kbps: None,
        measured: None,
        measured_at: None,
        destinations: Vec::new(),
        summary: String::new(),
        message: Some(message.into()),
        checks: Vec::new(),
        stopped: None,
    }
}

/// The answer when the stream starting stopped the test: nothing is wrong.
pub fn stopped() -> Preflight {
    Preflight { stopped: Some(true), ..failed("Stopped: the stream started, and the test would take its bandwidth.") }
}

/// The answer while the test runs.
pub fn measuring() -> Preflight {
    Preflight { status: "measuring", summary: "Measuring the internet upload...".into(), ..failed("") }
        .without_message()
}

impl Preflight {
    fn without_message(mut self) -> Preflight {
        self.message = None;
        self
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// A stand-in for the upload-test endpoint that reads at about `kbps` and answers like
    /// the real one. Returns its URL.
    pub(crate) fn fake_endpoint(kbps: u64) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                std::thread::spawn(move || {
                    let mut out = conn.try_clone().unwrap();
                    let mut r = BufReader::new(conn);
                    let mut chunked = false;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if line.to_ascii_lowercase().starts_with("transfer-encoding: chunked") {
                            chunked = true;
                        }
                        if line == "\r\n" {
                            break;
                        }
                    }
                    assert!(chunked, "the test streams its body");
                    let start = Instant::now();
                    let mut total = 0u64;
                    loop {
                        let mut size = String::new();
                        r.read_line(&mut size).unwrap();
                        let n = usize::from_str_radix(size.trim(), 16).unwrap();
                        let mut chunk = vec![0u8; n + 2];
                        r.read_exact(&mut chunk).unwrap();
                        if n == 0 {
                            break;
                        }
                        total += n as u64;
                        // Read no faster than the link would carry.
                        let due = Duration::from_secs_f64(total as f64 * 8.0 / (kbps as f64 * 1000.0));
                        std::thread::sleep(due.saturating_sub(start.elapsed()));
                    }
                    let body = format!(r#"{{"ok":true,"bytes":{total}}}"#);
                    let _ = write!(
                        out,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        format!("http://127.0.0.1:{port}/api/upload-test")
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jivvy-preflight-{name}-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const TWO: &str = r#"{"version":1,"destinations":[
        {"id":"youtube","name":"YouTube","url":"rtmps://a.rtmps.youtube.com:443/live2","key":"k"},
        {"id":"facebook","name":"Facebook","url":"rtmps://live-api-s.facebook.com:443/rtmp/","key":"k"}]}"#;

    #[test]
    fn measures_about_what_the_link_carries() {
        let url = fake_endpoint(4_000);
        let kbps = measure(&url, Arc::new(AtomicBool::new(false))).unwrap();
        // The socket's buffers send a little ahead, so a bit high is expected; never far off.
        assert!((3_000.0..=5_500.0).contains(&kbps), "measured {kbps:.0} kbps on a 4 Mbps link");
    }

    #[test]
    fn the_plan_is_the_bandwidth_managers_own_choice() {
        let dir = temp("plan");
        std::fs::write(dir.join("stream.json"), TWO).unwrap();
        // Default program: 1080p at 6 Mbps. 10 Mbps up: 7 to spend.
        let p = plan(&dir, 10_000.0);
        assert_eq!(preflight_summary(&p), "YouTube 1080p + Facebook 360p");
        let p = plan(&dir, 1_200.0);
        assert_eq!(preflight_summary(&p), "YouTube 360p. Facebook won't fit and would be paused.");
        // Advanced settings count.
        let advanced = TWO.replace(r#""version":1,"#, r#""version":1,"bandwidth":{"mode":"advanced"},"#).replace(
            r#""rtmps://live-api-s.facebook.com:443/rtmp/","key":"k""#,
            r#""rtmps://live-api-s.facebook.com:443/rtmp/","key":"k","whenShort":"audioOnly""#,
        );
        std::fs::write(dir.join("stream.json"), advanced).unwrap();
        assert_eq!(preflight_summary(&plan(&dir, 1_300.0)), "YouTube 360p + Facebook audio only");
    }

    #[test]
    fn the_checks_say_what_is_set_up_and_what_to_fix() {
        let dir = temp("checks");
        std::fs::write(dir.join("stream.json"), TWO).unwrap();
        let p = run(&dir, "http://127.0.0.1:9/api/upload-test", Arc::new(AtomicBool::new(false)), Live::default());
        let by = |id: &str| p.checks.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no {id} check: {p:?}"));
        assert_eq!(by("stream").level, "ok");
        assert_eq!(by("stream").message, "Set up: YouTube, Facebook.");
        assert_ne!(by("disk").level, "warn", "the temp folder's disk has a free space");
        // No screen windows have run here: amber, with a fix.
        assert_eq!((by("screens").level.as_str(), by("screens").fix.is_some()), ("warn", true));
        // Video isn't installed: not part of this setup, nothing to fix.
        for id in ["encoder", "camera", "microphone"] {
            assert_eq!((by(id).level.as_str(), by(id).fix.as_deref()), ("ok", None), "{id}");
            assert!(by(id).message.starts_with("Not part of this setup"), "{id}");
        }

        let level = |live: Live, id: &str| checks(&dir, live).into_iter().find(|c| c.id == id).unwrap().level;
        // A camera status file only counts while the video process is reporting.
        std::fs::write(dir.join(media::STATUS_FILE), r#"{"camera":{"state":"running"},"microphone":{"state":"off"}}"#)
            .unwrap();
        let alive = Live { video_installed: true, video_alive: true };
        assert_eq!((level(alive, "camera"), level(alive, "microphone")), ("ok".into(), "ok".into()));
        assert_eq!(level(Live { video_alive: false, ..alive }, "camera"), "warn", "left behind by a stopped process");
        // Screens: none showing isn't green; one is.
        let outputs = |shown: &str| {
            let body = format!(r#"{{"connected":true,"outputs":[{shown}],"problems":[]}}"#);
            std::fs::write(dir.join(crate::outputs::STATUS_FILE), body).unwrap();
        };
        outputs("");
        assert_eq!(level(alive, "screens"), "warn");
        outputs(r#"{"id":"main"}"#);
        assert_eq!(level(alive, "screens"), "ok");
        // Disk: amber below the warn level, red only below the floor.
        std::fs::write(dir.join("recording.json"), r#"{"version":1,"warnFreeGb":1000000}"#).unwrap();
        assert_eq!(level(alive, "disk"), "warn", "the temp disk has more than 2 GB but less than a petabyte");
        // Unreadable recording settings: said plainly, never a silent check of the default folder.
        std::fs::write(dir.join("recording.json"), "{").unwrap();
        let disk = checks(&dir, alive).into_iter().find(|c| c.id == "disk").unwrap();
        assert!(disk.level == "warn" && disk.message.contains("can't be read"), "{disk:?}");
    }

    #[test]
    fn a_pending_restart_is_amber_with_a_fix_and_nothing_pending_is_green() {
        let row = restart_row(Some(PendingRestart { windows_update: true, ..Default::default() }));
        assert_eq!((row.id.as_str(), row.level.as_str()), ("restart", "warn"));
        assert!(row.message.contains("Windows Update"), "{row:?}");
        assert_eq!(row.fix.as_deref(), Some("Restart the computer before the service, not during it."));
        let clear = restart_row(Some(PendingRestart::default()));
        assert_eq!((clear.level.as_str(), clear.fix), ("ok", None));
        assert_eq!(restart_row(None).level, "ok");
    }

    #[test]
    fn a_test_that_works_is_saved_and_used_when_the_next_one_cant_run() {
        let dir = temp("fallback");
        std::fs::write(dir.join("stream.json"), TWO).unwrap();
        let p = run(&dir, &fake_endpoint(20_000), Arc::new(AtomicBool::new(false)), Live::default());
        assert_eq!((p.status, p.measured), ("done", Some(true)), "{p:?}");
        let saved = load_saved(&dir).expect("saved");

        // The endpoint is gone (no internet): the last speed, said plainly.
        let p = run(&dir, "http://127.0.0.1:9/api/upload-test", Arc::new(AtomicBool::new(false)), Live::default());
        assert_eq!((p.status, p.measured, p.measured_at), ("done", Some(false), Some(saved.measured_at)));
        assert!(p.message.unwrap().contains("last speed measured"));
        assert!(!p.summary.is_empty());

        // Nothing measured ever: it fails, in words.
        let fresh = temp("never");
        let p = run(&fresh, "http://127.0.0.1:9/api/upload-test", Arc::new(AtomicBool::new(false)), Live::default());
        assert_eq!(p.status, "failed");
        assert_eq!(p.message.as_deref(), Some("Couldn't test the upload: check the internet connection."));
    }

    #[test]
    fn the_stream_starting_stops_the_test() {
        let dir = temp("cancel");
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            stop.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let p = run(&dir, &fake_endpoint(2_000), cancel, Live::default());
        assert_eq!(p.status, "failed");
        assert_eq!(p.stopped, Some(true));
        assert!(p.message.unwrap().contains("the stream started"));
        assert!(started.elapsed() < Duration::from_secs(3), "stopped promptly: {:?}", started.elapsed());
        assert!(load_saved(&dir).is_none(), "a stopped test isn't a measurement");
    }
}
