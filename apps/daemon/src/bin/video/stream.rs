//! Stream destinations: each runs on its own thread with its own small pipeline, fed
//! copies of the program's encoded packets. RTMP/RTMPS here (see `jivvy_daemon::stream`);
//! YouTube's HLS segment upload in `hls.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use jivvy_daemon::bandwidth::{self, Manager, Quality, Report, Tier};
use jivvy_daemon::log;
use jivvy_daemon::program::ProgramConfig;
use jivvy_daemon::stream::{
    self, Delivery, DeliveryTracker, Destination, DestinationStatus, QUEUE_PACKETS, RtmpStats, StreamConfig,
    reconnect_delay,
};

use crate::program::Feed;

/// One encoded packet from the program.
#[derive(Clone)]
pub enum Packet {
    Video(gst::Sample),
    Audio(gst::Sample),
}

struct Subscriber {
    id: u64,
    /// Which quality's video it takes (audio is the same for all).
    tier: Tier,
    tx: SyncSender<Packet>,
    overflowed: Arc<AtomicBool>,
}

/// Hands encoded packets to every connected destination without ever blocking the
/// program: a destination that can't keep up is marked and dropped.
#[derive(Default)]
pub struct Taps {
    subscribers: Mutex<Vec<Subscriber>>,
    next_id: AtomicU64,
}

impl Taps {
    /// From the program: its video for full-quality destinations, its audio for all.
    pub fn publish(&self, packet: Packet) {
        let video = matches!(packet, Packet::Video(_));
        self.send(|s| !video || s.tier == Tier::Full, packet);
    }

    /// Video from a lower-quality encode.
    pub fn publish_tier(&self, tier: Tier, sample: gst::Sample) {
        self.send(|s| s.tier == tier, Packet::Video(sample));
    }

