//! Images for slides, kept on the church computer under `media/` in the data directory, each
//! named by the SHA-256 of its bytes. A device sends one in pieces (`media.put`); the pieces
//! collect in `<id>.part` and only a complete file whose hash matches its name, and that is a
//! JPEG, PNG or WebP, is renamed into place. Nothing here needs the internet.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::protocol::{MAX_MEDIA_BYTES, MediaProgress};

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("media")
}

/// Where a complete image lives. `id` must already be a valid media id.
pub fn path(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(id)
}

/// Most space all images (complete and arriving) may take: a full service is a few hundred MB
/// at most, and no device can fill the disk the recording needs.
pub const MAX_STORAGE: u64 = 2 * 1024 * 1024 * 1024;

/// Bytes the images folder holds now.
pub fn usage(data_dir: &Path) -> u64 {
    fs::read_dir(dir(data_dir))
        .map(|d| d.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// A piece of an upload this recent is never pruned, even if it started after the list of
/// uploads arriving was taken (pruning runs on its own thread).
const FRESH_PART: std::time::Duration = std::time::Duration::from_secs(60);

/// Removes images not in `keep` (what the run sheet shows, and what devices sent since) and
/// pieces of uploads not arriving now (`arriving`; left by a crash or an abandoned send).
/// `keep` is checked under its lock right before each image is removed, so the engine can add
/// to it while this runs and nothing it adds is ever deleted. Returns how many files went.
pub fn prune(
    data_dir: &Path,
    keep: &std::sync::Mutex<std::collections::HashSet<String>>,
    arriving: &std::collections::HashSet<String>,
) -> usize {
    let Ok(entries) = fs::read_dir(dir(data_dir)) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.metadata().is_ok_and(|m| m.is_file()) {
            continue;
        }
        let gone = match name.strip_suffix(".part") {
            Some(id) => {
                let fresh = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < FRESH_PART));
                !arriving.contains(id) && !fresh && fs::remove_file(entry.path()).is_ok()
            }
            None => {
                let keep = keep.lock().unwrap_or_else(|p| p.into_inner());
                !keep.contains(&name) && fs::remove_file(entry.path()).is_ok()
            }
        };
        if gone {
            removed += 1;
        }
    }
    removed
}

pub fn has(data_dir: &Path, id: &str) -> bool {
    path(data_dir, id).is_file()
}

/// The image's type, from its first bytes; None for anything else.
pub fn content_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// What the output windows' `media` URLs serve: `/<id>` of a complete image here, with its
/// type; None for anything else (an unknown id, a part file, a path that isn't an id).
pub fn serve(data_dir: &Path, url_path: &str) -> Option<(&'static str, Vec<u8>)> {
    let id = url_path.trim_start_matches('/');
    crate::protocol::media_id(Some(&serde_json::Value::String(id.into())))?;
    let bytes = fs::read(path(data_dir, id)).ok()?;
    Some((content_type(&bytes)?, bytes))
}

/// Images arriving now: each one's hash so far and its first bytes (for its type), so the last
/// piece is checked without reading the whole file again under the engine's lock. Kept in
/// memory: after a restart an upload starts again from 0.
#[derive(Default)]
pub struct Uploads(std::collections::HashMap<String, Upload>, u64);

/// Most uploads in progress at once; starting another drops the one idle longest. A real send
/// does one at a time, so this only stops a device piling up half-uploads.
pub const MAX_ARRIVING: usize = 8;

impl Uploads {
    /// The ids arriving now, for pruning on another thread.
    pub fn ids(&self) -> std::collections::HashSet<String> {
        self.0.keys().cloned().collect()
    }
}

