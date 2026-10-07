//! `jivvy-watchdog`: starts and supervises `jivvy-engine`.
//!
//! jivvy-watchdog [--engine PATH] [--hang-timeout-ms N] [-- engine args...]
//! Any argument it doesn't recognise is passed through to the engine.

use std::path::PathBuf;
use std::time::Duration;

use jivvy_daemon::watchdog::{Config, run};

fn main() {
    let exe_name = format!("jivvy-engine{}", std::env::consts::EXE_SUFFIX);
    let sibling = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&exe_name)));
    let mut cfg = Config {
        engine: sibling.unwrap_or_else(|| PathBuf::from(&exe_name)),
        engine_args: Vec::new(),
        hang_timeout: Duration::from_secs(2),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--engine" => cfg.engine = PathBuf::from(args.next().expect("--engine needs a path")),
            "--hang-timeout-ms" => {
                cfg.hang_timeout = Duration::from_millis(
                    args.next().and_then(|v| v.parse().ok()).expect("--hang-timeout-ms needs a number"),
                )
            }
            "--" => cfg.engine_args.extend(args.by_ref()),
            _ => cfg.engine_args.push(a),
        }
    }
    run(cfg)
}
