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
    /// The session currently selected by the host actor. `None` means the
    /// next message will lazily create a new session.
    pub active_session_id: Option<String>,
    pub active_space_id: String,
    pub active_space_name: String,
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

/// A persisted transcript row exposed to thin clients. Provider credentials
/// and internal database identifiers are intentionally not part of this view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSnapshot {
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub tokens: Option<i64>,
    pub secs: Option<f64>,
    pub cost: Option<f64>,
    pub phrase: Option<String>,
    pub persona: Option<String>,
    pub created_at: Option<String>,
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
#[allow(clippy::struct_excessive_bools)] // Independent user preferences in the wire format.
pub struct SettingsSnapshot {
    #[serde(default)]
    pub show_stats: bool,
    #[serde(default)]
    pub show_reasoning: bool,
    #[serde(default)]
    pub hide_hints: bool,
    pub model: Option<String>,
    pub verbosity: String,
    pub web_mode: bool,
    pub incognito: bool,
    /// Sanitized `SearXNG` endpoint (credentials/query strings are removed).
    pub searxng_url: String,
    /// Whether the local `LangSearch` credential is configured. The key never
    /// crosses the host API boundary.
    #[serde(default)]
    pub langsearch_configured: bool,
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
    /// Partial answer buffer, allowing thin clients to recover after an SSE
    /// reconnect. It contains model output only, never credentials.
    #[serde(default)]
    pub buffer: String,
}

/// Remove URL userinfo, query, and fragment components before an endpoint is
/// exposed in a remote snapshot. Search endpoints are local configuration, not
/// credentials storage, but URLs commonly grow accidental `?token=` values.
fn sanitized_endpoint(value: &str) -> String {
    let value = value.trim();
    let value = value.split_once('#').map_or(value, |(prefix, _)| prefix);
    let value = value.split_once('?').map_or(value, |(prefix, _)| prefix);
    if let Some((scheme, authority)) = value.split_once("://")
        && let Some((_, host)) = authority.rsplit_once('@')
    {
        return format!("{scheme}://{host}");
    }
    value.to_string()
}

impl App {
    /// Load one session's transcript for thin clients such as the web UI.
    pub fn session_messages(&self, session_id: &str) -> Result<Vec<MessageSnapshot>> {
        self.db
            .load_messages(session_id)
            .context("reading session messages")
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|message| MessageSnapshot {
                        role: message.role,
                        content: message.content,
                        model: message.model,
                        reasoning: message.reasoning,
                        tokens: message.tokens,
                        secs: message.secs,
                        cost: message.cost,
                        phrase: message.phrase,
                        persona: message.persona,
                        created_at: message.created_at,
                    })
                    .collect()
            })
    }

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
            active_session_id: self.session.as_ref().map(|session| session.id.clone()),
            active_space_id: self.active_space.id.clone(),
            active_space_name: self.active_space.name.clone(),
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
                show_stats: self.settings.show_stats,
                show_reasoning: self.settings.show_reasoning,
                hide_hints: self.settings.hide_hints,
                model: self.current_model.clone(),
                verbosity: self.verbosity.clone(),
                web_mode: self.web_mode,
                incognito: self.incognito,
                searxng_url: sanitized_endpoint(&self.searxng_url),
                langsearch_configured: !self.langsearch_key.trim().is_empty(),
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
                    buffer: t.buffer.clone(),
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
            active_session_id: Some("s1".into()),
            active_space_id: "sp1".into(),
            active_space_name: "Default".into(),
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
                show_stats: false,
                show_reasoning: false,
                hide_hints: false,
                model: Some("openrouter:anthropic/claude-sonnet-4".into()),
                verbosity: "high".into(),
                web_mode: false,
                incognito: false,
                searxng_url: String::new(),
                langsearch_configured: false,
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
                buffer: "".into(),
            }],
        };
        let json = serde_json::to_string(&snap).expect("snapshot serializes");
        assert_eq!(
            json,
            r#"{"active_session_id":"s1","active_space_id":"sp1","active_space_name":"Default","sessions":[{"id":"s1","title":"hello","slug":"hello","model":"openrouter:anthropic/claude-sonnet-4","kind":"chat","web_mode":false,"created_at":"2025-01-01T00:00:00Z"}],"models":[{"id":"openrouter:anthropic/claude-sonnet-4","name":"Claude Sonnet 4","context_length":200000,"favorite":true}],"settings":{"show_stats":false,"show_reasoning":false,"hide_hints":false,"model":"openrouter:anthropic/claude-sonnet-4","verbosity":"high","web_mode":false,"incognito":false,"searxng_url":"","langsearch_configured":false,"search_provider":"searxng","temperature":0.7,"top_p":null,"max_tokens":null,"compact_threshold":60,"memory_model":"","transcriber_model":"","ocr_model":"","ocr_engine":"router","embedding_model":"","image_gen_model":"","video_gen_model":"","blocked_domains":[]},"tasks":[{"id":1,"session_id":"s1","session_title":"hello","model":"openrouter:anthropic/claude-sonnet-4","backend":"OpenRouter","status":"streaming","buffer_chars":12,"buffer":""}]}"#
        );
        // The golden string must also parse back into the same shape.
        let back: CoreSnapshot = serde_json::from_str(&json).expect("golden parses");
        assert_eq!(back.sessions[0].title, "hello");
        assert_eq!(back.sessions[0].slug.as_deref(), Some("hello"));
        assert_eq!(back.models[0].context_length, Some(200_000));
        assert_eq!(back.settings.temperature, Some(0.7));
        assert!(!back.settings.langsearch_configured);
        assert_eq!(back.settings.blocked_domains, Vec::<String>::new());
        assert_eq!(back.tasks[0].status, "streaming");
        assert_eq!(back.tasks[0].buffer_chars, 12);
    }

    #[test]
    fn sanitized_endpoint_drops_credentials_and_query() {
        assert_eq!(
            sanitized_endpoint("https://user:pass@example.test/search?token=secret#frag"),
            "https://example.test/search"
        );
        assert_eq!(
            sanitized_endpoint("http://localhost:8080"),
            "http://localhost:8080"
        );
    }
}
