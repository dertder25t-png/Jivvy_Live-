//! The church computer's library: songs, plans, themes and settings, in one SQLite file in the
//! data directory (`library.sqlite3`). It is the master copy during a service and the only
//! copy for churches without Plus; the cloud, later, is a synced copy. Each entry is a document
//! of a kind with an id, a title and a JSON body the UI shapes. Every save bumps its revision,
//! so a save based on an older copy can be refused instead of losing another device's edit.
//! Every save and delete also notes which fields changed, for the cloud sync (`sync.rs`).

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value};

use crate::protocol::{Doc, DocInfo};
use crate::sync::DocState;

pub const FILE: &str = "library.sqlite3";

pub struct Library {
    db: Connection,
}

#[derive(Debug, PartialEq)]
pub enum Error {
    /// The saved copy isn't the revision the change was based on (0: it already exists).
    Conflict {
        current: u64,
    },
    Db(String),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e.to_string())
    }
}

impl Library {
    pub fn open(data_dir: &Path) -> Result<Self, Error> {
        let db = Connection::open(data_dir.join(FILE))?;
        // WAL survives a crash mid-write; FULL syncs each commit, so a saved song is on disk
        // before the ack, like the run sheet.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS docs (
                kind TEXT NOT NULL,
                id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                rev INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                -- A deleted document stays as a marker, so its revisions keep counting up and a
                -- copy from before the delete can never match a document made again later.
                deleted INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (kind, id)
            )",
        )?;
        // A library made before deletes left markers: add the column, every row still there.
        let has_deleted: bool =
            db.query_row("SELECT COUNT(*) > 0 FROM pragma_table_info('docs') WHERE name = 'deleted'", [], |r| {
                r.get(0)
            })?;
        if !has_deleted {
            db.execute_batch("ALTER TABLE docs ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0")?;
        }
        let mut lib = Library { db };
        crate::sync::open(&mut lib.db, crate::now_ms())?;
        Ok(lib)
    }

    /// For the sync engine, which keeps its tables in this file.
    pub(crate) fn conn(&mut self) -> &mut Connection {
        &mut self.db
    }

    pub(crate) fn db(&self) -> &Connection {
        &self.db
    }

    /// A document as saved, deleted or not, and its revision.
    pub(crate) fn read_state(db: &Connection, kind: &str, id: &str) -> rusqlite::Result<Option<(DocState, u64)>> {
        db.query_row("SELECT title, body, deleted, rev FROM docs WHERE kind = ?1 AND id = ?2", params![kind, id], |r| {
            let body: String = r.get(1)?;
            let state = DocState {
                title: r.get(0)?,
                body: serde_json::from_str::<Map<String, Value>>(&body).unwrap_or_default(),
                deleted: r.get(2)?,
            };
            Ok((state, r.get::<_, u64>(3)?))
        })
        .optional()
    }

    /// Saves a document as the sync engine merged it (one revision on, so every screen's copy
    /// is now old). A deleted one keeps only its marker.
    pub(crate) fn write_state(
        db: &Connection,
        kind: &str,
        id: &str,
        state: &DocState,
        now: u64,
    ) -> rusqlite::Result<DocInfo> {
        let rev = Self::read_state(db, kind, id)?.map_or(0, |(_, rev)| rev) + 1;
        let body = if state.deleted { "{}".to_string() } else { Value::Object(state.body.clone()).to_string() };
        db.execute(
            "INSERT INTO docs (kind, id, title, body, rev, updated_at, deleted) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (kind, id) DO UPDATE SET title = ?3, body = ?4, rev = ?5, updated_at = ?6, deleted = ?7",
            params![kind, id, state.title, body, rev, now, state.deleted],
        )?;
        Ok(DocInfo { kind: kind.into(), id: id.into(), title: state.title.clone(), rev, updated_at: now })
    }

    /// The documents of a kind, newest first, without bodies.
    pub fn list(&self, kind: &str) -> Result<Vec<DocInfo>, Error> {
        let mut q = self.db.prepare_cached(
            "SELECT id, title, rev, updated_at FROM docs WHERE kind = ?1 AND deleted = 0 ORDER BY updated_at DESC, id",
        )?;
        let rows = q.query_map(params![kind], |r| {
            Ok(DocInfo {
                kind: kind.to_string(),
                id: r.get(0)?,
                title: r.get(1)?,
                rev: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, kind: &str, id: &str) -> Result<Option<Doc>, Error> {
        let row = self
            .db
            .query_row(
                "SELECT title, body, rev, updated_at FROM docs WHERE kind = ?1 AND id = ?2 AND deleted = 0",
                params![kind, id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u64>(2)?, r.get::<_, u64>(3)?)),
            )
            .optional()?;
        Ok(row.map(|(title, body, rev, updated_at)| Doc {
            info: DocInfo { kind: kind.into(), id: id.into(), title, rev, updated_at },
            // Only this code writes bodies, always objects; a damaged one reads as empty.
            body: serde_json::from_str::<Map<String, Value>>(&body).unwrap_or_default(),
        }))
    }

    /// Every picture id (64 hex characters) named anywhere in a saved document, so pruning
    /// keeps the pictures plans and themes use.
    /// Reads them with a connection of its own that only reads (and waits if the library is
    /// busy), so it can run beside the engine's.
    pub fn scan_media_ids(data_dir: &Path) -> Result<std::collections::HashSet<String>, Error> {
        let db = Connection::open_with_flags(
            data_dir.join(FILE),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        Library { db }.media_ids()
    }

    fn media_ids(&self) -> Result<std::collections::HashSet<String>, Error> {
        fn walk(v: &Value, out: &mut std::collections::HashSet<String>) {
            match v {
                Value::String(_) => out.extend(crate::protocol::media_id(Some(v))),
                Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                Value::Object(o) => o.values().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut stmt = self.db.prepare("SELECT body FROM docs WHERE deleted = 0")?;
        let bodies = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for body in bodies {
            if let Ok(v) = serde_json::from_str::<Value>(&body?) {
                walk(&v, &mut out);
            }
        }
        Ok(out)
    }

    /// The highest revision this id has had (deleted or not), and whether it exists now.
    fn revision(db: &Connection, kind: &str, id: &str) -> Result<(u64, bool), Error> {
        let row = db
            .query_row("SELECT rev, deleted FROM docs WHERE kind = ?1 AND id = ?2", params![kind, id], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, bool>(1)?))
            })
            .optional()?;
        Ok(row.map_or((0, false), |(rev, deleted)| (rev, !deleted)))
    }

    /// Saves a document; with `base_rev`, only if the saved copy is that revision (0: new).
    pub fn put(
        &mut self,
        kind: &str,
        id: &str,
        title: &str,
        body: &Map<String, Value>,
        base_rev: Option<u64>,
        now: u64,
    ) -> Result<DocInfo, Error> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (last, exists) = Self::revision(&tx, kind, id)?;
        // A deleted one counts as not there (0), but its revisions carry on from where they were.
        let current = if exists { last } else { 0 };
        if base_rev.is_some_and(|b| b != current) {
            return Err(Error::Conflict { current });
        }
        let rev = last + 1;
        let json = serde_json::to_string(body).map_err(|e| Error::Db(e.to_string()))?;
        let old = Self::read_state(&tx, kind, id)?.map(|(s, _)| s);
        tx.execute(
            "INSERT INTO docs (kind, id, title, body, rev, updated_at, deleted) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
             ON CONFLICT (kind, id) DO UPDATE SET title = ?3, body = ?4, rev = ?5, updated_at = ?6, deleted = 0",
            params![kind, id, title, json, rev, now],
        )?;
        let new = DocState { title: title.into(), body: body.clone(), deleted: false };
        crate::sync::record_local(&tx, kind, id, old.as_ref(), &new, now)?;
        tx.commit()?;
        Ok(DocInfo { kind: kind.into(), id: id.into(), title: title.into(), rev, updated_at: now })
    }

    /// Removes a document (leaving a marker, one revision on); with `base_rev`, only that
    /// revision. None if there was none.
    pub fn delete(&mut self, kind: &str, id: &str, base_rev: Option<u64>, now: u64) -> Result<Option<DocInfo>, Error> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((title, rev)) = tx
            .query_row(
                "SELECT title, rev FROM docs WHERE kind = ?1 AND id = ?2 AND deleted = 0",
                params![kind, id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)),
            )
            .optional()?
        else {
            return match base_rev {
                Some(b) if b != 0 => Err(Error::Conflict { current: 0 }),
                _ => Ok(None),
            };
        };
        if base_rev.is_some_and(|b| b != rev) {
            return Err(Error::Conflict { current: rev });
        }
        tx.execute(
            "UPDATE docs SET deleted = 1, body = '{}', rev = ?3, updated_at = ?4 WHERE kind = ?1 AND id = ?2",
            params![kind, id, rev + 1, now],
        )?;
        let gone = DocState { title: title.clone(), body: Map::new(), deleted: true };
        crate::sync::record_local(&tx, kind, id, None, &gone, now)?;
        tx.commit()?;
        Ok(Some(DocInfo { kind: kind.into(), id: id.into(), title, rev: rev + 1, updated_at: now }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_lists_and_refuses_a_change_based_on_an_old_copy() {
        let dir = std::env::temp_dir().join(format!("jivvy-lib-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = |t: &str| serde_json::json!({ "text": t }).as_object().unwrap().clone();
        let mut lib = Library::open(&dir).unwrap();
        let first = lib.put("song", "grace", "Amazing Grace", &body("v1"), Some(0), 1).unwrap();
        assert_eq!(first.rev, 1);
        // Two devices edit revision 1: the first save wins, the second is told.
        assert_eq!(lib.put("song", "grace", "Amazing Grace", &body("mine"), Some(1), 2).unwrap().rev, 2);
        assert_eq!(lib.put("song", "grace", "x", &body("theirs"), Some(1), 3), Err(Error::Conflict { current: 2 }));
        assert_eq!(lib.put("song", "grace", "x", &body("new"), Some(0), 3), Err(Error::Conflict { current: 2 }));
        lib.put("plan", "sun", "Sunday", &body("plan"), None, 4).unwrap();

        // Survives closing and opening again (a restart).
        drop(lib);
        let mut lib = Library::open(&dir).unwrap();
        let songs = lib.list("song").unwrap();
        assert_eq!((songs.len(), songs[0].rev, songs[0].title.as_str()), (1, 2, "Amazing Grace"));
        assert_eq!(lib.get("song", "grace").unwrap().unwrap().body, body("mine"));
        assert!(lib.get("song", "nope").unwrap().is_none());

        assert_eq!(lib.delete("song", "grace", Some(1), 5), Err(Error::Conflict { current: 2 }));
        assert_eq!(lib.delete("song", "grace", Some(2), 5).unwrap().unwrap().rev, 3);
        assert!(lib.list("song").unwrap().is_empty() && lib.get("song", "grace").unwrap().is_none());
        assert_eq!(lib.delete("song", "grace", None, 6), Ok(None), "deleting one that's gone is fine");
        // Made again after the delete: it is new (base 0), but its revisions carry on, so a copy
        // from before the delete (revision 1 or 2) can never overwrite it.
        assert_eq!(lib.put("song", "grace", "Again", &body("new"), Some(0), 7).unwrap().rev, 4);
        assert_eq!(lib.put("song", "grace", "x", &body("stale"), Some(2), 8), Err(Error::Conflict { current: 4 }));
        drop(lib);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_from_before_delete_markers_is_upgraded_and_keeps_its_songs() {
        let dir = std::env::temp_dir().join(format!("jivvy-lib-old-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = Connection::open(dir.join(FILE)).unwrap();
        old.execute_batch(
            "CREATE TABLE docs (kind TEXT NOT NULL, id TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
                rev INTEGER NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (kind, id));
             INSERT INTO docs VALUES ('song', 'grace', 'Amazing Grace', '{}', 3, 1);",
        )
        .unwrap();
        drop(old);
        let mut lib = Library::open(&dir).unwrap();
        assert_eq!(lib.list("song").unwrap()[0].rev, 3);
        assert_eq!(lib.put("song", "grace", "Amazing Grace", &Map::new(), Some(3), 2).unwrap().rev, 4);
        drop(lib);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
