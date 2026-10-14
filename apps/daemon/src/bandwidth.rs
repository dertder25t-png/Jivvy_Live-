//! Bandwidth manager: fits the stream destinations into the church's upload.
//!
//! The recording always gets the full program. Each destination streams the full program
//! too unless the upload can't carry it; then it gets a lower quality (its own encode,
//! made only while someone uses it) or, last, is paused. The manager measures what the
//! connection carries and spends about 70% of it, keeping the rest for catching up after
//! drops.
//!
//! Auto mode: destinations are in priority order (the first in `stream.json` is the main
//! platform). The main platform gets the best quality that fits; when even the lowest
//! quality doesn't fit everyone, the lowest-priority platform is paused first.
//!
//! Advanced mode: the church can also give each platform a top quality, and say what
//! happens to it when the upload runs short: a lower quality (as in Auto), audio only (the
//! program's sound with a slow still picture, which platforms need to stay live), or pause.
//! With neither set, Advanced mode behaves exactly like Auto.
//!
//! Stepping down is quick (congestion for 3 s); stepping back up is careful (20 s without
//! congestion, then one step at a time, waiting longer after each failed try), because
//! every change costs that platform a reconnect.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::program::ProgramConfig;

/// Share of the measured upload the streams may use.
pub const SPEND: f64 = 0.7;
/// The program's audio, on every quality.
pub const AUDIO_KBPS: u32 = 128;
/// Congestion this long before stepping down.
pub const CONGESTED_FOR: Duration = Duration::from_secs(3);
/// After any change, measurements settle this long before the next one.
pub const SETTLE: Duration = Duration::from_secs(8);
/// No congestion this long before trying one step up; doubled after each failed try.
pub const PROBE_AFTER: Duration = Duration::from_secs(20);
pub const PROBE_MAX: Duration = Duration::from_secs(160);

/// A stream quality. `Full` is the program itself; the others are extra encodes. `Audio`
/// is the program's sound with a slow still-ish picture (Advanced mode's "audio only").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Tier {
    Full,
    P720,
    P480,
    P360,
    Audio,
}

/// Audio only: the picture platforms need to stay live, at this many frames a second and
/// this bitrate, beside the program's audio.
pub const AUDIO_ONLY_FPS: u32 = 2;
pub const AUDIO_ONLY_VIDEO_KBPS: u32 = 100;

impl Tier {
    pub const LOWER: [Tier; 3] = [Tier::P720, Tier::P480, Tier::P360];

    /// Height and video bitrate of a lower quality.
    fn spec(self) -> Option<(u32, u32)> {
        match self {
            Tier::Full => None,
            Tier::P720 => Some((720, 3000)),
            Tier::P480 => Some((480, 1500)),
            Tier::P360 => Some((360, 500)),
            Tier::Audio => None,
        }
    }
}

/// One rung of the quality ladder for this program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub tier: Tier,
    pub width: u32,
    pub height: u32,
    pub video_kbps: u32,
}

impl Quality {
    /// What it costs on the wire, audio included.
    pub fn kbps(&self) -> u32 {
        self.video_kbps + AUDIO_KBPS
    }

    /// "1080p", "720p", ..., or "audio" for audio only.
    pub fn label(&self) -> String {
        if self.tier == Tier::Audio { "audio".into() } else { format!("{}p", self.height) }
    }
}

/// The program, then every lower quality that is really lower (smaller and cheaper),
/// best first.
pub fn ladder(program: &ProgramConfig) -> Vec<Quality> {
    let full =
        Quality { tier: Tier::Full, width: program.width, height: program.height, video_kbps: program.bitrate_kbps };
    let mut out = vec![full];
    for tier in Tier::LOWER {
        let (height, kbps) = tier.spec().expect("lower tiers have a spec");
        if height < program.height && kbps < program.bitrate_kbps {
            // Same shape as the program, even sizes for the encoder.
            let width = ((program.width as u64 * height as u64 / program.height.max(1) as u64) as u32) & !1;
            out.push(Quality { tier, width, height, video_kbps: kbps });
        }
    }
    out
}

/// Audio only for this ladder: the lowest quality's picture size, at a trickle.
pub fn audio_quality(ladder: &[Quality]) -> Option<Quality> {
    let lowest = ladder.last()?;
    Some(Quality { tier: Tier::Audio, width: lowest.width, height: lowest.height, video_kbps: AUDIO_ONLY_VIDEO_KBPS })
}

