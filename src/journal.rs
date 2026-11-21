//! Durable undo journal backed by SQLite.
//!
//! One row in `entries` represents one intercepted command. Zero or more rows
//! in `staged` describe the bytes rewind moved or copied aside for it. Undo
//! consumes staged rows one by one, which makes a partially completed undo
//! resumable: the next `rewind undo` only has the remaining items to fix.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::util::now_unix;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS entries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER,
    command     TEXT    NOT NULL,
    argv        TEXT    NOT NULL,
    cwd         TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    exit_code   INTEGER,
    undoable    INTEGER NOT NULL DEFAULT 0,
    meta        TEXT    NOT NULL DEFAULT '{}',
    purged_at   INTEGER
);

CREATE TABLE IF NOT EXISTS staged (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    origin   TEXT    NOT NULL,
    stored   TEXT    NOT NULL DEFAULT '',
    kind     TEXT    NOT NULL,
    size     INTEGER NOT NULL DEFAULT 0,
    meta     TEXT    NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS reservations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id    INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    pid         INTEGER NOT NULL,
    start_time  INTEGER NOT NULL DEFAULT 0,
    bytes       INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_staged_entry ON staged(entry_id);
CREATE INDEX IF NOT EXISTS idx_entries_status ON entries(status, id);
CREATE INDEX IF NOT EXISTS idx_reservations_entry ON reservations(entry_id);
PRAGMA user_version = 1;
"#;

/// Lifecycle of a journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    /// Process is still running (or was killed before finishing).
    Running,
    /// Command completed; staged data is available for undo.
    Done,
    /// Successfully reversed.
    Undone,
    /// Command failed and left nothing (or only partial) to undo.
    Failed,
    /// The process was killed before it could record a result.
    Interrupted,
    /// Staged data was purged by `rewind clean`; the entry is history only.
    Expired,
}

impl EntryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Undone => "undone",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "done" => Self::Done,
            "undone" => Self::Undone,
            "failed" => Self::Failed,
            "interrupted" => Self::Interrupted,
            "expired" => Self::Expired,
            _ => Self::Running,
        }
    }

    /// Entries a user can still undo.
    pub fn is_undoable_state(self) -> bool {
        matches!(self, Self::Done | Self::Interrupted)
    }
}

/// How a staged payload relates to the original path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedKind {
    /// Original was moved into the trash.
    Move,
    /// A snapshot copy was taken; the original stayed in place.
    Copy,
    /// Serialized metadata (mode/ownership) snapshot.
    Metadata,
    /// Path did not exist before the command; undo should remove it.
    Created,
    /// An empty directory removed by `git clean`; undo recreates it.
    Directory,
}

impl StagedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Copy => "copy",
            Self::Metadata => "metadata",
            Self::Created => "created",
            Self::Directory => "directory",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "move" => Self::Move,
            "copy" => Self::Copy,
            "metadata" => Self::Metadata,
            "directory" => Self::Directory,
            _ => Self::Created,
        }
    }
}

/// A command recorded in the journal.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub command: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub status: EntryStatus,
    pub exit_code: Option<i32>,
    pub undoable: bool,
    pub meta: Value,
    pub purged_at: Option<i64>,
}

impl Entry {
    /// Reconstruct an approximate command line for display.
    pub fn command_line(&self) -> String {
        let mut out = self.command.clone();
        for arg in &self.argv {
            out.push(' ');
            if arg.is_empty() || arg.contains([' ', '\t', '\'', '"']) {
                out.push('\'');
                out.push_str(&arg.replace('\'', "'\\''"));
                out.push('\'');
            } else {
                out.push_str(arg);
            }
        }
        out
    }

    /// True when the entry can be reversed right now.
    pub fn is_undoable(&self) -> bool {
        self.undoable && self.status.is_undoable_state()
    }
}

/// A staged payload row.
#[derive(Debug, Clone)]
pub struct StagedItem {
    pub id: i64,
    pub entry_id: i64,
    pub origin: String,
    pub stored: String,
    pub kind: StagedKind,
    pub size: u64,
    pub meta: Value,
}

/// Values needed to insert a staged payload row.
#[derive(Debug, Clone)]
pub struct NewStaged {
    pub origin: String,
    pub stored: String,
    pub kind: StagedKind,
    pub size: u64,
    pub meta: Value,
}

impl NewStaged {
    pub fn new(origin: impl Into<String>, stored: impl Into<String>, kind: StagedKind) -> Self {
        Self {
            origin: origin.into(),
            stored: stored.into(),
            kind,
            size: 0,
            meta: Value::Object(Default::default()),
        }
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.size = size;
        self
    }

    pub fn with_meta(mut self, meta: Value) -> Self {
        self.meta = meta;
        self
    }
}

