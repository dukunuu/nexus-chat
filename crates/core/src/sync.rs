//! Phase 3 merge engine: changeset build/apply, per-table sync rules,
//! cursor + ack bookkeeping, and the file blob channel.
//!
//! Every table that syncs declares itself in the registry below — its
//! cursor rule (how exports resume) and its apply rule (how imports land).
//! The rules are deliberately small:
//!
//! - **LWW** (`updated_at` cursor): the incoming row wins iff
//!   `(updated_at, pk…) > (local updated_at, pk…)`. RFC3339 timestamps
//!   compare lexically, so both devices compute the same winner with no
//!   origin columns. A tie (same row written in the same nanosecond on two
//!   clocks — the clock-skew residual) keeps the local row on both sides;
//!   it is stable, never flip-flops, and is accepted per the Phase 1
//!   decision.
//! - **Append** (`(created_at, id)` tuple or AUTOINCREMENT cursor):
//!   INSERT OR IGNORE by the row's sync identity (uuid id, or `sync_id`
//!   for the AUTOINCREMENT tables). Idempotent union — the exactly-once
//!   backstop when a changeset is re-imported after a crash.
//! - **Tombstones** (AUTOINCREMENT cursor): applied destructively — a
//!   session tombstone cascades its messages and sources, a space
//!   tombstone removes the space row and its directory, a file tombstone
//!   removes the row and the local blob.
//! - **`swarm_personas`** has no cursor of its own: it is versioned by the
//!   owning session (Phase 1 decision). Persona rows travel attached to
//!   their session row and are applied only when that session row wins
//!   LWW, as a wholesale roster replace.
//!
//! Cursor lifecycle (the ack design): an export does not advance
//! `push_cursor` — the receiver imports, then replies with an ack
//! carrying its new pull cursors, and only then does the sender advance
//! `push_cursor`. Idempotent apply is the backstop for crashes mid-
//! exchange.

// Casts here are on bounded values: byte sizes, row counts, ordinals — the
// same justification as db.rs.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension as _, params_from_iter};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::db::DEFAULT_SPACE;
use crate::db::Db;
use crate::space::Space;

// ── changeset types ──

/// One device's export, possibly carrying an ack for the device it was
/// sent to. Serde JSON over the wire (or inside a zip bundle with blobs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Changeset {
    /// The sender (from `device_meta`, created on first sync).
    pub device_id: String,
    /// "laptop" — for the receiver's log/status.
    pub device_name: String,
    /// "I imported up to here" — the sender's reply acks the *receiver*'s
    /// data, so entries are addressed by the receiver's device id.
    pub ack: Option<Vec<PeerCursor>>,
    /// Full rows as JSON objects keyed by column name.
    pub rows: Vec<RowChange>,
    /// Deletes that actually happened on the sender.
    pub tombstones: Vec<Tombstone>,
    /// File metadata — the blobs themselves follow via the transport's
    /// blob channel (`blobs/<space_id>/<name>` under a sync dir, or inside
    /// a zip bundle).
    pub files: Vec<FileChange>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RowChange {
    pub table: String,
    pub row: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tombstone {
    /// The sender's `sync_tombstones` AUTOINCREMENT id — cursor
    /// bookkeeping only, never applied.
    pub origin_id: i64,
    pub table_name: String,
    pub row_id: String,
    pub deleted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileChange {
    pub space_id: String,
    pub name: String,
    pub hash: String,
    pub size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerCursor {
    /// The device whose data was imported up to `cursor` — the ack's
    /// target, matched against the receiver's own device id.
    pub peer_id: String,
    pub table_name: String,
    pub cursor: String,
}

/// What applying a changeset did — the receiver's log line, plus the
/// blobs the transport still has to fetch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ApplySummary {
    pub rows_applied: usize,
    pub rows_skipped: usize,
    pub tombstones_applied: usize,
    pub acks_applied: usize,
    pub files_kept: usize,
    pub files_pulled: usize,
    pub files_missing: Vec<FileChange>,
    pub warnings: Vec<String>,
}

// ── the registry ──

/// How a table's rows are ordered and resumed across changesets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// Mutable rows: the cursor is the `updated_at` column (nullable — a
    /// NULL version sorts as `''`, i.e. the epoch; rows that never got a
    /// version are legacy and simply never export). Incoming row wins iff
    /// `(updated_at, pk…) > (local updated_at, pk…)`.
    UpdatedAt,
    /// Append-only rows: cursor is a `(col1, col2)` row-value tuple
    /// (e.g. messages `(created_at, id)`), encoded as `"a|b"`.
    Tuple(&'static [&'static str]),
    /// Append-only rows whose cursor is their device-local AUTOINCREMENT
    /// id (citations, `sync_tombstones`). The id rides along in the row JSON
    /// for cursor bookkeeping but is never applied.
    AutoId,
    /// No cursor of its own; rows travel attached to a parent row
    /// (`swarm_personas` → sessions).
    None,
}

/// One syncable table's cursor + apply rules.
pub struct TableSpec {
    pub name: &'static str,
    /// Columns exported in the row JSON (order = the SELECT list).
    pub columns: &'static [&'static str],
    /// Columns applied on insert/update — a subset of `columns`: the
    /// AUTOINCREMENT ids are excluded where they'd collide across devices
    /// (citations) and kept where the id is the sync identity.
    pub apply_columns: &'static [&'static str],
    /// The sync identity: the INSERT OR IGNORE target and the LWW tiebreak.
    pub pk: &'static [&'static str],
    pub cursor: Cursor,
}

