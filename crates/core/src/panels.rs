//! Durable storage for nexus-ui panels that outlive their message: per-panel
//! saved state, and panels pinned as named mini-apps in a space.
//!
//! Both tables live in the durable `nexus.db` and sync like the rest of it
//! (`sync.rs`): `panel_states` is a last-writer-wins row per (conversation,
//! panel), `pinned_panels` a last-writer-wins row per app with tombstones for
//! deletes. Functions take a plain connection so the TUI, the CLI, the host and
//! per-call tool connections all share one implementation.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::{Result, anyhow, bail, ensure};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;

use crate::interactive::{Document, MAX_SOURCE, PanelSnapshot, Session};
use crate::tui_apps::valid_name;

/// Most panels one space can pin.
pub const MAX_PINNED: usize = 64;

/// A panel saved as a named mini-app: its program and the state it was left in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    /// Stable identity; what the model and the sync engine address it by.
    pub id: String,
    /// Owning space.
    pub space_id: String,
    /// Space-unique name, `[a-z0-9-]{1,32}` like terminal apps.
    pub name: String,
    /// The panel's own title, for display.
    pub title: String,
    /// Current nexus-ui program (patches included).
    pub program: String,
    /// Encoded [`PanelSnapshot`].
    pub record: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last program or state change.
    pub updated_at: String,
}

/// Create the tables on an existing or fresh durable db.
/// # Errors
/// Returns database failures.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS panel_states (
            session_id TEXT NOT NULL,
            panel_key  TEXT NOT NULL,
            record     TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (session_id, panel_key)
        );
        CREATE TABLE IF NOT EXISTS pinned_panels (
            id         TEXT PRIMARY KEY,
            space_id   TEXT NOT NULL,
            name       TEXT NOT NULL,
            title      TEXT NOT NULL,
            program    TEXT NOT NULL,
            record     TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_pinned_panels_space
            ON pinned_panels(space_id, name);",
    )?;
    Ok(())
}

