//! Device-local model-call traces. All content is scrubbed before persistence.

mod scrub;

pub use scrub::Scrubber;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::provider::{ChatMessage, ToolCall, ToolDef, Usage};

/// Recording destination and identity captured at the start of a chat turn.
#[derive(Clone)]
pub struct TraceContext {
    pub store: Arc<TraceStore>,
    pub session_id: String,
    pub space_id: String,
    pub turn_id: String,
    pub backend: String,
}

impl std::fmt::Debug for TraceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceContext")
            .field("session_id", &self.session_id)
            .field("space_id", &self.space_id)
            .field("turn_id", &self.turn_id)
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

/// One model call, including its exact request and subsequent tool results.
pub struct TraceStep {
    pub session_id: String,
    pub space_id: String,
    pub turn_id: String,
    pub round: u32,
    pub model: String,
    pub backend: String,
    pub system: String,
    pub tools: Vec<ToolDef>,
    pub messages: Vec<ChatMessage>,
    pub params: Value,
    pub response: TraceResponse,
    pub usage: Option<Usage>,
    pub latency_ms: u64,
    pub tool_results: Vec<TraceToolResult>,
}

/// The raw accumulated response, before history reconstruction.
#[derive(Serialize)]
pub struct TraceResponse {
    pub content: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish: String,
}

/// Both the complete tool output and the possibly shortened model-visible text.
pub struct TraceToolResult {
    pub call_id: String,
    pub name: String,
    pub full: String,
    pub seen: String,
}

