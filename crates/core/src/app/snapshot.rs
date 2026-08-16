//! The serde-shaped view of app state the Phase 4 nexus host API will
//! consume: sessions, models, settings, tasks. Designed now so the wire
//! shape is stable before the API lands; the TUI does not consume this
//! (it reads fields directly until 2e).

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::App;

/// One snapshot of the app's domain state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreSnapshot {
    pub sessions: Vec<SessionSnapshot>,
    pub models: Vec<ModelSnapshot>,
    pub settings: SettingsSnapshot,
    pub tasks: Vec<TaskSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub title: String,
    pub slug: Option<String>,
    pub model: String,
    pub kind: String,
    pub web_mode: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSnapshot {
    /// Composite id (backend prefix + wire id) — what `current_model`,
    /// favorites, and last-used store.
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    pub favorite: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub model: Option<String>,
    pub verbosity: String,
    pub web_mode: bool,
    pub incognito: bool,
    pub searxng_url: String,
    pub langsearch_key: String,
    pub search_provider: String,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub compact_threshold: u8,
    pub memory_model: String,
    pub transcriber_model: String,
    pub ocr_model: String,
    pub ocr_engine: String,
    pub embedding_model: String,
    pub image_gen_model: String,
    pub video_gen_model: String,
    pub blocked_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: u64,
    pub session_id: String,
    pub session_title: String,
    pub model: String,
    pub backend: String,
    /// "tool" while a tool runs, "streaming" otherwise. Tasks only live in
    /// the map while their loop is active, so there is no idle state.
    pub status: String,
    pub buffer_chars: usize,
}