/// Save (or replace) one panel's state record within a conversation.
/// # Errors
/// Rejects oversized records and returns database failures.
pub fn save_state(
    conn: &Connection,
    session_id: &str,
    panel_key: &str,
    record: &str,
) -> Result<()> {
    ensure!(
        record.len() <= crate::interactive::MAX_RECORD,
        "saved panel state is too large"
    );
    conn.execute(
        "INSERT INTO panel_states (session_id, panel_key, record, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_id, panel_key)
         DO UPDATE SET record = excluded.record, updated_at = excluded.updated_at",
        params![session_id, panel_key, record, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Every saved panel record of a conversation, by panel key.
/// # Errors
/// Returns database failures.
pub fn load_states(conn: &Connection, session_id: &str) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT panel_key, record FROM panel_states WHERE session_id = ?1")?;
    let rows = stmt.query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn pinned_from(row: &rusqlite::Row) -> rusqlite::Result<Pinned> {
    Ok(Pinned {
        id: row.get(0)?,
        space_id: row.get(1)?,
        name: row.get(2)?,
        title: row.get(3)?,
        program: row.get(4)?,
        record: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

const PINNED_COLUMNS: &str = "id, space_id, name, title, program, record, created_at, updated_at";

/// Pinned apps of a space, by name.
/// # Errors
/// Returns database failures.
pub fn list(conn: &Connection, space_id: &str) -> Result<Vec<Pinned>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PINNED_COLUMNS} FROM pinned_panels WHERE space_id = ?1 ORDER BY name, id"
    ))?;
    let rows = stmt.query_map([space_id], pinned_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Look an app up by id, else by name (newest wins if a sync produced two).
/// # Errors
/// Returns database failures.
pub fn find(conn: &Connection, space_id: &str, reference: &str) -> Result<Option<Pinned>> {
    let by_id = conn
        .query_row(
            &format!("SELECT {PINNED_COLUMNS} FROM pinned_panels WHERE space_id = ?1 AND id = ?2"),
            params![space_id, reference],
            pinned_from,
        )
        .optional()?;
    if by_id.is_some() {
        return Ok(by_id);
    }
    Ok(conn
        .query_row(
            &format!(
                "SELECT {PINNED_COLUMNS} FROM pinned_panels WHERE space_id = ?1 AND name = ?2
                 ORDER BY updated_at DESC, id LIMIT 1"
            ),
            params![space_id, reference],
            pinned_from,
        )
        .optional()?)
}

fn name_taken(conn: &Connection, space_id: &str, name: &str, except_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pinned_panels
                       WHERE space_id = ?1 AND name = ?2 AND id <> ?3)",
        params![space_id, name, except_id],
        |row| row.get(0),
    )?)
}

/// Turn a panel title into a valid, unused app name.
/// # Errors
/// Returns database failures.
pub fn suggest_name(conn: &Connection, space_id: &str, title: &str) -> Result<String> {
    let mut slug = String::new();
    for ch in title.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let base: String = if slug.is_empty() { "panel" } else { slug }
        .chars()
        .take(28)
        .collect();
    let base = base.trim_end_matches('-').to_string();
    let taken = |name: &str| name_taken(conn, space_id, name, "");
    if !taken(&base)? {
        return Ok(base);
    }
    for n in 2..1000 {
        let mut name = base.clone();
        write!(name, "-{n}")?;
        if !taken(&name)? {
            return Ok(name);
        }
    }
    bail!("no free app name for {base}")
}

/// Pin a panel's program and state as a named app.
/// # Errors
/// Rejects invalid or taken names, invalid programs, a full space and
/// database failures.
pub fn pin(
    conn: &Connection,
    space_id: &str,
    name: &str,
    program: &str,
    snapshot: &PanelSnapshot,
) -> Result<Pinned> {
    ensure!(valid_name(name), "invalid app name: use [a-z0-9-]{{1,32}}");
    ensure!(program.len() <= MAX_SOURCE, "panel program is too large");
    let title = Document::parse(program)?.title().to_string();
    let record = snapshot
        .encode()
        .ok_or_else(|| anyhow!("panel state is too large to pin"))?;
    ensure!(
        !name_taken(conn, space_id, name, "")?,
        "an app named {name} is already pinned"
    );
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pinned_panels WHERE space_id = ?1",
        [space_id],
        |row| row.get(0),
    )?;
    ensure!(
        count < i64::try_from(MAX_PINNED)?,
        "this space already has {MAX_PINNED} pinned apps"
    );
    let now = Utc::now().to_rfc3339();
    let app = Pinned {
        id: Uuid::new_v4().to_string(),
        space_id: space_id.into(),
        name: name.into(),
        title,
        program: program.into(),
        record,
        created_at: now.clone(),
        updated_at: now,
    };
    conn.execute(
        "INSERT INTO pinned_panels
            (id, space_id, name, title, program, record, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            app.id,
            app.space_id,
            app.name,
            app.title,
            app.program,
            app.record,
            app.created_at,
            app.updated_at
        ],
    )?;
    Ok(app)
}

/// Save the state an opened app was left in.
/// # Errors
/// Rejects oversized records and returns database failures.
pub fn save_pinned_state(conn: &Connection, id: &str, record: &str) -> Result<()> {
    ensure!(
        record.len() <= crate::interactive::MAX_RECORD,
        "saved panel state is too large"
    );
    conn.execute(
        "UPDATE pinned_panels SET record = ?1, updated_at = ?2 WHERE id = ?3",
        params![record, Utc::now().to_rfc3339(), id],
    )?;
    Ok(())
}

/// Rename an app.
/// # Errors
/// Rejects unknown apps, invalid or taken names and database failures.
pub fn rename(conn: &Connection, space_id: &str, reference: &str, to: &str) -> Result<Pinned> {
    ensure!(valid_name(to), "invalid app name: use [a-z0-9-]{{1,32}}");
    let app =
        find(conn, space_id, reference)?.ok_or_else(|| anyhow!("no pinned app {reference}"))?;
    ensure!(
        !name_taken(conn, space_id, to, &app.id)?,
        "an app named {to} is already pinned"
    );
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE pinned_panels SET name = ?1, updated_at = ?2 WHERE id = ?3",
        params![to, now, app.id],
    )?;
    Ok(Pinned {
        name: to.into(),
        updated_at: now,
        ..app
    })
}

