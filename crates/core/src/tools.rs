//! Tools the model can call mid-response, advertised as nine consolidated
//! names (`batch`, `skills`, `scripts`, `search`, `fetch_url`,
//! `research_lookup`, `files`, `app`, `media`) that dispatch onto a larger
//! set of specialized implementations below, behind the `ToolExecutor` seam.

mod spec;

use std::fmt::Write as _;
use std::future::Future;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::provider::ToolDef;
use crate::provider::openrouter::{OpenRouter, normalize_video_params};
use crate::skills::{load_skills_from_dirs, resolve_skill_dir, resolve_skill_file, skill_body};
use crate::workspace::{Access, PathError, Workspace};

const MAX_TOOL_RESULT_CHARS: usize = 8_000;
/// Maximum sub-operations in one `batch` call — bounds execution time and
/// the size of the combined result.
const MAX_BATCH_CALLS: usize = 8;
/// Combined-result cap for a `batch` call. Each sub-result is already capped
/// at `MAX_TOOL_RESULT_CHARS`; this lets a few of them through together
/// while still bounding what a runaway batch pushes into the conversation.
const MAX_BATCH_RESULT_CHARS: usize = MAX_TOOL_RESULT_CHARS * 4;

const TOOL_ERROR_PREFIXES: &[(&str, &str)] = &[
    ("missing required field", "invalid_args"),
    ("invalid action for", "invalid_args"),
    ("invalid mode for", "invalid_args"),
    ("invalid scope for", "invalid_args"),
    ("invalid tool arguments", "invalid_args"),
    ("invalid path", "invalid_args"),
    ("privilege escalation is blocked", "blocked"),
    ("could not parse this command", "invalid_args"),
    ("timed out after", "upstream"),
    ("unknown tool:", "unknown_tool"),
    ("path escapes", "confinement"),
    ("unknown file:", "not_found"),
    ("unknown skill:", "not_found"),
    ("no such script:", "not_found"),
    ("unknown app", "not_found"),
    ("cannot read ", "not_found"),
    ("write failed:", "io"),
    ("db error:", "io"),
    ("store error:", "io"),
    ("fs.", "stale_edit"),
    ("changed since your last read", "stale_edit"),
];

pub(crate) struct ToolOutcome {
    ok: bool,
    error_kind: Option<&'static str>,
    latency: Duration,
    result_chars: usize,
}

pub fn classify_tool_result(result: &str, status: &str) -> Option<&'static str> {
    let line = result.lines().next().unwrap_or_default().trim();
    let prefix_kind = TOOL_ERROR_PREFIXES.iter().find_map(|(prefix, kind)| {
        let matches = match *prefix {
            "fs." => line.starts_with(prefix) && line.contains("needs a fresh read"),
            "changed since your last read" => line.contains(prefix),
            _ => line.starts_with(prefix),
        };
        matches.then_some(*kind)
    });
    if status == "invalid arguments" || prefix_kind == Some("invalid_args") {
        return Some("invalid_args");
    }
    if status == "blocked" {
        return Some("blocked");
    }
    if line.starts_with("hash ")
        && [
            "not found",
            "matches multiple",
            "targets a line already edited",
        ]
        .iter()
        .any(|suffix| line.contains(suffix))
    {
        return Some("stale_edit");
    }
    if prefix_kind.is_some() {
        return prefix_kind;
    }
    let (prefix, _) = line.split_once(" failed:")?;
    (prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && prefix.bytes().all(|b| b.is_ascii_lowercase() || b == b' '))
    .then_some("upstream")
}

/// A workspace action waiting for the user's permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub tool: String,
    pub action: String,
    pub summary: String,
}

/// Permission for one action, its kind for this session, or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Decision {
    Allow,
    AllowForSession,
    Deny,
}

/// Interactive permission provider; absent in headless toolboxes.
pub trait Approver: Send + Sync {
    fn request(&self, req: ApprovalRequest) -> Pin<Box<dyn Future<Output = Decision> + Send + '_>>;
}

// Independent capability and privacy switches, not mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub struct ToolBox {
    /// Confined agent filesystem access; absent outside agent mode.
    pub workspace: Option<Arc<Workspace>>,
    /// Require permission before mutating workspace calls.
    pub require_approval: bool,
    /// Interactive permission provider; absent on headless paths.
    pub approver: Option<Arc<dyn Approver>>,
    /// Offer `propose_command`: only a terminal UI has a composer to put it in.
    pub interactive_ui: bool,
    /// App-managed destination for newly created or downloaded skills.
    pub skills_dir: PathBuf,
    /// Skill roots in precedence order. These include the app-managed root and
    /// read-only project/user Agent Skills roots discovered at startup.
    skill_dirs: Vec<PathBuf>,
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
    /// Opt in to device-local tool outcome recording. Disabled in incognito.
    pub record_events: bool,
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
    /// Public host base, when the daemon is exposed through a tunnel.
    pub public_base: Option<String>,
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
    if let Some(tool) = spec::find(name) {
        let value: serde_json::Value =
            serde_json::from_str(args).map_err(|e| format!("invalid tool arguments: {e}"))?;
        return Ok((tool.validate(&value)?.to_string(), args.to_string()));
    }
    let public = matches!(
        name,
        "app"
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
            "init" => {
                required_arg(&value, "app")?;
                "init_app"
            }
            "build" => {
                required_arg(&value, "app")?;
                "build_app"
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

/// The tool-calling seam: everything an agent loop needs from the tool
/// layer — definitions for the request, read-only classification for
/// parallelizing independent calls, and execution. `ToolBox` is the local
/// implementation; a remote implementation (nexus host) speaks the
/// same methods over the wire, and the loops don't know the difference.
/// `supports_images`/`space_files_dir` are required because the tool loop
/// injects image references from tool results as vision content (the host
/// that runs the tools is the only one who knows the model's vision
/// support and where the space's files live).
pub trait ToolExecutor: Send + Sync + 'static {
    /// Tool definitions to attach to the request (see `ToolBox::defs`).
    fn defs(&self) -> Vec<ToolDef>;
    /// Compact tool definitions for small models; custom executors may keep full definitions.
    fn defs_compact(&self) -> Vec<ToolDef> {
        self.defs()
    }
    /// Whether a call is read-only (see `is_read_only_tool`) — parallel
    /// calls run concurrently only when every one of them is.
    fn is_read_only(&self, name: &str, args: &str) -> bool;
    /// Run one tool call, returning `(result, status-label)`. Boxed future
    /// so the trait stays dyn-compatible (the loops hold `Arc<dyn
    /// ToolExecutor>`).
    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, String)> + Send + 'a>>;
    /// Whether the current model accepts image inputs.
    fn supports_images(&self) -> bool;
    /// The active space's files dir, for resolving image references in tool
    /// results.
    fn space_files_dir(&self) -> Option<std::path::PathBuf>;
}

impl ToolExecutor for ToolBox {
    fn defs(&self) -> Vec<ToolDef> {
        ToolBox::defs(self)
    }

    fn defs_compact(&self) -> Vec<ToolDef> {
        ToolBox::defs_compact(self)
    }

    fn is_read_only(&self, name: &str, args: &str) -> bool {
        is_read_only_tool(name, args)
    }

