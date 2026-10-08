//! YouTube's HLS segment upload. Pure logic here; the segmenter and uploader live in
//! `jivvy-video`.
//!
//! The program is cut into short MPEG-TS segments on disk (one per keyframe interval, 2 s)
//! and uploaded in order over HTTPS, each after a playlist that lists it. A network
//! failure never stops the cutting: segments wait on disk and are retried until YouTube
//! confirms them, then the backlog goes out as fast as the connection allows. The queue
//! is saved after every change, so a crashed video process resumes the same playlist
//! (sequence numbers keep counting up, timestamps carry on) with the backlog it had.
//!
//! Rules from YouTube's HLS ingestion guide: the first playlist starts at sequence 0 and
//! sequence numbers only grow; `EXT-X-MEDIA-SEQUENCE` is the first segment listed; no more
//! than 5 listed segments may be unconfirmed; a few confirmed ones stay listed; segment
//! names are unique across restarts and no longer than 5 s.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::stream::Destination;

/// The media playlist's file name on the server.
pub const PLAYLIST: &str = "live.m3u8";
/// Segments and the queue, per destination: `<data dir>/stream-spool/<destination id>/`.
pub const SPOOL_DIR: &str = "stream-spool";
pub const QUEUE_FILE: &str = "queue.json";
pub const QUEUE_VERSION: u32 = 1;
/// The segmenter cuts at the first keyframe past this. The program sends one every 2 s,
/// so each segment is one 2 s keyframe interval, well under YouTube's 5 s limit.
pub const SEGMENT_MAX: Duration = Duration::from_secs(3);
/// Listed but not yet confirmed: YouTube allows at most 5.
pub const OUTSTANDING: usize = 5;
/// Confirmed segments kept in the playlist, as in YouTube's example.
pub const CONFIRMED_LISTED: usize = 2;
/// The most video kept waiting on disk (about 45 MB at 6 Mbps). After a longer outage
/// the oldest waiting video is skipped so viewers aren't left minutes behind; the local
/// recording still has it.
pub const BACKLOG_LIMIT: Duration = Duration::from_secs(60);
/// Up to this much video waiting still counts as live (one segment being cut, one sending).
pub const LIVE_BEHIND: Duration = Duration::from_secs(6);
/// A playlist upload that hasn't finished in this long has failed.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a segment gets to go up: three times its own length (at least 5 s). Slower
/// than that, the connection can't carry this stream live, and a 2 s segment that takes
/// longer is no use to viewers anyway.
pub fn segment_timeout(duration_secs: f64) -> Duration {
    Duration::from_secs_f64((duration_secs * 3.0).max(5.0))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// File name, on disk and on the server.
    pub name: String,
    /// Seconds.
    pub duration: f64,
    /// The stream bitrate it was cut at (kbps; 0 if unknown).
    #[serde(default)]
    pub kbps: u32,
}

/// What still has to reach the server, and where the playlist is up to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    pub version: u32,
    /// Which address and key this playlist belongs to (a hash, never the key), so a new
    /// key starts a new playlist at sequence 0.
    pub fingerprint: u64,
    /// Sequence number of the first segment listed: the oldest kept confirmed one, or the
    /// first waiting one when none is kept.
    pub first_seq: u64,
    /// Confirmed by the server (200/202) and still listed; their files are already deleted.
    /// Only ever segments the server has: skipped ones are never kept here.
    pub confirmed: VecDeque<Segment>,
    /// Cut and on disk, waiting for the server to confirm them, oldest first.
    pub waiting: VecDeque<Segment>,
    /// How many of `waiting` have been in a playlist sent to the server. Their sequence
    /// numbers are taken: never reused, even if the segment itself is skipped.
    pub listed: usize,
    /// The server has accepted a playlist, so the numbers it listed are spoken for.
    #[serde(default)]
    pub playlist_accepted: bool,
    /// Sequence numbers used up right after the first waiting segment by segments skipped
    /// behind it (`skip_backlog`). Until it is confirmed or discarded, nothing after it is
    /// listed; then the playlist starts after the gap.
    #[serde(default)]
    pub gap: u64,
    /// Where the last segment cut ends, in nanoseconds of stream time: the next segmenter
    /// run carries timestamps on from here.
    pub end_ns: u64,
    /// Seconds of video skipped after outages longer than the backlog limit.
    pub skipped_secs: f64,
    /// When this was saved (ms since the epoch).
    pub saved_ms: u64,
}

