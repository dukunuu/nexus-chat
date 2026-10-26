//! Read-only access to past conversations using the file embedding backend.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use rusqlite::OptionalExtension as _;
use serde_json::Value;

use super::FilesCtx;
use crate::db::chats::{self, Search};

pub(super) async fn search(ctx: &FilesCtx, current: &str, value: &Value) -> Result<String> {
    let query = value["query"].as_str().unwrap_or_default();
    let conn = crate::db::open_attached(&ctx.db_path)?;
    // A newly synced message need not wait for a vector to become searchable.
    // This is the same bounded backfill step the background job uses.
    chats::index_pending(&conn)?;
    let vector = match &ctx.embedder {
        Some(embedder) => embedder.query(query).await.ok(),
        None => None,
    };
    let identity = ctx
        .embedder
        .as_ref()
        .and_then(|b| b.identity())
        .map(crate::provider::embedding::EmbedIdentity::key);
    let hits = chats::search(
        &conn,
        &Search {
            space: &ctx.space_id,
            current,
            all_spaces: false,
            query,
            vector: identity.as_deref().zip(vector.as_deref()),
            limit: usize::try_from(value["limit"].as_u64().unwrap_or(10).clamp(1, 20))
                .expect("bounded hit limit"),
        },
    )?;
    let mut out = format!(
        "past chats ({} search); fetch_url a chat: URL to read one\n",
        if vector.is_some() {
            "hybrid"
        } else {
            "keyword"
        }
    );
    if hits.is_empty() {
        out.push_str("no matches");
    }
    for (n, hit) in hits.into_iter().enumerate() {
        let title: String = hit.title.chars().take(120).collect();
        let _ = write!(
            out,
            "\n[{}] {title} · {} · {}\n    chat:{}#{}\n    {}\n",
            n + 1,
            hit.date,
            hit.role,
            hit.session,
            hit.message,
            hit.snippet
        );
    }
    Ok(out)
}

/// Lines wider than this are split so line paging still reaches every part.
const LINE_CHARS: usize = 2_000;
/// Messages shown ahead of the one a `#message` fragment points at.
const CONTEXT_BEFORE: usize = 2;

/// A past conversation of the active space as numbered-line text for
/// `fetch_url`, and the 1-based line to start at: just ahead of the
/// `#message` fragment when there is one, else the top. Tool calls and
/// results collapse to one line each.
pub(super) fn transcript(ctx: &FilesCtx, url: &str) -> Result<(String, usize)> {
    let rest = url.strip_prefix("chat:").unwrap_or(url);
    let (session, message) = rest
        .split_once('#')
        .map_or((rest, None), |(s, m)| (s, Some(m)));
    let conn = crate::db::open_attached(&ctx.db_path)?;
    // Only this space's chats: another space's history stays out of reach
    // even when its session id is known.
    let title: Option<String> = conn
        .query_row(
            "SELECT title FROM sessions WHERE id = ?1 AND space_id = ?2",
            (session, &ctx.space_id),
            |r| r.get(0),
        )
        .optional()?;
    let Some(title) = title else {
        bail!("no chat {session} in this space")
    };
    let mut stmt = conn.prepare(
        "SELECT id, role, created_at, content FROM messages WHERE session_id = ?1
         ORDER BY created_at, rowid",
    )?;
    let rows = stmt.query_map([session], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let title: String = title.chars().take(120).collect();
    let mut lines = vec![format!("chat {title:?}")];
    let mut starts: Vec<(String, usize)> = Vec::new();
    for row in rows {
        let (id, role, date, content) = row?;
        starts.push((id, lines.len()));
        if role.starts_with("tool") {
            let summary: String = content
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect();
            lines.push(format!("[{date}] {role}: {summary}"));
            continue;
        }
        lines.push(format!("[{date}] {role}"));
        for line in content.lines() {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                lines.push(String::new());
            }
            for part in chars.chunks(LINE_CHARS) {
                lines.push(part.iter().collect());
            }
        }
        lines.push(String::new());
    }
    let start = match message {
        Some(id) => {
            let index = starts
                .iter()
                .position(|(m, _)| m == id)
                .ok_or_else(|| anyhow::anyhow!("message {id} is not in this chat"))?;
            starts[index.saturating_sub(CONTEXT_BEFORE)].1 + 1
        }
        None => 1,
    };
    Ok((lines.join("\n"), start))
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
