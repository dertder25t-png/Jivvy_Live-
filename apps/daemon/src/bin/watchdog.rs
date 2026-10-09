//! `jivvy-watchdog`: starts and supervises `jivvy-engine`, `jivvy-outputs` and `jivvy-video`.
//!
//! jivvy-watchdog [--data-dir DIR] [--listen ADDR] [--no-outputs | --outputs-headless] [--no-video]
//!                [--engine PATH] [--outputs PATH] [--video PATH] [--hang-timeout-ms N] [--test-sources]
//!                [engine args...]
//!
//! `--data-dir` and `--listen` go to every child (the others connect to the engine there).
//! `jivvy-video` is skipped when it isn't installed (builds without the `video` feature).
//! Anything else it doesn't recognise is passed through to the engine.

use std::path::PathBuf;
use std::time::Duration;

use jivvy_daemon::watchdog::{ChildSpec, run};

fn sibling(name: &str) -> PathBuf {
    let exe_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&exe_name))).unwrap_or(exe_name.into())
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next().unwrap_or_else(|| panic!("{flag} needs a value"))
}

fn main() {
    let mut engine = sibling("jivvy-engine");
    let mut outputs = Some(sibling("jivvy-outputs"));
    let mut outputs_headless = false;
    let mut test_sources = false;
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
    match video {
        Some(exe) if exe.exists() => {
            let mut args = [shared, vec!["--connect".into(), listen]].concat();
            if test_sources {
                args.push("--test-sources".into());
            }
            // Start-up loads GStreamer's GPU, encoder and device plugins: about 5 s normally,
            // much longer on a busy machine (e.g. Windows Update at boot). Killing it for a
            // slow start would mean it never starts, so the first heartbeat gets 30 s.
            children.push(ChildSpec { name: "video", exe, args, hang_timeout, startup_grace: Duration::from_secs(30) });
        }
        Some(exe) => {
            jivvy_daemon::log("watchdog", format!("{} not installed; running without camera and audio", exe.display()))
        }
        None => {}
    }
    run(children)
}
