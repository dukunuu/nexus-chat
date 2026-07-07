# Research Suite 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the four approved follow-up research areas from `docs/superpowers/specs/2026-07-07-research-suite-2-design.md`: deeper source ingestion (PDF/tables/YouTube/HN+Reddit), steer + verify (mid-flight steering, pin/discard sources, claim confidence, quote checking), standing research (watches), and report export.

**Architecture:** All source-ingestion changes live in `src/tools.rs`'s existing `fetch_cached`/`fetch_url_text` pipeline and the research-only tool dispatch, so every new content type flows through the existing `web_cache` and dedup/citation machinery unchanged. Steering and pin/discard extend the existing mpsc/oneshot channel patterns already used for the plan-approval gate in `src/app/research.rs`. Watches are a new small table plus an on-app-open due-check — no daemon. Export is a pure formatter over data already in `citations`/messages.

**Tech Stack:** Rust 2024, tokio, rusqlite (bundled), reqwest, existing `pdf-extract` dependency (already vendored, used by `src/extract.rs`), no new dependencies.

## Global Constraints

- Follow the migrate-on-open pattern in `src/db.rs::migrate()`: `CREATE TABLE IF NOT EXISTS` + `ALTER TABLE ... ADD COLUMN` (ignore "duplicate column" errors), never a destructive migration.
- Toolbox code never shares the app's `Db` handle — it opens its own short-lived `rusqlite::Connection` by path, same as every existing tool. New free functions in `db.rs` alongside `Db` method wrappers, mirroring `search_citations`/`add_session_sources`.
- Pure functions get unit tests; real network calls (HTTP fetch, YouTube, Algolia, Reddit, Semantic Scholar) are exercised manually per the file header convention already in `research.rs` and `tools.rs`.
- Every `ToolBox::new`/`ToolBox::research` signature change requires updating every call site: `src/tools.rs` tests, `src/app/mod.rs` (two constructor sites), `src/app/research.rs`. Run `cargo build` after each signature change to catch stragglers before writing more code.
- Commit after each task with `cargo build` (0 warnings) and `cargo test` green.

---

### Task 1: PDF reading in `fetch_url`

**Files:**
- Modify: `src/tools.rs` (`fetch_url_text`, around line 1250)
- Test: `src/tools.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new (uses existing `pdf-extract` dependency, already in `Cargo.toml`, already used via `pdf_extract::extract_text_from_mem(buffer: &[u8]) -> Result<String, pdf_extract::OutputError>` semantics — confirmed API in the vendored crate at `~/.cargo/registry/.../pdf-extract-0.12.0/src/lib.rs`).
- Produces: `fetch_url_text` now returns readable text for PDF URLs too — no signature change, so nothing downstream needs updating.

- [ ] **Step 1: Write the failing test**

Add to `src/tools.rs` test module:

```rust
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
```

`crate::extract::pdf_with_pages` is currently `#[cfg(test)]`-gated in `src/extract.rs` — that's fine, both call sites are test-only. Confirm it's visible: it's `pub(crate)`, so `crate::extract::pdf_with_pages` resolves from `tools.rs`'s test module.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test extract_pdf_or_html --lib`
Expected: FAIL with "cannot find function `extract_pdf_or_html`"

- [ ] **Step 3: Write minimal implementation**

In `src/tools.rs`, add near `fetch_url_text`:

```rust
/// Extract readable text from a fetched body: PDF (by content-type or
/// `%PDF` magic bytes) via `pdf-extract`, otherwise treated as HTML.
/// PDF extraction failures degrade to an explanatory string rather than
/// erroring the whole fetch — a scanned/malformed PDF shouldn't kill the
/// searcher's tool call.
fn extract_pdf_or_html(bytes: &[u8], content_type: &str) -> String {
    let looks_like_pdf = content_type.to_lowercase().contains("application/pdf") || bytes.starts_with(b"%PDF");
    if looks_like_pdf {
        return match pdf_extract::extract_text_from_mem(bytes) {
            Ok(text) => text.trim().to_string(),
            Err(e) => format!("[could not extract PDF text: {e}]"),
        };
    }
    let html = String::from_utf8_lossy(bytes);
    strip_html_to_text(&html)
}
```

Then change `fetch_url_text` to capture the content-type header and delegate:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS, including the 3 new tests and all 273 pre-existing.

- [ ] **Step 5: Commit**

```bash
git add src/tools.rs
git commit -m "Extract text from PDF responses in fetch_url

fetch_url previously ran every fetched body through the HTML-stripping
pipeline, which mangles a PDF's raw byte stream into garbage. Detect PDFs
by content-type or %PDF magic bytes and extract with pdf-extract instead,
reusing the dependency src/extract.rs already vendors for file imports."
```

---

### Task 2: HTML tables rendered as markdown

**Files:**
- Modify: `src/tools.rs` (`strip_html_to_text`, around line 1386)
- Test: `src/tools.rs`

**Interfaces:**
- Consumes: `strip_tags` (existing private fn in `tools.rs`) for cell text cleanup.
- Produces: `strip_html_to_text` output now contains `| a | b |` pipe-table blocks for `<table>` elements; unchanged for pages without tables.

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test strip_html_to_text_renders_table --lib`
Expected: FAIL — assertion on `| Model | Score |` not found (current code flattens table text without pipes).

- [ ] **Step 3: Write minimal implementation**

Add a table-extraction pass before the generic tag-stripping in `src/tools.rs`:

```rust
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
        let Some(tag_end) = rest[start..].find('>') else { break };
        let body_start = start + tag_end + 1;
        let Some(close_rel) = rest[body_start..].find(&close) else { break };
        let body_end = body_start + close_rel;
        out.push(rest[body_start..body_end].to_string());
        rest = &rest[body_end + close.len()..];
    }
    out
}
```

Then wire it into `strip_html_to_text`:

```rust
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
```

Note: pipe-table lines contain internal spaces around `|` that must survive the final `.map(str::trim)` — `str::trim` only trims the line's ends, so `| A | 91 |` is untouched. Good.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS, including the existing `strip_html_to_text_drops_tags_scripts_styles_and_blank_lines` test (no tables in that fixture, so `render_tables_as_markdown` is a no-op passthrough).

- [ ] **Step 5: Commit**

```bash
git add src/tools.rs
git commit -m "Render scraped HTML tables as markdown pipe tables

