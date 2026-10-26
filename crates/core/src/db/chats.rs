//! Rebuildable, device-local conversation search data.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use super::{blob_to_vec, cosine, vec_to_blob};

pub(crate) const INDEX_BATCH: usize = 16;
pub(crate) const EMBED_BATCH: usize = 32;
pub(crate) const CHUNK_TITLE: &str = "chat";

pub(super) fn migrate(conn: &Connection, schema: &str) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {schema}.message_index (
            message_id TEXT PRIMARY KEY
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS {schema}.message_chunks USING fts5(
            message_id UNINDEXED, seq UNINDEXED, text
        );
        CREATE TABLE IF NOT EXISTS {schema}.message_embeddings (
            message_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            identity TEXT NOT NULL,
            vec BLOB NOT NULL,
            PRIMARY KEY (message_id, seq, identity)
        );"
    ))?;
    Ok(())
}

// TEMP triggers belong to the connection, not the durable schema. They also
// cover sync's direct SQL deletes and edits made through tool connections.
pub(super) fn install_cleanup(conn: &Connection) -> Result<()> {
    if !super::has_column(conn, "messages", "id")? {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TEMP TRIGGER IF NOT EXISTS chat_message_deleted AFTER DELETE ON main.messages BEGIN
            DELETE FROM message_chunks WHERE message_id = OLD.id;
            DELETE FROM message_embeddings WHERE message_id = OLD.id;
            DELETE FROM message_index WHERE message_id = OLD.id;
        END;
        CREATE TEMP TRIGGER IF NOT EXISTS chat_message_edited AFTER UPDATE OF content, role ON main.messages BEGIN
            DELETE FROM message_chunks WHERE message_id = OLD.id;
            DELETE FROM message_embeddings WHERE message_id = OLD.id;
            DELETE FROM message_index WHERE message_id = OLD.id;
        END;
        CREATE TEMP TRIGGER IF NOT EXISTS chat_session_deleted BEFORE DELETE ON main.sessions BEGIN
            DELETE FROM message_chunks WHERE message_id IN (SELECT id FROM messages WHERE session_id = OLD.id);
            DELETE FROM message_embeddings WHERE message_id IN (SELECT id FROM messages WHERE session_id = OLD.id);
            DELETE FROM message_index WHERE message_id IN (SELECT id FROM messages WHERE session_id = OLD.id);
        END;",
    )?;
    Ok(())
}

/// Index a bounded page of missing messages, newest first, including synced rows.
pub(crate) fn index_pending(conn: &Connection) -> Result<usize> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut stmt = conn.prepare(
        "SELECT m.id, m.content FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.role IN ('user', 'assistant') AND trim(m.content) != ''
           AND NOT EXISTS (SELECT 1 FROM cache.message_index i WHERE i.message_id = m.id)
         ORDER BY m.created_at DESC, m.rowid DESC LIMIT ?1",
    )?;
    let messages = stmt
        .query_map([i64::try_from(INDEX_BATCH)?], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, text) in &messages {
        for (seq, (_, chunk)) in crate::extract::embedding::chunk_text(text, CHUNK_TITLE)
            .into_iter()
            .enumerate()
        {
            tx.execute(
                "INSERT INTO cache.message_chunks (message_id, seq, text) VALUES (?1, ?2, ?3)",
                (id, i64::try_from(seq)?, chunk),
            )?;
        }
        tx.execute("INSERT INTO cache.message_index VALUES (?1)", [id])?;
    }
    tx.commit()?;
    Ok(messages.len())
}