struct Upload {
    /// When it last took a piece (a counter, not a clock), to find the one idle longest.
    touched: u64,
    received: u64,
    hash: ring::digest::Context,
    head: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum PutError {
    /// Pieces must come in order; this is where the next one has to start.
    Offset(u64),
    TooBig,
    /// The bytes don't hash to the id, or aren't an image: start again from 0.
    Corrupt(&'static str),
    /// All images together would pass MAX_STORAGE.
    Full,
    Io(String),
}

/// Adds one piece of image `id` at `offset` (0 starts again); with `last`, checks it and puts
/// it in place. An image already complete is acknowledged as it is.
pub fn put(
    uploads: &mut Uploads,
    data_dir: &Path,
    id: &str,
    offset: u64,
    bytes: &[u8],
    last: bool,
) -> Result<MediaProgress, PutError> {
    let io = |e: std::io::Error| PutError::Io(e.to_string());
    let done = path(data_dir, id);
    if let Ok(meta) = fs::metadata(&done) {
        uploads.0.remove(id);
        return Ok(MediaProgress { id: id.into(), received: meta.len(), complete: true });
    }
    fs::create_dir_all(dir(data_dir)).map_err(io)?;
    let part = dir(data_dir).join(format!("{id}.part"));
    if offset == 0 {
        if !uploads.0.contains_key(id) && uploads.0.len() >= MAX_ARRIVING {
            let idle = uploads.0.iter().min_by_key(|(_, u)| u.touched).map(|(k, _)| k.clone());
            if let Some(idle) = idle {
                uploads.0.remove(&idle);
                let _ = fs::remove_file(dir(data_dir).join(format!("{idle}.part")));
            }
        }
        let fresh = Upload {
            touched: 0,
            received: 0,
            hash: ring::digest::Context::new(&ring::digest::SHA256),
            head: Vec::new(),
        };
        uploads.0.insert(id.into(), fresh);
    }
    // Only an upload this engine has been following can carry on; anything else starts over.
    let have = uploads.0.get(id).map_or(0, |u| u.received);
    if offset != have {
        return Err(PutError::Offset(have));
    }
    if usage(data_dir) + bytes.len() as u64 > MAX_STORAGE {
        return Err(PutError::Full);
    }
    if have + bytes.len() as u64 > MAX_MEDIA_BYTES {
        uploads.0.remove(id);
        let _ = fs::remove_file(&part);
        return Err(PutError::TooBig);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(offset != 0)
        .write(true)
        .truncate(offset == 0)
        .open(&part)
        .map_err(io)?;
    if let Err(e) = file.write_all(bytes) {
        uploads.0.remove(id); // the file may hold part of this piece: start again
        return Err(io(e));
    }
    uploads.1 += 1;
    let tick = uploads.1;
    let upload = uploads.0.get_mut(id).expect("inserted above or checked by offset");
    upload.touched = tick;
    upload.received += bytes.len() as u64;
    upload.hash.update(bytes);
    let room = 12usize.saturating_sub(upload.head.len());
    upload.head.extend_from_slice(&bytes[..room.min(bytes.len())]);
    let received = upload.received;
    if !last {
        return Ok(MediaProgress { id: id.into(), received, complete: false });
    }
    let upload = uploads.0.remove(id).expect("present");
    file.sync_all().map_err(io)?;
    drop(file);
    let hex: String = upload.hash.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let problem = if hex != id {
        Some("the image didn't arrive intact; send it again")
    } else if content_type(&upload.head).is_none() {
        Some("only JPEG, PNG or WebP images")
    } else {
        None
    };
    if let Some(problem) = problem {
        let _ = fs::remove_file(&part);
        return Err(PutError::Corrupt(problem));
    }
    fs::rename(&part, &done).map_err(io)?;
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir(data_dir)) {
        let _ = d.sync_all(); // persist the rename itself
    }
    Ok(MediaProgress { id: id.into(), received, complete: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_images_are_served_by_id() {
        let dir = std::env::temp_dir().join(format!("jivvy-images-{}-{}", std::process::id(), crate::now_ms()));
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
        let id: String =
            ring::digest::digest(&ring::digest::SHA256, &jpeg).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let mut up = Uploads::default();
        assert!(put(&mut up, &dir, &id, 0, &jpeg[..3], false).is_ok());
        assert_eq!(serve(&dir, &format!("/{id}")), None, "not while it is still arriving");
        assert_eq!(put(&mut up, &dir, &id, 1, &jpeg[3..], true), Err(PutError::Offset(3)));
        assert_eq!(
            put(&mut Uploads::default(), &dir, &id, 3, &jpeg[3..], true),
            Err(PutError::Offset(0)),
            "an upload this engine didn't follow (a restart) starts over"
        );
        assert!(put(&mut up, &dir, &id, 3, &jpeg[3..], true).unwrap().complete);
        assert_eq!(serve(&dir, &format!("/{id}")), Some(("image/jpeg", jpeg.to_vec())));
        assert_eq!(serve(&dir, "/../state.json"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_few_uploads_can_be_half_done_at_once() {
        let dir = std::env::temp_dir().join(format!("jivvy-cap-{}-{}", std::process::id(), crate::now_ms()));
        let mut up = Uploads::default();
        let ids: Vec<String> = (0..=MAX_ARRIVING).map(|n| format!("{n:064x}")).collect();
        for id in &ids {
            put(&mut up, &dir, id, 0, &[0xFF, 0xD8], false).unwrap();
        }
        assert_eq!(up.ids().len(), MAX_ARRIVING);
        assert_eq!(put(&mut up, &dir, &ids[0], 2, &[0xFF], true), Err(PutError::Offset(0)), "the idlest was dropped");
        assert!(!super::dir(&dir).join(format!("{}.part", ids[0])).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_keeps_what_the_run_sheet_shows_and_uploads_still_arriving() {
        let dir = std::env::temp_dir().join(format!("jivvy-prune-{}-{}", std::process::id(), crate::now_ms()));
        let hash = |b: &[u8]| -> String {
            ring::digest::digest(&ring::digest::SHA256, b).as_ref().iter().map(|x| format!("{x:02x}")).collect()
        };
        let (shown, unused, arriving) = ([0xFF, 0xD8, 0xFF, 1], [0xFF, 0xD8, 0xFF, 2], [0xFF, 0xD8, 0xFF, 3]);
        let mut up = Uploads::default();
        for img in [&shown, &unused] {
            put(&mut up, &dir, &hash(img), 0, img, true).unwrap();
        }
        put(&mut up, &dir, &hash(&arriving), 0, &arriving[..2], false).unwrap();
        fs::write(super::dir(&dir).join(format!("{}.part", "c".repeat(64))), b"left by a crash").unwrap();

        // The abandoned piece is old; one that started a moment ago is left alone.
        let old = super::dir(&dir).join(format!("{}.part", "c".repeat(64)));
        let a_while_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        fs::File::options().write(true).open(&old).unwrap().set_modified(a_while_ago).unwrap();
        fs::write(super::dir(&dir).join(format!("{}.part", "d".repeat(64))), b"just started").unwrap();

        let keep = std::sync::Mutex::new(std::collections::HashSet::from([hash(&shown)]));
        assert_eq!(prune(&dir, &keep, &up.ids()), 2, "the unused image and the abandoned piece");
        assert!(has(&dir, &hash(&shown)) && !has(&dir, &hash(&unused)));
        assert!(put(&mut up, &dir, &hash(&arriving), 2, &arriving[2..], true).unwrap().complete, "still arriving");
        let _ = fs::remove_dir_all(&dir);
    }
}