/// The engine's registry, in apply order (sessions before their children,
/// spaces/files before messages so foreign rows land after their parents).
pub const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "sessions",
        columns: &[
            "id",
            "title",
            "model",
            "slug",
            "space_id",
            "compact_summary",
            "compact_through",
            "web_mode",
            "swarm_mode",
            "kind",
            "research_parent_id",
            "created_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "title",
            "model",
            "slug",
            "space_id",
            "compact_summary",
            "compact_through",
            "web_mode",
            "swarm_mode",
            "kind",
            "research_parent_id",
            "created_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "swarm_personas",
        columns: &["session_id", "ord", "name", "model", "persona"],
        apply_columns: &["session_id", "ord", "name", "model", "persona"],
        pk: &["session_id", "ord"],
        cursor: Cursor::None,
    },
    TableSpec {
        name: "model_prefs",
        columns: &["id", "favorite", "last_used", "reasoning", "updated_at"],
        apply_columns: &["id", "favorite", "last_used", "reasoning", "updated_at"],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "spaces",
        columns: &["id", "name", "created_at", "updated_at"],
        apply_columns: &["id", "name", "created_at", "updated_at"],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "files",
        columns: &[
            "id",
            "space_id",
            "name",
            "hash",
            "size",
            "created_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "name",
            "hash",
            "size",
            "created_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "watches",
        columns: &[
            "id",
            "space_id",
            "topic",
            "interval_hours",
            "session_id",
            "last_run_at",
            "updated_at",
        ],
        apply_columns: &[
            "id",
            "space_id",
            "topic",
            "interval_hours",
            "session_id",
            "last_run_at",
            "updated_at",
        ],
        pk: &["id"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "app_settings",
        columns: &["key", "value", "scope", "updated_at"],
        apply_columns: &["key", "value", "scope", "updated_at"],
        pk: &["key"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "session_sources",
        columns: &["session_id", "url_norm", "flag", "updated_at"],
        apply_columns: &["session_id", "url_norm", "flag", "updated_at"],
        pk: &["session_id", "url_norm"],
        cursor: Cursor::UpdatedAt,
    },
    TableSpec {
        name: "messages",
        columns: &[
            "id",
            "session_id",
            "role",
            "content",
            "model",
            "reasoning",
            "tokens",
            "secs",
            "cost",
            "phrase",
            "persona",
            "created_at",
        ],
        apply_columns: &[
            "id",
            "session_id",
            "role",
            "content",
            "model",
            "reasoning",
            "tokens",
            "secs",
            "cost",
            "phrase",
            "persona",
            "created_at",
        ],
        pk: &["id"],
        cursor: Cursor::Tuple(&["created_at", "id"]),
    },
    TableSpec {
        name: "usage_log",
        columns: &[
            "sync_id",
            "created_at",
            "session_id",
            "space_id",
            "backend",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "cost",
            "cost_is_provider",
            "updated_at",
        ],
        apply_columns: &[
            "sync_id",
            "created_at",
            "session_id",
            "space_id",
            "backend",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "cost",
            "cost_is_provider",
            "updated_at",
        ],
        pk: &["sync_id"],
        cursor: Cursor::Tuple(&["created_at", "sync_id"]),
    },
    TableSpec {
        name: "citations",
        // `id` rides along for the cursor but is never applied — it is a
        // device-local AUTOINCREMENT id.
        columns: &["id", "sync_id", "space_id", "report_file", "url", "title"],
        apply_columns: &["sync_id", "space_id", "report_file", "url", "title"],
        pk: &["sync_id"],
        cursor: Cursor::AutoId,
    },
];

const SYNC_TOMBSTONES: &str = "sync_tombstones";

fn spec_for(table: &str) -> Option<&'static TableSpec> {
    TABLES.iter().find(|s| s.name == table)
}

fn known_table(table: &str) -> bool {
    table == SYNC_TOMBSTONES || spec_for(table).is_some()
}

/// Cursor comparisons are table-aware: the AUTOINCREMENT tables compare
/// numerically (string comparison would order "9" > "10"), everything else
/// lexically (RFC3339 sorts correctly, and the tuple cursors are
/// `timestamp|id` whose leading timestamp dominates).
fn position_gt(table: &str, a: &str, b: &str) -> bool {
    if table == "citations" || table == SYNC_TOMBSTONES {
        let n = |s: &str| s.parse::<i64>().unwrap_or(0);
        n(a) > n(b)
    } else {
        a > b
    }
}

/// A row's cursor position — the pull cursor advances to the max position
/// among received rows, win or lose (a losing row was still imported).
fn row_position(spec: &TableSpec, row: &serde_json::Value) -> String {
    match spec.cursor {
        Cursor::UpdatedAt => row
            .get("updated_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Cursor::Tuple(cols) => cols
            .iter()
            .map(|c| {
                row.get(*c)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("|"),
        Cursor::AutoId => row
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .map_or_else(|| "0".to_string(), |i| i.to_string()),
        Cursor::None => String::new(),
    }
}

/// A value that must be a single path component — names flow into
/// `spaces/<name>/files/<name>` and `blobs/<space_id>/<name>`, so they
/// must not smuggle separators or `..`.
fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// The human-readable device name riding in every changeset.
pub fn device_name() -> String {
    if let Ok(n) = std::env::var("NEXUS_DEVICE_NAME")
        && !n.trim().is_empty()
    {
        return n;
    }
    std::fs::read_to_string("/etc/hostname")
        .map_or_else(|_| "nexus-device".to_string(), |s| s.trim().to_string())
}

// ── export ──

/// Build this device's export for `peer_id` — every row past the peer's
/// acked cursor per table (no cursor → full export, the first-run
/// bootstrap), the un-acked tombstones, and the file manifest. Does not
/// advance any cursor: that happens only when the peer acks.
pub fn build_changeset(db: &Db, peer_id: Option<&str>, name: &str) -> Result<Changeset> {
    let device_id = db.device_id()?;
    let mut cs = Changeset {
        device_id,
        device_name: name.to_string(),
        ack: None,
        rows: Vec::new(),
        tombstones: Vec::new(),
        files: Vec::new(),
        generated_at: chrono::Utc::now().to_rfc3339(),
    };
    let states = db.load_sync_state()?;
    let push_cursor = |table: &str| {
        peer_id.and_then(|p| {
            states
                .iter()
                .find(|s| s.peer_id == p && s.table_name == table)
                .and_then(|s| s.push_cursor.clone())
        })
    };
    for spec in TABLES {
        if spec.cursor == Cursor::None {
            continue;
        }
        let rows = select_rows(db, spec, push_cursor(spec.name).as_deref())?;
        for row in rows {
            if spec.name == "sessions" {
                for persona in select_personas(db, &row)? {
                    cs.rows.push(RowChange {
                        table: "swarm_personas".to_string(),
                        row: persona,
                    });
                }
            }
            if spec.name == "files" {
                cs.files.push(FileChange {
                    space_id: row
                        .get("space_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: row
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    hash: row
                        .get("hash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    size: row
                        .get("size")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                });
            }
            cs.rows.push(RowChange {
                table: spec.name.to_string(),
                row,
            });
        }
    }
    let cursor = push_cursor(SYNC_TOMBSTONES);
    let c = cursor.as_deref().unwrap_or("0");
    let mut stmt = db.conn().prepare(&format!(
        "SELECT id, table_name, row_id, deleted_at FROM {SYNC_TOMBSTONES} WHERE id > ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([c], |r| {
        Ok(Tombstone {
            origin_id: r.get(0)?,
            table_name: r.get(1)?,
            row_id: r.get(2)?,
            deleted_at: r.get(3)?,
        })
    })?;
    cs.tombstones = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cs)
}

/// The ack for `peer_id`: this device's pull cursors for that peer's
/// data — "I imported your rows up to here". The transports embed it in
/// their next changeset so the peer can advance its push cursors; without
/// it the peer would re-export everything forever.
pub fn build_ack(db: &Db, peer_id: &str) -> Result<Vec<PeerCursor>> {
    Ok(db
        .load_sync_state()?
        .iter()
        .filter(|s| s.peer_id == peer_id)
        .filter_map(|s| {
            s.pull_cursor.clone().map(|cursor| PeerCursor {
                peer_id: peer_id.to_string(),
                table_name: s.table_name.clone(),
                cursor,
            })
        })
        .collect())
}

/// The rows of one table past a cursor, in cursor order. Rows that never
/// got an `updated_at` (legacy, pre-version rows) stay home — they could
/// never win LWW anyway.
fn select_rows(db: &Db, spec: &TableSpec, cursor: Option<&str>) -> Result<Vec<serde_json::Value>> {
    let cols = spec.columns.join(", ");
    let (sql, params): (String, Vec<rusqlite::types::Value>) = match spec.cursor {
        Cursor::UpdatedAt => {
            // Device-local settings are structurally invisible to sync.
            let scope_filter = if spec.name == "app_settings" {
                " AND scope = 'sync'"
            } else {
                ""
            };
            (
                format!(
                    "SELECT {cols} FROM {} WHERE COALESCE(updated_at, '') > ?1{scope_filter} \
                     ORDER BY COALESCE(updated_at, ''), {}",
                    spec.name,
                    spec.pk.join(", ")
                ),
                vec![rusqlite::types::Value::Text(
                    cursor.unwrap_or("").to_string(),
                )],
            )
        }
        Cursor::Tuple(tuple_cols) => {
            let (a, b) = (tuple_cols[0], tuple_cols[1]);
            let (ca, cb) = cursor.and_then(|c| c.split_once('|')).unwrap_or(("", ""));
            (
                format!(
                    "SELECT {cols} FROM {} WHERE ({a}, {b}) > (?1, ?2) ORDER BY {a}, {b}",
                    spec.name
                ),
                vec![
                    rusqlite::types::Value::Text(ca.to_string()),
                    rusqlite::types::Value::Text(cb.to_string()),
                ],
            )
        }
        Cursor::AutoId => (
            format!("SELECT {cols} FROM {} WHERE id > ?1 ORDER BY id", spec.name),
            vec![rusqlite::types::Value::Text(
                cursor.unwrap_or("0").to_string(),
            )],
        ),
        Cursor::None => bail!("{} has no cursor", spec.name),
    };
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |r| {
        row_to_json(r, spec.columns)
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A session row's persona roster — attached to the session row in the
/// changeset (personas have no cursor of their own).
fn select_personas(db: &Db, session: &serde_json::Value) -> Result<Vec<serde_json::Value>> {
    let Some(session_id) = session.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(Vec::new());
    };
    let mut stmt = db.conn().prepare(
        "SELECT session_id, ord, name, model, persona FROM swarm_personas WHERE session_id = ?1",
    )?;
    let rows = stmt.query_map([session_id], |r| {
        row_to_json(r, &["session_id", "ord", "name", "model", "persona"])
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn row_to_json(row: &rusqlite::Row, columns: &[&str]) -> rusqlite::Result<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    for (i, column) in columns.iter().enumerate() {
        let v: rusqlite::types::Value = row.get(i)?;
        obj.insert((*column).to_string(), sqlite_to_json(v));
    }
    Ok(serde_json::Value::Object(obj))
}

fn sqlite_to_json(v: rusqlite::types::Value) -> serde_json::Value {
    match v {
        rusqlite::types::Value::Null | rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::Value::from(i),
        rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        rusqlite::types::Value::Text(s) => serde_json::Value::from(s),
    }
}

fn json_to_value(v: &serde_json::Value) -> rusqlite::types::Value {
    match v {
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            rusqlite::types::Value::Null
        }
        serde_json::Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
            rusqlite::types::Value::Integer,
        ),
        serde_json::Value::String(s) => rusqlite::types::Value::Text(s.clone()),
    }
}

/// The first 8 chars of a uuid — enough to disambiguate in log lines, and
/// the suffix the space-name collision resolution renames with.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// ── apply ──

enum LwwOutcome {
    Applied,
    Skipped,
    Warned(String),
}

/// Whether an incoming row wins LWW against a local opponent.
/// `local_updated`/`local_tie` are `''`/empty when no local row exists.
fn lww_wins(
    spec: &TableSpec,
    row: &serde_json::Value,
    local_updated: &str,
    local_tie: &str,
) -> bool {
    let incoming_updated = row
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let incoming_tie = spec
        .pk
        .iter()
        .map(|c| {
            row.get(*c)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\u{1f}");
    (incoming_updated, incoming_tie.as_str()) > (local_updated, local_tie)
}

/// `(updated_at, id) > (local_updated_at, local_id)` — the spaces/files
/// LWW comparison, id-tiebroken so equal timestamps stay deterministic.
fn lww_wins_against(id: &str, updated: &str, local_id: &str, local_updated: &str) -> bool {
    (updated, id) > (local_updated, local_id)
}

/// Apply one changeset: rows (per the registry), tombstones, file blobs,
/// and the embedded ack. Returns the summary plus the reply cursors — the
/// receiver's new pull cursors per table, to be acked back to the sender.
/// `blob_source`, when given, is a directory whose `blobs/<space_id>/<name>`
/// holds the sender's payloads; missing blobs are reported instead.
// Long by design: one step per sync rule, in registry order.
#[allow(clippy::too_many_lines)]
pub fn apply_changeset(
    db: &Db,
    space: &Space,
    cs: &Changeset,
    blob_source: Option<&Path>,
) -> Result<(ApplySummary, Vec<PeerCursor>)> {
    let my_id = db.device_id()?;
    let mut summary = ApplySummary::default();
    let mut by_table: HashMap<&str, Vec<&serde_json::Value>> = HashMap::new();
    for rc in &cs.rows {
        by_table.entry(rc.table.as_str()).or_default().push(&rc.row);
    }
    for table in by_table.keys() {
        if !known_table(table) {
            summary
                .warnings
                .push(format!("changeset has rows for unknown table {table:?}"));
        }
    }

    // Rows, in registry order. `max_pos` tracks the highest position
    // received per table — the reply cursor. Positions advance even for
    // rows that lost LWW or failed: a single bad row must not block the
    // table forever (the warning names it).
    let mut max_pos: HashMap<&'static str, String> = HashMap::new();
    let mut won_sessions: HashSet<String> = HashSet::new();
    let mut won_files: HashSet<(String, String)> = HashSet::new();
    for spec in TABLES {
        if spec.cursor == Cursor::None {
            continue; // swarm_personas travel with their session row
        }
        let Some(rows) = by_table.get(spec.name) else {
            continue;
        };
        for row in rows {
            let pos = row_position(spec, row);
            max_pos
                .entry(spec.name)
                .and_modify(|p| {
                    if position_gt(spec.name, &pos, p) {
                        p.clone_from(&pos);
                    }
                })
                .or_insert(pos);
            let outcome = match apply_row(db, space, spec, row) {
                Ok(o) => o,
                Err(e) => LwwOutcome::Warned(format!("{} row failed: {e:#}", spec.name)),
            };
            match outcome {
                LwwOutcome::Applied => {
                    summary.rows_applied += 1;
                    if spec.name == "sessions"
                        && let Some(id) = row.get("id").and_then(serde_json::Value::as_str)
                    {
                        won_sessions.insert(id.to_string());
                    }
                    if spec.name == "files"
                        && let (Some(sid), Some(name)) = (
                            row.get("space_id").and_then(serde_json::Value::as_str),
                            row.get("name").and_then(serde_json::Value::as_str),
                        )
                    {
                        won_files.insert((sid.to_string(), name.to_string()));
                    }
                }
                LwwOutcome::Skipped => summary.rows_skipped += 1,
                LwwOutcome::Warned(w) => {
                    summary.rows_skipped += 1;
                    summary.warnings.push(w);
                }
            }
        }
    }

    // swarm_personas: applied only for sessions that won in this changeset,
    // as a wholesale roster replace (the collection has no per-row LWW).
    if let Some(personas) = by_table.get("swarm_personas") {
        let mut rosters: HashMap<&str, Vec<&serde_json::Value>> = HashMap::new();
        for p in personas {
            if let Some(sid) = p.get("session_id").and_then(serde_json::Value::as_str) {
                rosters.entry(sid).or_default().push(p);
            }
        }
        for (sid, roster) in rosters {
            if won_sessions.contains(sid) {
                let conn = db.conn();
                conn.execute("DELETE FROM swarm_personas WHERE session_id = ?1", [sid])?;
                for p in &roster {
                    let values: Vec<rusqlite::types::Value> =
                        ["session_id", "ord", "name", "model", "persona"]
                            .iter()
                            .map(|c| json_to_value(p.get(*c).unwrap_or(&serde_json::Value::Null)))
                            .collect();
                    conn.execute(
                        "INSERT INTO swarm_personas (session_id, ord, name, model, persona)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params_from_iter(values.iter()),
                    )?;
                }
                summary.rows_applied += roster.len();
            } else {
                summary.rows_skipped += roster.len();
                summary.warnings.push(format!(
                    "swarm personas for session {sid} skipped — their session row lost LWW"
                ));
            }
        }
    }

    // Tombstones, after rows: within one changeset a row and its tombstone
    // never coexist (a deleted row is not re-exported), and across
    // changesets the destructive delete is the newer intent.
    let mut tombstone_max = 0i64;
    for t in &cs.tombstones {
        tombstone_max = tombstone_max.max(t.origin_id);
        if apply_tombstone(db, space, t, &mut summary) {
            summary.tombstones_applied += 1;
        }
    }
    if !cs.tombstones.is_empty() {
        max_pos.insert(SYNC_TOMBSTONES, tombstone_max.to_string());
    }

    // File blobs: for every file row that won, keep the local blob when it
    // matches, else pull it from the transport's blob channel.
    for fc in &cs.files {
        if !won_files.contains(&(fc.space_id.clone(), fc.name.clone())) {
            continue;
        }
        if !valid_component(&fc.name) || !valid_component(&fc.space_id) {
            summary.warnings.push(format!(
                "skipping blob for unsafe file {:?}/{:?}",
                fc.space_id, fc.name
            ));
            continue;
        }
        let Some(space_name) = space_name_for(db, &fc.space_id)? else {
            summary.warnings.push(format!(
                "skipping blob for {:?} — its space no longer exists",
                fc.name
            ));
            continue;
        };
        let target = space.files_dir(&space_name).join(&fc.name);
        if file_matches(&target, &fc.hash) {
            summary.files_kept += 1;
            continue;
        }
        let pulled = match blob_source {
            Some(src) => match pull_blob(src, &fc.space_id, &fc.name, &fc.hash, &target) {
                Ok(true) => {
                    summary.files_pulled += 1;
                    true
                }
                _ => false,
            },
            None => false,
        };
        if !pulled {
            summary.files_missing.push(fc.clone());
            summary.warnings.push(format!(
                "blob for {:?} unavailable — fetch it with a dir or ssh transport",
                fc.name
            ));
        }
    }

    // The embedded ack: entries addressed to this device advance the
    // sender's push cursors — the only thing that does.
    if let Some(acks) = &cs.ack {
        let states = db.load_sync_state()?;
        for a in acks {
            if a.peer_id != my_id {
                continue;
            }
            if !known_table(&a.table_name) {
                summary
                    .warnings
                    .push(format!("ack for unknown table {:?}", a.table_name));
                continue;
            }
            let existing = states
                .iter()
                .find(|s| s.peer_id == cs.device_id && s.table_name == a.table_name)
                .and_then(|s| s.push_cursor.clone());
            if existing
                .as_deref()
                .is_none_or(|e| position_gt(&a.table_name, &a.cursor, e))
            {
                db.set_sync_state(&cs.device_id, &a.table_name, None, Some(&a.cursor))?;
                summary.acks_applied += 1;
            }
        }
    }

    // Advance pull cursors (monotonically) and build the reply cursors.
    let states = db.load_sync_state()?;
    let mut reply: Vec<PeerCursor> = Vec::new();
    for (table, pos) in &max_pos {
        let existing = states
            .iter()
            .find(|s| s.peer_id == cs.device_id && s.table_name == *table)
            .and_then(|s| s.pull_cursor.clone());
        let final_pos = match &existing {
            Some(e) if position_gt(table, e, pos) => e.clone(),
            _ => pos.clone(),
        };
        if existing.as_deref() != Some(final_pos.as_str()) {
            db.set_sync_state(&cs.device_id, table, Some(&final_pos), None)?;
            reply.push(PeerCursor {
                peer_id: cs.device_id.clone(),
                table_name: (*table).to_string(),
                cursor: final_pos,
            });
        }
    }
    Ok((summary, reply))
}

fn apply_row(
    db: &Db,
    space: &Space,
    spec: &TableSpec,
    row: &serde_json::Value,
) -> Result<LwwOutcome> {
    match spec.cursor {
        Cursor::UpdatedAt if spec.name == "spaces" => apply_space(db, space, row),
        Cursor::UpdatedAt if spec.name == "files" => apply_file(db, row),
        Cursor::UpdatedAt => apply_lww(db, spec, row),
        Cursor::Tuple(_) | Cursor::AutoId => Ok(if apply_append(db, spec, row)? {
            LwwOutcome::Applied
        } else {
            LwwOutcome::Skipped
        }),
        Cursor::None => Ok(LwwOutcome::Skipped),
    }
}

/// LWW upsert for a plain row (not spaces/files — those have their own
/// name-merge rules). A `scope != 'sync'` `app_settings` row never lands.
fn apply_lww(db: &Db, spec: &TableSpec, row: &serde_json::Value) -> Result<LwwOutcome> {
    if spec.name == "app_settings"
        && row.get("scope").and_then(serde_json::Value::as_str) != Some("sync")
    {
        return Ok(LwwOutcome::Skipped);
    }
    let conn = db.conn();
    let pk_values: Vec<String> = spec
        .pk
        .iter()
        .map(|c| {
            row.get(*c)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let where_sql = spec
        .pk
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c} = ?{}", i + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let existing: Option<String> = conn
        .query_row(
            &format!(
                "SELECT COALESCE(updated_at, '') FROM {} WHERE {where_sql}",
                spec.name
            ),
            params_from_iter(pk_values.iter()),
            |r| r.get(0),
        )
        .optional()?;
    let local_tie = pk_values.join("\u{1f}");
    if !lww_wins(spec, row, existing.as_deref().unwrap_or(""), &local_tie) {
        return Ok(LwwOutcome::Skipped);
    }
    let values: Vec<rusqlite::types::Value> = spec
        .apply_columns
        .iter()
        .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
        .collect();
    if existing.is_some() {
        let set_sql = spec
            .apply_columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        // The WHERE placeholders follow the SET's (the SELECT above used
        // its own 1-based clause).
        let update_where = spec
            .pk
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c} = ?{}", spec.apply_columns.len() + i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut all = values;
        all.extend(
            pk_values
                .iter()
                .map(|v| rusqlite::types::Value::Text(v.clone())),
        );
        conn.execute(
            &format!("UPDATE {} SET {set_sql} WHERE {update_where}", spec.name),
            params_from_iter(all.iter()),
        )?;
    } else {
        let cols = spec.apply_columns.join(", ");
        let marks = (1..=spec.apply_columns.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(
            &format!("INSERT INTO {} ({cols}) VALUES ({marks})", spec.name),
            params_from_iter(values.iter()),
        )?;
    }
    Ok(LwwOutcome::Applied)
}

/// INSERT OR IGNORE by the row's sync identity — the append-only union
/// rule. `changes()` tells an ignored duplicate apart from a real insert.
fn apply_append(db: &Db, spec: &TableSpec, row: &serde_json::Value) -> Result<bool> {
    let cols = spec.apply_columns.join(", ");
    let marks = (1..=spec.apply_columns.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let values: Vec<rusqlite::types::Value> = spec
        .apply_columns
        .iter()
        .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
        .collect();
    let n = db.conn().execute(
        &format!(
            "INSERT OR IGNORE INTO {} ({cols}) VALUES ({marks})",
            spec.name
        ),
        params_from_iter(values.iter()),
    )?;
    Ok(n > 0)
}

/// A space row's apply: LWW like the others, plus the name-merge rules —
/// spaces are UNIQUE by name, and two devices may have independently
/// created different spaces with the same name. The LWW loser is renamed
/// to `<name>-<first8(loser_id)>` on both sides (the rename is a pure
/// function of the loser, so both devices compute the same name). Fresh
/// winners get their directory; renames move it.
fn apply_space(db: &Db, space: &Space, row: &serde_json::Value) -> Result<LwwOutcome> {
    let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(LwwOutcome::Warned("space row without id".to_string()));
    };
    let Some(name) = row.get("name").and_then(serde_json::Value::as_str) else {
        return Ok(LwwOutcome::Warned(format!("space {id} without name")));
    };
    if !valid_component(name) {
        return Ok(LwwOutcome::Warned(format!(
            "skipping space {id}: unsafe name {name:?}"
        )));
    }
    let conn = db.conn();
    let incoming_updated = row
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let local: Option<(String, String)> = conn
        .query_row(
            "SELECT name, COALESCE(updated_at, '') FROM spaces WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((local_name, local_updated)) = local else {
        // New space. Its name may be taken by a different local space —
        // the loser of that pair is renamed deterministically.
        let colliding: Option<String> = conn
            .query_row(
                "SELECT id FROM spaces WHERE name = ?1 AND id != ?2",
                (name, id),
                |r| r.get(0),
            )
            .optional()?;
        let mut incoming_name = name.to_string();
        if let Some(other) = colliding {
            let other_updated: String = conn.query_row(
                "SELECT COALESCE(updated_at, '') FROM spaces WHERE id = ?1",
                [&other],
                |r| r.get(0),
            )?;
            if lww_wins_against(id, incoming_updated, &other, &other_updated) {
                let new_name = format!("{name}-{}", short_id(&other));
                conn.execute(
                    "UPDATE spaces SET name = ?1 WHERE id = ?2",
                    (new_name.as_str(), other.as_str()),
                )?;
                rename_dir(space, name, &new_name);
            } else {
                incoming_name = format!("{name}-{}", short_id(id));
            }
        }
        insert_space_row(conn, id, &incoming_name, row)?;
        if let Err(e) = space.ensure_space_dir(&incoming_name) {
            return Ok(LwwOutcome::Warned(format!(
                "space {id} applied but its dir failed: {e}"
            )));
        }
        return Ok(LwwOutcome::Applied);
    };
    if !lww_wins_against(id, incoming_updated, id, &local_updated) {
        return Ok(LwwOutcome::Skipped);
    }
    if name != local_name {
        rename_dir(space, &local_name, name);
    }
    insert_space_row(conn, id, name, row)?;
    Ok(LwwOutcome::Applied)
}

fn insert_space_row(
    conn: &Connection,
    id: &str,
    name: &str,
    row: &serde_json::Value,
) -> Result<()> {
    conn.execute(
        "INSERT INTO spaces (id, name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET name = ?2, created_at = ?3, updated_at = ?4",
        (
            id,
            name,
            row.get("created_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            row.get("updated_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        ),
    )?;
    Ok(())
}

/// Move a space's directory on a rename — best-effort: a missing old dir
/// is fine, a failing rename surfaces in the caller's warning path.
fn rename_dir(space: &Space, old: &str, new: &str) {
    let from = space.space_dir(old);
    let to = space.space_dir(new);
    if from.exists() && !to.exists() {
        let _ = std::fs::rename(&from, &to);
    }
}

/// A file row's apply: LWW, plus the (`space_id`, name) uniqueness merge —
/// the app keeps one row per space+name, so a synced row colliding with a
/// different id is the LWW opponent; the loser is replaced wholesale (id
/// and all) so both devices end on the same row id and deletes propagate.
fn apply_file(db: &Db, row: &serde_json::Value) -> Result<LwwOutcome> {
    let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(LwwOutcome::Warned("file row without id".to_string()));
    };
    let (Some(space_id), Some(name)) = (
        row.get("space_id").and_then(serde_json::Value::as_str),
        row.get("name").and_then(serde_json::Value::as_str),
    ) else {
        return Ok(LwwOutcome::Warned(format!("file {id} without space/name")));
    };
    if !valid_component(name) || !valid_component(space_id) {
        return Ok(LwwOutcome::Warned(format!(
            "skipping file {id}: unsafe name {name:?}"
        )));
    }
    let conn = db.conn();
    let incoming_updated = row
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    // The opponent: the row with this id, or the row owning this
    // space+name (the app's uniqueness rule).
    let local: Option<(String, String)> = conn
        .query_row(
            "SELECT id, COALESCE(updated_at, '') FROM files WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .or(conn
            .query_row(
                "SELECT id, COALESCE(updated_at, '') FROM files
                 WHERE space_id = ?1 AND name = ?2",
                (space_id, name),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?);
    let Some((local_id, local_updated)) = local else {
        insert_file_row(conn, row)?;
        return Ok(LwwOutcome::Applied);
    };
    if !lww_wins_against(id, incoming_updated, &local_id, &local_updated) {
        return Ok(LwwOutcome::Skipped);
    }
    // Incoming wins — replace the local row wholesale, including its id:
    // the winner's id becomes the row's identity on both devices.
    let values: Vec<rusqlite::types::Value> = [
        "id",
        "space_id",
        "name",
        "hash",
        "size",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
    .collect();
    let mut all = values;
    all.push(rusqlite::types::Value::Text(local_id));
    conn.execute(
        "UPDATE files SET id = ?1, space_id = ?2, name = ?3, hash = ?4, size = ?5,
            created_at = ?6, updated_at = ?7
         WHERE id = ?8",
        params_from_iter(all.iter()),
    )?;
    Ok(LwwOutcome::Applied)
}

fn insert_file_row(conn: &Connection, row: &serde_json::Value) -> Result<()> {
    let values: Vec<rusqlite::types::Value> = [
        "id",
        "space_id",
        "name",
        "hash",
        "size",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|c| json_to_value(row.get(*c).unwrap_or(&serde_json::Value::Null)))
    .collect();
    conn.execute(
        "INSERT INTO files (id, space_id, name, hash, size, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params_from_iter(values.iter()),
    )?;
    Ok(())
}

/// Apply one tombstone; returns whether it hit anything. The incoming
/// tombstone is also recorded locally (deduped by `(table_name, row_id)`)
/// — that is what makes a delete transitive across a mesh of devices
/// without every pair syncing directly. The ack prevents resends.
// Long by design: one arm per tombstonable table.
#[allow(clippy::too_many_lines)]
fn apply_tombstone(db: &Db, space: &Space, t: &Tombstone, summary: &mut ApplySummary) -> bool {
    let conn = db.conn();
    // Dedupe: the first tombstone for a (table, row) wins; later copies
    // are the same delete.
    let _ = conn.execute(
        "INSERT INTO sync_tombstones (table_name, row_id, deleted_at)
         SELECT ?1, ?2, ?3 WHERE NOT EXISTS (
             SELECT 1 FROM sync_tombstones WHERE table_name = ?1 AND row_id = ?2)",
        (&t.table_name, &t.row_id, &t.deleted_at),
    );
    match t.table_name.as_str() {
        "spaces" => {
            if t.row_id == DEFAULT_SPACE {
                summary
                    .warnings
                    .push("tombstone for the default space ignored".to_string());
                return false;
            }
            let name: Option<String> = conn
                .query_row("SELECT name FROM spaces WHERE id = ?1", [&t.row_id], |r| {
                    r.get(0)
                })
                .optional()
                .ok()
                .flatten();
            if conn
                .execute("DELETE FROM spaces WHERE id = ?1", [&t.row_id])
                .is_err()
            {
                return false;
            }
            if let Some(name) = name
                && let Err(e) = space.remove_space_dir(&name)
            {
                summary
                    .warnings
                    .push(format!("removing space dir {name}: {e}"));
            }
            true
        }
        "sessions" => {
            let cascade = conn.execute("DELETE FROM messages WHERE session_id = ?1", [&t.row_id]);
            let sources = conn.execute(
                "DELETE FROM session_sources WHERE session_id = ?1",
                [&t.row_id],
            );
            let row = conn.execute("DELETE FROM sessions WHERE id = ?1", [&t.row_id]);
            cascade.is_ok() && sources.is_ok() && row.is_ok()
        }
        "messages" => conn
            .execute("DELETE FROM messages WHERE id = ?1", [&t.row_id])
            .is_ok(),
        "files" => {
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT space_id, name FROM files WHERE id = ?1",
                    [&t.row_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .ok()
                .flatten();
            let _ = conn.execute(
                "DELETE FROM cache.file_chunks WHERE file_id = ?1",
                [&t.row_id],
            );
            let _ = conn.execute(
                "DELETE FROM cache.chunk_embeddings WHERE file_id = ?1",
                [&t.row_id],
            );
            let _ = conn.execute(
                "DELETE FROM cache.file_index_state WHERE file_id = ?1",
                [&t.row_id],
            );
            if conn
                .execute("DELETE FROM files WHERE id = ?1", [&t.row_id])
                .is_err()
            {
                return false;
            }
            if let Some((space_id, name)) = row
                && let Ok(Some(space_name)) = space_name_for(db, &space_id)
                && valid_component(&name)
            {
                let blob = space.files_dir(&space_name).join(&name);
                let _ = std::fs::remove_file(blob);
            }
            true
        }
        "watches" => conn
            .execute("DELETE FROM watches WHERE id = ?1", [&t.row_id])
            .is_ok(),
        "usage_log" => conn
            .execute("DELETE FROM usage_log WHERE sync_id = ?1", [&t.row_id])
            .is_ok(),
        "citations" => conn
            .execute("DELETE FROM citations WHERE sync_id = ?1", [&t.row_id])
            .is_ok(),
        // Persona tombstones are subsumed by the roster replace that
        // accompanies a winning session row — applying them alone could
        // delete slots of a roster the session's LWW winner restored.
        "swarm_personas" => false,
        other => {
            summary
                .warnings
                .push(format!("tombstone for unknown table {other:?}"));
            false
        }
    }
}

/// A space row's name for a space id, when the space still exists.
fn space_name_for(db: &Db, space_id: &str) -> Result<Option<String>> {
    Ok(db
        .conn()
        .query_row("SELECT name FROM spaces WHERE id = ?1", [space_id], |r| {
            r.get(0)
        })
        .optional()?)
}

// ── file blobs ──

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().fold(String::new(), |mut h, b| {
        let _ = std::fmt::Write::write_fmt(&mut h, format_args!("{b:02x}"));
        h
    })
}

/// Store one HTTP/transport blob after checking that its manifest row exists,
/// the declared size matches, and the content hash is exact. Metadata always
/// wins first; an upload can never create a new file row or escape its space.
pub fn put_blob(
    db: &Db,
    space: &Space,
    space_id: &str,
    name: &str,
    hash: &str,
    bytes: &[u8],
) -> Result<()> {
    if !valid_component(space_id) || !valid_component(name) {
        bail!("unsafe blob path");
    }
    let Some(space_name) = space_name_for(db, space_id)? else {
        bail!("unknown space");
    };
    let row = db
        .list_files(space_id)?
        .into_iter()
        .find(|file| file.name == name)
        .ok_or_else(|| anyhow!("unknown file manifest"))?;
    if row.hash != hash {
        bail!("blob hash does not match the current file manifest");
    }
    if row.size < 0 || row.size as usize != bytes.len() {
        bail!("blob size does not match the current file manifest");
    }
    if sha256_hex(bytes) != hash {
        bail!("blob content hash mismatch");
    }
    let target = space.files_dir(&space_name).join(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let temporary = target.with_extension(format!("nexus-upload-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, bytes)
        .with_context(|| format!("writing blob {}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("installing blob {}", target.display()));
    }
    Ok(())
}

/// Read a manifest-backed blob for an HTTP download. A missing or stale local
/// file is `None`, never an unverified byte stream.
pub fn read_blob(db: &Db, space: &Space, space_id: &str, name: &str) -> Result<Option<Vec<u8>>> {
    if !valid_component(space_id) || !valid_component(name) {
        return Ok(None);
    }
    let Some(space_name) = space_name_for(db, space_id)? else {
        return Ok(None);
    };
    let Some(row) = db
        .list_files(space_id)?
        .into_iter()
        .find(|file| file.name == name)
    else {
        return Ok(None);
    };
    let path = space.files_dir(&space_name).join(name);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(
        (row.size >= 0 && row.size as usize == bytes.len() && sha256_hex(&bytes) == row.hash)
            .then_some(bytes),
    )
}

/// Whether the file at `path` exists and its sha256 matches `hash`.
fn file_matches(path: &Path, hash: &str) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    sha256_hex(&bytes) == hash
}

/// The local disk path of a manifest entry's blob, when the space still
/// exists and the name is a valid path component.
fn blob_source_path(db: &Db, space: &Space, fc: &FileChange) -> Result<Option<PathBuf>> {
    if !valid_component(&fc.name) || !valid_component(&fc.space_id) {
        return Ok(None);
    }
    let Some(space_name) = space_name_for(db, &fc.space_id)? else {
        return Ok(None);
    };
    let path = space.files_dir(&space_name).join(&fc.name);
    Ok(path.exists().then_some(path))
}

/// Copy the local blobs of a changeset's manifest into `dest/blobs/` —
/// the transport's blob channel. Never re-sends identical content: the
/// receiver hash-checks before pulling.
pub fn export_blobs(db: &Db, space: &Space, cs: &Changeset, dest: &Path) -> Result<usize> {
    let mut n = 0usize;
    for fc in &cs.files {
        let Some(src) = blob_source_path(db, space, fc)? else {
            continue;
        };
        let target = dest.join("blobs").join(&fc.space_id).join(&fc.name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(&src, &target)
            .with_context(|| format!("copying blob {} to {}", src.display(), target.display()))?;
        n += 1;
    }
    Ok(n)
}

/// Pull one blob from `src/blobs/<space_id>/<name>` into `target`, after
/// verifying the source's content hash matches the manifest (a stale or
/// mismatched payload is never applied).
fn pull_blob(src: &Path, space_id: &str, name: &str, hash: &str, target: &Path) -> Result<bool> {
    let candidate = src.join("blobs").join(space_id).join(name);
    if !file_matches(&candidate, hash) {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::copy(&candidate, target)
        .with_context(|| format!("copying blob to {}", target.display()))?;
    Ok(true)
}

// ── zip bundles (the ssh transport's blob channel) ──

/// Write a changeset plus its blobs as a zip: `changeset.json` +
/// `blobs/<space_id>/<name>`. Returns how many blobs were included.
pub fn write_bundle(
    db: &Db,
    space: &Space,
    cs: &Changeset,
    writer: impl std::io::Write + std::io::Seek,
) -> Result<usize> {
    let mut zip = zip::ZipWriter::new(writer);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("changeset.json", opts)?;
    serde_json::to_writer(&mut zip, cs)?;
    let mut n = 0usize;
    for fc in &cs.files {
        let Some(src) = blob_source_path(db, space, fc)? else {
            continue;
        };
        zip.start_file(format!("blobs/{}/{}", fc.space_id, fc.name), opts)?;
        let mut f = std::fs::File::open(&src)?;
        std::io::copy(&mut f, &mut zip)?;
        n += 1;
    }
    zip.finish()?;
    Ok(n)
}

/// Unpack a bundle written by `write_bundle` into `dest_dir` (as
/// `blobs/…`), returning the changeset. Entry paths are validated — a
/// bundle must never write outside `dest_dir`.
pub fn unpack_bundle(src: &Path, dest_dir: &Path) -> Result<Changeset> {
    let file = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("reading {}", src.display()))?;
    let mut changeset: Option<Changeset> = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name == "changeset.json" {
            changeset = Some(serde_json::from_reader(&mut entry)?);
            continue;
        }
        let Some(rel) = name.strip_prefix("blobs/") else {
            continue;
        };
        let components: Vec<&str> = rel.split('/').collect();
        if components.len() != 2 || !components.iter().all(|c| valid_component(c)) {
            bail!("unsafe path in bundle: {name}");
        }
        let dest = dest_dir.join("blobs").join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut out =
            std::fs::File::create(&dest).with_context(|| format!("creating {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    changeset.ok_or_else(|| anyhow!("bundle has no changeset.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::io::Write as _;

    /// A fresh device pair: in-memory dbs (with their attached in-memory
    /// caches) plus temp roots standing in for the spaces layout.
    struct Pair {
        a: Db,
        b: Db,
        a_root: PathBuf,
        b_root: PathBuf,
        dir: PathBuf,
    }

    impl Pair {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("nexus-sync-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                a: Db::open_in_memory().unwrap(),
                b: Db::open_in_memory().unwrap(),
                a_root: dir.join("a"),
                b_root: dir.join("b"),
                dir,
            }
        }

        fn space_a(&self) -> Space {
            Space {
                root: self.a_root.clone(),
            }
        }

        fn space_b(&self) -> Space {
            Space {
                root: self.b_root.clone(),
            }
        }

        /// One full A→B exchange: A exports (+ blobs into the channel), B
        /// applies (pulling blobs), B replies with its own export + blobs
        /// + ack, A applies.
        fn exchange_ab(&self) -> (ApplySummary, ApplySummary) {
            let a_space = self.space_a();
            let b_space = self.space_b();
            let cs = build_changeset(&self.a, None, "a").unwrap();
            export_blobs(&self.a, &a_space, &cs, &self.dir).unwrap();
            let (sa, cursors) = apply_changeset(&self.b, &b_space, &cs, Some(&self.dir)).unwrap();
            let mut reply = build_changeset(&self.b, Some(&cs.device_id), "b").unwrap();
            reply.ack = Some(cursors);
            export_blobs(&self.b, &b_space, &reply, &self.dir).unwrap();
            let (sb, _) = apply_changeset(&self.a, &a_space, &reply, Some(&self.dir)).unwrap();
            (sa, sb)
        }

        /// The mirror image: B→A.
        fn exchange_ba(&self) -> (ApplySummary, ApplySummary) {
            let a_space = self.space_a();
            let b_space = self.space_b();
            let cs = build_changeset(&self.b, None, "b").unwrap();
            export_blobs(&self.b, &b_space, &cs, &self.dir).unwrap();
            let (sb, cursors) = apply_changeset(&self.a, &a_space, &cs, Some(&self.dir)).unwrap();
            let mut reply = build_changeset(&self.a, Some(&cs.device_id), "a").unwrap();
            reply.ack = Some(cursors);
            export_blobs(&self.a, &a_space, &reply, &self.dir).unwrap();
            let (sa, _) = apply_changeset(&self.b, &b_space, &reply, Some(&self.dir)).unwrap();
            (sb, sa)
        }

        /// Both directions.
        fn exchange(&self) -> (ApplySummary, ApplySummary) {
            let (_, _) = self.exchange_ab();
            self.exchange_ba()
        }
    }

    impl Drop for Pair {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Every syncable row on a device, as (table, pk, json) — the
    /// convergence oracle: two devices are converged iff their dumps are
    /// equal (tombstones included). The default space is excluded: its
    /// `updated_at` is NULL so it never exports, and its `created_at` is
    /// device-local (cosmetic).
    fn dump(db: &Db) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for spec in TABLES {
            if spec.cursor == Cursor::None {
                continue;
            }
            let rows = select_rows(db, spec, None).unwrap();
            for row in rows {
                let pk = spec
                    .pk
                    .iter()
                    .map(|c| {
                        row.get(*c)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(":");
                out.push((spec.name.to_string(), pk, row.to_string()));
            }
        }
        let mut stmt = db
            .conn()
            .prepare("SELECT table_name, row_id, deleted_at FROM sync_tombstones ORDER BY row_id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for (t, rid, _) in rows {
            out.push(("tombstone".to_string(), format!("{t}:{rid}"), String::new()));
        }
        out.sort();
        out
    }

    fn space(db: &Db, name: &str) -> String {
        db.list_spaces()
            .unwrap()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap()
            .id
    }

    fn session(db: &Db, title: &str) -> String {
        let sid = space(db, "default");
        let s = db.create_session(title, "m", &sid, "chat").unwrap();
        s.id
    }

    fn set_updated(db: &Db, table: &str, id: &str, at: &str) {
        db.conn()
            .execute(
                &format!("UPDATE {table} SET updated_at = ?1 WHERE id = ?2"),
                (at, id),
            )
            .unwrap();
    }

    fn session_updated(db: &Db, id: &str) -> String {
        db.conn()
            .query_row("SELECT updated_at FROM sessions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn minus_one_second(rfc3339: &str) -> String {
        let dt = chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap();
        (dt - chrono::Duration::seconds(1)).to_rfc3339()
    }

    fn write_blob(space: &Space, space_name: &str, name: &str, bytes: &[u8]) {
        let dir = space.files_dir(space_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn add_session_sources(db: &Db, session_id: &str, url_norms: &[String]) {
        crate::db::add_session_sources(db.conn(), session_id, url_norms).unwrap();
    }

    fn db_state(db: &Db) -> Vec<crate::db::SyncState> {
        db.load_sync_state().unwrap()
    }

    // ── types / registry ──

    #[test]
    fn changeset_serde_roundtrip() {
        let cs = Changeset {
            device_id: "dev-1".to_string(),
            device_name: "laptop".to_string(),
            ack: Some(vec![PeerCursor {
                peer_id: "dev-2".to_string(),
                table_name: "sessions".to_string(),
                cursor: "2026-01-01T00:00:00Z|id".to_string(),
            }]),
            rows: vec![RowChange {
                table: "sessions".to_string(),
                row: serde_json::json!({"id": "s1", "title": "t"}),
            }],
            tombstones: vec![Tombstone {
                origin_id: 3,
                table_name: "sessions".to_string(),
                row_id: "s1".to_string(),
                deleted_at: "2026-01-02T00:00:00Z".to_string(),
            }],
            files: vec![FileChange {
                space_id: "sp".to_string(),
                name: "f.txt".to_string(),
                hash: "abc".to_string(),
                size: 3,
            }],
            generated_at: "2026-01-03T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&cs).unwrap();
        let back: Changeset = serde_json::from_str(&json).unwrap();
        assert_eq!(cs, back);
    }

    #[test]
    fn cursor_positions_compare_table_aware() {
        // AUTOINCREMENT cursors are numeric: "10" > "9" as ids.
        assert!(position_gt("citations", "10", "9"));
        assert!(position_gt("sync_tombstones", "10", "9"));
        // RFC3339 cursors compare lexically.
        assert!(position_gt(
            "sessions",
            "2026-02-01T00:00:00Z",
            "2026-01-01T00:00:00Z"
        ));
        assert!(!position_gt(
            "sessions",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z"
        ));
    }

    #[test]
    fn unsafe_components_are_rejected() {
        assert!(valid_component("notes.txt"));
        assert!(!valid_component(""));
        assert!(!valid_component("."));
        assert!(!valid_component(".."));
        assert!(!valid_component("a/b"));
        assert!(!valid_component("a\\b"));
        assert!(!valid_component("a\0b"));
    }

    // ── export / cursor rules ──

    #[test]
    fn cold_start_full_export_covers_every_table() {
        let pair = Pair::new();
        let a = &pair.a;
        let s = session(a, "hello");
        a.add_user_message(&s, "hi").unwrap();
        a.log_usage(
            "openrouter",
            "m",
            1,
            2,
            0,
            0,
            Some(0.1),
            true,
            Some(&s),
            None,
        )
        .unwrap();
        a.add_citations(&space(a, "default"), "r.md", &[("https://x".into(), None)])
            .unwrap();
        add_session_sources(a, &s, &["https://x".to_string()]);
        a.create_watch(&space(a, "default"), "topic", 24, &s)
            .unwrap();
        a.set_reasoning("m", Some("high")).unwrap();
        a.set_setting("theme", "dark").unwrap();
        // A real space (with a version) — the default space has no
        // `updated_at`, so it never exports; its id/name are deterministic
        // on every device instead.
        a.create_space("work").unwrap();
        let persona = crate::db::Persona {
            name: "p".to_string(),
            model: "m".to_string(),
            blurb: "b".to_string(),
        };
        a.save_swarm_personas(&s, &[persona]).unwrap();
        a.upsert_file(&space(a, "default"), "f.txt", "deadbeef", 3, "ok")
            .unwrap();

        let cs = build_changeset(a, None, "a").unwrap();
        let tables: HashSet<&str> = cs.rows.iter().map(|r| r.table.as_str()).collect();
        for expected in [
            "sessions",
            "swarm_personas",
            "model_prefs",
            "spaces",
            "files",
            "watches",
            "app_settings",
            "session_sources",
            "messages",
            "usage_log",
            "citations",
        ] {
            assert!(tables.contains(expected), "missing {expected}");
        }
        // The file manifest matches the exported file row.
        assert_eq!(cs.files.len(), 1);
        assert_eq!(cs.files[0].space_id, space(a, "default"));
        assert_eq!(cs.files[0].name, "f.txt");
        assert_eq!(cs.files[0].hash, "deadbeef");
        assert_eq!(cs.files[0].size, 3);
        // The persona rides attached to its session.
        let personas: Vec<_> = cs
            .rows
            .iter()
            .filter(|r| r.table == "swarm_personas")
            .collect();
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].row["session_id"], s);
        assert_eq!(personas[0].row["name"], "p");
        assert!(cs.ack.is_none());
        // Nothing local-scope leaks out.
        assert!(
            !cs.rows
                .iter()
                .any(|r| r.table == "app_settings" && r.row["scope"] == "local")
        );
    }

    #[test]
    fn export_resumes_past_acked_cursor_only() {
        let pair = Pair::new();
        let a = &pair.a;
        let s1 = session(a, "one");
        let cs1 = build_changeset(a, None, "a").unwrap();
        assert!(!cs1.rows.is_empty());

        // No ack yet: the same rows re-export (the idempotent backstop).
        let cs_again = build_changeset(a, Some("peer-x"), "a").unwrap();
        assert_eq!(cs_again.rows.len(), cs1.rows.len());

        // A "peer-x" ack of the current position silences the resends.
        let sess_pos = cs1
            .rows
            .iter()
            .find(|r| r.table == "sessions" && r.row["id"] == s1)
            .map(|r| r.row["updated_at"].as_str().unwrap().to_string())
            .unwrap();
        a.set_sync_state("peer-x", "sessions", None, Some(&sess_pos))
            .unwrap();
        let cs2 = build_changeset(a, Some("peer-x"), "a").unwrap();
        assert!(!cs2.rows.iter().any(|r| r.table == "sessions"));
        // Newer rows still flow.
        let _s2 = session(a, "two");
        let cs3 = build_changeset(a, Some("peer-x"), "a").unwrap();
        assert!(
            cs3.rows
                .iter()
                .any(|r| r.table == "sessions" && r.row["title"] == "two")
        );
        let _ = s1;
    }

    // ── LWW ──

    #[test]
    fn lww_newer_wins_older_loses() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sa = session(a, "from a");
        let sb = session(b, "from b");
        // Same logical session on both devices — same id, so the merge is
        // a same-row LWW; A's version is newer.
        b.conn()
            .execute("UPDATE sessions SET id = ?1 WHERE id = ?2", (&sa, &sb))
            .unwrap();
        set_session_updated(a, &sa, "2026-02-01T00:00:00Z");
        set_session_updated(b, &sa, "2026-01-01T00:00:00Z");
        pair.exchange_ab();
        let title = |db: &Db| db.get_session(&sa).unwrap().unwrap().title.clone();
        assert_eq!(title(a), "from a");
        assert_eq!(title(b), "from a");
        // The older row arriving later changes nothing.
        let cs = build_changeset(b, None, "b").unwrap();
        let _ = apply_changeset(&pair.b, &pair.space_b(), &cs, None).unwrap();
        assert_eq!(title(a), "from a");
    }

    fn set_session_updated(db: &Db, id: &str, at: &str) {
        set_updated(db, "sessions", id, at);
    }

    #[test]
    fn equal_timestamp_keeps_local_and_is_stable() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sa = session(a, "a's title");
        let sb = session(b, "b's title");
        let sid = sa.clone();
        b.conn()
            .execute("UPDATE sessions SET id = ?1 WHERE id = ?2", (&sid, &sb))
            .unwrap();
        // Same row, same nanosecond, different content — the clock-skew
        // residual. Both sides keep their own copy, and re-imports never
        // flip-flop.
        set_session_updated(a, &sid, "2026-01-01T00:00:00Z");
        set_session_updated(b, &sid, "2026-01-01T00:00:00Z");
        let title_a = a.get_session(&sid).unwrap().unwrap().title.clone();
        let title_b = b.get_session(&sid).unwrap().unwrap().title.clone();
        pair.exchange();
        assert_eq!(a.get_session(&sid).unwrap().unwrap().title, title_a);
        assert_eq!(b.get_session(&sid).unwrap().unwrap().title, title_b);
        pair.exchange();
        assert_eq!(a.get_session(&sid).unwrap().unwrap().title, title_a);
        assert_eq!(b.get_session(&sid).unwrap().unwrap().title, title_b);
    }

    #[test]
    fn clock_skew_loser_converges() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sa = session(a, "fast clock");
        let sb = session(b, "slow clock");
        let sid = sa.clone();
        b.conn()
            .execute("UPDATE sessions SET id = ?1 WHERE id = ?2", (&sid, &sb))
            .unwrap();
        // B's clock is behind: its version of the row is the loser.
        set_session_updated(a, &sid, "2026-02-01T00:00:00Z");
        set_session_updated(b, &sid, "2026-01-01T00:00:00Z");
        pair.exchange();
        assert_eq!(a.get_session(&sid).unwrap().unwrap().title, "fast clock");
        assert_eq!(b.get_session(&sid).unwrap().unwrap().title, "fast clock");
    }

    #[test]
    fn scope_local_setting_never_syncs_or_applies() {
        let pair = Pair::new();
        let a = &pair.a;
        a.set_setting("searxng_url", "http://localhost:8888")
            .unwrap();
        a.set_setting("theme", "dark").unwrap();
        let cs = build_changeset(a, None, "a").unwrap();
        let keys: Vec<&str> = cs
            .rows
            .iter()
            .filter(|r| r.table == "app_settings")
            .map(|r| r.row["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, vec!["theme"]);
        // A smuggled local-scope row is refused on apply.
        let mut evil = cs.clone();
        evil.rows.push(RowChange {
            table: "app_settings".to_string(),
            row: serde_json::json!({
                "key": "searxng_url", "value": "http://evil", "scope": "local",
                "updated_at": "2099-01-01T00:00:00Z",
            }),
        });
        let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &evil, None).unwrap();
        assert!(summary.rows_skipped >= 1);
        let settings = pair.b.load_settings().unwrap();
        assert!(!settings.iter().any(|(k, _)| k == "searxng_url"));
    }

    // ── append-only union + dedupe ──

    #[test]
    fn messages_union_dedupes_by_id() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sa = session(a, "shared");
        // B gets the same session via sync.
        let cs = build_changeset(a, None, "a").unwrap();
        let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        a.add_user_message(&sa, "from a").unwrap();
        let sb = b.get_session(&sa).unwrap().unwrap().id;
        b.add_user_message(&sb, "from b").unwrap();
        pair.exchange();
        let count = |db: &Db| db.load_messages(&sa).unwrap().len();
        assert_eq!(count(a), 2);
        assert_eq!(count(b), 2);
        // Re-exchange: nothing new moves.
        let (sa2, _) = pair.exchange_ab();
        assert_eq!(count(a), 2);
        assert_eq!(count(b), 2);
        assert_eq!(sa2.rows_applied, 0);
    }

    #[test]
    fn usage_and_citations_dedupe_by_sync_id() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let s = session(a, "shared");
        a.log_usage("openrouter", "m", 1, 2, 0, 0, None, false, Some(&s), None)
            .unwrap();
        let cs = build_changeset(a, None, "a").unwrap();
        let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        b.log_usage("openrouter", "m", 3, 4, 0, 0, None, false, Some(&s), None)
            .unwrap();
        a.add_citations(&space(a, "default"), "r.md", &[("https://a".into(), None)])
            .unwrap();
        let cs2 = build_changeset(a, None, "a").unwrap();
        let _ = apply_changeset(b, &pair.space_b(), &cs2, None).unwrap();
        b.add_citations(&space(b, "default"), "r2.md", &[("https://b".into(), None)])
            .unwrap();
        pair.exchange();
        let usage = |db: &Db| {
            db.conn()
                .query_row("SELECT COUNT(*) FROM usage_log", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let cites = |db: &Db| {
            db.conn()
                .query_row("SELECT COUNT(*) FROM citations", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(usage(a), 2);
        assert_eq!(usage(b), 2);
        assert_eq!(cites(a), 2);
        assert_eq!(cites(b), 2);
        // sync_ids are unique on both sides.
        for db in [a, b] {
            let dupes: i64 = db
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM (SELECT sync_id FROM usage_log UNION ALL \
                     SELECT sync_id FROM citations) GROUP BY sync_id HAVING COUNT(*) > 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            assert_eq!(dupes, 0);
        }
    }

    // ── convergence ──

    #[test]
    fn two_way_convergence_and_then_silence() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        // Independent work on both devices.
        let sa = session(a, "a's chat");
        a.add_user_message(&sa, "from a").unwrap();
        a.set_setting("theme", "dark").unwrap();
        a.set_reasoning("m1", Some("high")).unwrap();
        a.log_usage("openrouter", "m1", 1, 2, 0, 0, None, false, Some(&sa), None)
            .unwrap();
        let sb = session(b, "b's chat");
        b.add_user_message(&sb, "from b").unwrap();
        b.add_user_message(&sb, "and another").unwrap();
        b.create_watch(&space(b, "default"), "topic", 24, &sb)
            .unwrap();

        pair.exchange();
        assert_eq!(dump(a), dump(b), "devices must converge");

        // A second round with no new work moves nothing.
        let (sa2, sb2) = pair.exchange_ab();
        assert_eq!(sa2.rows_applied, 0);
        assert_eq!(sb2.rows_applied, 0);
        assert_eq!(dump(a), dump(b));
    }

    #[test]
    fn idempotent_reimport_is_a_noop() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let s = session(a, "hello");
        a.add_user_message(&s, "hi").unwrap();
        a.log_usage("openrouter", "m", 1, 2, 0, 0, None, false, Some(&s), None)
            .unwrap();
        let cs = build_changeset(a, None, "a").unwrap();
        let (first, _) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        assert!(first.rows_applied > 0);
        let (again, _) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        assert_eq!(again.rows_applied, 0);
        assert_eq!(again.tombstones_applied, 0);
        assert!(again.warnings.is_empty());
    }

    #[test]
    fn session_sources_lww_flag_propagates() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let s = session(a, "s");
        add_session_sources(a, &s, &["https://x".to_string()]);
        let cs = build_changeset(a, None, "a").unwrap();
        let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        a.set_source_flag(&s, "https://x", Some("pinned")).unwrap();
        pair.exchange_ab();
        let flags: Vec<(String, String)> = b
            .conn()
            .prepare("SELECT session_id, flag FROM session_sources")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(flags, vec![(s, "pinned".to_string())]);
    }

    // ── tombstones ──

    #[test]
    fn session_tombstone_cascades_messages_and_sources() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let s = session(a, "doomed");
        a.add_user_message(&s, "one").unwrap();
        add_session_sources(a, &s, &["https://x".to_string()]);
        pair.exchange_ab();
        assert_eq!(dump(a), dump(b));
        a.delete_session(&s).unwrap();
        pair.exchange_ab();
        assert_eq!(dump(a), dump(b));
        let sessions = b.list_sessions(&space(b, "default")).unwrap();
        assert!(sessions.iter().all(|s| s.title != "doomed"));
        let messages: i64 = b
            .conn()
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 0);
        let sources: i64 = b
            .conn()
            .query_row("SELECT COUNT(*) FROM session_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sources, 0);
    }

    #[test]
    fn space_tombstone_removes_row_and_dir() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sp = a.create_space("work").unwrap();
        pair.space_a().ensure_space_dir("work").unwrap();
        pair.space_b().ensure_space_dir("work").unwrap();
        write_blob(&pair.space_b(), "work", "f.txt", b"content");
        pair.exchange_ab();
        assert!(pair.space_b().files_dir("work").join("f.txt").exists());
        a.delete_space(&sp.id).unwrap();
        pair.space_a().remove_space_dir("work").unwrap();
        pair.exchange_ab();
        assert!(b.list_spaces().unwrap().iter().all(|s| s.name != "work"));
        assert!(!pair.space_b().space_dir("work").exists());
    }

    #[test]
    fn file_tombstone_removes_row_and_blob() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sid = space(a, "default");
        write_blob(&pair.space_a(), "default", "f.txt", b"content");
        let hash = sha256_hex(b"content");
        let fid = a.upsert_file(&sid, "f.txt", &hash, 7, "ok").unwrap();
        pair.exchange_ab();
        assert!(pair.space_b().files_dir("default").join("f.txt").exists());
        a.delete_file(&fid).unwrap();
        std::fs::remove_file(pair.space_a().files_dir("default").join("f.txt")).unwrap();
        pair.exchange_ab();
        assert_eq!(dump(a), dump(b));
        let files = b.list_files(&sid).unwrap();
        assert!(files.is_empty());
        assert!(!pair.space_b().files_dir("default").join("f.txt").exists());
    }

    #[test]
    fn default_space_tombstone_is_ignored() {
        let pair = Pair::new();
        let a = &pair.a;
        let mut evil = build_changeset(a, None, "a").unwrap();
        evil.tombstones = vec![Tombstone {
            origin_id: 1,
            table_name: "spaces".to_string(),
            row_id: DEFAULT_SPACE.to_string(),
            deleted_at: "2026-01-01T00:00:00Z".to_string(),
        }];
        let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &evil, None).unwrap();
        assert_eq!(summary.tombstones_applied, 0);
        assert!(pair.b.default_space_id().is_ok());
    }

    #[test]
    fn tombstone_cursor_advances_and_no_resends() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let s = session(a, "doomed");
        pair.exchange_ab();
        a.delete_session(&s).unwrap();
        pair.exchange_ab();
        // The tombstone is acked: nothing re-sends.
        let cs = build_changeset(a, Some(&b.device_id().unwrap()), "a").unwrap();
        assert!(cs.tombstones.is_empty());
        // And B's pull cursor for tombstones advanced — its ack included it.
        let state = db_state(b);
        assert!(state.iter().any(|st| {
            st.peer_id == a.device_id().unwrap()
                && st.table_name == "sync_tombstones"
                && st.pull_cursor.is_some()
        }));
    }

    // ── swarm personas ──

    #[test]
    fn swarm_roster_follows_winning_session() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let s = session(a, "roundtable");
        let p1 = crate::db::Persona {
            name: "p1".to_string(),
            model: "m".to_string(),
            blurb: "b".to_string(),
        };
        a.save_swarm_personas(&s, &[p1]).unwrap();
        // B gets everything (no acks yet — exports are full either way).
        let cs = build_changeset(a, None, "a").unwrap();
        let _ = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        // B's roster v2 is newer (t2).
        let p2 = crate::db::Persona {
            name: "p2".to_string(),
            model: "m".to_string(),
            blurb: "b".to_string(),
        };
        let p3 = crate::db::Persona {
            name: "p3".to_string(),
            model: "m".to_string(),
            blurb: "b".to_string(),
        };
        b.save_swarm_personas(&s, &[p2, p3]).unwrap();
        let t2 = session_updated(b, &s);
        // A's stale roster v1b sits between: newer than v1, older than v2.
        let p1b = crate::db::Persona {
            name: "p1".to_string(),
            model: "m".to_string(),
            blurb: "b".to_string(),
        };
        a.save_swarm_personas(&s, &[p1b]).unwrap();
        set_session_updated(a, &s, &minus_one_second(&t2));
        // Full exchange: B's v2 wins everywhere; A's stale roster (and
        // both sides' persona tombstones) can't clobber it.
        let cs_a = build_changeset(a, None, "a").unwrap();
        let (_, cursors) = apply_changeset(b, &pair.space_b(), &cs_a, None).unwrap();
        let mut reply = build_changeset(b, Some(&cs_a.device_id), "b").unwrap();
        reply.ack = Some(cursors);
        let _ = apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
        let names = |db: &Db| -> Vec<String> {
            db.list_swarm_personas(&s)
                .unwrap()
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        assert_eq!(names(a), vec!["p2", "p3"]);
        assert_eq!(names(b), vec!["p2", "p3"]);
    }

    // ── files / blobs ──

    #[test]
    fn file_blobs_transfer_keep_and_report_missing() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sid = space(a, "default");
        write_blob(&pair.space_a(), "default", "notes.txt", b"hello sync");
        let hash = sha256_hex(b"hello sync");
        a.upsert_file(&sid, "notes.txt", &hash, 11, "ok").unwrap();

        // Import with no blob channel: the row lands, the blob is reported
        // missing.
        let cs = build_changeset(a, None, "a").unwrap();
        let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &cs, None).unwrap();
        assert_eq!(summary.files_missing.len(), 1);
        assert_eq!(summary.files_missing[0].name, "notes.txt");
        assert!(
            !pair
                .space_b()
                .files_dir("default")
                .join("notes.txt")
                .exists()
        );

        // The same changeset re-imported with a channel: the row loses LWW
        // (idempotent), so nothing is pulled or reported.
        export_blobs(a, &pair.space_a(), &cs, &pair.dir).unwrap();
        let (summary2, _) =
            apply_changeset(&pair.b, &pair.space_b(), &cs, Some(&pair.dir)).unwrap();
        assert_eq!(summary2.files_pulled, 0);
        assert!(summary2.files_missing.is_empty());
        assert!(
            !pair
                .space_b()
                .files_dir("default")
                .join("notes.txt")
                .exists()
        );

        // A newer version of the file re-wins the row and pulls its blob,
        // hash-verified.
        write_blob(&pair.space_a(), "default", "notes.txt", b"hello sync v2");
        let hash2 = sha256_hex(b"hello sync v2");
        a.upsert_file(&sid, "notes.txt", &hash2, 13, "ok").unwrap();
        let cs2 = build_changeset(a, None, "a").unwrap();
        export_blobs(a, &pair.space_a(), &cs2, &pair.dir).unwrap();
        let (summary3, _) =
            apply_changeset(&pair.b, &pair.space_b(), &cs2, Some(&pair.dir)).unwrap();
        assert_eq!(summary3.files_pulled, 1);
        assert!(summary3.files_missing.is_empty());
        assert_eq!(
            std::fs::read(pair.space_b().files_dir("default").join("notes.txt")).unwrap(),
            b"hello sync v2"
        );
    }

    #[test]
    fn stale_blob_in_channel_is_never_applied() {
        let pair = Pair::new();
        let a = &pair.a;
        let sid = space(a, "default");
        write_blob(&pair.space_a(), "default", "f.txt", b"real");
        let hash = sha256_hex(b"real");
        a.upsert_file(&sid, "f.txt", &hash, 4, "ok").unwrap();
        let cs = build_changeset(a, None, "a").unwrap();
        // A stale payload with the right name but wrong content.
        let blob_dir = pair.dir.join("blobs").join(&sid);
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("f.txt"), b"stale").unwrap();
        let (summary, _) = apply_changeset(&pair.b, &pair.space_b(), &cs, Some(&pair.dir)).unwrap();
        assert_eq!(summary.files_pulled, 0);
        assert_eq!(summary.files_missing.len(), 1);
        assert!(!pair.space_b().files_dir("default").join("f.txt").exists());
    }

    #[test]
    fn unsafe_blob_names_cannot_escape() {
        let pair = Pair::new();
        let b = &pair.b;
        let mut cs = build_changeset(b, None, "b").unwrap();
        cs.rows.push(RowChange {
            table: "files".to_string(),
            row: serde_json::json!({
                "id": "f1", "space_id": "sp", "name": "../escape.txt",
                "hash": "x", "size": 1,
                "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
            }),
        });
        cs.files.push(FileChange {
            space_id: "sp".to_string(),
            name: "../escape.txt".to_string(),
            hash: "x".to_string(),
            size: 1,
        });
        let (summary, _) = apply_changeset(&pair.a, &pair.space_a(), &cs, Some(&pair.dir)).unwrap();
        assert!(summary.warnings.iter().any(|w| w.contains("unsafe")));
        assert!(!pair.dir.join("blobs").join("sp").join("..").exists());
        assert!(!pair.a_root.join("escape.txt").exists());
    }

    // ── spaces ──

    #[test]
    fn default_space_is_the_same_row_on_both_devices() {
        let pair = Pair::new();
        assert_eq!(pair.a.default_space_id().unwrap(), DEFAULT_SPACE);
        assert_eq!(pair.b.default_space_id().unwrap(), DEFAULT_SPACE);
        pair.exchange_ab();
        let spaces = pair.b.list_spaces().unwrap();
        assert_eq!(spaces.len(), 1, "no name collision for the default space");
    }

    #[test]
    fn space_name_collision_renames_loser_deterministically() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let wa = a.create_space("work").unwrap();
        let wb = b.create_space("work").unwrap();
        // Make the loser deterministic: B's space is older.
        set_updated(a, "spaces", &wa.id, "2026-02-01T00:00:00Z");
        set_updated(b, "spaces", &wb.id, "2026-01-01T00:00:00Z");
        pair.exchange();
        // Both devices end with the same two rows: the winner keeps "work",
        // the loser is renamed from its own id — identically on both sides.
        let names_a: Vec<String> = a
            .list_spaces()
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let names_b: Vec<String> = b
            .list_spaces()
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        let expected = format!("work-{}", short_id(&wb.id));
        assert!(names_a.contains(&"work".to_string()));
        assert!(names_a.contains(&expected));
        assert_eq!(names_a, names_b);
        assert_eq!(dump(a), dump(b));
        // A later sync doesn't churn the names.
        pair.exchange();
        let names_a2: Vec<String> = a
            .list_spaces()
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names_a2, names_a);
    }

    #[test]
    fn space_rename_moves_the_dir() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let sp = a.create_space("work").unwrap();
        pair.space_a().ensure_space_dir("work").unwrap();
        pair.space_b().ensure_space_dir("work").unwrap();
        write_blob(&pair.space_b(), "work", "f.txt", b"content");
        pair.exchange_ab();
        assert!(pair.space_b().files_dir("work").join("f.txt").exists());
        a.rename_space(&sp.id, "work-2").unwrap();
        pair.space_a().rename_space_dir("work", "work-2").unwrap();
        pair.exchange_ab();
        assert!(
            pair.b
                .list_spaces()
                .unwrap()
                .iter()
                .any(|s| s.name == "work-2")
        );
        assert!(pair.space_b().files_dir("work-2").join("f.txt").exists());
        assert!(!pair.space_b().space_dir("work").exists());
    }

    // ── bundles ──

    #[test]
    fn bundle_roundtrip_carries_changeset_and_blobs() {
        let pair = Pair::new();
        let a = &pair.a;
        let sid = space(a, "default");
        write_blob(&pair.space_a(), "default", "f.txt", b"payload");
        let hash = sha256_hex(b"payload");
        a.upsert_file(&sid, "f.txt", &hash, 7, "ok").unwrap();
        let cs = build_changeset(a, None, "a").unwrap();
        let dest = pair.dir.join("out.bundle");
        let blobs = write_bundle(
            a,
            &pair.space_a(),
            &cs,
            std::fs::File::create(&dest).unwrap(),
        )
        .unwrap();
        assert_eq!(blobs, 1);
        let unpacked = pair.dir.join("unpacked");
        let back = unpack_bundle(&dest, &unpacked).unwrap();
        assert_eq!(back.device_id, cs.device_id);
        assert_eq!(back.rows.len(), cs.rows.len());
        assert_eq!(
            std::fs::read(unpacked.join("blobs").join(&sid).join("f.txt")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn bundle_rejects_escaping_paths() {
        let pair = Pair::new();
        let dest = pair.dir.join("evil.bundle");
        let file = std::fs::File::create(&dest).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("changeset.json", opts).unwrap();
        serde_json::to_writer(
            &mut zip,
            &Changeset {
                device_id: "x".to_string(),
                device_name: "x".to_string(),
                ack: None,
                rows: Vec::new(),
                tombstones: Vec::new(),
                files: Vec::new(),
                generated_at: "t".to_string(),
            },
        )
        .unwrap();
        zip.start_file("blobs/../escape.txt", opts).unwrap();
        zip.write_all(b"nope").unwrap();
        zip.finish().unwrap();
        let unpacked = pair.dir.join("evil-out");
        assert!(unpack_bundle(&dest, &unpacked).is_err());
        assert!(!pair.dir.join("escape.txt").exists());
    }

    // ── ack plumbing ──

    #[test]
    fn ack_built_from_pull_cursors_advances_peer_push() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let _s = session(a, "s");
        let cs = build_changeset(a, None, "a").unwrap();
        let (_, cursors) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        // B's reply is its export + the ack from the apply.
        let mut reply = build_changeset(b, Some(&cs.device_id), "b").unwrap();
        reply.ack = Some(cursors);
        let _ = apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
        // A's next export carries `build_ack` for B (the ssh transport's
        // shape) — B applies it and its push cursors advance.
        let bid = b.device_id().unwrap();
        let mut next = build_changeset(a, Some(&bid), "a").unwrap();
        next.ack = Some(build_ack(a, &bid).unwrap());
        let (summary, _) = apply_changeset(b, &pair.space_b(), &next, None).unwrap();
        assert!(summary.acks_applied >= 1);
        // B's export for A is now silent.
        let after = build_changeset(b, Some(&a.device_id().unwrap()), "b").unwrap();
        assert!(after.rows.is_empty());
    }

    #[test]
    fn acks_are_only_honored_for_this_device() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let _s = session(a, "s");
        pair.exchange_ab();
        // B's reply acked A; A's push cursor for B is set.
        let state = db_state(a);
        assert!(state.iter().any(|st| {
            st.peer_id == b.device_id().unwrap()
                && st.table_name == "sessions"
                && st.push_cursor.is_some()
        }));
        // A forged ack addressed to someone else is ignored.
        let forged = Changeset {
            device_id: b.device_id().unwrap(),
            device_name: "b".to_string(),
            ack: Some(vec![PeerCursor {
                peer_id: "someone-else".to_string(),
                table_name: "sessions".to_string(),
                cursor: "2099-01-01T00:00:00Z".to_string(),
            }]),
            rows: Vec::new(),
            tombstones: Vec::new(),
            files: Vec::new(),
            generated_at: "t".to_string(),
        };
        let (summary, _) = apply_changeset(a, &pair.space_a(), &forged, None).unwrap();
        assert_eq!(summary.acks_applied, 0);
    }

    #[test]
    fn push_cursor_advances_only_on_ack() {
        let pair = Pair::new();
        let a = &pair.a;
        let b = &pair.b;
        let _s = session(a, "s");
        let bid = b.device_id().unwrap();
        // Export alone advances nothing.
        let _ = build_changeset(a, Some(&bid), "a").unwrap();
        let after_export = db_state(a);
        assert!(after_export.iter().all(|st| st.peer_id != bid));
        // B imports and acks; only then does A's push cursor move.
        let cs = build_changeset(a, None, "a").unwrap();
        let (_, cursors) = apply_changeset(b, &pair.space_b(), &cs, None).unwrap();
        let mut reply = build_changeset(b, Some(&cs.device_id), "b").unwrap();
        reply.ack = Some(cursors);
        let _ = apply_changeset(a, &pair.space_a(), &reply, None).unwrap();
        let state = db_state(a);
        assert!(state.iter().any(|st| {
            st.peer_id == bid && st.table_name == "sessions" && st.push_cursor.is_some()
        }));
    }
}
