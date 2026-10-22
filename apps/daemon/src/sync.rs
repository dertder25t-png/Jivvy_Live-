//! Sync between the church computer's library and the church's cloud copy (Plus only).
//!
//! The library on this computer stays the master copy: a service never waits on the cloud, and
//! a church without Plus (or offline) keeps everything here. Sync runs on its own thread with
//! its own connection to `library.sqlite3`, never holds the engine's lock across a network
//! call, keeps its transactions short (one document at a time), and waits while a service is on.
//!
//! How it works:
//! - Every local save or delete notes which fields changed (`title`, `deleted`, and each
//!   top-level `body.<key>`), each stamped with a hybrid logical clock (`ms-counter-node`,
//!   fixed width, so it sorts bytewise as text) and marked dirty. The dirty fields are the
//!   outbox. Every edit also stamps `deleted = false`, so an edit made after a delete elsewhere
//!   brings the document back, and a delete made after the edit removes it.
//! - Each field also remembers its `base`: the clock this computer last knew the cloud had.
//!   A push says which base it built on, and the cloud takes it only if that is still what it
//!   has (compare and set). Otherwise it hands back its newer copy, which goes through the same
//!   merge as a pull, so no two edits ever pass each other unseen.
//! - Merge, per field: the later clock wins. A field changed here and elsewhere before syncing
//!   is a "changed elsewhere" notice. When a song's words (sections, arrangement) changed in
//!   both places, this computer keeps its own words and saves the other version as a new song,
//!   "<title> (changed elsewhere)", whose id comes from the song's id and the other version's
//!   clock (so it is only ever made once).
//! - A top-level body value of `null` counts as removed; a document made again sends `null` for
//!   every field it had before, so no copy brings an old field back.
//!
//! The cloud is behind `Transport`: `sync_cloud::SupabaseTransport` is the real one, and
//! `MemoryCloud` an in-memory stand-in with the same rules as `apps/site/supabase/0001_sync.sql`.
//! Nothing here logs a token.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// The clock's counter has six digits; past this the clock moves on a millisecond instead.
const MAX_COUNTER: u32 = 999_999;
/// A clock from elsewhere further ahead of this computer's than this is ignored (a computer
/// whose date is wrong mustn't win every edit for years).
const MAX_AHEAD_MS: u64 = 24 * 60 * 60 * 1000;
/// How often a round runs while all is well, and the longest wait after failures.
const EVERY: Duration = Duration::from_secs(15);
const IDLE: Duration = Duration::from_secs(10);
const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(300);

static WARNED_AHEAD: AtomicBool = AtomicBool::new(false);

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
    /// The hybrid logical clock it was stamped with; later sorts higher, bytewise.
    pub hlc: String,
    /// On a push: the clock this computer last knew the cloud had for the field (none: it
    /// knew of none). The cloud takes the change only if that is still what it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
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
    /// Saves these. A change whose `base` isn't the cloud's clock for that field any more is
    /// refused, and the cloud's own copy of that field is returned instead.
    fn push(&mut self, changes: &[FieldChange]) -> Result<Vec<FieldChange>, SyncError>;
    /// Fields saved after `cursor`, oldest first, up to `limit`, each with its own cursor.
    fn pull(&mut self, cursor: u64, limit: usize) -> Result<Vec<(u64, FieldChange)>, SyncError>;
}

/// An in-memory cloud with the same rules as the real one (`apps/site/supabase/0001_sync.sql`):
/// one row per field, changed only by a later clock built on the clock it has, each write given
/// the next cursor.
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
    /// Tests: a row as a computer with a wrong date could have left it (the real cloud refuses
    /// clocks more than a day ahead of its own, but a copy from elsewhere may still hold one).
    #[cfg(test)]
    fn plant(&self, change: FieldChange) {
        let mut c = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        c.seq += 1;
        let seq = c.seq;
        c.rows.insert((change.kind.clone(), change.id.clone(), change.field.clone()), (seq, change));
    }

    /// While true, every request fails as `Offline`.
    pub fn set_offline(&self, offline: bool) {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).offline = offline;
    }
}