impl Queue {
    pub fn new(fingerprint: u64) -> Queue {
        Queue { version: QUEUE_VERSION, fingerprint, ..Queue::default() }
    }

    /// The saved queue for this destination, or a fresh one (sequence 0) if there is none,
    /// it can't be read, or it belongs to another address or key. Video that has waited
    /// longer than the backlog limit is skipped; the playlist itself carries on.
    pub fn load(dir: &Path, fingerprint: u64, now_ms: u64) -> Queue {
        let mut q = std::fs::read(dir.join(QUEUE_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Queue>(&b).ok())
            .filter(|q| q.version == QUEUE_VERSION && q.fingerprint == fingerprint && q.listed <= q.waiting.len())
            .unwrap_or_else(|| Queue::new(fingerprint));
        if now_ms.saturating_sub(q.saved_ms) > BACKLOG_LIMIT.as_millis() as u64 {
            q.skip_waiting();
        }
        q
    }

    pub fn save(&mut self, dir: &Path, now_ms: u64) -> std::io::Result<()> {
        self.saved_ms = now_ms;
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        crate::write_atomic(&dir.join(QUEUE_FILE), &bytes)
    }

    /// Gives up on everything waiting (see `discard`).
    pub fn skip_waiting(&mut self) {
        while let Some(name) = self.waiting.front().map(|s| s.name.clone()) {
            self.discard(&name);
        }
    }

    /// Gives up on the next waiting segment without it reaching the server (it was
    /// rejected, or is too old to send). False if `name` isn't the next one.
    ///
    /// It never joins the confirmed ones: playlists only ever list media the server has.
    /// If it was listed, its sequence number stays used up; a playlist lists consecutive
    /// numbers, so the confirmed ones before it leave the playlist too and the next one
    /// starts after it. Before the server has accepted any playlist nothing is spoken for,
    /// and numbering starts over where it was (so the first playlist it sees is at 0).
    pub fn discard(&mut self, name: &str) -> bool {
        if self.waiting.front().is_none_or(|s| s.name != name) {
            return false;
        }
        let s = self.waiting.pop_front().expect("checked");
        self.skipped_secs += s.duration;
        if self.listed > 0 {
            self.listed -= 1;
            if self.playlist_accepted {
                self.first_seq += self.confirmed.len() as u64 + 1 + self.gap;
                self.confirmed.clear();
            }
        }
        self.gap = 0;
        true
    }

    /// Gives up on everything waiting behind the next segment (which may be on its way
    /// up): after stepping down to a lower quality, a backlog at the old bitrate would
    /// take far too long through the slow connection. Returns the skipped segments.
    pub fn skip_backlog(&mut self) -> Vec<Segment> {
        if self.waiting.len() <= 1 {
            return Vec::new();
        }
        let skipped: Vec<Segment> = self.waiting.drain(1..).collect();
        if self.playlist_accepted {
            self.gap += self.listed.saturating_sub(1) as u64;
        }
        self.listed = self.listed.min(1);
        self.skipped_secs += skipped.iter().map(|s| s.duration).sum::<f64>();
        skipped
    }

    /// Adds a segment just cut, ending at `end_ns`. Returns segments skipped to keep the
    /// backlog under the limit (their files should be deleted).
    pub fn push(&mut self, segment: Segment, end_ns: u64) -> Vec<Segment> {
        self.waiting.push_back(segment);
        self.end_ns = self.end_ns.max(end_ns);
        let mut skipped = Vec::new();
        // Skip the oldest video not yet given a sequence number, keeping the newest.
        while self.behind() > BACKLOG_LIMIT && self.waiting.len() > self.listed + 1 {
            let s = self.waiting.remove(self.listed).expect("index checked");
            self.skipped_secs += s.duration;
            skipped.push(s);
        }
        skipped
    }

    /// The next segment to upload.
    pub fn next(&self) -> Option<&Segment> {
        self.waiting.front()
    }

    /// The playlist to send before the next segment. Marks what it lists as listed.
    pub fn playlist(&mut self) -> String {
        // Numbers after the first waiting segment are used up: list nothing past it yet.
        let n = self.waiting.len().min(if self.gap > 0 { 1 } else { OUTSTANDING });
        self.listed = self.listed.max(n);
        let listed: Vec<&Segment> = self.confirmed.iter().chain(self.waiting.iter().take(n)).collect();
        // Every segment, rounded, must fit the target duration, and it should never change.
        let longest = listed.iter().map(|s| s.duration.round() as u64).max().unwrap_or(0);
        let target = longest.max(4);
        let mut m3u8 = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            self.first_seq
        );
        for s in listed {
            m3u8.push_str(&format!("#EXTINF:{:.3},\n{}\n", s.duration, s.name));
        }
        m3u8
    }

