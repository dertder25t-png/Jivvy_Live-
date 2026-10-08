//! YouTube HLS segment upload: a segmenter cuts the program's encoded packets into
//! MPEG-TS files on disk, and an uploader sends them in order, retrying until YouTube
//! confirms each one. The two run on separate threads, so a network failure never stops
//! the cutting. See `jivvy_daemon::hls`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::hls::{self, Outcome, Queue, Segment};
use jivvy_daemon::stream::{Destination, DestinationStatus, reconnect_delay};
use jivvy_daemon::{log, now_ms};

use crate::program::Feed;
use crate::stream::{Packet, retimed, running_time, set, wanted};

/// One destination's queue, shared by its segmenter and uploader.
struct Shared {
    queue: Mutex<Queue>,
    /// Signalled when a segment is cut.
    cut: Condvar,
    spool: PathBuf,
}

impl Shared {
    fn save(&self, q: &mut Queue) {
        if let Err(e) = q.save(&self.spool, now_ms()) {
            log("video", format!("could not save the upload queue: {e}"));
        }
    }

    /// Deletes segment files that aren't waiting (half-cut ones from a stopped segmenter or
    /// a crash, and skipped ones).
    fn clean(&self, q: &Queue) {
        let Ok(entries) = std::fs::read_dir(&self.spool) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".ts") && !q.waiting.iter().any(|s| s.name == name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Only one worker per destination uses its spool at a time: a worker replaced after a
/// config change finishes before the new one starts.
fn spool_lock(id: &str) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Vec<(String, Arc<Mutex<()>>)>> = Mutex::new(Vec::new());
    let mut locks = LOCKS.lock().unwrap();
    if let Some((_, l)) = locks.iter().find(|(i, _)| i == id) {
        return l.clone();
    }
    let l = Arc::new(Mutex::new(()));
    locks.push((id.to_string(), l.clone()));
    l
}

/// The engine's "wanted" flag: None until the engine has said (right after a restart).
fn wanted_known(feed: &Feed) -> Option<bool> {
    feed.live.lock().unwrap().map(|s| s.stream_wanted)
}

/// One HLS destination, for as long as it is configured.
pub fn run(
    dest: Destination,
    feed: Arc<Feed>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<DestinationStatus>>,
    data_dir: PathBuf,
) {
    let lock = spool_lock(&dest.id);
    let _spool = lock.lock().unwrap_or_else(|p| p.into_inner());
    let safe_id: String =
        dest.id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let spool = data_dir.join(hls::SPOOL_DIR).join(safe_id);
    while let Err(e) = std::fs::create_dir_all(&spool) {
        log("video", format!("stream {}: can't create {}: {e}", dest.id, spool.display()));
        set(&status, "reconnecting", "Can't save the stream to disk. Check the disk space.".into(), 0.0);
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if stop.load(Ordering::Relaxed) {
                set(&status, "off", String::new(), 0.0);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    // Picks up where a crashed process left off: same playlist, same backlog.
    let queue = Queue::load(&spool, hls::fingerprint(&dest), now_ms());
    let shared = Arc::new(Shared { queue: Mutex::new(queue), cut: Condvar::new(), spool });
    {
        let q = shared.queue.lock().unwrap();
        shared.clean(&q);
    }
    let done = Arc::new(AtomicBool::new(false));
    let uploader = {
        let (dest, feed, shared, done, status) =
            (dest.clone(), feed.clone(), shared.clone(), done.clone(), status.clone());
        std::thread::spawn(move || upload(dest, feed, shared, done, status))
    };

    let mut attempt = 0u32;
    let mut reset = false;
    while !stop.load(Ordering::Relaxed) {
        match wanted_known(&feed) {
            None => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Some(false) => {
                // Stopped on purpose: the next start is a new broadcast, from sequence 0.
                if !reset {
                    let mut q = shared.queue.lock().unwrap();
                    *q = Queue::new(q.fingerprint);
                    shared.save(&mut q);
                    shared.clean(&q);
                    reset = true;
                }
                attempt = 0;
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Some(true) => reset = false,
        }
        match segment(&feed, &stop, &shared) {
            Ok(()) => attempt = 0,
            Err(e) => {
                attempt += 1;
                log("video", format!("stream {}: segmenter: {}; restarting it", dest.id, dest.scrub(&e)));
                let until = Instant::now() + reconnect_delay(attempt);
                while Instant::now() < until && !stop.load(Ordering::Relaxed) && wanted(&feed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        let q = shared.queue.lock().unwrap();
        shared.clean(&q);
    }
    done.store(true, Ordering::Relaxed);
    shared.cut.notify_all();
    let _ = uploader.join();
    set(&status, "off", String::new(), 0.0);
}

/// Cuts the program into segments until the stream isn't wanted (Ok) or something fails.
fn segment(feed: &Arc<Feed>, stop: &AtomicBool, shared: &Shared) -> Result<(), String> {
    let sub = feed.taps.subscribe();
    // Start on a keyframe (the program sends one every 2 s), with audio from then on.
    let deadline = Instant::now() + Duration::from_secs(5);
    let first = loop {
        if stop.load(Ordering::Relaxed) || !wanted(feed) {
            return Ok(());
        }
        match sub.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Packet::Video(s)) if s.buffer().is_some_and(|b| !b.flags().contains(gst::BufferFlags::DELTA_UNIT)) => {
                break s;
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) if Instant::now() > deadline => {
                return Err("the program isn't producing video".into());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("program taps closed".into()),
        }
    };
    let video_caps = first.caps_owned().ok_or("no video caps")?;
    // Timestamps carry on from the last segment cut, across restarts of this segmenter
    // and of the whole process, so the server sees one continuous stream.
    let base = running_time(&first).ok_or("the first keyframe has no timestamp")?.0;
    let offset = gst::ClockTime::from_nseconds(shared.queue.lock().unwrap().end_ns);

    // Not live: the muxer must never wait on the clock (timestamps continue from an
    // earlier run, far from this pipeline's clock). Each segment is self-contained: a
    // fresh muxer per file, so it starts with the PAT and PMT, and SPS/PPS on every keyframe.
    let pipeline = gst::parse::launch(
        "appsrc name=v format=time ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au \
           ! queue ! split.video \
         appsrc name=a format=time ! aacparse ! queue ! split.audio_0 \
         splitmuxsink name=split muxer-factory=mpegtsmux send-keyframe-requests=false",
    )
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "not a pipeline")?;
    let get = |n: &str| pipeline.by_name(n).ok_or(format!("no element {n}"));
    let vsrc = get("v")?.downcast::<gst_app::AppSrc>().map_err(|_| "v is not an appsrc")?;
    let asrc = get("a")?.downcast::<gst_app::AppSrc>().map_err(|_| "a is not an appsrc")?;
    let split = get("split")?;
    let session = now_ms();
    let location = shared.spool.join(format!("s{session}-%05d.ts"));
    split.set_property("location", location.to_string_lossy().replace('\\', "/"));
    split.set_property("max-size-time", hls::SEGMENT_MAX.as_nanos() as u64);
    let sink = gst::ElementFactory::make("filesink")
        .property("sync", false)
        .property("async", false)
        .build()
        .map_err(|_| "GStreamer element filesink is missing")?;
    split.set_property("sink", &sink);
    vsrc.set_caps(Some(&video_caps));
    pipeline.set_state(gst::State::Playing).map_err(|_| "the segmenter would not start")?;

    let push = |src: &gst_app::AppSrc, sample: &gst::Sample| {
        if let Some(b) = retimed(sample, base, offset) {
            let _ = src.push_buffer(b);
        }
    };
    push(&vsrc, &first);

    let mut audio_caps_set = false;
    let mut opened_at: Option<u64> = None;
    let result = loop {
        if stop.load(Ordering::Relaxed) || !wanted(feed) {
            break Ok(());
        }
        if sub.overflowed.load(Ordering::Relaxed) {
            break Err("the segmenter couldn't keep up (is the disk very slow?)".into());
        }
        match sub.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Packet::Video(s)) => {
                if s.caps().is_some_and(|c| !c.is_strictly_equal(&video_caps)) {
                    break Err("the program changed (size or encoder)".into());
                }
                push(&vsrc, &s);
            }
            Ok(Packet::Audio(s)) => {
                if !audio_caps_set {
                    asrc.set_caps(s.caps_owned().as_ref());
                    audio_caps_set = true;
                }
                push(&asrc, &s);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Err("program taps closed".into()),
        }
        let Some(bus) = pipeline.bus() else { continue };
        let mut error = None;
        while let Some(msg) = bus.pop() {
            match msg.view() {
                gst::MessageView::Error(e) if error.is_none() => {
                    let debug = e.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                    error = Some(format!("{}{debug}", e.error()));
                }
                gst::MessageView::Element(e) => {
                    let Some(s) = e.structure() else { continue };
                    let running = s.get::<u64>("running-time").ok();
                    match s.name().as_str() {
                        "splitmuxsink-fragment-opened" => opened_at = running,
                        "splitmuxsink-fragment-closed" => {
                            let (Some(start), Some(end)) = (opened_at.take(), running) else { continue };
                            let Ok(path) = s.get::<String>("location") else { continue };
                            let Some(name) = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned())
                            else {
                                continue;
                            };
                            let segment = Segment { name, duration: end.saturating_sub(start) as f64 / 1e9 };
                            let mut q = shared.queue.lock().unwrap();
                            for skipped in q.push(segment, end) {
                                let _ = std::fs::remove_file(shared.spool.join(&skipped.name));
                            }
                            shared.save(&mut q);
                            shared.cut.notify_all();
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if let Some(e) = error {
            break Err(e);
        }
    };
    // The segment being cut is unfinished; it's deleted with the other leftovers.
    let _ = pipeline.set_state(gst::State::Null);
    result
}

/// Uploads one file. The URL holds the key: it goes only to the HTTP client.
fn put(agent: &ureq::Agent, url: &str, content_type: &str, body: &[u8]) -> Outcome {
    match agent.put(url).header("Content-Type", content_type).send(body) {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // Read the (tiny) answer so the connection can be reused.
            let _ = resp.into_body().with_config().limit(64 * 1024).read_to_vec();
            hls::outcome(status)
        }
        Err(e) => Outcome::Retry(e.to_string()),
    }
}

/// Sends waiting segments in order, each after a playlist listing it, until `done`.
fn upload(
    dest: Destination,
    feed: Arc<Feed>,
    shared: Arc<Shared>,
    done: Arc<AtomicBool>,
    status: Arc<Mutex<DestinationStatus>>,
) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(hls::REQUEST_TIMEOUT))
        .user_agent(format!("Jivvy / Jivvy Live / {}", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut attempt = 0u32;
    let mut retry_at: Option<Instant> = None;
    let mut confirmed_any = false;
    let mut last_error: Option<String> = None;
    let (mut sent, mut rate_from, mut kbps) = (0u64, Instant::now(), 0.0);

    while !done.load(Ordering::Relaxed) {
        // Status first, so it stays current while waiting.
        if wanted(&feed) {
            let behind = shared.queue.lock().unwrap().behind();
            let (state, detail) = hls::health(confirmed_any, last_error.as_deref(), behind);
            let elapsed = rate_from.elapsed();
            if elapsed >= Duration::from_secs(1) {
                kbps = sent as f64 * 8.0 / 1000.0 / elapsed.as_secs_f64();
                (sent, rate_from) = (0, Instant::now());
            }
            set(&status, state, detail, kbps);
        } else {
            set(&status, "off", String::new(), 0.0);
            (attempt, retry_at, confirmed_any, last_error) = (0, None, false, None);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if retry_at.is_some_and(|t| Instant::now() < t) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let (playlist, segment) = {
            let mut q = shared.queue.lock().unwrap();
            let Some(segment) = q.next().cloned() else {
                let _ = shared.cut.wait_timeout(q, Duration::from_millis(250));
                continue;
            };
            let playlist = q.playlist();
            shared.save(&mut q);
            (playlist, segment)
        };
        let outcome =
            match put(&agent, &dest.hls_url(hls::PLAYLIST), "application/vnd.apple.mpegurl", playlist.as_bytes()) {
                // A playlist the server can't read is our mistake; retrying is all we can do.
                Outcome::Unusable(e) => Outcome::Retry(format!("playlist: {e}")),
                Outcome::Confirmed => match std::fs::read(shared.spool.join(&segment.name)) {
                    Ok(bytes) => {
                        let o = put(&agent, &dest.hls_url(&segment.name), "video/MP2T", &bytes);
                        if o == Outcome::Confirmed {
                            sent += (bytes.len() + playlist.len()) as u64;
                        }
                        o
                    }
                    Err(e) => Outcome::Unusable(format!("can't read the segment: {e}")),
                },
                retry => retry,
            };
        match outcome {
            Outcome::Confirmed | Outcome::Unusable(_) => {
                if let Outcome::Unusable(e) = &outcome {
                    log("video", format!("stream {}: skipping {}: {}", dest.id, segment.name, dest.scrub(e)));
                }
                let mut q = shared.queue.lock().unwrap();
                if q.confirm(&segment.name) {
                    let _ = std::fs::remove_file(shared.spool.join(&segment.name));
                    shared.save(&mut q);
                }
                confirmed_any |= outcome == Outcome::Confirmed;
                (attempt, retry_at, last_error) = (0, None, None);
            }
            Outcome::Retry(e) => {
                attempt += 1;
                let e = dest.scrub(&e);
                log("video", format!("stream {} ({}): {e}; retrying", dest.id, dest.display_server()));
                status.lock().unwrap().reconnects += 1;
                retry_at = Some(Instant::now() + reconnect_delay(attempt));
                last_error = Some(e);
            }
        }
    }
}
