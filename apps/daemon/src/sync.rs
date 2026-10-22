//! Sync between the church computer's library and the church's cloud copy (Plus only).
//!
//! The library on this computer stays the master copy: a service never waits on the cloud, and
//! a church without Plus (or offline) keeps everything here. Sync runs on its own thread with
//! its own connection to `library.sqlite3`, never holds the engine's lock across a network
//! call, and waits while a service is on.
//!
//! How it works:
//! - Every local save or delete notes which fields changed (`title`, `deleted`, and each
//!   top-level `body.<key>`), each stamped with a hybrid logical clock (wall-clock ms, a
//!   counter, this computer's node id) and marked dirty. The dirty fields are the outbox, so
//!   ten edits to one song offline send one change per field.
//! - A round pulls the cloud's changes since the last cursor, merges them per field (the later
//!   clock wins), then pushes the outbox. The cloud keeps a field only if its clock is later
//!   than the one it has, so every copy ends on the same value.
//! - A field changed here and elsewhere before syncing is a "changed elsewhere" notice. When a
//!   song's words (sections, arrangement) changed in both places, this computer keeps its own
//!   words and saves the other version as a new song, "<title> (changed elsewhere)".
//! - A top-level body value of `null` counts as removed.
//!
//! The cloud is behind `Transport`; `MemoryCloud` is an in-memory stand-in for tests. Nothing
//! here logs a token: transport errors carry plain words only.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::library::Library;
use crate::protocol::{DocInfo, MAX_TITLE};

/// Notices kept for `sync.status`.
pub const MAX_NOTICES: usize = 20;
/// Changes asked for (and sent) per request.
const BATCH: usize = 200;
/// A song's words: when both copies changed these, both versions are kept.
const LYRIC_FIELDS: [&str; 2] = ["body.sections", "body.arrangement"];
/// How often a round runs while all is well, and the longest wait after failures.
const EVERY: Duration = Duration::from_secs(15);
const IDLE: Duration = Duration::from_secs(10);
const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(300);

/// A document as the sync engine sees it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DocState {
    pub title: String,
    pub body: Map<String, Value>,
    pub deleted: bool,
}

impl DocState {
    /// The fields that sync, by name. A null body value is a removed one.
    fn fields(&self) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::from([
            ("title".to_string(), Value::String(self.title.clone())),
            ("deleted".to_string(), Value::Bool(self.deleted)),
        ]);
        for (k, v) in self.body.iter().filter(|(_, v)| !v.is_null()) {
            out.insert(format!("body.{k}"), v.clone());
        }
        out
    }

    fn get(&self, field: &str) -> Value {
        match field {
            "title" => Value::String(self.title.clone()),
            "deleted" => Value::Bool(self.deleted),
            f => f.strip_prefix("body.").and_then(|k| self.body.get(k)).cloned().unwrap_or(Value::Null),
        }
    }

    fn set(&mut self, field: &str, value: &Value) {
        match (field, value) {
            ("title", Value::String(t)) => self.title = t.clone(),
            ("deleted", Value::Bool(d)) => self.deleted = *d,
            (f, v) => {
                if let Some(k) = f.strip_prefix("body.") {
                    if v.is_null() {
                        self.body.remove(k);
                    } else {
                        self.body.insert(k.to_string(), v.clone());
                    }
                }
            }
        }
    }
}

/// One field of one document, as it travels to and from the cloud.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldChange {
    pub kind: String,
    pub id: String,
    pub field: String,
    pub value: Value,
    /// The hybrid logical clock it was stamped with; later sorts higher as text.
    pub hlc: String,
}

/// Why a round stopped. Plain words only: never a token or a URL with one.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncError {
    /// No internet, or the cloud didn't answer.
    Offline,
    /// The cloud answered but refused.
    Cloud(String),
    /// This computer's library couldn't be read or written.
    Local(String),
}

impl From<rusqlite::Error> for SyncError {
    fn from(e: rusqlite::Error) -> Self {
        SyncError::Local(e.to_string())
    }
}

