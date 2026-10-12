//! Local recording: where the files go, how a set of files is named and resumed after a
//! crash, which recordings are ready to convert, and the low-disk warning. The GStreamer
//! side (`jivvy-video`'s `recorder.rs` and `remux.rs`) uses these.
//!
//! A recording is a **set**: a folder holding `part-001.mkv`, `part-002.mkv` and so on.
//! Every part is Matroska written as a stream, so a crash (or a power cut) leaves a file
//! that plays up to the last second or so. The video process starting again while the
//! recording is still wanted opens the next part in the same set; stopping the recording
//! closes the set, and after the service its parts are converted to MP4.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const CONFIG_FILE: &str = "recording.json";
pub const CONFIG_VERSION: u32 = 1;
/// Names the set being recorded while the recording is wanted. Survives a crash of the
/// video process, which is how the next process knows which set to carry on in.
pub const CURRENT_FILE: &str = "recording-current.json";
/// Where recordings go when `recording.json` doesn't say, inside the data folder.
pub const DEFAULT_FOLDER: &str = "recordings";
/// The recorder warns when the disk has less than this free (plan: "warns at 10 GB left").
pub const WARN_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// `recording.json`: all optional; the file may be missing.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    /// Where recordings go. A relative path is inside the data folder.
    #[serde(default)]
    pub folder: Option<String>,
    /// Warn below this much free space, in GB. Defaults to 10.
    #[serde(default)]
    pub warn_free_gb: Option<f64>,
}

impl Config {
    pub fn folder(&self, data_dir: &Path) -> PathBuf {
        match self.folder.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            Some(f) => data_dir.join(f), // an absolute path replaces the data folder
            None => data_dir.join(DEFAULT_FOLDER),
        }
    }

    pub fn warn_free_bytes(&self) -> u64 {
        match self.warn_free_gb {
            Some(gb) if gb.is_finite() && gb >= 0.0 => (gb * GIB) as u64,
            _ => WARN_FREE_BYTES,
        }
    }
}

/// Reads `recording.json`; missing means the defaults. Invalid is an error and the caller
/// keeps the settings it has.
pub fn load_config(dir: &Path) -> Result<Config, String> {
    let bytes = match std::fs::read(dir.join(CONFIG_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config { version: CONFIG_VERSION, ..Config::default() });
        }
        Err(e) => return Err(format!("can't read {CONFIG_FILE}: {e}")),
    };
    let c: Config =
        serde_json::from_slice(&bytes).map_err(|e| format!("{CONFIG_FILE} is not valid (line {})", e.line()))?;
    if c.version != CONFIG_VERSION {
        return Err(format!("{CONFIG_FILE} has version {}, expected {CONFIG_VERSION}", c.version));
    }
    Ok(c)
}

/// Whether the disk counts as low. Once low it stays low until there is a little more than
/// the limit free, so a disk hovering around it doesn't flip the warning on and off.
pub fn is_low(free: u64, warn_below: u64, was_low: bool) -> bool {
    free < warn_below || (was_low && free < warn_below.saturating_add(warn_below / 20))
}

/// Free space for the disk `folder` is on, even if `folder` doesn't exist yet.
pub fn free_space(folder: &Path) -> Option<u64> {
    folder.ancestors().find(|p| p.exists()).and_then(|p| fs4::available_space(p).ok())
}

/// The set being recorded, as `recording-current.json` keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Current {
    /// The set's folder name inside the recordings folder.
    pub set: String,
    pub started_ms: u64,
}

pub fn load_current(data_dir: &Path) -> Option<Current> {
    serde_json::from_slice(&std::fs::read(data_dir.join(CURRENT_FILE)).ok()?).ok()
}

fn save_current(data_dir: &Path, c: &Current) -> std::io::Result<()> {
    crate::write_atomic(&data_dir.join(CURRENT_FILE), &serde_json::to_vec_pretty(c).map_err(std::io::Error::other)?)
}

/// The recording is over: the next one starts a new set, and this one can be converted.
pub fn finish(data_dir: &Path) {
    let _ = std::fs::remove_file(data_dir.join(CURRENT_FILE));
}