/// A scrubbed, content-addressed blob store and its device-local index.
/// The mutex serializes blob writes, index transactions and garbage collection.
pub struct TraceStore {
    dir: PathBuf,
    index: Mutex<Connection>,
    scrubber: Scrubber,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS steps (
    id TEXT PRIMARY KEY,
    at TEXT NOT NULL,
    session_id TEXT NOT NULL,
    space_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    round INTEGER NOT NULL,
    model TEXT NOT NULL,
    backend TEXT NOT NULL,
    system_hash TEXT NOT NULL,
    tools_hash TEXT NOT NULL,
    messages TEXT NOT NULL,
    params TEXT NOT NULL,
    response_hash TEXT NOT NULL,
    finish TEXT NOT NULL,
    prompt_tokens INTEGER, completion_tokens INTEGER,
    cache_read_tokens INTEGER, cache_creation_tokens INTEGER, cost REAL,
    latency_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS steps_session ON steps(session_id, at);
CREATE TABLE IF NOT EXISTS tool_results (
    step_id TEXT NOT NULL, call_id TEXT NOT NULL, name TEXT NOT NULL,
    full_hash TEXT NOT NULL, seen_hash TEXT NOT NULL,
    PRIMARY KEY (step_id, call_id)
);
CREATE TABLE IF NOT EXISTS labels (
    id INTEGER PRIMARY KEY, step_id TEXT NOT NULL, kind TEXT NOT NULL,
    value TEXT NOT NULL, at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS labels_step ON labels(step_id);
";

impl TraceStore {
    /// Open (or create) a trace directory and its separate `SQLite` index.
    pub fn open(dir: &Path, scrubber: Scrubber) -> Result<Self> {
        fs::create_dir_all(dir.join("blobs"))?;
        let index = crate::db::open_conn(&dir.join("index.db"))?;
        index.execute_batch(SCHEMA)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            index: Mutex::new(index),
            scrubber,
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.index
            .lock()
            .map_err(|_| anyhow!("trace index mutex poisoned"))
    }

    fn scrub_json(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.scrubber.scrub(&text)),
            Value::Array(values) => {
                Value::Array(values.into_iter().map(|v| self.scrub_json(v)).collect())
            }
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (self.scrubber.scrub(&key), self.scrub_json(value)))
                    .collect(),
            ),
            other => other,
        }
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.dir
            .join("blobs")
            .join(&hash[..2])
            .join(format!("{}.json", &hash[2..]))
    }

    fn write_blob(&self, value: Value) -> Result<String> {
        let bytes = serde_json::to_vec(&self.scrub_json(value))?;
        let mut hash = String::with_capacity(64);
        for byte in Sha256::digest(&bytes) {
            let _ = write!(hash, "{byte:02x}");
        }
        let path = self.blob_path(&hash);
        if !path.exists() {
            let parent = path.parent().context("blob parent directory")?;
            fs::create_dir_all(parent)?;
            let temporary = parent.join(format!("{}.tmp", Uuid::new_v4()));
            fs::write(&temporary, bytes)?;
            fs::rename(temporary, path)?;
        }
        Ok(hash)
    }

    fn read_blob(&self, hash: &str) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(self.blob_path(hash))?)?)
    }

    /// Write scrubbed blobs first, then atomically insert all rows for a step.
    pub fn record_step(&self, step: &TraceStep) -> Result<String> {
        let mut connection = self.connection()?;
        let system = self.write_blob(json!(step.system))?;
        let tools = self.write_blob(serde_json::to_value(&step.tools)?)?;
        let messages = step
            .messages
            .iter()
            .map(|message| self.write_blob(serde_json::to_value(message)?))
            .collect::<Result<Vec<_>>>()?;
        let response = self.write_blob(serde_json::to_value(&step.response)?)?;
        let results = step
            .tool_results
            .iter()
            .map(|result| {
                Ok((
                    self.scrubber.scrub(&result.call_id),
                    self.scrubber.scrub(&result.name),
                    self.write_blob(json!(result.full))?,
                    self.write_blob(json!(result.seen))?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let id = Uuid::new_v4().to_string();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO steps (
                id, at, session_id, space_id, turn_id, round, model, backend,
                system_hash, tools_hash, messages, params, response_hash, finish,
                prompt_tokens, completion_tokens, cache_read_tokens,
                cache_creation_tokens, cost, latency_ms
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
            )",
            params![
                id,
                Utc::now().to_rfc3339(),
                self.scrubber.scrub(&step.session_id),
                self.scrubber.scrub(&step.space_id),
                self.scrubber.scrub(&step.turn_id),
                step.round,
                self.scrubber.scrub(&step.model),
                self.scrubber.scrub(&step.backend),
                system,
                tools,
                serde_json::to_string(&messages)?,
                serde_json::to_string(&self.scrub_json(step.params.clone()))?,
                response,
                self.scrubber.scrub(&step.response.finish),
                step.usage
                    .map(|u| i64::try_from(u.prompt_tokens))
                    .transpose()?,
                step.usage
                    .map(|u| i64::try_from(u.completion_tokens))
                    .transpose()?,
                step.usage
                    .map(|u| i64::try_from(u.cache_read_tokens))
                    .transpose()?,
                step.usage
                    .map(|u| i64::try_from(u.cache_creation_tokens))
                    .transpose()?,
                step.usage.and_then(|u| u.cost),
                i64::try_from(step.latency_ms)?,
            ],
        )?;
        for (call_id, name, full, seen) in results {
            transaction.execute(
                "INSERT INTO tool_results (step_id, call_id, name, full_hash, seen_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, call_id, name, full, seen],
            )?;
        }
        transaction.commit()?;
        Ok(id)
    }

    /// Attach a scrubbed annotation without changing the original step.
    pub fn add_label(&self, step_id: &str, kind: &str, value: &str) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO labels (step_id, kind, value, at) VALUES (?1, ?2, ?3, ?4)",
            params![
                self.scrubber.scrub(step_id),
                self.scrubber.scrub(kind),
                self.scrubber.scrub(value),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Return step IDs oldest first, preserving insertion order for timestamp ties.
    pub fn steps_for_session(&self, session_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT id FROM steps WHERE session_id = ?1 ORDER BY at, rowid")?;
        Ok(statement
            .query_map([self.scrubber.scrub(session_id)], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Return a turn's step IDs oldest first, preserving insertion order for timestamp ties.
    pub fn steps_for_turn(&self, turn_id: &str) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT id FROM steps WHERE turn_id = ?1 ORDER BY at, rowid")?;
        Ok(statement
            .query_map([self.scrubber.scrub(turn_id)], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Atomically delete a session's steps, tool results and labels, not blobs.
    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let session_id = self.scrubber.scrub(session_id);
        for table in ["tool_results", "labels"] {
            transaction.execute(
                &format!(
                    "DELETE FROM {table} WHERE step_id IN
                     (SELECT id FROM steps WHERE session_id = ?1)"
                ),
                [&session_id],
            )?;
        }
        transaction.execute("DELETE FROM steps WHERE session_id = ?1", [&session_id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Delete steps for missing sessions and collect their unreferenced blobs.
    /// Returns the number of steps deleted.
    pub fn prune_missing_sessions(&self, existing: &HashSet<String>) -> Result<usize> {
        let existing: HashSet<_> = existing.iter().map(|id| self.scrubber.scrub(id)).collect();
        let removed = {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            let sessions = {
                let mut statement = transaction.prepare("SELECT DISTINCT session_id FROM steps")?;
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut removed = 0;
            for session in sessions.iter().filter(|id| !existing.contains(*id)) {
                for table in ["tool_results", "labels"] {
                    transaction.execute(
                        &format!(
                            "DELETE FROM {table} WHERE step_id IN
                             (SELECT id FROM steps WHERE session_id = ?1)"
                        ),
                        [session],
                    )?;
                }
                removed +=
                    transaction.execute("DELETE FROM steps WHERE session_id = ?1", [session])?;
            }
            transaction.commit()?;
            removed
        };
        self.gc()?;
        Ok(removed)
    }

    /// Remove unreferenced blobs, including those left by failed transactions.
    pub fn gc(&self) -> Result<usize> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT system_hash FROM steps
             UNION SELECT tools_hash FROM steps
             UNION SELECT response_hash FROM steps
             UNION SELECT value FROM steps, json_each(steps.messages)
             UNION SELECT full_hash FROM tool_results
             UNION SELECT seen_hash FROM tool_results",
        )?;
        let referenced = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        let mut removed = 0;
        for shard in fs::read_dir(self.dir.join("blobs"))? {
            let shard = shard?;
            for blob in fs::read_dir(shard.path())? {
                let path = blob?.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    let hash = format!(
                        "{}{}",
                        shard.file_name().to_string_lossy(),
                        path.file_stem().context("blob filename")?.to_string_lossy()
                    );
                    if !referenced.contains(&hash) {
                        fs::remove_file(path)?;
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }

    /// Reassemble a scrubbed step, including tool results, usage and labels.
    /// Total tokens are reconstructed from prompt and completion counts.
    pub fn load_step(&self, step_id: &str) -> Result<Value> {
        let connection = self.connection()?;
        let mut step = connection.query_row(
            "SELECT * FROM steps WHERE id = ?1",
            [step_id],
            load_index_row,
        )?;
        for field in ["system", "tools", "response"] {
            step[field] = self.read_blob(step[field].as_str().context("blob hash")?)?;
        }
        let messages: Vec<String> =
            serde_json::from_str(step["messages"].as_str().context("message hashes")?)?;
        step["messages"] = Value::Array(
            messages
                .iter()
                .map(|hash| self.read_blob(hash))
                .collect::<Result<Vec<_>>>()?,
        );
        step["params"] = serde_json::from_str(step["params"].as_str().context("step params")?)?;
        let mut statement = connection.prepare(
            "SELECT call_id, name, full_hash, seen_hash FROM tool_results
             WHERE step_id = ?1 ORDER BY rowid",
        )?;
        let results = statement
            .query_map([step_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        step["tool_results"] = Value::Array(
            results
                .into_iter()
                .map(|(call_id, name, full, seen)| {
                    Ok(json!({
                        "call_id": call_id, "name": name,
                        "full": self.read_blob(&full)?, "seen": self.read_blob(&seen)?,
                    }))
                })
                .collect::<Result<Vec<_>>>()?,
        );
        let mut statement = connection
            .prepare("SELECT id, kind, value, at FROM labels WHERE step_id = ?1 ORDER BY id")?;
        step["labels"] = Value::Array(
            statement
                .query_map([step_id], |row| {
                    Ok(json!({
                        "id": row.get::<_, i64>(0)?,
                        "kind": row.get::<_, String>(1)?,
                        "value": row.get::<_, String>(2)?,
                        "at": row.get::<_, String>(3)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        Ok(step)
    }
}

fn load_index_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let prompt = row.get::<_, Option<i64>>("prompt_tokens")?;
    let completion = row.get::<_, Option<i64>>("completion_tokens")?;
    let usage = prompt.map(|prompt| {
        json!({
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt.cast_unsigned() + completion.unwrap_or(0).cast_unsigned(),
        })
    });
    let mut usage = usage.unwrap_or(Value::Null);
    if !usage.is_null() {
        usage["cache_read_tokens"] = json!(row.get::<_, Option<i64>>("cache_read_tokens")?);
        usage["cache_creation_tokens"] = json!(row.get::<_, Option<i64>>("cache_creation_tokens")?);
        usage["cost"] = json!(row.get::<_, Option<f64>>("cost")?);
    }
    Ok(json!({
        "id": row.get::<_, String>("id")?,
        "at": row.get::<_, String>("at")?,
        "session_id": row.get::<_, String>("session_id")?,
        "space_id": row.get::<_, String>("space_id")?,
        "turn_id": row.get::<_, String>("turn_id")?,
        "round": row.get::<_, u32>("round")?,
        "model": row.get::<_, String>("model")?,
        "backend": row.get::<_, String>("backend")?,
        "system": row.get::<_, String>("system_hash")?,
        "tools": row.get::<_, String>("tools_hash")?,
        "messages": row.get::<_, String>("messages")?,
        "params": row.get::<_, String>("params")?,
        "response": row.get::<_, String>("response_hash")?,
        "finish": row.get::<_, String>("finish")?,
        "usage": usage,
        "latency_ms": row.get::<_, i64>("latency_ms")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("nexus-trace-{}", Uuid::new_v4())))
        }

        fn open(&self) -> TraceStore {
            TraceStore::open(&self.0, Scrubber::new(Vec::new())).unwrap()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn step(session: &str) -> TraceStep {
        let call = ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: r#"{"path":"notes.txt"}"#.into(),
        };
        TraceStep {
            session_id: session.into(),
            space_id: "default".into(),
            turn_id: "turn-1".into(),
            round: 0,
            model: "test-model".into(),
            backend: "Local".into(),
            system: "Be helpful.".into(),
            tools: vec![ToolDef {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            }],
            messages: vec![
                ChatMessage::text("user", "Read notes.txt"),
                ChatMessage {
                    role: "assistant".into(),
                    reasoning_content: Some("I should read the file.".into()),
                    tool_calls: Some(vec![call.clone()]),
                    ..Default::default()
                },
            ],
            params: json!({"temperature": 0.5, "reasoning_effort": "low"}),
            response: TraceResponse {
                content: String::new(),
                reasoning: Some("Reading now.".into()),
                tool_calls: vec![call],
                finish: "tool_calls".into(),
            },
            usage: Some(Usage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                cache_read_tokens: 40,
                cache_creation_tokens: 10,
                cost: Some(0.001),
                ..Default::default()
            }),
            latency_ms: 123,
            tool_results: vec![TraceToolResult {
                call_id: "call-1".into(),
                name: "read".into(),
                full: "Full notes: one two three".into(),
                seen: "Full notes: one…".into(),
            }],
        }
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                paths.extend(files(&path));
            } else {
                paths.push(path);
            }
        }
        paths
    }

    #[test]
    fn record_and_load_round_trip() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TraceStore>();
        let dir = TempDir::new();
        let store = dir.open();
        let step = step("session-1");
        let id = store.record_step(&step).unwrap();
        assert_eq!(Uuid::parse_str(&id).unwrap().get_version_num(), 4);
        drop(store);
        let store = dir.open();
        let loaded = store.load_step(&id).unwrap();
        assert_eq!(loaded["system"], step.system);
        assert_eq!(loaded["tools"], serde_json::to_value(&step.tools).unwrap());
        assert_eq!(
            loaded["messages"],
            serde_json::to_value(&step.messages).unwrap()
        );
        assert_eq!(
            loaded["response"],
            serde_json::to_value(&step.response).unwrap()
        );
        assert_eq!(loaded["params"], step.params);
        assert_eq!(
            loaded["tool_results"],
            json!([{
                "call_id": "call-1", "name": "read",
                "full": step.tool_results[0].full, "seen": step.tool_results[0].seen,
            }])
        );
        assert_eq!(
            loaded["usage"],
            json!({
                "prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120,
                "cache_read_tokens": 40, "cache_creation_tokens": 10, "cost": 0.001,
            })
        );
        assert_eq!(loaded["latency_ms"], 123);
        assert_eq!(loaded["session_id"], "session-1");
        assert_eq!(loaded["space_id"], "default");
        assert_eq!(loaded["turn_id"], "turn-1");
        assert_eq!(loaded["round"], 0);
        assert_eq!(loaded["model"], "test-model");
        assert_eq!(loaded["backend"], "Local");
        assert_eq!(loaded["finish"], "tool_calls");
        let at = chrono::DateTime::parse_from_rfc3339(loaded["at"].as_str().unwrap()).unwrap();
        assert_eq!(at.offset().local_minus_utc(), 0);
        assert_eq!(store.steps_for_session("session-1").unwrap(), vec![id]);
        assert_eq!(store.gc().unwrap(), 0);
    }

    #[test]
    fn prune_missing_sessions_drops_orphans_only() {
        let dir = TempDir::new();
        let store = dir.open();
        let kept = store.record_step(&step("kept")).unwrap();
        let mut orphan = step("missing");
        orphan.system = "Orphan-only system".into();
        orphan.tool_results[0].full = "Orphan-only result".into();
        let removed = store.record_step(&orphan).unwrap();
        store.record_step(&orphan).unwrap();
        store.add_label(&kept, "quality", "good").unwrap();
        store.add_label(&removed, "quality", "bad").unwrap();
        let before = files(&dir.0.join("blobs")).len();
        let existing = HashSet::from(["kept".into(), "never-recorded".into()]);
        assert_eq!(store.prune_missing_sessions(&existing).unwrap(), 2);
        assert_eq!(store.steps_for_session("kept").unwrap(), vec![kept.clone()]);
        assert!(store.steps_for_session("missing").unwrap().is_empty());
        assert!(store.load_step(&removed).is_err());
        let loaded = store.load_step(&kept).unwrap();
        assert_eq!(loaded["labels"][0]["value"], "good");
        assert_eq!(
            loaded["tool_results"][0]["full"],
            "Full notes: one two three"
        );
        let connection = store.connection().unwrap();
        for table in ["tool_results", "labels"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1);
        }
        drop(connection);
        assert_eq!(files(&dir.0.join("blobs")).len(), before - 2);
        assert_eq!(store.gc().unwrap(), 0);
        assert_eq!(store.prune_missing_sessions(&HashSet::new()).unwrap(), 1);
        assert!(files(&dir.0.join("blobs")).is_empty());
    }

    #[test]
    fn identical_content_is_stored_once() {
        let dir = TempDir::new();
        let store = dir.open();
        let mut first = step("session-1");
        first.messages.truncate(1);
        first.tool_results.clear();
        store.record_step(&first).unwrap();
        let mut second = step("session-1");
        second.messages.truncate(1);
        second
            .messages
            .push(ChatMessage::text("user", "Next question"));
        second.tool_results.clear();
        store.record_step(&second).unwrap();
        let blobs = files(&dir.0.join("blobs"));
        assert_eq!(blobs.len(), 5); // system, tools, two messages, response
        for value in [
            json!(first.system),
            serde_json::to_value(&first.tools).unwrap(),
            serde_json::to_value(&first.messages[0]).unwrap(),
        ] {
            assert_eq!(
                blobs
                    .iter()
                    .filter(|path| {
                        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap() == value
                    })
                    .count(),
                1
            );
        }
        for path in blobs {
            let bytes = fs::read(&path).unwrap();
            let mut hash = String::new();
            for byte in Sha256::digest(bytes) {
                let _ = write!(hash, "{byte:02x}");
            }
            assert_eq!(path, store.blob_path(&hash));
        }
    }

    #[test]
    fn steps_for_turn_returns_only_that_turn_in_order() {
        let dir = TempDir::new();
        let store = dir.open();
        let mut step = step("session-1");
        let first = store.record_step(&step).unwrap();
        step.turn_id = "turn-2".into();
        let other = store.record_step(&step).unwrap();
        step.turn_id = "turn-1".into();
        step.round = 1;
        let second = store.record_step(&step).unwrap();
        store
            .connection()
            .unwrap()
            .execute("UPDATE steps SET at = '2026-01-01T00:00:00Z'", [])
            .unwrap();

        assert_eq!(store.steps_for_turn("turn-1").unwrap(), vec![first, second]);
        assert_eq!(store.steps_for_turn("turn-2").unwrap(), vec![other]);
        assert!(store.steps_for_turn("missing").unwrap().is_empty());
    }

    #[test]
    fn labels_attach_to_steps() {
        let dir = TempDir::new();
        let store = dir.open();
        let first = store.record_step(&step("session-1")).unwrap();
        let second = store.record_step(&step("session-1")).unwrap();
        store.add_label(&first, "rejected", "retry").unwrap();
        store.add_label(&first, "quality", "poor").unwrap();
        let loaded = store.load_step(&first).unwrap();
        assert_eq!(loaded["labels"][0]["kind"], "rejected");
        assert_eq!(loaded["labels"][0]["value"], "retry");
        assert_eq!(loaded["labels"][1]["kind"], "quality");
        assert_eq!(loaded["labels"][1]["value"], "poor");
        assert!(
            chrono::DateTime::parse_from_rfc3339(loaded["labels"][0]["at"].as_str().unwrap())
                .is_ok()
        );
        assert_eq!(store.load_step(&second).unwrap()["labels"], json!([]));
        assert_eq!(
            store.steps_for_session("session-1").unwrap(),
            vec![first, second]
        );
    }

    #[test]
    fn delete_session_removes_rows_and_gc_frees_blobs() {
        let dir = TempDir::new();
        let store = dir.open();
        let first = store.record_step(&step("session-1")).unwrap();
        let mut other = step("session-2");
        other.system = "Different system prompt".into();
        other.messages.truncate(1);
        other.tool_results.clear();
        let second = store.record_step(&other).unwrap();
        store.add_label(&first, "rejected", "retry").unwrap();
        store.add_label(&second, "quality", "good").unwrap();
        let before = files(&dir.0.join("blobs")).len();
        store.delete_session("session-1").unwrap();
        assert!(store.steps_for_session("session-1").unwrap().is_empty());
        assert!(store.load_step(&first).is_err());
        {
            let connection = store.connection().unwrap();
            for table in ["steps", "tool_results", "labels"] {
                let column = if table == "steps" { "id" } else { "step_id" };
                let count: i64 = connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                        [&first],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0);
            }
        }
        assert_eq!(store.gc().unwrap(), 4); // first system, assistant message, full and seen
        assert_eq!(files(&dir.0.join("blobs")).len(), before - 4);
        let loaded = store.load_step(&second).unwrap();
        assert_eq!(
            loaded["messages"],
            serde_json::to_value(&other.messages).unwrap()
        );
        assert_eq!(loaded["labels"][0]["value"], "good");
        assert_eq!(store.gc().unwrap(), 0);
        store.delete_session("session-2").unwrap();
        assert_eq!(store.gc().unwrap(), 4);
        assert!(files(&dir.0.join("blobs")).is_empty());
    }

    #[test]
    fn scrubber_redacts_exact_secrets_and_patterns() {
        let scrubber = Scrubber::new(vec![
            "exact-secret".into(),
            "exact-secret-longer".into(),
            String::new(),
            "tiny".into(),
        ]);
        for token in [
            "exact-secret",
            "exact-secret-longer",
            "sk-0123456789abcdefghij",
            "sk-or-v1-0123456789abcdefghij",
            "sk-proj-0123456789abcdefghij",
            "ghp_0123456789abcdefghij",
            "gho_0123456789abcdefghij",
            "ghs_0123456789abcdefghij",
            "github_pat_0123456789abcdefghij",
            "AKIA0123456789ABCDEF",
        ] {
            assert_eq!(scrubber.scrub(token), "[REDACTED]", "{token}");
            assert_eq!(
                scrubber.scrub(&format!("Here is {token}, voilà.")),
                "Here is [REDACTED], voilà."
            );
        }
        assert_eq!(
            scrubber.scrub("bEaReR aB09._~+/=-123456789"),
            "bEaReR [REDACTED]"
        );
        assert_eq!(
            scrubber.scrub("Bearer 0123456789abcdef"),
            "Bearer [REDACTED]"
        );
        for label in [
            "PRIVATE KEY",
            "RSA PRIVATE KEY",
            "EC PRIVATE KEY",
            "ENCRYPTED PRIVATE KEY",
        ] {
            let pem =
                format!("Before\n-----BEGIN {label}-----\nsecret\n-----END {label}-----\nAfter");
            assert_eq!(scrubber.scrub(&pem), "Before\n[REDACTED]\nAfter");
            let framing_secret = Scrubber::new(vec!["PRIVATE KEY".into()]);
            assert_eq!(framing_secret.scrub(&pem), "Before\n[REDACTED]\nAfter");
        }
        for plain in [
            "sk-short",
            "Ordinary prose: café. tiny",
            "AKIA0123456789ABCDE",
            "AKIA0123456789ABCDEFG",
            "Bearer short",
            "ghp_short",
            "-----BEGIN PUBLIC KEY-----\npublic\n-----END PUBLIC KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nno matching end\n-----END EC PRIVATE KEY-----",
        ] {
            assert_eq!(scrubber.scrub(plain), plain);
        }
    }

    #[test]
    fn secrets_never_reach_disk() {
        let dir = TempDir::new();
        let exact = "exact-credential-123456";
        let key = "sk-proj-0123456789abcdefghijklmnop";
        let escaped = "quoted-\"secret\\with\na-newline";
        let pem = "-----BEGIN PRIVATE KEY-----\nprivate-body\n-----END PRIVATE KEY-----";
        let store =
            TraceStore::open(&dir.0, Scrubber::new(vec![exact.into(), escaped.into()])).unwrap();
        let mut step = step(exact);
        let text = format!("{exact} {key} {escaped}\n{pem}");
        step.system.clone_from(&text);
        step.messages[0].content.clone_from(&text);
        step.tools[0].description.clone_from(&text);
        step.tools[0].parameters = json!({exact: text});
        step.params = json!({"nested": [{exact: text}]});
        step.response.content.clone_from(&text);
        step.response.reasoning = Some(text.clone());
        step.response.tool_calls[0].arguments.clone_from(&text);
        step.tool_results[0].full.clone_from(&text);
        step.tool_results[0].seen.clone_from(&text);
        step.tool_results[0].name = key.into();
        let id = store.record_step(&step).unwrap();
        store.add_label(&id, exact, &text).unwrap();
        let loaded = store.load_step(&id).unwrap();
        assert_eq!(
            loaded["messages"][0]["content"],
            "[REDACTED] [REDACTED] [REDACTED]\n[REDACTED]"
        );
        assert_eq!(
            loaded["params"]["nested"][0]["[REDACTED]"],
            loaded["system"]
        );
        for path in files(&dir.0) {
            let bytes = fs::read(&path).unwrap();
            for forbidden in [exact, key, "private-body", "quoted-", "secret\\\\with"] {
                assert!(
                    !bytes
                        .windows(forbidden.len())
                        .any(|window| window == forbidden.as_bytes()),
                    "secret in {}",
                    path.display()
                );
            }
        }
        drop(store);
        for path in files(&dir.0) {
            let bytes = fs::read(&path).unwrap();
            for forbidden in [exact, key] {
                assert!(
                    !bytes
                        .windows(forbidden.len())
                        .any(|window| window == forbidden.as_bytes()),
                    "secret in {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn failed_step_is_atomic_and_orphan_blobs_are_collected() {
        let dir = TempDir::new();
        let store = dir.open();
        let mut step = step("session-1");
        step.tool_results.push(TraceToolResult {
            call_id: "call-1".into(),
            name: "read".into(),
            full: "Another result".into(),
            seen: "Another excerpt".into(),
        });
        assert!(store.record_step(&step).is_err());
        assert!(store.steps_for_session("session-1").unwrap().is_empty());
        assert_eq!(
            store
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM tool_results", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let count = files(&dir.0.join("blobs")).len();
        assert!(count > 0);
        assert_eq!(store.gc().unwrap(), count);
    }

    #[test]
    fn missing_usage_and_cost_remain_null() {
        let dir = TempDir::new();
        let store = dir.open();
        let mut step = step("session-1");
        step.usage = None;
        let id = store.record_step(&step).unwrap();
        assert!(store.load_step(&id).unwrap()["usage"].is_null());
        step.usage = Some(Usage::default());
        let id = store.record_step(&step).unwrap();
        let loaded = store.load_step(&id).unwrap();
        assert_eq!(loaded["usage"]["total_tokens"], 0);
        assert!(loaded["usage"]["cost"].is_null());
    }
}
