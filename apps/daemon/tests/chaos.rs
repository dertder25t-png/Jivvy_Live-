//! Chaos tests for the reliability table's "Video engine crashes" row:
//! kill the engine over and over and require it back on the same slide in under 3 s.

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
    events: Receiver<String>,
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

fn start(name: &str, slides: u64) -> Daemon {
    let data_dir = std::env::temp_dir().join(format!("jivvy-chaos-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let addr = format!("127.0.0.1:{}", free_port());
    let mut watchdog = Command::new(env!("CARGO_BIN_EXE_jivvy-watchdog"))
        .args(["--engine", env!("CARGO_BIN_EXE_jivvy-engine")])
        .args(["--data-dir", data_dir.to_str().unwrap(), "--listen", &addr, "--slides", &slides.to_string()])
        .env("JIVVY_TEST_HOOKS", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (tx, events) = channel();
    let out = watchdog.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    Daemon { watchdog, events, addr, data_dir }
}

impl Daemon {
    /// Waits for the next "engine started" event and returns the engine's pid.
    fn next_engine_pid(&self, within: Duration) -> u32 {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.events.recv_timeout(left).expect("engine did not restart in time");
            if let Some(pid) = line.strip_prefix("engine started pid=") {
                return pid.trim().parse().unwrap();
            }
        }
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