/// A date and time in UTC, from milliseconds since the Unix epoch, as `2026-10-10_15-30-12Z`.
/// UTC because the standard library has no local time zone; the name only has to be unique
/// and sort in order. The web app labels recordings by service.
pub fn set_name(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}_{:02}-{:02}-{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn part_name(n: u32, ext: &str) -> String {
    format!("part-{n:03}.{ext}")
}

/// The number in `part-007.mkv`, for the given extension.
fn part_number(name: &str, ext: &str) -> Option<u32> {
    name.strip_prefix("part-")?.strip_suffix(ext)?.strip_suffix('.')?.parse().ok()
}

/// The parts with this extension in a set, in order.
pub fn parts(set_dir: &Path, ext: &str) -> Vec<(u32, PathBuf)> {
    let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(set_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((part_number(&e.file_name().to_string_lossy(), ext)?, e.path())))
        .collect();
    found.sort();
    found
}

/// Where the next file goes: the file to write, in a set that is carried on after a crash
/// or started now. Parts that never got any data (killed before the first byte) are removed
/// first, so they don't pile up.
pub struct Opened {
    pub set: String,
    pub set_dir: PathBuf,
    pub file: PathBuf,
    pub part: u32,
    /// True when this carries on an earlier set.
    pub resumed: bool,
}

pub fn open_part(data_dir: &Path, folder: &Path, now_ms: u64) -> std::io::Result<Opened> {
    std::fs::create_dir_all(folder)?;
    let existing = load_current(data_dir).filter(|c| folder.join(&c.set).is_dir());
    let (current, resumed) = match existing {
        Some(c) => (c, true),
        None => {
            // Two recordings in the same second get their own folder.
            let base = set_name(now_ms);
            let set = (1..)
                .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
                .find(|name| !folder.join(name).exists())
                .unwrap_or(base);
            let c = Current { set, started_ms: now_ms };
            std::fs::create_dir_all(folder.join(&c.set))?;
            save_current(data_dir, &c)?;
            (c, false)
        }
    };
    let set_dir = folder.join(&current.set);
    for (_, path) in parts(&set_dir, "mkv") {
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == 0) {
            let _ = std::fs::remove_file(path);
        }
    }
    let part = parts(&set_dir, "mkv").last().map_or(1, |(n, _)| n + 1);
    let file = set_dir.join(part_name(part, "mkv"));
    Ok(Opened { set: current.set, set_dir, file, part, resumed })
}

/// A part waiting to become MP4.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversion {
    pub mkv: PathBuf,
    pub mp4: PathBuf,
}

/// What is left to convert: the parts of every finished set that have no MP4 yet. The set
/// being recorded is never touched.
pub fn to_convert(data_dir: &Path, folder: &Path) -> Vec<Conversion> {
    let current = load_current(data_dir).map(|c| c.set);
    let mut sets: Vec<PathBuf> =
        std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    sets.sort();
    let mut out = Vec::new();
    for set in sets {
        if set.file_name().map(|n| n.to_string_lossy().into_owned()) == current {
            continue;
        }
        for (n, mkv) in parts(&set, "mkv") {
            let mp4 = set.join(part_name(n, "mp4"));
            if !mp4.exists() {
                out.push(Conversion { mkv, mp4 });
            }
        }
    }
    out
}

/// `recording` in `media-status.json`: what the System screen shows.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub wanted: bool,
    /// "off", "recording", or "waiting" (asked for, but nothing is being written yet).
    pub status: &'static str,
    pub detail: String,
    /// Where recordings are kept.
    pub folder: String,
    /// The set being recorded (folder name), empty when off.
    pub set: String,
    /// The file being written.
    pub file: String,
    /// Files written in this set so far, this one included.
    pub parts: u32,
    /// Bytes handed to the file being written.
    pub bytes: u64,
    /// Free space on the recording disk, GB; None if it can't be read.
    pub free_gb: Option<f64>,
    pub low_disk: bool,
    /// Settings that couldn't be read (the last good ones are kept).
    pub problems: Vec<String>,
    /// What the converter to MP4 is doing: empty when idle.
    pub converting: String,
    /// Parts converted since this process started.
    pub converted: u32,
}