    fn send(&self, to: impl Fn(&Subscriber) -> bool, packet: Packet) {
        let mut subs = self.subscribers.lock().unwrap();
        subs.retain(|s| {
            if !to(s) {
                return true;
            }
            match s.tx.try_send(packet.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    s.overflowed.store(true, Ordering::Relaxed);
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// The qualities destinations are taking right now.
    pub fn tiers_used(&self) -> HashSet<Tier> {
        self.subscribers.lock().unwrap().iter().map(|s| s.tier).collect()
    }

    pub(crate) fn subscribe(self: &Arc<Self>, tier: Tier) -> Subscription {
        let (tx, rx) = sync_channel(QUEUE_PACKETS);
        let overflowed = Arc::new(AtomicBool::new(false));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers.lock().unwrap().push(Subscriber { id, tier, tx, overflowed: overflowed.clone() });
        Subscription { taps: self.clone(), id, rx, overflowed }
    }
}

/// Between the bandwidth manager and one destination's worker.
pub(crate) struct Link {
    /// What to send: a quality, or None (paused for bandwidth).
    assigned: Mutex<Option<Quality>>,
    /// The worker's latest measurements.
    report: Mutex<Report>,
}

impl Link {
    fn new(id: &str, full: Quality) -> Link {
        Link { assigned: Mutex::new(Some(full)), report: Mutex::new(Report { id: id.into(), ..Report::default() }) }
    }

    pub(crate) fn assigned(&self) -> Option<Quality> {
        *self.assigned.lock().unwrap()
    }

    /// Called about once a second by the worker. Never call while holding `status`.
    pub(crate) fn report(&self, sent_kbps: f64, congested: bool, capacity_kbps: Option<f64>) {
        let mut r = self.report.lock().unwrap();
        (r.sent_kbps, r.congested, r.capacity_kbps) = (sent_kbps, congested, capacity_kbps);
    }

    fn clear(&self) {
        self.report(0.0, false, None);
    }
}

/// What a destination shows while live at `quality`.
pub(crate) fn lowered_detail(quality: &Quality) -> String {
    if quality.tier == Tier::Full {
        String::new()
    } else {
        format!("Lowered to {}: the internet upload is slow. It goes back up by itself.", quality.label())
    }
}

pub(crate) const PAUSED_DETAIL: &str =
    "Paused: the internet upload can't carry this platform as well. It resumes by itself.";

pub(crate) struct Subscription {
    taps: Arc<Taps>,
    id: u64,
    pub(crate) rx: Receiver<Packet>,
    pub(crate) overflowed: Arc<AtomicBool>,
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
    link: Arc<Link>,
}

/// All destinations, kept in step with `stream.json`, and the bandwidth manager fitting
/// them into the upload.
pub struct Streamer {
    workers: HashMap<String, Worker>,
    config: StreamConfig,
    problems: Vec<String>,
    manager: Manager,
    ladder: Vec<Quality>,
}

impl Streamer {
    pub fn new() -> Streamer {
        Streamer {
            workers: HashMap::new(),
            config: StreamConfig::default(),
            problems: Vec::new(),
            manager: Manager::new(Instant::now()),
            ladder: Vec::new(),
        }
    }

    /// The qualities on offer for the current program, best first.
    pub fn ladder(&self) -> &[Quality] {
        &self.ladder
    }

    /// Once a second: re-reads `stream.json`, starts, restarts or stops destination workers
    /// to match, and lets the bandwidth manager set each one's quality. Never blocks: old
    /// workers are told to stop and finish on their own threads.
    pub fn reconcile(&mut self, dir: &std::path::Path, feed: &Arc<Feed>, program: &ProgramConfig) {
        self.ladder = bandwidth::ladder(program);
        self.problems.clear();
        match stream::load_config(dir) {
            Ok(c) => self.config = c,
            Err(e) => self.problems.push(format!("{e}; keeping the last good destinations")),
        }
        let mut enabled: HashMap<String, Destination> = HashMap::new();
        // Priority order: the first destination in stream.json is the main platform.
        let mut order: Vec<String> = Vec::new();
        for d in self.config.destinations.iter().filter(|d| d.enabled) {
            match stream::validate(d) {
                Ok(()) if !enabled.contains_key(&d.id) => {
                    enabled.insert(d.id.clone(), d.clone());
                    order.push(d.id.clone());
                }
                Ok(()) => self.problems.push(format!("{}: another destination has the same id", d.id)),
                Err(e) => self.problems.push(e),
            }
        }
        self.workers.retain(|id, w| {
            let keep = enabled.get(id) == Some(&w.destination);
            if !keep {
                w.stop.store(true, Ordering::Relaxed);
            }
            keep
        });
        for (id, d) in enabled {
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
                quality: String::new(),
            }));
            let link = Arc::new(Link::new(&id, self.ladder[0]));
            let (dest, f, s, st, l) = (d.clone(), feed.clone(), stop.clone(), status.clone(), link.clone());
            if d.is_hls() {
                let dir = dir.to_path_buf();
                std::thread::spawn(move || crate::hls::run(dest, f, s, st, l, dir));
            } else {
                std::thread::spawn(move || run(dest, f, s, st, l));
            }
            self.workers.insert(id, Worker { destination: d, stop, status, link });
        }

        let reports: Vec<Report> =
            order.iter().filter_map(|id| self.workers.get(id)).map(|w| w.link.report.lock().unwrap().clone()).collect();
        let assignments = self.manager.update(&self.ladder, &order, &reports, wanted(feed), Instant::now());
        for (id, a) in assignments {
            if let Some(w) = self.workers.get(&id) {
                let quality = a.and_then(|t| self.ladder.iter().find(|q| q.tier == t).copied());
                *w.link.assigned.lock().unwrap() = quality;
            }
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
            upload_kbps: self.manager.capacity_kbps(),
            encodes: Vec::new(),
        }
    }
}

pub(crate) fn wanted(feed: &Feed) -> bool {
    feed.live.lock().unwrap().is_some_and(|s| s.stream_wanted)
}

/// Sets a destination's state. Never call with an argument that locks `status` itself.
pub(crate) fn set(status: &Mutex<DestinationStatus>, state: &'static str, detail: String, kbps: f64) {
    let mut s = status.lock().unwrap();
    (s.state, s.detail, s.kbps) = (state, detail, kbps);
}

/// One destination, for as long as it is configured: connect while the stream is wanted,
/// reconnect after any failure, stop when it isn't wanted.
fn run(
    dest: Destination,
    feed: Arc<Feed>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<DestinationStatus>>,
    link: Arc<Link>,
) {
    let mut attempt = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if !wanted(&feed) {
            set(&status, "off", String::new(), 0.0);
            status.lock().unwrap().quality.clear();
            link.clear();
            attempt = 0;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let Some(quality) = link.assigned() else {
            set(&status, "paused", PAUSED_DETAIL.into(), 0.0);
            status.lock().unwrap().quality.clear();
            link.clear();
            attempt = 0;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let state = if attempt == 0 { "connecting" } else { "reconnecting" };
        {
            // Keep the last error visible while retrying.
            let mut s = status.lock().unwrap();
            (s.state, s.kbps) = (state, 0.0);
        }
        status.lock().unwrap().quality = quality.label();
        match session(&dest, &feed, &stop, &status, &link, quality) {
            Ok(()) => attempt = 0,
            Err(e) => {
                attempt += 1;
                link.clear(); // a dead connection isn't congestion
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

/// A packet's timestamps (presentation, decode) as running time: the one clock video and
/// audio share. Encoders may shift their own timestamps (Media Foundation's by 1000 hours,
/// to keep decode times positive) and correct for it in their segment, so raw timestamps
/// of video and audio can't be compared; running times can.
pub(crate) fn running_time(sample: &gst::Sample) -> Option<(gst::ClockTime, gst::ClockTime)> {
    let buf = sample.buffer()?;
    let pts = buf.pts()?;
    let rt = sample.segment()?.downcast_ref::<gst::ClockTime>()?.to_running_time(pts)?;
    // The decode time moves by the same amount; without one it's the presentation time.
    let dts = buf.dts().and_then(|d| (d + rt).checked_sub(pts)).unwrap_or(rt);
    Some((rt, dts))
}

/// True when the program was rebuilt under us (e.g. after an encoder failure) with the
/// same caps: its timestamps start again from zero, so video jumps back in time. Packets
/// from then on can't be timed against the old start, so the destination starts over.
pub(crate) fn restarted(last: &mut Option<gst::ClockTime>, sample: &gst::Sample) -> bool {
    let Some((now, _)) = running_time(sample) else { return false };
    let back = last.is_some_and(|l| now + gst::ClockTime::from_seconds(1) < l);
    *last = Some(now);
    back
}

/// The packet's buffer (sharing its data) with timestamps counted from `base` (the first
/// keyframe's running time) and carried on from `offset`. None for packets from before
/// `base`.
pub(crate) fn retimed(sample: &gst::Sample, base: gst::ClockTime, offset: gst::ClockTime) -> Option<gst::Buffer> {
    let (pts, dts) = running_time(sample)?;
    let pts = pts.checked_sub(base)? + offset;
    let dts = dts.checked_sub(base).map_or(pts, |d| d + offset);
    let mut b = sample.buffer()?.copy();
    let m = b.get_mut()?;
    m.set_pts(pts);
    m.set_dts(dts);
    Some(b)
}

/// The RTMP sink's connection counters.
fn rtmp_stats(sink: &gst::Element) -> RtmpStats {
    let Ok(st) = sink.property_value("stats").get::<gst::Structure>() else { return RtmpStats::default() };
    let n = |f: &str| st.get::<u64>(f).unwrap_or(0);
    RtmpStats {
        in_bytes: n("in-bytes-total"),
        out_bytes: n("out-bytes-total"),
        acked: n("out-bytes-acked"),
        window: st.get::<u32>("in-window-ack-size").map(u64::from).unwrap_or(0),
    }
}

/// One connection at one quality, from the first keyframe until it fails (Err), or the
/// stream is no longer wanted or the bandwidth manager picks another quality (Ok).
fn session(
    dest: &Destination,
    feed: &Arc<Feed>,
    stop: &AtomicBool,
    status: &Mutex<DestinationStatus>,
    link: &Link,
    quality: Quality,
) -> Result<(), String> {
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
    let base = running_time(&first).ok_or("the first keyframe has no timestamp")?.0;
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
    let stop_pipeline = |p: &gst::Pipeline| {
        let _ = p.set_state(gst::State::Null);
    };
    pipeline.set_state(gst::State::Playing).map_err(|_| "the stream pipeline would not start")?;

    // Bytes handed to the pipeline: what the socket hasn't taken yet is the backlog. The
    // RTMP sink keeps whatever it can't send, so this is where a slow upload shows.
    let pushed = std::cell::Cell::new(0u64);
    let push = |src: &gst_app::AppSrc, sample: &gst::Sample| {
        if let Some(b) = retimed(sample, base, gst::ClockTime::ZERO) {
            pushed.set(pushed.get() + b.size() as u64);
            let _ = src.push_buffer(b);
        }
    };
    push(&vsrc, &first);
    let mut last_pushed = 0u64;

    let mut audio_caps_set = false;
    let mut last_video = running_time(&first).map(|(t, _)| t);
    // Delivery is judged from the sink's connection counters, never from buffers reaching
    // the sink (those are accepted before the connection even exists).
    let mut tracker = DeliveryTracker::new(Instant::now());
    let (mut rate_bytes, mut last_rate_at) = (0u64, Instant::now());
    let mut connected_at: Option<Instant> = None;
    let mut backlog_base: Option<u64> = None;
    let result = loop {
        if !still_wanted() {
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
        let st = rtmp_stats(&out);
        let delivery = tracker.update(st, now);
        if let Delivery::Failed(why) = delivery {
            break Err(why.to_string());
        }
        if now.duration_since(last_rate_at) >= Duration::from_secs(1) {
            let kbps = st.out_bytes.saturating_sub(rate_bytes) as f64 * 8.0
                / 1000.0
                / now.duration_since(last_rate_at).as_secs_f64();
            // One second of this stream, in bytes, from what was really pushed (an encoder
            // can send less than its nominal bitrate).
            let pushed_now = pushed.get();
            let push_rate = (pushed_now - last_pushed) as f64 / now.duration_since(last_rate_at).as_secs_f64();
            let second = (push_rate as u64).max(16_000);
            last_pushed = pushed_now;
            (rate_bytes, last_rate_at) = (st.out_bytes, now);
            // Congested: video waiting for the network (the sink can't take it fast
            // enough), or the server's confirmations falling behind. The backlog is
            // counted from 3 s after connecting: before that, data queues normally, and the
            // input drops what waits over 3 s (never sent, but counted as pushed).
            if connected_at.is_none() && st.in_bytes > 0 {
                connected_at = Some(now);
            }
            let settled = connected_at.is_some_and(|t| now.duration_since(t) > Duration::from_secs(3));
            let raw_backlog = pushed.get().saturating_sub(st.out_bytes);
            if settled && backlog_base.is_none() {
                backlog_base = Some(raw_backlog);
            }
            let backlog = backlog_base.map_or(0, |b| raw_backlog.saturating_sub(b));
            if backlog > 10 * second {
                break Err("the connection couldn't keep up".into());
            }
            let behind = st.window > 0 && st.out_bytes.saturating_sub(st.acked) > 2 * st.window;
            link.report(kbps, settled && (backlog > second || behind), None);
            let mut s = status.lock().unwrap();
            if delivery == Delivery::Live && backlog > 2 * second {
                // Connected, but the video goes out slower than it's made: viewers fall behind.
                s.state = "reconnecting";
                s.detail = "The internet upload is slower than the stream. Lowering the quality.".into();
            } else if delivery == Delivery::Live {
                s.state = "live";
                s.detail = lowered_detail(&quality);
            } else if s.state == "live" {
                // Was live, now catching up (e.g. confirmations falling behind).
                s.state = "reconnecting";
                s.detail = "The streaming server is slow to confirm. Waiting for it.".into();
            }
            s.kbps = kbps;
        }
    };
    stop_pipeline(&pipeline);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packet as the program's taps hand it over: its timestamp, and the segment its
    /// encoder sent (`start` shifted the way Media Foundation shifts video, by 1000 h).
    fn packet(pts_ms: u64, start: gst::ClockTime) -> gst::Sample {
        gst::init().unwrap();
        let mut buf = gst::Buffer::new();
        buf.get_mut().unwrap().set_pts(start + gst::ClockTime::from_mseconds(pts_ms));
        let mut segment = gst::FormattedSegment::<gst::ClockTime>::new();
        segment.set_start(start);
        gst::Sample::builder().buffer(&buf).segment(&segment).build()
    }

    const SHIFT: gst::ClockTime = gst::ClockTime::from_seconds(3_600_000);

    #[test]
    fn video_and_audio_line_up_whatever_the_encoder_did_to_its_timestamps() {
        let video = packet(2_000, SHIFT); // raw pts 1000 h + 2 s
        let audio = packet(2_100, gst::ClockTime::ZERO); // raw pts 2.1 s
        let base = running_time(&video).unwrap().0;
        assert_eq!(base, gst::ClockTime::from_seconds(2));
        let a = retimed(&audio, base, gst::ClockTime::ZERO).expect("audio after the keyframe is kept");
        assert_eq!(a.pts(), Some(gst::ClockTime::from_mseconds(100)));
        let offset = gst::ClockTime::from_seconds(60);
        assert_eq!(retimed(&video, base, offset).unwrap().pts(), Some(offset), "carried on from an earlier run");
        assert!(retimed(&packet(1_900, gst::ClockTime::ZERO), base, offset).is_none(), "before the keyframe");
    }

    #[test]
    fn a_rebuilt_program_is_noticed() {
        let mut last = None;
        for ms in [10_000, 10_033, 10_066] {
            assert!(!restarted(&mut last, &packet(ms, SHIFT)));
        }
        assert!(!restarted(&mut last, &packet(10_000, SHIFT)), "small jitter isn't a restart");
        assert!(restarted(&mut last, &packet(0, SHIFT)), "timestamps from zero again");
    }
}
