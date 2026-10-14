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

#[derive(Debug, PartialEq)]
pub enum PutError {
    /// Pieces must come in order; this is where the next one has to start.
    Offset(u64),
    TooBig,
    /// The bytes don't hash to the id, or aren't an image: start again from 0.
    Corrupt(&'static str),
    Io(String),
}

/// Adds one piece of image `id` at `offset` (0 starts again); with `last`, checks it and puts
/// it in place. An image already complete is acknowledged as it is.
pub fn put(data_dir: &Path, id: &str, offset: u64, bytes: &[u8], last: bool) -> Result<MediaProgress, PutError> {
    let io = |e: std::io::Error| PutError::Io(e.to_string());
    let done = path(data_dir, id);
    if let Ok(meta) = fs::metadata(&done) {
        return Ok(MediaProgress { id: id.into(), received: meta.len(), complete: true });
    }
    fs::create_dir_all(dir(data_dir)).map_err(io)?;
    let part = dir(data_dir).join(format!("{id}.part"));
    let have = if offset == 0 { 0 } else { fs::metadata(&part).map(|m| m.len()).unwrap_or(0) };
    if offset != have {
        return Err(PutError::Offset(have));
    }
    if have + bytes.len() as u64 > MAX_MEDIA_BYTES {
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
    file.write_all(bytes).map_err(io)?;
    let received = have + bytes.len() as u64;
    if !last {
        return Ok(MediaProgress { id: id.into(), received, complete: false });
    }
    file.sync_all().map_err(io)?;
    drop(file);
    let all = fs::read(&part).map_err(io)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &all);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let problem = if hex != id {
        Some("the image didn't arrive intact; send it again")
    } else if content_type(&all).is_none() {
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
        assert!(put(&dir, &id, 0, &jpeg[..3], false).is_ok());
        assert_eq!(serve(&dir, &format!("/{id}")), None, "not while it is still arriving");
        assert_eq!(put(&dir, &id, 1, &jpeg[3..], true), Err(PutError::Offset(3)));
        assert!(put(&dir, &id, 3, &jpeg[3..], true).unwrap().complete);
        assert_eq!(serve(&dir, &format!("/{id}")), Some(("image/jpeg", jpeg.to_vec())));
        assert_eq!(serve(&dir, "/../state.json"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
