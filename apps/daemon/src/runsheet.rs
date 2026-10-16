//! The run sheet on disk (`runsheet.json` in the data directory), so a restarted engine shows
//! the same service. Saved like the snapshot: temp file, flushed to disk, renamed over.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::protocol::RunSheet;

const FILE: &str = "runsheet.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub version: u32,
    /// Goes up by one every time a run sheet is set.
    pub rev: u64,
    pub run_sheet: RunSheet,
}

/// The saved run sheet, if there is a readable one. A broken file is logged and ignored: the
/// screens show slide numbers until the next `runsheet.set`, rather than the engine not starting.
pub fn load(dir: &Path) -> Option<Saved> {
    let bytes = fs::read(dir.join(FILE)).ok()?;
    match serde_json::from_slice::<Saved>(&bytes) {
        Ok(s) if s.version == 1 => Some(s),
        Ok(s) => {
            crate::log("engine", format!("{FILE} has version {}; ignoring it", s.version));
            None
        }
        Err(e) => {
            crate::log("engine", format!("{FILE} can't be read ({e}); ignoring it"));
            None
        }
    }
}

/// Saves atomically and durably; returns only once the data is on disk.
pub fn save(dir: &Path, saved: &Saved) -> std::io::Result<()> {
    let tmp = dir.join("runsheet.json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&serde_json::to_vec(saved)?)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(FILE))?;
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all(); // persist the rename itself
    }
    Ok(())
}
