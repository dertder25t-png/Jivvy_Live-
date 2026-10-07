//! Chaos tests for the reliability table's "Video engine crashes" row: kill the engine
//! over and over and require it back on the same slide in under 3 s; and for the output
//! windows (run headless): screens never blank while the engine restarts, a killed outputs
//! process comes back on the current slide, and monitors can come and go.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const RESTORE_TARGET: Duration = Duration::from_secs(3);

struct Daemon {
    watchdog: Child,
    engine_starts: Receiver<u32>,
    outputs_starts: Receiver<u32>,
    addr: String,
    data_dir: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.watchdog.kill();
        let _ = self.watchdog.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jivvy-chaos-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Starts the watchdog without output windows (they're covered by the headless tests).
fn start(name: &str, slides: u64) -> Daemon {
    start_in(test_dir(name), slides, false)
}

fn start_in(data_dir: PathBuf, slides: u64, headless_outputs: bool) -> Daemon {
    let addr = format!("127.0.0.1:{}", free_port());
    let outputs_flag = if headless_outputs { "--outputs-headless" } else { "--no-outputs" };
    let mut watchdog = Command::new(env!("CARGO_BIN_EXE_jivvy-watchdog"))
        .args(["--engine", env!("CARGO_BIN_EXE_jivvy-engine"), "--outputs", env!("CARGO_BIN_EXE_jivvy-outputs")])
        .args(["--data-dir", data_dir.to_str().unwrap(), "--listen", &addr, "--slides", &slides.to_string()])
        .arg(outputs_flag)
        .env("JIVVY_TEST_HOOKS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (engine_tx, engine_starts) = channel();
    let (outputs_tx, outputs_starts) = channel();
    let out = watchdog.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let started =
                |name: &str| line.strip_prefix(&format!("{name} started pid=")).map(|p| p.trim().parse().unwrap());
            if let Some(pid) = started("engine") {
                let _ = engine_tx.send(pid);
            } else if let Some(pid) = started("outputs") {
                let _ = outputs_tx.send(pid);
            }
        }
    });
    Daemon { watchdog, engine_starts, outputs_starts, addr, data_dir }
}

impl Daemon {
    /// Waits for the next "engine started" event and returns the engine's pid.
    fn next_engine_pid(&self, within: Duration) -> u32 {
        self.engine_starts.recv_timeout(within).expect("engine did not (re)start in time")
    }

    fn next_outputs_pid(&self, within: Duration) -> u32 {
        self.outputs_starts.recv_timeout(within).expect("outputs did not (re)start in time")
    }

