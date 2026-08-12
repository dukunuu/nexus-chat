//! Tools the model can call mid-response, advertised as nine consolidated
//! names (`batch`, `skills`, `scripts`, `search`, `fetch_url`,
//! `research_lookup`, `files`, `app`, `media`) that dispatch onto a larger
//! set of specialized implementations below. Concrete (no trait) —
//! there's exactly one implementation and no need for one yet.

use std::fmt::Write as _;
use std::path::PathBuf;

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::provider::ToolDef;
use crate::provider::openrouter::{OpenRouter, normalize_video_params};
use crate::skills::{load_skills, skill_body};

const MAX_TOOL_RESULT_CHARS: usize = 8_000;
/// Maximum sub-operations in one `batch` call — bounds execution time and
/// the size of the combined result.
const MAX_BATCH_CALLS: usize = 8;
/// Combined-result cap for a `batch` call. Each sub-result is already capped
/// at `MAX_TOOL_RESULT_CHARS`; this lets a few of them through together
/// while still bounding what a runaway batch pushes into the conversation.
const MAX_BATCH_RESULT_CHARS: usize = MAX_TOOL_RESULT_CHARS * 4;

pub struct ToolBox {
    pub skills_dir: PathBuf,
    /// Base URL of a `SearXNG` instance (e.g. `http://localhost:8080`), no
    /// trailing slash. Free and self-hosted — no API key needed.
    pub searxng_url: Option<String>,
    /// `LangSearch` API key (free tier, no card): <https://langsearch.com/dashboard>
    pub langsearch_key: Option<String>,
    /// Which backend `search(mode=web)` prefers: "auto" (`LangSearch`, then `SearXNG`,
    /// `DuckDuckGo`, and Brave), or an explicit "langsearch"/"searxng"/"duckduckgo".
    pub search_provider: String,
    /// When true, `defs()`/`run()` restrict to `search`/`fetch_url` only —
    /// used for deep-research searcher agents, which must never reach
    /// `scripts`/`app`/`media` tools even if hallucinated.
    research_only: bool,
    /// Domains a per-space setting always excludes from `search(mode=web)` results
    /// (appended to any `exclude_domains` the model passes).
    pub blocked_domains: Vec<String>,
    /// Main db path for tool connections. Connections open the db with its
    /// sibling `cache.db` attached (`open_attached`), so both durable tables
    /// (`session_sources`, `citations`, `files`) and device-local ones
    /// (`web_cache`, `file_chunks`, `model_prices`) resolve on one
    /// connection. `None` disables the cache-backed tools (some tests).
    db_path: Option<PathBuf>,
    /// Set for follow-up turns inside a `/research` session: enables the
    /// `research_lookup(scope=session_sources)` over that session's gathered source bundle.
    research_session_id: Option<String>,
    client: reqwest::Client,
    files: Option<FilesCtx>,
    apps: Option<AppsCtx>,
    /// Whether the current model supports image inputs. When false, tool
    /// image results are returned as text references instead of being
    /// injected as vision content (which would cause a 400).
    pub supports_images: bool,
    /// When true, `fetch_cached` never hits the network on a cache miss —
    /// used for the Verifier stage's quote-checking pass, which must only
    /// ever see pages the searchers actually gathered, never fresh fetches.
    cache_only: bool,
    /// Provider + model for AI image generation. `None` = tool disabled.
    pub image_gen_backend: Option<(OpenRouter, String)>,
    /// Directory to save generated images into / search for reference images.
    pub space_files_dir: PathBuf,
    pub space_apps_dir: PathBuf,
    /// Directory holding space-local scripts (created by the model via the
    /// `scripts` tool).
    pub space_scripts_dir: PathBuf,
    /// Current session id — for attaching generated images to a message.
    pub session_id: String,
    /// Provider + model for video generation. `None` = tools hidden.
    pub video_gen_backend: Option<(OpenRouter, String)>,
}

/// Where the file tools read from: the shared db plus the space to scope to.
/// The toolbox opens its own short-lived connection per call — the app's
/// `Db` handle stays on the UI task and is never shared with the stream task.
pub struct FilesCtx {
    pub db_path: std::path::PathBuf,
    pub space_id: String,
    /// (provider, embedding model) for semantic search; None = keyword only.
    pub embedder: Option<(crate::provider::openrouter::OpenRouter, String)>,
}

/// Where the app tools write: the active space's apps dir, plus the
/// registry, server port and space metadata. Only present while the server
/// runs.
pub struct AppsCtx {
    pub dir: PathBuf,
    pub server_port: u16,
    pub registry: crate::appserver::AppRegistry,
    pub space_name: String,
    pub space_id: String,
    pub space_db_path: PathBuf,
    pub files_dir: PathBuf,
    pub session_id: String,
}

fn tool_def(name: &str, description: &str, parameters: serde_json::Value) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
    }
}

fn required_arg(v: &serde_json::Value, key: &str) -> Result<(), String> {
    match v.get(key).and_then(|value| value.as_str()) {
        Some(value) if !value.trim().is_empty() => Ok(()),
        _ => Err(format!("missing required field: {key}")),
    }
}

fn required_array(v: &serde_json::Value, key: &str) -> Result<(), String> {
    match v.get(key).and_then(|value| value.as_array()) {
        Some(values) if !values.is_empty() => Ok(()),
        _ => Err(format!("missing required field: {key}")),
    }
}

// Long by design (tool dispatch).
#[allow(clippy::too_many_lines)]
/// Normalize advertised consolidated calls onto the specialized implementations below.
/// The retired names (`skill_admin`, `app_inspect`, `run_python`, …) are intentionally
/// still accepted by `run()` for persisted/replayed calls, but never returned by `defs()`.
fn public_call(name: &str, args: &str) -> Result<(String, String), String> {
    let public = matches!(
        name,
        "skills"
            | "scripts"
            | "search"
            | "research_lookup"
            | "files"
            | "app"
            | "media"
            | "skill_admin"
            | "app_inspect"
            | "app_modify"
            | "app_assets"
            | "script_files"
            | "video_transform"
            | "video_references"
    );
    if !public {
        return Ok((name.to_string(), args.to_string()));
    }
    let value: serde_json::Value =
        serde_json::from_str(args).map_err(|e| format!("invalid tool arguments: {e}"))?;
    let action = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("missing required field: {key}"))
    };
    let mapped = match name {
        "skills" => match action("action")? {
            "load" => {
                required_arg(&value, "name")?;
                "skill"
            }
            "create" => {
                required_arg(&value, "name")?;
                required_arg(&value, "description")?;
                "create_skill"
            }
            "install" => {
                required_arg(&value, "source")?;
                "install_skill"
            }
            other => return Err(format!("invalid action for skills: {other}")),
        },
        "scripts" => match action("action")? {
            "list" => "list_scripts",
            "read" => {
                required_arg(&value, "path")?;
                "read_script"
            }
            "write" => {
                required_arg(&value, "path")?;
                if value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "write_script"
            }
            "edit" => {
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_script"
            }
            "run" => {
                required_arg(&value, "path")?;
                "run_script"
            }
            "python" => {
                required_arg(&value, "code")?;
                required_arg(&value, "name")?;
                "run_python"
            }
            "install" => {
                required_array(&value, "packages")?;
                "install_packages"
            }
            other => return Err(format!("invalid action for scripts: {other}")),
        },
        "skill_admin" => match action("action")? {
            "create" => {
                required_arg(&value, "name")?;
                required_arg(&value, "description")?;
                "create_skill"
            }
            "install" => {
                required_arg(&value, "source")?;
                "install_skill"
            }
            other => return Err(format!("invalid action for skill_admin: {other}")),
        },
        "search" => match action("mode")? {
            "web" => {
                required_arg(&value, "query")?;
                "web_search"
            }
            "academic" => {
                required_arg(&value, "query")?;
                "academic_search"
            }
            "discussion" => {
                required_arg(&value, "query")?;
                "discussion_search"
            }
            other => return Err(format!("invalid mode for search: {other}")),
        },
        "research_lookup" => match action("scope")? {
            "session_sources" => {
                required_arg(&value, "query")?;
                "search_sources"
            }
            "citations" => "list_citations",
            other => return Err(format!("invalid scope for research_lookup: {other}")),
        },
        "files" => match action("action")? {
            "search" => {
                required_arg(&value, "query")?;
                "search_files"
            }
            "read" => {
                required_arg(&value, "name")?;
                "read_file"
            }
            "pdf_page" => {
                required_arg(&value, "name")?;
                if value
                    .get("page")
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
                {
                    return Err("missing required field: page".to_string());
                }
                "read_pdf_page"
            }
            other => return Err(format!("invalid action for files: {other}")),
        },
        "app" => match action("action")? {
            "read" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                "read_app_file"
            }
            "search" => {
                required_arg(&value, "app")?;
                required_arg(&value, "pattern")?;
                "grep_app"
            }
            "write" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_arg(&value, "content")?;
                "write_file"
            }
            "patch" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_file"
            }
            "diff" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                if value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "diff_app"
            }
            "list" => "list_images",
            "copy_file" => {
                required_arg(&value, "app")?;
                required_arg(&value, "file_name")?;
                "copy_file_to_app"
            }
            "copy_images" => {
                required_arg(&value, "app")?;
                required_array(&value, "image_ids")?;
                "copy_images_to_app"
            }
            other => return Err(format!("invalid action for app: {other}")),
        },
        "app_inspect" => match action("action")? {
            "read" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                "read_app_file"
            }
            "search" => {
                required_arg(&value, "app")?;
                required_arg(&value, "pattern")?;
                "grep_app"
            }
            other => return Err(format!("invalid action for app_inspect: {other}")),
        },
        "app_modify" => match action("action")? {
            "write" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_arg(&value, "content")?;
                "write_file"
            }
            "patch" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_file"
            }
            "diff" => {
                required_arg(&value, "app")?;
                required_arg(&value, "path")?;
                if value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "diff_app"
            }
            other => return Err(format!("invalid action for app_modify: {other}")),
        },
        "app_assets" => match action("action")? {
            "list" => "list_images",
            "copy_file" => {
                required_arg(&value, "app")?;
                required_arg(&value, "file_name")?;
                "copy_file_to_app"
            }
            "copy_images" => {
                required_arg(&value, "app")?;
                required_array(&value, "image_ids")?;
                "copy_images_to_app"
            }
            other => return Err(format!("invalid action for app_assets: {other}")),
        },
        "script_files" => match action("action")? {
            "list" => "list_scripts",
            "write" => {
                required_arg(&value, "path")?;
                if value.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err("missing required field: content".to_string());
                }
                "write_script"
            }
            "read" => {
                required_arg(&value, "path")?;
                "read_script"
            }
            "edit" => {
                required_arg(&value, "path")?;
                required_array(&value, "edits")?;
                "edit_script"
            }
            other => return Err(format!("invalid action for script_files: {other}")),
        },
        "media" => match action("action")? {
            "generate_image" => {
                required_arg(&value, "prompt")?;
                "generate_image"
            }
            "generate_video" => {
                required_arg(&value, "prompt")?;
                "generate_video"
            }
            "edit" => {
                required_arg(&value, "video_id")?;
                "edit_video"
            }
            "extract_frame" => {
                required_arg(&value, "video_id")?;
                "extract_frame"
            }
            "stitch" => {
                required_array(&value, "video_ids")?;
                "stitch_videos"
            }
            "save_reference" => {
                required_arg(&value, "name")?;
                required_arg(&value, "image_id")?;
                required_arg(&value, "description")?;
                "save_reference"
            }
            "list_references" => "list_references",
            "delete_reference" => {
                required_arg(&value, "name")?;
                "delete_reference"
            }
            other => return Err(format!("invalid action for media: {other}")),
        },
        "video_transform" => match action("action")? {
            "edit" => {
                required_arg(&value, "video_id")?;
                "edit_video"
            }
            "extract_frame" => {
                required_arg(&value, "video_id")?;
                "extract_frame"
            }
            "stitch" => {
                required_array(&value, "video_ids")?;
                "stitch_videos"
            }
            other => return Err(format!("invalid action for video_transform: {other}")),
        },
        "video_references" => match action("action")? {
            "save" => {
                required_arg(&value, "name")?;
                required_arg(&value, "image_id")?;
                required_arg(&value, "description")?;
                "save_reference"
            }
            "list" => "list_references",
            "delete" => {
                required_arg(&value, "name")?;
                "delete_reference"
            }
            other => return Err(format!("invalid action for video_references: {other}")),
        },
        _ => unreachable!(),
    };
    Ok((mapped.to_string(), args.to_string()))
}

