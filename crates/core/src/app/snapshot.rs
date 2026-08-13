//! The serde-shaped view of app state the Phase 4 nexus host API will
//! consume: sessions, models, settings, tasks. Designed now so the wire
//! shape is stable before the API lands; the TUI does not consume this
//! (it reads fields directly until 2e).

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
    /// "streaming" while tokens flow, "tool" while a tool runs, else "idle".
    pub status: String,
    pub buffer_chars: usize,
}

impl App {
    /// Serde-shaped state for API consumers (the Phase 4 host). Sessions
    /// come from the picker cache when loaded, else a fresh db read.
    pub fn snapshot(&self) -> CoreSnapshot {
        let sessions = if self.sessions_cache.is_empty() {
            self.db
                .list_sessions(&self.active_space.id)
                .unwrap_or_default()
        } else {
            self.sessions_cache.clone()
        };
        let favorite_ids = &self.favorites;
        CoreSnapshot {
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
                    backend: format!("{:?}", t.backend),
                    status: if t.tool_status.is_some() {
                        "tool".to_string()
                    } else {
                        "streaming".to_string()
                    },
                    buffer_chars: t.buffer.chars().count(),
                })
                .collect(),
        }
    }
}