/// Options for filtering journal history queries.
#[derive(Debug, Default, Clone)]
pub struct LogFilter {
    pub limit: usize,
    pub command: Option<String>,
    pub undoable_only: bool,
    pub status: Option<String>,
}

/// Result of a SQLite WAL checkpoint operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCheckpointResult {
    /// True if the checkpoint was blocked by an active reader or writer.
    pub busy: bool,
    /// Total number of pages in the WAL file before checkpointing.
    pub log_pages: i32,
    /// Number of pages successfully checkpointed to the database.
    pub checkpointed_pages: i32,
}

/// Aggregate counts for `rewind status`.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_entries: u64,
    pub undoable_entries: u64,
    pub undone_entries: u64,
    pub expired_entries: u64,
    pub staged_bytes: u64,
    pub oldest_undoable: Option<i64>,
    pub newest_entry: Option<i64>,
}

/// Handle to the journal database.
pub struct Journal {
    conn: Connection,
}

impl Journal {
    /// Open (creating if needed) the journal at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::paths::create_private_dir(parent)?;
        }
        let conn = Connection::open(path)
            .map_err(|err| Error::Journal(format!("opening {}: {err}", path.display())))?;
        conn.execute_batch(SCHEMA)
            .map_err(|err| Error::Journal(format!("initializing schema: {err}")))?;
        let _ = conn.execute(
            "ALTER TABLE reservations ADD COLUMN start_time INTEGER NOT NULL DEFAULT 0",
            [],
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let priv_perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(path, priv_perms.clone());
            let wal = path.with_extension("db-wal");
            if wal.exists() {
                let _ = std::fs::set_permissions(&wal, priv_perms.clone());
            }
            let shm = path.with_extension("db-shm");
            if shm.exists() {
                let _ = std::fs::set_permissions(&shm, priv_perms);
            }
        }

        Ok(Self { conn })
    }

    /// Open an in-memory journal (tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|err| Error::Journal(format!("open memory: {err}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|err| Error::Journal(format!("initializing schema: {err}")))?;
        let _ = conn.execute(
            "ALTER TABLE reservations ADD COLUMN start_time INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Self { conn })
    }

    /// Access the underlying SQLite connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Access the underlying SQLite connection mutably.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Open an existing journal without the ability to change entries.
    ///
    /// Unlike [`Journal::open`], this never creates the database or its
    /// schema. `PRAGMA query_only` makes SQLite reject accidental writes, so
    /// read-only commands such as `rewind inspect` cannot mutate the journal.
    pub fn open_query_only(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::Journal(format!(
                "no journal yet at {}",
                path.display()
            )));
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| Error::Journal(format!("opening {}: {err}", path.display())))?;
        conn.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 5000;")
            .map_err(|err| {
                Error::Journal(format!("opening {} read-only: {err}", path.display()))
            })?;
        Ok(Self { conn })
    }

    /// Record the start of an intercepted command and return its entry id.
    pub fn begin_entry(&mut self, command: &str, argv: &[String], cwd: &Path) -> Result<i64> {
        let redacted = crate::redact::redact_argv(argv);
        let argv_json = serde_json::to_string(&redacted)
            .map_err(|err| Error::Journal(format!("serializing argv: {err}")))?;
        self.conn
            .execute(
                "INSERT INTO entries (created_at, command, argv, cwd, status, meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, '{}')",
                params![
                    now_unix(),
                    command,
                    argv_json,
                    cwd.display().to_string(),
                    EntryStatus::Running.as_str()
                ],
            )
            .map_err(|err| Error::Journal(format!("inserting entry: {err}")))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Attach a staged payload row.
    pub fn add_staged(&mut self, entry_id: i64, item: &NewStaged) -> Result<i64> {
        let meta = serde_json::to_string(&item.meta)
            .map_err(|err| Error::Journal(format!("serializing staged meta: {err}")))?;
        self.conn
            .execute(
                "INSERT INTO staged (entry_id, origin, stored, kind, size, meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry_id,
                    item.origin,
                    item.stored,
                    item.kind.as_str(),
                    item.size as i64,
                    meta
                ],
            )
            .map_err(|err| Error::Journal(format!("inserting staged item: {err}")))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Mark the outcome of a command.
    pub fn finish_entry(
        &mut self,
        entry_id: i64,
        status: EntryStatus,
        exit_code: Option<i32>,
        undoable: bool,
        meta: Option<&Value>,
    ) -> Result<()> {
        let meta_json = meta
            .map(serde_json::to_string)
            .transpose()
            .map_err(|err| Error::Journal(format!("serializing entry meta: {err}")))?;
        self.conn
            .execute(
                "UPDATE entries
                    SET status = ?1,
                        exit_code = ?2,
                        undoable = ?3,
                        finished_at = ?4,
                        meta = COALESCE(?5, meta)
                  WHERE id = ?6",
                params![
                    status.as_str(),
                    exit_code,
                    i64::from(undoable),
                    now_unix(),
                    meta_json,
                    entry_id
                ],
            )
            .map_err(|err| Error::Journal(format!("finishing entry: {err}")))?;
        let _ = self.conn.execute(
            "DELETE FROM reservations WHERE entry_id = ?1",
            params![entry_id],
        );
        Ok(())
    }

    /// Store or replace entry-level metadata (for example mv source/dest pairs).
    pub fn set_entry_meta(&mut self, entry_id: i64, meta: &Value) -> Result<()> {
        let json = serde_json::to_string(meta)
            .map_err(|err| Error::Journal(format!("serializing entry meta: {err}")))?;
        self.conn
            .execute(
                "UPDATE entries SET meta = ?1 WHERE id = ?2",
                params![json, entry_id],
            )
            .map_err(|err| Error::Journal(format!("updating entry meta: {err}")))?;
        Ok(())
    }

    /// Fetch a single entry.
    pub fn entry(&self, entry_id: i64) -> Result<Option<Entry>> {
        self.conn
            .query_row(
                "SELECT id, created_at, finished_at, command, argv, cwd, status,
                        exit_code, undoable, meta, purged_at
                   FROM entries WHERE id = ?1",
                params![entry_id],
                row_to_entry,
            )
            .optional()
            .map_err(|err| Error::Journal(format!("loading entry {entry_id}: {err}")))
    }

    /// Staged rows for an entry, oldest first.
    pub fn staged_for(&self, entry_id: i64) -> Result<Vec<StagedItem>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, entry_id, origin, stored, kind, size, meta
                   FROM staged WHERE entry_id = ?1 ORDER BY id ASC",
            )
            .map_err(|err| Error::Journal(format!("preparing staged query: {err}")))?;
        let rows = stmt
            .query_map(params![entry_id], row_to_staged)
            .map_err(|err| Error::Journal(format!("querying staged items: {err}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Error::Journal(format!("reading staged item: {err}")))?);
        }
        Ok(out)
    }

    /// Remove a staged row after it has been restored.
    pub fn remove_staged(&mut self, staged_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM staged WHERE id = ?1", params![staged_id])
            .map_err(|err| Error::Journal(format!("deleting staged item: {err}")))?;
        Ok(())
    }

    /// Update the JSON metadata associated with one staged item.
    pub fn set_staged_meta(&mut self, staged_id: i64, meta: &Value) -> Result<()> {
        let text = serde_json::to_string(meta)
            .map_err(|err| Error::Journal(format!("serializing staged meta: {err}")))?;
        self.conn
            .execute(
                "UPDATE staged SET meta = ?1 WHERE id = ?2",
                params![text, staged_id],
            )
            .map_err(|err| Error::Journal(format!("updating staged meta: {err}")))?;
        Ok(())
    }

    /// Number of staged rows still attached to an entry.
    pub fn staged_count(&self, entry_id: i64) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM staged WHERE entry_id = ?1",
                params![entry_id],
                |row| row.get(0),
            )
            .map_err(|err| Error::Journal(format!("counting staged items: {err}")))?;
        Ok(count as u64)
    }

    /// Most recent entries that are still reversible, newest first.
    pub fn undoable_entries(&self, limit: usize) -> Result<Vec<Entry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, created_at, finished_at, command, argv, cwd, status,
                        exit_code, undoable, meta, purged_at
                   FROM entries
                  WHERE undoable = 1
                    AND status IN ('done', 'interrupted')
                  ORDER BY id DESC
                  LIMIT ?1",
            )
            .map_err(|err| Error::Journal(format!("preparing undo query: {err}")))?;
        let rows = stmt
            .query_map(params![limit as i64], row_to_entry)
            .map_err(|err| Error::Journal(format!("querying undoable entries: {err}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Error::Journal(format!("reading entry: {err}")))?);
        }
        Ok(out)
    }

    /// Mark an entry fully reversed.
    pub fn mark_undone(&mut self, entry_id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE entries
                    SET status = 'undone', finished_at = ?1, undoable = 0
                  WHERE id = ?2",
                params![now_unix(), entry_id],
            )
            .map_err(|err| Error::Journal(format!("marking entry undone: {err}")))?;
        Ok(())
    }

    /// List journal history, newest first.
    pub fn list(&self, limit: usize, include_undone: bool) -> Result<Vec<Entry>> {
        let sql = if include_undone {
            "SELECT id, created_at, finished_at, command, argv, cwd, status,
                    exit_code, undoable, meta, purged_at
               FROM entries ORDER BY id DESC LIMIT ?1"
        } else {
            "SELECT id, created_at, finished_at, command, argv, cwd, status,
                    exit_code, undoable, meta, purged_at
               FROM entries WHERE status != 'undone' ORDER BY id DESC LIMIT ?1"
        };
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|err| Error::Journal(format!("preparing log query: {err}")))?;
        let rows = stmt
            .query_map(params![limit as i64], row_to_entry)
            .map_err(|err| Error::Journal(format!("querying log: {err}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Error::Journal(format!("reading entry: {err}")))?);
        }
        Ok(out)
    }

    /// List journal history with flexible command, status, and undoability filters.
    pub fn list_filtered(&self, filter: &LogFilter) -> Result<Vec<Entry>> {
        let mut sql = String::from(
            "SELECT id, created_at, finished_at, command, argv, cwd, status,
                    exit_code, undoable, meta, purged_at
               FROM entries
              WHERE 1=1",
        );
        let mut params: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(cmd) = &filter.command {
            let cmd_lower = cmd.to_lowercase();
            sql.push_str(" AND (LOWER(command) = ? OR LOWER(command) LIKE ?)");
            params.push(rusqlite::types::Value::Text(cmd_lower.clone()));
            params.push(rusqlite::types::Value::Text(format!("%{cmd_lower}%")));
        }

        if let Some(st) = &filter.status {
            sql.push_str(" AND LOWER(status) = ?");
            params.push(rusqlite::types::Value::Text(st.to_lowercase()));
        }

        if filter.undoable_only {
            sql.push_str(" AND undoable = 1 AND status IN ('done', 'interrupted', 'running')");
        }

        sql.push_str(" ORDER BY id DESC LIMIT ?");
        let limit = if filter.limit == 0 { 30 } else { filter.limit };
        params.push(rusqlite::types::Value::Integer(limit as i64));

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|err| Error::Journal(format!("preparing filtered log query: {err}")))?;

        let param_refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), row_to_entry)
            .map_err(|err| Error::Journal(format!("querying filtered log: {err}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Error::Journal(format!("reading filtered entry: {err}")))?);
        }
        Ok(out)
    }

    /// Rows that were never finished (process killed, machine crashed).
    pub fn mark_stale_running(&mut self, older_than_secs: i64) -> Result<usize> {
        let cutoff = now_unix() - older_than_secs;
        let changed = self
            .conn
            .execute(
                "UPDATE entries
                    SET status = 'interrupted', undoable = 1
                  WHERE status = 'running' AND created_at < ?1",
                params![cutoff],
            )
            .map_err(|err| Error::Journal(format!("marking stale entries: {err}")))?;
        Ok(changed)
    }

    /// Sanitize historical entries in SQLite by running redaction on their stored argv.
    /// Returns the number of historical entries updated.
    pub fn sanitize_historical_entries(&mut self) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, argv FROM entries")
            .map_err(|err| Error::Journal(format!("preparing sanitize query: {err}")))?;
        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let argv_raw: String = row.get(1)?;
                Ok((id, argv_raw))
            })
            .map_err(|err| Error::Journal(format!("querying entries for sanitization: {err}")))?;

        let mut updates: Vec<(i64, String)> = Vec::new();
        for item in rows {
            let (id, argv_raw) =
                item.map_err(|err| Error::Journal(format!("reading entry: {err}")))?;
            if let Ok(argv) = serde_json::from_str::<Vec<String>>(&argv_raw) {
                let sanitized = crate::redact::redact_argv(&argv);
                if sanitized != argv {
                    if let Ok(new_json) = serde_json::to_string(&sanitized) {
                        updates.push((id, new_json));
                    }
                }
            }
        }

        let count = updates.len();
        for (id, new_json) in updates {
            self.conn
                .execute(
                    "UPDATE entries SET argv = ?1 WHERE id = ?2",
                    params![new_json, id],
                )
                .map_err(|err| Error::Journal(format!("updating sanitized entry: {err}")))?;
        }

        Ok(count)
    }

    /// Delete one entry (and its staged rows). Used for no-op commands.
    pub fn delete_entry(&mut self, entry_id: i64) -> Result<()> {
        let _ = self.conn.execute(
            "DELETE FROM reservations WHERE entry_id = ?1",
            params![entry_id],
        );
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![entry_id])
            .map_err(|err| Error::Journal(format!("deleting entry: {err}")))?;
        Ok(())
    }

    /// Entry ids whose staged payloads are older than `cutoff` and still
    /// present. Read-only; used by `clean --dry-run`.
    pub fn expirable_ids(&self, cutoff: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id FROM entries
                  WHERE created_at <= ?1
                    AND purged_at IS NULL
                    AND status IN ('done', 'interrupted', 'failed')
                  ORDER BY id ASC",
            )
            .map_err(|err| Error::Journal(format!("preparing expire query: {err}")))?;
        let rows = stmt
            .query_map(params![cutoff], |row| row.get::<_, i64>(0))
            .map_err(|err| Error::Journal(format!("querying expirable entries: {err}")))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(|err| Error::Journal(format!("reading entry id: {err}")))?);
        }
        Ok(ids)
    }

    /// Mark staged payloads older than `cutoff` as expired, returning the
    /// entry ids whose trash directories can now be deleted.
    pub fn expire_staged_before(&mut self, cutoff: i64) -> Result<Vec<i64>> {
        let ids = self.expirable_ids(cutoff)?;
        for id in &ids {
            self.expire_entry(*id)?;
        }
        Ok(ids)
    }

    /// Entries that are undoable but whose created_at is before `cutoff`.
    pub fn undoable_older_than(&self, cutoff: i64) -> Result<Vec<Entry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, created_at, finished_at, command, argv, cwd, status,
                        exit_code, undoable, meta, purged_at
                   FROM entries
                  WHERE created_at < ?1 AND undoable = 1
                    AND status IN ('done', 'interrupted')
                  ORDER BY id DESC",
            )
            .map_err(|err| Error::Journal(format!("preparing old-entry query: {err}")))?;
        let rows = stmt
            .query_map(params![cutoff], row_to_entry)
            .map_err(|err| Error::Journal(format!("querying old entries: {err}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Error::Journal(format!("reading entry: {err}")))?);
        }
        Ok(out)
    }

    /// Delete journal rows that were already purged and are older than `cutoff`.
    pub fn prune_history_before(&mut self, cutoff: i64) -> Result<usize> {
        self.conn
            .execute(
                "DELETE FROM entries
                  WHERE created_at < ?1 AND purged_at IS NOT NULL",
                params![cutoff],
            )
            .map_err(|err| Error::Journal(format!("pruning history: {err}")))
    }

    /// Expire one entry immediately (used to enforce the trash size cap).
    pub fn expire_entry(&mut self, entry_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM staged WHERE entry_id = ?1", params![entry_id])
            .map_err(|err| Error::Journal(format!("purging staged rows: {err}")))?;
        self.conn
            .execute(
                "UPDATE entries
                    SET purged_at = ?1, undoable = 0,
                        status = CASE WHEN status = 'done' THEN 'expired' ELSE status END
                  WHERE id = ?2",
                params![now_unix(), entry_id],
            )
            .map_err(|err| Error::Journal(format!("marking entry expired: {err}")))?;
        Ok(())
    }

    /// Delete finished journal history older than `cutoff`.
    pub fn prune_finished_before(&mut self, cutoff: i64) -> Result<usize> {
        self.conn
            .execute(
                "DELETE FROM entries
                  WHERE created_at < ?1 AND status IN ('undone', 'expired', 'failed')",
                params![cutoff],
            )
            .map_err(|err| Error::Journal(format!("pruning history: {err}")))
    }

    /// (id, created_at, size) for every undoable entry, oldest first.
    pub fn undoable_sizes(&self) -> Result<Vec<(i64, i64, u64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.id, e.created_at, COALESCE(SUM(s.size), 0)
                   FROM entries e LEFT JOIN staged s ON s.entry_id = e.id
                  WHERE e.undoable = 1 AND e.status IN ('done', 'interrupted')
                  GROUP BY e.id
                  ORDER BY e.id ASC",
            )
            .map_err(|err| Error::Journal(format!("preparing size query: {err}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })
            .map_err(|err| Error::Journal(format!("querying entry sizes: {err}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|err| Error::Journal(format!("reading entry size: {err}")))?);
        }
        Ok(out)
    }

    /// How many items and bytes are staged for one entry.
    pub fn staged_summary(&self, entry_id: i64) -> Result<(u64, u64)> {
        self.conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM staged WHERE entry_id = ?1",
                params![entry_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?.max(0) as u64,
                        row.get::<_, i64>(1)?.max(0) as u64,
                    ))
                },
            )
            .map_err(|err| Error::Journal(format!("staged summary: {err}")))
    }

    /// Aggregate statistics.
    pub fn stats(&self) -> Result<Stats> {
        let count = |sql: &str| -> Result<u64> {
            let value: i64 = self
                .conn
                .query_row(sql, [], |row| row.get(0))
                .map_err(|err| Error::Journal(format!("stats query: {err}")))?;
            Ok(value.max(0) as u64)
        };
        Ok(Stats {
            total_entries: count("SELECT COUNT(*) FROM entries")?,
            undoable_entries: count(
                "SELECT COUNT(*) FROM entries
                  WHERE undoable = 1 AND status IN ('done', 'interrupted')",
            )?,
            undone_entries: count("SELECT COUNT(*) FROM entries WHERE status = 'undone'")?,
            expired_entries: count("SELECT COUNT(*) FROM entries WHERE status = 'expired'")?,
            staged_bytes: count("SELECT COALESCE(SUM(size), 0) FROM staged")?,
            oldest_undoable: self
                .conn
                .query_row(
                    "SELECT MIN(created_at) FROM entries
                      WHERE undoable = 1 AND status IN ('done', 'interrupted')",
                    [],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(|err| Error::Journal(format!("stats query: {err}")))?,
            newest_entry: self
                .conn
                .query_row("SELECT MAX(created_at) FROM entries", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })
                .map_err(|err| Error::Journal(format!("stats query: {err}")))?,
        })
    }

    /// Return the total size in bytes of all currently staged files plus active reservations.
    pub fn current_staged_bytes(&self) -> Result<u64> {
        let staged: i64 = self
            .conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM staged", [], |row| {
                row.get(0)
            })
            .map_err(|err| Error::Journal(format!("querying total staged bytes: {err}")))?;
        let reserved: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(bytes), 0) FROM reservations",
                [],
                |row| row.get(0),
            )
            .map_err(|err| Error::Journal(format!("querying reservations: {err}")))?;
        Ok((staged.max(0) as u64).saturating_add(reserved.max(0) as u64))
    }

    /// Check whether staging `incoming_bytes` would exceed `max_bytes` capacity.
    ///
    /// If `max_bytes > 0` and current usage + `incoming_bytes` exceeds the cap:
    /// 1. Attempts to free space by expiring retention-expired entries (older than `retention_days`).
    /// 2. If capacity is still exceeded, returns `Error::Refused` with an actionable diagnostic.
    ///
    /// Valid undoable entries within retention are NEVER automatically purged (SEC-006).
    pub fn check_capacity(
        &mut self,
        incoming_bytes: u64,
        max_bytes: u64,
        retention_days: u64,
    ) -> Result<()> {
        if max_bytes == 0 {
            return Ok(());
        }

        let mut current = self.current_staged_bytes()?;
        if current.saturating_add(incoming_bytes) <= max_bytes {
            return Ok(());
        }

        if retention_days > 0 {
            let cutoff = now_unix() - (retention_days as i64) * 86_400;
            let expired_ids = self.expire_staged_before(cutoff)?;
            if !expired_ids.is_empty() {
                current = self.current_staged_bytes()?;
                if current.saturating_add(incoming_bytes) <= max_bytes {
                    return Ok(());
                }
            }
        }

        let remaining = max_bytes.saturating_sub(current);
        Err(Error::Refused(format!(
            "trash storage limit of {} exceeded (currently using {}, staging requires {}, only {} remaining); \
             run 'rewind clean' or increase trash.max_bytes in config",
            crate::util::human_size(max_bytes),
            crate::util::human_size(current),
            crate::util::human_size(incoming_bytes),
            crate::util::human_size(remaining)
        )))
    }

    /// Atomically check and reserve storage capacity for `entry_id` (SEC-006).
    ///
    /// Serializes with concurrent processes via SQLite `BEGIN IMMEDIATE`:
    /// 1. Sweeps dead PID and stale reservations.
    /// 2. Computes total staged bytes + active reservations from other entries.
    /// 3. If exceeding `max_bytes`, attempts to expire retention-expired entries.
    /// 4. If still exceeding, returns `Error::Refused`.
    /// 5. Otherwise records a reservation row for `entry_id` with current PID.
    pub fn reserve_capacity(
        &mut self,
        entry_id: i64,
        incoming_bytes: u64,
        max_bytes: u64,
        retention_days: u64,
    ) -> Result<()> {
        if max_bytes == 0 {
            return Ok(());
        }

        let pid = std::process::id();
        let now = now_unix();

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|err| Error::Journal(format!("begin immediate transaction: {err}")))?;

        // 1. Sweep stale or dead PID reservations
        {
            let mut stmt = tx
                .prepare("SELECT id, pid, start_time, created_at FROM reservations")
                .map_err(|err| Error::Journal(format!("prepare reservations query: {err}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, i64>(2)?.max(0) as u64,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|err| Error::Journal(format!("query reservations: {err}")))?;

            let mut stale_ids = Vec::new();
            for r in rows {
                let (res_id, res_pid, recorded_st, _created) =
                    r.map_err(|err| Error::Journal(format!("reading reservation: {err}")))?;
                let is_dead = {
                    #[cfg(unix)]
                    {
                        let ret = unsafe { libc::kill(res_pid as libc::pid_t, 0) };
                        if ret == -1 {
                            std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                        } else {
                            // Process exists. Detect PID reuse by comparing start_time.
                            if recorded_st > 0 {
                                if let Some(current_st) =
                                    crate::util::get_process_start_time(res_pid)
                                {
                                    current_st != recorded_st
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = res_pid;
                        let _ = recorded_st;
                        false
                    }
                };
                if is_dead {
                    stale_ids.push(res_id);
                }
            }
            drop(stmt);

            for id in stale_ids {
                tx.execute("DELETE FROM reservations WHERE id = ?1", params![id])
                    .map_err(|err| Error::Journal(format!("deleting stale reservation: {err}")))?;
            }
        }

        // 2. Query total usage: staged + reservations from other entries
        let get_usage = |tx: &rusqlite::Transaction| -> Result<u64> {
            let staged_bytes: i64 = tx
                .query_row("SELECT COALESCE(SUM(size), 0) FROM staged", [], |row| {
                    row.get(0)
                })
                .map_err(|err| Error::Journal(format!("summing staged bytes: {err}")))?;
            let reserved_bytes: i64 = tx
                .query_row(
                    "SELECT COALESCE(SUM(bytes), 0) FROM reservations WHERE entry_id != ?1",
                    params![entry_id],
                    |row| row.get(0),
                )
                .map_err(|err| Error::Journal(format!("summing reservations: {err}")))?;
            Ok((staged_bytes.max(0) as u64).saturating_add(reserved_bytes.max(0) as u64))
        };

        let mut current = get_usage(&tx)?;
        if current.saturating_add(incoming_bytes) > max_bytes && retention_days > 0 {
            let cutoff = now - (retention_days as i64) * 86_400;
            let mut stmt = tx
                .prepare(
                    "SELECT id FROM entries
                      WHERE created_at <= ?1
                        AND purged_at IS NULL
                        AND status IN ('done', 'interrupted', 'failed')
                      ORDER BY id ASC",
                )
                .map_err(|err| Error::Journal(format!("querying expirable entries: {err}")))?;
            let expirable_ids: Vec<i64> = stmt
                .query_map(params![cutoff], |row| row.get(0))
                .map_err(|err| Error::Journal(format!("mapping expirable entries: {err}")))?
                .filter_map(|r| r.ok())
                .collect();
            drop(stmt);

            for exp_id in expirable_ids {
                tx.execute("DELETE FROM staged WHERE entry_id = ?1", params![exp_id])
                    .map_err(|err| Error::Journal(format!("purging staged rows: {err}")))?;
                tx.execute(
                    "UPDATE entries
                        SET purged_at = ?1, undoable = 0,
                            status = CASE WHEN status = 'done' THEN 'expired' ELSE status END
                      WHERE id = ?2",
                    params![now, exp_id],
                )
                .map_err(|err| Error::Journal(format!("marking entry expired: {err}")))?;
            }

            current = get_usage(&tx)?;
        }

        if current.saturating_add(incoming_bytes) > max_bytes {
            let remaining = max_bytes.saturating_sub(current);
            return Err(Error::Refused(format!(
                "trash storage limit of {} exceeded (currently using {}, staging requires {}, only {} remaining); \
                 run 'rewind clean' or increase trash.max_bytes in config",
                crate::util::human_size(max_bytes),
                crate::util::human_size(current),
                crate::util::human_size(incoming_bytes),
                crate::util::human_size(remaining)
            )));
        }

        let start_time = crate::util::get_process_start_time(pid).unwrap_or(0);
        tx.execute(
            "INSERT INTO reservations (entry_id, pid, start_time, bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry_id, pid, start_time as i64, incoming_bytes as i64, now],
        )
        .map_err(|err| Error::Journal(format!("recording capacity reservation: {err}")))?;

        tx.commit()
            .map_err(|err| Error::Journal(format!("committing capacity reservation: {err}")))?;

        Ok(())
    }

    /// Run `VACUUM` to shrink the journal after garbage collection.
    pub fn vacuum(&self) -> Result<()> {
        self.conn
            .execute_batch("VACUUM;")
            .map_err(|err| Error::Journal(format!("vacuum: {err}")))
    }

    /// Checkpoint and truncate the SQLite Write-Ahead Log (WAL) to free disk space immediately.
    /// Returns the exact checkpoint statistics and whether the checkpoint was blocked by busy readers.
    pub fn wal_checkpoint(&self) -> Result<WalCheckpointResult> {
        let (busy, log_pages, checkpointed_pages): (i32, i32, i32) = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE);", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|err| Error::Journal(format!("wal checkpoint query: {err}")))?;

        Ok(WalCheckpointResult {
            busy: busy != 0,
            log_pages,
            checkpointed_pages,
        })
    }

    /// Return the total number of bytes currently reserved by active, in-flight commands.
    pub fn active_reservations_bytes(&self) -> Result<u64> {
        let bytes: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(bytes), 0) FROM reservations",
                [],
                |row| row.get(0),
            )
            .map_err(|err| Error::Journal(format!("querying active reservations: {err}")))?;
        Ok(bytes.max(0) as u64)
    }
}

fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    let argv_json: String = row.get(4)?;
    let meta_json: String = row.get(9)?;
    let status: String = row.get(6)?;
    Ok(Entry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        finished_at: row.get(2)?,
        command: row.get(3)?,
        argv: serde_json::from_str(&argv_json).unwrap_or_default(),
        cwd: row.get(5)?,
        status: EntryStatus::parse(&status),
        exit_code: row.get(7)?,
        undoable: row.get::<_, i64>(8)? != 0,
        meta: serde_json::from_str(&meta_json).unwrap_or(Value::Null),
        purged_at: row.get(10)?,
    })
}