/// One bounded embedding batch. Sequence numbers remain local to each message.
pub(crate) fn missing_vectors(
    conn: &Connection,
    identity: &str,
) -> Result<Vec<(String, i64, String)>> {
    conn.execute(
        "DELETE FROM cache.message_embeddings WHERE identity != ?1",
        [identity],
    )?;
    let mut stmt = conn.prepare(
        "SELECT c.message_id, CAST(c.seq AS INTEGER), c.text FROM cache.message_chunks c
         JOIN messages m ON m.id = c.message_id
         WHERE NOT EXISTS (SELECT 1 FROM cache.message_embeddings e
             WHERE e.message_id = c.message_id AND e.seq = CAST(c.seq AS INTEGER) AND e.identity = ?1)
         ORDER BY m.created_at DESC, m.rowid DESC, CAST(c.seq AS INTEGER) LIMIT ?2",
    )?;
    Ok(stmt
        .query_map((identity, i64::try_from(EMBED_BATCH)?), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Store only chunks that still exist with the same text (a delete/edit can race a job).
pub(crate) fn store_vectors(
    conn: &Connection,
    identity: &str,
    chunks: &[(String, i64, String)],
    vectors: &[(i64, Vec<f32>)],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (index, vector) in vectors {
        let Some((id, seq, text)) = usize::try_from(*index).ok().and_then(|i| chunks.get(i)) else {
            continue;
        };
        tx.execute(
            "INSERT OR REPLACE INTO cache.message_embeddings (message_id, seq, identity, vec)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (
                 SELECT 1 FROM cache.message_chunks c JOIN messages m ON m.id = c.message_id
                 WHERE c.message_id = ?1 AND CAST(c.seq AS INTEGER) = ?2 AND c.text = ?5)",
            (id, seq, identity, vec_to_blob(vector), text),
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Messages fully indexed for this identity, and total eligible messages in a space.
pub(crate) fn progress(
    conn: &Connection,
    space: &str,
    identity: Option<&str>,
) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(i.message_id IS NOT NULL AND
             (?2 IS NULL OR COALESCE(e.vectors, 0) = COALESCE(c.chunks, 0))), 0), COUNT(*)
         FROM messages m JOIN sessions s ON s.id = m.session_id
         LEFT JOIN cache.message_index i ON i.message_id = m.id
         LEFT JOIN (SELECT message_id, COUNT(*) AS chunks FROM cache.message_chunks
                    GROUP BY message_id) c ON c.message_id = m.id
         LEFT JOIN (SELECT message_id, COUNT(*) AS vectors FROM cache.message_embeddings
                    WHERE identity = ?2 GROUP BY message_id) e ON e.message_id = m.id
         WHERE s.space_id = ?1 AND m.role IN ('user', 'assistant') AND trim(m.content) != ''",
        (space, identity),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// A message-level search result; repeated chunks contribute only their best rank.
#[derive(Debug)]
pub(crate) struct Hit {
    pub session: String,
    pub title: String,
    pub date: String,
    pub role: String,
    pub message: String,
    pub snippet: String,
    pub score: f32,
}

pub(crate) struct Search<'a> {
    pub space: &'a str,
    pub current: &'a str,
    pub all_spaces: bool,
    pub query: &'a str,
    pub vector: Option<(&'a str, &'a [f32])>,
    pub limit: usize,
}

/// Merge keyword and cosine rankings with reciprocal rank fusion (k=60).
pub(crate) fn search(conn: &Connection, request: &Search<'_>) -> Result<Vec<Hit>> {
    let terms = request
        .query
        .split_whitespace()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut keyword = Vec::new();
    if !terms.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT c.message_id, c.text FROM cache.message_chunks c
             JOIN messages m ON m.id = c.message_id JOIN sessions s ON s.id = m.session_id
             WHERE message_chunks MATCH ?1 AND m.session_id != ?2 AND (?3 OR s.space_id = ?4)
             ORDER BY bm25(message_chunks) LIMIT 128",
        )?;
        keyword = stmt
            .query_map(
                (&terms, request.current, request.all_spaces, request.space),
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
    }
    let semantic = semantic_ranks(conn, request)?;
    let mut fused: HashMap<String, (String, f32)> = HashMap::new();
    for ranking in [keyword, semantic] {
        let mut seen = std::collections::HashSet::new();
        let mut rank = 0_u16;
        for (id, text) in ranking {
            if seen.insert(id.clone()) {
                rank += 1;
                let entry = fused.entry(id).or_insert((text, 0.0));
                entry.1 += 1.0 / (60.0 + f32::from(rank));
            }
        }
    }
    let mut hits = Vec::new();
    for (id, (text, score)) in fused {
        hits.push(conn.query_row(
            "SELECT s.id, s.title, m.created_at, m.role FROM messages m
             JOIN sessions s ON s.id = m.session_id WHERE m.id = ?1",
            [&id],
            |r| {
                Ok(Hit {
                    session: r.get(0)?,
                    title: r.get(1)?,
                    date: r.get(2)?,
                    role: r.get(3)?,
                    message: id.clone(),
                    snippet: text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(300)
                        .collect(),
                    score,
                })
            },
        )?);
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.date.cmp(&a.date))
            .then_with(|| a.message.cmp(&b.message))
    });
    hits.truncate(request.limit);
    Ok(hits)
}

fn semantic_ranks(conn: &Connection, request: &Search<'_>) -> Result<Vec<(String, String)>> {
    let Some((identity, query)) = request.vector else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT e.message_id, c.text, e.vec FROM cache.message_embeddings e
         JOIN cache.message_chunks c ON c.message_id = e.message_id AND CAST(c.seq AS INTEGER) = e.seq
         JOIN messages m ON m.id = e.message_id JOIN sessions s ON s.id = m.session_id
         WHERE e.identity = ?1 AND m.session_id != ?2 AND (?3 OR s.space_id = ?4)",
    )?;
    let rows = stmt.query_map(
        (identity, request.current, request.all_spaces, request.space),
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        },
    )?;
    let mut ranked = Vec::new();
    for row in rows {
        let (id, text, bytes) = row?;
        let vector = blob_to_vec(&bytes);
        if vector.len() == query.len() && vector.iter().all(|v| v.is_finite()) {
            let score = cosine(query, &vector);
            if score.is_finite() && score > 0.0 {
                ranked.push((id, text, score));
            }
        }
    }
    ranked.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(128);
    Ok(ranked.into_iter().map(|(id, text, _)| (id, text)).collect())
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