    /// Waits until `outputs-status.json` satisfies `ok`, returning it.
    fn outputs_status(&self, within: Duration, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + within;
        let mut last = Value::Null;
        while Instant::now() < deadline {
            if let Ok(b) = std::fs::read(self.data_dir.join("outputs-status.json"))
                && let Ok(v) = serde_json::from_slice::<Value>(&b)
            {
                if ok(&v) {
                    return v;
                }
                last = v;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("outputs never reached: {what}; last status {last}");
    }

    /// Sends one command, retrying the connection until the engine answers or time runs out.
    fn send(&self, command: Value, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        let msg = format!("{}\n", json!({ "v": 1, "id": "chaos", "ts": 1, "command": command }));
        loop {
            let attempt = (|| -> std::io::Result<Value> {
                let mut s = TcpStream::connect(&self.addr)?;
                s.set_read_timeout(Some(Duration::from_millis(500)))?;
                s.write_all(msg.as_bytes())?;
                let mut line = String::new();
                BufReader::new(s).read_line(&mut line)?;
                serde_json::from_str(&line).map_err(std::io::Error::other)
            })();
            match attempt {
                Ok(v) => return v,
                Err(e) if Instant::now() >= deadline => panic!("engine did not answer in time: {e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

fn kill_hard(pid: u32) {
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    assert!(status.unwrap().success(), "could not kill engine {pid}");
}

fn pid_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    } else {
        Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
    }
}

#[test]
fn engine_killed_100_times_comes_back_on_the_same_slide_in_under_3_seconds() {
    let d = start("kill100", 10);
    let mut pid = d.next_engine_pid(Duration::from_secs(10));
    let mut worst = Duration::ZERO;
    for i in 0..100u64 {
        let (slide, black) = (i * 7 % 10, i % 3 == 0);
        let r = d.send(json!({ "type": "slide.goto", "index": slide }), Duration::from_secs(5));
        assert_eq!(r["ok"], json!(true), "goto failed: {r}");
        d.send(json!({ "type": "output.black", "on": black }), Duration::from_secs(5));

        let killed_at = Instant::now();
        kill_hard(pid);
        pid = d.next_engine_pid(RESTORE_TARGET);
        let r = d.send(json!({ "type": "state.get" }), RESTORE_TARGET);
        let took = killed_at.elapsed();
        worst = worst.max(took);
        assert!(took < RESTORE_TARGET, "kill {i}: back after {took:?}");
        assert_eq!(r["state"]["slideIndex"], json!(slide), "kill {i}: wrong slide");
        assert_eq!(r["state"]["black"], json!(black), "kill {i}: wrong black state");
    }
    println!("100 kills; slowest return to the same slide: {worst:?}");
}

#[test]
fn hung_engine_is_killed_and_resumes_on_the_same_slide() {
    let d = start("hang", 10);
    let first = d.next_engine_pid(Duration::from_secs(10));
    d.send(json!({ "type": "slide.goto", "index": 4 }), Duration::from_secs(5));
    std::fs::write(d.data_dir.join("hang-once"), b"").unwrap();
    let hung_at = Instant::now();
    // 2 s without a heartbeat, then a restart.
    let second = d.next_engine_pid(Duration::from_secs(5));
    assert_ne!(first, second);
    let r = d.send(json!({ "type": "state.get" }), RESTORE_TARGET);
    assert_eq!(r["state"]["slideIndex"], json!(4));
    let took = hung_at.elapsed();
    // Hang detection needs the 2 s heartbeat timeout plus up to one 250 ms beat.
    assert!(took < Duration::from_millis(3500), "hang recovery took {took:?}");
    assert!(!pid_alive(first), "hung engine was not killed");
}

#[test]
fn engine_exits_when_the_watchdog_dies_so_the_port_is_freed() {
    let mut d = start("orphan", 5);
    let pid = d.next_engine_pid(Duration::from_secs(10));
    d.send(json!({ "type": "state.get" }), Duration::from_secs(5));
    d.watchdog.kill().unwrap();
    d.watchdog.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while pid_alive(pid) {
        assert!(Instant::now() < deadline, "engine outlived its watchdog");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn corrupted_state_file_falls_back_to_the_previous_snapshot() {
    let d = start("corrupt", 10);
    let pid = d.next_engine_pid(Duration::from_secs(10));
    d.send(json!({ "type": "slide.goto", "index": 2 }), Duration::from_secs(5));
    d.send(json!({ "type": "slide.goto", "index": 3 }), Duration::from_secs(5));
    std::fs::write(d.data_dir.join("state.json"), b"{not json").unwrap();
    kill_hard(pid);
    d.next_engine_pid(RESTORE_TARGET);
    let r = d.send(json!({ "type": "state.get" }), RESTORE_TARGET);
    assert_eq!(r["state"]["slideIndex"], json!(2));
}

fn displays(dir: &std::path::Path, names: &[&str]) {
    let list: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(i, n)| json!({ "name": n, "x": i as i32 * 1920, "y": 0, "width": 1920, "height": 1080, "scale": 1.0, "primary": i == 0 }))
        .collect();
    let tmp = dir.join("test-displays.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&list).unwrap()).unwrap();
    std::fs::rename(tmp, dir.join("test-displays.json")).unwrap();
}

fn output_ids(status: &Value) -> Vec<String> {
    status["outputs"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn outputs_keep_the_last_slide_while_the_engine_restarts_then_follow_again() {
    let dir = test_dir("outputs-engine");
    displays(&dir, &["OPERATOR", "PROJECTOR"]);
    let d = start_in(dir, 10, true);
    let engine = d.next_engine_pid(Duration::from_secs(10));
    d.next_outputs_pid(Duration::from_secs(10));
    d.send(json!({ "type": "slide.goto", "index": 5 }), Duration::from_secs(5));
    let s = d.outputs_status(Duration::from_secs(5), "showing slide 5 on the projector", |s| {
        s["shown"]["slideIndex"] == 5 && output_ids(s) == ["auto:PROJECTOR"]
    });
    assert_eq!(s["connected"], true);

    kill_hard(engine);
    // Until the new engine is up, every sample must still show slide 5. (The disconnect
    // itself can be too brief to see: on Linux the engine is back within one status write.)
    let deadline = Instant::now() + RESTORE_TARGET;
    loop {
        let s = d.outputs_status(Duration::from_secs(1), "a readable status", |_| true);
        assert_eq!(s["shown"]["slideIndex"], 5, "the screen must keep the last slide, not go blank");
        if let Ok(_new_engine) = d.engine_starts.try_recv() {
            break;
        }
        assert!(Instant::now() < deadline, "engine did not restart in time");
        std::thread::sleep(Duration::from_millis(20));
    }
    d.send(json!({ "type": "slide.goto", "index": 6 }), RESTORE_TARGET);
    d.outputs_status(RESTORE_TARGET, "following the restarted engine", |s| {
        s["connected"] == true && s["shown"]["slideIndex"] == 6
    });
    assert!(d.outputs_starts.try_recv().is_err(), "the outputs process must not restart when the engine does");
}

#[test]
fn killed_outputs_process_comes_back_on_the_current_slide() {
    let dir = test_dir("outputs-kill");
    displays(&dir, &["OPERATOR", "PROJECTOR"]);
    let d = start_in(dir.clone(), 10, true);
    d.next_engine_pid(Duration::from_secs(10));
    let outputs = d.next_outputs_pid(Duration::from_secs(10));
    d.send(json!({ "type": "slide.goto", "index": 3 }), Duration::from_secs(5));
    d.outputs_status(Duration::from_secs(5), "slide 3", |s| s["shown"]["slideIndex"] == 3);

    std::fs::remove_file(dir.join("outputs-status.json")).unwrap();
    let killed_at = Instant::now();
    kill_hard(outputs);
    d.next_outputs_pid(RESTORE_TARGET);
    d.outputs_status(RESTORE_TARGET, "slide 3 after restart", |s| s["shown"]["slideIndex"] == 3);
    assert!(killed_at.elapsed() < RESTORE_TARGET, "outputs back after {:?}", killed_at.elapsed());
}

#[test]
fn monitors_coming_and_going_never_put_an_output_on_the_operators_screen() {
    let dir = test_dir("outputs-hotplug");
    displays(&dir, &["OPERATOR", "PROJECTOR"]);
    std::fs::write(
        dir.join("outputs.json"),
        r#"{"version":1,"outputs":[{"id":"proj","monitor":"PROJECTOR","width":1280,"height":720}]}"#,
    )
    .unwrap();
    let d = start_in(dir.clone(), 10, true);
    d.next_outputs_pid(Duration::from_secs(10));
    let s = d.outputs_status(Duration::from_secs(5), "projector output", |s| output_ids(s) == ["proj"]);
    assert_eq!(
        (s["outputs"][0]["canvasWidth"].clone(), s["outputs"][0]["canvasHeight"].clone()),
        (json!(1280), json!(720))
    );

    displays(&dir, &["OPERATOR"]);
    let s = d.outputs_status(Duration::from_secs(3), "projector unplugged", |s| output_ids(s).is_empty());
    assert!(s["problems"][0]["message"].as_str().unwrap().contains("not connected"));

    displays(&dir, &["OPERATOR", "PROJECTOR"]);
    d.outputs_status(Duration::from_secs(3), "projector plugged back in", |s| output_ids(s) == ["proj"]);

    std::fs::write(dir.join("outputs.json"), "{ half-saved").unwrap();
    let s = d.outputs_status(Duration::from_secs(3), "a broken config reported", |s| {
        s["problems"].as_array().is_some_and(|p| !p.is_empty())
    });
    assert_eq!(output_ids(&s), ["proj"], "a broken config file keeps the screens as they were");
}
