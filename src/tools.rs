//! Tools the model can call mid-response: `skill` (progressive-disclosure
//! skill bodies) and, once configured, `web_search`. Concrete (no trait) —
//! there's exactly one implementation and no need for one yet.

use std::path::PathBuf;

use serde::Deserialize;

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
    client: reqwest::Client,
    files: Option<FilesCtx>,
    apps: Option<AppsCtx>,
}

/// Where the file tools read from: the shared db plus the space to scope to.
/// The toolbox opens its own short-lived connection per call — the app's
/// `Db` handle stays on the UI task and is never shared with the stream task.
pub struct FilesCtx {
    pub db_path: std::path::PathBuf,
    pub space_id: String,
}

/// Where the app tools write: the active space's apps dir, plus the URL
/// prefix the app server serves it at. Only present while the server runs.
pub struct AppsCtx {
    pub dir: PathBuf,
    pub space_url: String,
}

impl ToolBox {
    pub fn new(
        skills_dir: PathBuf,
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
        files: Option<FilesCtx>,
        apps: Option<AppsCtx>,
    ) -> Self {
        ToolBox {
            skills_dir,
            searxng_url,
            langsearch_key,
            search_provider,
            client: reqwest::Client::new(),
            files,
            apps,
        }
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
    async fn search(&self, query: &str) -> anyhow::Result<Vec<SearchHit>> {
        match self.search_provider.as_str() {
            "langsearch" => match &self.langsearch_key {
                Some(key) => langsearch_search(&self.client, key, query).await,
                None => anyhow::bail!("LangSearch selected but no API key is configured"),
            },
            "searxng" => match &self.searxng_url {
                Some(url) => searxng_search(&self.client, url, query).await,
                None => anyhow::bail!("SearXNG selected but no instance URL is configured"),
            },
            "duckduckgo" => duckduckgo_search(&self.client, query).await,
            _ => {
                if let Some(key) = &self.langsearch_key {
                    langsearch_search(&self.client, key, query).await
                } else if let Some(url) = &self.searxng_url {
                    searxng_search(&self.client, url, query).await
                } else {
                    duckduckgo_search(&self.client, query).await
                }
            }
        }
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
        }
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
            description: "Search the web and return numbered results with title, url, and snippet.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "the search query" } },
                "required": ["query"],
            }),
        });
        if self.files_count() > 0 {
            defs.push(ToolDef {
                name: "search_files".to_string(),
                description: "Full-text search the space's imported files; returns ranked snippets with file name and line location.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "keywords to search for" } },
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
                description: "Replace an exact string in an app file with a new string. old_string must appear exactly once; read the file first if unsure of its current content.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "app": { "type": "string", "description": "app name" },
                        "path": { "type": "string", "description": "file path inside the app" },
                        "old_string": { "type": "string", "description": "exact text to replace (must be unique in the file)" },
                        "new_string": { "type": "string", "description": "replacement text" },
                    },
                    "required": ["app", "path", "old_string", "new_string"],
                }),
            });
            defs.push(ToolDef {
                name: "read_app_file".to_string(),
                description: "Read a file from an app, up to 200 lines per call. Use offset to page through longer files.".to_string(),
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
        defs
    }

    /// Resolve `<apps dir>/<app>/<rel>`, rejecting anything that could
    /// escape the apps dir (absolute paths, `..`/`.` segments, backslashes).
    fn app_path(&self, app: &str, rel: &str) -> Result<PathBuf, String> {
        let Some(ctx) = &self.apps else { return Err("apps are not available".to_string()) };
        if app.is_empty() || app.contains(['/', '\\']) || app == "." || app == ".." {
            return Err(format!("invalid app name: {app:?}"));
        }
        if rel.is_empty() || rel.starts_with('/') {
            return Err(format!("path must be relative and non-empty: {rel:?}"));
        }
        for seg in rel.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
                return Err(format!("invalid path segment in {rel:?}"));
            }
        }
        let mut p = ctx.dir.join(app);
        for seg in rel.split('/') {
            p.push(seg);
        }
        Ok(p)
    }

    /// The live URL for an app.
    fn app_link(&self, app: &str) -> String {
        match &self.apps {
            Some(c) => format!("live at {}{}/", c.space_url, crate::appserver::encode(app)),
            None => String::new(),
        }
    }

    /// Run a tool by name. Returns `(result text sent back to the model,
    /// status label shown in the UI while it runs)`.
    pub async fn run(&self, name: &str, args: &str) -> (String, String) {
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
                        match crate::skills::install_from_github(&self.client, &owner, &repo, &path, &self.skills_dir)
                            .await
                        {
                            Ok(name) => format!("installed skill '{name}' — load it with the skill tool"),
                            Err(e) => format!("install failed: {e}"),
                        }
                    }
                };
                (result, status)
            }
            "web_search" => {
                let query = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
                    .unwrap_or_default();
                let status = "Searching the web…".to_string();
                let result = match self.search(&query).await {
                    Ok(hits) if hits.is_empty() => "no results".to_string(),
                    Ok(hits) => format_results(&hits),
                    Err(e) => format!("search failed: {e}"),
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
                    Some(ctx) => match rusqlite::Connection::open(&ctx.db_path)
                        .map_err(anyhow::Error::from)
                        .and_then(|conn| crate::db::search_chunks(&conn, &ctx.space_id, &query, 8))
                    {
                        Ok(hits) if hits.is_empty() => "no matches".to_string(),
                        Ok(hits) => hits
                            .iter()
                            .map(|(name, loc, snip)| format!("{name} ({loc}): {snip}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        Err(e) => format!("file search failed: {e}"),
                    },
                };
                (result, status)
            }
            "read_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let name = v.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
                let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).clamp(1, 200) as usize;
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
                                    slice.join("\n"),
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
                let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
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
                let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                let (app, path) = (field("app"), field("path"));
                let (old, new) = (field("old_string"), field("new_string"));
                let status = format!("Editing {app}/{path}…");
                let result = match self.app_path(&app, &path) {
                    Err(e) => e,
                    Ok(file) => match std::fs::read_to_string(&file) {
                        Err(e) => format!("cannot read {app}/{path}: {e}"),
                        Ok(text) if old.is_empty() => {
                            let _ = text;
                            "old_string must not be empty".to_string()
                        }
                        Ok(text) => match text.matches(&old).count() {
                            0 => format!("old_string not found in {app}/{path}; read the file to see its current content"),
                            1 => match std::fs::write(&file, text.replacen(&old, &new, 1)) {
                                Ok(()) => format!("edited {app}/{path} — {}", self.app_link(&app)),
                                Err(e) => format!("write failed: {e}"),
                            },
                            n => format!("old_string matches {n} places in {app}/{path} — include more surrounding text to make it unique"),
                        },
                    },
                };
                (result, status)
            }
            "read_app_file" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                let (app, path) = (field("app"), field("path"));
                let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
                let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).clamp(1, 200) as usize;
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
                                format!("{app}/{path}: offset {offset} is past the end ({total} lines)")
                            } else {
                                format!(
                                    "{app}/{path} (lines {}-{} of {total}):\n{}",
                                    start + 1,
                                    start + slice.len(),
                                    slice.join("\n"),
                                )
                            }
                        }
                    },
                };
                (result, status)
            }
            other => (format!("unknown tool: {other}"), "Running tool…".to_string()),
        }
    }
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
async fn send_and_parse<T: serde::de::DeserializeOwned>(req: reqwest::RequestBuilder) -> anyhow::Result<T> {
    req.send().await?.error_for_status()?.json::<T>().await.map_err(Into::into)
}

