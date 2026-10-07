//! `jivvy-watchdog`: starts and supervises `jivvy-engine` and `jivvy-outputs`.
//!
//! jivvy-watchdog [--data-dir DIR] [--listen ADDR] [--no-outputs | --outputs-headless]
//!                [--engine PATH] [--outputs PATH] [--hang-timeout-ms N] [engine args...]
//!
//! `--data-dir` and `--listen` go to both children (outputs connect to the engine there).
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
        let mut args = [shared, vec!["--connect".into(), listen]].concat();
        if outputs_headless {
            args.push("--headless".into());
        }
        // Creating the first WebView can take a few seconds on a slow laptop.
        children.push(ChildSpec { name: "outputs", exe, args, hang_timeout, startup_grace: Duration::from_secs(10) });
    }
    run(children)
}