    fn run<'a>(
        &'a self,
        name: &'a str,
        args: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = (String, String)> + Send + 'a>> {
        Box::pin(ToolBox::run(self, name, args))
    }

    fn supports_images(&self) -> bool {
        self.supports_images
    }

    fn space_files_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.space_files_dir.clone())
    }
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
            workspace: None,
            require_approval: false,
            approver: None,
            interactive_ui: false,
            skill_dirs: vec![skills_dir.clone()],
            skills_dir,
            searxng_url,
            langsearch_key,
            search_provider,
            research_only: false,
            blocked_domains,
            db_path,
            record_events: false,
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

    /// Replace the skill roots used for reads while retaining `skills_dir` as
    /// the app-managed install/create destination.
    #[must_use]
    pub fn with_skill_dirs(mut self, skill_dirs: Vec<PathBuf>) -> Self {
        self.skill_dirs = skill_dirs;
        self
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
    #[must_use]
    pub fn with_research_session(mut self, session_id: String) -> Self {
        if let Some(db_path) = &self.db_path
            && let Ok(conn) = crate::db::open_conn(db_path)
            && let Ok(hosts) = crate::db::discarded_domains(&conn, &session_id)
        {
            self.blocked_domains.extend(hosts);
        }
        self.research_session_id = Some(session_id);
        self
    }

    /// Restrict `fetch_url` to serving from `web_cache` only — a cache miss
    /// returns `[not cached]` instead of fetching. Used for the Verifier's
    /// quote-checking pass.
    #[must_use]
    pub const fn cache_only(mut self) -> Self {
        self.cache_only = true;
        self
    }

    fn files_count(&self) -> u64 {
        let Some(ctx) = &self.files else { return 0 };
        crate::db::open_conn(&ctx.db_path)
            .ok()
            .and_then(|conn| crate::db::count_files(&conn, &ctx.space_id).ok())
            .unwrap_or(0)
    }

    fn citation_count(&self) -> u64 {
        let Some(ctx) = &self.files else { return 0 };
        crate::db::open_conn(&ctx.db_path)
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

    /// Tool definitions to attach to the request, or empty to send a plain
    /// request (keeps models that don't support tools working). `search` always works —
    /// it prefers configured API backends, then uses keyless HTML fallbacks
    /// when those are unavailable, so it needs no setup.
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tool_definitions(false)
    }

    /// The same live tools and schemas, with concise descriptions for small models.
    pub fn defs_compact(&self) -> Vec<ToolDef> {
        self.tool_definitions(true)
    }

    // Long by design (tool-definition table).
    #[allow(clippy::too_many_lines)]
    fn tool_definitions(&self, compact: bool) -> Vec<ToolDef> {
        let def = |spec: &spec::ToolSpec, live: &[&str]| {
            if compact {
                spec.def_compact(live)
            } else {
                spec.def(live)
            }
        };
        let mut defs = Vec::new();
        defs.push(tool_def(
            "batch",
            "Run several independent tool operations in ONE call — multiple searches, multiple file \
             searches/reads, or multiple app/script writes. Every result comes back in a single \
             round-trip, each labeled [n/N]. Prefer this over calling tools one by one whenever you \
             need several operations at once. Sub-calls use the same public tool names and \
             parameters as normal calls. Never nest batch inside batch; keep dependent steps (e.g. \
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
        let has_skills = !load_skills_from_dirs(&self.skill_dirs).is_empty();
        let mut skills_actions = vec!["create", "install"];
        if has_skills {
            skills_actions.insert(0, "load");
        }
        defs.push(def(&spec::SKILLS, &skills_actions));
        defs.push(def(
            &spec::SCRIPTS,
            &["list", "read", "write", "edit", "run", "python", "install"],
        ));
        defs.push(def(&spec::SEARCH, &["web", "academic", "discussion"]));
        defs.push(def(&spec::FETCH_URL, &["fetch_url"]));
        if self.workspace.is_some() {
            defs.push(def(
                &spec::FS,
                &[
                    "list", "glob", "grep", "read", "edit", "write", "move", "delete",
                ],
            ));
            defs.push(def(&spec::SHELL, &["run", "log"]));
        }
        if self.interactive_ui {
            defs.push(def(&spec::PROPOSE_COMMAND, &["propose_command"]));
        }
        if self.workspace.is_none() && self.approver.is_some() {
            defs.push(def(&spec::REQUEST_WORKSPACE, &["request_workspace"]));
        }
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
            defs.push(def(&spec::RESEARCH_LOOKUP, &scopes));
        }
        if self.files_count() > 0 {
            defs.push(def(&spec::FILES, &["search", "read", "pdf_page"]));
        }
        if self.apps.is_some() {
            defs.push(tool_def(
                "app",
                "Build and manage locally served web apps. action=read returns stable hash-lines for safe editing and action=search greps non-ignored files; action=write replaces complete content, action=patch applies hash-line edits (include the line number when identical lines repeat), and action=diff previews a complete candidate without writing; action=list shows conversation/space images and action=copy_file/copy_images bring user data into an app (images go to _images/, text files to the app KV store); action=init scaffolds a React starter (framework astro = Astro + React islands, vite-react = Vite SPA; default astro) and action=build compiles the app — it installs missing deps from package.json, runs the framework's static build with --base=/<app-uuid>/ (so asset links resolve under the app's URL), and serves the result from dist/ (build errors come back here for you to fix; requires node/npm on this machine).",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["read", "search", "write", "patch", "diff", "list", "copy_file", "copy_images", "init", "build"] },
                        "app": { "type": "string", "description": "app name or UUID" },
                        "path": { "type": "string", "description": "file path within the app" },
                        "pattern": { "type": "string", "description": "case-insensitive search text for search" },
                        "content": { "type": "string", "description": "complete content for write or diff" },
                        "edits": { "type": "array", "description": "hash-line edits from the latest read; hashes remain valid when earlier lines move, and include line when identical lines repeat", "items": { "type": "object", "properties": { "hash": { "type": "string", "description": "hash from the latest read" }, "line": { "type": "integer", "description": "optional 1-based line number to disambiguate repeated lines" }, "new": { "type": ["string", "null"] } }, "required": ["hash"] } },
                        "file_name": { "type": "string", "description": "imported file name for copy_file" },
                        "image_ids": { "type": "array", "items": { "type": "string" }, "description": "image IDs for copy_images" },
                        "framework": { "type": "string", "enum": ["astro", "vite-react"], "description": "starter template for init (default astro)" },
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
            defs.push(def(&spec::MEDIA, &media_actions));
        }
        if self.research_only {
            defs.retain(|d| matches!(d.name.as_str(), "search" | "fetch_url"));
        }
        if compact {
            for tool in &mut defs {
                if matches!(tool.name.as_str(), "batch" | "app") {
                    if let Some(end) = tool.description.find(". ") {
                        tool.description.truncate(end + 2);
                    }
                    strip_param_descriptions(&mut tool.parameters);
                }
            }
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
            Some(ctx) => ctx.public_base.as_ref().map_or_else(
                || format!("live at http://127.0.0.1:{}/{}/", ctx.server_port, uuid),
                |base| format!("live at {}", crate::appserver::public_app_url(base, uuid)),
            ),
            None => String::new(),
        }
    }

    /// The build tool a package.json declares (dependencies or
    /// devDependencies), if any: "astro" or "vite".
    #[must_use]
    fn declared_build_tool(pkg: &serde_json::Value) -> Option<&'static str> {
        let mut deps: Vec<String> = Vec::new();
        for key in ["dependencies", "devDependencies"] {
            if let Some(obj) = pkg.get(key).and_then(serde_json::Value::as_object) {
                deps.extend(obj.keys().cloned());
            }
        }
        if deps.iter().any(|d| d == "astro") {
            Some("astro")
        } else if deps.iter().any(|d| d == "vite") {
            Some("vite")
        } else {
            None
        }
    }

    /// Run a tool by name. Returns `(result text sent back to the model,
    /// status label shown in the UI while it runs)`.
    pub async fn run(&self, name: &str, args: &str) -> (String, String) {
        let started = Instant::now();
        let (result, status) = self.run_inner(name, args).await;
        let latency = started.elapsed();
        let error_kind = classify_tool_result(&result, &status);
        let outcome = ToolOutcome {
            ok: error_kind.is_none(),
            error_kind,
            latency,
            result_chars: result.chars().count(),
        };
        self.record_tool_event(name, args, &outcome);
        (result, status)
    }

    fn record_tool_event(&self, name: &str, args: &str, outcome: &ToolOutcome) {
        if !self.record_events {
            return;
        }
        let Some(db_path) = &self.db_path else {
            return;
        };
        let Ok(conn) = crate::db::open_attached(db_path) else {
            return;
        };
        let args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
        let action = ["action", "mode", "scope"]
            .iter()
            .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
            .unwrap_or_default();
        let now = chrono::Utc::now();
        let _ = conn.execute(
            "INSERT INTO cache.tool_events
                (at, session_id, tool, action, ok, error_kind, latency_ms, result_chars)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                now.to_rfc3339(),
                self.session_id,
                name,
                action,
                outcome.ok,
                outcome.error_kind,
                i64::try_from(outcome.latency.as_millis()).unwrap_or(i64::MAX),
                i64::try_from(outcome.result_chars).unwrap_or(i64::MAX),
            ],
        );
        let _ = conn.execute(
            "DELETE FROM cache.tool_events WHERE at < ?1",
            [(now - chrono::Duration::days(90)).to_rfc3339()],
        );
    }

    fn approval_request(&self, name: &str, args: &str) -> Result<ApprovalRequest, String> {
        use crate::workspace::escalation::{Escalation, check_command};

        let (tool, action) = name.split_once('_').expect("workspace tool action");
        let mut args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("invalid tool arguments: {error}"))?;
        args.as_object_mut()
            .ok_or_else(|| "invalid tool arguments: expected an object".to_string())?
            .insert("action".to_string(), action.into());
        spec::find(tool)
            .expect("workspace tool spec")
            .validate(&args)?;
        let workspace = self.workspace.as_ref().ok_or_else(|| {
            format!("{tool} needs an agent workspace · switch with /agent <path>")
        })?;
        let summary = if tool == "shell" {
            let cmd = args["cmd"].as_str().expect("validated command");
            let mut summary = match check_command(cmd) {
                Ok(()) => String::new(),
                Err(Escalation::Blocked(word)) => {
                    return Err(format!(
                        "privilege escalation is blocked: {word} · ask the user to run it themselves (propose the command; they can run it with !)"
                    ));
                }
                Err(Escalation::Unparseable(reason)) => {
                    format!("could not parse safely: {reason}\n")
                }
            };
            let cwd = workspace
                .resolve(args["cwd"].as_str().unwrap_or("."), Access::Read)
                .map_err(|error| error.to_string())?;
            if !cwd.is_dir() {
                return Err("invalid path: shell cwd must be a directory".to_string());
            }
            let _ = write!(summary, "$ {cmd}");
            if cwd != workspace.root() {
                let _ = write!(summary, "\nin {}", fs_relative(workspace, &cwd));
            }
            summary
        } else {
            fs_approval_summary(workspace, name, &args)?
        };
        Ok(ApprovalRequest {
            tool: tool.to_string(),
            action: action.to_string(),
            summary,
        })
    }

    // Long by design: the model tool dispatch (each arm is one tool).
    #[allow(clippy::too_many_lines)]
    async fn run_inner(&self, name: &str, args: &str) -> (String, String) {
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
        let mut allow_unparseable = false;
        if self.require_approval
            && (name.starts_with("fs_") || name == "shell_run")
            && !is_read_only_tool(name, args)
        {
            let request = match self.approval_request(name, args) {
                Ok(request) => request,
                Err(error) => return (error, "blocked".to_string()),
            };
            let Some(approver) = &self.approver else {
                return (
                    "needs approval · not available here (headless); describe the change and let the user apply it".to_string(),
                    "blocked".to_string(),
                );
            };
            let unparseable = request.summary.starts_with("could not parse safely:");
            if approver.request(request).await == Decision::Deny {
                return (
                    "denied by the user · ask what they want instead of retrying".to_string(),
                    "blocked".to_string(),
                );
            }
            allow_unparseable = unparseable;
        }
        let (result, status) = match name {
            "propose_command" => {
                if !self.interactive_ui {
                    return (
                        "propose_command needs the terminal UI · tell the user the exact command and why instead".to_string(),
                        "blocked".to_string(),
                    );
                }
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let cmd = v["cmd"].as_str().unwrap_or_default().trim();
                // The command lands in the user's composer: one visible line,
                // so nothing can hide below or behind what they read.
                if cmd.is_empty() || cmd.chars().any(char::is_control) {
                    return (
                        "invalid tool arguments: propose_command takes one non-empty line without control characters · join steps with && or propose them one at a time".to_string(),
                        "invalid arguments".to_string(),
                    );
                }
                (
                    format!(
                        "proposed to the user: !{cmd} · wait for them to run it; its output will appear in the conversation"
                    ),
                    "Proposing a command…".to_string(),
                )
            }
            "request_workspace" => {
                let Some(approver) = self.approver.as_ref().filter(|_| self.workspace.is_none())
                else {
                    return (
                        "request_workspace is only available in chat mode in the terminal UI · ask the user to run /agent <path>".to_string(),
                        "blocked".to_string(),
                    );
                };
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let root = v["root"].as_str().unwrap_or_default().trim();
                let reason = v["reason"].as_str().unwrap_or_default().trim();
                let request = ApprovalRequest {
                    tool: "request_workspace".to_string(),
                    action: "agent".to_string(),
                    summary: format!("switch this session to agent mode in {root}\n{reason}"),
                };
                if approver.request(request).await == Decision::Deny {
                    return (
                        "denied by the user · ask what they want instead of retrying".to_string(),
                        "blocked".to_string(),
                    );
                }
                (
                    format!(
                        "approved: agent mode in {root} · workspace tools are available from your next turn"
                    ),
                    "Requesting a workspace…".to_string(),
                )
            }
            "shell_run" | "shell_log" => match &self.workspace {
                None => (
                    "shell needs an agent workspace · switch with /agent <path>".to_string(),
                    "blocked".to_string(),
                ),
                Some(workspace) => (
                    crate::workspace::shell::action(
                        workspace,
                        self.db_path.as_deref(),
                        name,
                        args,
                        allow_unparseable,
                    )
                    .await,
                    format!("Running {name}…"),
                ),
            },
            "fs_list" | "fs_glob" | "fs_grep" | "fs_read" | "fs_edit" | "fs_write" | "fs_move"
            | "fs_delete" => match &self.workspace {
                None => (
                    "fs needs an agent workspace · switch with /agent <path>".to_string(),
                    "blocked".to_string(),
                ),
                Some(workspace) => {
                    let workspace = Arc::clone(workspace);
                    let action = name.to_string();
                    let args = args.to_string();
                    let result =
                        tokio::task::spawn_blocking(move || fs_action(&workspace, &action, &args))
                            .await
                            .unwrap_or_else(|error| Err(PathError::Io(error.to_string())));
                    match result {
                        Ok(result) => (result, format!("Running {name}…")),
                        Err(error) => (error.to_string(), "blocked".to_string()),
                    }
                }
            },
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
                let result = match resolve_skill_file(&self.skill_dirs, &skill_name, file) {
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
                let result = if !crate::skills::valid_skill_name(&name) {
                    "name must be 1-64 lowercase letters, digits, and single hyphens".to_string()
                } else if description.is_empty()
                    || description.chars().count() > 1_024
                    || description.contains(['\n', '\r'])
                {
                    "description must be 1-1024 characters on one line".to_string()
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
                    match (
                        resolve_skill_dir(&self.skill_dirs, &skill),
                        resolve_skill_file(&self.skill_dirs, &skill, &script),
                    ) {
                        (Err(e), _) | (_, Err(e)) => e,
                        (Ok(_dir), Ok(file)) if !file.is_file() => {
                            format!("no such script: {skill}/{script}")
                        }
                        (Ok(dir), Ok(file)) => {
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
                        match resolve_skill_dir(&self.skill_dirs, &skill) {
                            Err(_) => format!("unknown skill: {skill}"),
                            Ok(dir) if !dir.join("SKILL.md").is_file() => {
                                format!("unknown skill: {skill}")
                            }
                            Ok(dir) => {
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
                if calls.iter().any(|(tool, _)| {
                    matches!(tool.as_str(), "propose_command" | "request_workspace")
                }) {
                    return (
                        "propose_command and request_workspace cannot run inside batch — call them directly"
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
            "init_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let framework = v
                    .get("framework")
                    .and_then(|x| x.as_str())
                    .unwrap_or("astro");
                let status = format!("Scaffolding {app} ({framework})…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        match crate::app_templates::scaffold(&app_dir, framework) {
                            Err(e) => e,
                            Ok(files) => format!(
                                "scaffolded {app} with the {framework} starter ({}) — edit with read/patch/write, then `app action=build` (installs deps + compiles to dist/). {}",
                                files.join(", "),
                                self.app_link(&uuid),
                            ),
                        }
                    }
                };
                (result, status)
            }
            "build_app" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let app = v
                    .get("app")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string();
                let status = format!("Building {app}…");
                let result = match self.resolve_app(&app) {
                    Err(e) => e,
                    Ok((uuid, app_dir)) => {
                        let pkg_json = app_dir.join("package.json");
                        let Some(pkg) = std::fs::read_to_string(&pkg_json)
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        else {
                            return (
                                "no package.json — write one (or `app action=init` to scaffold a React/Astro or React/Vite app) before building".to_string(),
                                status,
                            );
                        };
                        let Some(tool) = Self::declared_build_tool(&pkg) else {
                            return (
                                "no build step detected (package.json declares neither astro nor vite) — the app is served as-is".to_string(),
                                status,
                            );
                        };
                        // Deps declared but not installed: `npm install` from
                        // package.json first, so init → build just works.
                        let mut npm_err: Option<String> = None;
                        let bin = app_dir.join("node_modules").join(".bin").join(tool);
                        if !bin.exists() {
                            let refs: Vec<&std::ffi::OsStr> =
                                ["install", "--no-audit", "--no-fund"]
                                    .iter()
                                    .map(std::ffi::OsStr::new)
                                    .collect();
                            match run_cmd("npm".as_ref(), &refs, &app_dir, 600).await {
                                Ok(out) if out.status.success() => {}
                                Ok(out) => npm_err = Some(format_output(&out)),
                                Err(e) => npm_err = Some(e),
                            }
                        }
                        if let Some(err) = npm_err {
                            return (
                                format!(
                                    "cannot build {app}: deps from package.json are missing and `npm install` failed:\n{err}"
                                ),
                                status,
                            );
                        }
                        // Absolute base: the app is served at /<uuid>/ — pass it
                        // on the CLI so asset links resolve regardless of the
                        // framework's config (relative bases are unreliable).
                        let base_arg = format!("/{uuid}/");
                        let build_args =
                            ["--no-install", tool, "build", "--base", base_arg.as_str()];
                        let refs: Vec<&std::ffi::OsStr> =
                            build_args.iter().map(std::ffi::OsStr::new).collect();
                        match run_cmd("npx".as_ref(), &refs, &app_dir, 600).await {
                            Ok(out) if out.status.success() => {
                                // Only flip the served dir when the build
                                // really produced dist/ — a framework whose
                                // outDir is configured elsewhere exits 0
                                // without it, and serving dist/ then would
                                // 404 the whole app.
                                if !app_dir.join("dist").is_dir() {
                                    return (
                                        "build reported success but produced no dist/ — the \
framework's output directory is configured elsewhere (outDir/base); build output \
must land in dist/ for the app server to serve it"
                                            .to_string(),
                                        status,
                                    );
                                }
                                if let Some(ctx) = &self.apps {
                                    ctx.registry.set_served_from(&uuid, "dist");
                                }
                                format!("build ok — dist/ is live at {}", self.app_link(&uuid))
                            }
                            Ok(out) => format!("build failed:\n{}", format_output(&out)),
                            Err(e) => e,
                        }
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
                let edits: Vec<(String, Option<usize>, Option<String>)> = v
                    .get("edits")
                    .and_then(|e| e.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                let hash = e.get("hash")?.as_str()?.to_string();
                                let line = e
                                    .get("line")
                                    .and_then(serde_json::Value::as_u64)
                                    .and_then(|n| usize::try_from(n).ok());
                                let new = e.get("new").and_then(|n| n.as_str()).map(str::to_string);
                                Some((hash, line, new))
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
                let result = match confined_join(&self.space_scripts_dir, &path) {
                    Err(e) => e,
                    Ok(file) => {
                        let write = file
                            .parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|()| std::fs::write(&file, &content));
                        match write {
                            Ok(()) => format!("wrote {path} ({} bytes)", content.len()),
                            Err(e) => format!("write failed: {e}"),
                        }
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
                let result = match confined_join(&self.space_scripts_dir, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
                        Err(e) => format!("cannot read {path}: {e}"),
                        Ok(text) => {
                            let lines: Vec<&str> = text.lines().collect();
                            let total = lines.len();
                            let start = (offset - 1).min(total);
                            let slice = &lines[start..(start + limit).min(total)];
                            if slice.is_empty() {
                                format!("{path}: offset {offset} is past the end ({total} lines)")
                            } else {
                                format!(
                                    "{path} (lines {}-{} of {total}):\n{}",
                                    start + 1,
                                    start + slice.len(),
                                    number_lines_with_hash(slice, start),
                                )
                            }
                        }
                    },
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
                let edits: Vec<(String, Option<usize>, Option<String>)> = v
                    .get("edits")
                    .and_then(|e| e.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| {
                                let hash = e.get("hash")?.as_str()?.to_string();
                                let line = e
                                    .get("line")
                                    .and_then(serde_json::Value::as_u64)
                                    .and_then(|n| usize::try_from(n).ok());
                                let new = e.get("new").and_then(|n| n.as_str()).map(str::to_string);
                                Some((hash, line, new))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let status = format!("Editing {path}…");
                let result = match confined_join(&self.space_scripts_dir, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
                        Err(e) => format!("cannot read {path}: {e}"),
                        Ok(text) => match apply_hashline_edits(&text, &edits) {
                            Err(e) => e,
                            Ok((new_text, diff)) => match std::fs::write(&file, new_text) {
                                Ok(()) => format!("edited {path} — {diff}"),
                                Err(e) => format!("write failed: {e}"),
                            },
                        },
                    },
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
/// are still classified; mutating consolidations (`batch`) are never read-only.
pub fn is_read_only_tool(name: &str, args: &str) -> bool {
    if let Some(read_only) = spec::read_only(name, args) {
        return read_only;
    }
    let action_is = |actions: &[&str]| {
        serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(str::to_string))
            .is_some_and(|a| actions.contains(&a.as_str()))
    };
    match name {
        "app" => action_is(&["read", "search", "diff", "list"]),
        _ => matches!(
            name,
            "app_inspect" | "read_app_file" | "grep_app" | "diff_app" | "list_images"
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

/// Cap context-visible text while retaining roughly 70% head and 30% tail.
/// Counts Unicode characters, including the omission marker, not UTF-8 bytes.
#[must_use]
pub fn cap_head_tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    // Reserve enough marker space even if the omitted count has fewer digits.
    let marker = format!("\n… [{count} characters omitted] …\n");
    let keep = max_chars.saturating_sub(marker.chars().count());
    let head = keep * 7 / 10;
    let tail = keep - head;
    let omitted = count - keep;
    let marker = format!("\n… [{omitted} characters omitted] …\n");
    let mut result: String = text.chars().take(head).collect();
    result.extend(marker.chars().take(max_chars - keep));
    result.extend(text.chars().skip(count - tail));
    result
}

/// Remove schema annotations without removing parameters named `description`.
fn strip_param_descriptions(schema: &mut serde_json::Value) {
    if let Some(object) = schema.as_object_mut() {
        object.remove("description");
    }
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        for property in properties.values_mut() {
            strip_param_descriptions(property);
        }
    }
    if let Some(items) = schema.get_mut("items") {
        strip_param_descriptions(items);
    }
}

/// Quick check: does a string look like a UUID (36 chars, 4 dashes)?
fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

fn fs_action(workspace: &Arc<Workspace>, name: &str, args: &str) -> Result<String, PathError> {
    let args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let rel = args
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(if matches!(name, "fs_list" | "fs_glob" | "fs_grep") {
            "."
        } else {
            ""
        });
    let access = if matches!(name, "fs_edit" | "fs_write" | "fs_move" | "fs_delete") {
        Access::Write
    } else {
        Access::Read
    };
    let path = workspace.resolve(rel, access)?;
    if access == Access::Write {
        return fs_mutate(workspace, &path, name, &args);
    }
    let number = |key: &str, default: usize, max: usize| {
        usize::try_from(
            args.get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(default as u64),
        )
        .unwrap_or(max)
        .min(max)
    };
    let pattern = args
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match name {
        "fs_list" => {
            let mut entries = Vec::new();
            fs_list_dir(workspace, &path, number("depth", 2, 6), &mut entries)?;
            Ok(fs_capped_entries(entries, 400))
        }
        "fs_glob" => {
            let base = if path.is_dir() {
                &path
            } else {
                path.parent().unwrap_or(&path)
            };
            let mut builder = ignore::overrides::OverrideBuilder::new(base);
            builder
                .add(pattern)
                .map_err(|error| PathError::Invalid(error.to_string()))?;
            let matcher = builder
                .build()
                .map_err(|error| PathError::Invalid(error.to_string()))?;
            let mut entries = Vec::new();
            for entry in fs_walk(workspace, &path, None) {
                let entry = entry.map_err(|error| PathError::Io(error.to_string()))?;
                if matcher
                    .matched(
                        entry.path(),
                        entry.file_type().is_some_and(|kind| kind.is_dir()),
                    )
                    .is_whitelist()
                {
                    entries.push(fs_relative(workspace, entry.path()));
                }
            }
            entries.sort();
            Ok(fs_capped_entries(entries, 400))
        }
        "fs_grep" => fs_grep(workspace, &path, pattern, number("context", 0, 5)),
        "fs_read" => fs_read(
            workspace,
            &path,
            number("offset", 1, usize::MAX).max(1),
            number("limit", 400, 2000).max(1),
        ),
        _ => unreachable!("fs action dispatch"),
    }
}

fn fs_mutate(
    workspace: &Workspace,
    path: &Path,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, PathError> {
    let rel = fs_relative(workspace, path);
    if name == "fs_write" && !path.try_exists()? {
        return fs_replace(workspace, path, name, args, "", None);
    }
    let Some(previous) = workspace.last_read(path) else {
        return Ok(format!(
            "fs.{} needs a fresh read · call fs with action=read on {rel} first",
            name.trim_start_matches("fs_")
        ));
    };
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(PathError::Invalid(format!("{rel} is not a file")));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let current_stamp = Workspace::stamp_bytes(&bytes, &metadata)?;
    let current = String::from_utf8_lossy(&bytes);
    if previous.len != current_stamp.len || previous.sha256 != current_stamp.sha256 {
        let mut result = format!("{rel} changed since your last read · re-read it and re-apply");
        if let Some(text) = previous.text {
            let _ = write!(result, "\n{}", fs_diff(&rel, &text, &current, 60).0);
        }
        return Ok(result);
    }
    if name == "fs_edit" && (bytes.contains(&0) || std::str::from_utf8(&bytes).is_err()) {
        return Ok(format!(
            "fs.edit needs a UTF-8 text file · {rel} is binary or not UTF-8; use write to replace it whole"
        ));
    }
    match name {
        "fs_edit" | "fs_write" => fs_replace(
            workspace,
            path,
            name,
            args,
            &current,
            Some(metadata.permissions()),
        ),
        "fs_move" => {
            let to = args
                .get("to")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let target = workspace.resolve(to, Access::Write)?;
            if target.try_exists()? {
                return Ok(format!("fs.move target exists: {to} · choose a new path"));
            }
            std::fs::create_dir_all(target.parent().expect("workspace file parent"))?;
            workspace.before_change(path)?;
            workspace.before_change(&target)?;
            // A hard link refuses a target created after the existence check.
            #[cfg(unix)]
            match std::fs::hard_link(path, &target) {
                Ok(()) => std::fs::remove_file(path)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Ok(format!("fs.move target exists: {to} · choose a new path"));
                }
                Err(_) => std::fs::rename(path, &target)?,
            }
            #[cfg(not(unix))]
            std::fs::rename(path, &target)?;
            workspace.after_change(path);
            workspace.after_change(&target);
            workspace.forget_read(path);
            workspace.record_read(&target, current_stamp);
            Ok(format!(
                "moved {rel} -> {}",
                fs_relative(workspace, &target)
            ))
        }
        "fs_delete" => {
            let (diff, _, _) = fs_diff(&rel, &current, "", 200);
            workspace.before_change(path)?;
            std::fs::remove_file(path)?;
            workspace.after_change(path);
            workspace.forget_read(path);
            Ok(format!(
                "deleted {rel} (+0 −{})\n{diff}",
                current.lines().count()
            ))
        }
        _ => unreachable!("fs mutation dispatch"),
    }
}

fn fs_replace(
    workspace: &Workspace,
    path: &Path,
    name: &str,
    args: &serde_json::Value,
    current: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<String, PathError> {
    let content = match fs_replacement(name, args, current) {
        Ok(content) => content,
        Err(error) => return Ok(error),
    };
    let rel = fs_relative(workspace, path);
    let (diff, added, deleted) = fs_diff(&rel, current, &content, 200);
    workspace.before_change(path)?;
    let stamp = fs_atomic_write(path, &content, permissions)?;
    workspace.after_change(path);
    workspace.record_read(path, stamp);
    let verb = if name == "fs_edit" { "edited" } else { "wrote" };
    Ok(format!("{verb} {rel} (+{added} −{deleted})\n{diff}"))
}

fn fs_replacement(name: &str, args: &serde_json::Value, current: &str) -> Result<String, String> {
    let mut content = if name == "fs_edit" {
        let edits: Vec<_> = args
            .get("edits")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|edit| {
                Some((
                    edit.get("hash")?.as_str()?.to_string(),
                    edit.get("line")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|line| usize::try_from(line).ok()),
                    edit.get("new")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                ))
            })
            .collect();
        apply_hashline_edits(current, &edits)?.0
    } else {
        args.get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if current.contains("\r\n") && !content.contains("\r\n") {
        content = content.replace('\n', "\r\n");
    }
    Ok(content)
}

fn fs_approval_summary(
    workspace: &Workspace,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let path = workspace
        .resolve(
            args["path"].as_str().expect("validated path"),
            Access::Write,
        )
        .map_err(|error| error.to_string())?;
    let rel = fs_relative(workspace, &path);
    let exists = path.try_exists().map_err(|error| error.to_string())?;
    if name == "fs_write" && !exists {
        return Ok(format!("write {rel}"));
    }
    let previous = workspace.last_read(&path).ok_or_else(|| {
        format!(
            "fs.{} needs a fresh read · call fs with action=read on {rel} first",
            name.trim_start_matches("fs_")
        )
    })?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    let stamp = Workspace::stamp_bytes(&bytes, &metadata).map_err(|error| error.to_string())?;
    let current = String::from_utf8_lossy(&bytes);
    if previous.len != stamp.len || previous.sha256 != stamp.sha256 {
        let mut result = format!("{rel} changed since your last read · re-read it and re-apply");
        if let Some(text) = previous.text {
            let _ = write!(result, "\n{}", fs_diff(&rel, &text, &current, 60).0);
        }
        return Err(result);
    }
    match name {
        "fs_write" | "fs_edit" => {
            if name == "fs_edit" && (bytes.contains(&0) || std::str::from_utf8(&bytes).is_err()) {
                return Err(format!(
                    "fs.edit needs a UTF-8 text file · {rel} is binary or not UTF-8; use write to replace it whole"
                ));
            }
            let content = fs_replacement(name, args, &current)?;
            Ok(format!(
                "{} {rel}\n{}",
                name.trim_start_matches("fs_"),
                fs_diff(&rel, &current, &content, 40).0
            ))
        }
        "fs_move" => {
            let to = args["to"].as_str().expect("validated destination");
            let target = workspace
                .resolve(to, Access::Write)
                .map_err(|error| error.to_string())?;
            if target.try_exists().map_err(|error| error.to_string())? {
                return Err(format!("fs.move target exists: {to} · choose a new path"));
            }
            Ok(format!("move {rel} -> {}", fs_relative(workspace, &target)))
        }
        "fs_delete" => Ok(format!("delete {rel} ({} lines)", current.lines().count())),
        _ => unreachable!("fs approval action"),
    }
}

fn fs_atomic_write(
    path: &Path,
    content: &str,
    permissions: Option<std::fs::Permissions>,
) -> Result<crate::workspace::FileStamp, PathError> {
    let parent = path.parent().expect("workspace file parent");
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".nexus-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        let stamp = Workspace::stamp_bytes(content.as_bytes(), &file.metadata()?)?;
        std::fs::rename(&temp, path)?;
        Ok(stamp)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn fs_diff(path: &str, current: &str, candidate: &str, limit: usize) -> (String, usize, usize) {
    let diff = unified_diff("workspace", path, current, candidate);
    let added = diff
        .lines()
        .skip_while(|line| !line.starts_with("@@"))
        .filter(|line| line.starts_with('+'))
        .count();
    let deleted = diff
        .lines()
        .skip_while(|line| !line.starts_with("@@"))
        .filter(|line| line.starts_with('-'))
        .count();
    let mut capped = diff.lines().take(limit).collect::<Vec<_>>().join("\n");
    let more = diff.lines().count().saturating_sub(limit);
    if more > 0 {
        let _ = write!(capped, "\n… {more} more lines");
    }
    (capped, added, deleted)
}

// Never follow directory symlinks, and apply the workspace's hard exclusions
// before entering directories or reading files.
fn fs_walk(workspace: &Arc<Workspace>, path: &Path, depth: Option<usize>) -> ignore::Walk {
    let workspace = Arc::clone(workspace);
    ignore::WalkBuilder::new(path)
        .hidden(false)
        .require_git(false)
        .git_global(false)
        .follow_links(false)
        .max_depth(depth)
        .sort_by_file_name(std::ffi::OsStr::cmp)
        .filter_entry(move |entry| {
            entry.file_name() != ".git"
                && workspace
                    .resolve(&entry.path().to_string_lossy(), Access::Read)
                    .is_ok()
        })
        .build()
}

fn fs_relative(workspace: &Workspace, path: &Path) -> String {
    let rel = path
        .strip_prefix(workspace.root())
        .expect("confined workspace path");
    if rel.as_os_str().is_empty() {
        ".".to_string()
    } else {
        rel.to_string_lossy().into_owned()
    }
}

fn fs_capped_entries(mut entries: Vec<String>, max: usize) -> String {
    let more = entries.len().saturating_sub(max);
    entries.truncate(max);
    if more > 0 {
        entries.push(format!("… {more} more"));
    }
    entries.join("\n")
}

fn fs_list_dir(
    workspace: &Arc<Workspace>,
    path: &Path,
    depth: usize,
    out: &mut Vec<String>,
) -> Result<(), PathError> {
    let label = fs_relative(workspace, path);
    if !path.is_dir() {
        std::fs::metadata(path)?;
        out.push(label);
        return Ok(());
    }
    let mut entries = Vec::new();
    if depth > 0 {
        for entry in fs_walk(workspace, path, Some(1)).skip(1) {
            entries.push(entry.map_err(|error| PathError::Io(error.to_string()))?);
        }
    }
    if entries.len() > 100 {
        out.push(format!("{label}/ ({} entries)", entries.len()));
    } else {
        out.push(format!("{label}/"));
        for entry in entries {
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                fs_list_dir(workspace, entry.path(), depth - 1, out)?;
            } else {
                out.push(fs_relative(workspace, entry.path()));
            }
        }
    }
    Ok(())
}

fn fs_read(
    workspace: &Workspace,
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<String, PathError> {
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Ok(format!(
            "file too large ({} bytes) · use fs grep or shell (sed -n, head) for a slice",
            metadata.len()
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut result = if bytes.contains(&0) {
        format!("binary file ({} bytes) · not shown", bytes.len())
    } else if let Ok(text) = std::str::from_utf8(&bytes) {
        let lines: Vec<_> = text.lines().collect();
        let start = offset.saturating_sub(1).min(lines.len());
        let end = start.saturating_add(limit).min(lines.len());
        let (first, last) = if start == end {
            (0, 0)
        } else {
            (start + 1, end)
        };
        let mut result = format!(
            "{} (lines {first}-{last} of {}):\n{}",
            fs_relative(workspace, path),
            lines.len(),
            number_lines_with_hash(&lines[start..end], start)
        );
        if end < lines.len() {
            let _ = write!(result, "\n… continue with offset={}", end + 1);
        }
        result
    } else {
        format!("binary file ({} bytes) · not shown", bytes.len())
    };
    workspace.record_read(path, Workspace::stamp_bytes(&bytes, &metadata)?);
    // Empty files still have a header, but no empty hash-line.
    if result.ends_with('\n') {
        result.pop();
    }
    Ok(result)
}

fn fs_grep(
    workspace: &Arc<Workspace>,
    path: &Path,
    pattern: &str,
    context: usize,
) -> Result<String, PathError> {
    let mut result = String::new();
    let mut total = 0;
    let mut shown = 0;
    let mut skipped = 0;
    for entry in fs_walk(workspace, path, None) {
        let entry = entry.map_err(|error| PathError::Io(error.to_string()))?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let file = workspace.resolve(&entry.path().to_string_lossy(), Access::Read)?;
        if std::fs::metadata(&file)?.len() > 4 * 1024 * 1024 {
            skipped += 1;
            continue;
        }
        let bytes = std::fs::read(&file)?;
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<_> = text.lines().collect();
        let matches: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| line.contains(pattern).then_some(index))
            .collect();
        total += matches.len();
        let keep = matches.len().min(200 - shown);
        if keep == 0 {
            continue;
        }
        let _ = writeln!(result, "{}:", fs_relative(workspace, &file));
        let mut selected = std::collections::BTreeSet::new();
        let end = matches.get(keep).copied().unwrap_or(lines.len());
        for index in &matches[..keep] {
            selected
                .extend(index.saturating_sub(context)..index.saturating_add(context + 1).min(end));
        }
        for index in selected {
            let _ = writeln!(result, "  {}: {}", index + 1, lines[index]);
        }
        shown += keep;
    }
    if total > shown {
        let _ = writeln!(result, "… {} more", total - shown);
    }
    if result.is_empty() {
        result.push_str("no matches\n");
    }
    if skipped > 0 {
        let _ = writeln!(
            result,
            "… skipped {skipped} files over 4 MiB · use shell for those"
        );
    }
    Ok(result.trim_end().to_string())
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && std::path::Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn confined_join(root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    if !valid_relative_path(rel) {
        return Err(format!("invalid path: {rel:?}"));
    }
    let root_ancestor = root
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .unwrap_or(root);
    let canonical_root = std::fs::canonicalize(root_ancestor)
        .map_err(|e| format!("cannot resolve scripts directory: {e}"))?;
    let path = root.join(rel);
    // This includes the file itself when it exists, or its nearest existing parent otherwise.
    let ancestor = path
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor).is_ok())
        .unwrap_or(&path);
    let canonical_path = std::fs::canonicalize(ancestor)
        .map_err(|_| format!("path escapes the scripts directory: {rel:?}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(format!("path escapes the scripts directory: {rel:?}"));
    }
    Ok(path)
}

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

/// `cat -n`-style numbering for ranged reads, matching what agent harnesses
/// feed models so line references and edits anchor reliably.
fn number_lines(slice: &[&str], start: usize) -> String {
    slice
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>5}\t{l}", start + i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// sha256(content) truncated to 8 hex chars. Unlike a line-number hash,
/// this remains valid when an earlier edit inserts or removes lines. Repeated
/// identical lines are rejected as ambiguous unless the optional line hint is
/// supplied with the edit.
fn line_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hasher
        .finalize()
        .iter()
        .take(4)
        .fold(String::new(), |mut h, b| {
            let _ = write!(h, "{b:02x}");
            h
        })
}

/// Hash format used by older reads. Keep accepting it for a running session
/// after the stable content-hash format is deployed.
fn legacy_line_hash(line_no: usize, content: &str) -> String {
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
            format!("{:>5}:{}\t{l}", n, line_hash(l))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply hashline edits to `text`: each `(hash, line_hint, new)` targets the
/// matching line content (see `line_hash`). `new: None` deletes that line;
/// `Some(t)` replaces it with `t`'s lines (0, 1, or many — this is also how
/// you insert: include the original line's content in `t`). The optional
/// line hint disambiguates repeated identical lines. Hashes are resolved
/// against the current file and never silently land on a different line.
/// Returns the new file content plus a git-diff-style summary of what changed,
/// in the order edits were given.
fn apply_hashline_edits(
    text: &str,
    edits: &[(String, Option<usize>, Option<String>)],
) -> Result<(String, String), String> {
    if edits.is_empty() {
        return Err("edits must not be empty".to_string());
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut resolved: Vec<(usize, Option<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (hash, line_hint, new) in edits {
        let matches: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| line_hash(l) == *hash || legacy_line_hash(i + 1, l) == *hash)
            .map(|(i, _)| i)
            .collect();
        let idx = match matches.as_slice() {
            [] => {
                return Err(format!(
                    "hash {hash} not found — read the file again, it may have changed"
                ));
            }
            [idx] => *idx,
            _ => line_hint
                .and_then(|line| line.checked_sub(1))
                .filter(|idx| matches.contains(idx))
                .ok_or_else(|| {
                    format!(
                        "hash {hash} matches multiple lines — include the 1-based line from the latest read"
                    )
                })?,
        };
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
            // node_modules/.venv/.git are dependencies; dist/ is derived
            // build output (framework apps) — never search inside any of them.
            if name != "node_modules" && name != ".venv" && name != ".git" && name != "dist" {
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
    crate::config::scrub_secret_env(cmd.as_std_mut());
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
    use serde_json::json;

    struct FakeApprover {
        decision: Decision,
        requests: std::sync::Mutex<Vec<ApprovalRequest>>,
    }

    impl FakeApprover {
        fn new(decision: Decision) -> Arc<Self> {
            Arc::new(Self {
                decision,
                requests: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    impl Approver for FakeApprover {
        fn request(
            &self,
            req: ApprovalRequest,
        ) -> Pin<Box<dyn Future<Output = Decision> + Send + '_>> {
            self.requests.lock().unwrap().push(req);
            Box::pin(std::future::ready(self.decision))
        }
    }

    #[tokio::test]
    async fn approval_denies_without_writing_and_allows_an_edit_once() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let path = toolbox.workspace.as_ref().unwrap().root().join("file");
        std::fs::write(&path, "original\n").unwrap();
        toolbox
            .run("fs", r#"{"action":"read","path":"file"}"#)
            .await;
        let args = json!({
            "action": "edit", "path": "file",
            "edits": [{"hash": line_hash("original"), "new": "replacement"}],
        })
        .to_string();
        let deny = FakeApprover::new(Decision::Deny);
        toolbox.approver = Some(deny.clone());
        let (result, status) = toolbox.run("fs", &args).await;
        assert_eq!(
            result,
            "denied by the user · ask what they want instead of retrying"
        );
        assert_eq!(status, "blocked");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
        let requests = deny.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].tool, "fs");
        assert_eq!(requests[0].action, "edit");
        assert!(requests[0].summary.starts_with("edit file\n"));
        assert!(requests[0].summary.contains("\n--- "));
        assert!(requests[0].summary.contains("\n-original\n+replacement"));
        let allow = FakeApprover::new(Decision::Allow);
        toolbox.approver = Some(allow.clone());
        let (result, status) = toolbox.run("fs", &args).await;
        assert!(result.starts_with("edited file"), "{result}");
        assert_ne!(status, "blocked");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement\n");
        assert_eq!(allow.requests.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn approval_summaries_cover_writes_moves_and_deletes() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let deny = FakeApprover::new(Decision::Deny);
        toolbox.approver = Some(deny.clone());
        let path = toolbox.workspace.as_ref().unwrap().root().join("file");
        let original = "old\n".repeat(60);
        std::fs::write(&path, &original).unwrap();
        toolbox
            .run("fs", r#"{"action":"read","path":"file"}"#)
            .await;
        for args in [
            json!({"action": "write", "path": "new", "content": "new\n"}),
            json!({"action": "write", "path": "file", "content": "new\n".repeat(60)}),
            json!({"action": "move", "path": "file", "to": "moved"}),
            json!({"action": "delete", "path": "file"}),
        ] {
            assert_eq!(toolbox.run("fs", &args.to_string()).await.1, "blocked");
        }
        let requests = deny.requests.lock().unwrap();
        assert_eq!(requests[0].summary, "write new");
        assert!(requests[1].summary.starts_with("write file\n"));
        assert!(requests[1].summary.contains("\n--- "));
        assert_eq!(requests[1].summary.lines().count(), 42);
        assert!(
            requests[1]
                .summary
                .lines()
                .last()
                .unwrap()
                .contains("more lines")
        );
        assert_eq!(requests[2].summary, "move file -> moved");
        assert_eq!(requests[3].summary, "delete file (60 lines)");
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn invalid_workspace_mutations_never_request_approval() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let allow = FakeApprover::new(Decision::Allow);
        toolbox.approver = Some(allow.clone());
        let path = toolbox.workspace.as_ref().unwrap().root().join("file");
        std::fs::write(&path, "original\n").unwrap();
        for (tool, args) in [
            ("fs", json!({"action": "write", "path": "file"})),
            ("fs_write", json!(null)),
            (
                "fs",
                json!({"action": "write", "path": "../outside", "content": "no"}),
            ),
            (
                "fs",
                json!({"action": "write", "path": ".env", "content": "no"}),
            ),
            ("fs", json!({"action": "delete", "path": "file"})),
            (
                "shell",
                json!({"action": "run", "cmd": "echo no", "cwd": "file"}),
            ),
        ] {
            let (result, _) = toolbox.run(tool, &args.to_string()).await;
            assert!(!result.starts_with("needs approval"), "{result}");
        }
        toolbox
            .run("fs", r#"{"action":"read","path":"file"}"#)
            .await;
        let (result, _) = toolbox
            .run(
                "fs",
                &json!({
                    "action": "edit", "path": "file", "edits": [{"hash": "missing", "new": "no"}],
                })
                .to_string(),
            )
            .await;
        assert!(result.contains("not found"), "{result}");
        std::fs::write(&path, "changed\n").unwrap();
        let (result, _) = toolbox
            .run("fs", r#"{"action":"delete","path":"file"}"#)
            .await;
        assert!(result.contains("changed since your last read"), "{result}");
        assert!(allow.requests.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "changed\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn shell_approval_explains_unparseable_input_but_never_allows_escalation() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let deny = FakeApprover::new(Decision::Deny);
        toolbox.approver = Some(deny.clone());
        let (result, status) = toolbox
            .run("shell", r#"{"action":"run","cmd":"\"$COMMAND\""}"#)
            .await;
        assert!(result.starts_with("denied by the user"), "{result}");
        assert_eq!(status, "blocked");
        let requests = deny.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].summary.starts_with("could not parse safely:"));
        assert!(requests[0].summary.ends_with("$ \"$COMMAND\""));
        let (result, status) = toolbox
            .run("shell", r#"{"action":"run","cmd":"sudo true"}"#)
            .await;
        assert!(
            result.starts_with("privilege escalation is blocked"),
            "{result}"
        );
        assert_eq!(status, "blocked");
        assert_eq!(deny.requests.lock().unwrap().len(), 1);
        std::fs::create_dir(toolbox.workspace.as_ref().unwrap().root().join("sub")).unwrap();
        toolbox
            .run("shell", r#"{"action":"run","cmd":"true","cwd":"sub"}"#)
            .await;
        assert_eq!(deny.requests.lock().unwrap()[1].summary, "$ true\nin sub");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unparseable_shell_runs_only_with_permission_for_that_call() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.db_path = Some(dir.join("nexus.db"));
        let path = toolbox.workspace.as_ref().unwrap().root().join("approved");
        let args = json!({
            "action": "run",
            "cmd": "program=printf; \"$program\" approved > approved",
        })
        .to_string();
        let (result, _) = toolbox.run("shell", &args).await;
        assert!(
            result.starts_with("could not parse this command safely"),
            "{result}"
        );
        assert!(!path.exists());
        toolbox.require_approval = true;
        let deny = FakeApprover::new(Decision::Deny);
        toolbox.approver = Some(deny.clone());
        assert_eq!(toolbox.run("shell", &args).await.1, "blocked");
        assert!(!path.exists());
        assert!(
            deny.requests.lock().unwrap()[0]
                .summary
                .starts_with("could not parse safely:")
        );
        let allow = FakeApprover::new(Decision::Allow);
        toolbox.approver = Some(allow.clone());
        let (result, status) = toolbox.run("shell", &args).await;
        assert!(result.starts_with("exit 0"), "{result}");
        assert_ne!(status, "blocked");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "approved");
        assert_eq!(allow.requests.lock().unwrap().len(), 1);
        let (result, status) = toolbox
            .run("shell", r#"{"action":"run","cmd":"sudo true"}"#)
            .await;
        assert!(
            result.starts_with("privilege escalation is blocked"),
            "{result}"
        );
        assert_eq!(status, "blocked");
        assert_eq!(allow.requests.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn dropped_approval_reply_blocks_the_tool_call() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        toolbox.approver = Some(Arc::new(crate::app::ChannelApprover::new(sender)));
        let (outcome, ()) = tokio::join!(
            toolbox.run("fs", r#"{"action":"write","path":"new","content":"no"}"#),
            async {
                let (_, reply) = receiver.recv().await.unwrap();
                drop(reply);
            },
        );
        assert!(outcome.0.starts_with("denied by the user"));
        assert_eq!(outcome.1, "blocked");
        assert!(
            !toolbox
                .workspace
                .as_ref()
                .unwrap()
                .root()
                .join("new")
                .exists()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn fs_toolbox() -> (ToolBox, PathBuf) {
        let (mut toolbox, dir) = skills_toolbox();
        let root = dir.join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        toolbox.workspace = Some(Arc::new(Workspace::open(&root, Vec::new()).unwrap()));
        (toolbox, dir)
    }

    #[tokio::test]
    async fn workspace_mutations_need_approval_and_leave_files_untouched() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let root = toolbox.workspace.as_ref().unwrap().root();
        let path = root.join("file");
        std::fs::write(&path, "original\n").unwrap();
        toolbox
            .run("fs", r#"{"action":"read","path":"file"}"#)
            .await;
        for (action, fields) in [
            ("write", json!({"content": "replacement\n"})),
            (
                "edit",
                json!({"edits": [{"hash": line_hash("original"), "new": "replacement"}]}),
            ),
            ("move", json!({"to": "moved"})),
            ("delete", json!({})),
        ] {
            let mut args = fields;
            args["action"] = json!(action);
            args["path"] = json!("file");
            for name in ["fs".to_string(), format!("fs_{action}")] {
                let (result, status) = toolbox.run(&name, &args.to_string()).await;
                assert_eq!(
                    result,
                    "needs approval · not available here (headless); describe the change and let the user apply it"
                );
                assert_eq!(status, "blocked");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
                assert!(!root.join("moved").exists());
            }
        }
        for name in ["shell", "shell_run"] {
            let (result, status) = toolbox
                .run(name, r#"{"action":"run","cmd":"touch created"}"#)
                .await;
            assert!(result.starts_with("needs approval ·"), "{result}");
            assert_eq!(status, "blocked");
            assert!(!root.join("created").exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn workspace_writes_run_without_approval_by_default() {
        let (toolbox, dir) = fs_toolbox();
        assert!(!toolbox.require_approval);
        let (result, status) = toolbox
            .run(
                "fs",
                r#"{"action":"write","path":"new","content":"written"}"#,
            )
            .await;
        assert!(result.starts_with("wrote new"), "{result}");
        assert_ne!(status, "blocked");
        assert_eq!(
            std::fs::read_to_string(toolbox.workspace.as_ref().unwrap().root().join("new"))
                .unwrap(),
            "written"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn workspace_tools_are_conditional_and_reads_remain_available() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        std::fs::write(
            toolbox.workspace.as_ref().unwrap().root().join("file"),
            "readable\n",
        )
        .unwrap();
        for name in ["fs", "shell"] {
            assert!(toolbox.defs().iter().any(|tool| tool.name == name));
        }
        let (result, status) = toolbox
            .run("fs", r#"{"action":"read","path":"file"}"#)
            .await;
        assert!(result.contains("readable"), "{result}");
        assert_ne!(status, "blocked");
        toolbox.workspace = None;
        for name in ["fs", "shell"] {
            assert!(!toolbox.defs().iter().any(|tool| tool.name == name));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_shell_log_remains_readable_without_approval() {
        let (mut toolbox, dir) = fs_toolbox();
        toolbox.require_approval = true;
        let runs = dir.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join("0123456789ab.log"), "saved output\n").unwrap();
        toolbox.db_path = Some(dir.join("nexus.db"));
        let (result, status) = toolbox
            .run("shell", r#"{"action":"log","id":"0123456789ab"}"#)
            .await;
        assert!(result.contains("saved output"), "{result}");
        assert_ne!(status, "blocked");
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn run_fs(toolbox: &ToolBox, args: serde_json::Value) -> String {
        toolbox.run("fs", &args.to_string()).await.0
    }

    #[tokio::test]
    async fn fs_mutations_are_checkpointed_per_turn_and_undone() {
        let (mut toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root().to_path_buf();
        let workspace = Arc::new(
            Workspace::open(&root, Vec::new())
                .unwrap()
                .with_checkpoints(dir.join("checkpoints")),
        );
        toolbox.workspace = Some(Arc::clone(&workspace));
        for (name, content) in [
            ("edited", "one\ntwo\n"),
            ("deleted", "gone\n"),
            ("source", "moved\n"),
        ] {
            std::fs::write(root.join(name), content).unwrap();
            run_fs(&toolbox, json!({"action": "read", "path": name})).await;
        }
        workspace.begin_turn("s1");
        for (old, new) in [("two", "three"), ("three", "four")] {
            let result = run_fs(
                &toolbox,
                json!({"action": "edit", "path": "edited", "edits": [{"hash": line_hash(old), "new": new}]}),
            )
            .await;
            assert!(result.starts_with("edited edited"), "{result}");
        }
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "created", "content": "new\n"}),
        )
        .await;
        assert!(result.starts_with("wrote created"), "{result}");
        let result = run_fs(&toolbox, json!({"action": "delete", "path": "deleted"})).await;
        assert!(result.starts_with("deleted deleted"), "{result}");
        let result = run_fs(
            &toolbox,
            json!({"action": "move", "path": "source", "to": "target"}),
        )
        .await;
        assert!(result.starts_with("moved source -> target"), "{result}");
        assert_eq!(
            std::fs::read(dir.join("checkpoints/s1/1/0.pre")).unwrap(),
            b"one\ntwo\n"
        );

        let report = workspace.undo_last("s1").unwrap().unwrap();
        assert_eq!(report.restored, ["edited", "deleted", "source"]);
        assert_eq!(report.removed_created, ["created", "target"]);
        assert!(report.skipped_changed_by_user.is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("edited")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("deleted")).unwrap(),
            "gone\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("source")).unwrap(),
            "moved\n"
        );
        assert!(!root.join("created").exists());
        assert!(!root.join("target").exists());
        assert!(!dir.join("checkpoints/s1/1").exists());
        assert!(workspace.last_read(&root.join("edited")).is_none());
        let result = run_fs(
            &toolbox,
            json!({"action": "edit", "path": "edited", "edits": [{"hash": line_hash("two"), "new": "x"}]}),
        )
        .await;
        assert!(result.starts_with("fs.edit needs a fresh read"), "{result}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_write_creates_new_file_without_a_read() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("new.txt");
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "new.txt", "content": "one\ntwo\n"}),
        )
        .await;
        assert!(result.starts_with("wrote new.txt (+2 −0)\n"), "{result}");
        assert!(result.contains("\n+one\n+two"), "{result}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        assert_eq!(
            workspace.last_read(&path),
            Some(workspace.stamp(&path).unwrap())
        );
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "empty", "content": ""}),
        )
        .await;
        assert!(result.starts_with("wrote empty (+0 −0)"), "{result}");
        assert_eq!(std::fs::read(workspace.root().join("empty")).unwrap(), b"");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_write_creates_missing_parent_directories() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("new/dir/file.rs");
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "new/dir/file.rs", "content": "fn main() {}\n"}),
        )
        .await;
        assert!(
            result.starts_with("wrote new/dir/file.rs (+1 −0)\n"),
            "{result}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        assert_eq!(
            workspace.last_read(&path),
            Some(workspace.stamp(&path).unwrap())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_write_refuses_existing_file_without_a_read() {
        let (toolbox, dir) = fs_toolbox();
        let path = toolbox.workspace.as_ref().unwrap().root().join("file");
        std::fs::write(&path, "original\n").unwrap();
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "file", "content": "replacement\n"}),
        )
        .await;
        assert_eq!(
            result,
            "fs.write needs a fresh read · call fs with action=read on file first"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_edit_applies_hashline_edits_after_read_and_returns_diff() {
        let (toolbox, dir) = fs_toolbox();
        let path = toolbox.workspace.as_ref().unwrap().root().join("file");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let args = json!({
            "action": "edit", "path": "file",
            "edits": [{"hash": line_hash("two"), "new": "three"}]
        });
        let result = run_fs(&toolbox, args.clone()).await;
        assert!(result.starts_with("fs.edit needs a fresh read"), "{result}");
        run_fs(&toolbox, json!({"action": "read", "path": "file"})).await;
        let result = run_fs(&toolbox, args).await;
        assert!(result.starts_with("edited file (+1 −1)\n"), "{result}");
        assert!(result.contains("\n@@"), "{result}");
        assert!(result.contains("\n-two\n+three"), "{result}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nthree\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_edit_refuses_binary_and_non_utf8_without_corrupting_file() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("file");
        for content in [
            b"keep\nlatin-\xe9\n".as_slice(),
            b"keep\nnul\0\n".as_slice(),
            b"keep\nnul\0and\xff\n".as_slice(),
        ] {
            std::fs::write(&path, content).unwrap();
            run_fs(&toolbox, json!({"action": "read", "path": "file"})).await;
            let stamp = workspace.last_read(&path).unwrap();
            let result = run_fs(
                &toolbox,
                json!({"action": "edit", "path": "file", "edits": [{"hash": line_hash("keep"), "new": "changed"}]}),
            )
            .await;
            assert_eq!(
                result,
                "fs.edit needs a UTF-8 text file · file is binary or not UTF-8; use write to replace it whole"
            );
            assert_eq!(std::fs::read(&path).unwrap(), content);
            assert_eq!(workspace.last_read(&path), Some(stamp));
            let result = run_fs(
                &toolbox,
                json!({"action": "write", "path": "file", "content": "replacement\n"}),
            )
            .await;
            assert!(result.starts_with("wrote file"), "{result}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement\n");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_edit_refuses_after_external_change() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("file");
        std::fs::write(&path, "old\n").unwrap();
        run_fs(&toolbox, json!({"action": "read", "path": "file"})).await;
        let stamp = workspace.last_read(&path).unwrap();
        std::fs::write(&path, "new\n").unwrap();
        for action in ["edit", "write", "move", "delete"] {
            let result = run_fs(
                &toolbox,
                json!({
                    "action": action, "path": "file", "to": "moved", "content": "model\n",
                    "edits": [{"hash": line_hash("old"), "new": "model"}]
                }),
            )
            .await;
            assert!(result.contains("changed since your last read"), "{result}");
            assert!(result.contains("\n-old\n+new"), "{result}");
            assert_eq!(classify_tool_result(&result, ""), Some("stale_edit"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
            assert_eq!(workspace.last_read(&path), Some(stamp.clone()));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn consecutive_edits_need_no_reread() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        run_fs(
            &toolbox,
            json!({"action": "write", "path": "file", "content": "one\n"}),
        )
        .await;
        for (old, new) in [("one", "two"), ("two", "three")] {
            let result = run_fs(
                &toolbox,
                json!({"action": "edit", "path": "file", "edits": [{"hash": line_hash(old), "new": new}]}),
            )
            .await;
            assert!(result.starts_with("edited file (+1 −1)"), "{result}");
            let path = workspace.root().join("file");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{new}\n"));
            assert_eq!(
                workspace.last_read(&path),
                Some(workspace.stamp(&path).unwrap())
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_write_preserves_crlf_and_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let (toolbox, dir) = fs_toolbox();
        let path = toolbox.workspace.as_ref().unwrap().root().join("file");
        std::fs::write(&path, "one\r\ntwo\r\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
        run_fs(&toolbox, json!({"action": "read", "path": "file"})).await;
        for (content, expected) in [
            ("three\nfour\n", "three\r\nfour\r\n"),
            ("five\r\nsix", "five\r\nsix"),
            ("seven\neight", "seven\r\neight"),
        ] {
            let result = run_fs(
                &toolbox,
                json!({"action": "write", "path": "file", "content": content}),
            )
            .await;
            assert!(result.starts_with("wrote file"), "{result}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o751
            );
        }
        let result = run_fs(
            &toolbox,
            json!({"action": "edit", "path": "file", "edits": [{"hash": line_hash("eight"), "new": "nine"}]}),
        )
        .await;
        assert!(result.starts_with("edited file"), "{result}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "seven\r\nnine");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o751
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_move_and_delete_need_a_read_and_stay_inside() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("file");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        for action in ["move", "delete"] {
            let result = run_fs(
                &toolbox,
                json!({"action": action, "path": "file", "to": "moved"}),
            )
            .await;
            assert!(
                result.starts_with(&format!("fs.{action} needs a fresh read")),
                "{result}"
            );
            let result = run_fs(
                &toolbox,
                json!({"action": action, "path": "../escape", "to": "moved"}),
            )
            .await;
            assert!(result.starts_with("path escapes"), "{result}");
        }
        run_fs(&toolbox, json!({"action": "read", "path": "file"})).await;
        let result = run_fs(
            &toolbox,
            json!({"action": "move", "path": "file", "to": "../escape"}),
        )
        .await;
        assert!(result.starts_with("path escapes"), "{result}");
        let result = run_fs(
            &toolbox,
            json!({"action": "move", "path": "file", "to": "moved"}),
        )
        .await;
        assert_eq!(result, "moved file -> moved");
        assert!(!path.exists());
        let moved = workspace.root().join("moved");
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "one\ntwo\n");
        assert!(workspace.last_read(&path).is_none());
        assert_eq!(
            workspace.last_read(&moved),
            Some(workspace.stamp(&moved).unwrap())
        );
        let result = run_fs(&toolbox, json!({"action": "delete", "path": "moved"})).await;
        assert!(result.starts_with("deleted moved (+0 −2)\n"), "{result}");
        assert!(result.contains("\n-one\n-two"), "{result}");
        assert!(!moved.exists());
        assert!(workspace.last_read(&moved).is_none());
        std::fs::write(&moved, "external recreation").unwrap();
        let result = run_fs(&toolbox, json!({"action": "delete", "path": "moved"})).await;
        assert!(
            result.starts_with("fs.delete needs a fresh read"),
            "{result}"
        );
        std::fs::create_dir(workspace.root().join("directory")).unwrap();
        run_fs(&toolbox, json!({"action": "delete", "path": "directory"})).await;
        assert!(workspace.root().join("directory").is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_move_creates_missing_parent_directories() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let source = workspace.root().join("file.rs");
        let target = workspace.root().join("new/dir/file.rs");
        std::fs::write(&source, "fn main() {}\n").unwrap();
        run_fs(&toolbox, json!({"action": "read", "path": "file.rs"})).await;
        let result = run_fs(
            &toolbox,
            json!({"action": "move", "path": "file.rs", "to": "new/dir/file.rs"}),
        )
        .await;
        assert_eq!(result, "moved file.rs -> new/dir/file.rs");
        assert!(!source.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}\n");
        assert!(workspace.last_read(&source).is_none());
        assert_eq!(
            workspace.last_read(&target),
            Some(workspace.stamp(&target).unwrap())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_move_refuses_existing_target() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        std::fs::write(root.join("from"), "source").unwrap();
        std::fs::write(root.join("to"), "target").unwrap();
        run_fs(&toolbox, json!({"action": "read", "path": "from"})).await;
        let result = run_fs(
            &toolbox,
            json!({"action": "move", "path": "from", "to": "to"}),
        )
        .await;
        assert!(result.starts_with("fs.move target exists:"), "{result}");
        assert_eq!(
            std::fs::read_to_string(root.join("from")).unwrap(),
            "source"
        );
        assert_eq!(std::fs::read_to_string(root.join("to")).unwrap(), "target");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_write_refuses_secrets_files_and_git_internals() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        for path in [
            ".env",
            ".env.local",
            "server.pem",
            "private.key",
            "id_rsa",
            "id_ed25519",
            ".git/config",
            "sub/.git/config",
        ] {
            let result = run_fs(
                &toolbox,
                json!({"action": "write", "path": path, "content": "secret"}),
            )
            .await;
            assert!(result.starts_with("path denied:"), "{result}");
            assert!(!root.join(path).exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_write_updates_resolved_symlinks_atomically_and_refuses_escapes() {
        use std::os::unix::fs::symlink;

        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        std::fs::write(root.join("file"), "before\n").unwrap();
        symlink(root.join("file"), root.join("alias")).unwrap();
        std::fs::hard_link(root.join("file"), root.join("original-inode")).unwrap();
        run_fs(&toolbox, json!({"action": "read", "path": "alias"})).await;
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "alias", "content": "after\n"}),
        )
        .await;
        assert!(result.starts_with("wrote file"), "{result}");
        assert!(
            std::fs::symlink_metadata(root.join("alias"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("alias")).unwrap(),
            "after\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("original-inode")).unwrap(),
            "before\n"
        );
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 3);
        symlink(&dir, root.join("outside")).unwrap();
        std::fs::write(root.join(".env"), "secret").unwrap();
        symlink(root.join(".env"), root.join("secret-alias")).unwrap();
        for (path, prefix) in [
            ("outside/new", "path escapes"),
            ("secret-alias", "path denied"),
        ] {
            let result = run_fs(
                &toolbox,
                json!({"action": "write", "path": path, "content": "unsafe"}),
            )
            .await;
            assert!(result.starts_with(prefix), "{result}");
        }
        assert!(!dir.join("new").exists());
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "secret"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_write_ignores_mtime_only_changes() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("file");
        std::fs::write(&path, "old\n").unwrap();
        run_fs(&toolbox, json!({"action": "read", "path": "file"})).await;
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        let changed = workspace.last_read(&path).unwrap().mtime + Duration::from_secs(60);
        file.set_times(std::fs::FileTimes::new().set_modified(changed))
            .unwrap();
        assert_ne!(
            workspace.last_read(&path).unwrap().mtime,
            workspace.stamp(&path).unwrap().mtime
        );
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "file", "content": "new\n"}),
        )
        .await;
        assert!(result.starts_with("wrote file"), "{result}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_mutation_and_stale_diffs_are_capped() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        let before = (0..250).map(|n| format!("old {n}\n")).collect::<String>();
        let after = (0..250).map(|n| format!("new {n}\n")).collect::<String>();
        run_fs(
            &toolbox,
            json!({"action": "write", "path": "file", "content": before}),
        )
        .await;
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "file", "content": after}),
        )
        .await;
        assert!(result.starts_with("wrote file (+250 −250)"), "{result}");
        assert_eq!(result.lines().count(), 202);
        assert!(result.lines().last().unwrap().ends_with("more lines"));
        std::fs::write(root.join("file"), before).unwrap();
        let result = run_fs(
            &toolbox,
            json!({"action": "write", "path": "file", "content": "refused"}),
        )
        .await;
        assert!(result.contains("changed since your last read"), "{result}");
        assert_eq!(result.lines().count(), 62);
        assert!(result.lines().last().unwrap().ends_with("more lines"));
        assert_eq!(fs_diff("file", "--old\n", "++new\n", 200).1, 1);
        assert_eq!(fs_diff("file", "--old\n", "++new\n", 200).2, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn classify_fs_write_refusals_as_stale_edits() {
        for result in [
            "fs.write needs a fresh read · call fs with action=read on file first",
            "file changed since your last read · re-read it and re-apply\n-old\n+new",
        ] {
            assert_eq!(
                classify_tool_result(result, "Running fs_write…"),
                Some("stale_edit")
            );
        }
        assert_eq!(
            classify_tool_result("fs.move target exists: file", ""),
            None
        );
    }

    #[test]
    fn fs_definitions_require_a_workspace() {
        let (mut toolbox, dir) = skills_toolbox();
        for defs in [toolbox.defs(), toolbox.defs_compact()] {
            assert!(!defs.iter().any(|def| def.name == "fs"));
        }
        toolbox.workspace = Some(Arc::new(Workspace::open(&dir, Vec::new()).unwrap()));
        for defs in [toolbox.defs(), toolbox.defs_compact()] {
            let index = defs.iter().position(|def| def.name == "fetch_url").unwrap();
            assert_eq!(defs[index + 1].name, "fs");
            assert_eq!(
                defs[index + 1].parameters["properties"]["action"]["enum"],
                json!([
                    "list", "glob", "grep", "read", "edit", "write", "move", "delete"
                ])
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_actions_require_a_workspace_and_refuse_escaping_paths() {
        let (mut toolbox, dir) = skills_toolbox();
        for action in ["list", "glob", "grep", "read"] {
            let args = json!({ "action": action, "path": "../escape", "pattern": "x" }).to_string();
            let (result, _) = toolbox.run("fs", &args).await;
            assert_eq!(
                result,
                "fs needs an agent workspace · switch with /agent <path>"
            );
            let (result, _) = toolbox.run(&format!("fs_{action}"), &args).await;
            assert_eq!(
                result,
                "fs needs an agent workspace · switch with /agent <path>"
            );
        }
        toolbox.workspace = Some(Arc::new(Workspace::open(&dir, Vec::new()).unwrap()));
        let workspace = toolbox.workspace.as_ref().unwrap();
        let outside = dir.parent().unwrap().join("outside");
        for action in ["list", "glob", "grep", "read"] {
            for path in [
                "../escape",
                outside.to_str().unwrap(),
                ".git/config",
                "sub/.git/config",
            ] {
                let args = json!({ "action": action, "path": path, "pattern": "x" }).to_string();
                let (result, _) = toolbox.run("fs", &args).await;
                assert_eq!(
                    result,
                    workspace
                        .resolve(path, Access::Read)
                        .unwrap_err()
                        .to_string()
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_read_pages_hash_lines_and_records_stamps() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("notes.txt");
        let content = "one\ntwo\nthree\nfour\n";
        std::fs::write(&path, content).unwrap();
        let (result, _) = toolbox
            .run(
                "fs",
                r#"{"action":"read","path":"notes.txt","offset":2,"limit":2}"#,
            )
            .await;
        assert_eq!(
            result,
            format!(
                "notes.txt (lines 2-3 of 4):\n{}\n… continue with offset=4",
                number_lines_with_hash(&["two", "three"], 1)
            )
        );
        assert_eq!(
            workspace.last_read(&path),
            Some(workspace.stamp(&path).unwrap())
        );
        let stamp = workspace.last_read(&path).unwrap();
        assert_eq!(stamp.len, content.len() as u64);
        assert_eq!(stamp.sha256.as_slice(), Sha256::digest(content).as_slice());
        let (result, _) = toolbox
            .run("fs_read", r#"{"path":"notes.txt","offset":4}"#)
            .await;
        assert_eq!(
            result,
            format!(
                "notes.txt (lines 4-4 of 4):\n{}",
                number_lines_with_hash(&["four"], 3)
            )
        );
        std::fs::write(&path, "changed\n").unwrap();
        assert_ne!(
            workspace.last_read(&path),
            Some(workspace.stamp(&path).unwrap())
        );
        toolbox.run("fs_read", r#"{"path":"notes.txt"}"#).await;
        assert_eq!(
            workspace.last_read(&path),
            Some(workspace.stamp(&path).unwrap())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_read_handles_binary_empty_and_line_limits() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("file");
        std::fs::write(&path, b"a\0b").unwrap();
        let (result, _) = toolbox.run("fs_read", r#"{"path":"file"}"#).await;
        assert_eq!(result, "binary file (3 bytes) · not shown");
        assert_eq!(
            workspace.last_read(&path),
            Some(workspace.stamp(&path).unwrap())
        );
        std::fs::write(&path, "").unwrap();
        let (result, _) = toolbox.run("fs_read", r#"{"path":"file"}"#).await;
        assert_eq!(result, "file (lines 0-0 of 0):");
        std::fs::write(&path, "x\n".repeat(2005)).unwrap();
        let result = fs_action(workspace, "fs_read", r#"{"path":"file","limit":9999}"#).unwrap();
        assert!(result.starts_with("file (lines 1-2000 of 2005):"));
        assert!(result.ends_with("… continue with offset=2001"));
        let (result, _) = toolbox
            .run("fs_read", r#"{"path":"file","offset":9999}"#)
            .await;
        assert_eq!(result, "file (lines 0-0 of 2005):");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_read_refuses_files_over_sixteen_mib_without_recording_a_stamp() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let path = workspace.root().join("large");
        let file = std::fs::File::create(&path).unwrap();
        let limit = 16 * 1024 * 1024;
        file.set_len(limit + 1).unwrap();
        let expected = format!(
            "file too large ({} bytes) · use fs grep or shell (sed -n, head) for a slice",
            limit + 1
        );
        let (result, _) = toolbox.run("fs_read", r#"{"path":"large"}"#).await;
        assert_eq!(result, expected);
        assert!(workspace.last_read(&path).is_none());
        file.set_len(limit).unwrap();
        let (result, _) = toolbox.run("fs_read", r#"{"path":"large"}"#).await;
        assert_eq!(result, format!("binary file ({limit} bytes) · not shown"));
        let stamp = workspace.last_read(&path).unwrap();
        assert_eq!(stamp.len, limit);
        file.set_len(limit + 1).unwrap();
        let (result, _) = toolbox.run("fs_read", r#"{"path":"large"}"#).await;
        assert_eq!(result, expected);
        assert_eq!(workspace.last_read(&path), Some(stamp));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_list_respects_gitignore_hidden_files_and_depth() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        std::fs::create_dir_all(root.join("src/nested/deep")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), "hidden").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(root.join("ignored"), "hidden").unwrap();
        std::fs::write(root.join(".visible"), "shown").unwrap();
        std::fs::write(root.join("src/code.rs"), "code").unwrap();
        let (result, _) = toolbox.run("fs_list", "{}").await;
        assert_eq!(
            result,
            "./\n.gitignore\n.visible\nsrc/\nsrc/code.rs\nsrc/nested/"
        );
        let (result, _) = toolbox.run("fs_list", r#"{"depth":1}"#).await;
        assert!(!result.contains("src/code.rs"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_list_summarizes_large_directories() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        std::fs::create_dir_all(root.join("many")).unwrap();
        for index in 0..101 {
            std::fs::write(root.join(format!("many/{index}")), "").unwrap();
        }
        let (result, _) = toolbox.run("fs_list", "{}").await;
        assert_eq!(result, "./\nmany/ (101 entries)");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_glob_matches_sorted_relative_paths_and_caps_entries() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        for path in [
            "src/z.rs",
            "src/nested/a.rs",
            "src/nested/ignored.rs",
            "src/plain.txt",
            "other.rs",
        ] {
            std::fs::write(root.join(path), "").unwrap();
        }
        let (result, _) = toolbox
            .run("fs", r#"{"action":"glob","pattern":"src/**/*.rs"}"#)
            .await;
        assert_eq!(result, "src/nested/a.rs\nsrc/z.rs");
        for index in 0..405 {
            std::fs::write(root.join(format!("src/{index:03}.rs")), "").unwrap();
        }
        let (result, _) = toolbox.run("fs_glob", r#"{"pattern":"src/**/*.rs"}"#).await;
        assert_eq!(result.lines().count(), 401);
        assert!(result.ends_with("… 7 more"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fs_grep_builtin_groups_literal_case_sensitive_matches_and_context() {
        let (toolbox, dir) = fs_toolbox();
        let workspace = toolbox.workspace.as_ref().unwrap();
        let root = workspace.root();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("a.txt"), "before\na.b\na.b again\naXb\nA.B\n").unwrap();
        std::fs::write(root.join("b.txt"), "a.b\n").unwrap();
        std::fs::write(root.join("binary"), b"\0a.b\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "a.b\n").unwrap();
        let result = fs_grep(workspace, root, "a.b", 0).unwrap();
        assert_eq!(result, "a.txt:\n  2: a.b\n  3: a.b again\nb.txt:\n  1: a.b");
        let result = fs_grep(workspace, &root.join("a.txt"), "a.b", 1).unwrap();
        assert_eq!(
            result,
            "a.txt:\n  1: before\n  2: a.b\n  3: a.b again\n  4: aXb"
        );
        std::fs::write(root.join("a.txt"), "a.b\n".repeat(202)).unwrap();
        let result = fs_grep(workspace, root, "a.b", 0).unwrap();
        assert_eq!(
            result.lines().filter(|line| line.starts_with("  ")).count(),
            200
        );
        assert!(result.ends_with("… 3 more"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn fs_grep_skips_files_over_four_mib_and_reports_the_count() {
        let (toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root();
        let mut boundary = vec![b'x'; 4 * 1024 * 1024];
        boundary[..7].copy_from_slice(b"needle\n");
        std::fs::write(root.join("boundary.txt"), boundary).unwrap();
        for name in ["large.txt", "large.bin"] {
            std::fs::File::create(root.join(name))
                .unwrap()
                .set_len(4 * 1024 * 1024 + 1)
                .unwrap();
        }
        let (result, _) = toolbox.run("fs_grep", r#"{"pattern":"needle"}"#).await;
        assert_eq!(
            result,
            "boundary.txt:\n  1: needle\n… skipped 2 files over 4 MiB · use shell for those"
        );
        let (result, _) = toolbox.run("fs_grep", r#"{"pattern":"missing"}"#).await;
        assert_eq!(
            result,
            "no matches\n… skipped 2 files over 4 MiB · use shell for those"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_walks_never_expose_symlink_escapes_or_denied_directories() {
        let (mut toolbox, dir) = fs_toolbox();
        let root = toolbox.workspace.as_ref().unwrap().root().to_path_buf();
        let denied = root.join("nexus");
        std::fs::create_dir_all(&denied).unwrap();
        std::fs::write(denied.join("secret.txt"), "needle").unwrap();
        std::fs::write(dir.join("outside.txt"), "needle").unwrap();
        std::os::unix::fs::symlink(dir.join("outside.txt"), root.join("link.txt")).unwrap();
        toolbox.workspace = Some(Arc::new(Workspace::open(&root, vec![denied]).unwrap()));
        for action in ["list", "glob", "grep", "read"] {
            let (result, _) = toolbox
                .run(
                    "fs",
                    &json!({"action": action, "path": "link.txt", "pattern": "needle"}).to_string(),
                )
                .await;
            assert!(
                result.starts_with("path escapes the workspace:"),
                "{result}"
            );
        }
        for (action, pattern) in [("list", "*"), ("glob", "**/*.txt")] {
            let (result, _) = toolbox
                .run(
                    "fs",
                    &json!({"action": action, "pattern": pattern}).to_string(),
                )
                .await;
            assert!(!result.contains("nexus"), "{result}");
            assert!(!result.contains("link.txt"), "{result}");
        }
        let workspace = toolbox.workspace.as_ref().unwrap();
        assert_eq!(
            fs_grep(workspace, &root, "needle", 0).unwrap(),
            "no matches"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn propose_command_refuses_multiline_and_control_characters() {
        let (mut toolbox, dir) = skills_toolbox();
        toolbox.interactive_ui = true;
        for cmd in ["make\nrm -rf x", "ls\u{1b}[2K"] {
            let args = json!({ "cmd": cmd, "reason": "r" }).to_string();
            let (result, _) = toolbox.run("propose_command", &args).await;
            assert!(result.starts_with("invalid tool arguments"), "{result}");
        }
        let (result, _) = toolbox
            .run("propose_command", r#"{"cmd":"make install","reason":"r"}"#)
            .await;
        assert!(result.starts_with("proposed to the user: !make install"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn head_tail_cap_keeps_both_ends_and_counts_omitted_characters() {
        let text = format!("{}{}{}", "H".repeat(300), "M".repeat(400), "T".repeat(300));
        let capped = cap_head_tail(&text, 200);
        assert!(capped.starts_with(&"H".repeat(110)));
        assert!(capped.ends_with(&"T".repeat(45)));
        assert!(capped.chars().count() <= 200);
        let (head, rest) = capped.split_once("\n… [").unwrap();
        let (omitted, tail) = rest.split_once(" characters omitted] …\n").unwrap();
        let omitted: usize = omitted.parse().unwrap();
        assert_eq!(head.chars().count() + tail.chars().count() + omitted, 1_000);
    }

    #[test]
    fn head_tail_cap_is_unicode_safe_and_identity_when_it_fits() {
        let text = format!("{}{}", "界".repeat(300), "🦀".repeat(300));
        let capped = cap_head_tail(&text, 200);
        assert!(capped.starts_with(&"界".repeat(110)));
        assert!(capped.ends_with(&"🦀".repeat(45)));
        assert!(capped.chars().count() <= 200);
        assert_eq!(cap_head_tail(&text, 600), text);
        assert_eq!(cap_head_tail(&text, 700), text);
        assert_eq!(cap_head_tail("", 0), "");
        for limit in 0..200 {
            assert!(cap_head_tail(&text, limit).chars().count() <= limit);
        }
    }

    #[test]
    fn compact_batch_and_app_keep_schemas_and_first_sentences() {
        let dir = std::env::temp_dir().join(format!("nexus-compact-{}", uuid::Uuid::new_v4()));
        let mut toolbox = ToolBox::new(
            dir.join("skills"),
            None,
            None,
            "auto".into(),
            Vec::new(),
            None,
            None,
            None,
        );
        toolbox.apps = Some(AppsCtx {
            dir: dir.join("apps"),
            registry: crate::appserver::AppRegistry::load(&dir),
            space_name: "test".into(),
            server_port: 8642,
            public_base: None,
            space_id: "test".into(),
            space_db_path: dir.join("test.db"),
            files_dir: dir.join("files"),
            session_id: String::new(),
        });
        let full = toolbox.defs();
        let compact = toolbox.defs_compact();
        assert_eq!(full.len(), compact.len());
        for (full, compact) in full.iter().zip(&compact) {
            assert_eq!(full.name, compact.name);
            assert_eq!(full.parameters["required"], compact.parameters["required"]);
        }
        let batch = compact.iter().find(|tool| tool.name == "batch").unwrap();
        assert_eq!(
            batch.description,
            "Run several independent tool operations in ONE call — multiple searches, multiple file searches/reads, or multiple app/script writes. "
        );
        assert_eq!(
            batch.parameters["properties"]["calls"]["items"]["required"],
            serde_json::json!(["tool"])
        );
        assert!(
            batch.parameters["properties"]["calls"]
                .get("description")
                .is_none()
        );
        assert!(
            batch.parameters["properties"]["calls"]["items"]["properties"]["arguments"]
                .get("description")
                .is_none()
        );
        let app = compact.iter().find(|tool| tool.name == "app").unwrap();
        let full_app = full.iter().find(|tool| tool.name == "app").unwrap();
        assert_eq!(
            app.description,
            "Build and manage locally served web apps. "
        );
        assert_eq!(
            app.parameters["properties"]["action"],
            full_app.parameters["properties"]["action"]
        );
        assert!(
            app.parameters["properties"]["edits"]["items"]["properties"]["hash"]
                .get("description")
                .is_none()
        );
    }

    #[test]
    fn classify_tool_result_kinds() {
        for (result, status, expected) in [
            ("anything", "invalid arguments", Some("invalid_args")),
            ("missing required field: name", "", Some("invalid_args")),
            ("invalid action for scripts: x", "", Some("invalid_args")),
            ("invalid mode for search: x", "", Some("invalid_args")),
            (
                "invalid scope for research_lookup: x",
                "",
                Some("invalid_args"),
            ),
            ("invalid tool arguments: bad JSON", "", Some("invalid_args")),
            ("invalid path: ../x", "", Some("invalid_args")),
            ("anything", "blocked", Some("blocked")),
            (
                "missing required field: name",
                "blocked",
                Some("invalid_args"),
            ),
            ("unknown tool: x", "", Some("unknown_tool")),
            ("hash abc not found", "", Some("stale_edit")),
            ("hash abc matches multiple lines", "", Some("stale_edit")),
            (
                "hash abc targets a line already edited",
                "",
                Some("stale_edit"),
            ),
            (
                "path escapes the scripts directory",
                "",
                Some("confinement"),
            ),
            ("unknown file: x", "", Some("not_found")),
            ("unknown skill: x", "", Some("not_found")),
            ("no such script: x", "", Some("not_found")),
            ("unknown app x", "", Some("not_found")),
            ("cannot read x", "", Some("not_found")),
            ("  write failed: denied  ", "", Some("io")),
            ("db error: locked", "", Some("io")),
            ("store error: locked", "", Some("io")),
            ("search failed: offline", "", Some("upstream")),
            ("pip install failed: offline", "", Some("upstream")),
            ("video generation failed: offline", "", Some("upstream")),
            ("venv creation failed: offline", "", Some("upstream")),
            ("file search failed: offline", "", Some("upstream")),
            ("citation lookup failed: offline", "", Some("upstream")),
            ("normal result", "", None),
            ("file.txt:\nwrite failed: this is file content", "", None),
            ("file.txt: write failed: this is file content", "", None),
            ("hash abc\nnot found", "", None),
            ("Search failed: uppercase", "", None),
            ("api2 failed: digit", "", None),
            ("", "", None),
        ] {
            assert_eq!(
                classify_tool_result(result, status),
                expected,
                "result={result:?}, status={status:?}"
            );
        }
    }

    fn tool_events_toolbox() -> (ToolBox, crate::db::Db) {
        let dir = std::env::temp_dir().join(format!("nexus-tool-events-{}", uuid::Uuid::new_v4()));
        let scripts = dir.join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let path = dir.join("nexus.db");
        let db = crate::db::Db::open(&path).unwrap();
        let mut tb = ToolBox::new(
            dir.join("skills"),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(path),
            None,
            None,
        );
        tb.space_scripts_dir = scripts;
        tb.session_id = "s1".to_string();
        (tb, db)
    }

    #[tokio::test]
    async fn tool_calls_are_recorded_with_outcome() {
        let (mut tb, db) = tool_events_toolbox();
        tb.record_events = true;
        let (success, _) = tb.run("scripts", r#"{"action":"list"}"#).await;
        let (failure, _) = tb.run("scripts", r#"{"action":"python","code":"x"}"#).await;
        assert_eq!(success, "[]");
        assert!(
            failure.starts_with("missing required field: name"),
            "{failure}"
        );

        let mut stmt = db
            .raw()
            .prepare(
                "SELECT at, session_id, tool, action, ok, error_kind, latency_ms, result_chars
                 FROM cache.tool_events ORDER BY id",
            )
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        for (action, ok, error_kind, result) in [
            ("list", true, None, success),
            ("python", false, Some("invalid_args"), failure),
        ] {
            let row = rows.next().unwrap().unwrap();
            let at =
                chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(0).unwrap()).unwrap();
            assert_eq!(at.offset().local_minus_utc(), 0);
            assert_eq!(row.get::<_, String>(1).unwrap(), "s1");
            assert_eq!(row.get::<_, String>(2).unwrap(), "scripts");
            assert_eq!(row.get::<_, String>(3).unwrap(), action);
            assert_eq!(row.get::<_, bool>(4).unwrap(), ok);
            assert_eq!(
                row.get::<_, Option<String>>(5).unwrap().as_deref(),
                error_kind
            );
            assert!(row.get::<_, i64>(6).unwrap() >= 0);
            assert_eq!(
                row.get::<_, i64>(7).unwrap(),
                i64::try_from(result.chars().count()).unwrap()
            );
        }
        assert!(rows.next().unwrap().is_none());
    }

    #[tokio::test]
    async fn no_events_without_opt_in() {
        let (tb, db) = tool_events_toolbox();
        assert!(!tb.record_events);
        tb.run("scripts", r#"{"action":"list"}"#).await;
        tb.run("scripts", r#"{"action":"python","code":"x"}"#).await;
        let count: i64 = db
            .raw()
            .query_row("SELECT COUNT(*) FROM cache.tool_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn batch_records_each_sub_call() {
        let (mut tb, db) = tool_events_toolbox();
        tb.record_events = true;
        tb.run(
            "batch",
            r#"{"calls":[
                {"tool":"scripts","arguments":{"action":"list"}},
                {"tool":"scripts","arguments":{"action":"list"}}
            ]}"#,
        )
        .await;
        let mut stmt = db
            .raw()
            .prepare("SELECT tool, action, ok FROM cache.tool_events ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("scripts".to_string(), "list".to_string(), true),
                ("scripts".to_string(), "list".to_string(), true),
                ("batch".to_string(), String::new(), true),
            ]
        );
    }

    #[tokio::test]
    async fn run_cmd_env_never_passes_secrets() {
        let out = run_cmd_env(
            "env".as_ref(),
            &[],
            &std::env::temp_dir(),
            10,
            &[("OPENROUTER_API_KEY", "leak"), ("NEXUS_KEEP", "1")],
        )
        .await
        .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("NEXUS_KEEP=1"));
        assert!(!stdout.contains("OPENROUTER_API_KEY"));
    }

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
        let resp: SearxngResponse = serde_json::from_str(r"{}").unwrap();
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
        let hash = line_hash("echo hi");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn write_script_refuses_symlinked_dir_escape() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("scripts");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, scripts.join("out")).unwrap();
        tb.space_scripts_dir = scripts;

        let (result, _) = tb
            .run(
                "write_script",
                r#"{"path":"out/x.py","content":"print(1)"}"#,
            )
            .await;
        assert!(result.contains("escapes"), "{result}");
        assert!(!outside.join("x.py").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_script_refuses_dangling_symlink_escape() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("scripts");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(outside.join("new.py"), scripts.join("new.py")).unwrap();
        tb.space_scripts_dir = scripts;
        assert!(!outside.join("new.py").exists());

        let (result, _) = tb
            .run("write_script", r#"{"path":"new.py","content":"x"}"#)
            .await;
        assert!(result.contains("escapes"), "{result}");
        assert!(!outside.join("new.py").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_script_refuses_symlinked_file_escape() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("scripts");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOPSECRET").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), scripts.join("secret.py")).unwrap();
        tb.space_scripts_dir = scripts;

        let (result, _) = tb.run("read_script", r#"{"path":"secret.py"}"#).await;
        assert!(result.contains("escapes"), "{result}");
        assert!(!result.contains("TOPSECRET"), "{result}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_script_refuses_symlinked_file_escape() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("scripts");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOPSECRET").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), scripts.join("secret.py")).unwrap();
        tb.space_scripts_dir = scripts;

        let hash = line_hash("TOPSECRET");
        let (result, _) = tb
            .run(
                "edit_script",
                &format!(r#"{{"path":"secret.py","edits":[{{"hash":"{hash}","new":"changed"}}]}}"#),
            )
            .await;
        assert!(result.contains("escapes"), "{result}");
        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "TOPSECRET"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_script_allows_new_nested_path_and_inner_symlink() {
        let (mut tb, dir) = skills_toolbox();
        let scripts = dir.join("scripts");
        tb.space_scripts_dir = scripts.clone();
        assert!(!scripts.exists());

        let (result, _) = tb
            .run(
                "write_script",
                r#"{"path":"a/b/c.py","content":"print(1)"}"#,
            )
            .await;
        assert!(result.contains("wrote a/b/c.py"), "{result}");
        assert_eq!(
            std::fs::read_to_string(scripts.join("a/b/c.py")).unwrap(),
            "print(1)"
        );

        std::fs::create_dir_all(scripts.join("real")).unwrap();
        std::os::unix::fs::symlink(scripts.join("real"), scripts.join("alias")).unwrap();
        let (result, _) = tb
            .run(
                "write_script",
                r#"{"path":"alias/y.py","content":"print(2)"}"#,
            )
            .await;
        assert!(result.contains("wrote alias/y.py"), "{result}");
        assert_eq!(
            std::fs::read_to_string(scripts.join("real/y.py")).unwrap(),
            "print(2)"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
        let h1 = line_hash("alpha");
        let h2 = line_hash("beta");
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

    #[test]
    fn hashline_edits_survive_line_shifts_and_support_duplicate_hints() {
        let hash = line_hash("needle");
        let edits = vec![(hash, None, Some("changed".to_string()))];
        let (out, _) = apply_hashline_edits("inserted\nneedle", &edits).unwrap();
        assert_eq!(out, "inserted\nchanged");

        let hash = line_hash("same");
        let edits = vec![(hash, Some(3), Some("third".to_string()))];
        let (out, _) = apply_hashline_edits("same\nother\nsame", &edits).unwrap();
        assert_eq!(out, "same\nother\nthird");
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
    fn new_wires_searxng_url_and_langsearch_key() {
        let tb = ToolBox::new(
            PathBuf::new(),
            Some("http://localhost:8080".to_string()),
            Some("key-1".to_string()),
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        assert_eq!(tb.searxng_url.as_deref(), Some("http://localhost:8080"));
        assert_eq!(tb.langsearch_key.as_deref(), Some("key-1"));
        // The seam exposes the same wiring contract the App relies on.
        assert!(!tb.supports_images());
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
    // One row per tool — the classification table is the point.
    #[allow(clippy::too_many_lines)]
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
        assert!(status.contains('3'), "status was {status:?}");
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
        assert!(status.contains('2'), "status was {status:?}");
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
                public_base: None,
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
        {
            let t = "app";
            assert!(names.contains(&t.to_string()), "missing {t}");
        }
        // The app tool's action surface includes init/build.
        let defs = tb.defs();
        let app_def = defs.iter().find(|d| d.name == "app").unwrap();
        let actions = app_def
            .parameters
            .get("properties")
            .and_then(|p| p.get("action"))
            .and_then(|a| a.get("enum"))
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let actions: Vec<&str> = actions.iter().filter_map(|a| a.as_str()).collect();
        for a in ["init", "build"] {
            assert!(actions.contains(&a), "missing {a}");
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
    async fn init_scaffolds_astro_and_vite_templates() {
        let (tb, dir) = apps_toolbox();
        let (result, _) = tb
            .run("init_app", r#"{"app":"site","framework":"astro"}"#)
            .await;
        assert!(
            result.contains("scaffolded site with the astro starter"),
            "{result}"
        );
        assert!(result.contains("app action=build"), "{result}");
        assert!(
            result.contains("live at http://127.0.0.1:9999/"),
            "{result}"
        );
        assert!(dir.join("site/package.json").is_file());
        assert!(dir.join("site/src/pages/index.astro").is_file());
        assert!(dir.join("site/src/components/Counter.tsx").is_file());
        // Derived dirs are gitignored so the editing tools leave them alone.
        assert!(dir.join("site/.gitignore").is_file());
        let pkg = std::fs::read_to_string(dir.join("site/package.json")).unwrap();
        assert!(pkg.contains("astro build"), "{pkg}");

        let (result, _) = tb
            .run("init_app", r#"{"app":"spa","framework":"vite-react"}"#)
            .await;
        assert!(result.contains("vite-react"), "{result}");
        assert!(dir.join("spa/src/App.tsx").is_file());
        assert!(dir.join("spa/vite.config.js").is_file());

        // Unknown framework rejected; existing apps are never clobbered.
        let (result, _) = tb
            .run("init_app", r#"{"app":"x","framework":"svelte"}"#)
            .await;
        assert!(result.contains("unknown framework"), "{result}");
        let (result, _) = tb
            .run("init_app", r#"{"app":"site","framework":"astro"}"#)
            .await;
        assert!(result.contains("already exists"), "{result}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn build_app_guides_instead_of_failing_obscurely() {
        let (tb, dir) = apps_toolbox();
        // No package.json → point the model at init.
        let (result, _) = tb.run("build_app", r#"{"app":"bare"}"#).await;
        assert!(result.contains("no package.json"), "{result}");

        // Plain package.json without a bundler → served as-is.
        std::fs::create_dir_all(dir.join("plain")).unwrap();
        std::fs::write(
            dir.join("plain/package.json"),
            r#"{"name":"plain","private":true}"#,
        )
        .unwrap();
        let (result, _) = tb.run("build_app", r#"{"app":"plain"}"#).await;
        assert!(result.contains("no build step"), "{result}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The build tool shells out to `npx`; false on machines without node.
    async fn npx_available() -> bool {
        tokio::process::Command::new("npx")
            .arg("--version")
            .output()
            .await
            .is_ok_and(|o| o.status.success())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn build_runs_the_declared_tool_and_marks_dist_served() {
        use std::os::unix::fs::PermissionsExt;
        if !npx_available().await {
            return; // no node/npm on this machine — skip
        }
        let (tb, dir) = apps_toolbox();
        let app = dir.join("built");
        std::fs::create_dir_all(app.join("node_modules/.bin")).unwrap();
        std::fs::write(
            app.join("package.json"),
            r#"{"name":"built","dependencies":{"astro":"^5.0.0"}}"#,
        )
        .unwrap();
        // A fake `astro` bin: builds dist/ without node or the network.
        std::fs::write(
            app.join("node_modules/.bin/astro"),
            "#!/bin/sh\nmkdir -p dist\necho '<h1>ok</h1>' > dist/index.html\necho \"$*\" > build-args.txt\necho 'built ok'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            app.join("node_modules/.bin/astro"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let (result, _) = tb.run("build_app", r#"{"app":"built"}"#).await;
        assert!(result.contains("build ok"), "{result}");
        assert!(
            result.contains("live at http://127.0.0.1:9999/"),
            "{result}"
        );
        assert!(app.join("dist/index.html").is_file());

        // The build ran with the app's absolute base so asset links resolve.
        let ctx = tb.apps.as_ref().unwrap();
        let uuid = ctx.registry.resolve("default", "built").unwrap();
        let args = std::fs::read_to_string(app.join("build-args.txt")).unwrap();
        assert!(args.contains(&format!("--base /{uuid}/")), "{args}");

        // The registry now serves the app from dist/.
        assert_eq!(
            ctx.registry.lookup(&uuid).unwrap().served_from.as_deref(),
            Some("dist")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn build_without_dist_output_never_marks_dist_served() {
        use std::os::unix::fs::PermissionsExt;
        if !npx_available().await {
            return; // no node/npm on this machine — skip
        }
        let (tb, dir) = apps_toolbox();
        let app = dir.join("misbuilt");
        std::fs::create_dir_all(app.join("node_modules/.bin")).unwrap();
        std::fs::write(
            app.join("package.json"),
            r#"{"name":"misbuilt","dependencies":{"astro":"^5.0.0"}}"#,
        )
        .unwrap();
        // A fake `astro` bin that exits 0 but never writes dist/ (as if the
        // framework's outDir were configured elsewhere).
        std::fs::write(
            app.join("node_modules/.bin/astro"),
            "#!/bin/sh\necho 'build ok'\n",
        )
        .unwrap();
        std::fs::set_permissions(
            app.join("node_modules/.bin/astro"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let (result, _) = tb.run("build_app", r#"{"app":"misbuilt"}"#).await;
        assert!(result.contains("produced no dist/"), "{result}");
        // The app is not marked served-from-dist, so nothing 404s.
        let ctx = tb.apps.as_ref().unwrap();
        let uuid = ctx.registry.resolve("default", "misbuilt").unwrap();
        assert_eq!(ctx.registry.lookup(&uuid).unwrap().served_from, None);
        let _ = std::fs::remove_dir_all(&dir);
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

        let h = line_hash("<h1>Hello</h1>");
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
        let hash = line_hash("<h1>Hello</h1>");
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
        let h = line_hash("x y x");
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
        let h = line_hash("two");
        // Delete "two" and insert an extra line after "one" in the same call.
        let args = format!(
            r#"{{"app":"a","path":"f.txt","edits":[{{"hash":"{}","new":"one\ninserted"}},{{"hash":"{h}"}}]}}"#,
            line_hash("one"),
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

    fn bare_toolbox() -> ToolBox {
        ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        )
    }

    fn tool_names(toolbox: &ToolBox) -> Vec<String> {
        toolbox.defs().into_iter().map(|def| def.name).collect()
    }

    #[tokio::test]
    async fn propose_command_is_offered_only_to_the_terminal_ui() {
        let mut toolbox = bare_toolbox();
        let args = r#"{"cmd":"sudo make install","reason":"installs to /usr/local"}"#;
        assert!(!tool_names(&toolbox).contains(&"propose_command".to_string()));
        assert_eq!(toolbox.run("propose_command", args).await.1, "blocked");
        toolbox.interactive_ui = true;
        let def = toolbox
            .defs()
            .into_iter()
            .find(|def| def.name == "propose_command")
            .unwrap();
        assert_eq!(def.parameters["required"], json!(["cmd", "reason"]));
        let (result, status) = toolbox.run("propose_command", args).await;
        assert_eq!(
            result,
            "proposed to the user: !sudo make install · wait for them to run it; its output will appear in the conversation"
        );
        assert_eq!(classify_tool_result(&result, &status), None);
        assert!(is_read_only_tool("propose_command", args));
        let (result, status) = toolbox.run("propose_command", r#"{"cmd":"make"}"#).await;
        assert!(
            result.starts_with("missing required field: reason"),
            "{result}"
        );
        assert_eq!(status, "invalid arguments");
    }

    #[test]
    fn request_workspace_is_offered_only_in_chat_mode_with_an_approver() {
        let (mut toolbox, dir) = fs_toolbox();
        let offered = |toolbox: &ToolBox| tool_names(toolbox).contains(&"request_workspace".into());
        toolbox.approver = Some(FakeApprover::new(Decision::Allow));
        assert!(!offered(&toolbox), "agent mode already has a workspace");
        toolbox.workspace = None;
        assert!(offered(&toolbox));
        toolbox.approver = None;
        assert!(!offered(&toolbox), "headless toolboxes cannot ask");
        assert!(!is_read_only_tool(
            "request_workspace",
            r#"{"root":"/x","reason":"y"}"#
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn request_workspace_asks_the_user_and_reports_the_decision() {
        let mut toolbox = bare_toolbox();
        let args = r#"{"root":"/src/app","reason":"to fix the failing test"}"#;
        let (result, status) = toolbox.run("request_workspace", args).await;
        assert!(
            result.starts_with("request_workspace is only available"),
            "{result}"
        );
        assert_eq!(status, "blocked");
        let allow = FakeApprover::new(Decision::Allow);
        toolbox.approver = Some(allow.clone());
        let (result, status) = toolbox.run("request_workspace", args).await;
        assert_eq!(
            result,
            "approved: agent mode in /src/app · workspace tools are available from your next turn"
        );
        assert_eq!(classify_tool_result(&result, &status), None);
        assert_eq!(
            allow.requests.lock().unwrap().as_slice(),
            [ApprovalRequest {
                tool: "request_workspace".into(),
                action: "agent".into(),
                summary: "switch this session to agent mode in /src/app\nto fix the failing test"
                    .into(),
            }]
        );
        toolbox.approver = Some(FakeApprover::new(Decision::Deny));
        let (result, status) = toolbox.run("request_workspace", args).await;
        assert_eq!(
            result,
            "denied by the user · ask what they want instead of retrying"
        );
        assert_eq!(status, "blocked");
    }

    #[tokio::test]
    async fn batch_refuses_the_user_bridge_tools() {
        let mut toolbox = bare_toolbox();
        toolbox.interactive_ui = true;
        let allow = FakeApprover::new(Decision::Allow);
        toolbox.approver = Some(allow.clone());
        for tool in ["propose_command", "request_workspace"] {
            let args = json!({ "calls": [{ "tool": tool, "arguments": {
                "cmd": "make", "root": "/x", "reason": "y"
            } }] })
            .to_string();
            let (result, _) = toolbox.run("batch", &args).await;
            assert!(result.contains("cannot run inside batch"), "{result}");
        }
        assert!(allow.requests.lock().unwrap().is_empty());
    }
}