    /// The server accepted a playlist.
    pub fn accept_playlist(&mut self) {
        self.playlist_accepted = true;
    }

    /// The server confirmed `name`. False if it isn't the next waiting segment (the queue
    /// was reset meanwhile).
    pub fn confirm(&mut self, name: &str) -> bool {
        if self.waiting.front().is_none_or(|s| s.name != name) {
            return false;
        }
        let s = self.waiting.pop_front().expect("checked");
        self.listed = self.listed.saturating_sub(1);
        self.confirmed.push_back(s);
        if self.confirmed.len() > CONFIRMED_LISTED {
            self.confirmed.pop_front();
            self.first_seq += 1;
        }
        if self.gap > 0 {
            // A playlist lists consecutive numbers: start after the gap.
            self.first_seq += self.confirmed.len() as u64 + self.gap;
            (self.confirmed, self.gap) = (VecDeque::new(), 0);
        }
        true
    }

    /// Video cut but not yet confirmed.
    pub fn behind(&self) -> Duration {
        Duration::from_secs_f64(self.waiting.iter().map(|s| s.duration).sum::<f64>().max(0.0))
    }
}

/// Identifies a destination's address and key without revealing the key (FNV-1a, stable
/// across builds).
pub fn fingerprint(d: &Destination) -> u64 {
    d.hls_url("").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// A segment file name unique across restarts: the session (start time) and its index.
pub fn segment_name(session: u64, index: u32) -> String {
    format!("s{session}-{index:05}.ts")
}

/// How an upload went, in the uploader's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 200, or 202 for a segment that arrived before its playlist: confirmed.
    Confirmed,
    /// The server can't use this file (400). Retrying the same bytes won't help.
    Unusable(String),
    /// Anything else: network errors, 401 (key not accepted, maybe fixed in a moment),
    /// 5xx. Wait and retry.
    Retry(String),
    /// Connected and sending, but the upload didn't finish within `REQUEST_TIMEOUT`: the
    /// connection is too slow for this segment. Congestion, not a dead connection.
    TooSlow,
}

/// A segment that didn't fit through the connection this many times in a row is skipped:
/// the same bytes won't go faster. One cut at a higher bitrate than the destination now
/// sends is skipped on its first failure: lower-quality segments are coming behind it.
pub const TOO_SLOW_TRIES: u32 = 2;

/// An upper bound on the upload speed after `bytes` didn't go up within `timeout`.
pub fn too_slow_kbps(bytes: usize, timeout: Duration) -> f64 {
    bytes as f64 * 8.0 / 1000.0 / timeout.as_secs_f64()
}

