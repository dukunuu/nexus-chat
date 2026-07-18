//! Tools the model can call mid-response: `skill` (progressive-disclosure
//! skill bodies) and, once configured, `web_search`. Concrete (no trait) —
//! there's exactly one implementation and no need for one yet.

use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::provider::ToolDef;
use crate::skills::{load_skills, skill_body};

pub struct ToolBox {
    pub skills_dir: PathBuf,
    /// Base URL of a SearXNG instance (e.g. `http://localhost:8080`), no
    /// trailing slash. Free and self-hosted — no API key needed.
    pub searxng_url: Option<String>,
    /// LangSearch API key (free tier, no card): https://langsearch.com/dashboard
    pub langsearch_key: Option<String>,
    /// Which backend `web_search` prefers: "auto" (LangSearch, then SearXNG,
    /// then DuckDuckGo), or an explicit "langsearch"/"searxng"/"duckduckgo".
    pub search_provider: String,
    /// When true, `defs()`/`run()` restrict to `web_search`/`fetch_url` only —
    /// used for deep-research searcher agents, which must never reach
    /// run_python/install_packages/app tools even if hallucinated.
    research_only: bool,
    /// Domains a per-space setting always excludes from `web_search` results
    /// (appended to any exclude_domains the model passes).
    pub blocked_domains: Vec<String>,
    /// Db path for the (space-agnostic) fetched-page cache; None disables
    /// caching (some tests).
    web_cache_db: Option<PathBuf>,
    /// Set for follow-up turns inside a `/research` session: enables the
    /// `search_sources` tool over that session's gathered source bundle.
    research_session_id: Option<String>,
    client: reqwest::Client,
    files: Option<FilesCtx>,
    apps: Option<AppsCtx>,
    /// When true, `fetch_cached` never hits the network on a cache miss —
    /// used for the Verifier stage's quote-checking pass, which must only
    /// ever see pages the searchers actually gathered, never fresh fetches.
    cache_only: bool,
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
    pub space_db_path: PathBuf,
    pub images_dir: PathBuf,
    pub session_id: String,
}

impl ToolBox {
    pub fn new(
        skills_dir: PathBuf,
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        blocked_domains: Vec<String>,
        web_cache_db: Option<PathBuf>,
        files: Option<FilesCtx>,
        apps: Option<AppsCtx>,
    ) -> Self {
        ToolBox {
            skills_dir,
            searxng_url,
            langsearch_key,
            search_provider,
            research_only: false,
            blocked_domains,
            web_cache_db,
            research_session_id: None,
            client: reqwest::Client::new(),
            files,
            apps,
            cache_only: false,
        }
    }

    /// A toolbox restricted to `web_search`/`fetch_url` — for deep-research
    /// searcher agents, which get no filesystem/app/script access.
    pub fn research(
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        blocked_domains: Vec<String>,
        web_cache_db: Option<PathBuf>,
    ) -> Self {
        let mut tb = ToolBox::new(
            PathBuf::new(),
            searxng_url,
            langsearch_key,
            search_provider,
            blocked_domains,
            web_cache_db,
            None,
            None,
        );
        tb.research_only = true;
        tb
    }