/// The church's cloud copy.
pub trait Transport: Send {
    /// Saves these; the cloud keeps each field only if its `hlc` is later than the one it has.
    fn push(&mut self, changes: &[FieldChange]) -> Result<(), SyncError>;
    /// Fields saved after `cursor`, oldest first, up to `limit`, each with its own cursor.
    fn pull(&mut self, cursor: u64, limit: usize) -> Result<Vec<(u64, FieldChange)>, SyncError>;
}

/// An in-memory cloud with the same rules as the real one (`apps/site/supabase/0001_sync.sql`):
/// one row per field, kept if its clock is later, each write given the next cursor.
#[derive(Clone, Default)]
pub struct MemoryCloud {
    inner: Arc<Mutex<CloudRows>>,
}

#[derive(Default)]
struct CloudRows {
    rows: BTreeMap<(String, String, String), (u64, FieldChange)>,
    seq: u64,
    offline: bool,
}

impl MemoryCloud {
    /// While true, every request fails as `Offline`.
    pub fn set_offline(&self, offline: bool) {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).offline = offline;
    }
}

impl Transport for MemoryCloud {
    fn push(&mut self, changes: &[FieldChange]) -> Result<(), SyncError> {
        let mut c = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if c.offline {
            return Err(SyncError::Offline);
        }
        for ch in changes {
            let key = (ch.kind.clone(), ch.id.clone(), ch.field.clone());
            if c.rows.get(&key).is_none_or(|(_, have)| have.hlc < ch.hlc) {
                c.seq += 1;
                let seq = c.seq;
                c.rows.insert(key, (seq, ch.clone()));
            }
        }
        Ok(())
    }

    fn pull(&mut self, cursor: u64, limit: usize) -> Result<Vec<(u64, FieldChange)>, SyncError> {
        let c = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if c.offline {
            return Err(SyncError::Offline);
        }
        let mut out: Vec<_> = c.rows.values().filter(|(seq, _)| *seq > cursor).cloned().collect();
        out.sort_by_key(|(seq, _)| *seq);
        out.truncate(limit);
        Ok(out)
    }
}

/// What `sync.status` answers (the TypeScript `SyncStatus`, fixtures/sync-status.json).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// "ok", "offline", "paused_for_service", "no_plus" or "not_set_up".
    pub state: &'static str,
    /// Documents with changes here the cloud doesn't have yet.
    pub pending: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<u64>,
    pub changed_elsewhere: Vec<Notice>,
}

/// A document changed here and elsewhere before it synced.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_id: Option<String>,
}

/// Why sync waits now, if it does: without Plus it is off, unlinked it can't, and during a
/// service it waits until it's over.
pub fn paused(plus: bool, set_up: bool, in_service: bool) -> Option<&'static str> {
    if !plus {
        Some("no_plus")
    } else if !set_up {
        Some("not_set_up")
    } else if in_service {
        Some("paused_for_service")
    } else {
        None
    }
}

// ---- The sync tables, in library.sqlite3 ----

/// Makes the sync tables, and notes every document saved before them, so the cloud gets it.
pub(crate) fn open(db: &mut Connection, now: u64) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_fields (
            kind TEXT NOT NULL,
            id TEXT NOT NULL,
            field TEXT NOT NULL,
            hlc TEXT NOT NULL,
            -- 1: changed here and not sent yet. These rows are the outbox.
            dirty INTEGER NOT NULL,
            PRIMARY KEY (kind, id, field)
        );
        CREATE TABLE IF NOT EXISTS sync_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS sync_notices (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            at INTEGER NOT NULL,
            kind TEXT NOT NULL,
            id TEXT NOT NULL,
            title TEXT NOT NULL,
            copy_id TEXT
        );",
    )?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let unseen: Vec<(String, String)> = {
        let mut q = tx.prepare(
            "SELECT kind, id FROM docs d WHERE NOT EXISTS
               (SELECT 1 FROM sync_fields f WHERE f.kind = d.kind AND f.id = d.id)",
        )?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
    };
    for (kind, id) in unseen {
        if let Some((state, _)) = Library::read_state(&tx, &kind, &id)? {
            record_local(&tx, &kind, &id, None, &state, now)?;
        }
    }
    tx.commit()
}