impl ToolBox {
    /// All config knobs; kept flat because ~17 test call sites construct
    /// this with inline `None`/default args — a config struct would churn
    /// every one of them for no readability gain.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        skills_dir: PathBuf,
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        blocked_domains: Vec<String>,
        db_path: Option<PathBuf>,
        files: Option<FilesCtx>,
        apps: Option<AppsCtx>,
    ) -> Self {
        Self {
            skills_dir,
            searxng_url,
            langsearch_key,
            search_provider,
            research_only: false,
            blocked_domains,
            db_path,
            research_session_id: None,
            client: reqwest::Client::new(),
            files,
            apps,
            cache_only: false,
            image_gen_backend: None,
            supports_images: false,
            space_files_dir: PathBuf::new(),
            space_apps_dir: PathBuf::new(),
            space_scripts_dir: PathBuf::new(),
            session_id: String::new(),
            video_gen_backend: None,
        }
    }

    /// A toolbox restricted to `search`/`fetch_url` — for deep-research
    /// searcher agents, which get no filesystem/app/script access.
    pub fn research(
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        blocked_domains: Vec<String>,
        db_path: Option<PathBuf>,
    ) -> Self {
        let mut tb = Self::new(
            PathBuf::new(),
            searxng_url,
            langsearch_key,
            search_provider,
            blocked_domains,
            db_path,
            None,
            None,
        );
        tb.research_only = true;
        tb
    }

    /// Attach a research session id, enabling `research_lookup` for follow-up
    /// turns in that session's chat. Also merges any domains the user has
    /// discarded in this session into `blocked_domains`, so a later
    /// `search`/`fetch_url` call excludes them the same way the global
    /// setting does.
    pub fn with_research_session(mut self, session_id: String) -> Self {
        if let Some(db_path) = &self.db_path
            && let Ok(conn) = rusqlite::Connection::open(db_path)
            && let Ok(hosts) = crate::db::discarded_domains(&conn, &session_id)
        {
            self.blocked_domains.extend(hosts);
        }
        self.research_session_id = Some(session_id);
        self
    }

    /// Restrict `fetch_url` to serving from `web_cache` only — a cache miss
    /// returns `[not cached]` instead of fetching. Used for the Verifier's
    /// quote-checking pass (Task 8).
    pub const fn cache_only(mut self) -> Self {
        self.cache_only = true;
        self
    }

    fn files_count(&self) -> u64 {
        let Some(ctx) = &self.files else { return 0 };
        rusqlite::Connection::open(&ctx.db_path)
            .ok()
            .and_then(|conn| crate::db::count_files(&conn, &ctx.space_id).ok())
            .unwrap_or(0)
    }

    fn citation_count(&self) -> u64 {
        let Some(ctx) = &self.files else { return 0 };
        rusqlite::Connection::open(&ctx.db_path)
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM citations WHERE space_id = ?1",
                    [&ctx.space_id],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
            })
            .unwrap_or(0)
            .max(0)
            .unsigned_abs()
    }

    /// Resolve which backend to actually use for this call. An explicit
    /// choice ("langsearch"/"searxng"/"duckduckgo") is used as-is — if it's
    /// not configured, that's a clear error rather than a silent swap to
    /// something else the user didn't pick. "auto" (the default) tries
    /// `LangSearch`, `SearXNG`, `DuckDuckGo`, and finally Brave, continuing past
    /// transport errors, bot challenges, and empty result pages.
    async fn search(
        &self,
        query: &str,
        recency: Option<&str>,
        include_domains: &[String],
        exclude_domains: &[String],
    ) -> anyhow::Result<Vec<SearchHit>> {
        let query = rewrite_query_with_domains(query, include_domains, exclude_domains);
        match self.search_provider.as_str() {
            "langsearch" => match &self.langsearch_key {
                Some(key) => langsearch_search(&self.client, key, &query, recency).await,
                None => anyhow::bail!("LangSearch selected but no API key is configured"),
            },
            "searxng" => match &self.searxng_url {
                Some(url) => searxng_search(&self.client, url, &query, recency).await,
                None => anyhow::bail!("SearXNG selected but no instance URL is configured"),
            },
            "duckduckgo" => duckduckgo_search(&self.client, &query).await,
            // Auto mode is deliberately failover-based. A configured hosted
            // backend can still be unavailable (for example, LangSearch's
            // API host has occasionally had TLS/DNS problems), and a single
            // outage should not turn an otherwise usable search tool into an
            // error.
            _ => {
                let mut failures = Vec::new();

                if let Some(key) = &self.langsearch_key {
                    match langsearch_search(&self.client, key, &query, recency).await {
                        Ok(hits) if !hits.is_empty() => return Ok(hits),
                        Ok(_) => failures.push("LangSearch returned no results".to_string()),
                        Err(error) => failures.push(format!("LangSearch: {error}")),
                    }
                }
                if let Some(url) = &self.searxng_url {
                    match searxng_search(&self.client, url, &query, recency).await {
                        Ok(hits) if !hits.is_empty() => return Ok(hits),
                        Ok(_) => failures.push("SearXNG returned no results".to_string()),
                        Err(error) => failures.push(format!("SearXNG: {error}")),
                    }
                }

                match duckduckgo_search(&self.client, &query).await {
                    Ok(hits) if !hits.is_empty() => return Ok(hits),
                    Ok(_) => failures.push("DuckDuckGo returned no results".to_string()),
                    Err(error) => failures.push(format!("DuckDuckGo: {error}")),
                }

                // HTML search endpoints increasingly return bot-challenge
                // pages with a successful HTTP status. Treat an empty parse as
                // a backend failure and give auto mode one more independent,
                // keyless search source before reporting no results.
                match brave_search(&self.client, &query).await {
                    Ok(hits) => Ok(hits),
                    Err(error) => {
                        failures.push(format!("Brave: {error}"));
                        anyhow::bail!("all web search backends failed: {}", failures.join("; "))
                    }
                }
            }
        }
    }

    /// Fetch through the cache: serve a fresh (<24h) cached copy unless
    /// `force_fresh`, else live-fetch and write through. Cache read/write
    /// failures degrade to a live fetch — a broken db must never block a
    /// tool call.
    async fn fetch_cached(&self, url: &str, force_fresh: bool) -> anyhow::Result<String> {
        let url_norm = normalize_url(url);
        if !force_fresh
            && let Some(db_path) = &self.db_path
            && let Ok(conn) = crate::db::open_attached(db_path)
            && let Ok(Some((_, text, fetched_at))) = crate::db::cache_get(&conn, &url_norm)
            && crate::db::is_fresh(&fetched_at, chrono::Utc::now())
        {
            return Ok(text);
        }
        if self.cache_only {
            return Ok("[not cached]".to_string());
        }
        let text = if is_youtube_url(url) {
            fetch_youtube_transcript(&self.client, url).await?
        } else {
            fetch_url_text(&self.client, url).await?
        };
        if let Some(db_path) = &self.db_path
            && let Ok(conn) = crate::db::open_attached(db_path)
        {
            let _ = crate::db::cache_put(&conn, &url_norm, url, None, &text);
        }
        Ok(text)
    }

    /// Tool definitions to attach to the request, or empty to send a request
    /// identical to one from before tool-calling existed (keeps models that
    /// don't support tools working unchanged). `search` always works —
    /// it prefers configured API backends, then uses keyless HTML fallbacks
    /// when those are unavailable, so it needs no setup.
    // Long by design (tool-definition table).
    #[allow(clippy::too_many_lines)]
    pub fn defs(&self) -> Vec<ToolDef> {
        let mut defs = Vec::new();
        defs.push(tool_def(
            "batch",
            "Run several independent tool operations in ONE call — multiple searches, multiple file\
             searches/reads, or multiple app/script writes. Every result comes back in a single\
             round-trip, each labeled [n/N]. Prefer this over calling tools one by one whenever you\
             need several operations at once. Sub-calls use the same public tool names and\
             parameters as normal calls. Never nest batch inside batch; keep dependent steps (e.g.\
             write then edit the same file) as separate calls.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "calls": {
                        "type": "array",
                        "description": "up to 8 operations, run in order",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": { "type": "string", "description": "public tool name, e.g. search, fetch_url, files, app, scripts, research_lookup, skills" },
                                "arguments": { "type": "object", "description": "that tool's parameters, same shape as a normal call" }
                            },
                            "required": ["tool"]
                        }
                    }
                },
                "required": ["calls"]
            }),
        ));
        let has_skills = !load_skills(&self.skills_dir).is_empty();
        let mut skills_actions = vec!["create", "install"];
        if has_skills {
            skills_actions.insert(0, "load");
        }
        defs.push(tool_def(
            "skills",
            "Manage reusable skills. action=load returns a skill's full instructions (SKILL.md by default, or a specific file within it); action=create makes a new skill from name/description/body; action=install fetches one from GitHub.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": skills_actions },
                    "name": { "type": "string", "description": "skill name for load/create" },
                    "file": { "type": "string", "description": "optional path within the skill for load; defaults to SKILL.md" },
                    "description": { "type": "string", "description": "short description for create" },
                    "body": { "type": "string", "description": "skill instructions for create" },
                    "overwrite": { "type": "boolean", "description": "replace an existing skill (default false)" },
                    "source": { "type": "string", "description": "GitHub owner/repo/path for install" }
                },
                "required": ["action"]
            }),
        ));
        defs.push(tool_def(
            "scripts",
            "Everything script-related in one tool. action=list/read/write/edit manage files inside the space scripts directory (confined paths, hash-line editing); action=run executes an existing skill or space script; action=python writes and runs inline Python in the space scripts environment (persists unless temporary=true); action=install adds packages to a skill virtualenv, an app's npm dependencies, or the shared space-script Python virtualenv (at most one target).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "read", "write", "edit", "run", "python", "install"] },
                    "path": { "type": "string", "description": "script path relative to the scripts dir" },
                    "content": { "type": "string", "description": "complete script content for write" },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer", "description": "maximum 200 lines" },
                    "edits": { "type": "array", "items": { "type": "object", "properties": { "hash": { "type": "string" }, "new": { "type": ["string", "null"] } }, "required": ["hash"] } },
                    "code": { "type": "string", "description": "Python source for python" },
                    "name": { "type": "string", "description": "confined .py filename for python" },
                    "temporary": { "type": "boolean", "description": "delete the script after python runs (default false)" },
                    "skill": { "type": "string", "description": "skill name for run (unless space=true), or pip target for install" },
                    "space": { "type": "boolean", "description": "run from the space scripts directory" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "command-line arguments for run/python" },
                    "packages": { "type": "array", "items": { "type": "string" }, "description": "packages for install" },
                    "app": { "type": "string", "description": "app npm target for install" }
                },
                "required": ["action"]
            }),
        ));
        defs.push(tool_def(
            "search",
            "Search the web, scholarly literature, or HN/Reddit discussions. mode=web uses the configured web backend; academic preserves Semantic Scholar metadata; discussion preserves HN and Reddit engagement metadata.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["web", "academic", "discussion"] },
                    "query": { "type": "string" },
                    "recency": { "type": "string", "enum": ["day", "week", "month", "year"] },
                    "include_domains": { "type": "array", "items": { "type": "string" } },
                    "exclude_domains": { "type": "array", "items": { "type": "string" } },
                    "limit": { "type": "integer", "description": "maximum academic results (default 10, max 20)" }
                },
                "required": ["mode", "query"]
            }),
        ));
        defs.push(tool_def(
            "fetch_url",
            "Fetch an arbitrary URL as readable, paged text. Uses the 24-hour cache unless fresh=true; supports PDFs, YouTube transcripts, and research verifier cache-only mode.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer", "description": "maximum 200 lines" },
                    "fresh": { "type": "boolean" }
                },
                "required": ["url"]
            }),
        ));
        let has_session_sources = self.research_session_id.is_some();
        let has_citations = self.citation_count() > 0;
        if has_session_sources || has_citations {
            let mut scopes = Vec::new();
            if has_session_sources {
                scopes.push("session_sources");
            }
            if has_citations {
                scopes.push("citations");
            }
            defs.push(tool_def(
                "research_lookup",
                "Look up previously gathered research material. scope=session_sources searches this research session's source bundle; scope=citations searches citations saved across this space.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scope": { "type": "string", "enum": scopes },
                        "query": { "type": "string", "description": "keywords; optional for citations, required for session_sources" }
                    },
                    "required": ["scope"]
                }),
            ));
        }
        if self.files_count() > 0 {
            defs.push(tool_def(
                "files",
                "Work with imported space files. action=search performs semantic/keyword search, read pages extracted text, and pdf_page returns an imported PDF page image when available.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["search", "read", "pdf_page"] },
                        "query": { "type": "string", "description": "search query" },
                        "name": { "type": "string", "description": "imported file name" },
                        "offset": { "type": "integer" },
                        "limit": { "type": "integer", "description": "maximum 200 lines" },
                        "page": { "type": "integer", "description": "1-based PDF page" }
                    },
                    "required": ["action"]
                }),
            ));
        }
        if self.apps.is_some() {
            defs.push(tool_def(
                "app",
                "Build and manage locally served web apps. action=read returns hash-lines for safe editing and action=search greps non-ignored files; action=write replaces complete content, action=patch applies hash-line edits with stale-hash rejection, and action=diff previews a complete candidate without writing; action=list shows conversation/space images and action=copy_file/copy_images bring user data into an app (images go to _images/, text files to the app KV store).",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["read", "search", "write", "patch", "diff", "list", "copy_file", "copy_images"] },
                        "app": { "type": "string", "description": "app name or UUID" },
                        "path": { "type": "string", "description": "file path within the app" },
                        "pattern": { "type": "string", "description": "case-insensitive search text for search" },
                        "content": { "type": "string", "description": "complete content for write or diff" },
                        "edits": { "type": "array", "description": "hash-line edits for patch", "items": { "type": "object", "properties": { "hash": { "type": "string" }, "new": { "type": ["string", "null"] } }, "required": ["hash"] } },
                        "file_name": { "type": "string", "description": "imported file name for copy_file" },
                        "image_ids": { "type": "array", "items": { "type": "string" }, "description": "image IDs for copy_images" },
                        "offset": { "type": "integer" },
                        "limit": { "type": "integer", "description": "maximum 200 lines" },
                        "compact": { "type": "boolean", "description": "return locations only for search (default true)" }
                    },
                    "required": ["action"]
                }),
            ));
        }
        let mut media_actions = Vec::new();
        if self.image_gen_backend.is_some() {
            media_actions.push("generate_image");
        }
        if self.video_gen_backend.is_some() {
            media_actions.extend([
                "generate_video",
                "edit",
                "extract_frame",
                "stitch",
                "save_reference",
                "list_references",
                "delete_reference",
            ]);
        }
        if !media_actions.is_empty() {
            defs.push(tool_def(
                "media",
                "Generate and transform media. action=generate_image makes an image from a prompt, optionally using a pasted image ID as a reference; action=generate_video makes a video from text and optional frame/reference images; action=edit/extract_frame/stitch transform videos locally with ffmpeg (effects, frame extraction, clip concatenation); action=save_reference/list_references/delete_reference manage named image references used for video consistency.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": media_actions },
                        "prompt": { "type": "string" },
                        "image_id": { "type": "string", "description": "reference image for generate_image, or image being saved for save_reference" },
                        "size": { "type": "string", "default": "1024x1024" },
                        "duration": { "type": "integer" },
                        "resolution": { "type": "string" },
                        "aspect_ratio": { "type": "string" },
                        "generate_audio": { "type": "boolean" },
                        "first_frame_id": { "type": "string" },
                        "last_frame_id": { "type": "string" },
                        "ref_image_id": { "type": "string" },
                        "character_refs": { "type": "array", "items": { "type": "string" } },
                        "location_refs": { "type": "array", "items": { "type": "string" } },
                        "seed": { "type": "integer" },
                        "source_video_id": { "type": "string" },
                        "video_id": { "type": "string" },
                        "video_ids": { "type": "array", "items": { "type": "string" } },
                        "lighting": { "type": "string", "enum": ["noir", "warm", "cold", "vintage", "vivid", "bleach_bypass"] },
                        "camera_move": { "type": "string", "enum": ["dolly_in", "dolly_out", "pan_left", "pan_right", "tilt_up", "tilt_down"] },
                        "intensity": { "type": "number" },
                        "speed": { "type": "number" },
                        "trim_start": { "type": "number" },
                        "trim_end": { "type": "number" },
                        "remove_audio": { "type": "boolean" },
                        "time_sec": { "type": "number" },
                        "format": { "type": "string", "enum": ["png", "jpg"] },
                        "name": { "type": "string", "description": "reference name for save_reference/delete_reference" },
                        "description": { "type": "string", "description": "reference description for save_reference" }
                    },
                    "required": ["action"]
                }),
            ));
        }
        if self.research_only {
            defs.retain(|d| matches!(d.name.as_str(), "search" | "fetch_url"));
        }
        defs
    }

    /// Resolve an app name or UUID to `(uuid, app_dir)`. If a name is given and
    /// not yet registered, a new UUID is assigned. Returns Err if the app name
    /// is invalid (not allowed by `resolve_confined` constraints).
    fn resolve_app(&self, name_or_uuid: &str) -> Result<(String, PathBuf), String> {
        let ctx = self.apps.as_ref().ok_or("apps are not available")?;
        let (uuid, app_name) = if looks_like_uuid(name_or_uuid) {
            let entry = ctx
                .registry
                .lookup(name_or_uuid)
                .ok_or_else(|| format!("unknown app uuid: {name_or_uuid}"))?;
            (name_or_uuid.to_string(), entry.name)
        } else if name_or_uuid.is_empty()
            || name_or_uuid.contains(['/', '\\'])
            || name_or_uuid == "."
            || name_or_uuid == ".."
        {
            return Err(format!("invalid app name: {name_or_uuid:?}"));
        } else {
            let uuid = match ctx.registry.resolve(&ctx.space_name, name_or_uuid) {
                Some(u) => u,
                None => ctx.registry.assign(&ctx.space_name, name_or_uuid),
            };
            (uuid, name_or_uuid.to_string())
        };
        let app_dir = ctx.dir.join(&app_name);
        Ok((uuid, app_dir))
    }

    /// The live URL for an app (accepts a UUID).
    fn app_link(&self, uuid: &str) -> String {
        match &self.apps {
            Some(ctx) => format!("live at http://127.0.0.1:{}/{}/", ctx.server_port, uuid),
            None => String::new(),
        }
    }

    /// Run a tool by name. Returns `(result text sent back to the model,
    /// status label shown in the UI while it runs)`.
    // Long by design: the model tool dispatch (each arm is one tool).
    #[allow(clippy::too_many_lines)]
    pub async fn run(&self, name: &str, args: &str) -> (String, String) {
        if self.research_only
            && !matches!(
                name,
                "search" | "fetch_url" | "web_search" | "academic_search" | "discussion_search"
            )
        {
            return (
                format!("tool '{name}' is not available in research mode"),
                "blocked".to_string(),
            );
        }
        let (dispatch_name, dispatch_args) = match public_call(name, args) {
            Ok(call) => call,
            Err(error) => return (cap_tool_result(error), "invalid arguments".to_string()),
        };
        let name = dispatch_name.as_str();
        let args = dispatch_args.as_str();
        let (result, status) = match name {
            "skill" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let skill_name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let file = v
                    .get("file")
                    .and_then(|f| f.as_str())
                    .filter(|f| !f.is_empty())
                    .unwrap_or("SKILL.md");
                let status = format!("Reading {skill_name}/{file}…");
                let result = match resolve_confined(&self.skills_dir, &skill_name, file) {
                    Err(e) => e,
                    Ok(path) if !path.is_file() => format!("no such file: {skill_name}/{file}"),
                    Ok(path) => {
                        let text = match std::fs::read_to_string(&path) {
                            Ok(t) => t,
                            Err(e) => format!("error reading {skill_name}/{file}: {e}"),
                        };
                        if file == "SKILL.md" {
                            skill_body(&text).to_string()
                        } else {
                            text
                        }
                    }
                };
                (result, status)
            }
            "install_skill" => {
                let source = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("source").and_then(|s| s.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = format!("Installing skill {source}…");
                let result = match crate::skills::parse_gh_shorthand(&source) {
                    None => format!("invalid source {source:?} — expected owner/repo/path"),
                    Some((owner, repo, path)) => {
                        match crate::skills::install_from_github(
                            &self.client,
                            &owner,
                            &repo,
                            &path,
                            &self.skills_dir,
                        )
                        .await
                        {
                            Ok(name) => {
                                format!("installed skill '{name}' — load it with the skill tool")
                            }
                            Err(e) => format!("install failed: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "create_skill" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let body = v
                    .get("body")
                    .and_then(|x| x.as_str())
                    .filter(|b| !b.is_empty());
                let overwrite = v
                    .get("overwrite")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let status = format!("Creating skill {name}…");
                let result = if name.is_empty() {
                    "name must not be empty".to_string()
                } else if name.contains('/') || name.contains('\\') || name.contains("..") {
                    "name must not contain /, \\, or ..".to_string()
                } else if description.is_empty() {
                    "description must not be empty".to_string()
                } else {
                    let dir = self.skills_dir.join(&name);
                    let existed = dir.exists();
                    if existed && !overwrite {
                        format!("skill '{name}' already exists — set overwrite=true to replace")
                    } else if let Err(e) = std::fs::create_dir_all(&dir) {
                        format!("cannot create skill dir: {e}")
                    } else {
                        let body = body.unwrap_or("Write the skill instructions here. The model sees this text when it loads the skill.");
                        let md = format!(
                            "---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"
                        );
                        match std::fs::write(dir.join("SKILL.md"), &md) {
                            Ok(()) => {
                                let verb = if existed { "updated" } else { "created" };
                                format!("{verb} skill '{name}' — load it with the skill tool")
                            }
                            Err(e) => format!("cannot write SKILL.md: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "run_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let is_space = v
                    .get("space")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let (skill, script) = (field("skill"), field("path"));
                let extra: Vec<String> = v
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let status = if is_space {
                    format!("Running space script {script}…")
                } else {
                    format!("Running {skill}/{script}…")
                };
                let result = if is_space {
                    let file = self.space_scripts_dir.join(&script);
                    if !valid_relative_path(&script) || !file.starts_with(&self.space_scripts_dir) {
                        format!("invalid path: {script}")
                    } else if !file.is_file() {
                        format!("no such script: {script}")
                    } else {
                        let dir = self.space_scripts_dir.clone();
                        let ext = file
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let run = async {
                            let mut cmd: Vec<std::ffi::OsString> = Vec::new();
                            let program: std::ffi::OsString = match ext.as_str() {
                                "py" => {
                                    let py = ensure_venv(&dir).await?;
                                    cmd.push(file.clone().into());
                                    py.into()
                                }
                                "sh" | "bash" => {
                                    cmd.push(file.clone().into());
                                    "bash".into()
                                }
                                "js" | "mjs" => {
                                    cmd.push(file.clone().into());
                                    "node".into()
                                }
                                _ => file.clone().into(),
                            };
                            cmd.extend(extra.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                            let files_dir = self.space_files_dir.to_string_lossy().to_string();
                            let apps_dir = self.space_apps_dir.to_string_lossy().to_string();
                            let scripts_dir = self.space_scripts_dir.to_string_lossy().to_string();
                            if ext == "py" {
                                let pp_dir = dir.join("scripts");
                                let pp = pp_dir.to_string_lossy().to_string();
                                run_cmd_env(
                                    &program,
                                    &refs,
                                    &dir,
                                    120,
                                    &[
                                        ("SPACE_FILES_DIR", files_dir.as_str()),
                                        ("SPACE_APPS_DIR", apps_dir.as_str()),
                                        ("SPACE_SCRIPTS_DIR", scripts_dir.as_str()),
                                        ("PYTHONPATH", pp.as_str()),
                                    ],
                                )
                                .await
                            } else {
                                run_cmd_env(
                                    &program,
                                    &refs,
                                    &dir,
                                    120,
                                    &[
                                        ("SPACE_FILES_DIR", files_dir.as_str()),
                                        ("SPACE_APPS_DIR", apps_dir.as_str()),
                                        ("SPACE_SCRIPTS_DIR", scripts_dir.as_str()),
                                    ],
                                )
                                .await
                            }
                        };
                        match run.await {
                            Ok(out) => format_output(&out),
                            Err(e) => e,
                        }
                    }
                } else {
                    match resolve_confined(&self.skills_dir, &skill, &script) {
                        Err(e) => e,
                        Ok(file) if !file.is_file() => {
                            format!("no such script: {skill}/{script}")
                        }
                        Ok(file) => {
                            let dir = self.skills_dir.join(&skill);
                            let ext = file
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let run = async {
                                let mut cmd: Vec<std::ffi::OsString> = Vec::new();
                                let program: std::ffi::OsString = match ext.as_str() {
                                    "py" => {
                                        let py = ensure_venv(&dir).await?;
                                        cmd.push(file.clone().into());
                                        py.into()
                                    }
                                    "sh" | "bash" => {
                                        cmd.push(file.clone().into());
                                        "bash".into()
                                    }
                                    "js" | "mjs" => {
                                        cmd.push(file.clone().into());
                                        "node".into()
                                    }
                                    _ => file.clone().into(),
                                };
                                cmd.extend(extra.iter().map(std::ffi::OsString::from));
                                let refs: Vec<&std::ffi::OsStr> =
                                    cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                                run_cmd(&program, &refs, &dir, 120).await
                            };
                            match run.await {
                                Ok(out) => format_output(&out),
                                Err(e) => e,
                            }
                        }
                    }
                };
                (result, status)
            }
            "install_packages" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let pkgs: Vec<String> = v
                    .get("packages")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let (skill, app) = (field("skill"), field("app"));
                let status = format!("Installing {}…", pkgs.join(" "));
                let result = match validate_packages(&pkgs) {
                    Err(e) => e,
                    Ok(()) if !skill.is_empty() && !app.is_empty() => {
                        "pass either skill or app, not both".to_string()
                    }
                    Ok(()) if !skill.is_empty() => {
                        match resolve_confined(&self.skills_dir, &skill, "SKILL.md") {
                            Err(e) => e,
                            Ok(md) if !md.is_file() => format!("unknown skill: {skill}"),
                            Ok(_) => {
                                let dir = self.skills_dir.join(&skill);
                                let run = async {
                                    let py = ensure_venv(&dir).await?;
                                    let mut cmd: Vec<std::ffi::OsString> =
                                        vec!["-m".into(), "pip".into(), "install".into()];
                                    cmd.extend(pkgs.iter().map(std::ffi::OsString::from));
                                    let refs: Vec<&std::ffi::OsStr> =
                                        cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                                    run_cmd(py.as_os_str(), &refs, &dir, 300).await
                                };
                                match run.await {
                                    Ok(out) if out.status.success() => {
                                        format!("installed {} into {skill}'s venv", pkgs.join(" "))
                                    }
                                    Ok(out) => {
                                        format!("pip install failed:\n{}", format_output(&out))
                                    }
                                    Err(e) => e,
                                }
                            }
                        }
                    }
                    Ok(()) if !app.is_empty() => match self.resolve_app(&app) {
                        Err(e) => e,
                        Ok((uuid, app_dir)) => {
                            let pkg_json = app_dir.join("package.json");
                            let prep = std::fs::create_dir_all(&app_dir).and_then(|()| {
                                if pkg_json.exists() {
                                    Ok(())
                                } else {
                                    // npm walks up looking for a package.json — pin
                                    // the install to this app dir with a minimal one.
                                    std::fs::write(
                                        &pkg_json,
                                        format!("{{\"name\":{app:?},\"private\":true}}"),
                                    )
                                }
                            });
                            if let Err(e) = prep {
                                format!("cannot prepare {app}: {e}")
                            } else {
                                let mut cmd: Vec<std::ffi::OsString> =
                                    vec!["install".into(), "--no-audit".into(), "--no-fund".into()];
                                cmd.extend(pkgs.iter().map(std::ffi::OsString::from));
                                let refs: Vec<&std::ffi::OsStr> =
                                    cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                                match run_cmd("npm".as_ref(), &refs, &app_dir, 300).await {
                                    Ok(out) if out.status.success() => format!(
                                        "installed {} into {app}/node_modules — reference files as node_modules/<pkg>/… ; {}",
                                        pkgs.join(" "),
                                        self.app_link(&uuid),
                                    ),
                                    Ok(out) => {
                                        format!("npm install failed:\n{}", format_output(&out))
                                    }
                                    Err(e) => e,
                                }
                            }
                        }
                    },
                    Ok(()) => {
                        // No target: the space scripts dir venv (shared with run_python).
                        let dir = self.space_scripts_dir.clone();
                        let run = async {
                            std::fs::create_dir_all(&dir)
                                .map_err(|e| format!("cannot create scripts dir: {e}"))?;
                            let py = ensure_venv(&dir).await?;
                            let mut cmd: Vec<std::ffi::OsString> =
                                vec!["-m".into(), "pip".into(), "install".into()];
                            cmd.extend(pkgs.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                            run_cmd(py.as_os_str(), &refs, &dir, 300).await
                        };
                        match run.await {
                            Ok(out) if out.status.success() => {
                                format!("installed {} into the python scripts venv", pkgs.join(" "))
                            }
                            Ok(out) => format!("pip install failed:\n{}", format_output(&out)),
                            Err(e) => e,
                        }
                    }
                };
                (result, status)
            }
            "run_python" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let code = v
                    .get("code")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .filter(|n| !n.is_empty())
                    .map(str::to_string);
                let temporary = v
                    .get("temporary")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let extra: Vec<String> = v
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let result = match name {
                    None => "run_python requires a `name` parameter".to_string(),
                    Some(ref name) if !valid_relative_path(name) => {
                        format!("invalid script path: {name}")
                    }
                    Some(ref name) if !name.to_lowercase().ends_with(".py") => {
                        "run_python name must end with .py".to_string()
                    }
                    Some(ref name) => {
                        let (dir, file) = if temporary {
                            let tmp = std::env::temp_dir()
                                .join(format!("nexus-script-{}", uuid::Uuid::new_v4()));
                            let f = tmp.join(name);
                            (tmp, f)
                        } else {
                            let d = self.space_scripts_dir.clone();
                            let f = d.join(name);
                            (d, f)
                        };
                        let run = async {
                            if code.trim().is_empty() {
                                return Err("code must not be empty".to_string());
                            }
                            std::fs::create_dir_all(&dir)
                                .map_err(|e| format!("cannot create dir: {e}"))?;
                            std::fs::write(&file, &code)
                                .map_err(|e| format!("cannot write script: {e}"))?;
                            let py = ensure_venv(&dir).await?;
                            let mut cmd: Vec<std::ffi::OsString> = vec![file.into()];
                            cmd.extend(extra.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                cmd.iter().map(std::ffi::OsString::as_os_str).collect();
                            let files_dir = self.space_files_dir.to_string_lossy().to_string();
                            let apps_dir = self.space_apps_dir.to_string_lossy().to_string();
                            let scripts_dir = self.space_scripts_dir.to_string_lossy().to_string();
                            let envs = &[
                                ("SPACE_FILES_DIR", files_dir.as_str()),
                                ("SPACE_APPS_DIR", apps_dir.as_str()),
                                ("SPACE_SCRIPTS_DIR", scripts_dir.as_str()),
                            ];
                            run_cmd_env(py.as_os_str(), &refs, &dir, 120, envs).await
                        };
                        let output = run.await;
                        if temporary {
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                        match output {
                            Ok(out) => format_output(&out),
                            Err(e) => e,
                        }
                    }
                };
                (result, "Running script…".to_string())
            }
            "web_search" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = v
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or_default()
                    .to_string();
                let recency = v
                    .get("recency")
                    .and_then(|r| r.as_str())
                    .map(str::to_string);
                let str_list = |k: &str| {
                    v.get(k)
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };
                let include = str_list("include_domains");
                let mut exclude = str_list("exclude_domains");
                exclude.extend(self.blocked_domains.iter().cloned());
                let status = "Searching the web…".to_string();
                let result = match self
                    .search(&query, recency.as_deref(), &include, &exclude)
                    .await
                {
                    Ok(hits) if hits.is_empty() => "no results".to_string(),
                    Ok(hits) => format_results(&hits),
                    Err(e) => format!("search failed: {e}"),
                };
                (result, status)
            }
            "fetch_url" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let url = v
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or_default()
                    .to_string();
                let offset = usize::try_from(
                    v.get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(1);
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let fresh = v
                    .get("fresh")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let status = format!("Fetching {url}…");
                let result = match self.fetch_cached(&url, fresh).await {
                    Ok(text) => {
                        let lines: Vec<&str> = text.lines().collect();
                        let total = lines.len();
                        let start = (offset - 1).min(total);
                        let slice = &lines[start..(start + limit).min(total)];
                        if slice.is_empty() {
                            format!("{url}: offset {offset} is past the end ({total} lines)")
                        } else {
                            format!(
                                "{url} (lines {}-{} of {total}):\n{}",
                                start + 1,
                                start + slice.len(),
                                number_lines(slice, start),
                            )
                        }
                    }
                    Err(e) => format!("fetch failed: {e}"),
                };
                (result, status)
            }
            "academic_search" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = v
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or_default()
                    .to_string();
                let limit = usize::try_from(
                    v.get("limit")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(10),
                )
                .unwrap_or(10);
                let status = "Searching academic literature…".to_string();
                let result = match academic_search(&self.client, &query, limit).await {
                    Ok(papers) if papers.is_empty() => "no results".to_string(),
                    Ok(papers) => format_papers(&papers),
                    Err(e) => format!("academic search failed: {e}"),
                };
                (result, status)
            }
            "discussion_search" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let query = v
                    .get("query")
                    .and_then(|q| q.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = "Searching HN and Reddit…".to_string();
                let cache_key = format!("discussion://{query}");

                // Check cache first if enabled
                let cached = if let Some(db_path) = &self.db_path {
                    crate::db::open_attached(db_path)
                        .ok()
                        .and_then(|conn| crate::db::cache_get(&conn, &cache_key).ok().flatten())
                        .and_then(|(_, text, fetched_at)| {
                            // Do not preserve the old empty-result sentinel:
                            // it may have been produced by a blocked backend,
                            // and a later fallback can now recover results.
                            if crate::db::is_fresh(&fetched_at, chrono::Utc::now())
                                && text != "no results"
                            {
                                Some(text)
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };

                let result = if let Some(cached_text) = cached {
                    cached_text
                } else {
                    let text = discussion_search(&self.client, &query).await;
                    // Write through to cache if enabled
                    if text != "no results"
                        && let Some(db_path) = &self.db_path
                        && let Ok(conn) = crate::db::open_attached(db_path)
                    {
                        let _ = crate::db::cache_put(&conn, &cache_key, &cache_key, None, &text);
                    }
                    text
                };
                (result, status)
            }
            "search_sources" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = "Searching session sources…".to_string();
                let result = match (&self.research_session_id, &self.db_path) {
                    (Some(session_id), Some(db_path)) => match crate::db::open_attached(db_path) {
                        Err(e) => format!("source search failed: {e}"),
                        Ok(conn) => {
                            match crate::db::search_session_sources(&conn, session_id, &query) {
                                Ok(hits) if hits.is_empty() => {
                                    "no matches in this session's sources".to_string()
                                }
                                Ok(hits) => hits
                                    .iter()
                                    .map(|(url, text)| {
                                        let cut: String = text.chars().take(500).collect();
                                        format!("{url}:\n{cut}")
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n"),
                                Err(e) => format!("source search failed: {e}"),
                            }
                        }
                    },
                    _ => "no session source bundle available".to_string(),
                };
                (result, status)
            }
            "list_citations" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string));
                let status = "Listing citations…".to_string();
                let result = match &self.files {
                    None => "no space context available".to_string(),
                    Some(ctx) => match rusqlite::Connection::open(&ctx.db_path) {
                        Err(e) => format!("citation lookup failed: {e}"),
                        Ok(conn) => match crate::db::search_citations(
                            &conn,
                            &ctx.space_id,
                            query.as_deref(),
                        ) {
                            Ok(rows) if rows.is_empty() => "no citations recorded yet".to_string(),
                            Ok(rows) => rows
                                .iter()
                                .map(|(report, url, title)| {
                                    if title.is_empty() {
                                        format!("{report}: {url}")
                                    } else {
                                        format!("{report}: {url} ({title})")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Err(e) => format!("citation lookup failed: {e}"),
                        },
                    },
                };
                (result, status)
            }
            "batch" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let calls: Vec<(String, serde_json::Value)> = v
                    .get("calls")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                let tool = item
                                    .get("tool")
                                    .and_then(|t| t.as_str())
                                    .filter(|t| !t.is_empty())?
                                    .to_string();
                                let arguments = item
                                    .get("arguments")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({}));
                                Some((tool, arguments))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if calls.is_empty() {
                    return (
                        "batch requires a non-empty calls array".to_string(),
                        "Running batch…".to_string(),
                    );
                }
                if calls.len() > MAX_BATCH_CALLS {
                    return (
                        format!(
                            "batch accepts at most {MAX_BATCH_CALLS} calls, got {}",
                            calls.len()
                        ),
                        "Running batch…".to_string(),
                    );
                }
                if calls.iter().any(|(tool, _)| tool == "batch") {
                    return (
                        "nested batch calls are not allowed — flatten them into one list"
                            .to_string(),
                        "Running batch…".to_string(),
                    );
                }
                let status = format!("Running {} batched operations…", calls.len());
                // Serialize each sub-call once. Read-only batches run
                // concurrently (network latency overlaps); any mutating call
                // keeps the whole batch sequential so writes to the same file
                // can't race. Results are zipped back into call order.
                let items: Vec<(String, String)> = calls
                    .iter()
                    .map(|(tool, arguments)| {
                        (
                            tool.clone(),
                            serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
                        )
                    })
                    .collect();
                let results: Vec<(String, String)> = if items.len() > 1
                    && items
                        .iter()
                        .all(|(tool, args)| is_read_only_tool(tool, args))
                {
                    // Box::pin keeps the recursive `run` future behind a
                    // pointer so the join_all future stays finitely sized.
                    futures_util::future::join_all(
                        items
                            .iter()
                            .map(|(tool, args)| Box::pin(self.run(tool, args))),
                    )
                    .await
                } else {
                    let mut results = Vec::with_capacity(items.len());
                    for (tool, args) in &items {
                        results.push(Box::pin(self.run(tool, args)).await);
                    }
                    results
                };
                let mut out = String::new();
                for (i, ((tool, _), (_, arguments))) in items.iter().zip(calls.iter()).enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    let label: String = batch_call_label(tool, arguments)
                        .chars()
                        .take(120)
                        .collect();
                    let _ = write!(
                        out,
                        "[{}/{}] {}\n{}",
                        i + 1,
                        items.len(),
                        label,
                        results[i].0,
                    );
                }
                (out, status)
            }
            "search_files" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = "Searching files…".to_string();
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => search_files_impl(ctx, &query).await,
                };
                (result, status)
            }
            "read_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let offset = usize::try_from(
                    v.get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(1);
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {name}…");
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => match crate::db::open_attached(&ctx.db_path)
                        .and_then(|conn| crate::db::file_text(&conn, &ctx.space_id, &name))
                    {
                        Ok(Some(text)) => {
                            let lines: Vec<&str> = text.lines().collect();
                            let total = lines.len();
                            let start = (offset - 1).min(total);
                            let slice = &lines[start..(start + limit).min(total)];
                            if slice.is_empty() {
                                format!("{name}: offset {offset} is past the end ({total} lines)")
                            } else {
                                format!(
                                    "{name} (lines {}-{} of {total}):\n{}",
                                    start + 1,
                                    start + slice.len(),
                                    number_lines(slice, start),
                                )
                            }
                        }
                        Ok(None) => format!("unknown file: {name}"),
                        Err(e) => format!("file read failed: {e}"),
                    },
                };
                (result, status)
            }
            "read_pdf_page" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let page = v
                    .get("page")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1)
                    .max(1);
                let status = format!("Reading {name} page {page}…");
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => {
                        // Verify the PDF is known via DB lookup
                        let known = crate::db::open_attached(&ctx.db_path)
                            .ok()
                            .and_then(|conn| {
                                crate::db::file_text(&conn, &ctx.space_id, &name)
                                    .ok()
                                    .flatten()
                            })
                            .is_some();
                        if !known {
                            format!("unknown file: {name}")
                        } else if !name.to_lowercase().ends_with(".pdf") {
                            format!("not a PDF: {name}")
                        } else {
                            let stem = std::path::Path::new(&name)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or(&name);
                            let png_path = self
                                .space_files_dir
                                .join(stem)
                                .join(format!("page-{page}.png"));
                            if png_path.exists() {
                                format!("![page {page}]({stem}/page-{page}.png)")
                            } else {
                                format!(
                                    "page {page} image not available — the PDF was not imported through the vision OCR path. Use files with action=read to read its text content."
                                )
                            }
                        }
                    }
                };
                (result, status)
            }
            "copy_file_to_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let file_name = v
                    .get("file_name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Copying {file_name} to {app}…");
                let result = match (&self.apps, &self.files) {
                    (None, _) => "apps not available".to_string(),
                    (_, None) => "files not available".to_string(),
                    (Some(ctx), Some(fc)) => {
                        let (uuid, app_dir) = match self.resolve_app(&app) {
                            Err(e) => return (e, status),
                            Ok(t) => t,
                        };
                        let conn = match crate::db::open_attached(&fc.db_path) {
                            Err(e) => return (format!("db error: {e}"), status),
                            Ok(c) => c,
                        };
                        let text = match crate::db::file_text(&conn, &fc.space_id, &file_name) {
                            Err(e) => return (format!("file read error: {e}"), status),
                            Ok(None) => return (format!("unknown file: {file_name}"), status),
                            Ok(Some(t)) => t,
                        };
                        let store_path = app_dir.join("_store.db");
                        let store = match rusqlite::Connection::open(&store_path) {
                            Err(e) => return (format!("store error: {e}"), status),
                            Ok(s) => s,
                        };
                        let _ = store.execute_batch(
                            "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT)",
                        );
                        let key = format!("_file:{file_name}");
                        match store.execute(
                            "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                            rusqlite::params![key, text],
                        ) {
                            Ok(_) => {
                                let url = format!("http://127.0.0.1:{}/{uuid}/", ctx.server_port);
                                format!(
                                    "copied {file_name} into {app}'s KV — read it at {url}_api/kv/_file:{file_name}"
                                )
                            }
                            Err(e) => format!("kv write error: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "list_images" => {
                let status = "Listing images…".to_string();
                let result = match &self.apps {
                    None => "apps not available".to_string(),
                    Some(ctx) => {
                        let conn = match rusqlite::Connection::open(&ctx.space_db_path) {
                            Err(e) => return (format!("db error: {e}"), status),
                            Ok(c) => c,
                        };
                        // Scan messages for markdown image references
                        let mut images: Vec<serde_json::Value> = Vec::new();
                        let mut stmt = match conn.prepare(
                            "SELECT content FROM messages WHERE session_id = ?1 ORDER BY created_at ASC"
                        ) {
                            Ok(s) => s,
                            Err(e) => return (format!("query error: {e}"), status),
                        };
                        if let Ok(rows) =
                            stmt.query_map([&ctx.session_id], |r| r.get::<_, String>(0))
                        {
                            for row in rows.flatten() {
                                let mut rest = row.as_str();
                                while let Some(start) = rest.find("![") {
                                    if let Some(end) = rest[start..].find(')') {
                                        let inner = &rest[start + 2..start + end];
                                        if let Some((desc, file)) = inner.split_once("](") {
                                            let file = file.to_string();
                                            images.push(serde_json::json!({
                                                "id": file,
                                                "description": desc,
                                                "source": "conversation",
                                            }));
                                        }
                                        rest = &rest[start + end + 1..];
                                    } else {
                                        break;
                                    }
                                }
                            }
                        }
                        // Include space-file images
                        if let Ok(mut fstmt) = conn.prepare(
                            "SELECT f.name, f.status FROM files f
                             WHERE f.space_id = ?1
                             AND (f.name LIKE '%.jpg' OR f.name LIKE '%.jpeg'
                               OR f.name LIKE '%.png' OR f.name LIKE '%.gif'
                               OR f.name LIKE '%.webp' OR f.name LIKE '%.bmp')",
                        )
                            && let Ok(rows) = fstmt.query_map([&ctx.space_id], |r| {
                                let name: String = r.get(0)?;
                                let status: String = r.get(1)?;
                                Ok(serde_json::json!({"id": name, "description": null, "source": "space", "status": status}))
                            }) {
                                images.extend(rows.filter_map(std::result::Result::ok));
                            }
                        serde_json::to_string(&images).unwrap_or_else(|_| "[]".to_string())
                    }
                };
                (result, status)
            }
            "copy_images_to_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let image_ids: Vec<String> = v
                    .get("image_ids")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Copying {} images to {app}…", image_ids.len());
                let result = match self.apps.as_ref() {
                    None => "apps not available".to_string(),
                    Some(ctx) => {
                        let (uuid, app_dir) = match self.resolve_app(&app) {
                            Err(e) => return (e, status),
                            Ok(t) => t,
                        };
                        let images_dir = app_dir.join("_images");
                        if let Err(e) = std::fs::create_dir_all(&images_dir) {
                            return (format!("cannot create _images dir: {e}"), status);
                        }
                        let mut out: Vec<serde_json::Value> = Vec::new();
                        for img_id in &image_ids {
                            let src = ctx.files_dir.join(img_id);
                            let dst = images_dir.join(img_id);
                            if !valid_relative_path(img_id)
                                || !src.starts_with(&ctx.files_dir)
                                || !dst.starts_with(&images_dir)
                                || !src.exists()
                            {
                                out.push(serde_json::json!({"id": img_id, "error": "not found in space files"}));
                                continue;
                            }
                            match std::fs::copy(&src, &dst) {
                                Ok(_) => {
                                    out.push(serde_json::json!({
                                        "id": img_id,
                                        "url": format!("/{uuid}/_images/{img_id}"),
                                    }));
                                }
                                Err(e) => {
                                    out.push(
                                        serde_json::json!({"id": img_id, "error": format!("{e}")}),
                                    );
                                }
                            }
                        }
                        serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
                    }
                };
                (result, status)
            }
            "write_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path, content) = (field("app"), field("path"), field("content"));
                let status = format!("Writing {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        let file = app_dir.join(&path);
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else {
                            let write = file
                                .parent()
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .and_then(|()| std::fs::write(&file, &content));
                            match write {
                                Ok(()) => format!(
                                    "wrote {app}/{path} ({} bytes) — {}",
                                    content.len(),
                                    self.app_link(&uuid),
                                ),
                                Err(e) => format!("write failed: {e}"),
                            }
                        }
                    }
                };
                (result, status)
            }
            "edit_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path) = (field("app"), field("path"));
                let edits: Vec<(String, Option<String>)> = v
                    .get("edits")
                    .and_then(|e| e.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                let hash = e.get("hash")?.as_str()?.to_string();
                                let new = e.get("new").and_then(|n| n.as_str()).map(str::to_string);
                                Some((hash, new))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let status = format!("Editing {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        let file = app_dir.join(&path);
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else if app_path_ignored(&app_dir, &file) {
                            format!("{app}/{path} is ignored by .gitignore")
                        } else {
                            match std::fs::read_to_string(&file) {
                                Err(e) => format!("cannot read {app}/{path}: {e}"),
                                Ok(text) => match apply_hashline_edits(&text, &edits) {
                                    Err(e) => e,
                                    Ok((new_text, diff)) => match std::fs::write(&file, new_text) {
                                        Ok(()) => {
                                            format!(
                                                "edited {app}/{path} — {}{diff}",
                                                self.app_link(&uuid)
                                            )
                                        }
                                        Err(e) => format!("write failed: {e}"),
                                    },
                                },
                            }
                        }
                    }
                };
                (result, status)
            }
            "diff_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path, content) = (field("app"), field("path"), field("content"));
                let status = format!("Diffing {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((_uuid, app_dir)) => {
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else {
                            let file = app_dir.join(&path);
                            if app_path_ignored(&app_dir, &file) {
                                format!("{app}/{path} is ignored by .gitignore")
                            } else {
                                match std::fs::read_to_string(&file) {
                                    Ok(current) => unified_diff(&app, &path, &current, &content),
                                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                        unified_diff(&app, &path, "", &content)
                                    }
                                    Err(e) => format!("cannot read {app}/{path}: {e}"),
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "grep_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, pattern) = (field("app"), field("pattern"));
                let compact = v
                    .get("compact")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let status = format!("Searching {app}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((_uuid, app_dir)) => {
                        if !app_dir.is_dir() {
                            format!("unknown app: {app}")
                        } else if pattern.is_empty() {
                            "pattern must not be empty".to_string()
                        } else {
                            let mut hits = Vec::new();
                            grep_dir(&app_dir, &app_dir, &pattern.to_lowercase(), &mut hits);
                            if hits.is_empty() {
                                format!("no matches for {pattern:?} in {app}")
                            } else {
                                let n = hits.len();
                                hits.truncate(50);
                                let mut by_file = std::collections::BTreeMap::<
                                    String,
                                    Vec<(usize, String)>,
                                >::new();
                                for (path, line) in hits {
                                    let text = if compact {
                                        String::new()
                                    } else {
                                        std::fs::read_to_string(app_dir.join(&path))
                                            .ok()
                                            .and_then(|contents| {
                                                contents
                                                    .lines()
                                                    .nth(line.saturating_sub(1))
                                                    .map(str::to_string)
                                            })
                                            .unwrap_or_default()
                                    };
                                    by_file.entry(path).or_default().push((line, text));
                                }
                                let mut result = by_file
                                    .into_iter()
                                    .map(|(path, lines)| {
                                        if compact {
                                            let numbers = lines
                                                .into_iter()
                                                .map(|(line, _)| line.to_string())
                                                .collect::<Vec<_>>()
                                                .join(",");
                                            format!("{path}:{numbers}")
                                        } else {
                                            lines
                                                .into_iter()
                                                .map(|(line, text)| {
                                                    format!("{path}:{line}: {text}")
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n")
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if n > 50 {
                                    let _ = write!(result, "\n… ({} more matches)", n - 50);
                                }
                                result
                            }
                        }
                    }
                };
                (result, status)
            }
            "read_app_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (app, path) = (field("app"), field("path"));
                let offset = usize::try_from(
                    v.get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(1);
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {app}/{path}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((_uuid, app_dir)) => {
                        let file = app_dir.join(&path);
                        if path.is_empty() || path.starts_with('/') || path.contains("..") {
                            format!("invalid path: {path:?}")
                        } else if app_path_ignored(&app_dir, &file) {
                            format!("{app}/{path} is ignored by .gitignore")
                        } else {
                            match std::fs::read_to_string(&file) {
                                Err(e) => format!("cannot read {app}/{path}: {e}"),
                                Ok(text) => {
                                    let lines: Vec<&str> = text.lines().collect();
                                    let total = lines.len();
                                    let start = (offset - 1).min(total);
                                    let slice = &lines[start..(start + limit).min(total)];
                                    if slice.is_empty() {
                                        format!(
                                            "{app}/{path}: offset {offset} is past the end ({total} lines)"
                                        )
                                    } else {
                                        format!(
                                            "{app}/{path} (lines {}-{} of {total}):\n{}",
                                            start + 1,
                                            start + slice.len(),
                                            number_lines_with_hash(slice, start),
                                        )
                                    }
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "generate_image" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
                let image_id = v
                    .get("image_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let size = v
                    .get("size")
                    .and_then(|x| x.as_str())
                    .unwrap_or("1024x1024");
                let status = if image_id.is_some() {
                    "Editing image…".to_string()
                } else {
                    "Generating image…".to_string()
                };
                let result = match &self.image_gen_backend {
                    None => "no image generation model configured — set one in /config".to_string(),
                    Some((provider, model)) => {
                        if prompt.is_empty() {
                            "prompt must not be empty".to_string()
                        } else {
                            let image_data = image_id.and_then(|id| {
                                // Try id as a full filename first, then as a stem + .png.
                                resolve_image(&self.space_files_dir, id).or_else(|| {
                                    let stem = std::path::Path::new(id)
                                        .file_stem()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or(id);
                                    let files = self.space_files_dir.as_path();
                                    std::fs::read_dir(files).ok().and_then(|e| {
                                        e.flatten()
                                            .find(|e| {
                                                e.path().file_stem().is_some_and(|s| s == stem)
                                            })
                                            .and_then(|e| std::fs::read(e.path()).ok())
                                    })
                                })
                            });
                            if let Some(id) = image_id.filter(|_| image_data.is_none()) {
                                format!("image not found: {id}")
                            } else {
                                match provider
                                    .generate_image(model, prompt, size, image_data.as_deref())
                                    .await
                                {
                                    Err(e) => format!("image generation failed: {e}"),
                                    Ok((png_bytes, ext)) => {
                                        let id = uuid::Uuid::new_v4().to_string();
                                        let filename = format!("{id}.{ext}");
                                        let img_path = self.space_files_dir.join(&filename);
                                        if let Err(e) =
                                            std::fs::create_dir_all(&self.space_files_dir)
                                        {
                                            format!("cannot create images dir: {e}")
                                        } else if let Err(e) = std::fs::write(&img_path, &png_bytes)
                                        {
                                            format!("cannot write image: {e}")
                                        } else {
                                            let _ = std::fs::create_dir_all(&self.space_files_dir);
                                            let _ = std::fs::write(
                                                self.space_files_dir.join(&filename),
                                                &png_bytes,
                                            );
                                            let description =
                                                format!("generated image of {prompt}");
                                            serde_json::json!({
                                                "id": id,
                                                "path": img_path.to_string_lossy(),
                                                "description": description,
                                            })
                                            .to_string()
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "generate_video" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
                let duration = u32::try_from(
                    v.get("duration")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(6),
                )
                .unwrap_or(6);
                let resolution = v
                    .get("resolution")
                    .and_then(|x| x.as_str())
                    .unwrap_or("720p");
                let aspect_ratio = v
                    .get("aspect_ratio")
                    .and_then(|x| x.as_str())
                    .unwrap_or("16:9");
                let generate_audio = v
                    .get("generate_audio")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let first_frame_id = v
                    .get("first_frame_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let last_frame_id = v
                    .get("last_frame_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let ref_image_id = v
                    .get("ref_image_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let character_refs: Vec<String> = v
                    .get("character_refs")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let location_refs: Vec<String> = v
                    .get("location_refs")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let seed = v
                    .get("seed")
                    .and_then(serde_json::Value::as_i64)
                    .map(|x| i32::try_from(x).unwrap_or_default());
                let source_video_id = v
                    .get("source_video_id")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let status = "Generating video…".to_string();
                let result = match &self.video_gen_backend {
                    None => "no video generation model configured — set one in /config".to_string(),
                    Some((provider, model)) => {
                        let (duration, resolution, aspect_ratio) =
                            normalize_video_params(model, duration, resolution, aspect_ratio);
                        if prompt.is_empty() {
                            "prompt must not be empty".to_string()
                        } else {
                            let first_frame = first_frame_id
                                .and_then(|id| resolve_image(&self.space_files_dir, id));
                            let last_frame = last_frame_id
                                .and_then(|id| resolve_image(&self.space_files_dir, id));
                            let ref_img = ref_image_id
                                .and_then(|id| resolve_image(&self.space_files_dir, id));
                            let named = resolve_named_references(
                                &self.space_files_dir,
                                &character_refs,
                                &location_refs,
                            );
                            let mut all_refs = Vec::new();
                            if let Some(d) = ref_img {
                                all_refs.push(d);
                            }
                            all_refs.extend(named);
                            let provider_options = source_video_id.and_then(|sid| {
                                if !valid_relative_path(sid) {
                                    return None;
                                }
                                let path = self.space_files_dir.join(format!("{sid}.mp4"));
                                let data = std::fs::read(&path).ok()?;
                                let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                                Some(serde_json::json!({
                                    "alibaba": {
                                        "parameters": {
                                            "video": format!("data:video/mp4;base64,{}", b64)
                                        }
                                    }
                                }))
                            });
                            match provider
                                .generate_video(crate::provider::openrouter::VideoRequest {
                                    model: model.clone(),
                                    prompt: prompt.to_string(),
                                    duration,
                                    resolution: resolution.clone(),
                                    aspect_ratio: aspect_ratio.clone(),
                                    generate_audio,
                                    first_frame,
                                    last_frame,
                                    input_references: all_refs,
                                    seed,
                                    provider_options,
                                })
                                .await
                            {
                                Err(e) => format!("video generation failed: {e}"),
                                Ok((mp4_bytes, cost)) => {
                                    let id = uuid::Uuid::new_v4().to_string();
                                    let video_filename = format!("{id}.mp4");
                                    let thumb_filename = format!("{id}_first.png");
                                    let last_thumb_filename = format!("{id}_last.png");
                                    let meta_filename = format!("{id}.json");
                                    let video_path = self.space_files_dir.join(&video_filename);
                                    if let Err(e) = std::fs::create_dir_all(&self.space_files_dir) {
                                        format!("cannot create files dir: {e}")
                                    } else if let Err(e) = std::fs::write(&video_path, &mp4_bytes) {
                                        format!("cannot write video: {e}")
                                    } else {
                                        let thumb_path = self.space_files_dir.join(&thumb_filename);
                                        let last_thumb_path =
                                            self.space_files_dir.join(&last_thumb_filename);
                                        let has_ffmpeg =
                                            extract_ffmpeg_frame(&video_path, &thumb_path, false);
                                        if has_ffmpeg {
                                            extract_ffmpeg_frame(
                                                &video_path,
                                                &last_thumb_path,
                                                true,
                                            );
                                        } else if let Some(fid) = first_frame_id
                                            && let Some(data) =
                                                resolve_image(&self.space_files_dir, fid)
                                        {
                                            let _ = std::fs::write(&thumb_path, data);
                                        }
                                        let now = chrono::Utc::now().to_rfc3339();
                                        let meta = serde_json::json!({
                                            "type": "generated_video",
                                            "video_id": id,
                                            "prompt": prompt,
                                            "model": model,
                                            "duration_sec": duration,
                                            "resolution": resolution,
                                            "aspect_ratio": aspect_ratio,
                                            "has_audio": generate_audio,
                                            "character_refs": character_refs,
                                            "location_refs": location_refs,
                                            "seed": seed,
                                            "cost_usd": cost,
                                            "generated_at": now,
                                        });
                                        let _ = std::fs::write(
                                            self.space_files_dir.join(&meta_filename),
                                            serde_json::to_string_pretty(&meta).unwrap_or_default(),
                                        );
                                        let desc = format!("generated video of {prompt}");
                                        serde_json::json!({
                                            "id": id,
                                            "video_path": video_path.to_string_lossy(),
                                            "thumbnail_path": if thumb_path.exists() { thumb_path.to_string_lossy().to_string() } else { String::new() },
                                            "metadata_path": self.space_files_dir.join(&meta_filename).to_string_lossy(),
                                            "description": desc,
                                            "last_thumb": if last_thumb_path.exists() { last_thumb_path.to_string_lossy().to_string() } else { String::new() },
                                            "model": model,
                                            "duration_sec": duration,
                                            "cost_usd": cost,
                                        }).to_string()
                                    }
                                }
                            }
                        }
                    }
                };
                (result, status)
            }
            "edit_video" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let video_id = v
                    .get("video_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let lighting = v
                    .get("lighting")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let camera_move = v
                    .get("camera_move")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty());
                let intensity = v
                    .get("intensity")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.5)
                    .clamp(0.0, 1.0);
                let speed = v
                    .get("speed")
                    .and_then(serde_json::Value::as_f64)
                    .filter(|x| *x > 0.0);
                let trim_start = v
                    .get("trim_start")
                    .and_then(serde_json::Value::as_f64)
                    .filter(|x| *x >= 0.0);
                let trim_end = v
                    .get("trim_end")
                    .and_then(serde_json::Value::as_f64)
                    .filter(|x| *x >= 0.0);
                let remove_audio = v
                    .get("remove_audio")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let status = "Editing video…".to_string();
                let result = if video_id.is_empty() {
                    "video_id is required".to_string()
                } else if !valid_relative_path(&video_id) {
                    "invalid video_id".to_string()
                } else if !ffmpeg_available() {
                    "ffmpeg not found — install ffmpeg to edit videos".to_string()
                } else {
                    let src_path = self.space_files_dir.join(format!("{video_id}.mp4"));
                    if src_path.exists() {
                        let id = uuid::Uuid::new_v4().to_string();
                        let output_path = self.space_files_dir.join(format!("{id}.mp4"));
                        let thumb_path = self.space_files_dir.join(format!("{id}_first.png"));
                        let meta_path = self.space_files_dir.join(format!("{id}.json"));

                        let mut cmd = std::process::Command::new("ffmpeg");
                        cmd.arg("-y");
                        if let Some(s) = trim_start {
                            cmd.arg("-ss").arg(format!("{s}"));
                        }
                        if let Some(e) = trim_end {
                            cmd.arg("-to").arg(format!("{e}"));
                        }
                        cmd.arg("-i").arg(&src_path);

                        let mut filter_parts: Vec<String> = Vec::new();

                        // Camera move (crop animation)
                        if let Some(mv) = camera_move {
                            let m = intensity * 0.2;
                            let cf = build_camera_filter(mv, m);
                            filter_parts.push(cf);
                        }

                        // Lighting preset
                        if let Some(lt) = lighting {
                            let lf = build_lighting_filter(lt, intensity);
                            filter_parts.push(lf);
                        }

                        // Speed change
                        if let Some(sp) = speed {
                            filter_parts.push(format!("setpts={}*PTS", 1.0 / sp));
                            let atempo = format!("atempo={}", (1.0 / sp).clamp(0.5, 2.0));
                            cmd.arg("-af").arg(&atempo);
                        }

                        if !filter_parts.is_empty() {
                            cmd.arg("-vf").arg(filter_parts.join(","));
                        }

                        if remove_audio {
                            cmd.arg("-an");
                        }

                        cmd.arg(&output_path)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null());

                        match cmd.status() {
                            Err(e) => format!("ffmpeg failed to start: {e}"),
                            Ok(s) if !s.success() => "ffmpeg returned non-zero exit".to_string(),
                            Ok(_) => {
                                extract_ffmpeg_frame(&output_path, &thumb_path, false);
                                let now = chrono::Utc::now().to_rfc3339();
                                let meta = serde_json::json!({
                                    "type": "edited_video",
                                    "video_id": id,
                                    "source_video_id": video_id,
                                    "lighting": lighting,
                                    "camera_move": camera_move,
                                    "intensity": intensity,
                                    "speed": speed,
                                    "trim_start": trim_start,
                                    "trim_end": trim_end,
                                    "remove_audio": remove_audio,
                                    "generated_at": now,
                                });
                                let _ = std::fs::write(
                                    &meta_path,
                                    serde_json::to_string_pretty(&meta).unwrap_or_default(),
                                );
                                serde_json::json!({
                                    "id": id,
                                    "video_path": output_path.to_string_lossy(),
                                    "thumbnail_path": if thumb_path.exists() { thumb_path.to_string_lossy().to_string() } else { String::new() },
                                    "metadata_path": meta_path.to_string_lossy(),
                                    "description": format!("edited video from {video_id}"),
                                }).to_string()
                            }
                        }
                    } else {
                        format!("video '{video_id}' not found")
                    }
                };
                (result, status)
            }
            "extract_frame" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let video_id = v
                    .get("video_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let time_sec = v
                    .get("time_sec")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                let fmt = v.get("format").and_then(|x| x.as_str()).unwrap_or("png");
                let status = "Extracting frame…".to_string();
                let result = if video_id.is_empty() {
                    "video_id is required".to_string()
                } else if !valid_relative_path(&video_id) {
                    "invalid video_id".to_string()
                } else {
                    let src_path = self.space_files_dir.join(format!("{video_id}.mp4"));
                    if !src_path.exists() {
                        format!("video '{video_id}' not found")
                    } else if !ffmpeg_available() {
                        "ffmpeg not found — install ffmpeg to extract frames".to_string()
                    } else {
                        let id = uuid::Uuid::new_v4().to_string();
                        let ext = if fmt == "jpg" { "jpg" } else { "png" };
                        let output = self.space_files_dir.join(format!("{id}.{ext}"));
                        let status_code = std::process::Command::new("ffmpeg")
                            .arg("-y")
                            .arg("-ss")
                            .arg(format!("{time_sec}"))
                            .arg("-i")
                            .arg(&src_path)
                            .arg("-vframes")
                            .arg("1")
                            .arg("-f")
                            .arg("image2")
                            .arg(&output)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                        match status_code {
                            Err(_) => "ffmpeg execution failed".to_string(),
                            Ok(s) if !s.success() => "ffmpeg failed to extract frame".to_string(),
                            Ok(_) => {
                                serde_json::json!({
                                    "id": id,
                                    "path": output.to_string_lossy(),
                                    "description": format!("frame at {time_sec}s from video {video_id}"),
                                }).to_string()
                            }
                        }
                    }
                };
                (result, status)
            }
            "stitch_videos" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let video_ids: Vec<String> = v
                    .get("video_ids")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let status = "Stitching videos…".to_string();
                let result = if video_ids.is_empty() {
                    "video_ids must not be empty".to_string()
                } else if !ffmpeg_available() {
                    "ffmpeg not found — install ffmpeg to stitch videos".to_string()
                } else {
                    let mut video_files = Vec::new();
                    let mut total_cost = 0.0_f64;
                    for vid_id in &video_ids {
                        if !valid_relative_path(vid_id) {
                            break;
                        }
                        let mp4 = self.space_files_dir.join(format!("{vid_id}.mp4"));
                        if !mp4.exists() {
                            break;
                        }
                        let meta_path = self.space_files_dir.join(format!("{vid_id}.json"));
                        if let Ok(json_str) = std::fs::read_to_string(&meta_path)
                            && let Ok(meta) = serde_json::from_str::<serde_json::Value>(&json_str)
                        {
                            total_cost += meta
                                .get("cost_usd")
                                .and_then(serde_json::Value::as_f64)
                                .unwrap_or(0.0);
                        }
                        video_files.push(mp4);
                    }
                    if video_files.len() == video_ids.len() {
                        let id = uuid::Uuid::new_v4().to_string();
                        let concat_name = format!("_stitch_concat_{id}.txt");
                        let concat_path = self.space_files_dir.join(&concat_name);
                        let output_name = format!("_stitch_{id}.mp4");
                        let output_path = self.space_files_dir.join(&output_name);
                        let thumb_name = format!("{id}_first.png");
                        let thumb_path = self.space_files_dir.join(&thumb_name);
                        let concat_content: String =
                            video_files.iter().fold(String::new(), |mut c, p| {
                                let _ = writeln!(c, "file '{}'", p.display());
                                c
                            });
                        if let Err(e) = std::fs::write(&concat_path, &concat_content) {
                            format!("cannot write concat file: {e}")
                        } else {
                            let stitched = std::process::Command::new("ffmpeg")
                                .arg("-y")
                                .arg("-f")
                                .arg("concat")
                                .arg("-safe")
                                .arg("0")
                                .arg("-i")
                                .arg(&concat_path)
                                .arg("-c")
                                .arg("copy")
                                .arg(&output_path)
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status();
                            let _ = std::fs::remove_file(&concat_path);
                            match stitched {
                                Err(_) => "ffmpeg execution failed".to_string(),
                                Ok(s) if !s.success() => "ffmpeg concat failed".to_string(),
                                Ok(_) => {
                                    if let Some(first) = video_files.first() {
                                        extract_ffmpeg_frame(first, &thumb_path, false);
                                    }
                                    let now = chrono::Utc::now().to_rfc3339();
                                    let seq_meta = serde_json::json!({
                                        "type": "video_sequence",
                                        "video_id": id,
                                        "shot_ids": video_ids,
                                        "total_cost_usd": total_cost,
                                        "generated_at": now,
                                    });
                                    let _ = std::fs::write(
                                        self.space_files_dir.join(format!("{id}.json")),
                                        serde_json::to_string_pretty(&seq_meta).unwrap_or_default(),
                                    );
                                    serde_json::json!({
                                        "id": id,
                                        "video_path": output_path.to_string_lossy(),
                                        "thumbnail_path": if thumb_path.exists() { thumb_path.to_string_lossy().to_string() } else { String::new() },
                                        "metadata_path": self.space_files_dir.join(format!("{id}.json")).to_string_lossy(),
                                        "description": format!("stitched sequence of {} clips", video_files.len()),
                                        "cost_usd": total_cost,
                                    }).to_string()
                                }
                            }
                        }
                    } else {
                        format!("some video files not found for IDs: {video_ids:?}")
                    }
                };
                (result, status)
            }
            "save_reference" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let ref_type = v
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("character")
                    .to_string();
                let image_id = v
                    .get("image_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let description = v
                    .get("description")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Saving reference '{name}'…");
                let result = if name.is_empty() || image_id.is_empty() {
                    "name and image_id are required".to_string()
                } else {
                    let mut refs = read_video_refs(&self.space_files_dir);
                    if let Some(obj) = refs.as_object_mut() {
                        obj.insert(
                            name.clone(),
                            serde_json::json!({
                                "name": name,
                                "type": ref_type,
                                "description": description,
                                "image_id": image_id,
                                "created_at": chrono::Utc::now().to_rfc3339(),
                            }),
                        );
                    }
                    match write_video_refs(&self.space_files_dir, &refs) {
                        Err(e) => format!("failed to save reference: {e}"),
                        Ok(()) => format!(
                            "saved reference '{name}' ({ref_type}) — use in generate_video with character_refs/location_refs"
                        ),
                    }
                };
                (result, status)
            }
            "list_references" => {
                let status = "Listing references…".to_string();
                let refs = read_video_refs(&self.space_files_dir);
                let result = if refs.as_object().is_none_or(serde_json::Map::is_empty) {
                    "no references saved yet — use video_references with action=save to create one"
                        .to_string()
                } else {
                    let pretty: Vec<serde_json::Value> = refs
                        .as_object()
                        .map(|o| o.values().cloned().collect())
                        .unwrap_or_default();
                    serde_json::to_string_pretty(&pretty).unwrap_or_default()
                };
                (result, status)
            }
            "delete_reference" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Deleting reference '{name}'…");
                let result = if name.is_empty() {
                    "name is required".to_string()
                } else {
                    let mut refs = read_video_refs(&self.space_files_dir);
                    if refs.get(&name).is_none() {
                        format!("reference '{name}' not found")
                    } else {
                        refs.as_object_mut().map(|o| o.remove(&name));
                        match write_video_refs(&self.space_files_dir, &refs) {
                            Err(e) => format!("failed to delete reference: {e}"),
                            Ok(()) => format!("deleted reference '{name}'"),
                        }
                    }
                };
                (result, status)
            }
            "list_scripts" => {
                let status = "Listing scripts…".to_string();
                let result = match std::fs::read_dir(&self.space_scripts_dir) {
                    Err(_) => "[]".to_string(),
                    Ok(entries) => {
                        let scripts: Vec<serde_json::Value> = entries
                            .flatten()
                            .filter(|e| e.path().is_file())
                            .filter_map(|e| {
                                let meta = e.metadata().ok()?;
                                let ext = e
                                    .path()
                                    .extension()
                                    .and_then(|x| x.to_str())
                                    .unwrap_or("")
                                    .to_string();
                                Some(serde_json::json!({
                                    "name": e.file_name().to_string_lossy(),
                                    "size": meta.len(),
                                    "ext": ext,
                                }))
                            })
                            .collect();
                        serde_json::to_string(&scripts).unwrap_or_else(|_| "[]".to_string())
                    }
                };
                (result, status)
            }
            "write_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let path = v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let content = v
                    .get("content")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Writing {path}…");
                let result = {
                    let file = self.space_scripts_dir.join(&path);
                    if valid_relative_path(&path) {
                        let write = file
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&file, &content));
                        match write {
                            Ok(()) => format!("wrote {path} ({} bytes)", content.len()),
                            Err(e) => format!("write failed: {e}"),
                        }
                    } else {
                        format!("invalid path: {path:?}")
                    }
                };
                (result, status)
            }
            "read_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let path = v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let offset = usize::try_from(
                    v.get("offset")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(1)
                        .max(1),
                )
                .unwrap_or(1);
                let limit = v
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {path}…");
                let result = {
                    let file = self.space_scripts_dir.join(&path);
                    if valid_relative_path(&path) {
                        match std::fs::read_to_string(&file) {
                            Err(e) => format!("cannot read {path}: {e}"),
                            Ok(text) => {
                                let lines: Vec<&str> = text.lines().collect();
                                let total = lines.len();
                                let start = (offset - 1).min(total);
                                let slice = &lines[start..(start + limit).min(total)];
                                if slice.is_empty() {
                                    format!(
                                        "{path}: offset {offset} is past the end ({total} lines)"
                                    )
                                } else {
                                    format!(
                                        "{path} (lines {}-{} of {total}):\n{}",
                                        start + 1,
                                        start + slice.len(),
                                        number_lines_with_hash(slice, start),
                                    )
                                }
                            }
                        }
                    } else {
                        format!("invalid path: {path:?}")
                    }
                };
                (result, status)
            }
            "edit_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let path = v
                    .get("path")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let edits: Vec<(String, Option<String>)> = v
                    .get("edits")
                    .and_then(|e| e.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                let hash = e.get("hash")?.as_str()?.to_string();
                                let new = e.get("new").and_then(|n| n.as_str()).map(str::to_string);
                                Some((hash, new))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let status = format!("Editing {path}…");
                let result = {
                    let file = self.space_scripts_dir.join(&path);
                    if valid_relative_path(&path) {
                        match std::fs::read_to_string(&file) {
                            Err(e) => format!("cannot read {path}: {e}"),
                            Ok(text) => match apply_hashline_edits(&text, &edits) {
                                Err(e) => e,
                                Ok((new_text, diff)) => match std::fs::write(&file, new_text) {
                                    Ok(()) => format!("edited {path} — {diff}"),
                                    Err(e) => format!("write failed: {e}"),
                                },
                            },
                        }
                    } else {
                        format!("invalid path: {path:?}")
                    }
                };
                (result, status)
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        };
        let result = if name == "batch" {
            // The batch arm already bounded each sub-result; apply the larger
            // combined cap here so several packed results survive intact.
            cap_result(result, MAX_BATCH_RESULT_CHARS)
        } else {
            cap_tool_result(result)
        };
        (result, status)
    }
}

/// Tools safe to run concurrently in one round-trip: they read state or hit
/// the network but never mutate files/db in ways that could race. Used to
/// parallelize both model-issued parallel tool calls and `batch` sub-calls.
/// Consolidated names (`search`, `files`, `skills`, `scripts`, `app`, `media`,
/// `research_lookup`) count as read-only only for their read-only actions —
/// the `action` argument decides. Legacy names (`skill`, `web_search`, …)
/// classify as before; mutating consolidations (`batch`) are never read-only.
pub fn is_read_only_tool(name: &str, args: &str) -> bool {
    let action_is = |actions: &[&str]| {
        serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(str::to_string))
            .is_some_and(|a| actions.contains(&a.as_str()))
    };
    match name {
        "skills" => action_is(&["load"]),
        "scripts" => action_is(&["list", "read"]),
        "app" => action_is(&["read", "search", "diff", "list"]),
        "media" => action_is(&["list_references"]),
        _ => matches!(
            name,
            "skill"
                | "search"
                | "web_search"
                | "academic_search"
                | "discussion_search"
                | "fetch_url"
                | "research_lookup"
                | "search_sources"
                | "list_citations"
                | "files"
                | "search_files"
                | "read_file"
                | "read_pdf_page"
                | "app_inspect"
                | "read_app_file"
                | "grep_app"
                | "diff_app"
                | "list_images"
                | "read_script"
                | "list_scripts"
                | "list_references"
        ),
    }
}

/// Compact label for one `batch` sub-call, shown above its result so the
/// model can tell which output belongs to which operation.
fn batch_call_label(name: &str, v: &serde_json::Value) -> String {
    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let quoted = |k: &str| format!("{:?}", s(k));
    match name {
        "skills" => format!("skills/{} {}", s("action"), s("name")),
        "scripts" => {
            let target = if s("action") == "install" {
                "packages".to_string()
            } else {
                s("path")
            };
            format!("scripts/{} {}", s("action"), target)
        }
        "app" => {
            let path = s("path");
            if path.is_empty() {
                format!("app/{} {}", s("action"), s("app"))
            } else {
                format!("app/{} {}/{}", s("action"), s("app"), path)
            }
        }
        "media" => {
            let target = [s("video_id"), s("name"), s("image_id")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_else(|| s("prompt").chars().take(40).collect());
            format!("media/{} {}", s("action"), target)
        }
        "search" => format!("search {} {}", s("mode"), quoted("query")),
        "fetch_url" => format!("fetch_url {}", s("url")),
        "files" => {
            let target = if s("name").is_empty() {
                quoted("query")
            } else {
                s("name")
            };
            format!("files/{} {target}", s("action"))
        }
        "app_inspect" => format!("app_inspect/{} {}/{}", s("action"), s("app"), s("path")),
        "app_modify" => format!("app_modify/{} {}/{}", s("action"), s("app"), s("path")),
        "app_assets" => format!("app_assets/{} {}", s("action"), s("app")),
        "script_files" => format!("script_files/{} {}", s("action"), s("path")),
        "research_lookup" => format!("research_lookup/{} {}", s("scope"), quoted("query")),
        "web_search" => format!("web_search {:?}", s("query")),
        "academic_search" => format!("academic_search {:?}", s("query")),
        "discussion_search" => format!("discussion_search {:?}", s("query")),
        "search_files" => format!("search_files {:?}", s("query")),
        "search_sources" => format!("search_sources {:?}", s("query")),
        "list_citations" => "list_citations".to_string(),
        "read_file" => format!("read_file {}", s("name")),
        "read_pdf_page" => format!("read_pdf_page {} page {}", s("name"), s("page")),
        "read_app_file" => format!("read_app_file {}/{}", s("app"), s("path")),
        "grep_app" => format!("grep_app {} {:?}", s("app"), s("pattern")),
        "read_script" => format!("read_script {}", s("path")),
        "skill" => format!("skill {}", s("name")),
        "skill_admin" => format!("skill_admin {}", s("action")),
        "run_python" => format!("run_python {}", s("name")),
        "run_script" => format!("run_script {}", s("path")),
        "install_packages" => {
            let target = [s("skill"), s("app")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            format!("install_packages {target}")
        }
        "generate_image" | "generate_video" => {
            let prompt: String = s("prompt").chars().take(60).collect();
            format!("{name} {prompt:?}")
        }
        _ => name.to_string(),
    }
}

/// Marker prefix of `tool_result_unchanged_note` — a tool result omitted
/// because it is byte-identical to an earlier call with the same tool and
/// arguments. Both the live tool loop and `build_history` emit this exact
/// text for duplicates, which lets the loop recognize replayed notes when
/// seeding its dedup map. A real tool result beginning with this string
/// would at worst miss one dedup (and cause a single prompt-cache break),
/// never corrupt model-visible content.
pub const TOOL_RESULT_OMITTED_PREFIX: &str = "[result omitted: ";

/// One-line replacement for a tool result that duplicates an earlier call
/// with the same tool and arguments. The first full copy stays in the
/// conversation, so nothing is lost; the note keeps the duplicate from
/// re-entering the context on every subsequent request.
pub fn tool_result_unchanged_note(name: &str, args: &str) -> String {
    let label = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .map_or_else(|| name.to_string(), |v| batch_call_label(name, &v));
    format!(
        "{TOOL_RESULT_OMITTED_PREFIX}{label} returned exactly the same result as an earlier \
         call — content unchanged; the earlier result is above]"
    )
}

/// Bound every tool result before it is sent back to the model and persisted.
/// This is especially important for fetched web pages, which can otherwise
/// consume the conversation context one tool call at a time.
fn cap_tool_result(result: String) -> String {
    cap_result(result, MAX_TOOL_RESULT_CHARS)
}

fn cap_result(result: String, max_chars: usize) -> String {
    const SUFFIX: &str = "\n... (tool result truncated)";
    if result.chars().count() <= max_chars {
        return result;
    }
    let keep = max_chars.saturating_sub(SUFFIX.chars().count());
    let mut capped: String = result.chars().take(keep).collect();
    capped.push_str(SUFFIX);
    capped
}

/// Quick check: does a string look like a UUID (36 chars, 4 dashes)?
fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && std::path::Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// `cat -n`-style numbering for ranged reads, matching what agent harnesses
/// feed models so line references and edits anchor reliably.
/// Search imported files: embed the query and rank chunks by cosine when an
/// embedder is configured; otherwise (or when embedding fails / no vectors
/// are stored yet) fall back to FTS keywords, tagged so the model knows the
/// weaker path answered.
async fn search_files_impl(ctx: &FilesCtx, query: &str) -> String {
    let conn = match crate::db::open_attached(&ctx.db_path) {
        Ok(c) => c,
        Err(e) => return format!("file search failed: {e}"),
    };
    let mut fell_back = false;
    if let Some((provider, model)) = &ctx.embedder {
        match provider.embed(model, vec![query.to_string()]).await {
            Ok(mut vecs) if !vecs.is_empty() => {
                if let Some(out) = semantic_snippets(&conn, &ctx.space_id, &vecs.remove(0)) {
                    return out;
                }
                fell_back = true; // nothing embedded yet — keywords still help
            }
            _ => fell_back = true, // endpoint down — degrade, don't die
        }
    }
    match crate::db::search_chunks(&conn, &ctx.space_id, query, 8) {
        Ok(hits) if hits.is_empty() => "no matches".to_string(),
        Ok(hits) => {
            let body = hits
                .iter()
                .map(|(name, loc, snip)| format!("{name} ({loc}): {snip}"))
                .collect::<Vec<_>>()
                .join("\n");
            if fell_back {
                format!("(keyword fallback)\n{body}")
            } else {
                body
            }
        }
        Err(e) => format!("file search failed: {e}"),
    }
}

/// Top cosine-ranked chunks formatted as `name (location): text` (truncated),
/// or None when the space has no usable vectors.
fn semantic_snippets(conn: &rusqlite::Connection, space_id: &str, query: &[f32]) -> Option<String> {
    let hits = crate::db::semantic_chunks(conn, space_id, query, 8).ok()?;
    if hits.is_empty() {
        return None;
    }
    Some(
        hits.iter()
            .map(|(name, loc, text, _)| {
                let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let cut: String = flat.chars().take(300).collect();
                let ellipsis = if cut.len() < flat.len() { "…" } else { "" };
                format!("{name} ({loc}): {cut}{ellipsis}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn number_lines(slice: &[&str], start: usize) -> String {
    slice
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>5}\t{l}", start + i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// sha256("{1-based line number}:{content}") truncated to 8 hex chars —
/// stable for a given (position, content) pair, so a hash `edit_file` gets
/// back always resolves to the same line it was read from, and a line that
/// moved or changed since then simply won't match anything.
fn line_hash(line_no: usize, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{line_no}:{content}"));
    hasher
        .finalize()
        .iter()
        .take(4)
        .fold(String::new(), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        })
}

/// `read_app_file`'s line prefix: `N:HASH<tab>content`, one hash per line so
/// `edit_file` can target it without substring matching.
fn number_lines_with_hash(slice: &[&str], start: usize) -> String {
    slice
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let n = start + i + 1;
            format!("{:>5}:{}\t{l}", n, line_hash(n, l))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply hashline edits to `text`: each `(hash, new)` targets the line whose
/// current (1-based position, content) hashes to it (see `line_hash`) —
/// `new: None` deletes that line, `Some(t)` replaces it with `t`'s lines
/// (0, 1, or many — this is also how you insert: include the original
/// line's content in `t` alongside what's being added). Hashes are resolved
/// against `text` as given, so a stale hash fails loudly instead of
/// silently landing on the wrong line. Returns the new file content plus a
/// git-diff-style summary of what changed, in the order edits were given.
fn apply_hashline_edits(
    text: &str,
    edits: &[(String, Option<String>)],
) -> Result<(String, String), String> {
    if edits.is_empty() {
        return Err("edits must not be empty".to_string());
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut resolved: Vec<(usize, Option<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (hash, new) in edits {
        let idx = lines
            .iter()
            .enumerate()
            .position(|(i, l)| line_hash(i + 1, l) == *hash)
            .ok_or_else(|| {
                format!("hash {hash} not found — read the file again, it may have changed")
            })?;
        if !seen.insert(idx) {
            return Err(format!(
                "hash {hash} targets a line already edited by another entry in this call"
            ));
        }
        resolved.push((idx, new.clone()));
    }

    let mut diff = String::new();
    for (idx, new) in &resolved {
        let _ = write!(diff, "\n- {}", lines[*idx]);
        if let Some(t) = new {
            for l in t.lines() {
                let _ = write!(diff, "\n+ {l}");
            }
        }
    }

    // Apply highest index first so earlier (lower) indices, still unprocessed,
    // stay valid regardless of how many lines an edit adds or removes.
    let mut apply_order = resolved;
    apply_order.sort_by_key(|b| std::cmp::Reverse(b.0));
    let mut out: Vec<String> = lines.iter().map(std::string::ToString::to_string).collect();
    for (idx, new) in apply_order {
        match new {
            None => {
                out.remove(idx);
            }
            Some(t) => {
                let replacement: Vec<String> = t.lines().map(str::to_string).collect();
                out.splice(idx..=idx, replacement);
            }
        }
    }
    let mut new_text = out.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        new_text.push('\n');
    }
    Ok((new_text, diff))
}

/// Compare two complete file contents using git's unified-diff renderer. The
/// app files are not Git worktrees, so `--no-index` gives the same useful diff
/// without creating repository metadata or changing either file.
fn unified_diff(app: &str, path: &str, current: &str, candidate: &str) -> String {
    if current == candidate {
        return "no changes".to_string();
    }
    let dir = std::env::temp_dir().join(format!("nexus-diff-{}", uuid::Uuid::new_v4()));
    let before = dir.join("before");
    let after = dir.join("after");
    let result = (|| {
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create diff workspace: {e}"))?;
        std::fs::write(&before, current)
            .map_err(|e| format!("cannot write current snapshot: {e}"))?;
        std::fs::write(&after, candidate)
            .map_err(|e| format!("cannot write candidate snapshot: {e}"))?;
        let output = std::process::Command::new("git")
            .arg("diff")
            .arg("--no-index")
            .arg("--no-ext-diff")
            .arg("--unified=3")
            .arg(&before)
            .arg(&after)
            .output()
            .map_err(|e| format!("cannot run git diff: {e}"))?;
        match output.status.code() {
            Some(0) => Ok("no changes".to_string()),
            Some(1) => {
                let old_label = format!("a/{app}/{path}");
                let new_label = format!("b/{app}/{path}");
                let before_path = before.to_string_lossy();
                let after_path = after.to_string_lossy();
                let diff = String::from_utf8_lossy(&output.stdout)
                    .replace(before_path.as_ref(), &old_label)
                    .replace(after_path.as_ref(), &new_label);
                Ok(diff)
            }
            _ => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result.unwrap_or_else(|e| format!("diff failed: {e}"))
}

/// Apply the app root's `.gitignore` to reads and searches. Covers the common
/// Git ignore forms without adding a dependency: comments, negation, directory
/// rules, anchored paths, and `*`/`?` globs.
fn app_path_ignored(root: &std::path::Path, path: &std::path::Path) -> bool {
    if path.file_name().and_then(|n| n.to_str()) == Some(".gitignore") {
        return true;
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let rel = rel
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let Ok(rules) = std::fs::read_to_string(root.join(".gitignore")) else {
        return false;
    };
    let mut ignored = false;
    for raw in rules.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pattern) = match line.strip_prefix('!') {
            Some(pattern) => (true, pattern),
            None => (false, line),
        };
        let directory_rule = pattern.ends_with('/');
        let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
        let matches = if directory_rule {
            let mut prefix = String::new();
            let parts: Vec<&str> = rel.split('/').collect();
            parts[..parts.len().saturating_sub(1)].iter().any(|part| {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                gitignore_pattern_matches(pattern, &prefix)
            })
        } else {
            gitignore_pattern_matches(pattern, &rel)
        };
        if matches {
            ignored = !negated;
        }
    }
    ignored
}

fn gitignore_pattern_matches(pattern: &str, relative_path: &str) -> bool {
    if pattern.contains('/') {
        wildcard_match(pattern, relative_path)
    } else {
        relative_path
            .split('/')
            .any(|part| wildcard_match(pattern, part))
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let Some(p) = pattern.chars().next() else {
        return text.is_empty();
    };
    let Some(t) = text.chars().next() else {
        return p == '*';
    };
    match p {
        '*' => {
            wildcard_match(&pattern[p.len_utf8()..], text)
                || wildcard_match(pattern, &text[t.len_utf8()..])
        }
        '?' => wildcard_match(&pattern[p.len_utf8()..], &text[t.len_utf8()..]),
        _ if p == t => wildcard_match(&pattern[p.len_utf8()..], &text[t.len_utf8()..]),
        _ => false,
    }
}

/// Recursively collect `(relpath, line)` matches for a lowercase substring
/// pattern, skipping dependency/venv dirs and unreadable (binary) files.
fn grep_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    pattern: &str,
    out: &mut Vec<(String, usize)>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if app_path_ignored(root, &path) {
            continue;
        }
        let name = entry.file_name();
        if path.is_dir() {
            if name != "node_modules" && name != ".venv" && name != ".git" {
                grep_dir(root, &path, pattern, out);
            }
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            let rel = path.strip_prefix(root).unwrap_or(&path).display();
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(pattern) {
                    out.push((rel.to_string(), i + 1));
                }
            }
        }
    }
}

/// Resolve `<root>/<top>/<rel>`, rejecting anything that could escape `root`
/// (absolute paths, `..`/`.` segments, backslashes). Shared by the app and
/// skill-script tools.
fn resolve_confined(root: &std::path::Path, top: &str, rel: &str) -> Result<PathBuf, String> {
    if top.is_empty() || top.contains(['/', '\\']) || top == "." || top == ".." {
        return Err(format!("invalid name: {top:?}"));
    }
    if rel.is_empty() || rel.starts_with('/') {
        return Err(format!("path must be relative and non-empty: {rel:?}"));
    }
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
            return Err(format!("invalid path segment in {rel:?}"));
        }
    }
    let mut p = root.join(top);
    for seg in rel.split('/') {
        p.push(seg);
    }
    Ok(p)
}

/// Run a command with a timeout, kill-on-drop, and no shell. Returns the
/// raw output; spawn failures name the missing program.
async fn run_cmd(
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
    dir: &std::path::Path,
    secs: u64,
) -> Result<std::process::Output, String> {
    run_cmd_env(program, args, dir, secs, &[]).await
}

/// Like `run_cmd` but with extra environment variables.
async fn run_cmd_env(
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
    dir: &std::path::Path,
    secs: u64,
    envs: &[(&str, &str)],
) -> Result<std::process::Output, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).current_dir(dir).kill_on_drop(true);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let fut = cmd.output();
    match tokio::time::timeout(std::time::Duration::from_secs(secs), fut).await {
        Err(_) => Err(format!(
            "{} timed out after {secs}s",
            program.to_string_lossy()
        )),
        Ok(Err(e)) => Err(format!("cannot run {}: {e}", program.to_string_lossy())),
        Ok(Ok(out)) => Ok(out),
    }
}

/// Command output as tool-result text: stdout, then stderr, then a non-zero
/// exit code — truncated so a chatty script can't flood the context.
fn format_output(out: &std::process::Output) -> String {
    let mut s = String::from(String::from_utf8_lossy(&out.stdout).trim_end());
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("stderr:\n");
        s.push_str(err.trim_end());
    }
    let mut lines: Vec<&str> = s.lines().collect();
    if lines.len() > 200 {
        lines.truncate(200);
        lines.push("… (output truncated)");
    }
    let mut s = lines.join("\n");
    if s.chars().count() > 8000 {
        s = s.chars().take(8000).collect();
        s.push_str("\n… (output truncated)");
    }
    if !out.status.success() {
        let code = out
            .status
            .code()
            .map_or_else(|| "killed".to_string(), |c| c.to_string());
        if !s.is_empty() {
            s.push('\n');
        }
        let _ = write!(s, "exit code: {code}");
    }
    if s.is_empty() {
        s = "(no output)".to_string();
    }
    s
}

/// The python interpreter of a skill's own `.venv`, creating the venv (and
/// installing `requirements.txt` if the skill ships one) on first use.
/// Everything stays inside the skill's directory — nothing global.
async fn ensure_venv(skill_dir: &std::path::Path) -> Result<PathBuf, String> {
    let python = skill_dir.join(".venv/bin/python");
    if python.exists() {
        return Ok(python);
    }
    std::fs::create_dir_all(skill_dir)
        .map_err(|e| format!("cannot create {}: {e}", skill_dir.display()))?;
    // Corrupt venv from a system Python upgrade — nuke it and recreate.
    if skill_dir.join(".venv").exists() {
        std::fs::remove_dir_all(skill_dir.join(".venv"))
            .map_err(|e| format!("cannot remove corrupt venv: {e}"))?;
    }
    let out = run_cmd(
        "python3".as_ref(),
        &["-m".as_ref(), "venv".as_ref(), ".venv".as_ref()],
        skill_dir,
        120,
    )
    .await?;
    if !out.status.success() {
        return Err(format!("venv creation failed:\n{}", format_output(&out)));
    }
    if skill_dir.join("requirements.txt").exists() {
        let out = run_cmd(
            python.as_os_str(),
            &[
                "-m".as_ref(),
                "pip".as_ref(),
                "install".as_ref(),
                "-r".as_ref(),
                "requirements.txt".as_ref(),
            ],
            skill_dir,
            300,
        )
        .await?;
        if !out.status.success() {
            return Err(format!(
                "pip install -r requirements.txt failed:\n{}",
                format_output(&out)
            ));
        }
    }
    Ok(python)
}

/// Package names an installer may see: no flags, no whitespace — they land
/// in cmd directly, so a leading `-` would become an option injection.
fn validate_packages(pkgs: &[String]) -> Result<(), String> {
    if pkgs.is_empty() {
        return Err("no packages given".to_string());
    }
    for p in pkgs {
        if p.is_empty() || p.starts_with('-') || p.chars().any(char::is_whitespace) {
            return Err(format!("invalid package name: {p:?}"));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

/// Shared by backends that send a request and expect a JSON body back: send,
/// raise on a non-2xx status, then deserialize. `DuckDuckGo` scrapes HTML
/// instead of parsing JSON, so it doesn't use this helper.
async fn send_and_parse<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
) -> anyhow::Result<T> {
    req.send()
        .await?
        .error_for_status()?
        .json::<T>()
        .await
        .map_err(Into::into)
}

/// `SearXNG`'s JSON API needs `search: formats: [html, json]` enabled in the
/// instance's `settings.yml` — off by default. A misconfigured instance
/// surfaces as an HTML/error response here, which `error_for_status`/`json`
/// turns into a readable error for the model rather than a silent empty result.
async fn searxng_search(
    client: &reqwest::Client,
    base_url: &str,
    query: &str,
    recency: Option<&str>,
) -> anyhow::Result<Vec<SearchHit>> {
    let mut req = client
        .get(format!("{base_url}/search"))
        .query(&[("q", query), ("format", "json")]);
    if let Some(r) = recency {
        req = req.query(&[("time_range", r)]);
    }
    let resp = send_and_parse::<SearxngResponse>(req).await?;
    Ok(resp
        .results
        .into_iter()
        .take(8)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect())
}

#[derive(Deserialize)]
struct LangsearchResponse {
    data: Option<LangsearchData>,
}

#[derive(Deserialize)]
struct LangsearchData {
    #[serde(rename = "webPages")]
    web_pages: Option<LangsearchWebPages>,
}

#[derive(Deserialize)]
struct LangsearchWebPages {
    #[serde(default)]
    value: Vec<LangsearchResult>,
}

#[derive(Deserialize)]
struct LangsearchResult {
    name: String,
    url: String,
    #[serde(default)]
    snippet: String,
}

/// `LangSearch` (<https://langsearch.com)>: free-tier hosted search API, no card
/// required. More reliable than scraping `DuckDuckGo` when the service is
/// reachable; auto mode falls back when its endpoint is unavailable.
async fn langsearch_search(
    client: &reqwest::Client,
    key: &str,
    query: &str,
    recency: Option<&str>,
) -> anyhow::Result<Vec<SearchHit>> {
    let mut body = serde_json::json!({ "query": query, "count": 8 });
    if let Some(r) = recency {
        // LangSearch's freshness values are Bing-style camelCase.
        let freshness = match r {
            "day" => "oneDay",
            "week" => "oneWeek",
            "month" => "oneMonth",
            _ => "oneYear",
        };
        body["freshness"] = serde_json::json!(freshness);
    }
    let req = client
        .post("https://api.langsearch.com/v1/web-search")
        .bearer_auth(key)
        .json(&body);
    let resp = send_and_parse::<LangsearchResponse>(req).await?;
    Ok(resp
        .data
        .and_then(|d| d.web_pages)
        .map(|w| w.value)
        .unwrap_or_default()
        .into_iter()
        .map(|r| SearchHit {
            title: r.name,
            url: r.url,
            snippet: r.snippet,
        })
        .collect())
}

/// Zero-setup fallback used when no `SearXNG` instance is configured: scrapes
/// `DuckDuckGo`'s plain HTML search page (no JS, no API, no key) the same way
/// LM Studio/Open `WebUI`'s built-in `DuckDuckGo` tools do. Unofficial — `DuckDuckGo`
/// can change this markup or rate-limit it at any time; `SearXNG` is the more
/// durable option if this stops working for you.
async fn duckduckgo_search(
    client: &reqwest::Client,
    query: &str,
) -> anyhow::Result<Vec<SearchHit>> {
    let html = client
        .get("https://html.duckduckgo.com/html/")
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .query(&[("q", query)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    if html.contains("anomaly-modal") || html.contains("challenge-form") {
        anyhow::bail!("DuckDuckGo returned an anti-bot challenge")
    }
    Ok(parse_ddg_html(&html).into_iter().take(8).collect())
}

/// Last-resort zero-setup fallback. Brave's HTML endpoint currently remains
/// usable when `DuckDuckGo` serves its bot challenge, so keep this behind the
/// configured/API backends and only use it in auto mode.
async fn brave_search(client: &reqwest::Client, query: &str) -> anyhow::Result<Vec<SearchHit>> {
    let response = client
        .get("https://search.brave.com/search")
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
        )
        .query(&[("q", query)])
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await?;
    let status = response.status();
    let html = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("Brave returned HTTP {status}")
    }
    let hits = parse_brave_html(&html);
    if hits.is_empty() && html.to_ascii_lowercase().contains("captcha") {
        anyhow::bail!("Brave returned an anti-bot challenge")
    }
    Ok(hits.into_iter().take(8).collect())
}

/// Extract readable text from a fetched body: PDF (by content-type or
/// `%PDF` magic bytes) via `pdf-extract`, otherwise treated as HTML.
/// PDF extraction failures degrade to an explanatory string rather than
/// erroring the whole fetch — a scanned/malformed PDF shouldn't kill the
/// searcher's tool call.
fn extract_pdf_or_html(bytes: &[u8], content_type: &str) -> String {
    let looks_like_pdf =
        content_type.to_lowercase().contains("application/pdf") || bytes.starts_with(b"%PDF");
    if looks_like_pdf {
        return match pdf_extract::extract_text_from_mem(bytes) {
            Ok(text) => text.trim().to_string(),
            Err(e) => format!("[could not extract PDF text: {e}]"),
        };
    }
    let html = String::from_utf8_lossy(bytes);
    strip_html_to_text(&html)
}

/// GET `url` and return its readable text. Capped at 2MB of raw body and a
/// 30s timeout — a research searcher agent shouldn't be able to wedge on a
/// pathological page.
async fn fetch_url_text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let resp = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await?;
    let capped = &bytes[..bytes.len().min(2_000_000)];
    Ok(extract_pdf_or_html(capped, &content_type))
}

/// Whether `url` points at a `YouTube` watch page (long or short form).
fn is_youtube_url(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(u.host_str(), Some(h) if h == "youtube.com" || h.ends_with(".youtube.com") || h == "youtu.be")
}

/// Pull the first caption track's `baseUrl` out of a `YouTube` watch page's
/// embedded JSON (`ytInitialData`/`ytInitialPlayerResponse`). The value is
/// JSON-string-escaped (`\/` and `\uXXXX`); unescape just enough to get a
/// usable URL — a full JSON parse isn't needed for one field.
fn parse_caption_track_url(watch_page_html: &str) -> Option<String> {
    let marker = "\"baseUrl\":\"";
    let idx = watch_page_html.find(marker)? + marker.len();
    let end = watch_page_html[idx..].find('"')? + idx;
    let raw = &watch_page_html[idx..end];
    Some(raw.replace("\\/", "/").replace("\\u0026", "&"))
}

/// Join a `YouTube` timedtext XML transcript's `<text>` cue contents with
/// spaces into one plain-text string (no timing/markup kept — this is fed
/// to a research searcher, not rendered as captions).
fn strip_timedtext_xml(xml: &str) -> String {
    split_tag_blocks(xml, "text")
        .iter()
        .map(|inner| html_unescape_entities(inner))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Minimal HTML entity unescaping for cue text (`&amp;` last, so it doesn't
/// double-unescape compound entities — same ordering as `src/extract.rs`'s
/// `xml_unescape`).
fn html_unescape_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Fetch a `YouTube` video's transcript via the keyless timedtext endpoint:
/// scrape the watch page for a caption track URL, fetch it, and join the
/// cue text. Falls back to the normal page scrape when no caption track is
/// found (private/no-captions videos still return something searchable).
async fn fetch_youtube_transcript(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let watch_html = client
        .get(url)
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let Some(track_url) = parse_caption_track_url(&watch_html) else {
        return Ok(strip_html_to_text(&watch_html));
    };
    let xml = client
        .get(&track_url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(strip_timedtext_xml(&xml))
}

/// Pull `(title, url, snippet)` hits out of a `DuckDuckGo` HTML results page.
/// Each result is `<a class="result__a" href="...uddg=<url>...">title</a>`
/// followed by `<a class="result__snippet" ...>snippet</a>`.
fn parse_ddg_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("class=\"result__a\"") {
        let marker_at = pos + rel;
        let tag_start = html[..marker_at].rfind('<').unwrap_or(marker_at);
        let Some(gt) = html[marker_at..].find('>') else {
            break;
        };
        let tag = &html[tag_start..marker_at + gt];
        let text_start = marker_at + gt + 1;
        let Some(close_rel) = html[text_start..].find("</a>") else {
            break;
        };
        let title = strip_tags(&html[text_start..text_start + close_rel]);
        pos = text_start + close_rel + 4;

        let Some(href) = extract_attr(tag, "href") else {
            continue;
        };
        let Some(url) = resolve_ddg_href(&href) else {
            continue;
        };
        let snippet = find_snippet(html, pos);
        if !title.is_empty() {
            hits.push(SearchHit {
                title,
                url,
                snippet,
            });
        }
    }
    hits
}

/// Pull result cards from Brave's server-rendered HTML. This intentionally
/// relies only on stable semantic class names and returns ordinary links, so
/// callers can use the same formatter/citation path as API-backed results.
fn parse_brave_html(html: &str) -> Vec<SearchHit> {
    const TITLE_MARKER: &str = "<div class=\"title search-snippet-title";
    let mut hits = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find(TITLE_MARKER) {
        let marker_at = pos + rel;
        let Some(gt_rel) = html[marker_at..].find('>') else {
            break;
        };
        let text_start = marker_at + gt_rel + 1;
        let Some(close_rel) = html[text_start..].find("</div>") else {
            break;
        };
        let title = strip_tags(&html[text_start..text_start + close_rel]);
        let title_end = text_start + close_rel + "</div>".len();
        let Some(anchor_start) = html[..marker_at].rfind("<a ") else {
            pos = title_end;
            continue;
        };
        let Some(anchor_gt_rel) = html[anchor_start..].find('>') else {
            pos = title_end;
            continue;
        };
        let anchor_tag = &html[anchor_start..=(anchor_start + anchor_gt_rel)];
        let Some(url) = extract_attr(anchor_tag, "href") else {
            pos = title_end;
            continue;
        };
        let url = html_unescape(&url);
        if !url.starts_with("http") || title.is_empty() {
            pos = title_end;
            continue;
        }

        let segment_end = html[title_end..]
            .find(TITLE_MARKER)
            .map_or(html.len(), |end| title_end + end);
        let snippet = html[title_end..segment_end]
            .find("generic-snippet")
            .and_then(|generic| {
                let start = title_end + generic;
                let content = html[start..segment_end].find("class=\"content")?;
                let content_start = start + content;
                let gt = html[content_start..segment_end].find('>')?;
                let text_start = content_start + gt + 1;
                let close = html[text_start..segment_end].find("</div>")?;
                Some(strip_tags(&html[text_start..text_start + close]))
            })
            .unwrap_or_default();
        hits.push(SearchHit {
            title,
            url,
            snippet,
        });
        pos = title_end;
    }
    hits
}

/// The snippet immediately following a result's title anchor, if any.
fn find_snippet(html: &str, from: usize) -> String {
    let marker = "class=\"result__snippet\"";
    let Some(rel) = html[from..].find(marker) else {
        return String::new();
    };
    let idx = from + rel;
    let Some(gt) = html[idx..].find('>') else {
        return String::new();
    };
    let text_start = idx + gt + 1;
    let Some(close) = html[text_start..].find("</a>") else {
        return String::new();
    };
    strip_tags(&html[text_start..text_start + close])
}

/// `DuckDuckGo`'s result links redirect through `/l/?uddg=<percent-encoded-url>`.
fn resolve_ddg_href(href: &str) -> Option<String> {
    if href.contains("uddg=") {
        let absolute = if let Some(rest) = href.strip_prefix("//") {
            format!("https://{rest}")
        } else if href.starts_with("http") {
            href.to_string()
        } else {
            format!("https://duckduckgo.com{href}")
        };
        let decoded = reqwest::Url::parse(&absolute)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(k, _)| k == "uddg")
                    .map(|(_, v)| v.into_owned())
            })
            .unwrap_or_default();
        return (!decoded.is_empty()).then_some(decoded);
    }
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    href.starts_with("http").then(|| href.to_string())
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let idx = tag.find(&marker)? + marker.len();
    let end = tag[idx..].find('"')? + idx;
    Some(tag[idx..end].to_string())
}

/// Drop HTML tags and unescape entities, for anchor text pulled out of raw markup.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_unescape(out.trim())
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

/// Remove every `<tag>...</tag>` block (case-sensitive on the lowercase tag
/// name callers pass, e.g. "script"/"style") including its content. An
/// unterminated opening tag drops the remainder of the string rather than
/// looping forever or panicking on a truncated/malformed fetch.
fn drop_tag_blocks(html: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        match rest.find(&open) {
            None => {
                out.push_str(rest);
                break;
            }
            Some(start) => {
                out.push_str(&rest[..start]);
                match rest[start..].find(&close) {
                    None => break,
                    Some(end_rel) => {
                        rest = &rest[start + end_rel + close.len()..];
                    }
                }
            }
        }
    }
    out
}

/// Pull every top-level `<table>...</table>` block out of `html`, replacing
/// it with a markdown pipe-table rendering. Runs before the generic
/// tag-stripper so table structure survives; a `<table>` nested inside
/// another is left as inner markup (rendered as flattened text by the
/// generic stripper afterward) rather than recursed into — good enough for
/// the benchmark/pricing tables research actually hits.
fn render_tables_as_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find("<table") {
        out.push_str(&rest[..start]);
        let Some(tag_end) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let body_start = start + tag_end + 1;
        let Some(close_rel) = rest[body_start..].find("</table>") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let body_end = body_start + close_rel;
        out.push_str(&table_to_markdown(&rest[body_start..body_end]));
        rest = &rest[body_end + "</table>".len()..];
    }
    out.push_str(rest);
    out
}

/// Render one table's inner HTML (rows of `<tr>`, cells `<th>`/`<td>`) as a
/// GitHub-style pipe table. Header row = first `<tr>`'s cells; a `---`
/// separator follows it unconditionally (even if that row used `<td>`, not
/// `<th>` — most scraped tables don't bother with `<th>`).
fn table_to_markdown(table_html: &str) -> String {
    let rows: Vec<Vec<String>> = split_tag_blocks(table_html, "tr")
        .iter()
        .map(|row_html| {
            let mut cells: Vec<String> = split_tag_blocks(row_html, "th")
                .iter()
                .map(|c| strip_tags(c).replace('\n', " ").trim().to_string())
                .collect();
            cells.extend(
                split_tag_blocks(row_html, "td")
                    .iter()
                    .map(|c| strip_tags(c).replace('\n', " ").trim().to_string()),
            );
            cells
        })
        .filter(|r| !r.is_empty())
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows[0].len();
    let mut out = String::from("\n");
    let _ = writeln!(out, "| {} |", rows[0].join(" | "));
    let _ = writeln!(out, "| {} |", vec!["---"; cols].join(" | "));
    for row in &rows[1..] {
        let _ = writeln!(out, "| {} |", row.join(" | "));
    }
    out.push('\n');
    out
}

/// Every top-level `<tag>...</tag>` block's inner HTML, in order. Does not
/// recurse into nested same-named tags — a nested `<tr>` inside a cell (rare,
/// malformed markup) is left as part of the outer block's text.
fn split_tag_blocks(html: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find(&open) {
        let Some(tag_end) = rest[start..].find('>') else {
            break;
        };
        let body_start = start + tag_end + 1;
        let Some(close_rel) = rest[body_start..].find(&close) else {
            break;
        };
        let body_end = body_start + close_rel;
        out.push(rest[body_start..body_end].to_string());
        rest = &rest[body_end + close.len()..];
    }
    out
}

/// HTML page body → plain readable text: drop script/style blocks, strip all
/// remaining tags, unescape entities, and collapse blank/whitespace-only
/// lines so paginated output isn't mostly empty lines.
fn strip_html_to_text(html: &str) -> String {
    let no_script = drop_tag_blocks(html, "script");
    let no_style = drop_tag_blocks(&no_script, "style");
    let with_tables = render_tables_as_markdown(&no_style);
    strip_tags(&with_tables)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One scholarly-paper hit, flattened from the Semantic Scholar response.
struct Paper {
    title: String,
    authors: Vec<String>,
    year: Option<i64>,
    venue: Option<String>,
    abstract_snippet: Option<String>,
    citation_count: Option<i64>,
    url: String,
}

#[derive(Deserialize)]
struct SemanticScholarResponse {
    #[serde(default)]
    data: Vec<SemanticScholarPaper>,
}

#[derive(Deserialize)]
struct SemanticScholarPaper {
    title: String,
    #[serde(default)]
    authors: Vec<SemanticScholarAuthor>,
    year: Option<i64>,
    venue: Option<String>,
    #[serde(rename = "abstract")]
    abstract_snippet: Option<String>,
    #[serde(rename = "citationCount")]
    citation_count: Option<i64>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct SemanticScholarAuthor {
    name: String,
}

/// Semantic Scholar Graph API (api.semanticscholar.org): free, keyless.
/// A 429 (rate limited) surfaces as an error the caller turns into
/// tool-result text — the model falls back to search(mode=web).
async fn academic_search(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<Paper>> {
    let req = client
        .get("https://api.semanticscholar.org/graph/v1/paper/search")
        .query(&[
            ("query", query),
            ("limit", &limit.min(20).to_string()),
            (
                "fields",
                "title,authors,year,venue,abstract,citationCount,url",
            ),
        ]);
    let resp = send_and_parse::<SemanticScholarResponse>(req).await?;
    Ok(resp
        .data
        .into_iter()
        .map(|p| Paper {
            title: p.title,
            authors: p.authors.into_iter().map(|a| a.name).collect(),
            year: p.year,
            venue: p.venue,
            abstract_snippet: p.abstract_snippet,
            citation_count: p.citation_count,
            url: p.url.unwrap_or_default(),
        })
        .collect())
}

/// Numbered scholarly-paper results the model cites the same way as
/// `format_results`' web hits: `[n]` inline, matched against this list.
fn format_papers(papers: &[Paper]) -> String {
    papers
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut meta = Vec::new();
            if !p.authors.is_empty() {
                meta.push(p.authors.join(", "));
            }
            if let Some(y) = p.year {
                meta.push(y.to_string());
            }
            if let Some(v) = &p.venue {
                meta.push(v.clone());
            }
            if let Some(c) = p.citation_count {
                meta.push(format!("{c} citations"));
            }
            let abs = p.abstract_snippet.as_deref().unwrap_or("");
            format!(
                "[{}] {}\n    {}\n    {abs}\n    {}",
                i + 1,
                p.title,
                meta.join(" · "),
                p.url
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One discussion-forum hit (Hacker News story or Reddit post), flattened
/// to what the model needs to decide whether to `fetch_url` it.
struct DiscussionHit {
    title: String,
    url: String,
    meta: String,
}

#[derive(Deserialize)]
struct HnSearchResponse {
    #[serde(default)]
    hits: Vec<HnHit>,
}

#[derive(Deserialize)]
struct HnHit {
    title: Option<String>,
    url: Option<String>,
    #[serde(rename = "objectID")]
    object_id: String,
    #[serde(default)]
    points: i64,
    #[serde(default, rename = "num_comments")]
    num_comments: i64,
}

async fn hn_search(client: &reqwest::Client, query: &str) -> anyhow::Result<Vec<DiscussionHit>> {
    let req = client
        .get("https://hn.algolia.com/api/v1/search")
        .query(&[("query", query), ("tags", "story")]);
    let resp = send_and_parse::<HnSearchResponse>(req).await?;
    Ok(resp
        .hits
        .into_iter()
        .take(8)
        .map(|h| {
            let url = h
                .url
                .unwrap_or_else(|| format!("https://news.ycombinator.com/item?id={}", h.object_id));
            DiscussionHit {
                title: h.title.unwrap_or_else(|| "(untitled)".to_string()),
                url,
                meta: format!("{} points, {} comments", h.points, h.num_comments),
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct RedditSearchResponse {
    data: RedditListing,
}

#[derive(Deserialize)]
struct RedditListing {
    #[serde(default)]
    children: Vec<RedditChild>,
}

#[derive(Deserialize)]
struct RedditChild {
    data: RedditPost,
}

#[derive(Deserialize)]
struct RedditPost {
    title: String,
    permalink: String,
    subreddit: String,
    #[serde(default)]
    score: i64,
}

async fn reddit_search(
    client: &reqwest::Client,
    query: &str,
) -> anyhow::Result<Vec<DiscussionHit>> {
    let req = client
        .get("https://www.reddit.com/search.json")
        .header("User-Agent", "Mozilla/5.0 (compatible; nexus-chat)")
        .query(&[("q", query), ("sort", "relevance"), ("limit", "8")]);
    let resp = send_and_parse::<RedditSearchResponse>(req).await?;
    Ok(resp
        .data
        .children
        .into_iter()
        .map(|c| DiscussionHit {
            title: c.data.title,
            url: format!("https://reddit.com{}", c.data.permalink),
            meta: format!("r/{}, {} upvotes", c.data.subreddit, c.data.score),
        })
        .collect())
}

fn is_reddit_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "reddit.com" || host.ends_with(".reddit.com"))
}

/// Numbered discussion results, HN first then Reddit — same `[n]` citation
/// convention as `format_results`/`format_papers`.
fn format_discussion_hits(hn: &[DiscussionHit], reddit: &[DiscussionHit]) -> String {
    hn.iter()
        .chain(reddit.iter())
        .enumerate()
        .map(|(i, h)| format!("[{}] {}\n    {}\n    {}", i + 1, h.title, h.meta, h.url))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// HN (Algolia) + Reddit search run concurrently; either backend failing
/// independently still returns the other's hits. If both APIs are blocked,
/// use Brave's HTML index for Reddit links rather than silently reporting no
/// results.
async fn discussion_search(client: &reqwest::Client, query: &str) -> String {
    let (hn, reddit) = tokio::join!(hn_search(client, query), reddit_search(client, query));
    let hn = hn.unwrap_or_default();
    let reddit = reddit.unwrap_or_default();
    if !hn.is_empty() || !reddit.is_empty() {
        return format_discussion_hits(&hn, &reddit);
    }

    let fallback_query = format!("site:reddit.com {query}");
    let fallback = brave_search(client, &fallback_query)
        .await
        .unwrap_or_default();
    let reddit: Vec<DiscussionHit> = fallback
        .into_iter()
        .filter(|hit| is_reddit_url(&hit.url))
        .take(8)
        .map(|hit| DiscussionHit {
            title: hit.title,
            url: hit.url,
            meta: "Reddit web result (API unavailable)".to_string(),
        })
        .collect();
    if reddit.is_empty() {
        "no results".to_string()
    } else {
        format_discussion_hits(&[], &reddit)
    }
}

/// Normalize a source URL for dedup: lowercase the host only (path/query
/// case is preserved — some servers are case-sensitive there), strip
/// `utm_*`/`fbclid` query params, and drop a trailing `/` and any fragment.
/// Unparseable input (not actually a URL) is returned unchanged so it still
/// participates in a plain string-equality dedup.
pub fn normalize_url(url: &str) -> String {
    let Ok(mut u) = reqwest::Url::parse(url) else {
        return url.to_string();
    };
    u.set_fragment(None);
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| k != "fbclid" && !k.starts_with("utm_"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else {
        let q = kept
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        u.set_query(Some(&q));
    }
    if let Some(h) = u.host_str().map(str::to_lowercase) {
        let _ = u.set_host(Some(&h));
    }
    if u.path().ends_with('/') && u.path() != "/" {
        let trimmed = u.path().trim_end_matches('/').to_string();
        u.set_path(&trimmed);
    }
    let mut s = u.to_string();
    if let Some(stripped) = s.strip_suffix('/') {
        s = stripped.to_string();
    }
    s
}

/// Collapse duplicate cited sources across a set of Searcher findings.
/// Each finding may end with a `Sources:` block of `N. url` lines (see
/// `SEARCHER_PROMPT` in research.rs); a source line whose normalized URL
/// already appeared in an earlier finding is dropped from later ones so
/// the Synthesizer doesn't see the same source cited from every angle.
/// Non-source lines are untouched.
pub fn dedup_source_lines(findings: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    findings
        .iter()
        .map(|f| {
            let mut out_lines = Vec::new();
            let mut in_sources = false;
            for line in f.lines() {
                if line.trim().eq_ignore_ascii_case("Sources:") {
                    in_sources = true;
                    out_lines.push(line.to_string());
                    continue;
                }
                if in_sources
                    && let Some((_, url)) = line.trim().split_once(['.', ')'])
                    && !seen.insert(normalize_url(url.trim()))
                {
                    continue; // dup — drop this line
                }
                out_lines.push(line.to_string());
            }
            out_lines.join("\n")
        })
        .collect()
}

/// Every normalized URL cited in a set of findings' `Sources:` blocks —
/// what gets linked into a research session's source bundle.
pub fn cited_url_norms(findings: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in findings {
        let mut in_sources = false;
        for line in f.lines() {
            if line.trim().eq_ignore_ascii_case("Sources:") {
                in_sources = true;
                continue;
            }
            if in_sources && let Some((_, url)) = line.trim().split_once(['.', ')']) {
                let url = url.trim();
                if !url.is_empty() {
                    out.push(normalize_url(url));
                }
            }
        }
    }
    out
}

/// Rewrite `query` with `site:`/`-site:` terms — backend-agnostic (every
/// engine this app talks to honors Google-style site filters), so
/// `include_domains`/`exclude_domains`/`blocked_domains` need no per-backend
/// plumbing beyond this string rewrite.
pub fn rewrite_query_with_domains(query: &str, include: &[String], exclude: &[String]) -> String {
    let mut q = query.to_string();
    for d in include {
        let _ = write!(q, " site:{d}");
    }
    for d in exclude {
        let _ = write!(q, " -site:{d}");
    }
    q
}

/// Perplexity-style numbered results the model cites inline as `[n]`.
fn format_results(hits: &[SearchHit]) -> String {
    hits.iter()
        .enumerate()
        .map(|(i, h)| format!("[{}] {}\n    {}\n    {}", i + 1, h.title, h.url, h.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ── Video generation helpers ──────────────────────────────────────────────────

/// Try to read an image file from the files directory. Accepts the id as-is or
/// with a `.png` extension appended.
fn resolve_image(files_dir: &std::path::Path, id: &str) -> Option<Vec<u8>> {
    if !valid_relative_path(id) {
        return None;
    }
    let direct = files_dir.join(id);
    std::fs::read(&direct)
        .ok()
        .or_else(|| std::fs::read(files_dir.join(format!("{id}.png"))).ok())
}

/// Extract a single frame from a video via ffmpeg subprocess. When `use_eof`
/// is true, extracts the last frame (-sseof). Returns whether ffmpeg ran
/// successfully.
fn extract_ffmpeg_frame(
    video_path: &std::path::Path,
    output_path: &std::path::Path,
    use_eof: bool,
) -> bool {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-y");
    if use_eof {
        cmd.arg("-sseof").arg("-0.1");
    } else {
        cmd.arg("-ss").arg("0");
    }
    cmd.arg("-i")
        .arg(video_path)
        .arg("-vframes")
        .arg("1")
        .arg("-f")
        .arg("image2")
        .arg(output_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Read the `_video_refs.json` reference registry from the files directory.
/// Returns an empty JSON object `{}` if the file doesn't exist.
fn read_video_refs(files_dir: &std::path::Path) -> serde_json::Value {
    let path = files_dir.join("_video_refs.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Write the reference registry to `_video_refs.json` in the files directory.
fn write_video_refs(files_dir: &std::path::Path, refs: &serde_json::Value) -> anyhow::Result<()> {
    let path = files_dir.join("_video_refs.json");
    let json = serde_json::to_string_pretty(refs)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Check whether ffmpeg is available on $PATH.
fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Resolve named character and location references to image data from the
/// `_video_refs.json` registry.
fn resolve_named_references(
    files_dir: &std::path::Path,
    character_refs: &[String],
    location_refs: &[String],
) -> Vec<Vec<u8>> {
    let refs = read_video_refs(files_dir);
    let mut images = Vec::new();
    for name in character_refs.iter().chain(location_refs.iter()) {
        if let Some(entry) = refs.get(name)
            && let Some(image_id) = entry.get("image_id").and_then(|id| id.as_str())
            && let Some(data) = resolve_image(files_dir, image_id)
        {
            images.push(data);
        }
    }
    images
}

/// Build an ffmpeg `crop` + `scale` filter string for a camera move preset.
/// `margin` (0–1) controls how much of the frame edge is revealed (pan) or
/// how much the frame zooms in (dolly). 0.15 = 15% movement/zoom.
fn build_camera_filter(move_type: &str, margin: f64) -> String {
    // t = time in sec, du = input duration in sec, iw/ih = input width/height
    match move_type {
        "dolly_in" => {
            // Start full frame, end zoomed in centered
            let zoom = margin; // e.g. 0.15 → 15% zoom
            format!(
                "crop=w='iw-(iw*{zoom})*t/du':h='ih-(ih*{zoom})*t/du':x='(iw-w)/2':y='(ih-h)/2',scale=iw:ih"
            )
        }
        "dolly_out" => {
            let zoom = margin;
            format!(
                "crop=w='iw-(iw*{zoom})*(1-t/du)':h='ih-(ih*{zoom})*(1-t/du)':x='(iw-w)/2':y='(ih-h)/2',scale=iw:ih"
            )
        }
        "pan_left" => {
            // Crop window slides from right to left
            let m = margin.max(0.05);
            format!(
                "crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)*(1-t/du)':y='(ih-h)/2',scale=iw:ih"
            )
        }
        "pan_right" => {
            let m = margin.max(0.05);
            format!("crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)*t/du':y='(ih-h)/2',scale=iw:ih")
        }
        "tilt_up" => {
            let m = margin.max(0.05);
            format!(
                "crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)/2':y='(ih-h)*(1-t/du)',scale=iw:ih"
            )
        }
        "tilt_down" => {
            let m = margin.max(0.05);
            format!("crop=w='iw*(1-{m})':h='ih*(1-{m})':x='(iw-w)/2':y='(ih-h)*t/du',scale=iw:ih")
        }
        _ => String::new(),
    }
}

/// Build an ffmpeg filter string for a lighting/color preset, scaled by
/// intensity (0–1).
fn build_lighting_filter(preset: &str, intensity: f64) -> String {
    let i = intensity.clamp(0.0, 1.0);
    match preset {
        "noir" => {
            let b = -0.05 * i;
            let c = 0.3f64.mul_add(i, 1.0);
            let s = 0.8f64.mul_add(-i, 1.0);
            format!(
                "eq=brightness={b:.3}:contrast={c:.3}:saturation={s:.3},colorbalance=rh={rh:.3}:gh={gh:.3}:bh={bh:.3}",
                b = b,
                c = c,
                s = s,
                rh = -0.1 * i,
                gh = -0.05 * i,
                bh = 0.1 * i
            )
        }
        "warm" => {
            let s = 0.3f64.mul_add(i, 1.0);
            format!(
                "eq=saturation={s:.3},colorbalance=rs={rs:.3}:gs={gs:.3}:bs={bs:.3}",
                s = s,
                rs = 0.1 * i,
                gs = 0.05 * i,
                bs = -0.05 * i
            )
        }
        "cold" => {
            let s = 0.1f64.mul_add(-i, 1.0);
            format!(
                "eq=saturation={s:.3},colorbalance=rs={rs:.3}:gs={gs:.3}:bs={bs:.3}",
                s = s,
                rs = -0.05 * i,
                gs = -0.02 * i,
                bs = 0.1 * i
            )
        }
        "vintage" => {
            let b = 0.03 * i;
            let c = 0.1f64.mul_add(-i, 1.0);
            let s = 0.3f64.mul_add(-i, 1.0);
            format!(
                "eq=brightness={b:.3}:contrast={c:.3}:saturation={s:.3},colorbalance=rh={rh:.3}:rm={rm:.3}:gs={gs:.3}",
                b = b,
                c = c,
                s = s,
                rh = 0.05 * i,
                rm = 0.05 * i,
                gs = -0.05 * i
            )
        }
        "vivid" => {
            let s = 0.5f64.mul_add(i, 1.0);
            format!("eq=saturation={s:.3}:contrast=1.1:brightness=0.02")
        }
        "bleach_bypass" => {
            let c = 0.4f64.mul_add(i, 1.0);
            let s = 0.6f64.mul_add(-i, 1.0);
            format!(
                "eq=contrast={c:.3}:saturation={s:.3}:brightness=0.02:gamma={g:.3}",
                c = c,
                s = s,
                g = 0.1f64.mul_add(i, 1.0)
            )
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_sources_tool_only_appears_and_works_for_a_research_session_toolbox() {
        let dir = std::env::temp_dir().join(format!("nexus-searchsrc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Own directory: the attached sibling cache.db must not collide with
        // other tests' dbs, all of which live flat in the temp dir.
        let path = dir.join("nexus.db");
        let db = crate::db::Db::open(&path).unwrap();
        let space = db.default_space_id().unwrap();
        let s = db.create_session("t", "a/b", &space, "chat").unwrap();
        crate::db::cache_put(
            db.raw(),
            "https://example.com/a",
            "https://example.com/a",
            None,
            "rust borrow checker notes",
        )
        .unwrap();
        db.add_session_sources(&s.id, &["https://example.com/a".to_string()])
            .unwrap();

        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(path.clone()),
            None,
            None,
        );
        assert!(!tb.defs().iter().any(|d| d.name == "research_lookup"));

        let tb = tb.with_research_session(s.id.clone());
        assert!(tb.defs().iter().any(|d| d.name == "research_lookup"));
        let (result, _) = tb
            .run(
                "research_lookup",
                r#"{"scope":"session_sources","query":"borrow checker"}"#,
            )
            .await;
        assert!(result.contains("borrow checker"), "{result}");

        let (result, _) = tb
            .run(
                "research_lookup",
                r#"{"scope":"session_sources","query":"quantum"}"#,
            )
            .await;
        assert!(result.contains("no matches"), "{result}");
    }

    #[tokio::test]
    async fn list_citations_reports_recorded_sources_and_filters_by_query() {
        let (tb, db, space) = files_toolbox();
        db.add_citations(
            &space,
            "research-a.md",
            &[("https://nature.com/x".to_string(), None)],
        )
        .unwrap();
        let (result, _) = tb.run("research_lookup", r#"{"scope":"citations"}"#).await;
        assert!(result.contains("research-a.md"), "{result}");
        assert!(result.contains("nature.com"), "{result}");

        let (result, _) = tb
            .run("research_lookup", r#"{"scope":"citations","query":"nope"}"#)
            .await;
        assert!(result.contains("no citations"), "{result}");
    }

    #[tokio::test]
    async fn fetch_url_serves_from_cache_when_fresh() {
        let dir = std::env::temp_dir().join(format!("nexus-webcache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.db");
        let db = crate::db::Db::open(&path).unwrap();
        let cached_body = "x".repeat(MAX_TOOL_RESULT_CHARS + 100);
        crate::db::cache_put(
            db.raw(),
            "https://example.com/a",
            "https://example.com/a",
            None,
            &cached_body,
        )
        .unwrap();
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(path),
            None,
            None,
        );
        // A cache hit must not attempt the network — the result is the cached
        // text, not a "fetch failed" error.
        let (result, _) = tb
            .run("fetch_url", r#"{"url":"https://example.com/a"}"#)
            .await;
        assert_eq!(result.chars().count(), MAX_TOOL_RESULT_CHARS);
        assert!(result.ends_with("... (tool result truncated)"), "{result}");
    }

    #[test]
    fn normalize_url_lowercases_host_strips_tracking_params_and_trailing_slash() {
        assert_eq!(
            normalize_url(
                "HTTPS://Example.COM/Page/?utm_source=x&utm_medium=y&id=1&fbclid=abc#frag"
            ),
            "https://example.com/Page?id=1"
        );
        assert_eq!(normalize_url("https://example.com/"), "https://example.com");
        assert_eq!(normalize_url("https://example.com"), "https://example.com");
        assert_eq!(normalize_url("not a url"), "not a url");
    }

    #[test]
    fn cited_url_norms_extracts_every_sources_url() {
        let f = "text [1]\nSources:\n1. https://a.example/\n2. https://b.example?utm_source=x";
        assert_eq!(
            cited_url_norms(&[f.to_string()]),
            vec!["https://a.example", "https://b.example"]
        );
    }

    #[test]
    fn dedup_source_lines_keeps_first_occurrence_of_each_normalized_url() {
        let a = "Finding A body. [1]\nSources:\n1. https://example.com/a\n2. https://example.com/b?utm_source=x";
        let b = "Finding B body. [1]\nSources:\n1. https://EXAMPLE.com/a/\n2. https://other.com/c";
        let out = dedup_source_lines(&[a.to_string(), b.to_string()]);
        assert!(out[0].contains("https://example.com/a"));
        assert!(out[0].contains("https://example.com/b"));
        // b's first line (a dup of a's [1]) is dropped; its second (new) survives.
        assert!(!out[1].contains("example.com/a"));
        assert!(out[1].contains("other.com/c"));
    }

    #[test]
    fn rewrite_query_with_domains_appends_site_and_negated_site_terms() {
        let out = rewrite_query_with_domains(
            "rust async runtimes",
            &["docs.rs".into()],
            &["reddit.com".into(), "quora.com".into()],
        );
        assert_eq!(
            out,
            "rust async runtimes site:docs.rs -site:reddit.com -site:quora.com"
        );
        assert_eq!(rewrite_query_with_domains("q", &[], &[]), "q");
    }

    #[test]
    fn formats_papers_as_numbered_list_with_metadata() {
        let papers = vec![Paper {
            title: "Attention Is All You Need".into(),
            authors: vec!["A. Vaswani".into(), "N. Shazeer".into()],
            year: Some(2017),
            venue: Some("NeurIPS".into()),
            abstract_snippet: Some("We propose a new architecture...".into()),
            citation_count: Some(90000),
            url: "https://www.semanticscholar.org/paper/abc".into(),
        }];
        let out = format_papers(&papers);
        assert!(out.contains("[1] Attention Is All You Need"));
        assert!(out.contains("A. Vaswani, N. Shazeer"));
        assert!(out.contains("2017"));
        assert!(out.contains("NeurIPS"));
        assert!(out.contains("90000 citations"));
        assert!(out.contains("https://www.semanticscholar.org/paper/abc"));
    }

    #[test]
    fn format_papers_handles_missing_optional_fields() {
        let papers = vec![Paper {
            title: "Untitled Preprint".into(),
            authors: vec![],
            year: None,
            venue: None,
            abstract_snippet: None,
            citation_count: None,
            url: "https://x".into(),
        }];
        let out = format_papers(&papers);
        assert!(out.contains("[1] Untitled Preprint"));
        assert!(out.contains("https://x"));
    }

    #[test]
    fn format_discussion_hits_numbers_hn_then_reddit_with_metadata() {
        let hn = vec![DiscussionHit {
            title: "Rust 1.90 released".to_string(),
            url: "https://example.com/rust-190".to_string(),
            meta: "312 points, 88 comments".to_string(),
        }];
        let reddit = vec![DiscussionHit {
            title: "What do you think of Rust 1.90?".to_string(),
            url: "https://reddit.com/r/rust/abc".to_string(),
            meta: "r/rust, 245 upvotes".to_string(),
        }];
        let text = format_discussion_hits(&hn, &reddit);
        assert!(text.contains("[1] Rust 1.90 released"), "{text:?}");
        assert!(text.contains("312 points, 88 comments"), "{text:?}");
        assert!(
            text.contains("[2] What do you think of Rust 1.90?"),
            "{text:?}"
        );
        assert!(text.contains("r/rust, 245 upvotes"), "{text:?}");
    }

    #[test]
    fn format_discussion_hits_empty_both_yields_empty_string() {
        assert_eq!(format_discussion_hits(&[], &[]), "");
    }

    #[tokio::test]
    async fn discussion_search_serves_from_cache_when_fresh() {
        let dir = std::env::temp_dir().join(format!("nexus-discache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.db");
        let db = crate::db::Db::open(&path).unwrap();
        let cache_key = "discussion://rust performance";
        let cached_response =
            "[1] Rust is fast\n    HN · 100 points\n    https://news.ycombinator.com/rust";
        crate::db::cache_put(db.raw(), cache_key, cache_key, None, cached_response).unwrap();
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(path),
            None,
            None,
        );
        // A cache hit must not attempt the network — the result is the cached
        // text, not a "no results" or "search failed" error.
        let (result, _) = tb
            .run(
                "search",
                r#"{"mode":"discussion","query":"rust performance"}"#,
            )
            .await;
        assert!(result.contains("Rust is fast"), "{result}");
        assert!(
            result.contains("https://news.ycombinator.com/rust"),
            "{result}"
        );
    }

    #[test]
    fn parses_semantic_scholar_response_json() {
        let json = r#"{"data":[
            {"title":"A","authors":[{"name":"X"}],"year":2020,"venue":"V","abstract":"abs","citationCount":5,"url":"https://s2/a"}
        ]}"#;
        let resp: SemanticScholarResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].title, "A");
        assert_eq!(resp.data[0].authors[0].name, "X");
    }

    #[test]
    fn formats_results_as_numbered_list() {
        let hits = vec![
            SearchHit {
                title: "Rust 1.90".into(),
                url: "https://a".into(),
                snippet: "release notes".into(),
            },
            SearchHit {
                title: "Rust blog".into(),
                url: "https://b".into(),
                snippet: "announcement".into(),
            },
        ];
        let out = format_results(&hits);
        assert!(out.starts_with("[1] Rust 1.90\n    https://a\n    release notes"));
        assert!(out.contains("[2] Rust blog"));
    }

    #[test]
    fn caps_tool_results_before_context_replay() {
        let out = cap_tool_result("x".repeat(MAX_TOOL_RESULT_CHARS + 100));
        assert_eq!(out.chars().count(), MAX_TOOL_RESULT_CHARS);
        assert!(out.ends_with("... (tool result truncated)"));
        assert_eq!(cap_tool_result("short result".to_string()), "short result");
    }

    #[test]
    fn parses_searxng_response_json() {
        let json = r#"{"results":[
            {"title":"A","url":"https://a","content":"d1"},
            {"title":"B","url":"https://b","content":"d2"}
        ]}"#;
        let resp: SearxngResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].title, "A");
    }

    #[test]
    fn missing_results_field_yields_no_hits() {
        let resp: SearxngResponse = serde_json::from_str(r#"{}"#).unwrap();
        assert!(resp.results.is_empty());
    }

    #[test]
    fn parses_langsearch_response_json() {
        let json = r#"{"code":200,"data":{"webPages":{"value":[
            {"name":"A","url":"https://a","snippet":"d1"},
            {"name":"B","url":"https://b","snippet":"d2"}
        ]}}}"#;
        let resp: LangsearchResponse = serde_json::from_str(json).unwrap();
        let hits = resp.data.unwrap().web_pages.unwrap().value;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "A");
    }

    #[test]
    fn missing_langsearch_data_yields_no_hits() {
        let resp: LangsearchResponse = serde_json::from_str(r#"{"code":200}"#).unwrap();
        assert!(resp.data.is_none());
    }

    #[tokio::test]
    async fn explicit_choice_errors_clearly_when_unconfigured_instead_of_swapping() {
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "langsearch".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let err = tb.search("test", None, &[], &[]).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("LangSearch selected but no API key")
        );

        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "searxng".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let err = tb.search("test", None, &[], &[]).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("SearXNG selected but no instance URL")
        );
    }

    #[tokio::test]
    async fn auto_reaches_searxng_when_configured_instead_of_bailing() {
        // "auto" with only a SearXNG URL set must attempt it (proven by a
        // connection-level error, not the langsearch-key or no-backend message).
        let tb = ToolBox::new(
            PathBuf::new(),
            Some("http://127.0.0.1:1".to_string()),
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        // SearXNG is attempted first; if it is unavailable, auto mode may
        // still succeed through the zero-setup DuckDuckGo fallback.
        if let Err(err) = tb.search("test", None, &[], &[]).await {
            let msg = err.to_string();
            assert!(!msg.contains("no search backend configured"));
            assert!(!msg.contains("API key"));
            assert!(msg.contains("SearXNG"), "{msg}");
        }
    }

    #[test]
    fn resolves_uddg_redirect_href() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        assert_eq!(
            resolve_ddg_href(href).as_deref(),
            Some("https://example.com/page")
        );
    }

    #[test]
    fn resolves_protocol_relative_href_without_uddg() {
        assert_eq!(
            resolve_ddg_href("//example.com/x").as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn resolve_ddg_href_decodes_plus_as_space() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa+b";
        assert_eq!(
            resolve_ddg_href(href).as_deref(),
            Some("https://example.com/a b")
        );
    }

    #[test]
    fn strip_tags_drops_markup_and_unescapes_entities() {
        assert_eq!(strip_tags("<b>Rust</b> &amp; friends"), "Rust & friends");
    }

    #[test]
    fn drop_tag_blocks_removes_script_and_style_content() {
        let html =
            "<p>keep</p><script>var x = 1;</script><style>.a{color:red}</style><p>also keep</p>";
        let no_script = drop_tag_blocks(html, "script");
        assert!(!no_script.contains("var x"));
        assert!(no_script.contains("also keep"));
        let no_style = drop_tag_blocks(&no_script, "style");
        assert!(!no_style.contains("color:red"));
        assert!(no_style.contains("keep"));
    }

    #[test]
    fn drop_tag_blocks_handles_unterminated_tag_by_dropping_the_remainder() {
        // A truncated fetch (or malformed page) shouldn't panic or infinite-loop.
        let html = "<p>keep</p><script>var x = 1;";
        let out = drop_tag_blocks(html, "script");
        assert_eq!(out, "<p>keep</p>");
    }

    #[test]
    fn strip_html_to_text_drops_tags_scripts_styles_and_blank_lines() {
        let html = "<html><head><style>body{}</style><script>track();</script></head>\
                     <body>\n\n<h1>Title</h1>\n<p>Some   text</p>\n\n\n<p>More</p></body></html>";
        let text = strip_html_to_text(html);
        assert!(!text.contains("track()"));
        assert!(!text.contains("body{}"));
        assert!(text.contains("Title"));
        assert!(text.contains("More"));
        // No blank lines left over from stripped block-level tags.
        assert!(!text.contains("\n\n"));
    }

    #[test]
    fn strip_html_to_text_renders_table_as_markdown_pipe_table() {
        let html = "<body><p>Intro</p><table>\
            <tr><th>Model</th><th>Score</th></tr>\
            <tr><td>A</td><td>91</td></tr>\
            <tr><td>B</td><td>88</td></tr>\
            </table><p>Outro</p></body>";
        let text = strip_html_to_text(html);
        assert!(text.contains("| Model | Score |"), "{text:?}");
        assert!(text.contains("| --- | --- |"), "{text:?}");
        assert!(text.contains("| A | 91 |"), "{text:?}");
        assert!(text.contains("| B | 88 |"), "{text:?}");
        assert!(text.contains("Intro"));
        assert!(text.contains("Outro"));
    }

    #[test]
    fn strip_html_to_text_flattens_nested_tables_without_recursing() {
        let html = "<table><tr><td>outer<table><tr><td>inner</td></tr></table></td></tr></table>";
        // Must not panic or infinite-loop; nested content just degrades to flattened text.
        let text = strip_html_to_text(html);
        assert!(text.contains("outer"));
        assert!(text.contains("inner"));
    }

    #[test]
    fn is_youtube_url_matches_watch_and_short_links() {
        assert!(is_youtube_url("https://www.youtube.com/watch?v=abc123"));
        assert!(is_youtube_url("https://youtu.be/abc123"));
        assert!(!is_youtube_url("https://example.com/watch?v=abc123"));
        assert!(!is_youtube_url("https://notyoutube.com/watch?v=abc123"));
        assert!(!is_youtube_url("https://evilyoutube.com/watch?v=abc123"));
        assert!(is_youtube_url("https://m.youtube.com/watch?v=abc123"));
    }

    #[test]
    fn parse_caption_track_url_finds_the_first_baseurl_in_captiontracks() {
        let page = r#"var ytInitialData = {"captions":{"playerCaptionsTracklistRenderer":
            {"captionTracks":[{"baseUrl":"https:\/\/www.youtube.com\/api\/timedtext?v=abc&lang=en","name":{}}]}}};"#;
        let url = parse_caption_track_url(page).expect("should find a track");
        assert_eq!(url, "https://www.youtube.com/api/timedtext?v=abc&lang=en");
    }

    #[test]
    fn parse_caption_track_url_none_when_no_captions_present() {
        assert!(parse_caption_track_url("var ytInitialData = {};").is_none());
    }

    #[test]
    fn strip_timedtext_xml_joins_cue_text_with_spaces() {
        let xml = r#"<transcript><text start="0" dur="2">Hello there</text><text start="2" dur="3">world &amp; friends</text></transcript>"#;
        assert_eq!(strip_timedtext_xml(xml), "Hello there world & friends");
    }

    #[test]
    fn fetch_url_text_extracts_pdf_when_content_type_is_pdf() {
        let bytes = crate::extract::pdf_with_pages(&["HELLO FROM PDF"]);
        let text = extract_pdf_or_html(&bytes, "application/pdf");
        assert!(text.contains("HELLO FROM PDF"), "{text:?}");
    }

    #[test]
    fn extract_pdf_or_html_falls_back_to_html_for_non_pdf_content_type() {
        let html = b"<html><body><p>hi there</p></body></html>";
        let text = extract_pdf_or_html(html, "text/html; charset=utf-8");
        assert_eq!(text, "hi there");
    }

    #[test]
    fn extract_pdf_or_html_detects_pdf_by_magic_bytes_even_without_content_type() {
        let bytes = crate::extract::pdf_with_pages(&["MAGIC BYTES PDF"]);
        let text = extract_pdf_or_html(&bytes, "");
        assert!(text.contains("MAGIC BYTES PDF"), "{text:?}");
    }

    #[test]
    fn parses_ddg_html_result_block() {
        let html = r#"
            <div class="result">
              <h2 class="result__title">
                <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Frust&rut=x">Rust <b>1.90</b> released</a>
              </h2>
              <a class="result__snippet" href="...">The <b>Rust</b> team announces version 1.90.</a>
            </div>
            <div class="result">
              <h2 class="result__title">
                <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fblog&rut=y">Rust blog</a>
              </h2>
              <a class="result__snippet" href="...">Announcement post.</a>
            </div>
        "#;
        let hits = parse_ddg_html(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust 1.90 released");
        assert_eq!(hits[0].url, "https://example.com/rust");
        assert_eq!(hits[0].snippet, "The Rust team announces version 1.90.");
        assert_eq!(hits[1].url, "https://example.com/blog");
    }

    #[test]
    fn parses_brave_html_result_card() {
        let html = r#"
            <div class="snippet" data-type="web">
              <a href="https://example.com/project" class="svelte l1">
                <div class="title search-snippet-title line-clamp-1">Example <strong>Project</strong></div>
                <div class="generic-snippet"><div class="content desktop-default-regular">A useful <b>project</b>.</div></div>
              </a>
            </div>
        "#;
        let hits = parse_brave_html(html);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Example Project");
        assert_eq!(hits[0].url, "https://example.com/project");
        assert_eq!(hits[0].snippet, "A useful project.");
    }

    fn files_toolbox() -> (ToolBox, crate::db::Db, String) {
        // A real temp-file db (the toolbox opens its own connection by path).
        // Own directory: the attached sibling cache.db must not collide with
        // other tests' dbs, all of which live flat in the temp dir.
        let dir = std::env::temp_dir().join(format!("nexus-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nexus.db");
        let db = crate::db::Db::open(&path).unwrap();
        let space = db.default_space_id().unwrap();
        let id = db.upsert_file(&space, "report.md", "h", 1, "ok").unwrap();
        let text: String = (1..=250)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        db.set_file_chunks(&id, &crate::extract::chunk_lines(&text))
            .unwrap();
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            Some(FilesCtx {
                db_path: path,
                space_id: space.clone(),
                embedder: None,
            }),
            None,
        );
        (tb, db, space)
    }

    fn skills_toolbox() -> (ToolBox, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nexus-skills-tb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("t")).unwrap();
        std::fs::write(
            dir.join("t/SKILL.md"),
            "---\nname: t\ndescription: d\n---\nx",
        )
        .unwrap();
        let tb = ToolBox::new(
            dir.clone(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        (tb, dir)
    }

    #[tokio::test]
    async fn public_script_files_actions_are_confined_and_hash_based() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("space-scripts");
        tb.space_scripts_dir = scripts.clone();
        let (result, _) = tb
            .run(
                "script_files",
                r#"{"action":"write","path":"nested/tool.sh","content":"echo hi"}"#,
            )
            .await;
        assert!(result.contains("wrote nested/tool.sh"), "{result}");
        let (result, _) = tb
            .run(
                "script_files",
                r#"{"action":"read","path":"nested/tool.sh"}"#,
            )
            .await;
        assert!(result.contains("echo hi"), "{result}");
        let hash = line_hash(1, "echo hi");
        let (result, _) = tb
            .run(
                "script_files",
                &format!(r#"{{"action":"edit","path":"nested/tool.sh","edits":[{{"hash":"{hash}","new":"echo bye"}}]}}"#),
            )
            .await;
        assert!(result.contains("edited nested/tool.sh"), "{result}");
        let (result, _) = tb
            .run("script_files", r#"{"action":"read","path":"../escape.sh"}"#)
            .await;
        assert!(result.contains("invalid path"), "{result}");
    }

    #[tokio::test]
    async fn run_script_runs_sh_with_args_and_reports_exit_code() {
        let (tb, dir) = skills_toolbox();
        std::fs::write(dir.join("t/go.sh"), "echo \"hi $1\"\nexit 3\n").unwrap();
        let (result, status) = tb
            .run(
                "run_script",
                r#"{"skill":"t","path":"go.sh","args":["there"]}"#,
            )
            .await;
        assert!(status.contains("Running t/go.sh"));
        assert!(result.contains("hi there"), "{result}");
        assert!(result.contains("exit code: 3"), "{result}");
    }

    #[tokio::test]
    async fn run_script_is_confined_and_names_missing_scripts() {
        let (tb, _) = skills_toolbox();
        let (result, _) = tb
            .run("run_script", r#"{"skill":"t","path":"../evil.sh"}"#)
            .await;
        assert!(result.contains("invalid"), "{result}");
        let (result, _) = tb
            .run("run_script", r#"{"skill":"t","path":"nope.sh"}"#)
            .await;
        assert!(result.contains("no such script"), "{result}");
    }

    #[tokio::test]
    async fn install_packages_validates_names_and_target() {
        let (tb, _) = skills_toolbox();
        let (result, _) = tb.run("install_packages", r#"{"packages":[]}"#).await;
        assert!(result.contains("no packages"), "{result}");
        let (result, _) = tb
            .run(
                "install_packages",
                r#"{"packages":["--upgrade"],"skill":"t"}"#,
            )
            .await;
        assert!(result.contains("invalid package name"), "{result}");
        let (result, _) = tb
            .run("install_packages", r#"{"packages":["--upgrade"]}"#)
            .await;
        assert!(result.contains("invalid package name"), "{result}");
        let (result, _) = tb
            .run(
                "install_packages",
                r#"{"packages":["x"],"skill":"a","app":"b"}"#,
            )
            .await;
        assert!(result.contains("not both"), "{result}");
        let (result, _) = tb
            .run("install_packages", r#"{"packages":["x"],"skill":"ghost"}"#)
            .await;
        assert!(result.contains("unknown skill"), "{result}");
    }

    #[tokio::test]
    async fn run_python_persists_and_runs() {
        let dir = std::env::temp_dir().join(format!("nexus-py-{}", uuid::Uuid::new_v4()));
        let scripts_dir = dir.join("scripts");
        let mut tb = ToolBox::new(
            dir.join("skills"),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        tb.space_scripts_dir = scripts_dir.clone();
        let (result, status) = tb
            .run("run_python", r#"{"code":"print(2**32)","name":"test.py"}"#)
            .await;
        assert!(status.contains("Running script"), "status was {status:?}");
        assert!(result.contains("4294967296"), "{result}");
        assert!(scripts_dir.join("test.py").exists());
        assert!(scripts_dir.join(".venv/bin/python").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_app_finds_lines_and_skips_node_modules() {
        let (tb, dir) = apps_toolbox();
        let _ = tb.run("write_file", r#"{"app":"deck","path":"index.html","content":"<h1>slide one</h1>\n<p>quiet</p>\n<p>slide two</p>"}"#).await;
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"js/a.js","content":"// slide logic"}"#,
            )
            .await;
        std::fs::create_dir_all(dir.join("deck/node_modules/x")).unwrap();
        std::fs::write(dir.join("deck/node_modules/x/i.js"), "slide").unwrap();
        let (result, _) = tb
            .run("grep_app", r#"{"app":"deck","pattern":"SLIDE"}"#)
            .await;
        assert!(result.contains("index.html:1,3"), "{result}");
        assert!(result.contains("js/a.js:1"), "{result}");
        assert!(!result.contains("<h1>slide one</h1>"), "{result}");
        assert!(!result.contains("// slide logic"), "{result}");
        assert!(!result.contains("node_modules"), "{result}");
        let (result, _) = tb
            .run("grep_app", r#"{"app":"deck","pattern":"zzz"}"#)
            .await;
        assert!(result.contains("no matches"), "{result}");
    }

    #[tokio::test]
    async fn app_file_tools_respect_gitignore() {
        let (tb, _) = apps_toolbox();
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":".gitignore","content":"secret.txt\nprivate/\n"}"#,
            )
            .await;
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"public.txt","content":"visible"}"#,
            )
            .await;
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"secret.txt","content":"hidden"}"#,
            )
            .await;
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"private/data.txt","content":"hidden"}"#,
            )
            .await;

        let (result, _) = tb
            .run("grep_app", r#"{"app":"deck","pattern":"hidden"}"#)
            .await;
        assert!(result.contains("no matches"), "{result}");

        let (result, _) = tb
            .run("read_app_file", r#"{"app":"deck","path":"secret.txt"}"#)
            .await;
        assert!(result.contains("ignored by .gitignore"), "{result}");

        let (result, _) = tb
            .run("read_app_file", r#"{"app":"deck","path":".gitignore"}"#)
            .await;
        assert!(result.contains("ignored by .gitignore"), "{result}");
    }

    #[tokio::test]
    async fn reads_are_line_hashed_and_edits_show_a_diff() {
        let (tb, _) = apps_toolbox();
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"d","path":"i.html","content":"alpha\nbeta"}"#,
            )
            .await;
        let (result, _) = tb
            .run("read_app_file", r#"{"app":"d","path":"i.html"}"#)
            .await;
        let h1 = line_hash(1, "alpha");
        let h2 = line_hash(2, "beta");
        assert!(result.contains(&format!("    1:{h1}\talpha")), "{result}");
        assert!(result.contains(&format!("    2:{h2}\tbeta")), "{result}");
        let (result, _) = tb
            .run(
                "edit_file",
                &format!(
                    r#"{{"app":"d","path":"i.html","edits":[{{"hash":"{h2}","new":"gamma"}}]}}"#
                ),
            )
            .await;
        assert!(result.contains("- beta"), "{result}");
        assert!(result.contains("+ gamma"), "{result}");
    }

    #[tokio::test]
    async fn install_skill_rejects_bad_shorthand_without_network() {
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let (result, status) = tb.run("install_skill", r#"{"source":"nope"}"#).await;
        assert!(status.contains("Installing skill"));
        assert!(result.contains("invalid source"), "{result}");
    }

    #[test]
    fn defs_include_file_tools_only_when_files_exist() {
        let (tb, ..) = files_toolbox();
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        assert!(names.contains(&"files".to_string()));

        let empty = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let names: Vec<String> = empty.defs().iter().map(|d| d.name.clone()).collect();
        assert!(!names.contains(&"files".to_string()));
    }

    #[test]
    fn fetch_url_is_always_available() {
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        assert!(names.contains(&"fetch_url".to_string()));
        assert!(names.contains(&"search".to_string()));
    }

    #[test]
    fn public_definitions_have_only_consolidated_names() {
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let names: Vec<_> = tb.defs().into_iter().map(|def| def.name).collect();
        assert!(names.len() <= 9, "too many public tools: {names:?}");
        for old in [
            "skill",
            "skill_admin",
            "run_python",
            "run_script",
            "install_packages",
            "create_skill",
            "install_skill",
            "web_search",
            "academic_search",
            "discussion_search",
            "search_sources",
            "list_citations",
            "search_files",
            "read_file",
            "read_pdf_page",
            "app_inspect",
            "app_modify",
            "app_assets",
            "read_app_file",
            "grep_app",
            "write_file",
            "edit_file",
            "diff_app",
            "list_images",
            "copy_file_to_app",
            "copy_images_to_app",
            "script_files",
            "list_scripts",
            "write_script",
            "read_script",
            "edit_script",
            "generate_image",
            "generate_video",
            "video_transform",
            "video_references",
            "edit_video",
            "extract_frame",
            "stitch_videos",
            "save_reference",
            "list_references",
            "delete_reference",
        ] {
            assert!(
                !names.iter().any(|name| name == old),
                "deprecated tool advertised: {old}"
            );
        }
        for required in ["batch", "skills", "scripts", "search", "fetch_url"] {
            assert!(
                names.iter().any(|name| name == required),
                "missing {required}"
            );
        }
    }

    #[test]
    fn unchanged_result_note_labels_the_call_and_marks_it() {
        let note = super::tool_result_unchanged_note("read_file", r#"{"name":"report.pdf"}"#);
        assert!(
            note.starts_with(super::TOOL_RESULT_OMITTED_PREFIX),
            "{note}"
        );
        assert!(note.contains("read_file report.pdf"), "{note}");
        assert!(note.contains("unchanged"), "{note}");
        // Unknown tools fall back to the bare name.
        let note = super::tool_result_unchanged_note("mystery_tool", "{}");
        assert!(note.contains("mystery_tool"), "{note}");
    }

    #[test]
    fn read_only_tool_classification() {
        for (tool, args) in [
            ("search", r#"{"mode":"web","query":"x"}"#),
            ("web_search", r#"{"query":"x"}"#),
            ("academic_search", r#"{"query":"x"}"#),
            ("discussion_search", r#"{"query":"x"}"#),
            ("fetch_url", r#"{"url":"https://x"}"#),
            ("research_lookup", r#"{"scope":"citations"}"#),
            ("search_sources", r#"{"query":"x"}"#),
            ("list_citations", "{}"),
            ("files", r#"{"action":"read","name":"x"}"#),
            ("search_files", r#"{"query":"x"}"#),
            ("read_file", r#"{"name":"x"}"#),
            ("read_pdf_page", r#"{"name":"x","page":1}"#),
            ("app_inspect", r#"{"action":"read","app":"a","path":"x"}"#),
            ("read_app_file", r#"{"app":"a","path":"x"}"#),
            ("grep_app", r#"{"app":"a","pattern":"x"}"#),
            ("diff_app", r#"{"app":"a","path":"x","content":"y"}"#),
            ("list_images", "{}"),
            ("read_script", r#"{"path":"x"}"#),
            ("list_scripts", "{}"),
            ("list_references", "{}"),
            ("skill", r#"{"name":"t"}"#),
            ("skills", r#"{"action":"load","name":"t"}"#),
            ("scripts", r#"{"action":"list"}"#),
            ("scripts", r#"{"action":"read","path":"a.sh"}"#),
            ("app", r#"{"action":"read","app":"a","path":"x"}"#),
            ("app", r#"{"action":"search","app":"a","pattern":"x"}"#),
            ("app", r#"{"action":"list"}"#),
            ("media", r#"{"action":"list_references"}"#),
        ] {
            assert!(
                super::is_read_only_tool(tool, args),
                "{tool} {args} should be read-only"
            );
        }
        for (tool, args) in [
            ("batch", "{}"),
            (
                "skills",
                r#"{"action":"create","name":"x","description":"y"}"#,
            ),
            ("skills", r#"{"action":"install","source":"a/b"}"#),
            (
                "scripts",
                r#"{"action":"write","path":"a.sh","content":"x"}"#,
            ),
            ("scripts", r#"{"action":"run","path":"a.sh"}"#),
            (
                "scripts",
                r#"{"action":"python","code":"print(1)","name":"x.py"}"#,
            ),
            ("scripts", r#"{"action":"install","packages":["x"]}"#),
            (
                "app",
                r#"{"action":"write","app":"a","path":"x","content":"y"}"#,
            ),
            (
                "app",
                r#"{"action":"patch","app":"a","path":"x","edits":[{"hash":"h"}]}"#,
            ),
            (
                "app",
                r#"{"action":"copy_images","app":"a","image_ids":["i"]}"#,
            ),
            ("media", r#"{"action":"generate_image","prompt":"x"}"#),
            ("media", r#"{"action":"edit","video_id":"v"}"#),
            (
                "media",
                r#"{"action":"save_reference","name":"n","image_id":"i","description":"d"}"#,
            ),
            (
                "skill_admin",
                r#"{"action":"create","name":"x","description":"y"}"#,
            ),
            ("create_skill", r#"{"name":"x","description":"y"}"#),
            ("install_skill", r#"{"source":"a/b"}"#),
            ("run_python", r#"{"code":"print(1)","name":"x.py"}"#),
            ("run_script", r#"{"path":"x"}"#),
            ("install_packages", r#"{"packages":["x"]}"#),
            (
                "app_modify",
                r#"{"action":"write","app":"a","path":"x","content":"y"}"#,
            ),
            ("write_file", r#"{"app":"a","path":"x","content":"y"}"#),
            (
                "edit_file",
                r#"{"app":"a","path":"x","edits":[{"hash":"h"}]}"#,
            ),
            (
                "app_assets",
                r#"{"action":"copy_file","app":"a","file_name":"f"}"#,
            ),
            ("copy_file_to_app", r#"{"app":"a","file_name":"f"}"#),
            ("copy_images_to_app", r#"{"app":"a","image_ids":["i"]}"#),
            (
                "script_files",
                r#"{"action":"write","path":"a.sh","content":"x"}"#,
            ),
            ("write_script", r#"{"path":"a.sh","content":"x"}"#),
            ("edit_script", r#"{"path":"a.sh","edits":[{"hash":"h"}]}"#),
            ("generate_image", r#"{"prompt":"x"}"#),
            ("generate_video", r#"{"prompt":"x"}"#),
            ("video_transform", r#"{"action":"edit","video_id":"v"}"#),
            (
                "save_reference",
                r#"{"name":"n","image_id":"i","description":"d"}"#,
            ),
            ("delete_reference", r#"{"name":"n"}"#),
        ] {
            assert!(
                !super::is_read_only_tool(tool, args),
                "{tool} {args} should be mutating"
            );
        }
    }

    #[tokio::test]
    async fn batch_runs_mixed_operations_in_order() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("space-scripts");
        tb.space_scripts_dir = scripts.clone();
        let (result, status) = tb
            .run(
                "batch",
                r#"{"calls":[
                    {"tool":"scripts","arguments":{"action":"write","path":"a.sh","content":"echo a"}},
                    {"tool":"scripts","arguments":{"action":"read","path":"a.sh"}},
                    {"tool":"skills","arguments":{"action":"load","name":"t"}}
                ]}"#,
            )
            .await;
        assert!(status.contains("3"), "status was {status:?}");
        assert!(result.contains("[1/3] scripts/write a.sh"), "{result}");
        assert!(result.contains("[2/3] scripts/read a.sh"), "{result}");
        assert!(result.contains("[3/3] skills/load t"), "{result}");
        assert!(result.contains("wrote a.sh"), "{result}");
        assert!(result.contains("echo a"), "{result}");
    }

    #[tokio::test]
    async fn batch_of_reads_returns_every_result() {
        let (tb, _dir) = apps_toolbox();
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"index.html","content":"<h1>hi</h1>"}"#,
            )
            .await;
        let (result, status) = tb
            .run(
                "batch",
                r#"{"calls":[
                    {"tool":"app","arguments":{"action":"read","app":"deck","path":"index.html"}},
                    {"tool":"app","arguments":{"action":"read","app":"deck","path":"index.html"}}
                ]}"#,
            )
            .await;
        assert!(status.contains("2"), "status was {status:?}");
        assert!(
            result.contains("[1/2] app/read deck/index.html"),
            "{result}"
        );
        assert!(
            result.contains("[2/2] app/read deck/index.html"),
            "{result}"
        );
        assert!(result.contains("<h1>hi</h1>"), "{result}");
    }

    #[tokio::test]
    async fn batch_rejects_nested_oversized_and_empty() {
        let (tb, _) = skills_toolbox();
        let (result, _) = tb.run("batch", r#"{"calls":[]}"#).await;
        assert!(result.contains("non-empty"), "{result}");
        let (result, _) = tb
            .run("batch", r#"{"calls":[{"tool":"batch","arguments":{}}]}"#)
            .await;
        assert!(result.contains("nested"), "{result}");
        let calls: Vec<serde_json::Value> = (0..9)
            .map(|_| serde_json::json!({ "tool": "skill", "arguments": { "name": "t" } }))
            .collect();
        let args = serde_json::json!({ "calls": calls }).to_string();
        let (result, _) = tb.run("batch", &args).await;
        assert!(result.contains("at most 8"), "{result}");
    }

    #[tokio::test]
    async fn batch_isolates_unknown_subcalls() {
        let (tb, _) = skills_toolbox();
        let (result, _) = tb
            .run(
                "batch",
                r#"{"calls":[
                    {"tool":"nonexistent","arguments":{}},
                    {"tool":"skills","arguments":{"action":"load","name":"t"}}
                ]}"#,
            )
            .await;
        assert!(result.contains("unknown tool: nonexistent"), "{result}");
        assert!(result.contains("[2/2] skills/load t"), "{result}");
    }

    #[tokio::test]
    async fn consolidated_tools_reject_invalid_actions_and_missing_fields() {
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        for (name, args, expected) in [
            ("skills", r#"{"action":"nope"}"#, "invalid action"),
            (
                "skills",
                r#"{"action":"create","name":"x"}"#,
                "missing required field: description",
            ),
            ("scripts", r#"{"action":"nope"}"#, "invalid action"),
            (
                "scripts",
                r#"{"action":"python","code":"print(1)"}"#,
                "missing required field: name",
            ),
            ("skill_admin", r#"{"action":"nope"}"#, "invalid action"),
            (
                "search",
                r#"{"mode":"web"}"#,
                "missing required field: query",
            ),
            (
                "research_lookup",
                r#"{"scope":"session_sources"}"#,
                "missing required field: query",
            ),
            ("files", r#"{"action":"nope"}"#, "invalid action"),
            ("app", r#"{"action":"nope"}"#, "invalid action"),
            (
                "app",
                r#"{"action":"read","app":"a"}"#,
                "missing required field: path",
            ),
            (
                "app_inspect",
                r#"{"action":"nope","app":"a"}"#,
                "invalid action",
            ),
            (
                "app_modify",
                r#"{"action":"nope","app":"a","path":"x"}"#,
                "invalid action",
            ),
            ("app_assets", r#"{"action":"nope"}"#, "invalid action"),
            ("script_files", r#"{"action":"nope"}"#, "invalid action"),
            ("media", r#"{"action":"nope"}"#, "invalid action"),
            (
                "media",
                r#"{"action":"save_reference","name":"x","image_id":"i"}"#,
                "missing required field: description",
            ),
            ("video_transform", r#"{"action":"nope"}"#, "invalid action"),
            (
                "video_references",
                r#"{"action":"save","name":"x"}"#,
                "missing required field: image_id",
            ),
        ] {
            let (result, status) = tb.run(name, args).await;
            assert_eq!(status, "invalid arguments", "{name}: {result}");
            assert!(result.contains(expected), "{name}: {result}");
        }
    }

    #[tokio::test]
    async fn search_files_returns_ranked_snippets() {
        let (tb, ..) = files_toolbox();
        let (result, status) = tb.run("search_files", r#"{"query":"line 42"}"#).await;
        assert!(status.contains("Searching files"));
        assert!(result.contains("report.md"));
        assert!(result.contains("lines 41-80"));
        // No embedder configured → keyword search IS the primary, no fallback tag.
        assert!(!result.contains("keyword fallback"), "{result}");
    }

    #[tokio::test]
    async fn public_files_actions_preserve_search_and_paging() {
        let (tb, ..) = files_toolbox();
        let (result, _) = tb
            .run("files", r#"{"action":"search","query":"line 42"}"#)
            .await;
        assert!(result.contains("report.md"), "{result}");
        let (result, _) = tb
            .run(
                "files",
                r#"{"action":"read","name":"report.md","offset":201}"#,
            )
            .await;
        assert!(result.contains("line 201"), "{result}");
        assert!(!result.contains("line 1"), "{result}");
    }

    #[test]
    fn semantic_snippets_rank_truncate_and_report_none_without_vectors() {
        let (_, db, space) = files_toolbox();
        let conn = db.raw();
        // No vectors stored yet.
        assert!(semantic_snippets(conn, &space, &[1.0, 0.0]).is_none());

        let id = db.upsert_file(&space, "notes.md", "h2", 1, "ok").unwrap();
        let long = "long ".repeat(200);
        db.set_file_chunks(
            &id,
            &[
                ("p1".into(), long.clone()),
                ("p2".into(), "short target".into()),
            ],
        )
        .unwrap();
        db.set_chunk_embeddings(&id, &[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])])
            .unwrap();

        let out = semantic_snippets(conn, &space, &[0.0, 1.0]).unwrap();
        let first = out.lines().next().unwrap();
        assert!(first.contains("notes.md (p2)"), "{first}");
        assert!(first.contains("short target"), "{first}");
        // The long chunk is truncated, not dumped whole.
        assert!(out.lines().nth(1).unwrap().len() < long.len(), "{out}");
    }

    #[tokio::test]
    async fn read_file_is_ranged_and_capped() {
        let (tb, ..) = files_toolbox();
        let (result, _) = tb.run("read_file", r#"{"name":"report.md"}"#).await;
        assert!(result.contains("line 1"));
        assert!(result.contains("line 200"));
        assert!(!result.contains("line 201")); // 200-line cap

        let (result, _) = tb
            .run("read_file", r#"{"name":"report.md","offset":201}"#)
            .await;
        assert!(result.contains("line 201"));
        assert!(result.contains("line 250"));

        let (result, _) = tb.run("read_file", r#"{"name":"nope.md"}"#).await;
        assert!(result.contains("unknown file"));
    }

    fn apps_toolbox() -> (ToolBox, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nexus-apps-{}", uuid::Uuid::new_v4()));
        let registry = crate::appserver::AppRegistry::load(&PathBuf::from("/tmp"));
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            Some(AppsCtx {
                dir: dir.clone(),
                server_port: 9999,
                registry,
                space_name: "default".to_string(),
                space_id: "default".to_string(),
                space_db_path: PathBuf::from("/tmp/test.db"),
                files_dir: dir.clone(),
                session_id: String::new(),
            }),
        );
        (tb, dir)
    }

    #[test]
    fn defs_include_app_tools_only_with_apps_ctx() {
        let (tb, _) = apps_toolbox();
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        for t in ["app"] {
            assert!(names.contains(&t.to_string()), "missing {t}");
        }
        let empty = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let names: Vec<String> = empty.defs().iter().map(|d| d.name.clone()).collect();
        assert!(!names.contains(&"app".to_string()));
    }

    #[tokio::test]
    async fn write_edit_read_round_trip_with_live_url() {
        let (tb, dir) = apps_toolbox();
        let (result, _) = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"index.html","content":"<h1>Hello</h1>"}"#,
            )
            .await;
        assert!(result.contains("wrote deck/index.html"), "{result}");
        assert!(
            result.contains("live at http://127.0.0.1:9999/"),
            "{result}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("deck/index.html")).unwrap(),
            "<h1>Hello</h1>"
        );

        // nested path creates parent dirs
        let (result, _) = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"js/a.js","content":"1"}"#,
            )
            .await;
        assert!(result.contains("wrote deck/js/a.js"), "{result}");

        let h = line_hash(1, "<h1>Hello</h1>");
        let (result, _) = tb
            .run(
                "edit_file",
                &format!(r#"{{"app":"deck","path":"index.html","edits":[{{"hash":"{h}","new":"<h1>Bye</h1>"}}]}}"#),
            )
            .await;
        assert!(result.contains("edited deck/index.html"), "{result}");
        assert_eq!(
            std::fs::read_to_string(dir.join("deck/index.html")).unwrap(),
            "<h1>Bye</h1>"
        );

        let (result, _) = tb
            .run("read_app_file", r#"{"app":"deck","path":"index.html"}"#)
            .await;
        assert!(result.contains("<h1>Bye</h1>"), "{result}");
        assert!(result.contains("lines 1-1 of 1"), "{result}");
    }

    #[tokio::test]
    async fn public_app_delegates_to_hashline_backend() {
        let (tb, dir) = apps_toolbox();
        let (result, _) = tb
            .run(
                "app",
                r#"{"action":"write","app":"public-deck","path":"index.html","content":"<h1>Hello</h1>"}"#,
            )
            .await;
        assert!(
            result.contains("live at http://127.0.0.1:9999/"),
            "{result}"
        );
        let (result, _) = tb
            .run(
                "app",
                r#"{"action":"read","app":"public-deck","path":"index.html"}"#,
            )
            .await;
        assert!(result.contains("<h1>Hello</h1>"), "{result}");
        let hash = line_hash(1, "<h1>Hello</h1>");
        let (result, _) = tb
            .run(
                "app",
                &format!(r#"{{"action":"patch","app":"public-deck","path":"index.html","edits":[{{"hash":"{hash}","new":"<h1>Bye</h1>"}}]}}"#),
            )
            .await;
        assert!(result.contains("edited public-deck/index.html"), "{result}");
        let (result, _) = tb
            .run(
                "app",
                r#"{"action":"search","app":"public-deck","pattern":"bye","compact":false}"#,
            )
            .await;
        assert!(result.contains("index.html:1: <h1>Bye</h1>"), "{result}");
        assert_eq!(
            std::fs::read_to_string(dir.join("public-deck/index.html")).unwrap(),
            "<h1>Bye</h1>"
        );
    }

    #[tokio::test]
    async fn consolidated_skills_scripts_and_media_delegate() {
        let (mut tb, dir) = skills_toolbox();
        let scripts_dir = dir.join("space-scripts");
        tb.space_scripts_dir = scripts_dir.clone();
        let (result, status) = tb.run("skills", r#"{"action":"load","name":"t"}"#).await;
        assert!(status.contains("Reading"), "{status}");
        assert_eq!(result, "x"); // skill body, frontmatter stripped

        let (result, _) = tb
            .run(
                "scripts",
                r#"{"action":"python","code":"print(2**32)","name":"t.py"}"#,
            )
            .await;
        assert!(result.contains("4294967296"), "{result}");
        assert!(scripts_dir.join("t.py").exists());

        let (result, status) = tb.run("media", r#"{"action":"list_references"}"#).await;
        assert!(status.contains("Listing"), "{status}");
        assert!(result.contains("no references"), "{result}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn diff_app_previews_candidate_changes_without_writing() {
        let (tb, dir) = apps_toolbox();
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"deck","path":"index.html","content":"<h1>Hello</h1>"}"#,
            )
            .await;

        let (result, _) = tb
            .run(
                "diff_app",
                r#"{"app":"deck","path":"index.html","content":"<h1>Bye</h1>\n<p>New</p>"}"#,
            )
            .await;

        assert!(result.contains("-<h1>Hello</h1>"), "{result}");
        assert!(result.contains("+<h1>Bye</h1>"), "{result}");
        assert!(result.contains("+<p>New</p>"), "{result}");
        assert_eq!(
            std::fs::read_to_string(dir.join("deck/index.html")).unwrap(),
            "<h1>Hello</h1>"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_stale_and_duplicate_hashes() {
        let (tb, _) = apps_toolbox();
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"a","path":"f.txt","content":"x y x"}"#,
            )
            .await;

        // A hash for content that isn't in the file at all.
        let (result, _) = tb
            .run(
                "edit_file",
                r#"{"app":"a","path":"f.txt","edits":[{"hash":"deadbeef","new":"w"}]}"#,
            )
            .await;
        assert!(result.contains("not found"), "{result}");

        // Two edits resolving to the same line in one call.
        let h = line_hash(1, "x y x");
        let args = format!(
            r#"{{"app":"a","path":"f.txt","edits":[{{"hash":"{h}","new":"a"}},{{"hash":"{h}","new":"b"}}]}}"#
        );
        let (result, _) = tb.run("edit_file", &args).await;
        assert!(result.contains("already edited"), "{result}");
    }

    #[tokio::test]
    async fn edit_file_can_delete_and_insert_via_multiline_replacement() {
        let (tb, dir) = apps_toolbox();
        let _ = tb
            .run(
                "write_file",
                r#"{"app":"a","path":"f.txt","content":"one\ntwo\nthree"}"#,
            )
            .await;
        let h = line_hash(2, "two");
        // Delete "two" and insert an extra line after "one" in the same call.
        let args = format!(
            r#"{{"app":"a","path":"f.txt","edits":[{{"hash":"{}","new":"one\ninserted"}},{{"hash":"{h}"}}]}}"#,
            line_hash(1, "one"),
        );
        let (result, _) = tb.run("edit_file", &args).await;
        assert!(result.contains("edited a/f.txt"), "{result}");
        assert_eq!(
            std::fs::read_to_string(dir.join("a/f.txt")).unwrap(),
            "one\ninserted\nthree"
        );
    }

    #[tokio::test]
    async fn app_paths_are_confined() {
        let (tb, dir) = apps_toolbox();
        for args in [
            r#"{"app":"..","path":"f.txt","content":"x"}"#,
            r#"{"app":"a/b","path":"f.txt","content":"x"}"#,
            r#"{"app":"a","path":"../f.txt","content":"x"}"#,
            r#"{"app":"a","path":"/etc/f.txt","content":"x"}"#,
            r#"{"app":"a","path":"b/../../f.txt","content":"x"}"#,
            r#"{"app":"a","path":"","content":"x"}"#,
        ] {
            let (result, _) = tb.run("write_file", args).await;
            assert!(
                result.contains("invalid") || result.contains("must be relative"),
                "{args} -> {result}"
            );
        }
        assert!(!dir.join("../f.txt").exists());
    }

    #[test]
    fn research_toolbox_offers_search_modes_and_fetch_url() {
        let tb = ToolBox::research(None, None, "auto".to_string(), Vec::new(), None);
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"search".to_string()));
        assert!(names.contains(&"fetch_url".to_string()));
    }

    #[tokio::test]
    async fn research_toolbox_refuses_to_run_other_tools() {
        let tb = ToolBox::research(None, None, "auto".to_string(), Vec::new(), None);
        let (result, _) = tb.run("run_python", r#"{"code":"print(1)"}"#).await;
        assert!(
            result.contains("not available in research mode"),
            "{result}"
        );
    }

    #[tokio::test]
    async fn cache_only_toolbox_returns_not_cached_marker_on_miss_without_network() {
        let dir = std::env::temp_dir().join(format!("nexus-cacheonly-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("db.sqlite3");
        // Any valid sqlite file works — fetch_cached only needs cache_get/cache_put's
        // table, migrated on open elsewhere in real use; here confirm the miss path
        // never reaches the network.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE web_cache (url_norm TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT, text TEXT NOT NULL, fetched_at TEXT NOT NULL);",
        ).unwrap();
        drop(conn);

        let tb = ToolBox::research(None, None, "auto".to_string(), Vec::new(), Some(db_path))
            .cache_only();
        let (result, _status) = tb
            .run(
                "fetch_url",
                r#"{"url":"https://never-fetched.example/page"}"#,
            )
            .await;
        assert!(result.contains("not cached"), "{result}");
    }
}
