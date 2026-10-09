//! Chaos tests for the reliability table's "Video engine crashes" row: kill the engine
//! over and over and require it back on the same slide in under 3 s; and for the output
//! windows (run headless): screens never blank while the engine restarts, a killed outputs
//! process comes back on the current slide, and monitors can come and go. With the `video`
//! feature, also camera and audio input (GStreamer test sources): levels reach screens,
//! survive a video crash, and a missing camera never stops the audio; and streaming to a
//! local MediaMTX server and a fake YouTube HLS ingest (the reliability table's "Internet
//! drops" row).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const RESTORE_TARGET: Duration = Duration::from_secs(3);

/// How many times the kill test kills the engine: `JIVVY_CHAOS_ITERATIONS` if set, else the
/// reliability table's full 100 in CI (GitHub Actions sets `CI`) and 10 for a quick local run.
/// `.\dev.ps1 test` and releases run the full 100.
fn chaos_iterations() -> u64 {
    match std::env::var("JIVVY_CHAOS_ITERATIONS") {
        // Zero would pass without a single kill, so it is refused like any bad value.
        Ok(n) => match n.trim().parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => panic!("JIVVY_CHAOS_ITERATIONS must be a whole number of at least 1, got {n:?}"),
        },
        Err(_) if std::env::var_os("CI").is_some() => 100,
        Err(_) => 10,
    }
}

struct Daemon {
    watchdog: Child,
    /// Held while this daemon runs a video process (see `VIDEO_SLOTS`).
    _video_slot: Option<VideoSlot>,
    engine_starts: Receiver<u32>,
    outputs_starts: Receiver<u32>,
    #[allow(dead_code)] // only read by the video tests
    video_starts: Receiver<u32>,
    addr: String,
    data_dir: TestDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.watchdog.kill();
        let _ = self.watchdog.wait();
    }
}

/// Daemons with a video process run one at a time. Each loads GStreamer's GPU plugins at
/// start-up and encodes 1080p30 on the machine's one hardware encoder: two or three at once
/// held the founder's laptop to ~20 fps, while a church computer only ever runs one.
const VIDEO_SLOTS: u32 = 1;
static VIDEO_IN_USE: std::sync::Mutex<u32> = std::sync::Mutex::new(0);
static VIDEO_FREED: std::sync::Condvar = std::sync::Condvar::new();

struct VideoSlot;

impl VideoSlot {
    fn take() -> VideoSlot {
        let mut n = VIDEO_IN_USE.lock().unwrap_or_else(|p| p.into_inner());
        while *n >= VIDEO_SLOTS {
            n = VIDEO_FREED.wait(n).unwrap_or_else(|p| p.into_inner());
        }
        *n += 1;
        VideoSlot
    }
}

impl Drop for VideoSlot {
    fn drop(&mut self) {
        *VIDEO_IN_USE.lock().unwrap_or_else(|p| p.into_inner()) -= 1;
        VIDEO_FREED.notify_one();
    }
}

/// A port for a test daemon or server, from 20000-32767: below the ephemeral ranges (Windows
/// 49152+, Linux 32768+) that outgoing connections take their local ports from. The tests make
/// many short connections, and one landing on a daemon's port while it is briefly free (before
/// the engine binds it, or across an engine restart) would keep the engine off it.
fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    // Each test process starts at its own place, so two runs at once rarely try the same ports.
    let seed = std::process::id().wrapping_mul(2_654_435_761);
    for _ in 0..1000 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let port = 20_000 + (seed.wrapping_add(n) % 12_768) as u16;
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free port between 20000 and 32767");
}

/// A test's folder in the temp directory. Deleted when the test passes; kept, with its path
/// printed, when it fails, so the daemon's log and files can be read.
struct TestDir(PathBuf);

impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("test files kept: {}", self.0.display());
            return;
        }
        // The daemon's children exit a moment after its watchdog (they see its pipe close), and
        // Windows won't delete a file a process still has open.
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::remove_dir_all(&self.0).is_err() && self.0.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn test_dir(name: &str) -> TestDir {
    let dir = std::env::temp_dir().join(format!("jivvy-chaos-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TestDir(dir)
}

/// Puts this test process in a Windows job object that ends everything in it when the process
/// ends, so the daemons and MediaMTX servers it starts never outlive it, even when the test
/// binary is killed outright (an IDE's stop button, Task Manager). `cargo test` does this for
/// its own children, but not for a test binary run directly. Children join the job as they are
/// created, so this runs before the first spawn.
#[cfg(windows)]
fn end_children_with_this_process() {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain Win32 calls on a job handle this function owns. The handle is never
        // closed: Windows closes it when this process exits, which is what ends the job.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            assert!(!job.is_null(), "CreateJobObjectW failed: {}", std::io::Error::last_os_error());
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of_val(&info) as u32;
            let set = SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const info).cast(), size);
            assert!(set != 0, "SetInformationJobObject failed: {}", std::io::Error::last_os_error());
            let joined = AssignProcessToJobObject(job, GetCurrentProcess());
            assert!(joined != 0, "AssignProcessToJobObject failed: {}", std::io::Error::last_os_error());
        }
    });
}

#[cfg(not(windows))]
fn end_children_with_this_process() {}

/// Starts the watchdog without output windows (they're covered by the headless tests)
/// and without camera/audio (never open the real camera during tests).
fn start(name: &str, slides: u64) -> Daemon {
    start_in(test_dir(name), slides, false)
}

fn start_in(data_dir: TestDir, slides: u64, headless_outputs: bool) -> Daemon {
    start_with(data_dir, slides, headless_outputs, &["--no-video"])
}

fn start_with(data_dir: TestDir, slides: u64, headless_outputs: bool, extra: &[&str]) -> Daemon {
    start_with_env(data_dir, slides, headless_outputs, extra, &[])
}

fn start_with_env(
    data_dir: TestDir,
    slides: u64,
    headless_outputs: bool,
    extra: &[&str],
    envs: &[(&str, &str)],
) -> Daemon {
    start_at(free_port(), data_dir, &DaemonArgs { slides, headless_outputs, extra, envs })
}

struct DaemonArgs<'a> {
    slides: u64,
    headless_outputs: bool,
    extra: &'a [&'a str],
    envs: &'a [(&'a str, &'a str)],
}

/// Starts the daemon on `port`. If that port is taken by the time the engine binds it, the
/// daemon is stopped and started again on a fresh port (up to 5 tries).
fn start_at(mut port: u16, data_dir: TestDir, args: &DaemonArgs) -> Daemon {
    end_children_with_this_process();
    let video_slot = (!args.extra.contains(&"--no-video")).then(VideoSlot::take);
    for attempt in 1..=5 {
        let addr = format!("127.0.0.1:{port}");
        let (mut watchdog, [engine_starts, outputs_starts, video_starts]) = spawn_watchdog(&data_dir, &addr, args);
        if engine_bound(&data_dir, &addr) != Some(false) {
            return Daemon {
                watchdog,
                _video_slot: video_slot,
                engine_starts,
                outputs_starts,
                video_starts,
                addr,
                data_dir,
            };
        }
        println!("attempt {attempt}: port {port} was taken; starting again on another");
        let _ = watchdog.kill();
        let _ = watchdog.wait();
        // Its engine exits when it sees the watchdog gone; wait so it never shares the folder.
        if let Ok(pid) = engine_starts.recv_timeout(Duration::from_secs(1)) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while pid_alive(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        port = free_port();
    }
    let log = std::fs::read_to_string(data_dir.join("daemon.log")).unwrap_or_default();
    panic!("the engine could not get a port in 5 tries\n{log}");
}

/// The watchdog, and its engine, outputs and video start events (pids, in that order).
fn spawn_watchdog(data_dir: &Path, addr: &str, args: &DaemonArgs) -> (Child, [Receiver<u32>; 3]) {
    let outputs_flag = if args.headless_outputs { "--outputs-headless" } else { "--no-outputs" };
    let mut watchdog = Command::new(env!("CARGO_BIN_EXE_jivvy-watchdog"))
        .args(["--engine", env!("CARGO_BIN_EXE_jivvy-engine"), "--outputs", env!("CARGO_BIN_EXE_jivvy-outputs")])
        .args(["--data-dir", data_dir.to_str().unwrap(), "--listen", addr, "--slides", &args.slides.to_string()])
        .arg(outputs_flag)
        .args(args.extra)
        .env("JIVVY_TEST_HOOKS", "1")
        .envs(args.envs.iter().copied())
        .stdout(Stdio::piped())
        // The daemon's log goes to a file in its data directory, shown when a check fails.
        .stderr(if std::env::var_os("JIVVY_TEST_STDERR").is_some() {
            Stdio::inherit()
        } else {
            std::fs::File::create(data_dir.join("daemon.log")).map(Stdio::from).unwrap_or(Stdio::null())
        })
        .spawn()
        .unwrap();
    let (engine_tx, engine_starts) = channel();
    let (outputs_tx, outputs_starts) = channel();
    let (video_tx, video_starts) = channel();
    let out = watchdog.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let started =
                |name: &str| line.strip_prefix(&format!("{name} started pid=")).map(|p| p.trim().parse().unwrap());
            if let Some(pid) = started("engine") {
                let _ = engine_tx.send(pid);
            } else if let Some(pid) = started("outputs") {
                let _ = outputs_tx.send(pid);
            } else if let Some(pid) = started("video") {
                let _ = video_tx.send(pid);
            } else {
                println!("watchdog: {line}"); // stop reasons, shown when a test fails
            }
        }
    });
    (watchdog, [engine_starts, outputs_starts, video_starts])
}