fn meta(db: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    db.query_row("SELECT value FROM sync_meta WHERE key = ?1", params![key], |r| r.get(0)).optional()
}

fn set_meta(db: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO sync_meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
        params![key, value],
    )
    .map(|_| ())
}

/// Wall-clock ms and a counter: the time part of a hybrid logical clock.
fn parse_clock(hlc: &str) -> Option<(u64, u32)> {
    let mut parts = hlc.splitn(3, '-');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The computer that stamped a clock.
fn node_of(hlc: &str) -> Option<&str> {
    hlc.splitn(3, '-').nth(2)
}

/// The next clock for a change made here: later than every clock seen so far, local or remote.
fn stamp(db: &Connection, now: u64) -> rusqlite::Result<String> {
    let node = match meta(db, "node")? {
        Some(n) => n,
        None => {
            let n = crate::devices::random_id(6);
            set_meta(db, "node", &n)?;
            n
        }
    };
    let (ms, n) = meta(db, "clock")?.and_then(|c| parse_clock(&c)).unwrap_or((0, 0));
    let next = if now > ms { (now, 0) } else { (ms, n + 1) };
    set_meta(db, "clock", &format!("{}-{}", next.0, next.1))?;
    Ok(format!("{:013}-{:06}-{node}", next.0, next.1))
}

/// A clock from elsewhere: the next local one comes after it.
fn observe(db: &Connection, hlc: &str) -> rusqlite::Result<()> {
    let Some(seen) = parse_clock(hlc) else { return Ok(()) };
    let mine = meta(db, "clock")?.and_then(|c| parse_clock(&c)).unwrap_or((0, 0));
    if seen > mine {
        set_meta(db, "clock", &format!("{}-{}", seen.0, seen.1))?;
    }
    Ok(())
}

fn mark_dirty(db: &Connection, kind: &str, id: &str, field: &str, hlc: &str) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO sync_fields (kind, id, field, hlc, dirty) VALUES (?1, ?2, ?3, ?4, 1)
         ON CONFLICT (kind, id, field) DO UPDATE SET hlc = ?4, dirty = 1",
        params![kind, id, field, hlc],
    )
    .map(|_| ())
}

/// Notes a change made on this computer (inside the save's own transaction): the fields that
/// differ from `old` get a new clock and go in the outbox. A delete sends only `deleted`; a
/// new document (or one made again) sends every field.
pub(crate) fn record_local(
    db: &Connection,
    kind: &str,
    id: &str,
    old: Option<&DocState>,
    new: &DocState,
    now: u64,
) -> rusqlite::Result<()> {
    let changed: Vec<String> = if new.deleted {
        vec!["deleted".into()]
    } else {
        let before = old.filter(|o| !o.deleted).map(DocState::fields).unwrap_or_default();
        let after = new.fields();
        let mut names: Vec<&String> = before.keys().chain(after.keys()).collect();
        names.sort();
        names.dedup();
        names.into_iter().filter(|f| before.get(*f) != after.get(*f)).cloned().collect()
    };
    if changed.is_empty() {
        return Ok(());
    }
    let hlc = stamp(db, now)?;
    for field in changed {
        mark_dirty(db, kind, id, &field, &hlc)?;
    }
    Ok(())
}

impl Library {
    /// Documents with changes the cloud doesn't have yet.
    pub fn sync_pending(&self) -> Result<u64, crate::library::Error> {
        Ok(self.db().query_row(
            "SELECT COUNT(*) FROM (SELECT DISTINCT kind, id FROM sync_fields WHERE dirty = 1)",
            [],
            |r| r.get(0),
        )?)
    }