strip_html_to_text flattened <table> content into run-on prose, destroying
benchmark/pricing/spec data researchers actually need. Extract each
top-level table and render it as a GitHub-style pipe table before the
generic tag-stripping pass."
```

---

### Task 3: YouTube transcript fetching

**Files:**
- Modify: `src/tools.rs` (`fetch_cached`/`fetch_url_text` call site, around line 162)
- Test: `src/tools.rs`

**Interfaces:**
- Consumes: `client: &reqwest::Client` (existing field on `ToolBox`).
- Produces: `is_youtube_url(url: &str) -> bool` and `parse_caption_track_url(watch_page_html: &str) -> Option<String>` — pure, unit-tested. `fetch_youtube_transcript` is the async network fn (manually tested).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn is_youtube_url_matches_watch_and_short_links() {
    assert!(is_youtube_url("https://www.youtube.com/watch?v=abc123"));
    assert!(is_youtube_url("https://youtu.be/abc123"));
    assert!(!is_youtube_url("https://example.com/watch?v=abc123"));
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test is_youtube_url --lib parse_caption_track_url --lib strip_timedtext_xml --lib`
Expected: FAIL — functions not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Whether `url` points at a YouTube watch page (long or short form).
fn is_youtube_url(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    matches!(u.host_str(), Some(h) if h.ends_with("youtube.com") || h == "youtu.be")
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
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
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
```

Wire it into `fetch_cached` (the private helper `fetch_url_text` is only called from the cache-miss branch — find that call and branch on `is_youtube_url` first):

```rust
async fn fetch_cached(&self, url: &str, force_fresh: bool) -> anyhow::Result<String> {
    if !force_fresh
        && let Some(db_path) = &self.web_cache_db
        && let Ok(conn) = rusqlite::Connection::open(db_path)
        && let Ok(Some(text)) = cache_get(&conn, url)
    {
        return Ok(text);
    }
    let text = if is_youtube_url(url) {
        fetch_youtube_transcript(&self.client, url).await?
    } else {
        fetch_url_text(&self.client, url).await?
    };
    if let Some(db_path) = &self.web_cache_db
        && let Ok(conn) = rusqlite::Connection::open(db_path)
    {
        let _ = cache_put(&conn, url, &text);
    }
    Ok(text)
}
```

This is a rewrite of the existing method body — read the current `fetch_cached` at `src/tools.rs:162` first and match its exact existing cache-hit/cache-put structure (title handling, if any) rather than assuming the above verbatim; keep whatever else it already does and only add the `is_youtube_url` branch around the single `fetch_url_text(...)` call.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tools.rs
git commit -m "Fetch YouTube transcripts instead of scraping the watch page

fetch_url on a YouTube link previously returned the mostly-JS watch page
shell. Detect youtube.com/youtu.be URLs, pull the first caption track via
the keyless timedtext endpoint, and return the joined cue text — unlocks
talks/interviews/conference sessions as research sources."
```

---

### Task 4: HN/Reddit discussion mining tool

**Files:**
- Modify: `src/tools.rs` (new `discussion_search` tool: struct types, async fn, `defs()`, `run()` dispatch, research-only allowlist)
- Test: `src/tools.rs`

**Interfaces:**
- Consumes: `self.client`, `self.web_cache_db` (cache the raw query→results text same as `fetch_cached`, keyed by a synthetic `discussion://<query>` URL so it reuses the existing `cache_get`/`cache_put` free functions unchanged).
- Produces: `format_discussion_hits(hn: &[DiscussionHit], reddit: &[DiscussionHit]) -> String` — pure, unit-tested. Adds `"discussion_search"` to the research-only allowlist alongside `"web_search" | "fetch_url" | "academic_search"`.

- [ ] **Step 1: Write the failing test**

```rust
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
    assert!(text.contains("[2] What do you think of Rust 1.90?"), "{text:?}");
    assert!(text.contains("r/rust, 245 upvotes"), "{text:?}");
}

#[test]
fn format_discussion_hits_empty_both_yields_empty_string() {
    assert_eq!(format_discussion_hits(&[], &[]), "");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test format_discussion_hits --lib`
Expected: FAIL — `DiscussionHit`/`format_discussion_hits` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
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
            let url = h.url.unwrap_or_else(|| format!("https://news.ycombinator.com/item?id={}", h.object_id));
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

async fn reddit_search(client: &reqwest::Client, query: &str) -> anyhow::Result<Vec<DiscussionHit>> {
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
```

Add the tool schema in `defs()` (find the existing `academic_search` `ToolDef` block and add a sibling, gated the same way — check the surrounding `if research_only`/always-visible pattern at `src/tools.rs:186` before placing it) and the `run()` dispatch arm:

```rust
"discussion_search" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let query = v.get("query").and_then(|q| q.as_str()).unwrap_or_default().to_string();
    let status = "Searching HN and Reddit…".to_string();
    let result = discussion_search(&self.client, &query).await;
    (result, status)
}
```

And extend the two research-only guards:

```rust
if self.research_only && !matches!(name, "web_search" | "fetch_url" | "academic_search" | "discussion_search") {
```

```rust
defs.retain(|d| matches!(d.name.as_str(), "web_search" | "fetch_url" | "academic_search" | "discussion_search"));
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tools.rs
git commit -m "Add discussion_search tool: Hacker News + Reddit mining

New research-only tool hitting HN's Algolia search API and Reddit's public
search.json, both keyless. Gives searcher agents access to discussion/
sentiment sources alongside web_search and academic_search."
```

---

### Task 5: `session_sources.flag` column + pin/discard keybinds

**Files:**
- Modify: `src/db.rs` (migration, `add_session_sources`/new query fns)
- Modify: `src/tools.rs` (`ToolBox` reads session-discarded domains)
- Modify: `src/events.rs` (`x`/`p` keybinds)
- Modify: `src/app/chat.rs` or new `src/app/sources.rs` (the two new App methods)
- Test: `src/db.rs`, `src/app/*`

**Interfaces:**
- Consumes: `crate::selection::HistorySel::owner_at_selection_start`, `crate::citations::{citation_number_in, parse_citations}` (all existing, same pattern as `open_citation_under_selection`).
- Produces: `Db::set_source_flag(session_id, url_norm, flag: Option<&str>) -> Result<()>`; free fn `discarded_domains(conn: &Connection, session_id: &str) -> Result<Vec<String>>`; free fn `pinned_urls(conn: &Connection, session_id: &str) -> Result<Vec<String>>`.

- [ ] **Step 1: Write the failing test**

Add to `src/db.rs` test module:

```rust
#[test]
fn set_source_flag_pins_and_discards_then_clears() {
    let db = test_db();
    let session_id = "sess-1";
    add_session_sources(&db.conn, session_id, &["https://a.example/x".to_string()]).unwrap();
    db.set_source_flag(session_id, "https://a.example/x", Some("pinned")).unwrap();
    assert_eq!(pinned_urls(&db.conn, session_id).unwrap(), vec!["https://a.example/x".to_string()]);
    assert!(discarded_domains(&db.conn, session_id).unwrap().is_empty());

    db.set_source_flag(session_id, "https://a.example/x", Some("discarded")).unwrap();
    assert!(pinned_urls(&db.conn, session_id).unwrap().is_empty());
    assert_eq!(discarded_domains(&db.conn, session_id).unwrap(), vec!["a.example".to_string()]);

    db.set_source_flag(session_id, "https://a.example/x", None).unwrap();
    assert!(discarded_domains(&db.conn, session_id).unwrap().is_empty());
}
```

Check `src/db.rs`'s test module for its existing `test_db()` helper name before using it — reuse whatever fixture helper the file already has (grep `fn test_db` in `src/db.rs`); if it's named differently, use that name instead.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test set_source_flag --lib`
Expected: FAIL — methods not defined.

- [ ] **Step 3: Write minimal implementation**

Add to the migration's `ALTER TABLE` list in `src/db.rs::migrate()`:

```rust
"ALTER TABLE session_sources ADD COLUMN flag TEXT",
```

Add methods/free functions:

```rust
impl Db {
    /// Pin (`Some("pinned")`), discard (`Some("discarded")`), or clear
    /// (`None`) a session source's flag. `url_norm` must already exist in
    /// `session_sources` for this session (a no-op UPDATE otherwise — the
    /// row is created by `add_session_sources` when a source is first
    /// cited, not here).
    pub fn set_source_flag(&self, session_id: &str, url_norm: &str, flag: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE session_sources SET flag = ?3 WHERE session_id = ?1 AND url_norm = ?2",
            (session_id, url_norm, flag),
        )?;
        Ok(())
    }
}

/// URLs pinned in a session's source bundle — the Synthesizer/Writer
/// prompts list these as "prioritize these sources".
pub fn pinned_urls(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT url_norm FROM session_sources WHERE session_id = ?1 AND flag = 'pinned'",
    )?;
    let rows = stmt.query_map([session_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Distinct hostnames discarded in a session — excluded from later searcher
/// rounds the same way the global `blocked_domains` setting is.
pub fn discarded_domains(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT url_norm FROM session_sources WHERE session_id = ?1 AND flag = 'discarded'",
    )?;
    let rows: Vec<String> = stmt
        .query_map([session_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut hosts: Vec<String> = rows
        .iter()
        .filter_map(|u| reqwest::Url::parse(u).ok().and_then(|p| p.host_str().map(str::to_string)))
        .collect();
    hosts.sort();
    hosts.dedup();
    Ok(hosts)
}
```

`reqwest` is already a `db.rs`-reachable dependency (used elsewhere in the crate); if `db.rs` doesn't already `use reqwest` anywhere, add a fully-qualified `reqwest::Url::parse` call as above rather than a new `use` (matches the file's existing style of qualifying rarely-used externs inline).

Now the `ToolBox` side: add a `session_discarded_domains: Vec<String>` populated by `with_research_session` — read it once via `discarded_domains` when the toolbox is built. Simplest: extend `with_research_session` to also query and store the list at construction time:

```rust
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
```

This reuses the existing `blocked_domains` exclusion path (`web_search`'s exclude-list and any future fetch guard) with zero new plumbing in `run()`/`defs()`.

Add the keybinds in `src/events.rs`, next to the existing `'o'` citation-open binding:

```rust
// 'p' pins, 'x' discards the [n] source under the current selection —
// same selection→citation resolution as 'o'.
KeyCode::Char('p') if !ctrl && !shift && app.sel.selected_text().is_some() => {
    app.flag_source_under_selection(Some("pinned"));
}
KeyCode::Char('x') if !ctrl && !shift && app.sel.selected_text().is_some() => {
    app.flag_source_under_selection(Some("discarded"));
}
```

Place these near the existing `'o'` arm (`src/events.rs:289`) so they share its guard ordering; check there's no existing `'x'`/`'p'` binding earlier in `handle_normal` that would shadow these (grep confirmed only `Ctrl+x` cut-selection exists, which requires `ctrl` — the `!ctrl` guard here avoids collision).

Add `App::flag_source_under_selection` in `src/app/chat.rs`, right after `open_citation_under_selection`:

```rust
/// Pin or discard the `[n]` source under the current history selection
/// (same selection→citation resolution as `open_citation_under_selection`).
/// Flags are keyed by the message's normalized URL, session-scoped.
pub(crate) fn flag_source_under_selection(&mut self, flag: Option<&str>) {
    let Some(selected) = self.sel.selected_text() else {
        self.status = "select a [n] citation, then press p/x".to_string();
        return;
    };
    let Some(n) = crate::citations::citation_number_in(&selected) else {
        self.status = "no [n] citation in the current selection".to_string();
        return;
    };
    let Some(msg) = self.sel.owner_at_selection_start().and_then(|i| self.messages.get(i)) else {
        self.status = "no [n] citation in the current selection".to_string();
        return;
    };
    let citations = crate::citations::parse_citations(&msg.content);
    let Some((_, url)) = citations.iter().find(|(num, _)| *num == n) else {
        self.status = format!("no source [{n}] in this message");
        return;
    };
    let Some(session) = &self.session else {
        self.status = "no active session".to_string();
        return;
    };
    let url_norm = crate::tools::normalize_url(url);
    let verb = if flag.is_some() { "pinned" } else { "cleared" };
    match self.db.set_source_flag(&session.id, &url_norm, flag) {
        Ok(()) => {
            self.status = format!("{verb} [{n}]: {url}");
            self.refresh_toolbox();
        }
        Err(e) => self.status = format!("flag failed: {e}"),
    }
}
```

`crate::tools::normalize_url` is currently `pub(crate)` — confirm it's visible from `app/chat.rs` (same crate, already `pub(crate)`, so yes). `self.refresh_toolbox()` picks up the new discarded-domain list on the next tool call by reconstructing the toolbox with `with_research_session` re-run — check `refresh_toolbox`'s body at `src/app/mod.rs:902` calls `with_research_session` when `is_research_session()`, so this is already correct with no further change needed there.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/tools.rs src/events.rs src/app/chat.rs
git commit -m "Add pin/discard for research sources (p/x keybinds)

session_sources gains a flag column. 'x' on a selected [n] citation adds
its domain to a session-scoped blocklist (merged into the toolbox's
existing blocked_domains exclusion on refresh_toolbox); 'p' pins it for
future use by the synthesis/writer prompts (wired in a later task)."
```

---

### Task 6: Pinned sources surfaced to Synthesizer/Writer prompts

**Files:**
- Modify: `src/app/research.rs` (`synthesizer_messages`, `writer_messages`, `run_research_inner`)
- Test: `src/app/research.rs`

**Interfaces:**
- Consumes: `crate::db::pinned_urls(conn, session_id)` (Task 5).
- Produces: `synthesizer_messages`/`writer_messages` gain a `pinned: &[String]` parameter; both call sites in `run_research_inner` pass the list read once at pipeline start.

- [ ] **Step 1: Write the failing test**

Find the existing `synthesizer_messages` unit test in `src/app/research.rs` (grep `fn synthesizer_messages` for its test) and add:

```rust
#[test]
fn synthesizer_messages_lists_pinned_sources_when_present() {
    let msgs = synthesizer_messages("topic", &["finding one".to_string()], &["https://a.example".to_string()]);
    let user = msgs.iter().find(|m| m.role == "user").unwrap();
    assert!(user.content.contains("https://a.example"), "{}", user.content);
    assert!(user.content.to_lowercase().contains("prioritize"), "{}", user.content);
}

#[test]
fn synthesizer_messages_omits_pinned_section_when_empty() {
    let msgs = synthesizer_messages("topic", &["finding one".to_string()], &[]);
    let user = msgs.iter().find(|m| m.role == "user").unwrap();
    assert!(!user.content.to_lowercase().contains("prioritize"), "{}", user.content);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test synthesizer_messages --lib`
Expected: FAIL — wrong number of arguments (current signature is 2 params, test passes 3).

- [ ] **Step 3: Write minimal implementation**

Read the current `synthesizer_messages` and `writer_messages` bodies in `src/app/research.rs` first (their exact existing message-building code), then add a third parameter to each and prepend a pinned-sources line to the user message when non-empty:

```rust
fn synthesizer_messages(topic: &str, findings: &[String], pinned: &[String]) -> Vec<ChatMessage> {
    let mut user = format!("Topic: {topic}\n\n");
    if !pinned.is_empty() {
        user.push_str(&format!(
            "Prioritize these pinned sources in the synthesis if their content is present in the findings below:\n{}\n\n",
            pinned.join("\n")
        ));
    }
    user.push_str(&findings.join("\n\n---\n\n"));
    vec![ChatMessage::text("system", SYNTHESIZER_PROMPT), ChatMessage::text("user", &user)]
}
```

(Match this against the file's actual current body — keep whatever findings-joining/formatting it already does; only add the `pinned` prefix block and the new parameter. Apply the identical `pinned` prefix pattern to `writer_messages`.)

Update `run_research_inner` to read pinned URLs once (right after the plan-approval gate, before the first `run_searchers` call) and thread them through both synthesis call sites:

```rust
let pinned = rusqlite::Connection::open(db_path)
    .ok()
    .and_then(|conn| crate::db::pinned_urls(&conn, &ids.0).ok())
    .unwrap_or_default();
```

then change both `synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings))` call sites to `synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings), &pinned)`, and the `writer_messages(topic, &verified)` call to `writer_messages(topic, &verified, &pinned)`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/research.rs
git commit -m "Feed pinned sources into synthesis and final-writer prompts

Sources pinned via the p keybind (Task 5) are now read once per research
run and passed to both synthesizer_messages and writer_messages, which
prepend a 'prioritize these' line when the pinned list is non-empty."
```

---

### Task 7: Claim confidence tags from the Verifier

**Files:**
- Modify: `src/app/research.rs` (`VERIFIER_PROMPT`)
- Modify: `src/citations.rs` (new pure styling helper)
- Modify: `src/ui/history.rs` (wire the new styling into both assistant render paths)
- Test: `src/citations.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `crate::citations::style_confidence_tags(lines: Vec<Line<'static>>) -> Vec<Line<'static>>` — pure, unit-tested; called alongside the existing `style_citations` call sites in `src/ui/history.rs`.

- [ ] **Step 1: Write the failing test**

Add to `src/citations.rs` test module:

```rust
#[test]
fn style_confidence_tags_dims_low_and_med_tags() {
    use ratatui::text::Line;
    let lines = vec![Line::from("Some claim [1] \u{2039}low\u{203a}. Another [2] \u{2039}med\u{203a}.")];
    let styled = style_confidence_tags(lines);
    let tag_spans: Vec<_> = styled[0].spans.iter().filter(|s| s.content.contains('\u{2039}')).collect();
    assert_eq!(tag_spans.len(), 2);
    assert!(tag_spans.iter().all(|s| s.style.add_modifier.contains(ratatui::style::Modifier::DIM)));
}

#[test]
fn style_confidence_tags_leaves_lines_without_tags_untouched() {
    use ratatui::text::Line;
    let lines = vec![Line::from("Plain claim, high confidence, no tag.")];
    let styled = style_confidence_tags(lines.clone());
    assert_eq!(styled[0].spans.len(), lines[0].spans.len());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test style_confidence_tags --lib`
Expected: FAIL — function not defined.

- [ ] **Step 3: Write minimal implementation**

The tag delimiters are `‹` (U+2039) and `›` (U+203A) — chosen because they never occur in normal report prose, so no escaping/ambiguity vs. markdown or the `[n]` citation syntax. Add to `src/citations.rs`, following the same split-span pattern as `split_citation_span`:

```rust
/// Re-style every `‹low›`/`‹med›` confidence tag the Verifier stage emits
/// (see `VERIFIER_PROMPT` in `app/research.rs`) with a dim modifier;
/// everything else keeps its existing style. High confidence is the
/// default (unmarked), so there's nothing to style for it.
pub(crate) fn style_confidence_tags(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let alignment = line.alignment;
            let style = line.style;
            let mut spans = Vec::new();
            for span in line.spans {
                spans.extend(split_confidence_span(span));
            }
            let mut out = Line::from(spans);
            out.alignment = alignment;
            out.style = style;
            out
        })
        .collect()
}

fn split_confidence_span(span: Span<'static>) -> Vec<Span<'static>> {
    let text = span.content.to_string();
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('\u{2039}') {
        if start > 0 {
            out.push(Span::styled(rest[..start].to_string(), span.style));
        }
        let Some(end_rel) = rest[start..].find('\u{203a}') else {
            out.push(Span::styled(rest[start..].to_string(), span.style));
            return out;
        };
        let end = start + end_rel + '\u{203a}'.len_utf8();
        out.push(Span::styled(rest[start..end].to_string(), span.style.add_modifier(Modifier::DIM)));
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        out.push(Span::styled(rest.to_string(), span.style));
    }
    out
}
```

Wire it into `src/ui/history.rs`'s two `style_citations(...)` call sites (lines 283 and 325), applying it right after:

```rust
rendered.lines = crate::citations::style_citations(rendered.lines, theme.accent);
rendered.lines = crate::citations::style_confidence_tags(rendered.lines);
```

(and the same for the second call site at line 325, using `app.theme.accent`).

Update `VERIFIER_PROMPT` in `src/app/research.rs`:

```rust
const VERIFIER_PROMPT: &str = "You are the verifier stage. Given the topic, the gathered source findings (with their citations), and a draft report, check every factual claim in the draft against the source findings. Rewrite the draft unchanged except: (1) remove or mark with '⚠ unverifiable:' any claim not actually supported by the gathered findings; (2) immediately after a claim's citations, judge its confidence from citation count and cross-source agreement and, only for low or medium confidence, append the tag ‹low› or ‹med› right after the citation (high confidence is the default and stays untagged — do not tag it). Output the corrected draft in markdown, nothing else.";
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/citations.rs src/ui/history.rs src/app/research.rs
git commit -m "Render Verifier confidence tags (‹low›/‹med›) dimmed

The Verifier now tags low/medium-confidence claims inline; style_confidence_tags
dims them in the transcript the same way style_citations colors [n] markers.
High confidence stays the unmarked default — no visual noise for the common case."
```

---

### Task 8: Quote checking via a cache-only Verifier toolbox

**Files:**
- Modify: `src/tools.rs` (`ToolBox`: `cache_only: bool` field, `fetch_cached` gate)
- Modify: `src/app/research.rs` (Verifier stage runs with tools + cache-only toolbox)
- Test: `src/tools.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `ToolBox::cache_only(mut self) -> Self` builder method (mirrors `with_research_session`). `fetch_cached` returns `Ok("[not cached]".to_string())` on a cache miss when `cache_only` is set, instead of hitting the network.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn cache_only_toolbox_returns_not_cached_marker_on_miss_without_network() {
    let dir = std::env::temp_dir().join(format!("nexus-cacheonly-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("db.sqlite3");
    // Any valid sqlite file works — fetch_cached only needs cache_get/cache_put's table, migrated on open elsewhere in real use; here confirm the miss path never reaches the network.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE web_cache (url_norm TEXT PRIMARY KEY, url TEXT NOT NULL, title TEXT, text TEXT NOT NULL, fetched_at TEXT NOT NULL);",
    ).unwrap();
    drop(conn);

    let tb = ToolBox::research(
        std::path::PathBuf::from("/nonexistent"),
        None,
        None,
        "auto".to_string(),
        Vec::new(),
        Some(db_path),
    )
    .cache_only();
    let (result, _status) = tb.run("fetch_url", r#"{"url":"https://never-fetched.example/page"}"#).await;
    assert!(result.contains("not cached"), "{result}");
}
```

Check `ToolBox::research`'s exact current parameter list at `src/tools.rs:85` before writing this call — match the real signature (skills_dir, searxng_url, langsearch_key, search_provider, blocked_domains, web_cache_db, in whatever order it's actually declared) rather than the placeholder order shown here; this is illustrative of intent, not the literal call.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test cache_only_toolbox --lib`
Expected: FAIL — `cache_only` method not defined.

- [ ] **Step 3: Write minimal implementation**

Add the field and builder to `ToolBox`:

```rust
pub struct ToolBox {
    // ...existing fields...
    /// When true, `fetch_cached` never hits the network on a cache miss —
    /// used for the Verifier stage's quote-checking pass, which must only
    /// ever see pages the searchers actually gathered, never fresh fetches.
    cache_only: bool,
}
```

Initialize `cache_only: false` in both `new` and `research` constructors' struct-literal bodies. Add the builder next to `with_research_session`:

```rust
/// Restrict `fetch_url` to serving from `web_cache` only — a cache miss
/// returns `[not cached]` instead of fetching. Used for the Verifier's
/// quote-checking pass (Task 8).
pub fn cache_only(mut self) -> Self {
    self.cache_only = true;
    self
}
```

Modify `fetch_cached` to check the flag on a miss (read the current method body at `src/tools.rs:162` and insert the check where the cache-miss branch currently falls through to `fetch_url_text`):

```rust
async fn fetch_cached(&self, url: &str, force_fresh: bool) -> anyhow::Result<String> {
    if !force_fresh
        && let Some(db_path) = &self.web_cache_db
        && let Ok(conn) = rusqlite::Connection::open(db_path)
        && let Ok(Some(text)) = cache_get(&conn, url)
    {
        return Ok(text);
    }
    if self.cache_only {
        return Ok("[not cached]".to_string());
    }
    // ...existing fetch + cache_put logic unchanged...
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tools.rs
git commit -m "Add ToolBox::cache_only for the Verifier's quote-check pass

A cache-only toolbox never reaches the network on a fetch_url miss —
returns [not cached] instead. Lets the Verifier stage look up direct
quotes against exactly what searchers already gathered, without risking
a fresh (possibly different) fetch."
```

Now wire the Verifier to run with tools. Find the current Verifier call site in `run_research_inner`:

```rust
send_stage(tx, ids, "verifying", "");
let verified = complete_text(provider, research_model, verifier_messages(topic, &draft, &findings)).await?;
```

`complete_text` is a non-tool single-shot `Provider::complete` wrapper (confirm by reading its definition in `research.rs`). Quote-checking needs the tool loop (`stream_chat`), same shape as `run_searcher`. Add a small dedicated runner:

```rust
/// Run the Verifier stage with a cache-only toolbox so it can check direct
/// quotes against exactly the pages searchers already gathered (never a
/// fresh fetch). Falls back to the given `draft` unchanged if the stream
/// errors before producing any text — verification failing must never
/// blank out an otherwise-good report.
async fn verify_with_quote_check(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    cache_only_toolbox: Arc<ToolBox>,
) -> String {
    let tools = cache_only_toolbox.defs();
    let (mut rx, _abort) =
        provider.stream_chat(model.to_string(), messages, ChatParams::default(), tools, cache_only_toolbox, RESEARCH_SEARCHER_MAX_ITERS);
    let mut buf = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Done | StreamEvent::Error(_) => break,
            _ => {}
        }
    }
    buf
}
```

Update `VERIFIER_PROMPT` to mention the tool:

```rust
const VERIFIER_PROMPT: &str = "... [existing text] ... You have a fetch_url tool restricted to already-cached pages: use it to check any direct quote in the draft against the cached source text, and mark a quote that doesn't actually match with '‹unverified quote›' immediately after it.";
```

(Append this sentence to whatever the Task 7 version of `VERIFIER_PROMPT` currently is — don't replace the confidence-tagging instructions, add to them.)

Change the call site in `run_research_inner`:

```rust
send_stage(tx, ids, "verifying", "");
let verify_toolbox = Arc::new((*toolbox).research_toolbox_clone_cache_only()); // see note below
let verified = verify_with_quote_check(provider, research_model, verifier_messages(topic, &draft, &findings), verify_toolbox).await;
```

`ToolBox` has no `Clone` today (its fields are plain, but check — if it doesn't derive `Clone`, add `#[derive(Clone)]` to the struct only if every field is `Clone`; `reqwest::Client` is `Clone` (internally `Arc`-backed), `Option<FilesCtx>`/`Option<AppsCtx>` need `Clone` too — check those structs derive it, and add `#[derive(Clone)]` to `FilesCtx`/`AppsCtx`/`ToolBox` together if not already present). If cloning the whole toolbox is awkward given its current derives, the simpler alternative — and the one to prefer if the above turns out non-trivial — is to build a **fresh** cache-only toolbox at the call site instead of cloning:

```rust
let verify_toolbox = Arc::new(
    ToolBox::research(
        toolbox.skills_dir.clone(),
        None,
        None,
        "auto".to_string(),
        Vec::new(),
        toolbox.web_cache_db_for_verify(), // expose via a small pub(crate) accessor, or store db_path separately in run_research_inner's own scope (it's already a parameter: `db_path`)
    )
    .cache_only(),
);
```

Since `run_research_inner` already has `db_path: &std::path::Path` in scope, the fresh-toolbox construction is simpler and avoids any `Clone` question:

```rust
let verify_toolbox = Arc::new(ToolBox::research(
    toolbox.skills_dir.clone(),
    None,
    None,
    "auto".to_string(),
    Vec::new(),
    Some(db_path.to_path_buf()),
).cache_only());
```

`toolbox.skills_dir` must be a visible field — confirm it's `pub` on `ToolBox` (it is, per the struct definition read in Task setup). Use this fresh-construction approach; skip the `Clone`-derive path above entirely (kept in the plan only as the reasoning trail — implement the fresh-construction version).

- [ ] **Step 6: Write the failing test for the wiring**

```rust
// in src/app/research.rs tests
#[tokio::test]
async fn verify_with_quote_check_falls_back_to_empty_on_stream_error() {
    // This exercises only the "never panics, returns a String" contract;
    // a real provider call is exercised manually per this file's convention.
}
```

Given the header comment's stated convention (network-calling async orchestration is exercised manually, not unit tested), skip a unit test for `verify_with_quote_check` itself — it's in the same category as `run_searcher`, which also has no direct unit test. Do add one pure test for the prompt-text change:

```rust
#[test]
fn verifier_prompt_mentions_quote_checking() {
    assert!(VERIFIER_PROMPT.to_lowercase().contains("quote"));
}
```

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test verifier_prompt_mentions_quote_checking --lib`
Expected: PASS.

- [ ] **Step 8: Full build + test**

Run: `cargo build && cargo test --lib`
Expected: 0 warnings, all tests PASS.

- [ ] **Step 9: Commit**

```bash
git add src/app/research.rs
git commit -m "Run the Verifier stage with a cache-only fetch_url tool

Verifier previously ran as a single complete() call with no tools. It now
runs through the tool loop with a fresh cache-only ToolBox, so it can look
up direct quotes against exactly what searchers cached and flag mismatches
inline with ‹unverified quote›."
```

---

### Task 9: `/steer` mid-flight research steering

**Files:**
- Modify: `src/app/mod.rs` (`App` field, `run_command`)
- Modify: `src/app/research.rs` (queue plumbing, round-boundary drain)
- Modify: `src/input.rs` (`COMMANDS` entry)
- Test: `src/app/research.rs`

**Interfaces:**
- Consumes: existing `mpsc::unbounded_channel` pattern (same as `research_rx`).
- Produces: `pub(crate) fn drain_steers(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String>` — pure-ish (only reads a channel, but no I/O; unit-tested by sending then draining in a `#[tokio::test]`), called at each round boundary in `run_research_inner`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn drain_steers_collects_all_queued_without_blocking() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tx.send("look into X".to_string()).unwrap();
    tx.send("also Y".to_string()).unwrap();
    let drained = drain_steers(&mut rx).await;
    assert_eq!(drained, vec!["look into X".to_string(), "also Y".to_string()]);
    // Second call with nothing queued returns empty immediately (no hang).
    let empty = drain_steers(&mut rx).await;
    assert!(empty.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test drain_steers --lib`
Expected: FAIL — function not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Every steer instruction queued since the last drain, without blocking —
/// `try_recv` until the channel is empty. Called at each round boundary so
/// a user's mid-flight `/steer` gets picked up as an extra searcher round.
pub(crate) async fn drain_steers(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(s) = rx.try_recv() {
        out.push(s);
    }
    out
}
```

Add the channel to `App` (`src/app/mod.rs`, next to `research_plan_gate`):

```rust
/// Queues `/steer` instructions into the currently running research job's
/// round-boundary check. `None` when no research job is running.
pub(crate) research_steer_tx: Option<mpsc::UnboundedSender<String>>,
```

Initialize `research_steer_tx: None` in `App::new`'s struct literal (same spot as `research_plan_gate: None`).

Add `/steer` to `COMMANDS` in `src/input.rs`, right after the `"web"` entry:

```rust
Command { name: "steer", desc: "inject a research instruction mid-flight", aliases: &["nudge"] },
```

Add to `run_command`'s match in `src/app/mod.rs`, next to `"web" => self.toggle_web_mode()`:

```rust
"steer" => self.steer_research(cmd[token.len()..].trim()),
```

Add the method in `src/app/research.rs`:

```rust
/// `/steer <text>`: queue an extra instruction for the running research
/// job, picked up at the next round boundary. No-op with a status message
/// if no research job is running.
pub(crate) fn steer_research(&mut self, text: &str) {
    if text.is_empty() {
        self.status = "usage: /steer <what to also look into>".to_string();
        return;
    }
    match &self.research_steer_tx {
        Some(tx) if tx.send(text.to_string()).is_ok() => {
            self.status = format!("queued steer: {text}");
        }
        _ => self.status = "no research job is running".to_string(),
    }
}
```

Wire the channel through `start_research_with_gate` (create it alongside the existing gate/research channel setup — find where `research_rx`/`gate_tx` are created and add):

```rust
let (steer_tx, steer_rx) = mpsc::unbounded_channel();
self.research_steer_tx = Some(steer_tx);
```

Pass `steer_rx` into `run_research`/`run_research_inner` (new parameter `mut steer_rx: mpsc::UnboundedReceiver<String>`), and at each round boundary — right before the round-2 gap-round `run_searchers` call, and add a matching check after round 1 too — drain and fold into extra searcher rounds:

```rust
let steers = drain_steers(&mut steer_rx).await;
if !steers.is_empty() {
    for s in &steers {
        send_stage(tx, ids, "steer", s.clone());
    }
    let steered = run_searchers(provider, research_model, toolbox, &steers, tx, ids, round_counter).await;
    persist_session_sources(db_path, &ids.0, &steered);
    findings.extend(steered);
}
```

Place one such block right after the round-1 `run_searchers` call (before synthesis) and another right after the gap round's `run_searchers` (before re-synthesis) — `round_counter` is illustrative; reuse whatever round-number literal/variable is already in scope at each insertion point (`1`, `2`, etc., matching the existing `round` arguments already passed to `run_searchers` at each call site) rather than introducing a new counter variable.

Clear `self.research_steer_tx = None;` in the same place `research_rx`/`research_plan_gate` get cleared when a job finishes (`on_research_done`, at the `Done(...)` match arms).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs src/app/research.rs src/input.rs
git commit -m "Add /steer for mid-flight research instructions

/steer <text> queues an instruction while a research job runs. Picked up
at each round boundary (after round 1, after the gap round) as an extra
searcher sub-question, folded into findings before (re-)synthesis."
```

---

### Task 10: `watches` table + `/watch` command + on-open due-check

**Files:**
- Modify: `src/db.rs` (new table, CRUD methods)
- Create: `src/app/watches.rs` (new module: picker state + due-check + diff)
- Modify: `src/app/mod.rs` (`mod watches;`, `Popup::Watch` variant, `App` fields, startup hook, `run_command`)
- Modify: `src/input.rs` (`COMMANDS` entry)
- Test: `src/db.rs`, `src/app/watches.rs`

**Interfaces:**
- Consumes: `App::start_research_with_gate` (existing) to kick off a watch's re-run.
- Produces: `Db::create_watch`, `Db::list_watches`, `Db::delete_watch`, `Db::touch_watch(id, now)`, all in `db.rs`. `pub(crate) fn due_watches(watches: &[Watch], now: DateTime<Utc>) -> Vec<Watch>` — pure, unit-tested — in `watches.rs`.

- [ ] **Step 1: Write the failing test**

Add to `src/db.rs` test module:

```rust
#[test]
fn create_list_touch_delete_watch_roundtrip() {
    let db = test_db();
    let id = db.create_watch("space-1", "rust async runtimes", 24, "sess-1").unwrap();
    let watches = db.list_watches("space-1").unwrap();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].topic, "rust async runtimes");
    assert_eq!(watches[0].interval_hours, 24);
    assert!(watches[0].last_run_at.is_none());

    db.touch_watch(&id, "2026-07-07T00:00:00+00:00").unwrap();
    let watches = db.list_watches("space-1").unwrap();
    assert_eq!(watches[0].last_run_at.as_deref(), Some("2026-07-07T00:00:00+00:00"));

    db.delete_watch(&id).unwrap();
    assert!(db.list_watches("space-1").unwrap().is_empty());
}
```

Add to `src/app/watches.rs` (new file):

```rust
//! Standing research: `watches` re-run their topic's research on an
//! interval, with no daemon — `due_watches` is checked once on app startup.

use chrono::{DateTime, Utc};

use crate::db::Watch;

/// Watches whose interval has elapsed since their last run (or that have
/// never run) as of `now`.
pub(crate) fn due_watches(watches: &[Watch], now: DateTime<Utc>) -> Vec<Watch> {
    watches
        .iter()
        .filter(|w| match &w.last_run_at {
            None => true,
            Some(t) => DateTime::parse_from_rfc3339(t)
                .map(|last| now.signed_duration_since(last) >= chrono::Duration::hours(w.interval_hours))
                .unwrap_or(true),
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(topic: &str, interval_hours: i64, last_run_at: Option<&str>) -> Watch {
        Watch {
            id: "w1".to_string(),
            space_id: "space-1".to_string(),
            topic: topic.to_string(),
            interval_hours,
            session_id: "sess-1".to_string(),
            last_run_at: last_run_at.map(str::to_string),
        }
    }

    #[test]
    fn never_run_watch_is_always_due() {
        let w = watch("topic", 24, None);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T00:00:00+00:00").unwrap().to_utc();
        assert_eq!(due_watches(&[w], now).len(), 1);
    }

    #[test]
    fn watch_run_recently_is_not_due() {
        let w = watch("topic", 24, Some("2026-07-07T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T05:00:00+00:00").unwrap().to_utc();
        assert!(due_watches(&[w], now).is_empty());
    }

    #[test]
    fn watch_past_its_interval_is_due() {
        let w = watch("topic", 24, Some("2026-07-06T00:00:00+00:00"));
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-07T01:00:00+00:00").unwrap().to_utc();
        assert_eq!(due_watches(&[w], now).len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test create_list_touch_delete_watch_roundtrip --lib` and `cargo test --lib due_watches`
Expected: FAIL — `Db::create_watch` etc. and the `watches` module don't exist yet.

- [ ] **Step 3: Write minimal implementation**

Add to `src/db.rs::migrate()`'s `CREATE TABLE` block:

```rust
CREATE TABLE IF NOT EXISTS watches (
    id             TEXT PRIMARY KEY,
    space_id       TEXT NOT NULL,
    topic          TEXT NOT NULL,
    interval_hours INTEGER NOT NULL,
    session_id     TEXT NOT NULL,
    last_run_at    TEXT
);
```

Add the `Watch` struct and CRUD methods to `src/db.rs` (near `Session`):

```rust
#[derive(Debug, Clone)]
pub struct Watch {
    pub id: String,
    pub space_id: String,
    pub topic: String,
    pub interval_hours: i64,
    pub session_id: String,
    pub last_run_at: Option<String>,
}

impl Db {
    pub fn create_watch(&self, space_id: &str, topic: &str, interval_hours: i64, session_id: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO watches (id, space_id, topic, interval_hours, session_id, last_run_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            (&id, space_id, topic, interval_hours, session_id),
        )?;
        Ok(id)
    }

    pub fn list_watches(&self, space_id: &str) -> Result<Vec<Watch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, space_id, topic, interval_hours, session_id, last_run_at
             FROM watches WHERE space_id = ?1 ORDER BY topic",
        )?;
        let rows = stmt.query_map([space_id], |r| {
            Ok(Watch {
                id: r.get(0)?,
                space_id: r.get(1)?,
                topic: r.get(2)?,
                interval_hours: r.get(3)?,
                session_id: r.get(4)?,
                last_run_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every watch across all spaces — used by the startup due-check, which
    /// runs before any space is necessarily "active".
    pub fn list_all_watches(&self) -> Result<Vec<Watch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, space_id, topic, interval_hours, session_id, last_run_at FROM watches",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Watch {
                id: r.get(0)?,
                space_id: r.get(1)?,
                topic: r.get(2)?,
                interval_hours: r.get(3)?,
                session_id: r.get(4)?,
                last_run_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn touch_watch(&self, id: &str, now_rfc3339: &str) -> Result<()> {
        self.conn.execute("UPDATE watches SET last_run_at = ?2 WHERE id = ?1", (id, now_rfc3339))?;
        Ok(())
    }

    pub fn delete_watch(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM watches WHERE id = ?1", [id])?;
        Ok(())
    }
}
```

Add `mod watches;` to `src/app/mod.rs`'s module list.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/app/watches.rs src/app/mod.rs
git commit -m "Add watches table and pure due_watches scheduling check

A watch is a topic + interval + the research session it reports into.
due_watches computes which are due (never run, or past their interval) as
of a given instant — pure, feeding the on-open background check wired in
the next task."
```

---

### Task 11: `/watch` picker popup + startup due-check + diff section

**Files:**
- Modify: `src/app/mod.rs` (`Popup::Watch` variant, `App` fields, `run_command`, startup hook site)
- Modify: `src/app/watches.rs` (picker methods, `run_due_watches`, diff assembly)
- Modify: `src/app/research.rs` (`on_research_done`: detect watch-session completion, prepend diff)
- Modify: `src/ui/popups/watches.rs` (new render, following the pattern of an existing simple popup — check `src/ui/popups/` for the smallest existing list-popup to mirror, e.g. the skills or files popup's Browse-mode rendering)
- Modify: `src/input.rs` (`COMMANDS` entry), `src/events.rs` (picker keybinds: up/down, `d` delete, Enter jump)
- Test: `src/app/watches.rs`

**Interfaces:**
- Consumes: `Db::list_watches`/`due_watches` (Task 10), `App::start_research_with_gate` (existing, called with `gated: false` for background watch re-runs).
- Produces: `pub(crate) fn diff_section(previous_report: &str, new_report: &str, new_sources: &[String]) -> String` — pure, unit-tested.

- [ ] **Step 1: Write the failing test**

```rust
// src/app/watches.rs
#[test]
fn diff_section_lists_new_sources_when_present() {
    let section = diff_section(
        "# Old Report\nOld body.",
        "# New Report\nNew body.",
        &["https://new-source.example".to_string()],
    );
    assert!(section.contains("What changed since last run"), "{section:?}");
    assert!(section.contains("https://new-source.example"), "{section:?}");
}

#[test]
fn diff_section_empty_new_sources_still_produces_a_header() {
    let section = diff_section("old", "new", &[]);
    assert!(section.contains("What changed since last run"));
    assert!(!section.contains("New sources"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test diff_section --lib`
Expected: FAIL — function not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `src/app/watches.rs`:

```rust
/// A "## What changed since last run" section prepended to a watch's new
/// report: lists newly-seen sources (by URL) not cited in the previous
/// report. Does not diff prose — an LLM-generated summary of what changed
/// is out of scope for this pass (YAGNI: a source-level diff is what a
/// user actually scans for first).
pub(crate) fn diff_section(previous_report: &str, new_report: &str, new_sources: &[String]) -> String {
    let _ = (previous_report, new_report); // reserved for a future prose diff; unused today
    let mut out = String::from("## What changed since last run\n\n");
    if new_sources.is_empty() {
        out.push_str("No new sources since the last run.\n");
    } else {
        out.push_str("New sources:\n");
        for s in new_sources {
            out.push_str(&format!("- {s}\n"));
        }
    }
    out
}

/// New (not-previously-cited) sources in `new_report` vs `previous_citations`
/// — a plain set difference over normalized URLs.
pub(crate) fn new_sources_since(new_report: &str, previous_citations: &[String]) -> Vec<String> {
    let previous: std::collections::HashSet<String> =
        previous_citations.iter().map(|u| crate::tools::normalize_url(u)).collect();
    crate::citations::parse_citations(new_report)
        .into_iter()
        .map(|(_, url)| url)
        .filter(|url| !previous.contains(&crate::tools::normalize_url(url)))
        .collect()
}
```

Add the picker/startup-check plumbing to `src/app/watches.rs`:

```rust
impl super::App {
    pub(crate) fn open_watch_picker(&mut self) -> anyhow::Result<()> {
        self.watches_cache = self.db.list_watches(&self.active_space.id)?;
        self.watch_selected = 0;
        self.popup = super::Popup::Watch;
        Ok(())
    }

    /// `/watch <topic>` with no existing watch of that exact topic in this
    /// space: create one (fixed 24h interval) plus its own research
    /// session, and kick off the first run immediately (ungated).
    pub(crate) fn create_watch(&mut self, topic: &str) {
        if topic.is_empty() {
            self.status = "usage: /watch <topic>".to_string();
            return;
        }
        self.new_session();
        self.start_research_with_gate(topic, false);
        let Some(session) = &self.session else {
            self.status = "could not start watch: no session created".to_string();
            return;
        };
        match self.db.create_watch(&self.active_space.id, topic, 24, &session.id) {
            Ok(_) => self.status = format!("watching: {topic} (every 24h)"),
            Err(e) => self.status = format!("watch creation failed: {e}"),
        }
    }

    pub(crate) fn delete_selected_watch(&mut self) {
        if let Some(w) = self.watches_cache.get(self.watch_selected).cloned() {
            let _ = self.db.delete_watch(&w.id);
            self.watches_cache.retain(|x| x.id != w.id);
            self.watch_selected = self.watch_selected.min(self.watches_cache.len().saturating_sub(1));
            self.status = format!("deleted watch: {}", w.topic);
        }
    }

    /// Startup hook: re-run every due watch (across all spaces) in the
    /// background, ungated. Best-effort — a watch whose research job can't
    /// start (e.g. no model configured) is silently skipped; it'll be
    /// retried on the next app open since `last_run_at` isn't touched.
    pub(crate) fn run_due_watches(&mut self) {
        let Ok(all) = self.db.list_all_watches() else { return };
        let due = due_watches(&all, chrono::Utc::now());
        for w in due {
            // Watches run into their own session, not the one currently
            // active — start_research_with_gate always operates on
            // self.session, so switch into the watch's session first,
            // fire the run, and restore whatever the user had open.
            let restore = self.session.clone();
            let restore_messages = std::mem::take(&mut self.messages);
            if let Ok(Some(s)) = self.db.get_session(&w.session_id) {
                self.session = Some(s);
                self.start_research_with_gate(&w.topic, false);
                let _ = self.db.touch_watch(&w.id, &chrono::Utc::now().to_rfc3339());
            }
            self.session = restore;
            self.messages = restore_messages;
        }
    }
}
```

`Db::get_session(&self, id: &str) -> Result<Option<Session>>` may not exist yet — grep `fn get_session` in `src/db.rs` first; if absent, add it (a straightforward `SELECT ... WHERE id = ?1` mirroring `list_sessions`'s row-mapping closure) as part of this step, since `run_due_watches` depends on it.

Wire `run_due_watches()` into app startup — find where `App::new` (or the `main.rs`/event-loop init right after constructing `App`) currently does other one-time startup work and call it there, e.g. right after skills are loaded. Grep `App::new(` in `src/main.rs` to find the exact call site and add `app.run_due_watches();` immediately after.

Add the diff section into `on_research_done`'s `ResearchUpdate::Done(Ok(report))` arm in `src/app/research.rs` — right where `save_research_report` is called, detect whether `session_id` belongs to a watch and prepend the diff:

```rust
ResearchUpdate::Done(Ok(report)) => {
    let report = if let Ok(Some(prev_citations)) = self.previous_citations_for_watch_session(&session_id) {
        let new_sources = crate::app::watches::new_sources_since(&report, &prev_citations);
        format!("{}\n\n{}", crate::app::watches::diff_section("", &report, &new_sources), report)
    } else {
        report
    };
    // ...existing body unchanged, using `report` in place of the raw `report` parameter...
}
```

Add the small helper `previous_citations_for_watch_session` in `src/app/watches.rs`:

```rust
impl super::App {
    /// `Some(urls)` if `session_id` is a watch's session and it has a prior
    /// run (citations already indexed from an earlier `save_research_report`
    /// call); `Ok(None)` for a first run or a non-watch session — either way
    /// means "no diff section".
    pub(crate) fn previous_citations_for_watch_session(&self, session_id: &str) -> anyhow::Result<Option<Vec<String>>> {
        let is_watch_session = self.db.list_all_watches()?.iter().any(|w| w.session_id == session_id);
        if !is_watch_session {
            return Ok(None);
        }
        let rows = self.db.search_citations(&self.active_space.id, None)?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(rows.into_iter().map(|(_, url, _)| url).collect()))
    }
}
```

Note `Db::search_citations` is currently `#[cfg(test)]`-gated (per earlier context: "only used by tests, not production code" — the free function is what production uses). This call site is production code needing it non-test — remove the `#[cfg(test)]` attribute from `Db::search_citations` in `src/db.rs` as part of this task (it now has a real caller), and keep the free `search_citations` function as-is (still used by the toolbox's `list_citations` tool).

Add `Popup::Watch` to the `Popup` enum in `src/app/mod.rs`, `App` fields `watches_cache: Vec<crate::db::Watch>` and `watch_selected: usize` (initialized empty/0 in `App::new`), `/watch` to `COMMANDS` in `src/input.rs`, and the `"watch" => ...` arm in `run_command`:

```rust
"watch" => {
    let arg = cmd[token.len()..].trim();
    if arg.is_empty() {
        self.open_watch_picker()?;
    } else {
        self.create_watch(arg);
    }
}
```

Add keybinds in `src/events.rs` for `Popup::Watch` mode (mirror the existing session-picker's up/down/Enter/`d` handling — find that block, e.g. under a `Popup::Session` match arm, and add an equivalent `Popup::Watch` arm calling `move_watch_selection`, `App::confirm_session`-equivalent (jump: set `self.session` from the watch's `session_id` and load its messages, same shape as `confirm_session` in `src/app/sessions.rs`), and `delete_selected_watch` on `d`).

Add a minimal render in `src/ui/popups/` — create `src/ui/popups/watches.rs` mirroring the simplest existing list popup (check `src/ui/popups/mod.rs` for how popups are dispatched and copy that file's structure: a bordered list of `topic — every Nh — last run: <date or never>`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo build && cargo test --lib`
Expected: 0 warnings, all PASS. This is the largest task in the plan — build errors are expected mid-way; work through them file by file (missing `Popup::Watch` match arms will show up as non-exhaustive-match compiler errors pointing at exact locations to fix, same pattern as the `SettingsField::BlockedDomains` fix from the previous research suite).

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/app/watches.rs src/app/mod.rs src/app/research.rs src/ui/popups/watches.rs src/ui/popups/mod.rs src/input.rs src/events.rs
git commit -m "Add /watch: standing research with on-open due-check

/watch <topic> creates a watch (fixed 24h interval) plus a dedicated
session, running research immediately. On every app startup, due watches
re-run in the background (ungated), and their new report gets a 'What
changed since last run' section listing newly-seen sources. /watch alone
opens a picker (d deletes, Enter jumps to the watch's session)."
```

---

### Task 12: `/export` report export

**Files:**
- Modify: `src/app/mod.rs` (`run_command`)
- Create: `src/app/export.rs` (new module: pure assembly + the App method)
- Modify: `src/main.rs` (`mod export;` under `app/mod.rs`'s module list, or add to `app/mod.rs`'s own `mod` list — match wherever `mod research;`/`mod watches;` are declared)
- Modify: `src/input.rs` (`COMMANDS` entry)
- Test: `src/app/export.rs`

**Interfaces:**
- Consumes: `crate::db::search_citations` (now non-test per Task 11), `crate::citations::parse_citations`.
- Produces: `pub(crate) fn assemble_report(report_body: &str, citations: &[(String, String, String)]) -> String` — pure, unit-tested.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn assemble_report_appends_numbered_bibliography() {
    let citations = vec![
        ("report-a.md".to_string(), "https://a.example".to_string(), "Title A".to_string()),
        ("report-a.md".to_string(), "https://b.example".to_string(), "".to_string()),
    ];
    let out = assemble_report("# Report\nBody [1] [2].", &citations);
    assert!(out.contains("## Sources"));
    assert!(out.contains("1. Title A — https://a.example") || out.contains("1. https://a.example"));
    assert!(out.contains("https://b.example"));
    assert!(out.starts_with("# Report"));
}

#[test]
fn assemble_report_with_no_citations_has_no_sources_section() {
    let out = assemble_report("# Report\nBody.", &[]);
    assert!(!out.contains("## Sources"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test assemble_report --lib`
Expected: FAIL — function not defined.

- [ ] **Step 3: Write minimal implementation**

Create `src/app/export.rs`:

```rust
//! `/export`: write a research session's latest report + bibliography to a
//! markdown file in the active space's files dir, overwritten on every run
//! so it stays a living document.

use anyhow::Result;

/// Append a numbered `## Sources` bibliography built from `citations`
/// (`(report_file, url, title)` rows, as returned by `Db::search_citations`)
/// to `report_body`. No section at all when `citations` is empty — an
/// export with nothing to cite shouldn't show an empty heading.
pub(crate) fn assemble_report(report_body: &str, citations: &[(String, String, String)]) -> String {
    if citations.is_empty() {
        return report_body.to_string();
    }
    let mut out = report_body.trim_end().to_string();
    out.push_str("\n\n## Sources\n\n");
    for (i, (_, url, title)) in citations.iter().enumerate() {
        if title.is_empty() {
            out.push_str(&format!("{}. {url}\n", i + 1));
        } else {
            out.push_str(&format!("{}. {title} — {url}\n", i + 1));
        }
    }
    out
}

impl super::App {
    /// `/export`: write the active session's latest research report (the
    /// most recent `assistant` message) plus its citations to
    /// `<space>/files/reports/<session-slug>.md`, overwriting any earlier
    /// export of the same session. No-op with a status message if the
    /// session has no research report yet.
    pub(crate) fn export_report(&mut self) -> Result<()> {
        let Some(session) = &self.session else {
            self.status = "no active session".to_string();
            return Ok(());
        };
        let Some(report) = self.messages.iter().rev().find(|m| m.role == "assistant").map(|m| m.content.clone())
        else {
            self.status = "nothing to export — no assistant reply yet".to_string();
            return Ok(());
        };
        let citations = self.db.search_citations(&self.active_space.id, None)?;
        let cited_here: Vec<(String, String, String)> = {
            let urls_in_report: std::collections::HashSet<String> = crate::citations::parse_citations(&report)
                .into_iter()
                .map(|(_, url)| url)
                .collect();
            citations.into_iter().filter(|(_, url, _)| urls_in_report.contains(url)).collect()
        };
        let assembled = assemble_report(&report, &cited_here);
        let dir = self.space.files_dir(&self.active_space.name).join("reports");
        std::fs::create_dir_all(&dir)?;
        let slug = super::sessions::slugify(&session.title);
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, assembled)?;
        self.status = format!("exported to {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_report_appends_numbered_bibliography() {
        let citations = vec![
            ("report-a.md".to_string(), "https://a.example".to_string(), "Title A".to_string()),
            ("report-a.md".to_string(), "https://b.example".to_string(), "".to_string()),
        ];
        let out = assemble_report("# Report\nBody [1] [2].", &citations);
        assert!(out.contains("## Sources"));
        assert!(out.contains("1. Title A — https://a.example"));
        assert!(out.contains("https://b.example"));
        assert!(out.starts_with("# Report"));
    }

    #[test]
    fn assemble_report_with_no_citations_has_no_sources_section() {
        let out = assemble_report("# Report\nBody.", &[]);
        assert!(!out.contains("## Sources"));
    }
}
```

`self.space.files_dir(name)` returns the space's files dir per `src/space.rs:75` — confirm the exact signature (`&self, name: &str) -> PathBuf`) matches this call.

Add `mod export;` to `src/app/mod.rs`'s module declarations (alongside `mod watches;`, `mod research;`, etc.).

Add `/export` to `COMMANDS` in `src/input.rs`:

```rust
Command { name: "export", desc: "write session's report + sources to a file", aliases: &["save-report"] },
```

Add the dispatch arm in `run_command`:

```rust
"export" => self.export_report()?,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/app/export.rs src/app/mod.rs src/input.rs
git commit -m "Add /export: write research report + bibliography to a file

Writes the session's latest assistant reply plus a numbered ## Sources
section (from the citation index, filtered to sources actually cited in
that report) to <space>/files/reports/<slug>.md. Re-running /export
overwrites the same file, so it stays a living document."
```

---

### Task 13: Full-suite verification pass

**Files:** none (verification only)

**Interfaces:** none.

- [ ] **Step 1: Run the full test suite**

Run: `cargo test`
Expected: All tests pass (pre-existing 273+ plus every test added in Tasks 1–12), 0 failures.

- [ ] **Step 2: Run a clean build and confirm zero warnings**

Run: `cargo build 2>&1 | grep -c warning`
Expected: `0`

- [ ] **Step 3: Run clippy if the project uses it in CI**

Check for a `.github/workflows` or `Makefile` clippy invocation (grep `clippy` in the repo root) — if present, run the same command locally, e.g. `cargo clippy --all-targets -- -D warnings`, and fix any new lints introduced by Tasks 1–12.

- [ ] **Step 4: Manual smoke test (network-touching paths, per this codebase's stated test convention)**

Exercise, in a running `nexus-chat` session against a real space with a configured research/escalation model:
- `/research some real topic` — confirm PDF/table/YouTube/discussion sources appear when relevant, confidence tags render dimmed, plan-gate still works.
- `/steer <text>` mid-run — confirm a new stage row appears and the steer's findings show up in the final report.
- Select a `[n]` citation, press `p` then `x` — confirm status messages and that a discarded domain stops appearing in later rounds.
- `/watch a topic`, then restart the app — confirm it does *not* immediately re-run (interval not yet elapsed); manually set `last_run_at` further back via `sqlite3` and restart again to confirm it does.
- `/export` on a finished research session — confirm the file lands at `<space>/files/reports/<slug>.md` with a `## Sources` section.

- [ ] **Step 5: Commit any smoke-test fixups**

If the manual pass surfaces bugs, fix them with their own small commit(s) rather than folding into earlier task commits (per this repo's git-hygiene convention observed in prior sessions — see recent commit history for the style: focused, one-concern-per-commit).

---

## Self-Review Notes

**Spec coverage:** §1.1 PDF → Task 1. §1.2 tables → Task 2. §1.3 YouTube → Task 3. §1.4 HN/Reddit → Task 4. §2.1 steering → Task 9. §2.2 pin/discard → Tasks 5–6. §2.3 confidence → Task 7. §2.4 quote checking → Task 8. §3 watches → Tasks 10–11. §4 export → Task 12. All spec sections covered.

**Sequencing note:** Tasks 1–4 (deeper sources) and Task 7–8 (confidence/quote-check, which touch `VERIFIER_PROMPT` and the Verifier call site) are independent of each other and of Tasks 5–6 (pin/discard) and 9 (steer) — all touch different call sites in `run_research_inner`/`tools.rs` but not the same lines, so they can be done in any order or in parallel by different workers per `superpowers:dispatching-parallel-agents` if desired. Tasks 10 and 11 must be sequential (11 depends on 10's schema/CRUD). Task 12 depends on Task 11's un-test-gating of `Db::search_citations`. Task 13 must be last.