/// Whether a segment that didn't go up in time should be skipped rather than retried.
pub fn give_up_too_slow(segment_kbps: u32, now_kbps: Option<u32>, tries: u32) -> bool {
    tries >= TOO_SLOW_TRIES || now_kbps.is_some_and(|k| segment_kbps > k)
}

pub fn outcome(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Confirmed,
        400 => Outcome::Unusable("the server answered 400 (it couldn't read what we sent)".into()),
        401 => Outcome::Retry("the server answered 401 (stream key not accepted)".into()),
        403 => Outcome::Retry("the server answered 403 (forbidden)".into()),
        s => Outcome::Retry(format!("the server answered {s}")),
    }
}

/// A destination's state and the words on screen, from the uploader's progress.
pub fn health(confirmed_any: bool, last_error: Option<&str>, behind: Duration) -> (&'static str, String) {
    if let Some(e) = last_error {
        ("reconnecting", crate::stream::plain_error(e))
    } else if !confirmed_any {
        ("connecting", String::new())
    } else if behind > LIVE_BEHIND {
        let secs = behind.as_secs();
        ("reconnecting", format!("{secs} s behind. Sending the saved video as fast as the connection allows."))
    } else {
        ("live", String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(i: u32) -> Segment {
        Segment { name: segment_name(7, i), duration: 2.0, kbps: 6128 }
    }

    /// (sequence, name) pairs a playlist lists.
    fn listed(m3u8: &str) -> Vec<(u64, String)> {
        let first: u64 = m3u8
            .lines()
            .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
            .and_then(|n| n.parse().ok())
            .expect("media sequence");
        m3u8.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .enumerate()
            .map(|(i, n)| (first + i as u64, n.into()))
            .collect()
    }

    #[test]
    fn the_first_playlist_starts_at_zero_and_each_segment_follows_its_playlist() {
        let mut q = Queue::new(1);
        q.push(seg(0), 2_000_000_000);
        let p = q.playlist();
        assert!(p.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n"), "{p}");
        assert!(p.contains("#EXTINF:2.000,\ns7-00000.ts\n"), "{p}");
        assert_eq!(q.next().unwrap().name, "s7-00000.ts");
        assert!(q.confirm("s7-00000.ts"));
        q.push(seg(1), 4_000_000_000);
        assert_eq!(listed(&q.playlist()), [(0, "s7-00000.ts".into()), (1, "s7-00001.ts".into())]);
    }

    #[test]
    fn confirmed_segments_stay_listed_two_at_a_time_and_sequence_numbers_never_move() {
        let mut q = Queue::new(1);
        let mut seen: Vec<(u64, String)> = Vec::new();
        for i in 0..10 {
            q.push(seg(i), 0);
            let l = listed(&q.playlist());
            assert!(l.len() <= CONFIRMED_LISTED + 1, "{l:?}");
            // A sequence number always names the same segment.
            for (n, name) in &l {
                if let Some((_, before)) = seen.iter().find(|(m, _)| m == n) {
                    assert_eq!(before, name);
                } else {
                    seen.push((*n, name.clone()));
                }
            }
            assert!(q.confirm(&segment_name(7, i)));
        }
        assert_eq!(q.first_seq, 8);
        assert!(!q.confirm("s7-00003.ts"), "only the next waiting segment can be confirmed");
    }

    #[test]
    fn a_backlog_lists_at_most_five_unconfirmed() {
        let mut q = Queue::new(1);
        for i in 0..12 {
            q.push(seg(i), 0);
        }
        let l = listed(&q.playlist());
        assert_eq!(l.len(), OUTSTANDING);
        assert_eq!(l[0], (0, "s7-00000.ts".into()));
        assert_eq!(q.behind(), Duration::from_secs(24));
        // Catching up: each confirmation lets one more in.
        assert!(q.confirm("s7-00000.ts"));
        let l = listed(&q.playlist());
        assert_eq!(l.len(), 1 + OUTSTANDING);
        assert_eq!(l.last().unwrap(), &(5, "s7-00005.ts".into()));
    }

    #[test]
    fn a_long_outage_skips_the_oldest_unlisted_video_never_a_listed_one() {
        let mut q = Queue::new(1);
        q.push(seg(0), 0);
        q.playlist(); // seg 0 has sequence 0 now
        let mut skipped = Vec::new();
        for i in 1..=40 {
            skipped.extend(q.push(seg(i), 0));
        }
        assert!(q.behind() <= BACKLOG_LIMIT);
        assert_eq!(q.waiting.front().unwrap().name, "s7-00000.ts", "listed: kept");
        assert_eq!(q.waiting.back().unwrap().name, "s7-00040.ts", "newest: kept");
        assert_eq!(skipped.first().unwrap().name, "s7-00001.ts", "the oldest unlisted goes first");
        assert_eq!(q.skipped_secs, 2.0 * skipped.len() as f64);
        // Sequence numbers stay contiguous: the skipped ones never had any.
        let l = listed(&q.playlist());
        assert_eq!(l[1].1, q.waiting[1].name);
    }

    #[test]
    fn the_queue_survives_a_restart_but_a_new_key_starts_over() {
        let dir = std::env::temp_dir().join(format!("jivvy-hls-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut q = Queue::new(42);
        for i in 0..4 {
            q.push(seg(i), (i as u64 + 1) * 2_000_000_000);
        }
        q.playlist();
        q.accept_playlist();
        q.confirm("s7-00000.ts");
        q.save(&dir, 1_000_000).unwrap();
        assert_eq!(Queue::load(&dir, 42, 1_005_000), q);
        assert_eq!(Queue::load(&dir, 43, 1_005_000), Queue::new(43));

        // Saved long ago: the old video is skipped, but numbering carries on, and the
        // skipped segments (never confirmed) are never listed again.
        let mut stale = Queue::load(&dir, 42, 1_000_000 + 61_000);
        assert!(stale.waiting.is_empty() && stale.confirmed.is_empty());
        assert_eq!(stale.end_ns, q.end_ns);
        stale.push(seg(9), 0);
        let l = listed(&stale.playlist());
        assert_eq!(l, [(4, "s7-00009.ts".into())], "0-3 were taken");

        std::fs::write(dir.join(QUEUE_FILE), b"{broken").unwrap();
        assert_eq!(Queue::load(&dir, 42, 1_005_000), Queue::new(42));
    }

    #[test]
    fn a_rejected_segment_is_never_listed_as_confirmed_and_its_number_stays_used() {
        let mut q = Queue::new(1);
        for i in 0..4 {
            q.push(seg(i), 0);
        }
        q.playlist();
        q.accept_playlist();
        assert!(q.confirm("s7-00000.ts"));
        assert!(q.confirm("s7-00001.ts"));
        // The server answers 400 for segment 2.
        assert!(!q.discard("s7-00003.ts"), "only the next one");
        assert!(q.discard("s7-00002.ts"));
        let l = listed(&q.playlist());
        assert_eq!(l, [(3, "s7-00003.ts".into())], "0 and 1 can't be listed past the gap at 2");
        assert!(q.confirm("s7-00003.ts"));
        q.push(seg(4), 0);
        assert_eq!(listed(&q.playlist()), [(3, "s7-00003.ts".into()), (4, "s7-00004.ts".into())]);
    }

    #[test]
    fn stepping_down_skips_the_backlog_but_never_reuses_a_number() {
        let mut q = Queue::new(1);
        q.push(seg(0), 0);
        q.playlist();
        q.accept_playlist();
        q.confirm("s7-00000.ts");
        for i in 1..=6 {
            q.push(seg(i), 0);
        }
        q.playlist(); // lists 1..=5 (sequence 1..=5); 1 is being uploaded
        let skipped = q.skip_backlog();
        assert_eq!(skipped.len(), 5);
        assert_eq!(q.waiting.len(), 1);
        q.push(seg(7), 0);
        assert_eq!(
            listed(&q.playlist()),
            [(0, "s7-00000.ts".into()), (1, "s7-00001.ts".into())],
            "nothing past the gap while 1 is on its way"
        );
        assert!(q.confirm("s7-00001.ts"));
        assert_eq!(listed(&q.playlist()), [(6, "s7-00007.ts".into())], "2-5 were listed: used up");

        // The same when the segment on its way is then rejected.
        q.push(seg(8), 0);
        q.push(seg(9), 0);
        q.playlist(); // 7, 8, 9 as 6, 7, 8
        q.skip_backlog();
        assert!(q.discard("s7-00007.ts"));
        q.push(seg(10), 0);
        assert_eq!(listed(&q.playlist()), [(9, "s7-00010.ts".into())]);
    }

    #[test]
    fn before_any_playlist_is_accepted_numbering_starts_over_at_zero() {
        let mut q = Queue::new(1);
        q.push(seg(0), 0);
        q.push(seg(1), 0);
        q.playlist(); // sent, but never accepted
        assert!(q.discard("s7-00000.ts"));
        assert_eq!(listed(&q.playlist()), [(0, "s7-00001.ts".into())]);
    }

    #[test]
    fn fingerprints_follow_the_key_without_containing_it() {
        let d = |key: &str| Destination {
            id: "yt".into(),
            name: String::new(),
            url: "https://a.upload.youtube.com/http_upload_hls".into(),
            key: key.into(),
            enabled: true,
        };
        assert_eq!(fingerprint(&d("abcd-1")), fingerprint(&d("abcd-1")));
        assert_ne!(fingerprint(&d("abcd-1")), fingerprint(&d("abcd-2")));
    }

    #[test]
    fn server_answers() {
        assert_eq!(outcome(200), Outcome::Confirmed);
        assert_eq!(outcome(202), Outcome::Confirmed, "segment before its playlist still counts");
        assert!(matches!(outcome(400), Outcome::Unusable(_)));
        assert!(matches!(outcome(401), Outcome::Retry(e) if e.contains("401")));
        assert!(matches!(outcome(500), Outcome::Retry(_)));
    }

    #[test]
    fn a_segment_that_cant_go_up_in_time_bounds_the_speed_and_is_skipped_when_its_bitrate_is_gone() {
        // A 2 s segment gets 6 s; at 6 Mbps (1.5 MB) not up by then means under 2 Mbps.
        assert_eq!(segment_timeout(2.0), Duration::from_secs(6));
        assert_eq!(segment_timeout(0.5), Duration::from_secs(5));
        assert_eq!(too_slow_kbps(1_500_000, segment_timeout(2.0)).round(), 2000.0);
        assert!(!give_up_too_slow(6128, Some(6128), 1), "same quality: one more try");
        assert!(give_up_too_slow(6128, Some(6128), 2));
        assert!(give_up_too_slow(6128, Some(628), 1), "stepped down since: skip it now");
        assert!(!give_up_too_slow(6128, None, 1), "paused: the pause skips it");
    }

    #[test]
    fn live_only_when_confirmed_and_caught_up() {
        assert_eq!(health(false, None, Duration::ZERO).0, "connecting");
        assert_eq!(health(true, None, Duration::from_secs(2)), ("live", String::new()));
        let (state, detail) = health(true, None, Duration::from_secs(20));
        assert_eq!(state, "reconnecting");
        assert!(detail.starts_with("20 s behind"), "{detail}");
        let (state, detail) = health(true, Some("Could not connect: Connection refused"), Duration::ZERO);
        assert_eq!((state, detail.contains("Can't reach")), ("reconnecting", true));
        assert!(
            health(true, Some("the server answered 401 (stream key not accepted)"), Duration::ZERO)
                .1
                .contains("rejected")
        );
    }
}