/// What happens to a platform when the upload runs short (Advanced mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhenShort {
    /// Step down through the lower qualities (as in Auto mode).
    #[default]
    Lower,
    /// Keep its quality while it fits, then audio only.
    AudioOnly,
    /// Keep its quality while it fits, then pause.
    Pause,
}

/// A platform's Advanced-mode settings; the default is Auto mode's behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pref {
    /// Never above this height (e.g. 720 for "720p").
    pub max_height: Option<u32>,
    pub when_short: WhenShort,
}

/// Settings per destination id; a missing one is the default.
pub type Prefs = HashMap<String, Pref>;

/// The best a platform may send: the program, or its own top quality in Advanced mode.
pub fn top_quality(ladder: &[Quality], pref: Pref) -> Option<Quality> {
    options(ladder, pref).first().copied()
}

/// What a platform may send, best first, before being paused.
fn options(ladder: &[Quality], pref: Pref) -> Vec<Quality> {
    let mut video = ladder.iter().filter(|q| q.tier != Tier::Audio);
    let mut capped: Vec<Quality> =
        video.clone().filter(|q| pref.max_height.is_none_or(|m| q.height <= m)).copied().collect();
    if capped.is_empty() {
        // A cap below every quality: the lowest there is.
        capped.extend(video.next_back().copied());
    }
    match pref.when_short {
        WhenShort::Lower => capped,
        WhenShort::AudioOnly => capped.first().copied().into_iter().chain(audio_quality(ladder)).collect(),
        WhenShort::Pause => capped.into_iter().take(1).collect(),
    }
}

/// What a destination's worker reports once a second.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    pub id: String,
    /// Kilobits a second the network took from it over the last second.
    pub sent_kbps: f64,
    /// Data is piling up waiting for the network.
    pub congested: bool,
    /// A direct measurement of the upload speed, when the worker has one (HLS: how fast
    /// a segment went up while there was a backlog).
    pub capacity_kbps: Option<f64>,
}

/// What a destination should send: a quality, or nothing (paused).
pub type Assignment = Option<Tier>;

#[derive(Debug)]
pub struct Manager {
    /// Estimated upload speed; None until the connection has shown a limit.
    capacity: Option<f64>,
    congested_since: Option<Instant>,
    /// What the connection carried each second of the current congestion (kbps).
    carried: Vec<f64>,
    calm_since: Instant,
    last_change: Instant,
    /// The last change was a step up that hasn't proved itself yet.
    probing: bool,
    probe_wait: Duration,
    assigned: HashMap<String, Assignment>,
}

impl Manager {
    pub fn new(now: Instant) -> Manager {
        Manager {
            capacity: None,
            congested_since: None,
            carried: Vec::new(),
            calm_since: now,
            last_change: now - SETTLE,
            probing: false,
            probe_wait: PROBE_AFTER,
            assigned: HashMap::new(),
        }
    }

    /// The upload speed the manager is working with, if it has seen a limit.
    pub fn capacity_kbps(&self) -> Option<f64> {
        self.capacity
    }

    /// Takes this second's reports and returns what every destination in `order`
    /// (highest priority first) should send. `streaming` is false while the stream is off.
    pub fn update(
        &mut self,
        ladder: &[Quality],
        order: &[String],
        reports: &[Report],
        streaming: bool,
        now: Instant,
    ) -> HashMap<String, Assignment> {
        self.update_with(ladder, order, &Prefs::new(), reports, streaming, now)
    }

    /// `update` with Advanced-mode settings per destination.
    pub fn update_with(
        &mut self,
        ladder: &[Quality],
        order: &[String],
        prefs: &Prefs,
        reports: &[Report],
        streaming: bool,
        now: Instant,
    ) -> HashMap<String, Assignment> {
        if streaming {
            self.observe(ladder, order, prefs, reports, now);
        } else {
            (self.congested_since, self.calm_since, self.probing) = (None, now, false);
            self.carried.clear();
        }
        self.assigned = allocate_with(ladder, order, prefs, self.capacity);
        self.assigned.clone()
    }