    /// `sync.status`, with the sync thread's latest `state`.
    pub fn sync_status(&self, state: &'static str) -> Result<SyncStatus, crate::library::Error> {
        let db = self.db();
        let last_synced_at = meta(db, "last_synced")?.and_then(|t| t.parse().ok());
        let mut q =
            db.prepare_cached("SELECT kind, id, title, at, copy_id FROM sync_notices ORDER BY seq DESC LIMIT ?1")?;
        let changed_elsewhere = q
            .query_map(params![MAX_NOTICES], |r| {
                Ok(Notice { kind: r.get(0)?, id: r.get(1)?, title: r.get(2)?, at: r.get(3)?, copy_id: r.get(4)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(SyncStatus { state, pending: self.sync_pending()?, last_synced_at, changed_elsewhere })
    }
}

/// The outbox: every dirty field with its value now.
fn outbox(db: &Connection) -> rusqlite::Result<Vec<FieldChange>> {
    let rows: Vec<(String, String, String, String)> = {
        let mut q =
            db.prepare("SELECT kind, id, field, hlc FROM sync_fields WHERE dirty = 1 ORDER BY kind, id, field")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?
    };
    let mut out = Vec::new();
    let mut doc: Option<(String, String, Option<DocState>)> = None;
    for (kind, id, field, hlc) in rows {
        if doc.as_ref().is_none_or(|(k, i, _)| *k != kind || *i != id) {
            let state = Library::read_state(db, &kind, &id)?.map(|(s, _)| s);
            doc = Some((kind.clone(), id.clone(), state));
        }
        if let Some((_, _, Some(state))) = &doc {
            out.push(FieldChange { value: state.get(&field), kind, id, field, hlc });
        }
    }
    Ok(out)
}

/// Sent: out of the outbox, unless it changed again since.
fn mark_sent(db: &mut Connection, sent: &[FieldChange]) -> rusqlite::Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for c in sent {
        tx.execute(
            "UPDATE sync_fields SET dirty = 0 WHERE kind = ?1 AND id = ?2 AND field = ?3 AND hlc = ?4",
            params![c.kind, c.id, c.field, c.hlc],
        )?;
    }
    tx.commit()
}

/// An id for the other version of a song: the song's own, and a few random characters.
fn copy_id(id: &str) -> String {
    let base: String = id.chars().take(40).collect();
    format!("{base}-elsewhere-{}", crate::devices::random_id(3))
}

fn copy_title(title: &str) -> String {
    let suffix = " (changed elsewhere)";
    let keep = MAX_TITLE.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", title.chars().take(keep).collect::<String>())
}

/// Merges one batch from the cloud, in one transaction with its cursor. Returns the documents
/// that changed here (and whether each is now deleted), for `doc.changed`.
fn merge(db: &mut Connection, batch: &[(u64, FieldChange)], now: u64) -> rusqlite::Result<Vec<(DocInfo, bool)>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let me = meta(&tx, "node")?;
    let mut docs: Vec<((String, String), Vec<&FieldChange>)> = Vec::new();
    for (_, c) in batch {
        observe(&tx, &c.hlc)?;
        let key = (c.kind.clone(), c.id.clone());
        match docs.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(c),
            None => docs.push((key, vec![c])),
        }
    }
    let mut changed = Vec::new();
    for ((kind, id), changes) in docs {
        let saved = Library::read_state(&tx, &kind, &id)?.map(|(s, _)| s);
        let mut state = saved.clone().unwrap_or(DocState { title: id.clone(), body: Map::new(), deleted: true });
        // The other copy as it stands: this one with every field the cloud sent.
        let mut theirs = state.clone();
        let (mut both, mut keep_both, mut won) = (false, false, Vec::new());
        for c in changes {
            let local: Option<(String, bool)> = tx
                .query_row(
                    "SELECT hlc, dirty FROM sync_fields WHERE kind = ?1 AND id = ?2 AND field = ?3",
                    params![kind, id, c.field],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            // Our own change coming back, perhaps older than an edit made here since.
            if me.is_some() && node_of(&c.hlc) == me.as_deref() {
                continue;
            }
            if local.as_ref().is_some_and(|(hlc, _)| *hlc == c.hlc) {
                continue;
            }
            theirs.set(&c.field, &c.value);
            let edited_here = local.as_ref().is_some_and(|(_, dirty)| *dirty) && state.get(&c.field) != c.value;
            both |= edited_here;
            if edited_here && kind == "song" && LYRIC_FIELDS.contains(&c.field.as_str()) {
                keep_both = true;
                continue; // our words stay here; theirs go in the copy
            }
            if local.as_ref().is_none_or(|(hlc, _)| c.hlc > *hlc) {
                state.set(&c.field, &c.value);
                won.push(c);
            }
        }
        let mut copy = None;
        if keep_both {
            let new_id = copy_id(&id);
            let other = DocState { title: copy_title(&theirs.title), deleted: false, ..theirs };
            changed.push((Library::write_state(&tx, &kind, &new_id, &other, now)?, false));
            record_local(&tx, &kind, &new_id, None, &other, now)?;
            // This computer's words win in the original, even over a later edit elsewhere.
            let hlc = stamp(&tx, now)?;
            for field in LYRIC_FIELDS {
                mark_dirty(&tx, &kind, &id, field, &hlc)?;
            }
            copy = Some(new_id);
        }
        if both {
            tx.execute(
                "INSERT INTO sync_notices (at, kind, id, title, copy_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now, kind, id, state.title, copy],
            )?;
            tx.execute(
                "DELETE FROM sync_notices WHERE seq NOT IN (SELECT seq FROM sync_notices ORDER BY seq DESC LIMIT ?1)",
                params![MAX_NOTICES],
            )?;
        }
        if won.is_empty() || (saved.is_none() && state.deleted) {
            continue;
        }
        for c in &won {
            tx.execute(
                "INSERT INTO sync_fields (kind, id, field, hlc, dirty) VALUES (?1, ?2, ?3, ?4, 0)
                 ON CONFLICT (kind, id, field) DO UPDATE SET hlc = ?4, dirty = 0",
                params![kind, id, c.field, c.hlc],
            )?;
        }
        changed.push((Library::write_state(&tx, &kind, &id, &state, now)?, state.deleted));
    }
    if let Some((seq, _)) = batch.last() {
        set_meta(&tx, "cursor", &seq.to_string())?;
    }
    tx.commit()?;
    Ok(changed)
}

/// What a round did.
#[derive(Debug, Default)]
pub struct Round {
    /// Documents changed here by the cloud's changes, and whether each is now deleted.
    pub changed: Vec<(DocInfo, bool)>,
    /// Fields sent.
    pub pushed: usize,
    /// False if it stopped before pushing because `go_on` said so (a service started).
    pub finished: bool,
}

/// Syncs one library with one cloud.
pub struct Syncer {
    lib: Library,
    transport: Box<dyn Transport>,
}

impl Syncer {
    pub fn new(lib: Library, transport: Box<dyn Transport>) -> Self {
        Syncer { lib, transport }
    }

    pub fn library(&mut self) -> &mut Library {
        &mut self.lib
    }

    /// One round: pull and merge everything new, then (if `go_on` still says so) push the outbox.
    pub fn round(&mut self, now: u64, go_on: &dyn Fn() -> bool) -> Result<Round, SyncError> {
        let mut round = Round::default();
        loop {
            let cursor = meta(self.lib.conn(), "cursor")?.and_then(|c| c.parse().ok()).unwrap_or(0);
            let batch = self.transport.pull(cursor, BATCH)?;
            if batch.is_empty() {
                break;
            }
            round.changed.extend(merge(self.lib.conn(), &batch, now)?);
            if batch.len() < BATCH || !go_on() {
                break;
            }
        }
        if !go_on() {
            return Ok(round);
        }
        let out = outbox(self.lib.conn())?;
        for chunk in out.chunks(BATCH) {
            self.transport.push(chunk)?;
            mark_sent(self.lib.conn(), chunk)?;
            round.pushed += chunk.len();
        }
        set_meta(self.lib.conn(), "last_synced", &now.to_string())?;
        round.finished = true;
        Ok(round)
    }
}

/// Runs sync on its own thread for the engine's whole life. `gate` says (Plus active, service
/// on), taking the engine's lock only for that; `report` hands over the state and the documents
/// a round changed, again under the lock only briefly. No network call is made under either.
pub fn spawn(
    data_dir: std::path::PathBuf,
    transport: Option<Box<dyn Transport>>,
    gate: impl Fn() -> (bool, bool) + Send + 'static,
    report: impl Fn(&'static str, Vec<(DocInfo, bool)>) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let set_up = transport.is_some();
        let mut transport = transport;
        let mut syncer: Option<Syncer> = None;
        let mut retry = FIRST_RETRY;
        loop {
            let (plus, service) = gate();
            if let Some(why) = paused(plus, set_up, service) {
                report(why, Vec::new());
                std::thread::sleep(IDLE);
                continue;
            }
            if syncer.is_none() {
                match Library::open(&data_dir) {
                    Ok(lib) => syncer = transport.take().map(|t| Syncer::new(lib, t)),
                    Err(e) => crate::log("sync", format!("the library can't be opened for sync: {e:?}")),
                }
            }
            let Some(s) = syncer.as_mut() else {
                std::thread::sleep(MAX_RETRY);
                continue;
            };
            let go_on = || {
                let (plus, service) = gate();
                plus && !service
            };
            let wait = match s.round(crate::now_ms(), &go_on) {
                Ok(round) => {
                    retry = FIRST_RETRY;
                    report("ok", round.changed);
                    EVERY
                }
                Err(e) => {
                    if let SyncError::Cloud(why) | SyncError::Local(why) = &e {
                        crate::log("sync", format!("sync stopped, trying again later: {why}"));
                    }
                    report("offline", Vec::new());
                    let wait = retry;
                    retry = (retry * 2).min(MAX_RETRY);
                    wait
                }
            };
            std::thread::sleep(wait);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("jivvy-sync-{name}-{}-{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn body(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn two_copies_edited_offline_converge_per_field_and_a_song_keeps_both_versions() {
        let cloud = MemoryCloud::default();
        let (da, db) = (dir("a"), dir("b"));
        let mut a = Syncer::new(Library::open(&da).unwrap(), Box::new(cloud.clone()));
        let mut b = Syncer::new(Library::open(&db).unwrap(), Box::new(cloud.clone()));
        let go = || true;
        let words = |t: &str| json!([{ "id": "v1", "label": "Verse 1", "text": t }]);
        let song = json!({ "sections": words("Amazing grace"), "arrangement": ["v1"], "author": "John Newton" });
        a.library().put("song", "grace", "Amazing Grace", &body(song), Some(0), 1_000).unwrap();
        a.library().put("plan", "sun", "Sunday", &body(json!({ "items": [], "notes": "" })), Some(0), 1_000).unwrap();
        assert_eq!(a.library().sync_pending().unwrap(), 2);
        assert_eq!(a.round(1_100, &go).unwrap().pushed, 9);
        assert_eq!(a.library().sync_pending().unwrap(), 0);
        b.round(1_200, &go).unwrap();
        assert_eq!(b.library().get("song", "grace").unwrap().unwrap().info.title, "Amazing Grace");

        // Offline, both edit: the plan's different fields, and the song's words in both places.
        cloud.set_offline(true);
        assert_eq!(a.round(2_000, &go).unwrap_err(), SyncError::Offline);
        let plan_a = body(json!({ "items": ["welcome"], "notes": "" }));
        let plan_b = body(json!({ "items": [], "notes": "Bring the hymnals" }));
        a.library().put("plan", "sun", "Sunday", &plan_a, Some(1), 2_000).unwrap();
        b.library().put("plan", "sun", "Sunday 9:30", &plan_b, Some(1), 2_100).unwrap();
        let mut song_a = a.library().get("song", "grace").unwrap().unwrap().body;
        song_a.insert("sections".into(), words("Amazing grace, how sweet"));
        a.library().put("song", "grace", "Amazing Grace", &song_a, Some(1), 2_200).unwrap();
        let mut song_b = b.library().get("song", "grace").unwrap().unwrap().body;
        song_b.insert("sections".into(), words("Amazing grace! How sweet the sound"));
        song_b.insert("copyright".into(), json!("Public domain"));
        b.library().put("song", "grace", "Amazing Grace", &song_b, Some(1), 2_300).unwrap();

        // Back online: A, then B (which sees A's edits), then A again.
        cloud.set_offline(false);
        a.round(3_000, &go).unwrap();
        b.round(3_100, &go).unwrap();
        a.round(3_200, &go).unwrap();
        b.round(3_300, &go).unwrap();

        for s in [&mut a, &mut b] {
            let lib = s.library();
            // Each field kept its own edit.
            let plan = lib.get("plan", "sun").unwrap().unwrap();
            assert_eq!(plan.info.title, "Sunday 9:30");
            assert_eq!(plan.body, body(json!({ "items": ["welcome"], "notes": "Bring the hymnals" })));
            // B saw both changes to the words: it kept its own and saved A's as a copy.
            let grace = lib.get("song", "grace").unwrap().unwrap().body;
            assert_eq!(grace["sections"], words("Amazing grace! How sweet the sound"));
            assert_eq!(
                (grace["author"].clone(), grace["copyright"].clone()),
                (json!("John Newton"), json!("Public domain"))
            );
            let songs = lib.list("song").unwrap();
            assert_eq!(songs.len(), 2, "one copy, made once");
            let copy = songs.iter().find(|d| d.id != "grace").expect("both versions kept");
            assert_eq!(copy.title, "Amazing Grace (changed elsewhere)");
            assert_eq!(lib.get("song", &copy.id).unwrap().unwrap().body["sections"], words("Amazing grace, how sweet"));
            assert_eq!(lib.sync_pending().unwrap(), 0);
        }
        let status = b.library().sync_status("ok").unwrap();
        assert_eq!(status.last_synced_at, Some(3_300));
        let notice = status.changed_elsewhere.iter().find(|n| n.id == "grace").expect("a changed-elsewhere notice");
        assert!(notice.copy_id.is_some());
        // The plan's edits were to different fields: nothing to tell.
        assert!(!status.changed_elsewhere.iter().any(|n| n.id == "sun"));
        drop((a, b));
        let _ = (std::fs::remove_dir_all(&da), std::fs::remove_dir_all(&db));
    }

    #[test]
    fn the_status_matches_the_shared_fixture() {
        const FIXTURE: &str = include_str!("../../../packages/protocol/fixtures/sync-status.json");
        let f: Value = serde_json::from_str(FIXTURE).unwrap();
        let status = SyncStatus {
            state: "offline",
            pending: 2,
            last_synced_at: Some(1_791_646_212_000),
            changed_elsewhere: vec![
                Notice {
                    kind: "song".into(),
                    id: "grace".into(),
                    title: "Amazing Grace".into(),
                    at: 1_791_646_200_000,
                    copy_id: Some("grace-elsewhere-AbCd".into()),
                },
                Notice {
                    kind: "plan".into(),
                    id: "sunday".into(),
                    title: "Sunday 9:30".into(),
                    at: 1_791_646_100_000,
                    copy_id: None,
                },
            ],
        };
        assert_eq!(serde_json::to_value(&status).unwrap(), f["ack"]["sync"]);
        assert_eq!(
            [
                paused(false, true, true),
                paused(true, false, false),
                paused(true, true, true),
                paused(true, true, false)
            ],
            [Some("no_plus"), Some("not_set_up"), Some("paused_for_service"), None]
        );
    }
}