impl App {
    /// Serde-shaped state for API consumers (the Phase 4 host). Sessions
    /// come from the picker cache when loaded, else a fresh db read — a
    /// failed read is an error, never a silently-empty session list.
    pub fn snapshot(&self) -> Result<CoreSnapshot> {
        let sessions = if self.sessions_cache.is_empty() {
            self.db
                .list_sessions(&self.active_space.id)
                .context("reading sessions for snapshot")?
        } else {
            self.sessions_cache.clone()
        };
        let favorite_ids = &self.favorites;
        Ok(CoreSnapshot {
            sessions: sessions
                .into_iter()
                .map(|s| SessionSnapshot {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    slug: s.slug.clone(),
                    model: s.model.clone(),
                    kind: s.kind.clone(),
                    web_mode: s.web_mode,
                    created_at: s.created_at.clone(),
                })
                .collect(),
            models: self
                .models
                .iter()
                .map(|m| ModelSnapshot {
                    id: super::composite_id(m),
                    name: m.name.clone(),
                    context_length: m.context_length,
                    favorite: favorite_ids.contains(&super::composite_id(m)),
                })
                .collect(),
            settings: SettingsSnapshot {
                model: self.current_model.clone(),
                verbosity: self.verbosity.clone(),
                web_mode: self.web_mode,
                incognito: self.incognito,
                searxng_url: self.searxng_url.clone(),
                langsearch_key: self.langsearch_key.clone(),
                search_provider: self.search_provider.clone(),
                temperature: self.settings.temperature,
                top_p: self.settings.top_p,
                max_tokens: self.settings.max_tokens,
                compact_threshold: self.settings.compact_threshold,
                memory_model: self.memory_model.clone(),
                transcriber_model: self.transcriber_model.clone(),
                ocr_model: self.ocr_model.clone(),
                ocr_engine: self.ocr_engine.clone(),
                embedding_model: self.embedding_model.clone(),
                image_gen_model: self.image_gen_model.clone(),
                video_gen_model: self.video_gen_model.clone(),
                blocked_domains: self.blocked_domains(),
            },
            tasks: self
                .chat_tasks
                .values()
                .map(|t| TaskSnapshot {
                    id: t.id,
                    session_id: t.session_id.clone(),
                    session_title: t.session_title.clone(),
                    model: t.model.clone(),
                    backend: t.backend.name().to_string(),
                    status: if t.tool_status.is_some() {
                        "tool".to_string()
                    } else {
                        "streaming".to_string()
                    },
                    buffer_chars: t.buffer.chars().count(),
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 5 clients (web/mobile/`--remote`) parse `CoreSnapshot` `JSON`
    /// — this golden string locks the wire shape so a field rename or
    /// reorder can never silently break a client. Update it deliberately
    /// when the shape changes.
    #[test]
    fn golden_json_locks_wire_shape() {
        let snap = CoreSnapshot {
            sessions: vec![SessionSnapshot {
                id: "s1".into(),
                title: "hello".into(),
                slug: Some("hello".into()),
                model: "openrouter:anthropic/claude-sonnet-4".into(),
                kind: "chat".into(),
                web_mode: false,
                created_at: "2025-01-01T00:00:00Z".into(),
            }],
            models: vec![ModelSnapshot {
                id: "openrouter:anthropic/claude-sonnet-4".into(),
                name: "Claude Sonnet 4".into(),
                context_length: Some(200_000),
                favorite: true,
            }],
            settings: SettingsSnapshot {
                model: Some("openrouter:anthropic/claude-sonnet-4".into()),
                verbosity: "high".into(),
                web_mode: false,
                incognito: false,
                searxng_url: String::new(),
                langsearch_key: String::new(),
                search_provider: "searxng".into(),
                temperature: Some(0.7),
                top_p: None,
                max_tokens: None,
                compact_threshold: 60,
                memory_model: String::new(),
                transcriber_model: String::new(),
                ocr_model: String::new(),
                ocr_engine: "router".into(),
                embedding_model: String::new(),
                image_gen_model: String::new(),
                video_gen_model: String::new(),
                blocked_domains: Vec::new(),
            },
            tasks: vec![TaskSnapshot {
                id: 1,
                session_id: "s1".into(),
                session_title: "hello".into(),
                model: "openrouter:anthropic/claude-sonnet-4".into(),
                backend: "OpenRouter".into(),
                status: "streaming".into(),
                buffer_chars: 12,
            }],
        };
        let json = serde_json::to_string(&snap).expect("snapshot serializes");
        assert_eq!(
            json,
            r#"{"sessions":[{"id":"s1","title":"hello","slug":"hello","model":"openrouter:anthropic/claude-sonnet-4","kind":"chat","web_mode":false,"created_at":"2025-01-01T00:00:00Z"}],"models":[{"id":"openrouter:anthropic/claude-sonnet-4","name":"Claude Sonnet 4","context_length":200000,"favorite":true}],"settings":{"model":"openrouter:anthropic/claude-sonnet-4","verbosity":"high","web_mode":false,"incognito":false,"searxng_url":"","langsearch_key":"","search_provider":"searxng","temperature":0.7,"top_p":null,"max_tokens":null,"compact_threshold":60,"memory_model":"","transcriber_model":"","ocr_model":"","ocr_engine":"router","embedding_model":"","image_gen_model":"","video_gen_model":"","blocked_domains":[]},"tasks":[{"id":1,"session_id":"s1","session_title":"hello","model":"openrouter:anthropic/claude-sonnet-4","backend":"OpenRouter","status":"streaming","buffer_chars":12}]}"#
        );
        // The golden string must also parse back into the same shape.
        let back: CoreSnapshot = serde_json::from_str(&json).expect("golden parses");
        assert_eq!(back.sessions[0].title, "hello");
        assert_eq!(back.sessions[0].slug.as_deref(), Some("hello"));
        assert_eq!(back.models[0].context_length, Some(200_000));
        assert_eq!(back.settings.temperature, Some(0.7));
        assert_eq!(back.settings.blocked_domains, Vec::<String>::new());
        assert_eq!(back.tasks[0].status, "streaming");
        assert_eq!(back.tasks[0].buffer_chars, 12);
    }
}