/// SearXNG's JSON API needs `search: formats: [html, json]` enabled in the
/// instance's `settings.yml` — off by default. A misconfigured instance
/// surfaces as an HTML/error response here, which `error_for_status`/`json`
/// turns into a readable error for the model rather than a silent empty result.
async fn searxng_search(client: &reqwest::Client, base_url: &str, query: &str) -> anyhow::Result<Vec<SearchHit>> {
    let req = client.get(format!("{base_url}/search")).query(&[("q", query), ("format", "json")]);
    let resp = send_and_parse::<SearxngResponse>(req).await?;
    Ok(resp
        .results
        .into_iter()
        .take(8)
        .map(|r| SearchHit { title: r.title, url: r.url, snippet: r.content })
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
async fn langsearch_search(client: &reqwest::Client, key: &str, query: &str) -> anyhow::Result<Vec<SearchHit>> {
    let req = client
        .post("https://api.langsearch.com/v1/web-search")
        .bearer_auth(key)
        .json(&serde_json::json!({ "query": query, "count": 8 }));
    let resp = send_and_parse::<LangsearchResponse>(req).await?;
    Ok(resp
        .data
        .and_then(|d| d.web_pages)
        .map(|w| w.value)
        .unwrap_or_default()
        .into_iter()
        .map(|r| SearchHit { title: r.name, url: r.url, snippet: r.snippet })
        .collect())
}

/// Zero-setup fallback used when no SearXNG instance is configured: scrapes
/// DuckDuckGo's plain HTML search page (no JS, no API, no key) the same way
/// LM Studio/Open WebUI's built-in DuckDuckGo tools do. Unofficial — DuckDuckGo
/// can change this markup or rate-limit it at any time; SearXNG is the more
/// durable option if this stops working for you.
async fn duckduckgo_search(client: &reqwest::Client, query: &str) -> anyhow::Result<Vec<SearchHit>> {
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

/// Pull `(title, url, snippet)` hits out of a DuckDuckGo HTML results page.
/// Each result is `<a class="result__a" href="...uddg=<url>...">title</a>`
/// followed by `<a class="result__snippet" ...>snippet</a>`.
fn parse_ddg_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("class=\"result__a\"") {
        let marker_at = pos + rel;
        let tag_start = html[..marker_at].rfind('<').unwrap_or(marker_at);
        let Some(gt) = html[marker_at..].find('>') else { break };
        let tag = &html[tag_start..marker_at + gt];
        let text_start = marker_at + gt + 1;
        let Some(close_rel) = html[text_start..].find("</a>") else { break };
        let title = strip_tags(&html[text_start..text_start + close_rel]);
        pos = text_start + close_rel + 4;

        let Some(href) = extract_attr(tag, "href") else { continue };
        let Some(url) = resolve_ddg_href(&href) else { continue };
        let snippet = find_snippet(html, pos);
        if !title.is_empty() {
            hits.push(SearchHit { title, url, snippet });
        }
    }
    hits
}

/// The snippet immediately following a result's title anchor, if any.
fn find_snippet(html: &str, from: usize) -> String {
    let marker = "class=\"result__snippet\"";
    let Some(rel) = html[from..].find(marker) else { return String::new() };
    let idx = from + rel;
    let Some(gt) = html[idx..].find('>') else { return String::new() };
    let text_start = idx + gt + 1;
    let Some(close) = html[text_start..].find("</a>") else { return String::new() };
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
            .and_then(|url| url.query_pairs().find(|(k, _)| k == "uddg").map(|(_, v)| v.into_owned()))
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

    #[test]
    fn formats_results_as_numbered_list() {
        let hits = vec![
            SearchHit { title: "Rust 1.90".into(), url: "https://a".into(), snippet: "release notes".into() },
            SearchHit { title: "Rust blog".into(), url: "https://b".into(), snippet: "announcement".into() },
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
        let tb = ToolBox::new(PathBuf::new(), None, None, "langsearch".to_string(), None, None);
        let err = tb.search("test").await.unwrap_err();
        assert!(err.to_string().contains("LangSearch selected but no API key"));

        let tb = ToolBox::new(PathBuf::new(), None, None, "searxng".to_string(), None, None);
        let err = tb.search("test").await.unwrap_err();
        assert!(err.to_string().contains("SearXNG selected but no instance URL"));
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
            None,
            None,
        );
        let err = tb.search("test").await.unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("no search backend configured"));
        assert!(!msg.contains("API key"));
    }

    #[test]
    fn resolves_uddg_redirect_href() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc";
        assert_eq!(resolve_ddg_href(href).as_deref(), Some("https://example.com/page"));
    }

    #[test]
    fn resolves_protocol_relative_href_without_uddg() {
        assert_eq!(resolve_ddg_href("//example.com/x").as_deref(), Some("https://example.com/x"));
    }

    #[test]
    fn resolve_ddg_href_decodes_plus_as_space() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa+b";
        assert_eq!(resolve_ddg_href(href).as_deref(), Some("https://example.com/a b"));
    }

    #[test]
    fn strip_tags_drops_markup_and_unescapes_entities() {
        assert_eq!(strip_tags("<b>Rust</b> &amp; friends"), "Rust & friends");
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
        let text: String = (1..=250).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        db.set_file_chunks(&id, &crate::extract::chunk_lines(&text)).unwrap();
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            Some(FilesCtx { db_path: path, space_id: space.clone() }),
            None,
        );
        (tb, db, space)
    }

    #[tokio::test]
    async fn install_skill_rejects_bad_shorthand_without_network() {
        let tb = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), None, None);
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

        let empty = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), None, None);
        let names: Vec<String> = empty.defs().iter().map(|d| d.name.clone()).collect();
        assert!(!names.contains(&"search_files".to_string()));
    }

    #[tokio::test]
    async fn search_files_returns_ranked_snippets() {
        let (tb, ..) = files_toolbox();
        let (result, status) = tb.run("search_files", r#"{"query":"line 42"}"#).await;
        assert!(status.contains("Searching files"));
        assert!(result.contains("report.md"));
        assert!(result.contains("lines 41-80"));
    }

    #[tokio::test]
    async fn read_file_is_ranged_and_capped() {
        let (tb, ..) = files_toolbox();
        let (result, _) = tb.run("read_file", r#"{"name":"report.md"}"#).await;
        assert!(result.contains("line 1"));
        assert!(result.contains("line 200"));
        assert!(!result.contains("line 201")); // 200-line cap

        let (result, _) = tb.run("read_file", r#"{"name":"report.md","offset":201}"#).await;
        assert!(result.contains("line 201"));
        assert!(result.contains("line 250"));

        let (result, _) = tb.run("read_file", r#"{"name":"nope.md"}"#).await;
        assert!(result.contains("unknown file"));
    }

    fn apps_toolbox() -> (ToolBox, PathBuf) {
        let dir = std::env::temp_dir().join(format!("nexus-apps-{}", uuid::Uuid::new_v4()));
        let tb = ToolBox::new(
            PathBuf::new(),
            None,
            None,
            "auto".to_string(),
            None,
            Some(AppsCtx { dir: dir.clone(), space_url: "http://127.0.0.1:9999/default/".to_string() }),
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
        let empty = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), None, None);
        let names: Vec<String> = empty.defs().iter().map(|d| d.name.clone()).collect();
        assert!(!names.contains(&"write_file".to_string()));
    }

    #[tokio::test]
    async fn write_edit_read_round_trip_with_live_url() {
        let (tb, dir) = apps_toolbox();
        let (result, _) = tb
            .run("write_file", r#"{"app":"deck","path":"index.html","content":"<h1>Hello</h1>"}"#)
            .await;
        assert!(result.contains("wrote deck/index.html"), "{result}");
        assert!(result.contains("http://127.0.0.1:9999/default/deck/"), "{result}");
        assert_eq!(std::fs::read_to_string(dir.join("deck/index.html")).unwrap(), "<h1>Hello</h1>");

        // nested path creates parent dirs
        let (result, _) =
            tb.run("write_file", r#"{"app":"deck","path":"js/a.js","content":"1"}"#).await;
        assert!(result.contains("wrote deck/js/a.js"), "{result}");

        let (result, _) = tb
            .run("edit_file", r#"{"app":"deck","path":"index.html","old_string":"Hello","new_string":"Bye"}"#)
            .await;
        assert!(result.contains("edited deck/index.html"), "{result}");
        assert_eq!(std::fs::read_to_string(dir.join("deck/index.html")).unwrap(), "<h1>Bye</h1>");

        let (result, _) =
            tb.run("read_app_file", r#"{"app":"deck","path":"index.html"}"#).await;
        assert!(result.contains("<h1>Bye</h1>"), "{result}");
        assert!(result.contains("lines 1-1 of 1"), "{result}");
    }

    #[tokio::test]
    async fn edit_file_rejects_zero_and_multiple_matches() {
        let (tb, _) = apps_toolbox();
        let _ = tb
            .run("write_file", r#"{"app":"a","path":"f.txt","content":"x y x"}"#)
            .await;
        let (result, _) = tb
            .run("edit_file", r#"{"app":"a","path":"f.txt","old_string":"z","new_string":"w"}"#)
            .await;
        assert!(result.contains("not found"), "{result}");
        let (result, _) = tb
            .run("edit_file", r#"{"app":"a","path":"f.txt","old_string":"x","new_string":"w"}"#)
            .await;
        assert!(result.contains("matches 2 places"), "{result}");
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
            assert!(result.contains("invalid") || result.contains("must be relative"), "{args} -> {result}");
        }
        assert!(!dir.join("../f.txt").exists());
    }
}
