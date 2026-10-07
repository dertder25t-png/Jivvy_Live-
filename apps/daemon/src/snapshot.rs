//! The engine's live state on disk, so a restarted engine resumes on the same slide.
//!
//! Saved before every change is acknowledged (stricter than the plan's "every second").
//! Each save writes a temp file, flushes it to disk and renames it over `state.json`;
//! the previous good file is kept as `state.prev.json`. A crash mid-save therefore
//! leaves at least one complete file, and a corrupted file falls back to the other.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: u32,
    pub slide_index: u64,
    pub black: bool,
    /// The operator asked for the stream to be on; a restart resumes it.
    pub stream_wanted: bool,
    pub saved_at: u64,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot { version: SNAPSHOT_VERSION, slide_index: 0, black: false, stream_wanted: false, saved_at: 0 }
    }
}

/// Where a loaded snapshot came from, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Current,
    Previous,
    Default,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Store { dir })
    }

    pub fn current_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn previous_path(&self) -> PathBuf {
        self.dir.join("state.prev.json")
    }

    fn read(path: &Path) -> Option<Snapshot> {
        let s: Snapshot = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        // A newer daemon's snapshot we can't understand is treated like a missing one.
        (s.version == SNAPSHOT_VERSION).then_some(s)
    }

    /// Loads the newest readable snapshot: current, then previous, then defaults.
    pub fn load(&self) -> (Snapshot, Source) {
        if let Some(s) = Self::read(&self.current_path()) {
            return (s, Source::Current);
        }
        if let Some(s) = Self::read(&self.previous_path()) {
            return (s, Source::Previous);
        }
        (Snapshot::default(), Source::Default)
    }

    /// Saves atomically and durably; returns only once the data is on disk.
    pub fn save(&self, snap: &Snapshot) -> std::io::Result<()> {
        let tmp = self.dir.join("state.json.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(snap)?)?;
            f.sync_all()?;
        }
        let cur = self.current_path();
        if Self::read(&cur).is_some() {
            // Keep the last good file; never overwrite it with a corrupted one.
            fs::copy(&cur, self.previous_path())?;
        }
        fs::rename(&tmp, &cur)?;
        #[cfg(unix)]
        if let Ok(d) = File::open(&self.dir) {
            let _ = d.sync_all(); // persist the rename itself
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("jivvy-snap-{name}-{}-{}", std::process::id(), crate::now_ms()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir).unwrap()
    }

    fn snap(i: u64) -> Snapshot {
        Snapshot { slide_index: i, black: i % 2 == 1, saved_at: i, ..Snapshot::default() }
    }

    #[test]
    fn empty_dir_loads_defaults() {
        let s = temp_store("empty");
        assert_eq!(s.load(), (Snapshot::default(), Source::Default));
    }

    #[test]
    fn save_then_load_round_trips_and_keeps_the_previous_file() {
        let s = temp_store("roundtrip");
        s.save(&snap(3)).unwrap();
        s.save(&snap(4)).unwrap();
        assert_eq!(s.load(), (snap(4), Source::Current));
        assert_eq!(Store::read(&s.previous_path()), Some(snap(3)));
    }

    #[test]
    fn corrupted_current_falls_back_to_previous() {
        let s = temp_store("corrupt");
        s.save(&snap(5)).unwrap();
        s.save(&snap(6)).unwrap();
        fs::write(s.current_path(), b"{\"version\":1,\"slideIn").unwrap();
        assert_eq!(s.load(), (snap(5), Source::Previous));
    }

    #[test]
    fn both_corrupted_loads_defaults_and_next_save_recovers() {
        let s = temp_store("both");
        fs::write(s.current_path(), b"garbage").unwrap();
        fs::write(s.previous_path(), b"").unwrap();
        assert_eq!(s.load().1, Source::Default);
        s.save(&snap(2)).unwrap();
        assert_eq!(s.load(), (snap(2), Source::Current));
    }

    #[test]
    fn a_corrupted_current_file_never_replaces_the_good_previous_one() {
        let s = temp_store("keep-prev");
        s.save(&snap(7)).unwrap();
        s.save(&snap(8)).unwrap();
        fs::write(s.current_path(), b"garbage").unwrap();
        s.save(&snap(9)).unwrap();
        assert_eq!(Store::read(&s.previous_path()), Some(snap(7)));
        assert_eq!(s.load(), (snap(9), Source::Current));
    }

    #[test]
    fn unknown_snapshot_version_is_ignored() {
        let s = temp_store("version");
        s.save(&snap(1)).unwrap();
        s.save(&snap(2)).unwrap();
        fs::write(s.current_path(), br#"{"version":99,"slideIndex":9,"black":false,"streamWanted":false,"savedAt":0}"#)
            .unwrap();
        assert_eq!(s.load(), (snap(1), Source::Previous));
    }
}