impl Transport for MemoryCloud {
    fn push(&mut self, changes: &[FieldChange]) -> Result<Vec<FieldChange>, SyncError> {
        let mut c = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if c.offline {
            return Err(SyncError::Offline);
        }
        let mut refused = Vec::new();
        for ch in changes {
            let key = (ch.kind.clone(), ch.id.clone(), ch.field.clone());
            match c.rows.get(&key) {
                Some((_, have)) if ch.base.as_deref() != Some(have.hlc.as_str()) || ch.hlc <= have.hlc => {
                    refused.push(have.clone());
                }
                _ => {
                    c.seq += 1;
                    let seq = c.seq;
                    c.rows.insert(key, (seq, FieldChange { base: None, ..ch.clone() }));
                }
            }
        }
        Ok(refused)
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
    /// Why sync can't run on this computer, in plain words; none when nothing is wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
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
    /// Deleted in one place and edited in the other: the later edit brought it back.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
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

fn add_column(db: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<()> {
    let has: bool = db.query_row(
        &format!("SELECT COUNT(*) > 0 FROM pragma_table_info('{table}') WHERE name = ?1"),
        params![column],
        |r| r.get(0),
    )?;
    if !has {
        db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// Makes the sync tables, notes every document saved before them so the cloud gets it, and
/// gives this library a new node id if it now lives somewhere else (`home`: a copied or
/// restored data folder must never stamp clocks as the computer it came from).
pub(crate) fn open(db: &mut Connection, now: u64, home: &str) -> rusqlite::Result<()> {
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
    // The clock the cloud had when this computer last heard (none: never).
    add_column(db, "sync_fields", "base", "TEXT")?;
    add_column(db, "sync_notices", "restored", "INTEGER NOT NULL DEFAULT 0")?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if meta(&tx, "home")?.as_deref() != Some(home) {
        if meta(&tx, "node")?.is_some() {
            crate::log("sync", "the library was moved or restored; it syncs under a new id from now on");
        }
        set_meta(&tx, "node", &crate::devices::random_id(6))?;
        set_meta(&tx, "home", home)?;
    }
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

/// Where this library lives: the computer's name and the data folder.
pub(crate) fn home(data_dir: &std::path::Path) -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    format!("{host}|{}", dir.display())
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

/// Wall-clock ms and counter of a well-formed clock: 13 digits, 6 digits, a node id of 1-16
/// characters (A-Z a-z 0-9 _ -). Anything else is refused.
fn parse_hlc(hlc: &str) -> Option<(u64, u32)> {
    let (ms, rest) = hlc.split_at_checked(13)?;
    let (counter, node) = rest.strip_prefix('-')?.split_at_checked(6)?;
    let node = node.strip_prefix('-')?;
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let node_ok =
        (1..=16).contains(&node.len()) && node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    (digits(ms) && digits(counter) && node_ok).then(|| Some((ms.parse().ok()?, counter.parse().ok()?)))?
}

fn last_clock(db: &Connection) -> rusqlite::Result<(u64, u32)> {
    let saved = meta(db, "clock")?;
    let parsed = saved.as_deref().and_then(|c| {
        let (ms, n) = c.split_once('-')?;
        Some((ms.parse().ok()?, n.parse().ok()?))
    });
    Ok(parsed.unwrap_or((0, 0)))
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
    let (ms, n) = last_clock(db)?;
    let next = if now > ms {
        (now, 0)
    } else if n >= MAX_COUNTER {
        (ms + 1, 0)
    } else {
        (ms, n + 1)
    };
    set_meta(db, "clock", &format!("{}-{}", next.0, next.1))?;
    Ok(format!("{:013}-{:06}-{node}", next.0, next.1))
}

/// A clock from elsewhere more than a day ahead of ours. It still merges by plain last-writer-
/// wins, but this computer's clock never jumps to it (logged once).
fn far_ahead(hlc: &str, now: u64) -> bool {
    let ahead = parse_hlc(hlc).is_some_and(|(ms, _)| ms > now.saturating_add(MAX_AHEAD_MS));
    if ahead && !WARNED_AHEAD.swap(true, Ordering::Relaxed) {
        crate::log(
            "sync",
            "a change is stamped more than a day ahead (another computer's date is wrong); not following its clock",
        );
    }
    ahead
}

/// The clock just after `hlc`, stamped by `node`: for a field whose clock is ahead of ours.
fn just_after(hlc: &str, node: &str) -> Option<String> {
    let (ms, n) = parse_hlc(hlc)?;
    let (ms, n) = if n >= MAX_COUNTER { (ms + 1, 0) } else { (ms, n + 1) };
    Some(format!("{ms:013}-{n:06}-{node}"))
}

/// A clock from elsewhere: the next local one comes after it.
fn observe(db: &Connection, hlc: &str) -> rusqlite::Result<()> {
    let Some(seen) = parse_hlc(hlc) else { return Ok(()) };
    if seen > last_clock(db)? {
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
/// differ from `old` get a new clock and go in the outbox. A delete sends only `deleted`; an
/// edit always sends `deleted = false` too. A document made again (or new here) also sends
/// `null` for every field it had before that it doesn't have now.
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
        let before = old.map(DocState::fields).unwrap_or_default();
        let after = new.fields();
        let mut names: Vec<String> = before.keys().chain(after.keys()).cloned().collect();
        if old.is_none_or(|o| o.deleted) {
            let mut q = db.prepare_cached("SELECT field FROM sync_fields WHERE kind = ?1 AND id = ?2")?;
            let known = q.query_map(params![kind, id], |r| r.get::<_, String>(0))?;
            names.extend(known.collect::<Result<Vec<_>, _>>()?);
        }
        names.sort();
        names.dedup();
        let remade = old.is_none_or(|o| o.deleted);
        names.retain(|f| f == "deleted" || before.get(f) != after.get(f) || (remade && !after.contains_key(f)));
        names
    };
    let hlc = stamp(db, now)?;
    let node = hlc.splitn(3, '-').nth(2).unwrap_or_default().to_string();
    for field in changed {
        // A field whose clock is ahead of ours (stamped far ahead elsewhere) gets the clock
        // just after it, so a later edit here still wins without moving our whole clock.
        let have: Option<String> = db
            .query_row(
                "SELECT hlc FROM sync_fields WHERE kind = ?1 AND id = ?2 AND field = ?3",
                params![kind, id, field],
                |r| r.get(0),
            )
            .optional()?;
        let mine = have.filter(|h| *h >= hlc).and_then(|h| just_after(&h, &node)).unwrap_or_else(|| hlc.clone());
        mark_dirty(db, kind, id, &field, &mine)?;
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
        let mut q = db.prepare_cached(
            "SELECT kind, id, title, at, copy_id, restored FROM sync_notices ORDER BY seq DESC LIMIT ?1",
        )?;
        let changed_elsewhere = q
            .query_map(params![MAX_NOTICES], |r| {
                Ok(Notice {
                    kind: r.get(0)?,
                    id: r.get(1)?,
                    title: r.get(2)?,
                    at: r.get(3)?,
                    copy_id: r.get(4)?,
                    restored: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(SyncStatus { state, pending: self.sync_pending()?, last_synced_at, changed_elsewhere, problem: None })
    }
}

/// The outbox: every dirty field with its value now and the base it was built on.
fn outbox(db: &Connection) -> rusqlite::Result<Vec<FieldChange>> {
    let rows: Vec<(String, String, String, String, Option<String>)> = {
        let mut q =
            db.prepare("SELECT kind, id, field, hlc, base FROM sync_fields WHERE dirty = 1 ORDER BY kind, id, field")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?
    };
    let mut out = Vec::new();
    let mut doc: Option<(String, String, Option<DocState>)> = None;
    for (kind, id, field, hlc, base) in rows {
        if doc.as_ref().is_none_or(|(k, i, _)| *k != kind || *i != id) {
            let state = Library::read_state(db, &kind, &id)?.map(|(s, _)| s);
            doc = Some((kind.clone(), id.clone(), state));
        }
        if let Some((_, _, Some(state))) = &doc {
            out.push(FieldChange { value: state.get(&field), kind, id, field, hlc, base });
        }
    }
    Ok(out)
}

/// Taken by the cloud: out of the outbox (unless it changed again since), and now the base.
fn mark_sent(db: &mut Connection, sent: &[&FieldChange]) -> rusqlite::Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for c in sent {
        tx.execute(
            "UPDATE sync_fields SET dirty = 0, base = ?4 WHERE kind = ?1 AND id = ?2 AND field = ?3 AND hlc = ?4",
            params![c.kind, c.id, c.field, c.hlc],
        )?;
        tx.execute(
            "UPDATE sync_fields SET base = ?4 WHERE kind = ?1 AND id = ?2 AND field = ?3 AND hlc <> ?4",
            params![c.kind, c.id, c.field, c.hlc],
        )?;
    }
    tx.commit()
}

/// FNV-1a, stable across builds.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// The other version's id: the song's own and that version's clock, so it is made only once.
fn copy_id(id: &str, their_hlc: &str) -> String {
    let base: String = id.chars().take(40).collect();
    format!("{base}-elsewhere-{:08x}", fnv(&format!("{id}|{their_hlc}")) as u32)
}

fn copy_title(title: &str) -> String {
    let suffix = " (changed elsewhere)";
    let keep = MAX_TITLE.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", title.chars().take(keep).collect::<String>())
}

fn set_clock(
    db: &Connection,
    kind: &str,
    id: &str,
    field: &str,
    hlc: &str,
    base: &str,
    dirty: bool,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO sync_fields (kind, id, field, hlc, dirty, base) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (kind, id, field) DO UPDATE SET hlc = ?4, dirty = ?5, base = ?6",
        params![kind, id, field, hlc, dirty, base],
    )
    .map(|_| ())
}

/// Merges the cloud's fields for one document, in a transaction of its own. Returns what
/// changed here (and whether each is now deleted), for `doc.changed`. Every field is applied,
/// even for a document this computer doesn't have yet (it waits as a hidden marker until its
/// `deleted = false` arrives), so a document split across batches is never lost.
fn merge_doc(
    db: &mut Connection,
    kind: &str,
    id: &str,
    changes: &[&FieldChange],
    now: u64,
) -> rusqlite::Result<Vec<(DocInfo, bool)>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let saved = Library::read_state(&tx, kind, id)?.map(|(s, _)| s);
    let visible_before = saved.as_ref().is_some_and(|s| !s.deleted);
    let mut state = saved.clone().unwrap_or(DocState { title: id.to_string(), body: Map::new(), deleted: true });
    // The other copy as it stands: this one with every field the cloud sent.
    let mut theirs = state.clone();
    let (mut both, mut delete_fight, mut wrote) = (false, false, false);
    let mut lyric_hlc: Option<String> = None;
    for c in changes {
        if parse_hlc(&c.hlc).is_none() {
            continue;
        }
        let local: Option<(String, bool, Option<String>)> = tx
            .query_row(
                "SELECT hlc, dirty, base FROM sync_fields WHERE kind = ?1 AND id = ?2 AND field = ?3",
                params![kind, id, c.field],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        // Already known: our own change coming back, or the version we built on.
        if local.as_ref().is_some_and(|(hlc, _, base)| *hlc == c.hlc || base.as_deref() == Some(c.hlc.as_str())) {
            continue;
        }
        if !far_ahead(&c.hlc, now) {
            observe(&tx, &c.hlc)?;
        }
        theirs.set(&c.field, &c.value);
        let dirty = local.as_ref().is_some_and(|(_, d, _)| *d);
        let edited_here = dirty && state.get(&c.field) != c.value;
        both |= edited_here;
        delete_fight |= edited_here && c.field == "deleted";
        if edited_here && kind == "song" && LYRIC_FIELDS.contains(&c.field.as_str()) {
            // Our words stay here (re-stamped below); theirs go in the copy.
            lyric_hlc = lyric_hlc.max(Some(c.hlc.clone()));
            let mine = local.as_ref().map_or(String::new(), |(h, _, _)| h.clone());
            set_clock(&tx, kind, id, &c.field, &mine, &c.hlc, true)?;
            continue;
        }
        if local.as_ref().is_none_or(|(hlc, _, _)| c.hlc > *hlc) {
            state.set(&c.field, &c.value);
            set_clock(&tx, kind, id, &c.field, &c.hlc, &c.hlc, false)?;
            wrote = true;
        } else if let Some((mine, dirty, _)) = &local {
            // Ours is later: it stays (and goes out, built on theirs).
            set_clock(&tx, kind, id, &c.field, mine, &c.hlc, *dirty)?;
        }
    }
    let mut changed = Vec::new();
    let mut copy = None;
    if let Some(their_hlc) = lyric_hlc {
        let new_id = copy_id(id, &their_hlc);
        let other = DocState { title: copy_title(&theirs.title), deleted: false, ..theirs };
        let old = Library::read_state(&tx, kind, &new_id)?.map(|(s, _)| s);
        changed.push((Library::write_state(&tx, kind, &new_id, &other, now)?, false));
        record_local(&tx, kind, &new_id, old.as_ref(), &other, now)?;
        // This computer's words win in the original, even over a later edit elsewhere.
        let hlc = stamp(&tx, now)?;
        for field in LYRIC_FIELDS {
            tx.execute(
                "UPDATE sync_fields SET hlc = ?4, dirty = 1 WHERE kind = ?1 AND id = ?2 AND field = ?3",
                params![kind, id, field, hlc],
            )?;
        }
        copy = Some(new_id);
    }
    if both {
        let restored = delete_fight && !state.deleted;
        tx.execute(
            "INSERT INTO sync_notices (at, kind, id, title, copy_id, restored) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now, kind, id, state.title, copy, restored],
        )?;
        tx.execute(
            "DELETE FROM sync_notices WHERE seq NOT IN (SELECT seq FROM sync_notices ORDER BY seq DESC LIMIT ?1)",
            params![MAX_NOTICES],
        )?;
    }
    if wrote {
        let info = Library::write_state(&tx, kind, id, &state, now)?;
        if visible_before || !state.deleted {
            changed.push((info, state.deleted));
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Merges changes from the cloud one document at a time.
fn merge(db: &mut Connection, changes: &[&FieldChange], now: u64) -> rusqlite::Result<Vec<(DocInfo, bool)>> {
    let mut docs: Vec<((&str, &str), Vec<&FieldChange>)> = Vec::new();
    for c in changes {
        let key = (c.kind.as_str(), c.id.as_str());
        match docs.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(c),
            None => docs.push((key, vec![c])),
        }
    }
    let mut changed = Vec::new();
    for ((kind, id), list) in docs {
        changed.extend(merge_doc(db, kind, id, &list, now)?);
    }
    Ok(changed)
}

/// What a round did.
#[derive(Debug, Default)]
pub struct Round {
    /// Documents changed here by the cloud's changes, and whether each is now deleted.
    pub changed: Vec<(DocInfo, bool)>,
    /// Fields the cloud took.
    pub pushed: usize,
    /// False if it stopped early because `go_on` said so (a service started).
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

    /// Pulls and merges everything new. The cursor moves only after a batch is all applied.
    pub fn pull(&mut self, now: u64, go_on: &dyn Fn() -> bool, round: &mut Round) -> Result<bool, SyncError> {
        loop {
            if !go_on() {
                return Ok(false);
            }
            let cursor = meta(self.lib.conn(), "cursor")?.and_then(|c| c.parse().ok()).unwrap_or(0);
            let batch = self.transport.pull(cursor, BATCH)?;
            let Some((last, _)) = batch.last() else { return Ok(true) };
            let last = *last;
            let changes: Vec<&FieldChange> = batch.iter().map(|(_, c)| c).collect();
            round.changed.extend(merge(self.lib.conn(), &changes, now)?);
            set_meta(self.lib.conn(), "cursor", &last.to_string())?;
            if batch.len() < BATCH {
                return Ok(true);
            }
        }
    }

    /// Pushes the outbox. What the cloud refuses (it had a newer edit) is merged like a pull,
    /// and what still wins here goes out again, built on the cloud's version.
    pub fn push(&mut self, now: u64, go_on: &dyn Fn() -> bool, round: &mut Round) -> Result<bool, SyncError> {
        for _ in 0..3 {
            let out = outbox(self.lib.conn())?;
            if out.is_empty() {
                return Ok(true);
            }
            let mut refused_any = false;
            for chunk in out.chunks(BATCH) {
                if !go_on() {
                    return Ok(false);
                }
                let refused = self.transport.push(chunk)?;
                let is_refused =
                    |c: &FieldChange| refused.iter().any(|r| (&r.kind, &r.id, &r.field) == (&c.kind, &c.id, &c.field));
                let taken: Vec<&FieldChange> = chunk.iter().filter(|c| !is_refused(c)).collect();
                mark_sent(self.lib.conn(), &taken)?;
                round.pushed += taken.len();
                refused_any |= !refused.is_empty();
                round.changed.extend(merge(self.lib.conn(), &refused.iter().collect::<Vec<_>>(), now)?);
            }
            if !refused_any {
                return Ok(true);
            }
        }
        // Still refused after three tries: the outbox stays, so the status keeps saying it's waiting.
        Ok(outbox(self.lib.conn())?.is_empty())
    }

    /// One round: pull and merge everything new, then push the outbox, checking `go_on` (no
    /// service started, Plus still on) between every request.
    pub fn round(&mut self, now: u64, go_on: &dyn Fn() -> bool) -> Result<Round, SyncError> {
        let mut round = Round::default();
        if self.pull(now, go_on, &mut round)? && self.push(now, go_on, &mut round)? {
            set_meta(self.lib.conn(), "last_synced", &now.to_string())?;
            round.finished = true;
        }
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
                    let state = if round.finished || go_on() { "ok" } else { "paused_for_service" };
                    report(state, round.changed);
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

    fn words(t: &str) -> Value {
        json!([{ "id": "v1", "label": "Verse 1", "text": t }])
    }

    #[test]
    fn two_copies_edited_offline_converge_per_field_and_a_song_keeps_both_versions() {
        let cloud = MemoryCloud::default();
        let (da, db) = (dir("a"), dir("b"));
        let mut a = Syncer::new(Library::open(&da).unwrap(), Box::new(cloud.clone()));
        let mut b = Syncer::new(Library::open(&db).unwrap(), Box::new(cloud.clone()));
        let go = || true;
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
        let edit = |s: &mut Syncer, text: &str, extra: Option<(&str, Value)>, now: u64| {
            let doc = s.library().get("song", "grace").unwrap().unwrap();
            let mut song = doc.body;
            song.insert("sections".into(), words(text));
            if let Some((k, v)) = extra {
                song.insert(k.into(), v);
            }
            s.library().put("song", "grace", "Amazing Grace", &song, Some(doc.info.rev), now).unwrap();
        };
        edit(&mut a, "Amazing grace, how sweet", None, 2_200);
        edit(&mut b, "Amazing grace! How sweet the sound", Some(("copyright", json!("Public domain"))), 2_300);

        // Back online: A, then B (which sees A's edits), then A again.
        cloud.set_offline(false);
        a.round(3_000, &go).unwrap();
        b.round(3_100, &go).unwrap();
        a.round(3_200, &go).unwrap();
        b.round(3_300, &go).unwrap();

        // Then again, but both pull before either pushes: the cloud refuses the second push,
        // whose computer then keeps the other words as a copy too.
        edit(&mut a, "A third verse from A", None, 4_000);
        edit(&mut b, "A third verse from B", None, 4_100);
        let mut r = Round::default();
        a.pull(4_200, &go, &mut r).unwrap();
        b.pull(4_200, &go, &mut r).unwrap();
        a.push(4_300, &go, &mut r).unwrap();
        b.push(4_300, &go, &mut r).unwrap();
        a.round(4_400, &go).unwrap();
        b.round(4_500, &go).unwrap();
        a.round(4_600, &go).unwrap();

        for s in [&mut a, &mut b] {
            let lib = s.library();
            // Each field kept its own edit.
            let plan = lib.get("plan", "sun").unwrap().unwrap();
            assert_eq!(plan.info.title, "Sunday 9:30");
            assert_eq!(plan.body, body(json!({ "items": ["welcome"], "notes": "Bring the hymnals" })));
            let grace = lib.get("song", "grace").unwrap().unwrap().body;
            assert_eq!(grace["sections"], words("A third verse from B"));
            assert_eq!(
                (grace["author"].clone(), grace["copyright"].clone()),
                (json!("John Newton"), json!("Public domain"))
            );
            // Every version of the words survives, each copy made once.
            let songs = lib.list("song").unwrap();
            assert_eq!(songs.len(), 3, "one copy per conflict, made once");
            let mut copies: Vec<Value> = songs
                .iter()
                .filter(|d| d.id != "grace")
                .map(|d| {
                    assert_eq!(d.title, "Amazing Grace (changed elsewhere)");
                    lib.get("song", &d.id).unwrap().unwrap().body["sections"].clone()
                })
                .collect();
            copies.sort_by_key(|v| v.to_string());
            assert_eq!(copies, [words("A third verse from A"), words("Amazing grace, how sweet")]);
            assert_eq!(lib.sync_pending().unwrap(), 0);
        }
        let status = b.library().sync_status("ok").unwrap();
        assert_eq!(status.last_synced_at, Some(4_500));
        let notice = status.changed_elsewhere.iter().find(|n| n.id == "grace").expect("a changed-elsewhere notice");
        assert!(notice.copy_id.is_some());
        // The plan's edits were to different fields: nothing to tell.
        assert!(!status.changed_elsewhere.iter().any(|n| n.id == "sun"));
        drop((a, b));
        let _ = (std::fs::remove_dir_all(&da), std::fs::remove_dir_all(&db));
    }

    #[test]
    fn a_later_edit_beats_an_earlier_delete_and_a_remade_document_drops_its_old_fields() {
        let cloud = MemoryCloud::default();
        let (da, db, dc) = (dir("del-a"), dir("del-b"), dir("del-c"));
        let mut a = Syncer::new(Library::open(&da).unwrap(), Box::new(cloud.clone()));
        let mut b = Syncer::new(Library::open(&db).unwrap(), Box::new(cloud.clone()));
        let go = || true;
        let plan = body(json!({ "items": [], "notes": "old note" }));
        a.library().put("plan", "sun", "Sunday", &plan, Some(0), 1_000).unwrap();
        a.round(1_100, &go).unwrap();
        b.round(1_200, &go).unwrap();
        // A deletes it, then B (not yet told) edits it later: the edit wins, with a notice.
        a.library().delete("plan", "sun", None, 2_000).unwrap();
        let edited = body(json!({ "items": ["welcome"], "notes": "old note" }));
        b.library().put("plan", "sun", "Sunday", &edited, None, 2_100).unwrap();
        a.round(3_000, &go).unwrap();
        b.round(3_100, &go).unwrap();
        a.round(3_200, &go).unwrap();
        for s in [&mut a, &mut b] {
            assert_eq!(s.library().get("plan", "sun").unwrap().unwrap().body, edited);
        }
        let status = b.library().sync_status("ok").unwrap();
        assert!(status.changed_elsewhere.iter().any(|n| n.id == "sun" && n.restored));

        // Deleted and made again without its notes: a computer new to it never sees the old note.
        a.library().delete("plan", "sun", None, 4_000).unwrap();
        a.library().put("plan", "sun", "Sunday", &body(json!({ "items": [] })), None, 4_100).unwrap();
        a.round(4_200, &go).unwrap();
        let mut c = Syncer::new(Library::open(&dc).unwrap(), Box::new(cloud.clone()));
        c.round(4_300, &go).unwrap();
        assert_eq!(c.library().get("plan", "sun").unwrap().unwrap().body, body(json!({ "items": [] })));

        // A far-future cloud field doesn't block later edits, and our clock stays our own.
        let far = format!("{:013}-000000-wrongdate", 9_000_000_000_000u64);
        cloud.plant(FieldChange {
            kind: "plan".into(),
            id: "sun".into(),
            field: "title".into(),
            value: json!("From the future"),
            hlc: far,
            base: None,
        });
        a.round(5_000, &go).unwrap();
        assert_eq!(a.library().get("plan", "sun").unwrap().unwrap().info.title, "From the future");
        a.library().put("plan", "sun", "Sunday again", &body(json!({ "items": [] })), None, 5_100).unwrap();
        let r = a.round(5_200, &go).unwrap();
        assert!(r.finished && a.library().sync_pending().unwrap() == 0, "the later edit went out");
        c.round(5_300, &go).unwrap();
        assert_eq!(c.library().get("plan", "sun").unwrap().unwrap().info.title, "Sunday again");
        let clock = meta(a.library().conn(), "clock").unwrap().unwrap();
        assert!(clock.starts_with("5100-"), "our clock didn't jump ahead: {clock}");
        drop((a, b, c));
        let _ = (std::fs::remove_dir_all(&da), std::fs::remove_dir_all(&db), std::fs::remove_dir_all(&dc));
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
                    restored: false,
                },
                Notice {
                    kind: "plan".into(),
                    id: "sunday".into(),
                    title: "Sunday 9:30".into(),
                    at: 1_791_646_100_000,
                    copy_id: None,
                    restored: true,
                },
            ],
            problem: None,
        };
        assert_eq!(serde_json::to_value(&status).unwrap(), f["ack"]["sync"]);
        let broken = SyncStatus {
            state: "not_set_up",
            pending: 0,
            last_synced_at: None,
            changed_elsewhere: vec![],
            problem: Some("sync device key can't be read".into()),
        };
        assert_eq!(serde_json::to_value(&broken).unwrap(), f["withProblem"]["sync"]);
        assert_eq!(
            [
                paused(false, true, true),
                paused(true, false, false),
                paused(true, true, true),
                paused(true, true, false)
            ],
            [Some("no_plus"), Some("not_set_up"), Some("paused_for_service"), None]
        );
        // Clocks are fixed width, so text order is time order; anything else is refused.
        assert!(parse_hlc("0000000002000-000001-AbC_d-9").is_some());
        assert!(parse_hlc("2000-1-AbC").is_none() && parse_hlc("0000000002000-000001-").is_none());
        assert!(far_ahead(&format!("{:013}-000000-x", 10 + MAX_AHEAD_MS + 1), 10));
        assert_eq!(just_after("0000000002000-999999-a", "b").as_deref(), Some("0000000002001-000000-b"));
    }
}