/// Remove an app, leaving a tombstone so other devices drop it too.
/// # Errors
/// Rejects unknown apps and returns database failures.
pub fn unpin(conn: &Connection, space_id: &str, reference: &str) -> Result<Pinned> {
    let app =
        find(conn, space_id, reference)?.ok_or_else(|| anyhow!("no pinned app {reference}"))?;
    conn.execute("DELETE FROM pinned_panels WHERE id = ?1", [&app.id])?;
    conn.execute(
        "INSERT INTO sync_tombstones (table_name, row_id, deleted_at) VALUES (?1, ?2, ?3)",
        params!["pinned_panels", app.id, Utc::now().to_rfc3339()],
    )?;
    Ok(app)
}

/// Apply an edit-mode patch (changed assignments only) to a pinned app. The
/// merged program must validate and the app's saved state survives by name; a
/// failed patch leaves the app untouched.
/// # Errors
/// Returns the validation failure or database failures.
pub fn patch(conn: &Connection, space_id: &str, reference: &str, patch: &str) -> Result<Pinned> {
    let app =
        find(conn, space_id, reference)?.ok_or_else(|| anyhow!("no pinned app {reference}"))?;
    let mut session = Session::new(Document::parse(&app.program)?)?;
    if let Ok(snapshot) = PanelSnapshot::decode(&app.record) {
        // A record the program no longer accepts only costs the saved state.
        let _ = session.restore(&snapshot);
    }
    session.patch(patch)?;
    let program = session
        .program()
        .ok_or_else(|| anyhow!("legacy panels cannot be patched"))?
        .to_string();
    ensure!(program.len() <= MAX_SOURCE, "patched program is too large");
    let record = session
        .snapshot()
        .encode()
        .ok_or_else(|| anyhow!("panel state is too large to save"))?;
    let title = session.document().title().to_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE pinned_panels
         SET program = ?1, record = ?2, title = ?3, updated_at = ?4 WHERE id = ?5",
        params![program, record, title, now, app.id],
    )?;
    Ok(Pinned {
        program,
        record,
        title,
        updated_at: now,
        ..app
    })
}

/// The model's view of pinned apps: list, read, patch. Reads are free; the
/// executor classes `patch` as a write, so the usual approval ask applies.
/// # Errors
/// Returns missing arguments, unknown apps, patch validation and database
/// failures.
pub fn tool(
    db_path: &std::path::Path,
    space_id: &str,
    action: &str,
    args: &serde_json::Value,
) -> Result<String> {
    let conn = crate::db::open_conn(db_path)?;
    let text = |key: &str| {
        args[key]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("{key} is required"))
    };
    match action {
        "list" => Ok(serde_json::to_string(
            &list(&conn, space_id)?
                .iter()
                .map(|app| {
                    serde_json::json!({
                        "id": app.id, "name": app.name, "title": app.title,
                        "updated": app.updated_at,
                    })
                })
                .collect::<Vec<_>>(),
        )?),
        "read" => {
            let reference = text("app")?;
            let app = find(&conn, space_id, reference)?
                .ok_or_else(|| anyhow!("no pinned app {reference}"))?;
            let state = PanelSnapshot::decode(&app.record)
                .map(|snapshot| serde_json::Value::Object(snapshot.state))
                .unwrap_or_default();
            Ok(format!("{}\n\nState: {state}", app.program))
        }
        "patch" => {
            let patch_text = text("patch")?;
            let body =
                crate::interactive::edit_patch(patch_text).map_or(patch_text, |(_, body)| body);
            let app = patch(&conn, space_id, text("app")?, body)?;
            Ok(format!("patched {} ({})", app.name, app.id))
        }
        _ => bail!("unknown panel_app action"),
    }
}

#[cfg(test)]
mod tests;