/// Whether the engine got its port, read from the daemon's log: `Some(false)` if the port was
/// taken. `None` when the log isn't in a file (`JIVVY_TEST_STDERR`) or says neither in time;
/// the test's own checks then report what went wrong.
fn engine_bound(data_dir: &Path, addr: &str) -> Option<bool> {
    if std::env::var_os("JIVVY_TEST_STDERR").is_some() {
        return None;
    }
    let (listening, taken) = (format!("[engine] listening on {addr}"), format!("[engine] bind {addr} failed"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let log = std::fs::read_to_string(data_dir.join("daemon.log")).unwrap_or_default();
        if log.contains(&listening) {
            return Some(true);
        }
        if log.contains(&taken) {
            return Some(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

impl Daemon {
    /// The last lines of the daemon's log, for a failure message.
    fn log_tail(&self) -> String {
        let log = std::fs::read_to_string(self.data_dir.join("daemon.log")).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        format!("daemon log (last 40 lines):\n{}", lines[lines.len().saturating_sub(40)..].join("\n"))
    }

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
        panic!("outputs never reached: {what}; last status {last}\n{}", self.log_tail());
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
            .stderr(if std::env::var_os("JIVVY_TEST_STDERR").is_some() { Stdio::inherit() } else { Stdio::null() })
            .status()
    } else {
        Command::new("kill").args(["-9", &pid.to_string()]).status()
    };
    // Already gone is fine: on a slow machine the watchdog may have killed it as hung first
    // (and restarted it, which the callers check).
    assert!(status.unwrap().success() || !pid_alive(pid), "could not kill engine {pid}");
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
    let kills = chaos_iterations();
    for i in 0..kills {
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
    println!("{kills} kills; slowest return to the same slide: {worst:?}");
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

#[test]
fn a_test_folder_is_deleted_when_the_test_passes_and_kept_when_it_fails() {
    let passed = test_dir("dir-passed");
    std::fs::write(passed.join("daemon.log"), b"ok").unwrap();
    let passed_path = passed.to_path_buf();
    drop(passed);
    assert!(!passed_path.exists(), "a passing test's folder is deleted");

    let failed_path = std::env::temp_dir().join(format!("jivvy-chaos-dir-failed-{}", std::process::id()));
    let failed = std::panic::catch_unwind(|| {
        let dir = test_dir("dir-failed");
        std::fs::write(dir.join("daemon.log"), b"what went wrong").unwrap();
        panic!("a failing check (expected by this test)");
    });
    assert!(failed.is_err());
    assert!(failed_path.join("daemon.log").exists(), "a failing test's folder is kept");
    std::fs::remove_dir_all(&failed_path).unwrap();
}

#[test]
fn a_daemon_whose_port_is_taken_starts_again_on_another() {
    let port = free_port();
    assert!((20_000..32_768).contains(&port), "test ports stay below the ephemeral range: {port}");
    let _taken = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let args = DaemonArgs { slides: 5, headless_outputs: false, extra: &["--no-video"], envs: &[] };
    let d = start_at(port, test_dir("port-taken"), &args);
    assert_ne!(d.addr, format!("127.0.0.1:{port}"), "started again on another port");
    let r = d.send(json!({ "type": "slide.goto", "index": 2 }), Duration::from_secs(5));
    assert_eq!(r["ok"], json!(true), "the restarted daemon answers: {r}");
}

/// Run by `a_killed_test_run_leaves_no_daemon_behind` in a child test process: starts a
/// daemon, reports its engine's pid, then waits to be killed.
#[test]
#[ignore = "helper for a_killed_test_run_leaves_no_daemon_behind"]
fn helper_daemon_that_waits_to_be_killed() {
    if std::env::var_os("JIVVY_CHAOS_HELPER").is_none() {
        return;
    }
    let d = start("killed-run", 5);
    println!("engine pid={}", d.next_engine_pid(Duration::from_secs(10)));
    std::thread::sleep(Duration::from_secs(60));
}

#[cfg(windows)]
#[test]
fn a_killed_test_run_leaves_no_daemon_behind() {
    end_children_with_this_process();
    let mut run = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "helper_daemon_that_waits_to_be_killed", "--ignored", "--nocapture"])
        .env("JIVVY_CHAOS_HELPER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let engine: u32 = BufReader::new(run.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
        .find_map(|l| l.strip_prefix("engine pid=").map(|p| p.trim().parse().unwrap()))
        .expect("the helper started a daemon");

    // Killed outright, as by an IDE's stop button: no destructors run in the test process.
    kill_hard(run.id());
    let _ = run.wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(engine) {
        assert!(Instant::now() < deadline, "the daemon outlived the killed test run");
        std::thread::sleep(Duration::from_millis(100));
    }
    // Its folder has no test left to delete it.
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("jivvy-chaos-killed-run-{}", run.id())));
}

/// Runs `jivvy-ctl` with `args`: (exit code, stdout, stderr).
fn ctl(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_jivvy-ctl")).args(args).output().unwrap();
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (out.status.code().unwrap_or(-1), text(out.stdout), text(out.stderr))
}

#[test]
fn jivvy_ctl_drives_a_running_daemon_in_one_line() {
    let d = start("ctl", 10);
    d.next_engine_pid(Duration::from_secs(10));
    let at = ["--connect", d.addr.as_str()];
    let state = |args: &[&str]| -> Value {
        let (code, out, err) = ctl(&[&at[..], args].concat());
        assert_eq!(code, 0, "jivvy-ctl {args:?}: {err}");
        serde_json::from_str::<Value>(&out).unwrap()["state"].clone()
    };
    assert_eq!(state(&["next"])["slideIndex"], 1);
    assert_eq!(state(&["next"])["slideIndex"], 2);
    assert_eq!(state(&["back"])["slideIndex"], 1);
    assert_eq!(state(&["goto", "7"])["slideIndex"], 7);
    assert_eq!(state(&["black"])["black"], true);
    assert_eq!(state(&["black", "off"])["black"], false);
    assert_eq!(state(&["state"])["slideIndex"], 7);

    let (code, out, err) = ctl(&[&at[..], &["goto", "99"]].concat());
    assert_eq!(code, 1, "a refused command exits 1: {out}");
    assert!(out.contains("bad_arguments") && err.contains("refused"), "{out} {err}");
    let (code, _, err) = ctl(&["--connect", &format!("127.0.0.1:{}", free_port()), "state"]);
    assert_eq!(code, 1);
    assert!(err.contains("can't reach the daemon"), "{err}");
    let (code, _, err) = ctl(&["stream", "pause"]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown command"), "{err}");
}

/// True if every field in `expected` is in `actual` with the same value (objects compared
/// the same way, recursively): how the shared scenarios match an ack.
fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => e.iter().all(|(k, v)| a.get(k).is_some_and(|av| contains(av, v))),
        _ => actual == expected,
    }
}

#[test]
fn shared_scenarios_behave_the_same_on_the_real_daemon() {
    // Also run against the simulated daemon (packages/sim-daemon/test/scenarios.test.ts).
    const SCENARIOS: &str = include_str!("../../../packages/protocol/fixtures/scenarios.json");
    let shared: Value = serde_json::from_str(SCENARIOS).unwrap();
    let scenarios = shared["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 5, "the shared scenarios are there");
    // Each scenario gets its own daemon; they run side by side.
    std::thread::scope(|scope| {
        for (n, scenario) in scenarios.iter().enumerate() {
            scope.spawn(move || {
                let name = scenario["name"].as_str().unwrap();
                let d = start(&format!("scenario-{n}"), scenario["slides"].as_u64().unwrap());
                let mut engine = d.next_engine_pid(Duration::from_secs(10));
                for (i, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
                    if step["crash"] == true {
                        kill_hard(engine);
                        engine = d.next_engine_pid(RESTORE_TARGET);
                        continue;
                    }
                    // Waits for a restarted engine to answer, like any client reconnecting.
                    let ack = d.send(step["send"].clone(), RESTORE_TARGET);
                    assert!(
                        contains(&ack, &step["expect"]),
                        "{name}, step {i} ({}): expected {} in {ack}",
                        step["send"],
                        step["expect"]
                    );
                }
            });
        }
    });
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
    let d = start_in(dir, 10, true);
    d.next_engine_pid(Duration::from_secs(10));
    let outputs = d.next_outputs_pid(Duration::from_secs(10));
    d.send(json!({ "type": "slide.goto", "index": 3 }), Duration::from_secs(5));
    d.outputs_status(Duration::from_secs(5), "slide 3", |s| s["shown"]["slideIndex"] == 3);

    std::fs::remove_file(d.data_dir.join("outputs-status.json")).unwrap();
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
    let d = start_in(dir, 10, true);
    d.next_outputs_pid(Duration::from_secs(10));
    let s = d.outputs_status(Duration::from_secs(5), "projector output", |s| output_ids(s) == ["proj"]);
    assert_eq!(
        (s["outputs"][0]["canvasWidth"].clone(), s["outputs"][0]["canvasHeight"].clone()),
        (json!(1280), json!(720))
    );

    displays(&d.data_dir, &["OPERATOR"]);
    let s = d.outputs_status(Duration::from_secs(3), "projector unplugged", |s| output_ids(s).is_empty());
    assert!(s["problems"][0]["message"].as_str().unwrap().contains("not connected"));

    displays(&d.data_dir, &["OPERATOR", "PROJECTOR"]);
    d.outputs_status(Duration::from_secs(3), "projector plugged back in", |s| output_ids(s) == ["proj"]);

    std::fs::write(d.data_dir.join("outputs.json"), "{ half-saved").unwrap();
    let s = d.outputs_status(Duration::from_secs(3), "a broken config reported", |s| {
        s["problems"].as_array().is_some_and(|p| !p.is_empty())
    });
    assert_eq!(output_ids(&s), ["proj"], "a broken config file keeps the screens as they were");
}

#[cfg(feature = "video")]
mod video {
    use super::*;

    impl Daemon {
        /// Subscribes to the engine and waits for a `levels` event matching `ok`.
        fn wait_for_levels(&self, within: Duration, ok: impl Fn(&Value) -> bool) -> Value {
            let deadline = Instant::now() + within;
            while Instant::now() < deadline {
                let Ok(mut s) = TcpStream::connect(&self.addr) else {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
                let _ = s.write_all(b"{\"v\":1,\"id\":\"m\",\"ts\":1,\"command\":{\"type\":\"state.subscribe\"}}\n");
                let mut lines = BufReader::new(s).lines();
                while Instant::now() < deadline {
                    let Some(Ok(line)) = lines.next() else { break };
                    let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    if v["event"] == "levels" && ok(&v["levels"]) {
                        return v["levels"].clone();
                    }
                }
            }
            panic!("no matching levels event in {within:?}");
        }

        fn media_status(&self, within: Duration, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
            let deadline = Instant::now() + within;
            let mut last = Value::Null;
            while Instant::now() < deadline {
                if let Ok(b) = std::fs::read(self.data_dir.join("media-status.json"))
                    && let Ok(v) = serde_json::from_slice::<Value>(&b)
                {
                    if ok(&v) {
                        return v;
                    }
                    last = v;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("video never reached: {what}; last status {last}\n{}", self.log_tail());
        }
    }

    fn start_video(name: &str, media_json: &str) -> Daemon {
        let dir = test_dir(name);
        std::fs::write(dir.join("media.json"), media_json).unwrap();
        start_with(dir, 10, false, &["--video", env!("CARGO_BIN_EXE_jivvy-video")])
    }

    /// A -6 dBFS test tone (volume 0.5), give or take.
    fn is_test_tone(levels: &Value) -> bool {
        levels["peakDb"]
            .as_array()
            .is_some_and(|p| !p.is_empty() && p.iter().all(|v| (-9.0..=-3.0).contains(&v.as_f64().unwrap())))
    }

    #[test]
    fn audio_levels_reach_screens_and_come_back_after_a_video_crash() {
        let d = start_video("video-levels", r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#);
        let video = d.video_starts.recv_timeout(Duration::from_secs(10)).unwrap();
        d.wait_for_levels(Duration::from_secs(10), is_test_tone);
        let s = d.media_status(Duration::from_secs(5), "camera and microphone running", |s| {
            s["camera"]["state"] == "running"
                && s["camera"]["rate"].as_f64().unwrap_or(0.0) > 20.0
                && s["microphone"]["state"] == "running"
        });
        assert!(s["microphone"]["rate"].as_f64().unwrap() > 5.0, "about ten level reports a second: {s}");
        d.media_status(Duration::from_secs(3), "levels acknowledged by the engine", |s| s["connected"] == true);

        let killed_at = Instant::now();
        kill_hard(video);
        d.video_starts.recv_timeout(RESTORE_TARGET).expect("video restarted");
        // A restarted video process brings the program back first, then the inputs; loading
        // GStreamer's GPU and encoder plugins takes most of the ~4 s on the founder's laptop.
        d.wait_for_levels(Duration::from_secs(10), is_test_tone);
        println!("levels back {:?} after the video process was killed", killed_at.elapsed());
    }

    #[test]
    fn a_missing_camera_waits_without_stopping_the_audio_and_is_picked_up_when_it_appears() {
        let d = start_video(
            "video-missing",
            r#"{"version":1,"camera":{"use":"device","id":"no-such-path","name":"Blackmagic ATEM"},"microphone":{"use":"test"}}"#,
        );
        let s = d.media_status(Duration::from_secs(10), "camera waiting, audio running", |s| {
            s["camera"]["state"] == "waiting" && s["microphone"]["state"] == "running"
        });
        assert!(s["camera"]["detail"].as_str().unwrap().contains("Blackmagic ATEM is not connected"));
        d.wait_for_levels(Duration::from_secs(5), is_test_tone);

        // "Plugging it in": the configured source becomes available.
        std::fs::write(
            d.data_dir.join("media.json"),
            r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#,
        )
        .unwrap();
        d.media_status(Duration::from_secs(3), "camera running", |s| s["camera"]["state"] == "running");
    }

    #[test]
    fn with_test_sources_the_whole_daemon_runs_without_any_device() {
        // Names a capture card that isn't here and the default microphone: --test-sources
        // replaces both, so this runs on a machine with no camera or microphone at all.
        let dir = test_dir("test-sources");
        std::fs::write(
            dir.join("media.json"),
            r#"{"version":1,"camera":{"use":"device","id":"no-such-path","name":"Blackmagic ATEM"},"microphone":{"use":"auto"}}"#,
        )
        .unwrap();
        let d = start_with(dir, 10, false, &["--video", env!("CARGO_BIN_EXE_jivvy-video"), "--test-sources"]);
        let s = d.media_status(Duration::from_secs(15), "camera, microphone and program on test sources", |s| {
            s["camera"]["state"] == "running"
                && s["microphone"]["state"] == "running"
                && program(s)["state"] == "running"
                && program(s)["slate"] == false
        });
        assert_eq!(s["camera"]["choice"]["source"], "test", "{}", s["camera"]);
        d.wait_for_levels(Duration::from_secs(5), is_test_tone);
    }

    fn program(s: &Value) -> &Value {
        &s["program"]
    }

    #[test]
    fn the_program_runs_at_full_rate_with_lyrics_that_follow_the_slide() {
        let d = start_video("program-lyrics", r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#);
        let s = d.media_status(Duration::from_secs(15), "program running at full rate", |s| {
            program(s)["state"] == "running" && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        assert!(program(&s)["kbps"].as_f64().unwrap() > 100.0, "encoding real video: {s}");
        assert_eq!(program(&s)["slate"], false);

        d.send(json!({ "type": "slide.goto", "index": 6 }), Duration::from_secs(5));
        d.media_status(Duration::from_secs(3), "lyric layer shows slide 7", |s| {
            program(s)["overlay"] == json!(["Slide 7"])
        });
        d.send(json!({ "type": "output.black", "on": true }), Duration::from_secs(5));
        d.media_status(Duration::from_secs(3), "no lyrics while black", |s| program(s)["overlay"] == json!([]));
    }

    #[test]
    fn losing_the_camera_and_microphone_never_stops_the_program() {
        let d = start_video("program-slate", r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#);
        d.media_status(Duration::from_secs(15), "program running on the camera", |s| {
            program(s)["state"] == "running"
                && program(s)["slate"] == false
                && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        let encoder = d.media_status(Duration::from_secs(1), "status", |_| true)["program"]["encoder"].clone();

        // Both inputs gone (unplugged camera, no microphone).
        std::fs::write(
            d.data_dir.join("media.json"),
            r#"{"version":1,"camera":{"use":"device","id":"gone","name":"Unplugged camera"},"microphone":{"use":"none"}}"#,
        )
        .unwrap();
        let s = d.media_status(Duration::from_secs(5), "slate on, still at full rate", |s| {
            s["camera"]["state"] == "waiting"
                && program(s)["slate"] == true
                && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        assert_eq!(program(&s)["state"], "running");
        assert_eq!(program(&s)["encoder"], encoder, "the program itself was never restarted");
        let before = program(&s)["framesEncoded"].as_u64().unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let s = d.media_status(Duration::from_secs(1), "status", |_| true);
        let gained = program(&s)["framesEncoded"].as_u64().unwrap() - before;
        assert!(gained >= 50, "about 60 frames in 2 s on the slate, got {gained}");

        // The camera comes back: off the slate.
        std::fs::write(
            d.data_dir.join("media.json"),
            r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"none"}}"#,
        )
        .unwrap();
        d.media_status(Duration::from_secs(5), "camera back on the program", |s| program(s)["slate"] == false);
    }

    #[test]
    fn changing_the_program_size_mid_run_comes_back_clean_at_the_new_size() {
        let d = start_video("program-resize", r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#);
        d.video_starts.recv_timeout(Duration::from_secs(10)).unwrap();
        let s = d.media_status(Duration::from_secs(15), "running at 1080p", |s| {
            program(s)["state"] == "running"
                && program(s)["width"] == 1920
                && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        let before = program(&s)["framesEncoded"].as_u64().unwrap();

        std::fs::write(
            d.data_dir.join("media.json"),
            r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"},"program":{"width":1280,"height":720}}"#,
        )
        .unwrap();
        // The replacement must encode frames of its own: the old pipeline's count doesn't
        // count, and no frame from the old size may reach it (that would error it out).
        let s = d.media_status(Duration::from_secs(10), "running at 720p on the camera at full rate", |s| {
            program(s)["state"] == "running"
                && program(s)["width"] == 1280
                && program(s)["slate"] == false
                && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        assert!(
            d.video_starts.try_recv().is_err(),
            "the video process must not restart (or be killed as hung) on a resize"
        );
        assert!(program(&s)["framesEncoded"].as_u64().unwrap() > before + 25, "before {before}, now {s}");
        assert_eq!(program(&s)["detail"], "", "no encoder failed along the way: {s}");
        assert!(
            program(&s)["outFps"].as_f64().unwrap() < 40.0,
            "the rate isn't inflated by the old pipeline's frames: {s}"
        );
    }

    /// A local MediaMTX server standing in for YouTube/Facebook's RTMP ingest.
    struct MediaMtx {
        child: Option<Child>,
        config: PathBuf,
        rtmp: u16,
        api: u16,
    }

    fn mediamtx_exe() -> PathBuf {
        if let Some(p) = std::env::var_os("JIVVY_MEDIAMTX") {
            return p.into();
        }
        let exe = if cfg!(windows) { "mediamtx.exe" } else { "mediamtx" };
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tools/mediamtx").join(exe);
        assert!(p.exists(), "MediaMTX not found at {}; see apps/daemon/README.md (Streaming tests)", p.display());
        p
    }

    fn read_log(config: &std::path::Path) -> String {
        std::fs::read_to_string(config.with_extension("log")).unwrap_or_default()
    }

    impl MediaMtx {
        /// A MediaMTX on fresh ports. If one is taken by the time MediaMTX binds it, MediaMTX
        /// exits at once and is started again on others (up to 5 tries).
        fn start(dir: &std::path::Path) -> MediaMtx {
            let config = dir.join("mediamtx.yml");
            for _ in 0..5 {
                let (rtmp, api) = (free_port(), free_port());
                std::fs::write(
                    &config,
                    format!(
                        "logLevel: warn\napi: true\napiAddress: 127.0.0.1:{api}\nrtmp: true\nrtmpAddress: 127.0.0.1:{rtmp}\n\
                         rtsp: false\nhls: false\nwebrtc: false\nsrt: false\nmoq: false\nplayback: false\n\
                         paths:\n  all_others:\n"
                    ),
                )
                .unwrap();
                let mut m = MediaMtx { child: None, config: config.clone(), rtmp, api };
                if m.try_start() {
                    return m;
                }
            }
            panic!("MediaMTX could not get its ports in 5 tries; its log:\n{}", read_log(&config));
        }

        /// Starts MediaMTX again on the same ports, as a server coming back after an outage.
        fn restart(&mut self) {
            assert!(self.try_start(), "MediaMTX exited at start; its log:\n{}", read_log(&self.config));
        }

        /// Starts MediaMTX and waits for its API: false if it exited first (a port was taken).
        fn try_start(&mut self) -> bool {
            end_children_with_this_process();
            let child = std::fs::File::create(self.config.with_extension("log")).and_then(|log| {
                let err = log.try_clone()?;
                Command::new(mediamtx_exe()).arg(&self.config).stdout(log).stderr(err).spawn()
            });
            self.child = Some(child.expect("MediaMTX starts"));
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.get_paths().is_none() {
                if self.child.as_mut().is_some_and(|c| c.try_wait().is_ok_and(|s| s.is_some())) {
                    self.child = None;
                    return false;
                }
                assert!(Instant::now() < deadline, "MediaMTX API didn't come up; its log:\n{}", read_log(&self.config));
                std::thread::sleep(Duration::from_millis(50));
            }
            true
        }

        fn kill(&mut self) {
            if let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }

        fn get_paths(&self) -> Option<Vec<Value>> {
            let mut s = TcpStream::connect(("127.0.0.1", self.api)).ok()?;
            s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
            s.write_all(b"GET /v3/paths/list HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
            let mut body = String::new();
            std::io::Read::read_to_string(&mut s, &mut body).ok()?;
            let json = body.split_once("\r\n\r\n")?.1;
            serde_json::from_str::<Value>(json).ok()?["items"].as_array().cloned()
        }

        /// Bytes received on a ready path, if the stream is arriving.
        fn receiving(&self) -> Option<u64> {
            let paths = self.get_paths()?;
            paths.iter().find(|p| p["ready"] == true).and_then(|p| p["bytesReceived"].as_u64())
        }

        /// The tracks (codecs) of the stream arriving, e.g. `["H264", "MPEG-4 Audio"]`.
        fn tracks(&self) -> Vec<String> {
            let paths = self.get_paths().unwrap_or_default();
            let ready = paths.iter().find(|p| p["ready"] == true);
            ready
                .and_then(|p| p["tracks"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str())
                .map(String::from)
                .collect()
        }

        /// Waits until the stream arrives and keeps growing.
        fn wait_receiving(&self, within: Duration) {
            let deadline = Instant::now() + within;
            let mut first = None;
            while Instant::now() < deadline {
                if let Some(b) = self.receiving() {
                    match first {
                        None => first = Some(b),
                        // A new connection (e.g. the bandwidth manager trying a step up)
                        // starts the count again.
                        Some(f) if b < f => first = Some(b),
                        Some(f) if b > f + 100_000 => return,
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            panic!("MediaMTX never received a growing stream; paths {:?}", self.get_paths());
        }
    }

    impl Drop for MediaMtx {
        fn drop(&mut self) {
            self.kill();
        }
    }

    const TEST_KEY: &str = "secret-key-do-not-leak-7731";

    /// A daemon streaming test sources to `server_port`, and its video process's pid.
    fn start_streaming(name: &str, server_port: u16) -> (Daemon, u32) {
        let dir = test_dir(name);
        std::fs::write(dir.join("media.json"), r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#)
            .unwrap();
        std::fs::write(
            dir.join("stream.json"),
            format!(
                r#"{{"version":1,"destinations":[{{"id":"local","name":"Local test","url":"rtmp://127.0.0.1:{server_port}/live","key":"{TEST_KEY}"}}]}}"#
            ),
        )
        .unwrap();
        let d = start_with(dir, 10, false, &["--video", env!("CARGO_BIN_EXE_jivvy-video")]);
        let video = d.video_starts.recv_timeout(Duration::from_secs(10)).unwrap();
        (d, video)
    }

    fn stream_state(d: &Daemon) -> Value {
        d.send(json!({ "type": "state.get" }), Duration::from_secs(5))["state"]["stream"].clone()
    }

    fn wait_stream_state(d: &Daemon, want: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while stream_state(d) != want {
            assert!(
                Instant::now() < deadline,
                "stream never became {want}; status {:?}",
                std::fs::read_to_string(d.data_dir.join("media-status.json"))
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn the_stream_comes_back_by_itself_after_the_server_drops_and_the_program_never_notices() {
        let dir = test_dir("stream-drop-mtx");
        let mut server = MediaMtx::start(&dir);
        let (d, _) = start_streaming("stream-drop", server.rtmp);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        server.wait_receiving(Duration::from_secs(20));
        wait_stream_state(&d, "live", Duration::from_secs(10));
        // Sound too: the encoders shift video timestamps, so a naive rebase drops all audio.
        assert_eq!(server.tracks(), ["H264", "MPEG-4 Audio"]);

        // The internet drops.
        server.kill();
        wait_stream_state(&d, "reconnecting", Duration::from_secs(5));
        let s = d.media_status(Duration::from_secs(3), "program still at full rate", |s| {
            program(s)["state"] == "running" && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        let detail = s["stream"]["destinations"][0]["detail"].as_str().unwrap().to_string();
        assert!(!detail.is_empty() && !detail.contains("gst") && !detail.contains(TEST_KEY), "plain words: {detail}");

        // It comes back: no one touches anything.
        std::thread::sleep(Duration::from_secs(3));
        let back_at = Instant::now();
        server.restart();
        server.wait_receiving(Duration::from_secs(20));
        wait_stream_state(&d, "live", Duration::from_secs(10));
        println!("stream live again {:?} after the server returned", back_at.elapsed());
        assert!(d.video_starts.try_recv().is_err(), "the video process never restarted");
        // A dead connection isn't a slow one: the quality was never lowered for it.
        let s = d.media_status(Duration::from_secs(3), "status", |_| true);
        assert_eq!(s["stream"]["destinations"][0]["quality"], "1080p", "{}", s["stream"]);
        assert!(s["stream"]["uploadKbps"].is_null(), "no upload limit inferred: {}", s["stream"]);
    }

    #[test]
    fn a_video_crash_resumes_the_stream_and_stop_really_disconnects() {
        let dir = test_dir("stream-crash-mtx");
        let server = MediaMtx::start(&dir);
        let (d, video) = start_streaming("stream-crash", server.rtmp);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        wait_stream_state(&d, "live", Duration::from_secs(20));

        // The engine keeps "wanted", so a restarted video process resumes the stream.
        kill_hard(video);
        d.video_starts.recv_timeout(Duration::from_secs(5)).expect("video restarted");
        let back = Instant::now();
        server.wait_receiving(Duration::from_secs(25));
        wait_stream_state(&d, "live", Duration::from_secs(10));
        println!("stream live again {:?} after the video process restarted", back.elapsed());

        d.send(json!({ "type": "stream.stop" }), Duration::from_secs(5));
        wait_stream_state(&d, "off", Duration::from_secs(3));
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.receiving().is_some() {
            assert!(Instant::now() < deadline, "the server still receives after stop");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn an_unreachable_server_is_reported_in_plain_words_and_the_key_never_leaks() {
        let unused = free_port();
        let (d, _) = start_streaming("stream-unreachable", unused);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        let s = d.media_status(Duration::from_secs(15), "a few failed attempts", |s| {
            s["stream"]["destinations"][0]["reconnects"].as_u64().unwrap_or(0) >= 2
        });
        let dest = &s["stream"]["destinations"][0];
        assert_eq!(dest["state"], "reconnecting");
        assert!(dest["detail"].as_str().unwrap().contains("Can't reach"), "{dest}");
        assert_eq!(dest["server"], format!("rtmp://127.0.0.1:{unused}/live"));
        assert_eq!(stream_state(&d), "reconnecting");
        for f in ["media-status.json", "state.json", "outputs-status.json", "media-memory.json"] {
            if let Ok(text) = std::fs::read_to_string(d.data_dir.join(f)) {
                assert!(!text.contains(TEST_KEY), "{f} contains the stream key");
            }
        }
    }

    #[test]
    fn a_server_that_accepts_but_never_answers_is_never_shown_live() {
        // Accepts TCP connections and then says nothing: buffers still reach the sink.
        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in silent.incoming().flatten() {
                held.push(conn);
            }
        });
        let (d, _) = start_streaming("stream-silent", port);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(14);
        let mut reconnected = false;
        while Instant::now() < deadline {
            assert_ne!(stream_state(&d), "live", "never live when nothing reaches the server");
            if let Ok(t) = std::fs::read_to_string(d.data_dir.join("media-status.json"))
                && let Ok(v) = serde_json::from_str::<Value>(&t)
            {
                let dest = &v["stream"]["destinations"][0];
                assert_ne!(dest["state"], "live", "{dest}");
                if dest["reconnects"].as_u64().unwrap_or(0) >= 1 {
                    assert!(dest["detail"].as_str().unwrap().contains("Can't reach"), "{dest}");
                    reconnected = true;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(reconnected, "gave up on the silent server and reconnected");
    }

    /// What a fake YouTube HLS ingest received, and every rule of YouTube's ingestion guide
    /// the uploader broke.
    #[derive(Default, Debug)]
    struct Ingest {
        /// Media sequence of the last playlist.
        last_seq: Option<u64>,
        /// Every sequence number ever listed, with the segment it named.
        names: std::collections::HashMap<u64, String>,
        /// Segments received in order, with their sizes.
        segments: Vec<(String, usize)>,
        /// Segments that arrived before a playlist listed them (answered 202).
        early: usize,
        /// The newest segment's bytes, to check it plays.
        last: Vec<u8>,
        violations: Vec<String>,
    }

    /// Stands in for YouTube's HLS ingestion (`http_upload_hls`) on a local port, checking
    /// the uploader against the ingestion guide. While `up` is false it reads each request
    /// and drops the connection without an answer, like a dead internet connection.
    struct FakeYouTube {
        port: u16,
        up: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ingest: std::sync::Arc<std::sync::Mutex<Ingest>>,
    }

    impl FakeYouTube {
        fn start() -> FakeYouTube {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let up = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let ingest = std::sync::Arc::new(std::sync::Mutex::new(Ingest::default()));
            let (u, i) = (up.clone(), ingest.clone());
            std::thread::spawn(move || {
                for conn in listener.incoming().flatten() {
                    let (u, i) = (u.clone(), i.clone());
                    std::thread::spawn(move || FakeYouTube::serve(conn, &u, &i));
                }
            });
            FakeYouTube { port, up, ingest }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/http_upload_hls", self.port)
        }

        fn set_up(&self, up: bool) {
            self.up.store(up, std::sync::atomic::Ordering::SeqCst);
        }

        fn segments(&self) -> Vec<(String, usize)> {
            self.ingest.lock().unwrap().segments.clone()
        }

        /// A new broadcast on the same key: its first playlist must start at 0 again.
        fn new_broadcast(&self) {
            *self.ingest.lock().unwrap() = Ingest::default();
        }

        fn assert_no_violations(&self) {
            let i = self.ingest.lock().unwrap();
            assert!(i.violations.is_empty(), "broke YouTube's ingestion rules: {:#?}", i.violations);
        }

        /// Waits until `n` more segments arrive.
        fn wait_segments(&self, n: usize, within: Duration) {
            let start = self.segments().len();
            let deadline = Instant::now() + within;
            while self.segments().len() < start + n {
                assert!(Instant::now() < deadline, "only {} new segments in {within:?}", self.segments().len() - start);
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        fn serve(conn: TcpStream, up: &std::sync::atomic::AtomicBool, ingest: &std::sync::Mutex<Ingest>) -> Option<()> {
            let mut reader = BufReader::new(conn.try_clone().ok()?);
            let mut conn = conn;
            loop {
                let mut request = String::new();
                reader.read_line(&mut request).ok().filter(|n| *n > 0)?;
                let mut length = 0usize;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).ok().filter(|n| *n > 0)?;
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':')
                        && k.eq_ignore_ascii_case("content-length")
                    {
                        length = v.trim().parse().ok()?;
                    }
                }
                let mut body = vec![0u8; length];
                std::io::Read::read_exact(&mut reader, &mut body).ok()?;
                if !up.load(std::sync::atomic::Ordering::SeqCst) {
                    return None; // the connection dies without an answer
                }
                let status = FakeYouTube::handle(&request, &body, &mut ingest.lock().unwrap());
                conn.write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\n\r\n").as_bytes()).ok()?;
            }
        }

        /// The ingestion guide's rules, as a status code.
        fn handle(request: &str, body: &[u8], i: &mut Ingest) -> u16 {
            let mut parts = request.split_whitespace();
            let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            if method != "PUT" && method != "POST" {
                i.violations.push(format!("method {method}"));
                return 405;
            }
            let Some(query) = target.strip_prefix("/http_upload_hls?") else {
                i.violations.push(format!("path {target}"));
                return 400;
            };
            let param = |n: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{n}="))).unwrap_or("");
            if param("cid") != TEST_KEY {
                return 401;
            }
            if param("copy") != "0" {
                i.violations.push(format!("copy={}", param("copy")));
            }
            let file = param("file");
            if file.is_empty() || !file.chars().all(|c| c.is_ascii_alphanumeric() || "_/-.".contains(c)) {
                i.violations.push(format!("file name {file:?}"));
                return 400;
            }
            if file.ends_with(".m3u8") {
                let text = String::from_utf8_lossy(body);
                let tag = |t: &str| text.lines().find_map(|l| l.strip_prefix(t)).and_then(|v| v.parse::<u64>().ok());
                let (Some(seq), Some(target)) = (tag("#EXT-X-MEDIA-SEQUENCE:"), tag("#EXT-X-TARGETDURATION:")) else {
                    i.violations.push(format!("playlist without sequence or target duration: {text}"));
                    return 400;
                };
                match i.last_seq {
                    None if seq != 0 => i.violations.push(format!("first playlist starts at {seq}, not 0")),
                    Some(last) if seq < last => i.violations.push(format!("media sequence went back {last} -> {seq}")),
                    _ => {}
                }
                i.last_seq = Some(seq);
                let mut outstanding = 0;
                let mut lines = text.lines().peekable();
                let mut n = seq;
                while let Some(l) = lines.next() {
                    let Some(d) = l.strip_prefix("#EXTINF:") else { continue };
                    let d: f64 = d.trim_end_matches(',').parse().unwrap_or(99.0);
                    if d > 5.0 || d.round() as u64 > target {
                        i.violations.push(format!("segment of {d} s (target {target})"));
                    }
                    let name = lines.next().unwrap_or("").to_string();
                    if let Some(before) = i.names.get(&n)
                        && *before != name
                    {
                        i.violations.push(format!("sequence {n} was {before}, now {name}"));
                    }
                    if !i.segments.iter().any(|(s, _)| *s == name) {
                        outstanding += 1;
                    }
                    i.names.insert(n, name);
                    n += 1;
                }
                if outstanding > 5 {
                    i.violations.push(format!("{outstanding} outstanding segments listed"));
                }
                return 200;
            }
            if !file.ends_with(".ts") {
                i.violations.push(format!("file {file}"));
                return 400;
            }
            // Self-initializing MPEG-TS: whole 188-byte packets, starting with the PAT and PMT.
            let table = |p: usize| -> Option<u8> {
                let pkt = body.get(p * 188..(p + 1) * 188)?;
                // Skip the header and any adaptation field (padding), then the pointer field.
                let start = 4 + if pkt[3] & 0x20 != 0 { 1 + usize::from(pkt[4]) } else { 0 };
                pkt.get(start + 1 + usize::from(*pkt.get(start)?)).copied()
            };
            let pid =
                |p: usize| body.get(p * 188 + 1..p * 188 + 3).map(|b| (u16::from(b[0] & 0x1f) << 8) | u16::from(b[1]));
            let whole =
                body.len() >= 376 && body.len().is_multiple_of(188) && body.iter().step_by(188).all(|b| *b == 0x47);
            if !whole || pid(0) != Some(0) || table(0) != Some(0x00) || table(1) != Some(0x02) {
                let head: Vec<String> = body.iter().take(12).map(|b| format!("{b:02x}")).collect();
                i.violations.push(format!(
                    "{file} isn't a self-initializing transport stream ({} bytes; starts {})",
                    body.len(),
                    head.join(" ")
                ));
            }
            // Sent again (the answer didn't reach the client in time): harmless, PUT replaces
            // it, but it counts once.
            if !i.segments.iter().any(|(s, _)| s == file) {
                i.segments.push((file.to_string(), body.len()));
            }
            i.last = body.to_vec();
            if i.names.values().any(|n| n == file) {
                200
            } else {
                i.early += 1;
                202
            }
        }
    }

    /// Decodes a segment on its own (as YouTube must, each one self-contained): video frames
    /// and audio buffers decoded.
    fn decode(segment: &[u8], dir: &std::path::Path) -> (u64, u64) {
        use gstreamer as gst;
        use gstreamer::prelude::*;
        let path = dir.join("received.ts");
        std::fs::write(&path, segment).unwrap();
        gst::init().unwrap();
        let p = gst::parse::launch(&format!(
            "filesrc location=\"{}\" ! decodebin name=d \
             d. ! queue ! videoconvert ! fakesink name=v sync=false \
             d. ! queue ! audioconvert ! fakesink name=a sync=false",
            path.to_string_lossy().replace('\\', "/")
        ))
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let counts: Vec<std::sync::Arc<std::sync::atomic::AtomicU64>> = ["v", "a"]
            .iter()
            .map(|n| {
                let c = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                let c2 = c.clone();
                p.by_name(n).unwrap().static_pad("sink").unwrap().add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                    c2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    gst::PadProbeReturn::Ok
                });
                c
            })
            .collect();
        p.set_state(gst::State::Playing).unwrap();
        let bus = p.bus().unwrap();
        let msg =
            bus.timed_pop_filtered(gst::ClockTime::from_seconds(10), &[gst::MessageType::Eos, gst::MessageType::Error]);
        let _ = p.set_state(gst::State::Null);
        if let Some(m) = msg
            && let gst::MessageView::Error(e) = m.view()
        {
            panic!("a received segment doesn't decode: {} ({:?})", e.error(), e.debug());
        }
        let n = |i: usize| counts[i].load(std::sync::atomic::Ordering::Relaxed);
        (n(0), n(1))
    }

    /// A daemon streaming test sources to a fake YouTube HLS ingest, and its video pid.
    fn start_hls(name: &str, url: &str, key: &str) -> (Daemon, u32) {
        let dir = test_dir(name);
        std::fs::write(dir.join("media.json"), r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"}}"#)
            .unwrap();
        std::fs::write(
            dir.join("stream.json"),
            format!(
                r#"{{"version":1,"destinations":[{{"id":"youtube","name":"YouTube","url":"{url}","key":"{key}"}}]}}"#
            ),
        )
        .unwrap();
        let d = start_with(dir, 10, false, &["--video", env!("CARGO_BIN_EXE_jivvy-video")]);
        let video = d.video_starts.recv_timeout(Duration::from_secs(10)).unwrap();
        (d, video)
    }

    /// Segment indexes per session (`s<session>-<index>.ts`), to check nothing was lost.
    fn sessions(segments: &[(String, usize)]) -> Vec<(String, Vec<u32>)> {
        let mut out: Vec<(String, Vec<u32>)> = Vec::new();
        for (name, _) in segments {
            let (session, index) = name.trim_end_matches(".ts").rsplit_once('-').unwrap();
            let index: u32 = index.parse().unwrap();
            match out.iter_mut().find(|(s, _)| s == session) {
                Some((_, v)) => v.push(index),
                None => out.push((session.to_string(), vec![index])),
            }
        }
        out
    }

    fn spool_files(d: &Daemon) -> usize {
        std::fs::read_dir(d.data_dir.join("stream-spool").join("youtube"))
            .map(|r| r.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".ts")).count())
            .unwrap_or(0)
    }

    #[test]
    fn youtube_hls_keeps_every_second_through_an_outage_and_catches_up() {
        let yt = FakeYouTube::start();
        let (d, _) = start_hls("hls-outage", &yt.url(), TEST_KEY);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        wait_stream_state(&d, "live", Duration::from_secs(20));
        yt.wait_segments(2, Duration::from_secs(10));

        // The internet drops for 20 s: segments keep being cut to disk.
        yt.set_up(false);
        wait_stream_state(&d, "reconnecting", Duration::from_secs(10));
        let s = d.media_status(Duration::from_secs(3), "program still at full rate", |s| {
            program(s)["state"] == "running" && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        let detail = s["stream"]["destinations"][0]["detail"].as_str().unwrap().to_string();
        assert!(!detail.is_empty() && !detail.contains(TEST_KEY), "plain words: {detail}");
        std::thread::sleep(Duration::from_secs(17));
        assert!(spool_files(&d) >= 8, "the outage's video waits on disk ({} files)", spool_files(&d));

        // It comes back: the backlog goes out faster than real time, then it's live again.
        let back = Instant::now();
        yt.set_up(true);
        wait_stream_state(&d, "live", Duration::from_secs(20));
        println!("caught up and live {:?} after the connection returned", back.elapsed());
        assert!(back.elapsed() < Duration::from_secs(15), "caught up in {:?}", back.elapsed());
        yt.wait_segments(1, Duration::from_secs(5));
        yt.assert_no_violations();
        let sessions = sessions(&yt.segments());
        assert_eq!(sessions.len(), 1, "one segmenter run throughout: {sessions:?}");
        let indexes = &sessions[0].1;
        assert!(indexes.windows(2).all(|w| w[1] == w[0] + 1), "no segment lost or reordered: {indexes:?}");
        assert!(indexes.len() >= 14, "the outage's 20 s arrived: {} segments", indexes.len());
        assert!(spool_files(&d) <= 2, "confirmed segments are deleted ({} left)", spool_files(&d));
        // Each segment plays on its own, picture and sound: 2 s at 30 fps.
        let (frames, audio) = decode(&yt.ingest.lock().unwrap().last.clone(), &d.data_dir);
        assert!((55..=65).contains(&frames), "{frames} video frames in a segment");
        assert!(audio >= 50, "{audio} audio buffers in a segment (2 s of AAC is ~94)");
        assert!(d.video_starts.try_recv().is_err(), "the video process never restarted");
    }

    #[test]
    fn a_video_crash_carries_the_youtube_playlist_on_and_a_new_broadcast_starts_at_zero() {
        let yt = FakeYouTube::start();
        let (d, video) = start_hls("hls-crash", &yt.url(), TEST_KEY);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        wait_stream_state(&d, "live", Duration::from_secs(20));
        yt.wait_segments(3, Duration::from_secs(10));

        // Same playlist after the crash: sequence numbers keep counting up (checked by the
        // fake), segment names never repeat.
        kill_hard(video);
        d.video_starts.recv_timeout(Duration::from_secs(5)).expect("video restarted");
        let back = Instant::now();
        yt.wait_segments(3, Duration::from_secs(30));
        wait_stream_state(&d, "live", Duration::from_secs(10));
        println!("stream live again {:?} after the video process restarted", back.elapsed());
        yt.assert_no_violations();
        assert_eq!(sessions(&yt.segments()).len(), 2, "a second segmenter run after the crash");

        // Stopped, then started again: a new broadcast, from sequence 0.
        d.send(json!({ "type": "stream.stop" }), Duration::from_secs(5));
        wait_stream_state(&d, "off", Duration::from_secs(3));
        std::thread::sleep(Duration::from_secs(3));
        let after_stop = yt.segments().len();
        std::thread::sleep(Duration::from_secs(3));
        assert_eq!(yt.segments().len(), after_stop, "nothing is sent after stop");
        assert!(spool_files(&d) == 0, "stop clears what was waiting");
        yt.new_broadcast();
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        wait_stream_state(&d, "live", Duration::from_secs(20));
        yt.wait_segments(2, Duration::from_secs(10));
        yt.assert_no_violations();
    }

    #[test]
    fn a_youtube_key_that_is_not_accepted_is_reported_in_plain_words_and_never_leaks() {
        let yt = FakeYouTube::start();
        let wrong = "wrong-key-do-not-leak-4410";
        let (d, _) = start_hls("hls-badkey", &yt.url(), wrong);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        let s = d.media_status(Duration::from_secs(20), "a few rejected attempts", |s| {
            s["stream"]["destinations"][0]["reconnects"].as_u64().unwrap_or(0) >= 2
        });
        let dest = &s["stream"]["destinations"][0];
        assert_eq!(dest["state"], "reconnecting");
        assert!(dest["detail"].as_str().unwrap().contains("rejected"), "{dest}");
        assert_eq!(dest["server"], yt.url());
        assert_eq!(stream_state(&d), "reconnecting");
        assert!(yt.segments().is_empty());
        for f in ["media-status.json", "state.json", "stream-spool/youtube/queue.json"] {
            if let Ok(text) = std::fs::read_to_string(d.data_dir.join(f)) {
                assert!(!text.contains(wrong), "{f} contains the stream key");
            }
        }
    }

    /// A shared upload limit for throttling proxies: kbps, 0 for unlimited.
    #[derive(Default)]
    struct Uplink {
        kbps: std::sync::atomic::AtomicU64,
        next_free: std::sync::Mutex<Option<Instant>>,
    }

    impl Uplink {
        fn set(&self, kbps: u64) {
            self.kbps.store(kbps, std::sync::atomic::Ordering::SeqCst);
        }

        /// Waits until `n` more bytes fit under the limit.
        fn pass(&self, n: usize) {
            let kbps = self.kbps.load(std::sync::atomic::Ordering::SeqCst);
            if kbps == 0 {
                return;
            }
            let cost = Duration::from_secs_f64(n as f64 * 8.0 / (kbps as f64 * 1000.0));
            let until = {
                let mut next = self.next_free.lock().unwrap();
                let start = next.map_or(Instant::now(), |t| t.max(Instant::now()));
                *next = Some(start + cost);
                start + cost
            };
            std::thread::sleep(until.saturating_duration_since(Instant::now()));
        }
    }

    /// A TCP proxy to `target` whose upload (towards the server) goes through `uplink`,
    /// like the church's internet connection. Returns its port.
    fn throttled(uplink: std::sync::Arc<Uplink>, target: u16) -> u16 {
        // A small receive buffer, so the sender feels the limit through TCP flow control as
        // it would on a real slow link. Without it Linux's auto-tuned loopback buffers soak
        // up megabytes first and hide the congestion for tens of seconds.
        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        socket.set_recv_buffer_size(64 * 1024).unwrap();
        socket.bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into()).unwrap();
        socket.listen(16).unwrap();
        let listener: TcpListener = socket.into();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let Ok(server) = TcpStream::connect(("127.0.0.1", target)) else { continue };
                let (mut c_in, mut s_out) = (client.try_clone().unwrap(), server.try_clone().unwrap());
                let up = uplink.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = std::io::Read::read(&mut c_in, &mut buf) {
                        if n == 0 {
                            break;
                        }
                        up.pass(n);
                        if s_out.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = s_out.shutdown(std::net::Shutdown::Both);
                });
                let (mut s_in, mut c_out) = (server, client);
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut s_in, &mut c_out);
                    let _ = c_out.shutdown(std::net::Shutdown::Both);
                });
            }
        });
        port
    }

    fn destination(s: &Value, id: &str) -> Value {
        s["stream"]["destinations"].as_array().unwrap().iter().find(|d| d["id"] == id).cloned().unwrap_or(Value::Null)
    }

    /// A daemon for the bandwidth tests, streaming to `destinations` (JSON). The test
    /// picture is noise, like a camera's, so every encoder really spends its bitrate (x264
    /// squeezes the usual test pattern to almost nothing), at 720p and 3 Mbps so x264 keeps
    /// up on a CI runner. Lower qualities: 480p and 360p.
    fn start_bandwidth(name: &str, destinations: &str) -> Daemon {
        let dir = test_dir(name);
        std::fs::write(
            dir.join("media.json"),
            r#"{"version":1,"camera":{"use":"test"},"microphone":{"use":"test"},
                "program":{"width":1280,"height":720,"bitrateKbps":3000}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("stream.json"), format!(r#"{{"version":1,"destinations":[{destinations}]}}"#)).unwrap();
        let video = env!("CARGO_BIN_EXE_jivvy-video");
        let d = start_with_env(dir, 10, false, &["--video", video], &[("JIVVY_TEST_PATTERN", "snow")]);
        d.video_starts.recv_timeout(Duration::from_secs(10)).unwrap();
        d
    }

    fn rtmp_destination(id: &str, port: u16) -> String {
        format!(r#"{{"id":"{id}","name":"{id}","url":"rtmp://127.0.0.1:{port}/live","key":"{TEST_KEY}"}}"#)
    }

    fn hls_destination(id: &str, port: u16) -> String {
        format!(r#"{{"id":"{id}","name":"{id}","url":"http://127.0.0.1:{port}/http_upload_hls","key":"{TEST_KEY}"}}"#)
    }

    /// What the program really sends (kbps).
    fn program_kbps(d: &Daemon) -> f64 {
        let s = d.media_status(Duration::from_secs(20), "program running", |s| {
            program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0 && program(s)["kbps"].as_f64().unwrap_or(0.0) > 0.0
        });
        program(&s)["kbps"].as_f64().unwrap()
    }

    #[test]
    fn a_slow_upload_steps_the_stream_down_and_back_up_while_the_program_stays_full() {
        let dir = test_dir("bw-slow-mtx");
        let server = MediaMtx::start(&dir);
        let uplink = std::sync::Arc::new(Uplink::default());
        let d = start_bandwidth("bw-slow", &rtmp_destination("local", throttled(uplink.clone(), server.rtmp)));
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        wait_stream_state(&d, "live", Duration::from_secs(20));
        d.media_status(Duration::from_secs(5), "live at full quality", |s| {
            destination(s, "local")["quality"] == "720p"
        });

        // The church's upload drops to 1 Mbps.
        let full = program_kbps(&d);
        let limit = 1000.0;
        uplink.set(limit as u64);
        let slowed = Instant::now();
        let s = d.media_status(Duration::from_secs(40), "stepped down to 360p and live", |s| {
            let dest = destination(s, "local");
            dest["quality"] == "360p" && dest["state"] == "live"
        });
        println!("live at 360p {:?} after the upload dropped to {limit:.0} kbps", slowed.elapsed());
        let dest = destination(&s, "local");
        assert!(dest["detail"].as_str().unwrap().contains("Lowered to 360p"), "{dest}");
        let upload = s["stream"]["uploadKbps"].as_f64().expect("an upload estimate");
        println!("upload estimate {upload:.0} kbps on a {limit:.0} kbps link");
        // On Linux a stalling loopback socket takes nothing for whole seconds, which makes
        // the reading coarse; Windows (the church computer) measures it closely.
        if cfg!(windows) {
            assert!((limit * 0.4..=limit * 1.5).contains(&upload), "upload estimate {upload} kbps");
        } else {
            assert!(upload < full, "upload estimate {upload} kbps, below the program's {full:.0}");
        }
        assert_eq!(s["stream"]["encodes"][0]["quality"], "360p", "{}", s["stream"]);
        // The program (and so the recording) never noticed: full size, full rate, full bitrate.
        let s = d.media_status(Duration::from_secs(5), "program untouched", |s| {
            program(s)["width"] == 1280 && program(s)["outFps"].as_f64().unwrap_or(0.0) > 25.0
        });
        let kbps = program(&s)["kbps"].as_f64().unwrap();
        assert!(kbps > full * 0.6, "program bitrate untouched: {kbps:.0} kbps, was {full:.0}");
        // The server keeps getting it (a step up the manager tries and takes back meanwhile
        // reconnects, so allow for one).
        server.wait_receiving(Duration::from_secs(30));

        // The upload comes back: the stream steps back up by itself.
        uplink.set(0);
        let back = Instant::now();
        d.media_status(Duration::from_secs(60), "stepped back up", |s| {
            let dest = destination(s, "local");
            dest["quality"] != "360p" && dest["quality"] != "" && dest["state"] == "live"
        });
        println!("stepped up {:?} after the upload came back", back.elapsed());
        assert!(d.video_starts.try_recv().is_err(), "the video process never restarted");
    }

    #[test]
    fn when_two_platforms_dont_fit_the_lower_priority_one_is_paused_and_the_main_one_stays_live() {
        let dir = test_dir("bw-share-mtx");
        let server = MediaMtx::start(&dir);
        let yt = FakeYouTube::start();
        let uplink = std::sync::Arc::new(Uplink::default());
        // YouTube first: the main platform.
        let destinations = format!(
            "{},{}",
            hls_destination("youtube", throttled(uplink.clone(), yt.port)),
            rtmp_destination("facebook", throttled(uplink.clone(), server.rtmp))
        );
        let d = start_bandwidth("bw-share", &destinations);
        // 1.2 Mbps shared: 840 kbps to spend can't carry both even at 360p (~630 each).
        uplink.set(1200);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        let s = d.media_status(Duration::from_secs(90), "Facebook paused, YouTube live", |s| {
            destination(s, "facebook")["state"] == "paused" && destination(s, "youtube")["state"] == "live"
        });
        let (yt_status, fb_status) = (destination(&s, "youtube"), destination(&s, "facebook"));
        assert_eq!(yt_status["quality"], "360p", "{yt_status}");
        assert!(fb_status["detail"].as_str().unwrap().contains("Paused"), "{fb_status}");
        assert_eq!(stream_state(&d), "live", "screens: live, the paused platform aside");
        // YouTube keeps getting video, within the ingestion rules.
        yt.wait_segments(3, Duration::from_secs(20));
        yt.assert_no_violations();
    }

    #[test]
    fn youtube_on_a_link_too_slow_for_one_full_quality_segment_steps_down_and_goes_live() {
        let yt = FakeYouTube::start();
        let uplink = std::sync::Arc::new(Uplink::default());
        let port = throttled(uplink.clone(), yt.port);
        let d = start_bandwidth("bw-hls-slow", &hls_destination("youtube", port));
        // 900 kbps: a 2 s full-quality segment (3 Mbps, ~750 kB) needs ~6.7 s, more than the
        // 6 s it gets, so nothing is ever confirmed at full quality; 360p (~630 kbps) fits.
        let limit = 900.0;
        uplink.set(limit as u64);
        let started = Instant::now();
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        let s = d.media_status(Duration::from_secs(120), "YouTube live at 360p", |s| {
            let dest = destination(s, "youtube");
            dest["quality"] == "360p" && dest["state"] == "live"
        });
        println!("live at 360p {:?} after start on a {limit:.0} kbps link", started.elapsed());
        assert!(destination(&s, "youtube")["detail"].as_str().unwrap().contains("Lowered to 360p"));
        yt.wait_segments(3, Duration::from_secs(20));
        yt.assert_no_violations();
    }

    #[test]
    fn a_paused_youtube_destination_stops_uploading_at_once() {
        let dir = test_dir("bw-pause-hls-mtx");
        let server = MediaMtx::start(&dir);
        let yt = FakeYouTube::start();
        let uplink = std::sync::Arc::new(Uplink::default());
        // Facebook first this time: YouTube (HLS) is the one to pause.
        let destinations = format!(
            "{},{}",
            rtmp_destination("facebook", throttled(uplink.clone(), server.rtmp)),
            hls_destination("youtube", throttled(uplink.clone(), yt.port))
        );
        let d = start_bandwidth("bw-pause-hls", &destinations);
        uplink.set(1200);
        d.send(json!({ "type": "stream.start" }), Duration::from_secs(5));
        d.media_status(Duration::from_secs(90), "YouTube paused, Facebook live", |s| {
            destination(s, "youtube")["state"] == "paused" && destination(s, "facebook")["state"] == "live"
        });
        assert_eq!(stream_state(&d), "live");
        let paused_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        // Paused means paused: nothing is cut after the pause, so nothing cut after it ever
        // reaches YouTube (what was already on its way may still land), and nothing waits on
        // disk. Checked well inside the 20 s before the manager tries un-pausing it.
        std::thread::sleep(Duration::from_secs(10));
        for (name, _) in yt.segments() {
            let session: u128 = name.trim_start_matches('s').split('-').next().unwrap().parse().unwrap();
            assert!(session < paused_at, "{name} was cut after the pause");
        }
        assert_eq!(spool_files(&d), 0, "its backlog is skipped");
        yt.assert_no_violations();
    }
}