/// Wording for the problems a recording can run into, for the screen. The raw text goes
/// to the log.
pub fn plain_error(raw: &str) -> String {
    let r = raw.to_ascii_lowercase();
    if r.contains("recordings folder") {
        "Can't use the recordings folder. Check recording.json, and that the folder can be created and written to."
            .into()
    } else if r.contains("no space left")
        || r.contains("disk full")
        || r.contains("not enough space")
        || r.contains("os error 112")
        || r.contains("os error 28")
    {
        "The disk is full. Free some space; the recording carries on by itself.".into()
    } else if r.contains("permission denied") || r.contains("access is denied") || r.contains("os error 5") {
        "Can't write to the recordings folder. Check that it exists and that Jivvy Live may save there.".into()
    } else if r.contains("isn't producing video") {
        "Waiting for the program video to start.".into()
    } else if r.contains("keep up") {
        "The disk was too slow to keep up. Starting a new file.".into()
    } else {
        "The recording hit a problem and is starting a new file in the same recording.".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jivvy-rec-{name}-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sets_are_named_by_date_and_time_in_utc() {
        assert_eq!(set_name(0), "1970-01-01_00-00-00Z");
        assert_eq!(set_name(1_791_646_212_000), "2026-10-10_15-30-12Z");
        assert_eq!(set_name(951_782_400_000), "2000-02-29_00-00-00Z", "a leap day");
        assert_eq!(set_name(1_709_251_199_000), "2024-02-29_23-59-59Z");
        assert!(set_name(1_000) < set_name(2_000_000_000_000), "names sort in time order");
    }

    #[test]
    fn the_folder_is_inside_the_data_folder_unless_one_is_named() {
        let data = Path::new("data");
        assert_eq!(Config::default().folder(data), data.join("recordings"));
        let c = Config { folder: Some("  ".into()), ..Config::default() };
        assert_eq!(c.folder(data), data.join("recordings"), "blank means the default");
        let c = Config { folder: Some("services".into()), ..Config::default() };
        assert_eq!(c.folder(data), data.join("services"));
        let elsewhere = std::env::temp_dir().join("elsewhere");
        let c = Config { folder: Some(elsewhere.to_string_lossy().into_owned()), ..Config::default() };
        assert_eq!(c.folder(data), elsewhere, "an absolute path is used as it is");
    }

    #[test]
    fn the_config_is_optional_but_a_broken_one_is_an_error_not_a_reset() {
        let d = dir("config");
        assert_eq!(load_config(&d).unwrap().warn_free_bytes(), WARN_FREE_BYTES);
        std::fs::write(d.join(CONFIG_FILE), r#"{"version":1,"folder":"x","warnFreeGb":2.5,"future":true}"#).unwrap();
        let c = load_config(&d).unwrap();
        assert_eq!((c.folder.as_deref(), c.warn_free_bytes()), (Some("x"), (2.5 * GIB) as u64));
        std::fs::write(d.join(CONFIG_FILE), r#"{"version":1,"warnFreeGb":-1}"#).unwrap();
        assert_eq!(load_config(&d).unwrap().warn_free_bytes(), WARN_FREE_BYTES, "nonsense falls back to 10 GB");
        std::fs::write(d.join(CONFIG_FILE), "{").unwrap();
        assert!(load_config(&d).unwrap_err().contains("not valid"));
        std::fs::write(d.join(CONFIG_FILE), r#"{"version":2}"#).unwrap();
        assert!(load_config(&d).unwrap_err().contains("version 2"));
    }

    #[test]
    fn the_disk_warning_starts_below_the_limit_and_clears_only_with_a_little_to_spare() {
        let limit = WARN_FREE_BYTES;
        assert!(!is_low(limit, limit, false), "exactly 10 GB free is not low");
        assert!(is_low(limit - 1, limit, false));
        assert!(is_low(limit, limit, true), "once low it stays low at the limit");
        assert!(is_low(limit + limit / 20 - 1, limit, true));
        assert!(!is_low(limit + limit / 20, limit, true));
        assert!(!is_low(limit + limit / 20 - 1, limit, false), "a disk that was fine isn't warned about");
    }

    #[test]
    fn free_space_is_read_for_a_folder_that_does_not_exist_yet() {
        let d = dir("free");
        assert!(free_space(&d.join("not").join("there")).is_some_and(|b| b > 0));
    }

    #[test]
    fn a_crash_carries_on_in_the_same_set_and_stop_starts_a_new_one() {
        let data = dir("resume");
        let folder = data.join("recordings");
        let a = open_part(&data, &folder, 1_791_646_212_000).unwrap();
        assert_eq!((a.set.as_str(), a.part, a.resumed), ("2026-10-10_15-30-12Z", 1, false));
        assert!(a.file.ends_with("part-001.mkv"));
        std::fs::write(&a.file, b"data").unwrap();

        // The video process dies and starts again, minutes later: same set, next part.
        let b = open_part(&data, &folder, 1_791_646_500_000).unwrap();
        assert_eq!((b.set.clone(), b.part, b.resumed), (a.set.clone(), 2, true));
        std::fs::write(&b.file, b"more").unwrap();
        assert_eq!(parts(&b.set_dir, "mkv").len(), 2);

        // Stopped: the next recording is a set of its own.
        finish(&data);
        let c = open_part(&data, &folder, 1_791_646_900_000).unwrap();
        assert_eq!((c.set.as_str(), c.part, c.resumed), ("2026-10-10_15-41-40Z", 1, false));
    }

    #[test]
    fn parts_that_never_got_a_byte_are_dropped_and_their_numbers_reused() {
        let data = dir("empty");
        let folder = data.join("recordings");
        let a = open_part(&data, &folder, 5_000).unwrap();
        std::fs::write(&a.file, b"").unwrap(); // killed before the first write
        let b = open_part(&data, &folder, 6_000).unwrap();
        assert_eq!((b.part, parts(&b.set_dir, "mkv").len()), (1, 0), "the empty file is gone, its number reused");
    }

    #[test]
    fn two_recordings_in_the_same_second_never_share_a_folder() {
        let data = dir("same-second");
        let folder = data.join("recordings");
        let a = open_part(&data, &folder, 5_000).unwrap();
        finish(&data);
        let b = open_part(&data, &folder, 5_400).unwrap();
        assert_ne!(a.set, b.set);
        assert!(b.set.starts_with(&a.set));
    }

    #[test]
    fn a_marker_pointing_at_a_deleted_set_starts_a_new_one() {
        let data = dir("deleted");
        let folder = data.join("recordings");
        let a = open_part(&data, &folder, 5_000).unwrap();
        std::fs::remove_dir_all(&a.set_dir).unwrap();
        let b = open_part(&data, &folder, 9_000_000).unwrap();
        assert_eq!((b.resumed, b.part), (false, 1));
    }

    #[test]
    fn only_finished_sets_without_an_mp4_are_converted() {
        let data = dir("convert");
        let folder = data.join("recordings");
        let old = folder.join("2026-10-04_15-00-00Z");
        std::fs::create_dir_all(&old).unwrap();
        for f in ["part-001.mkv", "part-002.mkv", "part-001.mp4", "notes.txt", "part-003.converting.mp4"] {
            std::fs::write(old.join(f), b"x").unwrap();
        }
        let a = open_part(&data, &folder, 1_791_646_212_000).unwrap(); // being recorded now
        std::fs::write(&a.file, b"x").unwrap();
        let todo = to_convert(&data, &folder);
        assert_eq!(todo, vec![Conversion { mkv: old.join("part-002.mkv"), mp4: old.join("part-002.mp4") }]);
        finish(&data);
        assert_eq!(to_convert(&data, &folder).len(), 2, "the set just recorded is ready now");
    }

    #[test]
    fn the_screen_gets_words_not_error_codes() {
        assert!(plain_error("No space left on device (os error 28)").contains("disk is full"));
        assert!(plain_error("There is not enough space (os error 112)").contains("disk is full"));
        assert!(plain_error("Access is denied. (os error 5)").contains("Can't write"));
        assert!(
            plain_error("can't use the recordings folder: File exists (os error 80)").contains("recordings folder")
        );
        assert!(plain_error("the disk couldn't keep up").contains("too slow"));
        assert!(plain_error("something odd").contains("new file in the same recording"));
    }
}
