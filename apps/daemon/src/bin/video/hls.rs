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
use jivvy_daemon::bandwidth::{Quality, Tier};

use crate::stream::{Link, PAUSED_DETAIL, Packet, lowered_detail, restarted, retimed, running_time, set, wanted};

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

/// The spool folder name for a destination id: anything but lowercase a-z, digits, `-`
/// and `_` is written as `~XX` (hex bytes). One-to-one, so two ids never share a folder,
/// even on a filesystem that ignores case.
fn spool_name(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => char::from(b).to_string(),
            _ => format!("~{b:02X}"),
        })
        .collect()
}

/// Only one worker per spool folder uses it at a time: a worker replaced after a config
/// change finishes before the new one starts.
fn spool_lock(name: &str) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Vec<(String, Arc<Mutex<()>>)>> = Mutex::new(Vec::new());
    let mut locks = LOCKS.lock().unwrap();
    if let Some((_, l)) = locks.iter().find(|(n, _)| n == name) {
        return l.clone();
    }
    let l = Arc::new(Mutex::new(()));
    locks.push((name.to_string(), l.clone()));
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
    link: Arc<Link>,
    data_dir: PathBuf,
) {
    let name = spool_name(&dest.id);
    let lock = spool_lock(&name);
    let _spool = lock.lock().unwrap_or_else(|p| p.into_inner());
    let spool = data_dir.join(hls::SPOOL_DIR).join(name);
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
        let (dest, feed, shared, done, status, link) =
            (dest.clone(), feed.clone(), shared.clone(), done.clone(), status.clone(), link.clone());
        std::thread::spawn(move || upload(dest, feed, shared, done, status, link))
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
        let Some(quality) = link.assigned() else {
            // Paused for bandwidth: nothing new is cut; the uploader finishes what it has.
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let result = segment(&feed, &stop, &shared, &link, quality);
        // Stepped down: a backlog at the old bitrate would take far too long through the
        // slow connection, so it's skipped (the segment on its way up finishes).
        if link.assigned().is_none_or(|q| q.tier > quality.tier) {
            let mut q = shared.queue.lock().unwrap();
            for skipped in q.skip_backlog() {
                let _ = std::fs::remove_file(shared.spool.join(&skipped.name));
            }
            shared.save(&mut q);
        }
        match result {
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

/// Cuts the program (at `quality`) into segments until the stream isn't wanted or the
/// bandwidth manager picks another quality (Ok), or something fails.
fn segment(feed: &Arc<Feed>, stop: &AtomicBool, shared: &Shared, link: &Link, quality: Quality) -> Result<(), String> {
    let sub = feed.taps.subscribe(quality.tier);
    let still_wanted = || !stop.load(Ordering::Relaxed) && wanted(feed) && link.assigned() == Some(quality);
    // Start on a keyframe (the program sends one every 2 s), with audio from then on. A
    // lower quality's encode may need a few seconds to start.
    let deadline = Instant::now() + Duration::from_secs(if quality.tier == Tier::Full { 5 } else { 10 });
    let first = loop {
        if !still_wanted() {
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
    let mut last_video = running_time(&first).map(|(t, _)| t);
    let mut opened_at: Option<u64> = None;
    let result = loop {
        if !still_wanted() {
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
                if restarted(&mut last_video, &s) {
                    break Err("the program changed (it restarted)".into());
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
                            let duration = end.saturating_sub(start) as f64 / 1e9;
                            let segment = Segment { name, duration, kbps: quality.kbps() };
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
fn put(agent: &ureq::Agent, url: &str, content_type: &str, body: &[u8], timeout: Duration) -> Outcome {
    let request = agent.put(url).header("Content-Type", content_type).config().timeout_global(Some(timeout)).build();
    match request.send(body) {
        Ok(resp) => {
            let status = resp.status().as_u16();
            // Read the (tiny) answer so the connection can be reused.
            let _ = resp.into_body().with_config().limit(64 * 1024).read_to_vec();
            hls::outcome(status)
        }
        // Connected and still sending when time ran out: too slow, not dead (a dead
        // connection fails to connect, resolve or answer instead).
        Err(ureq::Error::Timeout(ureq::Timeout::Global | ureq::Timeout::PerCall | ureq::Timeout::SendBody)) => {
            Outcome::TooSlow
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
    link: Arc<Link>,
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
    // How fast the last segment went up, and when: a direct measure of the upload speed.
    let mut speed: Option<(f64, Instant)> = None;
    let mut last_confirmed: Option<Instant> = None;
    // The last segment that didn't fit through in time, how many tries, and when.
    let mut too_slow: Option<(String, u32, Instant)> = None;

    while !done.load(Ordering::Relaxed) {
        // Status first, so it stays current while waiting.
        if wanted(&feed) {
            let (behind, waiting) = {
                let q = shared.queue.lock().unwrap();
                (q.behind(), q.waiting.len())
            };
            let elapsed = rate_from.elapsed();
            if elapsed >= Duration::from_secs(1) {
                kbps = sent as f64 * 8.0 / 1000.0 / elapsed.as_secs_f64();
                (sent, rate_from) = (0, Instant::now());
                // Congested: connected and sending, but video piles up faster than it goes
                // (a dead connection isn't congestion: that's for reconnecting, not for a
                // lower quality).
                let flowing =
                    last_error.is_none() && last_confirmed.is_some_and(|t| t.elapsed() < Duration::from_secs(10));
                // A segment that didn't fit through in time is congestion too, even if
                // nothing has gone up yet (a full-quality segment may never fit).
                let slow = too_slow.as_ref().is_some_and(|(_, _, at)| at.elapsed() < Duration::from_secs(20));
                let congested = slow || (flowing && behind > hls::LIVE_BEHIND);
                let measured = speed.filter(|(_, at)| at.elapsed() < Duration::from_secs(10)).map(|(k, _)| k);
                link.report(kbps, congested, measured);
            }
            let Some(quality) = link.assigned() else {
                // Paused for bandwidth: stop at once, so the connection goes to the platforms
                // that matter more. What was waiting is skipped (numbers stay used up).
                if waiting > 0 {
                    let mut q = shared.queue.lock().unwrap();
                    for s in q.waiting.iter() {
                        let _ = std::fs::remove_file(shared.spool.join(&s.name));
                    }
                    q.skip_waiting();
                    shared.save(&mut q);
                }
                set(&status, "paused", PAUSED_DETAIL.into(), 0.0);
                status.lock().unwrap().quality.clear();
                link.report(0.0, false, None);
                (attempt, retry_at, last_error, too_slow) = (0, None, None, None);
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            let (state, mut detail) = hls::health(confirmed_any, last_error.as_deref(), behind);
            if state == "live" {
                detail = lowered_detail(&quality);
            }
            set(&status, state, detail, kbps);
            status.lock().unwrap().quality = quality.label();
        } else {
            set(&status, "off", String::new(), 0.0);
            status.lock().unwrap().quality.clear();
            link.report(0.0, false, None);
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
        let listed = put(
            &agent,
            &dest.hls_url(hls::PLAYLIST),
            "application/vnd.apple.mpegurl",
            playlist.as_bytes(),
            hls::REQUEST_TIMEOUT,
        );
        if listed == Outcome::Confirmed && !shared.queue.lock().unwrap().playlist_accepted {
            let mut q = shared.queue.lock().unwrap();
            q.accept_playlist();
            shared.save(&mut q);
        }
        let outcome = match listed {
            // A playlist the server can't read is our mistake; retrying is all we can do.
            Outcome::Unusable(e) => Outcome::Retry(format!("playlist: {e}")),
            // A playlist is tiny: not getting it up in time isn't about the stream's bitrate.
            Outcome::TooSlow => Outcome::Retry("the playlist upload timed out".into()),
            Outcome::Confirmed => match std::fs::read(shared.spool.join(&segment.name)) {
                Ok(bytes) => {
                    let started = Instant::now();
                    let timeout = hls::segment_timeout(segment.duration);
                    let o = put(&agent, &dest.hls_url(&segment.name), "video/MP2T", &bytes, timeout);
                    if o == Outcome::TooSlow {
                        speed = Some((hls::too_slow_kbps(bytes.len(), timeout), Instant::now()));
                    }
                    if o == Outcome::Confirmed {
                        sent += (bytes.len() + playlist.len()) as u64;
                        let secs = started.elapsed().as_secs_f64().max(0.001);
                        speed = Some((bytes.len() as f64 * 8.0 / 1000.0 / secs, Instant::now()));
                        last_confirmed = Some(Instant::now());
                    }
                    o
                }
                Err(e) => Outcome::Unusable(format!("can't read the segment: {e}")),
            },
            retry => retry,
        };
        match outcome {
            Outcome::Confirmed | Outcome::Unusable(_) => {
                let mut q = shared.queue.lock().unwrap();
                // A segment the server can't use (or we can't read) is skipped, never
                // listed as if the server had it.
                let done = match &outcome {
                    Outcome::Unusable(e) => {
                        log("video", format!("stream {}: skipping {}: {}", dest.id, segment.name, dest.scrub(e)));
                        q.discard(&segment.name)
                    }
                    _ => q.confirm(&segment.name),
                };
                if done {
                    let _ = std::fs::remove_file(shared.spool.join(&segment.name));
                    shared.save(&mut q);
                }
                confirmed_any |= outcome == Outcome::Confirmed;
                (attempt, retry_at, last_error) = (0, None, None);
                if outcome == Outcome::Confirmed {
                    too_slow = None;
                }
            }
            Outcome::TooSlow => {
                // Retried straight away (the connection works, it's just slow) while the
                // bandwidth manager steps down; after a few tries this segment is skipped,
                // since lower-quality ones are coming behind it.
                let tries = match &too_slow {
                    Some((name, n, _)) if *name == segment.name => n + 1,
                    _ => 1,
                };
                status.lock().unwrap().reconnects += 1;
                last_error = Some("the upload is too slow for this stream".into());
                log("video", format!("stream {}: {} didn't go up in time (try {tries})", dest.id, segment.name));
                if hls::give_up_too_slow(segment.kbps, link.assigned().map(|q| q.kbps()), tries) {
                    let mut q = shared.queue.lock().unwrap();
                    if q.discard(&segment.name) {
                        let _ = std::fs::remove_file(shared.spool.join(&segment.name));
                        shared.save(&mut q);
                    }
                }
                too_slow = Some((segment.name.clone(), tries, Instant::now()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_folders_never_collide() {
        assert_eq!(spool_name("youtube"), "youtube");
        let ids = ["a/b", "a_b", "a~2Fb", "YouTube", "youtube", "a b", "a.b"];
        let names: std::collections::HashSet<String> = ids.iter().map(|i| spool_name(i).to_lowercase()).collect();
        assert_eq!(names.len(), ids.len(), "{names:?}");
        assert!(names.iter().all(|n| n.chars().all(|c| c.is_ascii_alphanumeric() || "-_~".contains(c))));
    }
}