    fn observe(&mut self, ladder: &[Quality], order: &[String], prefs: &Prefs, reports: &[Report], now: Instant) {
        let settled = now.duration_since(self.last_change) >= SETTLE;
        if reports.iter().any(|r| r.congested) {
            self.calm_since = now;
            let since = *self.congested_since.get_or_insert(now);
            self.carried.push(reports.iter().map(|r| r.sent_kbps).sum());
            if now.duration_since(since) >= CONGESTED_FOR && settled {
                // What the connection really carried over the whole stretch (one second can
                // show nothing while a blocked socket drains), or a direct measurement if higher.
                let carried = self.carried.iter().sum::<f64>() / self.carried.len() as f64;
                let measured = reports.iter().filter_map(|r| r.capacity_kbps).fold(carried, f64::max);
                if self.probing {
                    self.probe_wait = (self.probe_wait * 2).min(PROBE_MAX);
                }
                // Never above what was already too much.
                let current = self.capacity.unwrap_or(f64::INFINITY);
                self.capacity = Some(measured.min(current * 0.9).max(1.0));
                (self.probing, self.congested_since, self.last_change) = (false, None, now);
                self.carried.clear();
            }
            return;
        }
        self.congested_since = None;
        self.carried.clear();
        let calm = now.duration_since(self.calm_since);
        if self.probing && calm >= PROBE_AFTER {
            // The step up held.
            (self.probing, self.probe_wait) = (false, PROBE_AFTER);
        }
        if calm >= self.probe_wait
            && settled
            && let Some(c) = self.capacity
            && let Some(next) = next_step(ladder, order, prefs, c)
        {
            (self.capacity, self.probing, self.last_change, self.calm_since) = (Some(next), true, now, now);
        }
    }
}

/// The smallest capacity above `c` that changes the allocation (one step up for someone),
/// or None when everyone already has the best.
fn next_step(ladder: &[Quality], order: &[String], prefs: &Prefs, c: f64) -> Option<f64> {
    let now = allocate_with(ladder, order, prefs, Some(c));
    let best = allocate_with(ladder, order, prefs, None);
    if now == best {
        return None;
    }
    // Grows from any estimate, however low, until someone steps up (at the latest when the
    // budget covers everyone at the best quality).
    let everyone_best = ladder.first().map_or(0, |q| q.kbps()) as f64 * order.len() as f64 / SPEND;
    let mut next = c.max(1.0);
    loop {
        next *= 1.05;
        if allocate_with(ladder, order, prefs, Some(next)) != now || next > everyone_best {
            return Some(next);
        }
    }
}

/// Auto mode: fit `order` (highest priority first) into 70% of `capacity`.
pub fn allocate(ladder: &[Quality], order: &[String], capacity: Option<f64>) -> HashMap<String, Assignment> {
    allocate_with(ladder, order, &Prefs::new(), capacity)
}