fn row_to_staged(row: &Row<'_>) -> rusqlite::Result<StagedItem> {
    let kind: String = row.get(4)?;
    let meta_json: String = row.get(6)?;
    Ok(StagedItem {
        id: row.get(0)?,
        entry_id: row.get(1)?,
        origin: row.get(2)?,
        stored: row.get(3)?,
        kind: StagedKind::parse(&kind),
        size: row.get::<_, i64>(5)?.max(0) as u64,
        meta: serde_json::from_str(&meta_json).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal() -> Journal {
        Journal::open_memory().unwrap()
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn entry_lifecycle() {
        let mut j = journal();
        let id = j
            .begin_entry("rm", &args(&["-rf", "build"]), Path::new("/tmp"))
            .unwrap();
        j.add_staged(
            id,
            &NewStaged::new("/tmp/build", "/data/trash/1/build", StagedKind::Move).with_size(42),
        )
        .unwrap();
        j.finish_entry(id, EntryStatus::Done, Some(0), true, None)
            .unwrap();

        let entry = j.entry(id).unwrap().unwrap();
        assert_eq!(entry.status, EntryStatus::Done);
        assert!(entry.is_undoable());
        assert_eq!(entry.command_line(), "rm -rf build");
        assert_eq!(j.staged_count(id).unwrap(), 1);
        assert_eq!(j.stats().unwrap().staged_bytes, 42);

        j.mark_undone(id).unwrap();
        assert_eq!(j.entry(id).unwrap().unwrap().status, EntryStatus::Undone);
        assert!(j.undoable_entries(10).unwrap().is_empty());
    }

    #[test]
    fn undoable_entries_are_newest_first_and_skip_failures() {
        let mut j = journal();
        for (cmd, undoable) in [("rm", true), ("cp", false), ("mv", true)] {
            let id = j.begin_entry(cmd, &args(&["x"]), Path::new("/")).unwrap();
            j.finish_entry(
                id,
                if undoable {
                    EntryStatus::Done
                } else {
                    EntryStatus::Failed
                },
                Some(0),
                undoable,
                None,
            )
            .unwrap();
        }
        let entries = j.undoable_entries(10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].command, "mv");
        assert_eq!(entries[1].command, "rm");
        assert_eq!(j.undoable_entries(1).unwrap().len(), 1);
    }

    #[test]
    fn entry_meta_roundtrips() {
        let mut j = journal();
        let id = j
            .begin_entry("mv", &args(&["a", "b"]), Path::new("/"))
            .unwrap();
        let meta = serde_json::json!({"pairs": [{"src": "a", "dest": "b"}]});
        j.set_entry_meta(id, &meta).unwrap();
        assert_eq!(j.entry(id).unwrap().unwrap().meta, meta);
    }

    #[test]
    fn expiry_marks_entries_and_returns_ids() {
        let mut j = journal();
        let id = j.begin_entry("rm", &args(&["x"]), Path::new("/")).unwrap();
        j.add_staged(
            id,
            &NewStaged::new("x", "/trash/x", StagedKind::Move).with_size(10),
        )
        .unwrap();
        j.finish_entry(id, EntryStatus::Done, Some(0), true, None)
            .unwrap();

        let ids = j.expire_staged_before(now_unix() + 10).unwrap();
        assert_eq!(ids, vec![id]);
        let entry = j.entry(id).unwrap().unwrap();
        assert_eq!(entry.status, EntryStatus::Expired);
        assert!(!entry.undoable);
        assert_eq!(j.staged_count(id).unwrap(), 0);
        assert!(j.expire_staged_before(now_unix() + 10).unwrap().is_empty());
    }

    #[test]
    fn stale_running_entries_become_interrupted() {
        let mut j = journal();
        let id = j.begin_entry("rm", &args(&["x"]), Path::new("/")).unwrap();
        assert_eq!(j.mark_stale_running(-1).unwrap(), 1);
        assert_eq!(
            j.entry(id).unwrap().unwrap().status,
            EntryStatus::Interrupted
        );
    }

    #[test]
    fn open_query_only_enforces_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("journal.db");
        assert!(Journal::open_query_only(&db_path).is_err());
        assert!(!db_path.exists());

        {
            let mut j = Journal::open(&db_path).unwrap();
            j.begin_entry("rm", &args(&["test"]), Path::new("/"))
                .unwrap();
        }

        let qj = Journal::open_query_only(&db_path).unwrap();
        let entries = qj.list(10, false).unwrap();
        assert_eq!(entries.len(), 1);

        // Attempting an insert or write must fail under SQLITE_OPEN_READ_ONLY / PRAGMA query_only
        let write_res = qj.conn.execute("DELETE FROM entries", []);
        assert!(write_res.is_err());
    }
}