    /// Attach a research session id, enabling `search_sources` for follow-up
    /// turns in that session's chat. Also merges any domains the user has
    /// discarded in this session into `blocked_domains`, so a later
    /// `web_search`/`fetch_url` call excludes them the same way the global
    /// setting does.
    pub fn with_research_session(mut self, session_id: String) -> Self {
        if let Some(db_path) = &self.web_cache_db
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
    pub fn cache_only(mut self) -> Self {
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

    /// Resolve which backend to actually use for this call. An explicit
    /// choice ("langsearch"/"searxng"/"duckduckgo") is used as-is — if it's
    /// not configured, that's a clear error rather than a silent swap to
    /// something else the user didn't pick. "auto" (the default) picks the
    /// best configured option: LangSearch, then SearXNG, then DuckDuckGo
    /// scraping last — DuckDuckGo now routinely serves a CAPTCHA to automated
    /// requests, so it's unreliable, not just unofficial.
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
            _ => {
                if let Some(key) = &self.langsearch_key {
                    langsearch_search(&self.client, key, &query, recency).await
                } else if let Some(url) = &self.searxng_url {
                    searxng_search(&self.client, url, &query, recency).await
                } else {
                    duckduckgo_search(&self.client, &query).await
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
            && let Some(db_path) = &self.web_cache_db
            && let Ok(conn) = rusqlite::Connection::open(db_path)
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
        if let Some(db_path) = &self.web_cache_db
            && let Ok(conn) = rusqlite::Connection::open(db_path)
        {
            let _ = crate::db::cache_put(&conn, &url_norm, url, None, &text);
        }
        Ok(text)
    }

    /// Tool definitions to attach to the request, or empty to send a request
    /// identical to one from before tool-calling existed (keeps models that
    /// don't support tools working unchanged). `web_search` always works —
    /// it prefers the configured SearXNG instance, falling back to scraping
    /// DuckDuckGo's HTML search when none is set, so it needs no setup.
    pub fn defs(&self) -> Vec<ToolDef> {
        let mut defs = Vec::new();
        if !load_skills(&self.skills_dir).is_empty() {
            defs.push(ToolDef {
                name: "skill".to_string(),
                description: "Load the full instructions for a named skill.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "name": { "type": "string", "description": "the skill's name" } },
                    "required": ["name"],
                }),
            });
            defs.push(ToolDef {
                name: "run_script".to_string(),
                description: "Run a script that ships inside an installed skill. Python scripts run in the skill's own virtualenv (created on first use; the skill's requirements.txt is installed into it automatically). Returns stdout/stderr.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "skill": { "type": "string", "description": "the skill's name" },
                        "script": { "type": "string", "description": "script path inside the skill, e.g. 'scripts/convert.py'" },
                        "args": { "type": "array", "items": { "type": "string" }, "description": "command-line arguments" },
                    },
                    "required": ["skill", "script"],
                }),
            });
        }
        defs.push(ToolDef {
            name: "run_python".to_string(),
            description: "Run a Python script and return its output. Use this for any nontrivial calculation, data processing, or exact math instead of computing mentally. Runs in a persistent scratch virtualenv; print() what you need back. Add packages to the venv with install_packages (no skill/app target).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "the python source to run" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "command-line arguments" },
                },
                "required": ["code"],
            }),
        });
        defs.push(ToolDef {
            name: "install_packages".to_string(),
            description: "Install packages into an isolated environment — pip packages into a skill's own virtualenv (pass skill), npm packages into an app's node_modules (pass app; reference files as node_modules/<pkg>/… in the app's HTML), or pip packages into the run_python scratch venv (pass neither). Never installs globally.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "packages": { "type": "array", "items": { "type": "string" }, "description": "package names" },
                    "skill": { "type": "string", "description": "skill to pip-install into (mutually exclusive with app)" },
                    "app": { "type": "string", "description": "app to npm-install into (mutually exclusive with skill)" },
                },
                "required": ["packages"],
            }),
        });
        defs.push(ToolDef {
            name: "install_skill".to_string(),
            description: "Install a skill from GitHub. source is owner/repo/path pointing at a directory that contains SKILL.md (bare owner/repo for a skill at the repo root).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "source": { "type": "string", "description": "owner/repo/path of the skill directory" } },
                "required": ["source"],
            }),
        });
        defs.push(ToolDef {
            name: "web_search".to_string(),
            description: "Search the web and return numbered results with title, url, and snippet. recency restricts to recent results (ignored by the DuckDuckGo fallback backend). include_domains/exclude_domains restrict or exclude specific sites.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "the search query" },
                    "recency": { "type": "string", "enum": ["day", "week", "month", "year"], "description": "restrict to results from this recent window" },
                    "include_domains": { "type": "array", "items": { "type": "string" }, "description": "only these domains" },
                    "exclude_domains": { "type": "array", "items": { "type": "string" }, "description": "never these domains" },
                },
                "required": ["query"],
            }),
        });
        defs.push(ToolDef {
            name: "fetch_url".to_string(),
            description: "Fetch a web page and return its readable text (HTML stripped), up to 200 lines per call. Use offset to page through longer pages. Use after web_search to read a promising result in full.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "the page URL to fetch" },
                    "offset": { "type": "integer", "description": "1-based first line to read (default 1)" },
                    "limit": { "type": "integer", "description": "lines to read, max 200 (default 200)" },
                    "fresh": { "type": "boolean", "description": "bypass the 24h page cache and re-fetch live" },
                },
                "required": ["url"],
            }),
        });
        defs.push(ToolDef {
            name: "academic_search".to_string(),
            description: "Search scholarly literature (Semantic Scholar): title, authors, year, venue, abstract, citation count, and URL per paper. Use for research topics needing peer-reviewed sources.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "the search query" },
                    "limit": { "type": "integer", "description": "max papers to return (default 10, max 20)" },
                },
                "required": ["query"],
            }),
        });
        defs.push(ToolDef {
            name: "discussion_search".to_string(),
            description: "Search Hacker News and Reddit discussions for a query: title, URL, and engagement metadata (points/comments or subreddit/upvotes) per hit. Use for community sentiment/opinion on a topic, not authoritative facts.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "the search query" },
                },
                "required": ["query"],
            }),
        });
        if self.research_session_id.is_some() {
            defs.push(ToolDef {
                name: "search_sources".to_string(),
                description: "Keyword-search this research session's already-gathered sources (fetched pages from its /research run). Prefer this over web_search for follow-up questions — only reach for web_search on a miss.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "keywords to search the session's cached sources for" } },
                    "required": ["query"],
                }),
            });
        }
        if self.files.is_some() {
            defs.push(ToolDef {
                name: "list_citations".to_string(),
                description: "List sources cited in past research reports in this space, optionally filtered by a substring match against url/title/report name. Use to answer 'what have we researched about X' or 'which reports cite <site>'.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "substring to match (omit to list everything)" } },
                }),
            });
        }
        if self.files_count() > 0 {
            defs.push(ToolDef {
                name: "search_files".to_string(),
                description: "Search the space's imported files by meaning (semantic embedding search, any language); returns the most relevant passages with file name and location. Natural-language questions and keywords both work.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "what to look for — a question, phrase, or keywords" } },
                    "required": ["query"],
                }),
            });
            defs.push(ToolDef {
                name: "read_file".to_string(),
                description: "Read the extracted text of an imported file, up to 200 lines per call. Use offset to page through longer files.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "the file's name as listed in the system prompt" },
                        "offset": { "type": "integer", "description": "1-based first line to read (default 1)" },
                        "limit": { "type": "integer", "description": "lines to read, max 200 (default 200)" },
                    },
                    "required": ["name"],
                }),
            });
        }
        if self.apps.is_some() {
            defs.push(ToolDef {
                name: "write_file".to_string(),
                description: "Create or overwrite a file in a named app (a static web app served locally). Use it to build HTML/CSS/JS the user can open in a browser; the result includes the live URL.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "app": { "type": "string", "description": "app name (directory), e.g. 'presentation'" },
                        "path": { "type": "string", "description": "file path inside the app, e.g. 'index.html' or 'js/deck.js'" },
                        "content": { "type": "string", "description": "full file content" },
                    },
                    "required": ["app", "path", "content"],
                }),
            });
            defs.push(ToolDef {
                name: "edit_file".to_string(),
                description: "Edit lines in an app file by hash, not by string matching. Call read_app_file first — each line comes back as `N:HASH<tab>content`. Each edit is {\"hash\": \"<the HASH for a line you read>\", \"new\": \"<replacement>\"}; `new` replaces that ENTIRE line (include the parts you're keeping) and may contain \\n to turn one line into several — that's also how you insert (replace a line with itself plus the new lines). Omit \"new\" (or set it null) to delete the line. Hashes are recomputed against the file's current content each call, so a stale hash (someone/something else changed the file since you read it) is rejected instead of silently hitting the wrong line.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "app": { "type": "string", "description": "app name" },
                        "path": { "type": "string", "description": "file path inside the app" },
                        "edits": {
                            "type": "array",
                            "description": "one or more line edits, from the HASH column of a prior read_app_file call",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "hash": { "type": "string", "description": "the line's HASH, from read_app_file's N:HASH prefix" },
                                    "new": { "type": ["string", "null"], "description": "replacement text for the whole line (may contain \\n); null/omitted deletes the line" },
                                },
                                "required": ["hash"],
                            },
                        },
                    },
                    "required": ["app", "path", "edits"],
                }),
            });
            defs.push(ToolDef {
                name: "grep_app".to_string(),
                description: "Search an app's files for a substring (case-insensitive). Returns path:line: text matches — use it to find where something lives before editing.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "app": { "type": "string", "description": "app name" },
                        "pattern": { "type": "string", "description": "text to search for" },
                    },
                    "required": ["app", "pattern"],
                }),
            });
            defs.push(ToolDef {
                name: "read_app_file".to_string(),
                description: "Read a file from an app, up to 200 lines per call. Use offset to page through longer files. Lines come back as `N:HASH\tcontent` — HASH is what edit_file targets, and it changes if the line's content or position changes, so always re-read before editing something you read a while ago.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "app": { "type": "string", "description": "app name" },
                        "path": { "type": "string", "description": "file path inside the app" },
                        "offset": { "type": "integer", "description": "1-based first line to read (default 1)" },
                        "limit": { "type": "integer", "description": "lines to read, max 200 (default 200)" },
                    },
                    "required": ["app", "path"],
                }),
            });
        }
        if self.research_only {
            defs.retain(|d| {
                matches!(
                    d.name.as_str(),
                    "web_search" | "fetch_url" | "academic_search" | "discussion_search"
                )
            });
        }
        defs
    }

    /// Resolve `<apps dir>/<app>/<rel>`, rejecting anything that could
    /// escape the apps dir (absolute paths, `..`/`.` segments, backslashes).
    fn app_path(&self, app: &str, rel: &str) -> Result<PathBuf, String> {
        let Some(ctx) = &self.apps else {
            return Err("apps are not available".to_string());
        };
        resolve_confined(&ctx.dir, app, rel)
    }

    /// The `run_python` scratch dir (venv + script), a sibling of the skills
    /// dir: `<data>/python`. Persistent so installed packages survive.
    fn python_dir(&self) -> PathBuf {
        self.skills_dir
            .parent()
            .map(|p| p.join("python"))
            .unwrap_or_else(|| PathBuf::from("python"))
    }

    /// The live URL for an app.
    fn app_link(&self, app: &str) -> String {
        match &self.apps {
            Some(ctx) => {
                match ctx.registry.resolve(&ctx.space_name, app) {
                    Some(uuid) => format!("live at http://127.0.0.1:{}/{}/", ctx.server_port, uuid),
                    None => format!("live at http://127.0.0.1:{}/{}/", ctx.server_port, crate::appserver::encode(app)),
                }
            }
            None => String::new(),
        }
    }

    /// Run a tool by name. Returns `(result text sent back to the model,
    /// status label shown in the UI while it runs)`.
    pub async fn run(&self, name: &str, args: &str) -> (String, String) {
        if self.research_only
            && !matches!(
                name,
                "web_search" | "fetch_url" | "academic_search" | "discussion_search"
            )
        {
            return (
                format!("tool '{name}' is not available in research mode"),
                "blocked".to_string(),
            );
        }
        match name {
            "skill" => {
                let skill_name = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = format!("Reading skill {skill_name}…");
                let path = self.skills_dir.join(&skill_name).join("SKILL.md");
                let result = match std::fs::read_to_string(&path) {
                    Ok(md) => skill_body(&md).to_string(),
                    Err(_) => format!("unknown skill: {skill_name}"),
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
            "run_script" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| {
                    v.get(k)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (skill, script) = (field("skill"), field("script"));
                let extra: Vec<String> = v
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let status = format!("Running {skill}/{script}…");
                let result = match resolve_confined(&self.skills_dir, &skill, &script) {
                    Err(e) => e,
                    Ok(file) if !file.is_file() => format!("no such script: {skill}/{script}"),
                    Ok(file) => {
                        let dir = self.skills_dir.join(&skill);
                        let ext = file
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let run = async {
                            let mut argv: Vec<std::ffi::OsString> = Vec::new();
                            let program: std::ffi::OsString = match ext.as_str() {
                                "py" => {
                                    let py = ensure_venv(&dir).await?;
                                    argv.push(file.clone().into());
                                    py.into()
                                }
                                "sh" | "bash" => {
                                    argv.push(file.clone().into());
                                    "bash".into()
                                }
                                "js" | "mjs" => {
                                    argv.push(file.clone().into());
                                    "node".into()
                                }
                                _ => file.clone().into(),
                            };
                            argv.extend(extra.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                argv.iter().map(|s| s.as_os_str()).collect();
                            run_cmd(&program, &refs, &dir, 120).await
                        };
                        match run.await {
                            Ok(out) => format_output(&out),
                            Err(e) => e,
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
                                    let mut argv: Vec<std::ffi::OsString> =
                                        vec!["-m".into(), "pip".into(), "install".into()];
                                    argv.extend(pkgs.iter().map(std::ffi::OsString::from));
                                    let refs: Vec<&std::ffi::OsStr> =
                                        argv.iter().map(|s| s.as_os_str()).collect();
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
                    Ok(()) if !app.is_empty() => match self.app_path(&app, "package.json") {
                        Err(e) => e,
                        Ok(pkg_json) => {
                            let dir = pkg_json.parent().unwrap().to_path_buf();
                            let prep = std::fs::create_dir_all(&dir).and_then(|()| {
                                if pkg_json.exists() {
                                    Ok(())
                                } else {
                                    // npm walks up looking for a package.json — pin
                                    // the install to this app dir with a minimal one.
                                    std::fs::write(
                                        &pkg_json,
                                        format!("{{\"name\":{:?},\"private\":true}}", app),
                                    )
                                }
                            });
                            match prep {
                                Err(e) => format!("cannot prepare {app}: {e}"),
                                Ok(()) => {
                                    let mut argv: Vec<std::ffi::OsString> = vec![
                                        "install".into(),
                                        "--no-audit".into(),
                                        "--no-fund".into(),
                                    ];
                                    argv.extend(pkgs.iter().map(std::ffi::OsString::from));
                                    let refs: Vec<&std::ffi::OsStr> =
                                        argv.iter().map(|s| s.as_os_str()).collect();
                                    match run_cmd("npm".as_ref(), &refs, &dir, 300).await {
                                        Ok(out) if out.status.success() => format!(
                                            "installed {} into {app}/node_modules — reference files as node_modules/<pkg>/… ; {}",
                                            pkgs.join(" "),
                                            self.app_link(&app),
                                        ),
                                        Ok(out) => {
                                            format!("npm install failed:\n{}", format_output(&out))
                                        }
                                        Err(e) => e,
                                    }
                                }
                            }
                        }
                    },
                    Ok(()) => {
                        // No target: the run_python scratch venv.
                        let dir = self.python_dir();
                        let run = async {
                            std::fs::create_dir_all(&dir)
                                .map_err(|e| format!("cannot create {dir:?}: {e}"))?;
                            let py = ensure_venv(&dir).await?;
                            let mut argv: Vec<std::ffi::OsString> =
                                vec!["-m".into(), "pip".into(), "install".into()];
                            argv.extend(pkgs.iter().map(std::ffi::OsString::from));
                            let refs: Vec<&std::ffi::OsStr> =
                                argv.iter().map(|s| s.as_os_str()).collect();
                            run_cmd(py.as_os_str(), &refs, &dir, 300).await
                        };
                        match run.await {
                            Ok(out) if out.status.success() => {
                                format!("installed {} into the python scratch venv", pkgs.join(" "))
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
                let extra: Vec<String> = v
                    .get("args")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let status = "Running python…".to_string();
                let dir = self.python_dir();
                let run = async {
                    if code.trim().is_empty() {
                        return Err("code must not be empty".to_string());
                    }
                    std::fs::create_dir_all(&dir)
                        .map_err(|e| format!("cannot create {dir:?}: {e}"))?;
                    let script = dir.join("script.py");
                    std::fs::write(&script, &code)
                        .map_err(|e| format!("cannot write script: {e}"))?;
                    let py = ensure_venv(&dir).await?;
                    let mut argv: Vec<std::ffi::OsString> = vec![script.into()];
                    argv.extend(extra.iter().map(std::ffi::OsString::from));
                    let refs: Vec<&std::ffi::OsStr> = argv.iter().map(|s| s.as_os_str()).collect();
                    run_cmd(py.as_os_str(), &refs, &dir, 120).await
                };
                let result = match run.await {
                    Ok(out) => format_output(&out),
                    Err(e) => e,
                };
                (result, status)
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
                let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
                let limit = v
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let fresh = v.get("fresh").and_then(|f| f.as_bool()).unwrap_or(false);
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
                let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
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
                let cached = if let Some(db_path) = &self.web_cache_db {
                    rusqlite::Connection::open(db_path)
                        .ok()
                        .and_then(|conn| crate::db::cache_get(&conn, &cache_key).ok().flatten())
                        .and_then(|(_, text, fetched_at)| {
                            if crate::db::is_fresh(&fetched_at, chrono::Utc::now()) {
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
                    if let Some(db_path) = &self.web_cache_db {
                        if let Ok(conn) = rusqlite::Connection::open(db_path) {
                            let _ =
                                crate::db::cache_put(&conn, &cache_key, &cache_key, None, &text);
                        }
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
                let result = match (&self.research_session_id, &self.web_cache_db) {
                    (Some(session_id), Some(db_path)) => {
                        match rusqlite::Connection::open(db_path) {
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
                        }
                    }
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
                let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
                let limit = v
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {name}…");
                let result = match &self.files {
                    None => "no files imported".to_string(),
                    Some(ctx) => match rusqlite::Connection::open(&ctx.db_path)
                        .map_err(anyhow::Error::from)
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
                let result = match self.app_path(&app, &path) {
                    Err(e) => e,
                    Ok(file) => {
                        let write = file
                            .parent()
                            .map(std::fs::create_dir_all)
                            .unwrap_or(Ok(()))
                            .and_then(|()| std::fs::write(&file, &content));
                        match write {
                            Ok(()) => format!(
                                "wrote {app}/{path} ({} bytes) — {}",
                                content.len(),
                                self.app_link(&app),
                            ),
                            Err(e) => format!("write failed: {e}"),
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
                let result = match self.app_path(&app, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
                        Err(e) => format!("cannot read {app}/{path}: {e}"),
                        Ok(text) => match apply_hashline_edits(&text, &edits) {
                            Err(e) => e,
                            Ok((new_text, diff)) => match std::fs::write(&file, new_text) {
                                Ok(()) => {
                                    format!("edited {app}/{path} — {}{diff}", self.app_link(&app))
                                }
                                Err(e) => format!("write failed: {e}"),
                            },
                        },
                    },
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
                let status = format!("Searching {app}…");
                let result = match self
                    .app_path(&app, "index.html")
                    .map(|p| p.parent().unwrap().to_path_buf())
                {
                    Err(e) => e,
                    Ok(dir) if !dir.is_dir() => format!("unknown app: {app}"),
                    Ok(_) if pattern.is_empty() => "pattern must not be empty".to_string(),
                    Ok(dir) => {
                        let mut hits = Vec::new();
                        grep_dir(&dir, &dir, &pattern.to_lowercase(), &mut hits);
                        if hits.is_empty() {
                            format!("no matches for {pattern:?} in {app}")
                        } else {
                            let n = hits.len();
                            hits.truncate(50);
                            if n > 50 {
                                hits.push(format!("… ({} more matches)", n - 50));
                            }
                            hits.join("\n")
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
                let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
                let limit = v
                    .get("limit")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(200)
                    .clamp(1, 200) as usize;
                let status = format!("Reading {app}/{path}…");
                let result = match self.app_path(&app, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
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
                    },
                };
                (result, status)
            }
            other => (
                format!("unknown tool: {other}"),
                "Running tool…".to_string(),
            ),
        }
    }
}

/// `cat -n`-style numbering for ranged reads, matching what agent harnesses
/// feed models so line references and edits anchor reliably.
/// Search imported files: embed the query and rank chunks by cosine when an
/// embedder is configured; otherwise (or when embedding fails / no vectors
/// are stored yet) fall back to FTS keywords, tagged so the model knows the
/// weaker path answered.
async fn search_files_impl(ctx: &FilesCtx, query: &str) -> String {
    let conn = match rusqlite::Connection::open(&ctx.db_path) {
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
        .map(|b| format!("{b:02x}"))
        .collect()
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
        diff.push_str(&format!("\n- {}", lines[*idx]));
        if let Some(t) = new {
            for l in t.lines() {
                diff.push_str(&format!("\n+ {l}"));
            }
        }
    }

    // Apply highest index first so earlier (lower) indices, still unprocessed,
    // stay valid regardless of how many lines an edit adds or removes.
    let mut apply_order = resolved;
    apply_order.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    for (idx, new) in apply_order {
        match new {
            None => {
                out.remove(idx);
            }
            Some(t) => {
                let replacement: Vec<String> = t.lines().map(str::to_string).collect();
                out.splice(idx..idx + 1, replacement);
            }
        }
    }
    let mut new_text = out.join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        new_text.push('\n');
    }
    Ok((new_text, diff))
}

/// Recursively collect `relpath:line: text` matches for a lowercase substring
/// pattern, skipping dependency/venv dirs and unreadable (binary) files.
fn grep_dir(root: &std::path::Path, dir: &std::path::Path, pattern: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "node_modules" && name != ".venv" && name != ".git" {
                grep_dir(root, &path, pattern, out);
            }
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            let rel = path.strip_prefix(root).unwrap_or(&path).display();
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(pattern) {
                    out.push(format!("{rel}:{}: {}", i + 1, line.trim()));
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
    let fut = tokio::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .kill_on_drop(true)
        .output();
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
            .map(|c| c.to_string())
            .unwrap_or_else(|| "killed".to_string());
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&format!("exit code: {code}"));
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
        .map_err(|e| format!("cannot create {skill_dir:?}: {e}"))?;
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
/// in argv directly, so a leading `-` would become an option injection.
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
/// raise on a non-2xx status, then deserialize. DuckDuckGo scrapes HTML
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

/// SearXNG's JSON API needs `search: formats: [html, json]` enabled in the
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

/// LangSearch (https://langsearch.com): free-tier hosted search API, no card
/// required. More reliable than scraping DuckDuckGo — recommended default
/// once you have a key.
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

/// Zero-setup fallback used when no SearXNG instance is configured: scrapes
/// DuckDuckGo's plain HTML search page (no JS, no API, no key) the same way
/// LM Studio/Open WebUI's built-in DuckDuckGo tools do. Unofficial — DuckDuckGo
/// can change this markup or rate-limit it at any time; SearXNG is the more
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
    Ok(parse_ddg_html(&html).into_iter().take(8).collect())
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

/// Whether `url` points at a YouTube watch page (long or short form).
fn is_youtube_url(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(u.host_str(), Some(h) if h == "youtube.com" || h.ends_with(".youtube.com") || h == "youtu.be")
}

/// Pull the first caption track's `baseUrl` out of a YouTube watch page's
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

/// Join a YouTube timedtext XML transcript's `<text>` cue contents with
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

/// Fetch a YouTube video's transcript via the keyless timedtext endpoint:
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

/// Pull `(title, url, snippet)` hits out of a DuckDuckGo HTML results page.
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

/// DuckDuckGo's result links redirect through `/l/?uddg=<percent-encoded-url>`.
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
    out.push_str(&format!("| {} |\n", rows[0].join(" | ")));
    out.push_str(&format!("| {} |\n", vec!["---"; cols].join(" | ")));
    for row in &rows[1..] {
        out.push_str(&format!("| {} |\n", row.join(" | ")));
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
/// tool-result text — the model falls back to web_search.
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
/// to what the model needs to decide whether to fetch_url it.
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
/// independently still returns the other's hits (never fails the whole
/// tool call over one down API).
async fn discussion_search(client: &reqwest::Client, query: &str) -> String {
    let (hn, reddit) = tokio::join!(hn_search(client, query), reddit_search(client, query));
    let hn = hn.unwrap_or_default();
    let reddit = reddit.unwrap_or_default();
    if hn.is_empty() && reddit.is_empty() {
        "no results".to_string()
    } else {
        format_discussion_hits(&hn, &reddit)
    }
}

/// Normalize a source URL for dedup: lowercase the host only (path/query
/// case is preserved — some servers are case-sensitive there), strip
/// `utm_*`/`fbclid` query params, and drop a trailing `/` and any fragment.
/// Unparseable input (not actually a URL) is returned unchanged so it still
/// participates in a plain string-equality dedup.
pub(crate) fn normalize_url(url: &str) -> String {
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
pub(crate) fn dedup_source_lines(findings: &[String]) -> Vec<String> {
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
pub(crate) fn cited_url_norms(findings: &[String]) -> Vec<String> {
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
pub(crate) fn rewrite_query_with_domains(
    query: &str,
    include: &[String],
    exclude: &[String],
) -> String {
    let mut q = query.to_string();
    for d in include {
        q.push_str(&format!(" site:{d}"));
    }
    for d in exclude {
        q.push_str(&format!(" -site:{d}"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_sources_tool_only_appears_and_works_for_a_research_session_toolbox() {
        let path =
            std::env::temp_dir().join(format!("nexus-searchsrc-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::Db::open(&path).unwrap();
        let space = db.default_space_id().unwrap();
        let s = db.create_session("t", "a/b", &space).unwrap();
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
        assert!(!tb.defs().iter().any(|d| d.name == "search_sources"));

        let tb = tb.with_research_session(s.id.clone());
        assert!(tb.defs().iter().any(|d| d.name == "search_sources"));
        let (result, _) = tb
            .run("search_sources", r#"{"query":"borrow checker"}"#)
            .await;
        assert!(result.contains("borrow checker"), "{result}");

        let (result, _) = tb.run("search_sources", r#"{"query":"quantum"}"#).await;
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
        let (result, _) = tb.run("list_citations", r#"{}"#).await;
        assert!(result.contains("research-a.md"), "{result}");
        assert!(result.contains("nature.com"), "{result}");

        let (result, _) = tb.run("list_citations", r#"{"query":"nope"}"#).await;
        assert!(result.contains("no citations"), "{result}");
    }

    #[tokio::test]
    async fn fetch_url_serves_from_cache_when_fresh() {
        let path = std::env::temp_dir().join(format!("nexus-webcache-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::Db::open(&path).unwrap();
        crate::db::cache_put(
            db.raw(),
            "https://example.com/a",
            "https://example.com/a",
            None,
            "cached body",
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
        assert!(result.contains("cached body"), "{result}");
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
        let path = std::env::temp_dir().join(format!("nexus-discache-{}.db", uuid::Uuid::new_v4()));
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
            .run("discussion_search", r#"{"query":"rust performance"}"#)
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
        let err = tb.search("test", None, &[], &[]).await.unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("no search backend configured"));
        assert!(!msg.contains("API key"));
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

    fn files_toolbox() -> (ToolBox, crate::db::Db, String) {
        // A real temp-file db (the toolbox opens its own connection by path).
        let path = std::env::temp_dir().join(format!("nexus-tools-{}.db", uuid::Uuid::new_v4()));
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
    async fn run_script_runs_sh_with_args_and_reports_exit_code() {
        let (tb, dir) = skills_toolbox();
        std::fs::write(dir.join("t/go.sh"), "echo \"hi $1\"\nexit 3\n").unwrap();
        let (result, status) = tb
            .run(
                "run_script",
                r#"{"skill":"t","script":"go.sh","args":["there"]}"#,
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
            .run("run_script", r#"{"skill":"t","script":"../evil.sh"}"#)
            .await;
        assert!(result.contains("invalid"), "{result}");
        let (result, _) = tb
            .run("run_script", r#"{"skill":"t","script":"nope.sh"}"#)
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
    async fn run_python_computes_in_scratch_venv() {
        let dir = std::env::temp_dir().join(format!("nexus-py-{}", uuid::Uuid::new_v4()));
        let tb = ToolBox::new(
            dir.join("skills"),
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            None,
            None,
            None,
        );
        let (result, status) = tb.run("run_python", r#"{"code":"print(2**32)"}"#).await;
        assert!(status.contains("Running python"));
        assert!(result.contains("4294967296"), "{result}");
        assert!(dir.join("python/.venv/bin/python").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_app_finds_lines_and_skips_node_modules() {
        let (tb, dir) = apps_toolbox();
        let _ = tb.run("write_file", r#"{"app":"deck","path":"index.html","content":"<h1>Title</h1>\n<p>slide two</p>"}"#).await;
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
        assert!(
            result.contains("index.html:2: <p>slide two</p>"),
            "{result}"
        );
        assert!(result.contains("js/a.js:1: // slide logic"), "{result}");
        assert!(!result.contains("node_modules"), "{result}");
        let (result, _) = tb
            .run("grep_app", r#"{"app":"deck","pattern":"zzz"}"#)
            .await;
        assert!(result.contains("no matches"), "{result}");
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
        assert!(names.contains(&"search_files".to_string()));
        assert!(names.contains(&"read_file".to_string()));

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
        assert!(!names.contains(&"search_files".to_string()));
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
        assert!(names.contains(&"web_search".to_string()));
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
                space_db_path: PathBuf::from("/tmp/test.db"),
                images_dir: dir.clone(),
                session_id: String::new(),
            }),
        );
        (tb, dir)
    }

    #[test]
    fn defs_include_app_tools_only_with_apps_ctx() {
        let (tb, _) = apps_toolbox();
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        for t in ["write_file", "edit_file", "read_app_file"] {
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
        assert!(!names.contains(&"write_file".to_string()));
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
            result.contains("http://127.0.0.1:9999/deck/"),
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
    fn research_toolbox_offers_web_search_fetch_url_academic_search_and_discussion_search() {
        let tb = ToolBox::research(None, None, "auto".to_string(), Vec::new(), None);
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        assert_eq!(names.len(), 4, "{names:?}");
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"fetch_url".to_string()));
        assert!(names.contains(&"academic_search".to_string()));
        assert!(names.contains(&"discussion_search".to_string()));
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
