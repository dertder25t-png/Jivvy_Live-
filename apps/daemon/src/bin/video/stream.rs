//! RTMP/RTMPS destinations: each runs on its own thread with its own small pipeline, fed
//! copies of the program's encoded packets. See `jivvy_daemon::stream`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::log;
use jivvy_daemon::stream::{self, Destination, DestinationStatus, QUEUE_PACKETS, STALL, StreamConfig, reconnect_delay};

use crate::program::Feed;

/// One encoded packet from the program.
#[derive(Clone)]
pub enum Packet {
    Video(gst::Sample),
    Audio(gst::Sample),
}

struct Subscriber {
    id: u64,
    tx: SyncSender<Packet>,
    overflowed: Arc<AtomicBool>,
}

/// Hands the program's encoded packets to every connected destination without ever
/// blocking the program: a destination that can't keep up is marked and dropped.
#[derive(Default)]
pub struct Taps {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

impl Taps {
    pub fn publish(&self, packet: Packet) {
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|s| match s.tx.try_send(packet.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                s.overflowed.store(true, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    fn subscribe(self: &Arc<Self>) -> Subscription {
        let (tx, rx) = sync_channel(QUEUE_PACKETS);
        let overflowed = Arc::new(AtomicBool::new(false));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers.lock().unwrap().push(Subscriber { id, tx, overflowed: overflowed.clone() });
        Subscription { taps: self.clone(), id, rx, overflowed }
    }
}

struct Subscription {
    taps: Arc<Taps>,
    id: u64,
    rx: Receiver<Packet>,
    overflowed: Arc<AtomicBool>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.taps.subscribers.lock().unwrap().retain(|s| s.id != self.id);
    }
}

struct Worker {
    destination: Destination,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<DestinationStatus>>,
}

/// All destinations, kept in step with `stream.json`.
pub struct Streamer {
    workers: HashMap<String, Worker>,
    config: StreamConfig,
    problems: Vec<String>,
}

impl Streamer {
    pub fn new() -> Streamer {
        Streamer { workers: HashMap::new(), config: StreamConfig::default(), problems: Vec::new() }
    }

    /// Re-reads `stream.json` and starts, restarts or stops destination workers to match.
    /// Never blocks: old workers are told to stop and finish on their own threads.
    pub fn reconcile(&mut self, dir: &std::path::Path, feed: &Arc<Feed>) {
        self.problems.clear();
        match stream::load_config(dir) {
            Ok(c) => self.config = c,
            Err(e) => self.problems.push(format!("{e}; keeping the last good destinations")),
        }
        let mut wanted: HashMap<String, Destination> = HashMap::new();
        for d in self.config.destinations.iter().filter(|d| d.enabled) {
            match stream::validate(d) {
                Ok(()) => {
                    wanted.insert(d.id.clone(), d.clone());
                }
                Err(e) => self.problems.push(e),
            }
        }
        self.workers.retain(|id, w| {
            let keep = wanted.get(id) == Some(&w.destination);
            if !keep {
                w.stop.store(true, Ordering::Relaxed);
            }
            keep
        });
        for (id, d) in wanted {
            if self.workers.contains_key(&id) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let status = Arc::new(Mutex::new(DestinationStatus {
                id: d.id.clone(),
                name: d.name.clone(),
                server: d.display_server(),
                state: "off",
                detail: String::new(),
                kbps: 0.0,
                reconnects: 0,
            }));
            let (dest, f, s, st) = (d.clone(), feed.clone(), stop.clone(), status.clone());
            std::thread::spawn(move || run(dest, f, s, st));
            self.workers.insert(id, Worker { destination: d, stop, status });
        }
    }

    pub fn status(&self, wanted: bool) -> stream::StreamStatus {
        let mut destinations: Vec<DestinationStatus> =
            self.workers.values().map(|w| w.status.lock().unwrap().clone()).collect();
        destinations.sort_by(|a, b| a.id.cmp(&b.id));
        stream::StreamStatus {
            wanted,
            status: stream::overall(wanted, &destinations),
            destinations,
            problems: self.problems.clone(),
        }
    }
}

fn wanted(feed: &Feed) -> bool {
    feed.live.lock().unwrap().is_some_and(|s| s.stream_wanted)
}

/// Sets a destination's state. Never call with an argument that locks `status` itself.
fn set(status: &Mutex<DestinationStatus>, state: &'static str, detail: String, kbps: f64) {
    let mut s = status.lock().unwrap();
    (s.state, s.detail, s.kbps) = (state, detail, kbps);
}

/// One destination, for as long as it is configured: connect while the stream is wanted,
/// reconnect after any failure, stop when it isn't wanted.
fn run(dest: Destination, feed: Arc<Feed>, stop: Arc<AtomicBool>, status: Arc<Mutex<DestinationStatus>>) {
    let mut attempt = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if !wanted(&feed) {
            set(&status, "off", String::new(), 0.0);
            attempt = 0;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let state = if attempt == 0 { "connecting" } else { "reconnecting" };
        {
            // Keep the last error visible while retrying.
            let mut s = status.lock().unwrap();
            (s.state, s.kbps) = (state, 0.0);
        }
        match session(&dest, &feed, &stop, &status) {
            Ok(()) => attempt = 0,
            Err(e) => {
                attempt += 1;
                // The log keeps the technical reason (key removed); the screen gets plain words.
                log(
                    "video",
                    format!("stream {} ({}): {}; reconnecting", dest.id, dest.display_server(), dest.scrub(&e)),
                );
                set(&status, "reconnecting", stream::plain_error(&e), 0.0);
                status.lock().unwrap().reconnects += 1;
                let until = Instant::now() + reconnect_delay(attempt);
                while Instant::now() < until && !stop.load(Ordering::Relaxed) && wanted(&feed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    set(&status, "off", String::new(), 0.0);
}

/// One connection, from the first keyframe until it fails (Err) or the stream is no longer
/// wanted (Ok).
fn session(
    dest: &Destination,
    feed: &Arc<Feed>,
    stop: &AtomicBool,
    status: &Mutex<DestinationStatus>,
) -> Result<(), String> {
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
    let base = first.buffer().and_then(|b| b.pts()).unwrap_or(gst::ClockTime::ZERO);
    let video_caps = first.caps_owned().ok_or("no video caps")?;

    let pipeline = gst::parse::launch(
        // The muxer's pads are named: the appsrcs get their caps only once packets arrive.
        "flvmux name=mux streamable=true ! rtmp2sink name=out async-connect=true timeout=5 \
         appsrc name=v is-live=true format=time leaky-type=downstream max-time=3000000000 ! queue ! mux.video \
         appsrc name=a is-live=true format=time leaky-type=downstream max-time=3000000000 ! queue ! mux.audio",
    )
    .map_err(|e| e.to_string())?
    .downcast::<gst::Pipeline>()
    .map_err(|_| "not a pipeline")?;
    let get = |n: &str| pipeline.by_name(n).ok_or(format!("no element {n}"));
    let vsrc = get("v")?.downcast::<gst_app::AppSrc>().map_err(|_| "v is not an appsrc")?;
    let asrc = get("a")?.downcast::<gst_app::AppSrc>().map_err(|_| "a is not an appsrc")?;
    let out = get("out")?;
    // The key goes straight to the sink, never into a pipeline description or a log line.
    out.set_property("location", dest.publish_url());
    vsrc.set_caps(Some(&video_caps));
    let sent = Arc::new(AtomicU64::new(0));
    {
        let sent = sent.clone();
        out.static_pad("sink").ok_or("rtmp2sink has no pad")?.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(b) = info.buffer() {
                sent.fetch_add(b.size() as u64, Ordering::Relaxed);
            }
            gst::PadProbeReturn::Ok
        });
    }
    let stop_pipeline = |p: &gst::Pipeline| {
        let _ = p.set_state(gst::State::Null);
    };
    pipeline.set_state(gst::State::Playing).map_err(|_| "the stream pipeline would not start")?;

    let rebase = |b: &gst::BufferRef| b.pts().and_then(|p| p.checked_sub(base));
    let push = |src: &gst_app::AppSrc, sample: &gst::Sample| {
        let Some(buf) = sample.buffer() else { return };
        let Some(pts) = rebase(buf) else { return }; // from before our first keyframe
        let mut b = buf.copy(); // shares the data; new timestamps only
        if let Some(m) = b.get_mut() {
            m.set_pts(pts);
            m.set_dts(buf.dts().and_then(|d| d.checked_sub(base)).or(Some(pts)));
        }
        let _ = src.push_buffer(b);
    };
    push(&vsrc, &first);

    let mut audio_caps_set = false;
    let (mut last_sent, mut last_progress, mut last_rate_at) = (0u64, Instant::now(), Instant::now());
    let mut rate_bytes = 0u64;
    let mut live_since: Option<Instant> = None;
    let result = loop {
        if stop.load(Ordering::Relaxed) || !wanted(feed) {
            break Ok(());
        }
        if sub.overflowed.load(Ordering::Relaxed) {
            break Err("the connection couldn't keep up".into());
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
        if let Some(bus) = pipeline.bus() {
            let mut error = None;
            while let Some(msg) = bus.pop() {
                // The first error is the cause (e.g. the sink's "connection refused");
                // elements upstream then report a generic "internal data stream error".
                if let gst::MessageView::Error(e) = msg.view()
                    && error.is_none()
                {
                    let debug = e.debug().map(|d| format!(" ({d})")).unwrap_or_default();
                    error = Some(format!("{}{debug}", e.error()));
                }
            }
            if let Some(e) = error {
                break Err(e);
            }
        }
        let now = Instant::now();
        let total = sent.load(Ordering::Relaxed);
        if total > last_sent {
            (last_sent, last_progress) = (total, now);
            live_since.get_or_insert(now);
        } else if now.duration_since(last_progress) > STALL {
            break Err(format!("no data reached the server for {} s", STALL.as_secs()));
        }
        if now.duration_since(last_rate_at) >= Duration::from_secs(1) {
            let kbps = (total - rate_bytes) as f64 * 8.0 / 1000.0 / now.duration_since(last_rate_at).as_secs_f64();
            (rate_bytes, last_rate_at) = (total, now);
            let mut s = status.lock().unwrap();
            // Live once data has kept flowing for a second without an error.
            if live_since.is_some_and(|t| now.duration_since(t) >= Duration::from_secs(1)) {
                s.state = "live";
                s.detail.clear();
            }
            s.kbps = kbps;
        }
    };
    stop_pipeline(&pipeline);
    result
}