/// Fits `order` (highest priority first) into 70% of `capacity`, honoring each platform's
/// top quality and what it does when short. With default settings this is Auto mode.
pub fn allocate_with(
    ladder: &[Quality],
    order: &[String],
    prefs: &Prefs,
    capacity: Option<f64>,
) -> HashMap<String, Assignment> {
    if ladder.is_empty() {
        return HashMap::new();
    }
    let budget = capacity.map_or(f64::INFINITY, |c| c * SPEND);
    let opts: Vec<Vec<Quality>> =
        order.iter().map(|id| options(ladder, prefs.get(id).copied().unwrap_or_default())).collect();
    let min = |i: usize| opts[i].last().map_or(0.0, |q| q.kbps() as f64);
    // Pause from the bottom until everyone left fits at their cheapest. The main platform is
    // never paused unless it asked to be (at worst it gets its cheapest).
    let pausable = |i: usize| i > 0 || prefs.get(&order[i]).is_some_and(|p| p.when_short == WhenShort::Pause);
    let mut playing: Vec<bool> = vec![true; order.len()];
    let cost = |playing: &[bool]| (0..order.len()).filter(|&i| playing[i]).map(min).sum::<f64>();
    while cost(&playing) > budget {
        let Some(i) = (0..order.len()).rev().find(|&i| playing[i] && pausable(i)) else { break };
        playing[i] = false;
    }
    // A platform paused on the way may fit again now that one above it is paused too.
    for i in 0..order.len() {
        if !playing[i] && cost(&playing) + min(i) <= budget {
            playing[i] = true;
        }
    }
    let mut left = budget;
    let mut out = HashMap::new();
    for (i, id) in order.iter().enumerate() {
        if !playing[i] {
            out.insert(id.clone(), None);
            continue;
        }
        // The best that fits, keeping everyone after it at their cheapest.
        let reserve: f64 = (i + 1..order.len()).filter(|&j| playing[j]).map(min).sum();
        let q = opts[i].iter().find(|q| q.kbps() as f64 <= left - reserve).or(opts[i].last()).copied();
        if let Some(q) = q {
            left -= q.kbps() as f64;
        }
        out.insert(id.clone(), q.map(|q| q.tier));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        ["youtube", "facebook", "vimeo"][..n].iter().map(|s| s.to_string()).collect()
    }

    fn program() -> Vec<Quality> {
        ladder(&ProgramConfig::default())
    }

    fn report(id: &str, sent: f64, congested: bool) -> Report {
        Report { id: id.into(), sent_kbps: sent, congested, capacity_kbps: None }
    }

    #[test]
    fn the_ladder_only_has_qualities_really_below_the_program() {
        let l = program();
        let labels: Vec<String> = l.iter().map(|q| q.label()).collect();
        assert_eq!(labels, ["1080p", "720p", "480p", "360p"]);
        assert_eq!((l[1].width, l[1].height, l[1].video_kbps), (1280, 720, 3000));
        assert_eq!((l[3].width, l[3].height), (640, 360));
        let small = ladder(&ProgramConfig { width: 1280, height: 720, bitrate_kbps: 2500, ..ProgramConfig::default() });
        let labels: Vec<String> = small.iter().map(|q| q.label()).collect();
        assert_eq!(labels, ["720p", "480p", "360p"], "no 720p tier above a 2.5 Mbps 720p program");
    }

    #[test]
    fn auto_gives_the_main_platform_the_best_and_pauses_from_the_bottom() {
        let l = program();
        let order = ids(2);
        let a = allocate(&l, &order, None);
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::Full), Some(Tier::Full)), "no limit seen");
        // 10 Mbps: 7 Mbps to spend. YouTube keeps 1080p (6.1) and Facebook gets what's left.
        let a = allocate(&l, &order, Some(10_000.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::Full), Some(Tier::P360)));
        // 8 Mbps: 5.6 to spend. 1080p would leave Facebook less than its lowest, so YouTube
        // gets 720p (3.1) and Facebook 480p (1.6) from the 2.5 left.
        let a = allocate(&l, &order, Some(8_000.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::P720), Some(Tier::P480)));
        let a = allocate(&l, &order, Some(4_000.0)); // 2.8 Mbps
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::P480), Some(Tier::P360)));
        // 1.2 Mbps: 840 kbps can't carry two at 360p (628 each): Facebook is paused.
        let a = allocate(&l, &order, Some(1_200.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::P360), None));
        // The main platform is never paused, even below the lowest quality.
        assert_eq!(allocate(&l, &ids(1), Some(300.0))["youtube"], Some(Tier::P360));
    }

    fn prefs(list: &[(&str, Option<u32>, WhenShort)]) -> Prefs {
        list.iter().map(|(id, max, when)| (id.to_string(), Pref { max_height: *max, when_short: *when })).collect()
    }

    #[test]
    fn advanced_mode_with_no_settings_is_auto_mode() {
        let (l, order) = (program(), ids(3));
        let defaults = prefs(&[("youtube", None, WhenShort::Lower), ("facebook", None, WhenShort::Lower)]);
        for c in [
            None,
            Some(300.0),
            Some(1_200.0),
            Some(2_000.0),
            Some(4_000.0),
            Some(8_000.0),
            Some(10_000.0),
            Some(30_000.0),
        ] {
            assert_eq!(allocate_with(&l, &order, &defaults, c), allocate(&l, &order, c), "{c:?}");
        }
    }

    #[test]
    fn advanced_mode_ranks_platforms_and_caps_each_one() {
        let l = program();
        // Facebook first: the church's main platform.
        let order: Vec<String> = ["facebook", "youtube"].iter().map(|s| s.to_string()).collect();
        let p = prefs(&[("youtube", Some(720), WhenShort::Lower)]);
        let a = allocate_with(&l, &order, &p, None);
        assert_eq!((a["facebook"], a["youtube"]), (Some(Tier::Full), Some(Tier::P720)), "YouTube never above 720p");
        // 8 Mbps (5.6 to spend): Facebook keeps the best that leaves YouTube its lowest.
        let a = allocate_with(&l, &order, &p, Some(8_000.0));
        assert_eq!((a["facebook"], a["youtube"]), (Some(Tier::P720), Some(Tier::P480)));
        // The cap saves bandwidth for the main platform: with 7 Mbps to spend, Facebook
        // keeps 1080p (6.1) and YouTube, capped, gets the rest.
        let a = allocate_with(&l, &order, &p, Some(10_000.0));
        assert_eq!((a["facebook"], a["youtube"]), (Some(Tier::Full), Some(Tier::P360)));
        // A cap below every quality still streams, at the lowest.
        let low = prefs(&[("facebook", Some(240), WhenShort::Lower)]);
        assert_eq!(allocate_with(&l, &order, &low, None)["facebook"], Some(Tier::P360));
    }

    #[test]
    fn audio_only_keeps_a_platform_live_on_a_trickle_instead_of_lowering_it() {
        let l = program();
        let audio = audio_quality(&l).unwrap();
        assert_eq!((audio.label(), audio.kbps()), ("audio".to_string(), AUDIO_ONLY_VIDEO_KBPS + AUDIO_KBPS));
        let order = ids(2);
        let p = prefs(&[("facebook", Some(720), WhenShort::AudioOnly)]);
        // Plenty: Facebook at its top quality.
        assert_eq!(allocate_with(&l, &order, &p, None)["facebook"], Some(Tier::P720));
        // 10 Mbps (7 to spend): YouTube 1080p leaves 0.9, short of Facebook's 720p (3.1): audio only.
        let a = allocate_with(&l, &order, &p, Some(10_000.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::Full), Some(Tier::Audio)));
        // 1.3 Mbps (910): in Auto, Facebook would be paused (two 360p need 1256); audio only
        // (228) still fits beside YouTube's 360p (628).
        assert_eq!(allocate(&l, &order, Some(1_300.0))["facebook"], None);
        let a = allocate_with(&l, &order, &p, Some(1_300.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::P360), Some(Tier::Audio)));
        // 1.2 Mbps (840): not even that beside YouTube's 360p; Facebook is paused, YouTube streams.
        let a = allocate_with(&l, &order, &p, Some(1_200.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::P360), None));
    }

    #[test]
    fn pause_keeps_a_platform_at_its_quality_or_not_at_all_and_others_take_its_place() {
        let l = program();
        let order = ids(2);
        // The main platform itself would rather pause than lower.
        let p = prefs(&[("youtube", None, WhenShort::Pause)]);
        assert_eq!(allocate_with(&l, &order, &p, Some(10_000.0))["youtube"], Some(Tier::Full));
        // 4 Mbps (2.8 to spend): 1080p doesn't fit, so YouTube pauses; Facebook gets the room.
        let a = allocate_with(&l, &order, &p, Some(4_000.0));
        assert_eq!((a["youtube"], a["facebook"]), (None, Some(Tier::P480)));
        // A lower-ranked "pause" platform pauses rather than squeeze the one above it: at
        // 3 Mbps (2.1 to spend) Auto gives both 360p; here YouTube gets 480p instead.
        assert_eq!(allocate(&l, &order, Some(3_000.0))["youtube"], Some(Tier::P360));
        let p = prefs(&[("facebook", Some(480), WhenShort::Pause)]);
        let a = allocate_with(&l, &order, &p, Some(3_000.0));
        assert_eq!((a["youtube"], a["facebook"]), (Some(Tier::P480), None));
    }

    #[test]
    fn advanced_mode_steps_up_and_down_through_its_own_options() {
        let l = program();
        let order = ids(2);
        let p = prefs(&[("facebook", None, WhenShort::AudioOnly)]);
        // From audio only back up: the next step changes someone's assignment.
        let c = 1_200.0;
        let next = next_step(&l, &order, &p, c).expect("there is better");
        assert_ne!(allocate_with(&l, &order, &p, Some(next)), allocate_with(&l, &order, &p, Some(c)));
        let t0 = Instant::now();
        let mut m = Manager::new(t0);
        let reports = [report("youtube", 1_000.0, true), report("facebook", 300.0, true)];
        for s in 0..=3 {
            m.update_with(&l, &order, &p, &reports, true, t0 + Duration::from_secs(s));
        }
        assert_eq!(m.assigned["facebook"], Some(Tier::Audio), "1.3 Mbps carried: 910 to spend");
    }

    #[test]
    fn congestion_steps_down_after_3_seconds_to_what_the_connection_carried() {
        let (l, order) = (program(), ids(1));
        let t0 = Instant::now();
        let mut m = Manager::new(t0);
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert_eq!(m.update(&l, &order, &[report("youtube", 6100.0, false)], true, at(1))["youtube"], Some(Tier::Full));
        // Throttled to 1 Mbps: congested, carrying ~1000 kbps.
        for s in 2..5 {
            let a = m.update(&l, &order, &[report("youtube", 1000.0, true)], true, at(s));
            assert_eq!(a["youtube"], Some(Tier::Full), "not before 3 s of congestion");
        }
        let a = m.update(&l, &order, &[report("youtube", 1000.0, true)], true, at(5));
        assert_eq!(a["youtube"], Some(Tier::P360), "700 kbps to spend");
        assert_eq!(m.capacity_kbps(), Some(1000.0));
        // Still congested right after the change (the backlog drains): no new step until it settles.
        let a = m.update(&l, &order, &[report("youtube", 400.0, true)], true, at(9));
        assert_eq!(a["youtube"], Some(Tier::P360));
    }

    #[test]
    fn steps_back_up_one_at_a_time_when_calm_and_waits_longer_after_a_failed_try() {
        let (l, order) = (program(), ids(1));
        let t0 = Instant::now();
        let mut m = Manager::new(t0);
        let at = |s: u64| t0 + Duration::from_secs(s);
        for s in 0..=3 {
            m.update(&l, &order, &[report("youtube", 1000.0, true)], true, at(s));
        }
        assert_eq!(m.assigned["youtube"], Some(Tier::P360));
        let calm =
            |m: &mut Manager, s| m.update(&l, &order, &[report("youtube", 600.0, false)], true, at(s))["youtube"];
        // 20 s calm: one step up.
        assert_eq!(calm(&mut m, 22), Some(Tier::P360));
        assert_eq!(calm(&mut m, 23), Some(Tier::P480));
        // The try fails: back down, and the next try waits 40 s.
        for s in 24..=32 {
            m.update(&l, &order, &[report("youtube", 1000.0, true)], true, at(s));
        }
        assert_eq!(m.assigned["youtube"], Some(Tier::P360));
        assert_eq!(calm(&mut m, 60), Some(Tier::P360), "not after 20 s this time");
        assert_eq!(calm(&mut m, 73), Some(Tier::P480), "after 40 s");
        // That one holds; later steps continue up to the full program.
        let mut s = 74;
        while m.assigned["youtube"] != Some(Tier::Full) {
            calm(&mut m, s);
            s += 1;
            assert!(s < 300, "never got back to full quality");
        }
    }

    #[test]
    fn a_direct_measurement_beats_a_low_carried_rate() {
        let (l, order) = (program(), ids(1));
        let t0 = Instant::now();
        let mut m = Manager::new(t0);
        let r = Report { capacity_kbps: Some(5_000.0), ..report("youtube", 1_500.0, true) };
        for s in 0..=3 {
            m.update(&l, &order, std::slice::from_ref(&r), true, t0 + Duration::from_secs(s));
        }
        assert_eq!(m.capacity_kbps(), Some(5_000.0));
        assert_eq!(m.assigned["youtube"], Some(Tier::P720), "3.5 Mbps to spend");
    }

    #[test]
    fn a_second_when_the_socket_takes_nothing_doesnt_collapse_the_estimate() {
        let (l, order) = (program(), ids(1));
        let t0 = Instant::now();
        let mut m = Manager::new(t0);
        for (s, sent) in [(0, 1100.0), (1, 900.0), (2, 1000.0), (3, 0.0)] {
            m.update(&l, &order, &[report("youtube", sent, true)], true, t0 + Duration::from_secs(s));
        }
        assert_eq!(m.capacity_kbps(), Some(750.0), "the average, not the last second's 0");
    }

    #[test]
    fn steps_up_even_from_a_tiny_estimate() {
        let (l, order) = (program(), ids(1));
        assert!(
            next_step(&l, &order, &Prefs::new(), 1.0)
                .is_some_and(|c| allocate(&l, &order, Some(c))["youtube"] == Some(Tier::P480))
        );
        assert_eq!(next_step(&l, &order, &Prefs::new(), 100_000.0), None, "already at the best");
    }

    #[test]
    fn nothing_changes_while_the_stream_is_off() {
        let (l, order) = (program(), ids(1));
        let t0 = Instant::now();
        let mut m = Manager::new(t0);
        for s in 0..10 {
            m.update(&l, &order, &[report("youtube", 0.0, true)], false, t0 + Duration::from_secs(s));
        }
        assert_eq!(m.capacity_kbps(), None);
    }
}
