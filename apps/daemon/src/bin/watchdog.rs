//! `jivvy-watchdog`: starts and supervises `jivvy-engine`, `jivvy-outputs` and `jivvy-video`.
//!
//! jivvy-watchdog [--data-dir DIR] [--listen ADDR] [--no-outputs | --outputs-headless] [--no-video]
//!                [--engine PATH] [--outputs PATH] [--video PATH] [--hang-timeout-ms N] [--test-sources]
//!                [--recheck-hardware | --no-hardware-check]
//!                [engine args...]
//!
//! `--data-dir` and `--listen` go to every child (the others connect to the engine there).
//! `jivvy-video` is skipped when it isn't installed (builds without the `video` feature).
//! Before video starts, the hardware check runs if there's no `hardware.json` yet or this machine
//! no longer matches the one it was made on (a new graphics card or driver, another computer),
//! while the engine and outputs are already up (see `hardware_check`). `--recheck-hardware`
//! measures again whatever the report says; `--no-hardware-check` skips it.
//! Anything else it doesn't recognise is passed through to the engine.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jivvy_daemon::watchdog::{ChildSpec, run, supervise};

/// A first hardware check that hasn't finished by now is stopped, and video starts anyway.
const HARDWARE_CHECK_LIMIT: Duration = Duration::from_secs(300);

fn sibling(name: &str) -> PathBuf {
    let exe_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&exe_name))).unwrap_or(exe_name.into())
}

/// When the hardware check runs before video.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckMode {
    /// No report yet, or this machine differs from the one it was made on (the default).
    IfChanged,
    /// Whatever the report says (`--recheck-hardware`).
    Always,
    /// Never (`--no-hardware-check`): video uses the report as it is.
    Never,
}

/// Setup: make sure the program starts on a picture path, encoder and quality that keep up on
/// *this* machine. Slides and outputs are already running; only video waits, and only when the
/// machine changed (or never was measured). A check that finds nothing changed takes about a
/// second, so a restart mid-service is not held up.
fn hardware_check(video: &Path, data_dir: &Path, mode: CheckMode) {
    if mode == CheckMode::Never {
        return;
    }
    println!("hardware-check started");
    let started = Instant::now();
    // Supervised: it holds our stdin pipe and exits when we do, so a restarted watchdog never
    // finds an old check still running (two checks would slow each other and race to save).
    let mut cmd = Command::new(video);
    cmd.args(["--hardware-check", "--supervised", "--data-dir"]).arg(data_dir);
    if mode == CheckMode::IfChanged {
        cmd.arg("--if-changed");
    }
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null()) // the report goes to hardware.json; the log to our stderr
        .spawn();
    let result = match child {
        Err(e) => format!("could not start ({e})"),
        Ok(mut c) => loop {
            match c.try_wait() {
                Ok(Some(status)) => break format!("{status}"),
                Ok(None) if started.elapsed() > HARDWARE_CHECK_LIMIT => {
                    let _ = c.kill();
                    let _ = c.wait();
                    break format!("stopped after {} s", HARDWARE_CHECK_LIMIT.as_secs());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => break format!("wait failed ({e})"),
            }
        },
    };
    println!("hardware-check finished secs={} result={result}", started.elapsed().as_secs());
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next().unwrap_or_else(|| panic!("{flag} needs a value"))
}

fn main() {
    let mut engine = sibling("jivvy-engine");
    let mut outputs = Some(sibling("jivvy-outputs"));
    let mut outputs_headless = false;
    let mut test_sources = false;
    let mut check_mode = CheckMode::IfChanged;
    let mut video = Some(sibling("jivvy-video"));
    let mut hang_timeout = Duration::from_secs(2);
    let mut shared: Vec<String> = Vec::new();
    let mut listen = jivvy_daemon::DEFAULT_LISTEN.to_string();
    let mut engine_args: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--engine" => engine = value(&mut args, "--engine").into(),
            "--outputs" => outputs = Some(value(&mut args, "--outputs").into()),
            "--no-outputs" => outputs = None,
            "--outputs-headless" => outputs_headless = true,
            "--video" => video = Some(value(&mut args, "--video").into()),
            "--no-video" => video = None,
            // Camera and microphone from test sources: runs on any machine (see jivvy-video).
            "--test-sources" => test_sources = true,
            "--recheck-hardware" => check_mode = CheckMode::Always,
            "--no-hardware-check" => check_mode = CheckMode::Never,
            "--hang-timeout-ms" => {
                hang_timeout =
                    Duration::from_millis(value(&mut args, "--hang-timeout-ms").parse().expect("a number of ms"))
            }
            "--data-dir" => shared.extend([a, value(&mut args, "--data-dir")]),
            "--listen" => listen = value(&mut args, "--listen"),
            "--" => engine_args.extend(args.by_ref()),
            _ => engine_args.push(a),
        }
    }

    let mut children = vec![ChildSpec {
        name: "engine",
        exe: engine,
        args: [shared.clone(), vec!["--listen".into(), listen.clone()], engine_args].concat(),
        hang_timeout,
        startup_grace: hang_timeout,
    }];
    if let Some(exe) = outputs {
        let mut args = [shared.clone(), vec!["--connect".into(), listen.clone()]].concat();
        if outputs_headless {
            args.push("--headless".into());
        }
        // Creating the first WebView can take a few seconds on a slow laptop.
        children.push(ChildSpec { name: "outputs", exe, args, hang_timeout, startup_grace: Duration::from_secs(10) });
    }
    let data_dir = shared
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| shared.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(jivvy_daemon::default_data_dir);
    match video {
        Some(exe) if exe.exists() => {
            let mut args = [shared, vec!["--connect".into(), listen]].concat();
            if test_sources {
                args.push("--test-sources".into());
            }
            // Start-up loads GStreamer's GPU, encoder and device plugins: about 5 s normally,
            // much longer on a busy machine (e.g. Windows Update at boot). Killing it for a
            // slow start would mean it never starts, so the first heartbeat gets 30 s.
            let spec = ChildSpec { name: "video", exe, args, hang_timeout, startup_grace: Duration::from_secs(30) };
            std::thread::spawn(move || {
                hardware_check(&spec.exe, &data_dir, check_mode);
                supervise(spec)
            });
        }
        Some(exe) => {
            jivvy_daemon::log("watchdog", format!("{} not installed; running without camera and audio", exe.display()))
        }
        None => {}
    }
    run(children)
}
