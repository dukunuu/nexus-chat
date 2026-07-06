# Research Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the four coordinated research-suite upgrades from `docs/superpowers/specs/2026-07-06-research-suite-design.md` in one pass: web answer mode, research UX (plan gate / live feed / drill-down / citations), source quality (freshness, domain filters, dedup, academic search), and knowledge persistence (web cache, local-first planning, citation index).

**Architecture:** Every change layers onto the existing `ToolBox` (tool defs/run in `src/tools.rs`), the `research.rs` pipeline (pure prompt/parse functions + one background `tokio::spawn`ed orchestration), and `db.rs`'s migrate-on-open SQLite schema. New pure functions (URL normalization, dedup, citation parsing, plan-edit parsing) get unit tests; new network paths (Semantic Scholar, live fetches, the plan-gate oneshot under real timing) are exercised manually, exactly like every other background job in this codebase (`maybe_generate_title`, embedding, deep research today).

**Tech Stack:** Rust 2024, `tokio` (mpsc/oneshot/timeout), `rusqlite` (bundled SQLite, migrate-on-open `ALTER TABLE ... ; let _ = ...` pattern), `reqwest` for HTTP, `ratatui`/`tui-textarea` for the TUI, `open` crate (already a dependency) for opening citation URLs.

## Global Constraints

- Rust edition 2024 (per `Cargo.toml`); build with `cargo build` and validate with `cargo test` before each commit.
- No new dependencies — `open`, `reqwest`, `chrono`, `tokio` (`sync`, `time`) already cover everything this spec needs.
- Every pure function (URL normalization, dedup, citation parsing, plan-edit parsing, freshness/domain query rewriting) gets a unit test in the same file's `#[cfg(test)] mod tests`, matching the existing convention in `tools.rs`/`research.rs`/`db.rs`.
- Network-calling paths (Semantic Scholar HTTP calls, live `fetch_url`/`web_search`, the plan-gate timeout under real wall-clock time beyond what a fast in-process `tokio::time::pause`-free test can assert) are exercised manually — do not write flaky network-dependent tests.
- Follow existing conventions exactly: DB migrations are `ALTER TABLE ... ADD COLUMN` / `CREATE TABLE IF NOT EXISTS` in `Db::migrate`, tool defs are pushed onto `Vec<ToolDef>` in `ToolBox::defs()`, tool dispatch is a `match name` arm in `ToolBox::run()` returning `(result, status)`, settings fields follow the `SettingsField` enum + `SETTINGS_GROUPS` + `settings_inputs` index pattern.
- `cargo test` must pass after every task; `cargo build` must be warning-clean (existing code has no `#[allow(dead_code)]` noise — don't introduce any).
- Ponytail ceilings from the spec are final, not a starting point: Semantic Scholar is the only academic backend, and citation lookups are substring/keyword matches — do not add claim-level indexing, arXiv/Crossref, or a multi-provider abstraction.
- Reuse existing helpers rather than re-implementing: `crate::db::fts_quote`, `resolve_confined`, `send_and_parse`, `strip_html_to_text`, `App::refresh_toolbox`, `super::clamp_cursor`, `fuzzy_filter_sorted`.

---

### Task 1: Source-quality tool params + URL normalization/dedup

**Files:**
- Modify: `src/tools.rs` (`ToolBox::search`, `defs()` web_search entry ~L184-192, `run()` "web_search" arm ~L524-536, new pure functions + tests)
- Modify: `src/app/research.rs` (wire dedup into `run_research_inner` before the Synthesizer call, ~L291)
- Modify: `src/app/mod.rs` (new `SettingsField::BlockedDomains`, `SETTINGS_GROUPS` "Web Search" entry, `settings_inputs` grows to 8, `text_index()` arm)
- Modify: `src/app/settings.rs` (`open_settings`/`save_settings` read/write the per-space blocked-domains file)
- Modify: `src/space.rs` (new `blocked_domains_path`)

**Interfaces:**
- Consumes: `ToolBox::search(&self, query: &str) -> anyhow::Result<Vec<SearchHit>>`, `SearchHit { title, url, snippet }`, `searxng_search`/`langsearch_search`/`duckduckgo_search` signatures, `App::refresh_toolbox(&mut self)`, `Space::instructions_path` pattern for the new `blocked_domains_path`.
- Produces: `pub(crate) fn normalize_url(url: &str) -> String`, `pub(crate) fn dedup_source_lines(findings: &[String]) -> Vec<String>`, `pub(crate) fn rewrite_query_with_domains(query: &str, include: &[String], exclude: &[String]) -> String`, extended `ToolBox::search` accepting `recency`/`include_domains`/`exclude_domains`, all consumed by Task 6/7 wiring and by `research.rs`'s synthesis step.

- [ ] **Step 1: Write failing tests for `normalize_url`**
  In `src/tools.rs`'s `#[cfg(test)] mod tests`, add:
  ```rust
  #[test]
  fn normalize_url_lowercases_host_strips_tracking_params_and_trailing_slash() {
      assert_eq!(
          normalize_url("HTTPS://Example.COM/Page/?utm_source=x&utm_medium=y&id=1&fbclid=abc#frag"),
          "https://example.com/Page?id=1"
      );
      assert_eq!(normalize_url("https://example.com/"), "https://example.com");
      assert_eq!(normalize_url("https://example.com"), "https://example.com");
      assert_eq!(normalize_url("not a url"), "not a url");
  }
  ```
- [ ] **Step 2: Run test to verify it fails**
  `cargo test normalize_url_lowercases_host_strips_tracking_params_and_trailing_slash` — expected failure: `cannot find function 'normalize_url' in this scope`.
- [ ] **Step 3: Write minimal `normalize_url`**
  Add above the `#[cfg(test)]` block in `src/tools.rs`:
  ```rust
  /// Normalize a source URL for dedup: lowercase the host only (path/query
  /// case is preserved — some servers are case-sensitive there), strip
  /// `utm_*`/`fbclid` query params, and drop a trailing `/` and any fragment.
  /// Unparseable input (not actually a URL) is returned unchanged so it still
  /// participates in a plain string-equality dedup.
  pub(crate) fn normalize_url(url: &str) -> String {
      let Ok(mut u) = reqwest::Url::parse(url) else { return url.to_string() };
      u.set_fragment(None);
      let kept: Vec<(String, String)> = u
          .query_pairs()
          .filter(|(k, _)| k != "fbclid" && !k.starts_with("utm_"))
          .map(|(k, v)| (k.into_owned(), v.into_owned()))
          .collect();
      if kept.is_empty() {
          u.set_query(None);
      } else {
          let q = kept.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
          u.set_query(Some(&q));
      }
      let host = u.host_str().map(str::to_lowercase);
      if let Some(h) = host {
          let _ = u.set_host(Some(&h));
      }
      let mut s = u.to_string();
      if let Some(stripped) = s.strip_suffix('/') {
          s = stripped.to_string();
      }
      s
  }
  ```
- [ ] **Step 4: Run test to verify it passes**
  `cargo test normalize_url` — expected: 1 passed.
- [ ] **Step 5: Write failing test for `dedup_source_lines`**
  ```rust
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
  ```
- [ ] **Step 6: Run test to verify it fails**
  `cargo test dedup_source_lines_keeps_first_occurrence` — expected failure: unresolved function.
- [ ] **Step 7: Write minimal `dedup_source_lines`**
  ```rust
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
                  if in_sources {
                      if let Some((_, url)) = line.trim().split_once(['.', ')']) {
                          let key = normalize_url(url.trim());
                          if !seen.insert(key) {
                              continue; // dup — drop this line
                          }
                      }
                  }
                  out_lines.push(line.to_string());
              }
              out_lines.join("\n")
          })
          .collect()
  }
  ```
- [ ] **Step 8: Run test to verify it passes**
  `cargo test dedup_source_lines` — expected: 1 passed.
- [ ] **Step 9: Wire dedup into the research pipeline**
  In `src/app/research.rs`, change `run_research_inner`'s synthesis call (currently `synthesizer_messages(topic, &findings)`) to dedup first:
  ```rust
  send_stage(tx, ids, "synthesizing…");
  let deduped = crate::tools::dedup_source_lines(&findings);
  let mut draft = complete_text(provider, research_model, synthesizer_messages(topic, &deduped)).await?;
  ```
  and the same substitution at the round-2 re-synthesis call site (`findings.extend(more); ... synthesizer_messages(topic, &findings)`) — pass `&crate::tools::dedup_source_lines(&findings)` there too.
- [ ] **Step 10: Write failing test for domain-filter query rewriting**
  ```rust
  #[test]
  fn rewrite_query_with_domains_appends_site_and_negated_site_terms() {
      let out = rewrite_query_with_domains("rust async runtimes", &["docs.rs".into()], &["reddit.com".into(), "quora.com".into()]);
      assert_eq!(out, "rust async runtimes site:docs.rs -site:reddit.com -site:quora.com");
      assert_eq!(rewrite_query_with_domains("q", &[], &[]), "q");
  }
  ```
- [ ] **Step 11: Run test to verify it fails, then implement**
  `cargo test rewrite_query_with_domains` fails with unresolved function; then add:
  ```rust
  /// Rewrite `query` with `site:`/`-site:` terms — backend-agnostic (every
  /// engine this app talks to honors Google-style site filters), so
  /// `include_domains`/`exclude_domains`/`blocked_domains` need no per-backend
  /// plumbing beyond this string rewrite.
  pub(crate) fn rewrite_query_with_domains(query: &str, include: &[String], exclude: &[String]) -> String {
      let mut q = query.to_string();
      for d in include {
          q.push_str(&format!(" site:{d}"));
      }
      for d in exclude {
          q.push_str(&format!(" -site:{d}"));
      }
      q
  }
  ```
  Then `cargo test rewrite_query_with_domains` — expected: 1 passed.
- [ ] **Step 12: Extend `ToolBox::search` with recency + domain params**
  Change the signature and every backend call site:
  ```rust
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
  ```
  Update `searxng_search`/`langsearch_search` signatures to take `recency: Option<&str>` and set `.query(&[("time_range", r)])` / include `"freshness": r` in the LangSearch JSON body respectively, only when `Some`. `duckduckgo_search` keeps its existing signature (recency ignored, per spec).
- [ ] **Step 13: Update the two existing call sites of `search`**
  `run("web_search", ...)` now parses `recency`, `include_domains`, `exclude_domains` from the tool args JSON and passes them through:
  ```rust
  "web_search" => {
      let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
      let query = v.get("query").and_then(|q| q.as_str()).unwrap_or_default().to_string();
      let recency = v.get("recency").and_then(|r| r.as_str()).map(str::to_string);
      let str_list = |k: &str| v.get(k).and_then(|a| a.as_array())
          .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>())
          .unwrap_or_default();
      let mut exclude = str_list("exclude_domains");
      exclude.extend(self.blocked_domains.clone());
      let include = str_list("include_domains");
      let status = "Searching the web…".to_string();
      let result = match self.search(&query, recency.as_deref(), &include, &exclude).await {
          Ok(hits) if hits.is_empty() => "no results".to_string(),
          Ok(hits) => format_results(&hits),
          Err(e) => format!("search failed: {e}"),
      };
      (result, status)
  }
  ```
  Update the two direct-call tests in `tests` module (`explicit_choice_errors_clearly...`, `auto_reaches_searxng...`) to pass the new params: `tb.search("test", None, &[], &[])`.
- [ ] **Step 14: Add `blocked_domains` field to `ToolBox` + `new`/`research` constructors**
  Add `pub blocked_domains: Vec<String>` to the `ToolBox` struct, thread it through `ToolBox::new(...)` (new last-but-one param before `files`) and `ToolBox::research(...)`, defaulting to `Vec::new()` — update every existing call site in `src/tools.rs` tests, `src/app/mod.rs` (`App::new`, `refresh_toolbox`), and `src/app/research.rs` (`ToolBox::research(...)`) to pass an explicit `Vec::new()` (research jobs get blocked domains from the app in Step 18).
- [ ] **Step 15: Update `defs()`'s web_search description + schema**
  ```rust
  defs.push(ToolDef {
      name: "web_search".to_string(),
      description: "Search the web and return numbered results with title, url, and snippet. recency restricts to recent results (day/week/month/year — ignored by the DuckDuckGo fallback backend). include_domains/exclude_domains restrict/exclude specific sites.".to_string(),
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
  ```
- [ ] **Step 16: Add `Space::blocked_domains_path`**
  In `src/space.rs`, next to `instructions_path`:
  ```rust
  pub fn blocked_domains_path(&self, name: &str) -> PathBuf {
      self.space_dir(name).join("blocked_domains.txt")
  }
  ```
- [ ] **Step 17: Add `SettingsField::BlockedDomains` (per-space, in the "Web Search" group)**
  In `src/app/mod.rs`: add the variant to `SettingsField`, bump `ALL` to `[SettingsField; 19]` with it appended, add its `label()` arm (`"blocked domains (comma-separated, always excluded)"`), append it to the `"Web Search"` `SettingsGroup`, and add a `Some(7)` arm in `text_index()` (the 8th `settings_inputs` slot).
- [ ] **Step 18: Read/write the per-space file in the settings popup**
  In `src/app/settings.rs`:
  - `open_settings`: grow `self.settings_inputs` to `[String; 8]`, with `[7]` seeded from `std::fs::read_to_string(self.space.blocked_domains_path(&self.active_space.name)).unwrap_or_default()`.
  - `save_settings`: after the other writes, add
    ```rust
    let blocked = self.settings_inputs[7].trim().to_string();
    let _ = std::fs::write(self.space.blocked_domains_path(&self.active_space.name), &blocked);
    ```
    before the existing `self.refresh_toolbox();` call.
  - `settings_input_char`/`paste` in `src/input.rs`: add `SettingsField::BlockedDomains` to the "free text, not numeric" match arms alongside `SearxngUrl`/`LangsearchKey`/`EmbeddingModel`.
- [ ] **Step 19: Populate `ToolBox.blocked_domains` in `refresh_toolbox`**
  In `App::refresh_toolbox` (`src/app/mod.rs`), before constructing the new `ToolBox`, read:
  ```rust
  let blocked_domains: Vec<String> = std::fs::read_to_string(self.space.blocked_domains_path(&self.active_space.name))
      .unwrap_or_default()
      .split(',')
      .map(|d| d.trim().to_string())
      .filter(|d| !d.is_empty())
      .collect();
  ```
  and pass `blocked_domains` into the `ToolBox::new(...)` call.
- [ ] **Step 20: Run the full test suite**
  `cargo test tools::` and `cargo test app::` — expected: all passing, including the two updated `search(...)` call-site tests.
- [ ] **Step 21: Manual verification**
  With a SearXNG or LangSearch key configured, run `/config`, set "blocked domains" to a real domain, save, then ask the model a web-mode question and confirm (via Ctrl+T tool detail) the rewritten query carries `-site:<domain>`. Not unit-tested — live network path.
- [ ] **Step 22: Commit**
  `git add src/tools.rs src/app/research.rs src/app/mod.rs src/app/settings.rs src/space.rs src/input.rs && git commit -m "$(cat <<'EOF'
  Add web_search recency/domain filters and cross-searcher source dedup

  Lets research and web-mode answers restrict to recent/allowed domains and
  stops the same source from being cited redundantly across searchers.
  EOF
  )"`

---

### Task 2: Web cache table + `fetch_url` write-through/read path

**Files:**
- Modify: `src/db.rs` (`migrate()` new `web_cache` table, new `Db` methods, tests)
- Modify: `src/tools.rs` (`fetch_url_text` gains a cache-aware wrapper, `run()` "fetch_url" arm, `FilesCtx`/new cache ctx field)

**Interfaces:**
- Consumes: `Db::open`/`open_in_memory` pattern, `rusqlite::Connection::open(&ctx.db_path)` (the toolbox's own short-lived connection pattern from `search_files_impl`), `chrono::Utc::now()`.
- Produces: `pub fn cache_get(conn: &Connection, url_norm: &str) -> Result<Option<(String, String, String)>>` (title, text, fetched_at), `pub fn cache_put(conn: &Connection, url_norm: &str, url: &str, title: Option<&str>, text: &str) -> Result<()>`, `pub(crate) fn is_fresh(fetched_at: &str, now: chrono::DateTime<Utc>) -> bool` (pure, unit tested), extended `fetch_url` tool with a `fresh: bool` param and a `db_path: Option<PathBuf>` field on `ToolBox` for cache access.

- [ ] **Step 1: Write failing test for `is_fresh`**
  In `src/db.rs`'s `#[cfg(test)] mod tests`:
  ```rust
  #[test]
  fn is_fresh_true_under_24h_false_over() {
      let now = Utc::now();
      let recent = (now - chrono::Duration::hours(1)).to_rfc3339();
      let stale = (now - chrono::Duration::hours(25)).to_rfc3339();
      assert!(is_fresh(&recent, now));
      assert!(!is_fresh(&stale, now));
      assert!(!is_fresh("not a timestamp", now)); // unparseable = not fresh
  }
  ```
- [ ] **Step 2: Run test to verify it fails**
  `cargo test is_fresh_true_under_24h_false_over` — expected failure: unresolved function.
- [ ] **Step 3: Write minimal `is_fresh`**
  Above `#[cfg(test)]` in `src/db.rs`:
  ```rust
  /// Whether a cached fetch (`fetched_at`, rfc3339) is still usable — under
  /// 24h old. An unparseable timestamp is treated as stale, not an error:
  /// the caller just re-fetches live.
  pub(crate) fn is_fresh(fetched_at: &str, now: chrono::DateTime<Utc>) -> bool {
      chrono::DateTime::parse_from_rfc3339(fetched_at)
          .map(|dt| now.signed_duration_since(dt) < chrono::Duration::hours(24))
          .unwrap_or(false)
  }
  ```
- [ ] **Step 4: Run test to verify it passes**
  `cargo test is_fresh` — expected: 1 passed.
- [ ] **Step 5: Add the `web_cache` table to `migrate()`**
  In `src/db.rs`, append to the `execute_batch` string in `migrate()` (after `message_images`'s index):
  ```sql
  CREATE TABLE IF NOT EXISTS web_cache (
      url_norm   TEXT PRIMARY KEY,
      url        TEXT NOT NULL,
      title      TEXT,
      text       TEXT NOT NULL,
      fetched_at TEXT NOT NULL
  );
  ```
- [ ] **Step 6: Write failing test for cache_get/cache_put roundtrip**
  ```rust
  #[test]
  fn web_cache_roundtrips_and_updates_on_rewrite() {
      let db = Db::open_in_memory().unwrap();
      assert!(cache_get(db.raw(), "example.com/a").unwrap().is_none());
      cache_put(db.raw(), "example.com/a", "https://example.com/a", Some("Title"), "body text").unwrap();
      let (title, text, fetched_at) = cache_get(db.raw(), "example.com/a").unwrap().unwrap();
      assert_eq!(title, "Title");
      assert_eq!(text, "body text");
      assert!(!fetched_at.is_empty());

      // Re-fetching (fresh: true path) overwrites, not duplicates.
      cache_put(db.raw(), "example.com/a", "https://example.com/a", None, "new body").unwrap();
      let (title, text, _) = cache_get(db.raw(), "example.com/a").unwrap().unwrap();
      assert_eq!(title, "");
      assert_eq!(text, "new body");
  }
  ```
- [ ] **Step 7: Run test to verify it fails**
  `cargo test web_cache_roundtrips_and_updates_on_rewrite` — expected failure: unresolved functions.
- [ ] **Step 8: Write minimal `cache_get`/`cache_put`**
  ```rust
  /// A cached fetched page: (title, text, fetched_at rfc3339), or None on a
  /// cache miss.
  pub fn cache_get(conn: &Connection, url_norm: &str) -> Result<Option<(String, String, String)>> {
      let row = conn.query_row(
          "SELECT COALESCE(title, ''), text, fetched_at FROM web_cache WHERE url_norm = ?1",
          [url_norm],
          |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
      );
      match row {
          Ok(v) => Ok(Some(v)),
          Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
          Err(e) => Err(e.into()),
      }
  }

  /// Write (or overwrite) a fetched page into the cache, stamped `now`.
  pub fn cache_put(conn: &Connection, url_norm: &str, url: &str, title: Option<&str>, text: &str) -> Result<()> {
      let now = Utc::now().to_rfc3339();
      conn.execute(
          "INSERT INTO web_cache (url_norm, url, title, text, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)
           ON CONFLICT(url_norm) DO UPDATE SET url = ?2, title = ?3, text = ?4, fetched_at = ?5",
          (url_norm, url, title, text, &now),
      )?;
      Ok(())
  }
  ```
- [ ] **Step 9: Run test to verify it passes**
  `cargo test web_cache_roundtrips` — expected: 1 passed.
- [ ] **Step 10: Give `ToolBox` its own db path (independent of `FilesCtx`)**
  The web cache is global (space-agnostic per the spec), while `FilesCtx` is only present when files exist. Add a new field to `ToolBox`:
  ```rust
  pub struct ToolBox {
      // ...existing fields...
      /// Shared db path for the (space-agnostic) web page cache. `None` only
      /// for the research-only toolbox variant used before Task 7 wires it in
      /// (research searchers get one too, from `App::start_research`).
      web_cache_db: Option<PathBuf>,
  }
  ```
  Thread it through `ToolBox::new(...)` (new final param, before `apps`) and `ToolBox::research(...)` (takes it as a new leading param), updating every call site (`App::new`, `App::refresh_toolbox`, `research.rs::start_research`, and every test constructor in `tools.rs`) to pass `Some(self.space.db_path())` (app side) or the equivalent test db path.
- [ ] **Step 11: Cache-aware `fetch_url_text`**
  Replace the direct call in the `"fetch_url"` arm of `ToolBox::run` with a cache-checking wrapper:
  ```rust
  /// Fetch (through the cache): serve a fresh (<24h) cached copy unless
  /// `force_fresh`, else live-fetch and write through. Cache read/write
  /// failures degrade to a live fetch — a broken db must never block a tool
  /// call.
  async fn fetch_cached(&self, client: &reqwest::Client, url: &str, force_fresh: bool) -> anyhow::Result<String> {
      let url_norm = normalize_url(url);
      if !force_fresh
          && let Some(db_path) = &self.web_cache_db
          && let Ok(conn) = rusqlite::Connection::open(db_path)
          && let Ok(Some((_, text, fetched_at))) = crate::db::cache_get(&conn, &url_norm)
          && crate::db::is_fresh(&fetched_at, chrono::Utc::now())
      {
          return Ok(text);
      }
      let text = fetch_url_text(client, url).await?;
      if let Some(db_path) = &self.web_cache_db
          && let Ok(conn) = rusqlite::Connection::open(db_path)
      {
          let _ = crate::db::cache_put(&conn, &url_norm, url, None, &text);
      }
      Ok(text)
  }
  ```
- [ ] **Step 12: Update the `"fetch_url"` arm and tool schema**
  ```rust
  "fetch_url" => {
      let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
      let url = v.get("url").and_then(|u| u.as_str()).unwrap_or_default().to_string();
      let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
      let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).clamp(1, 200) as usize;
      let fresh = v.get("fresh").and_then(|f| f.as_bool()).unwrap_or(false);
      let status = format!("Fetching {url}…");
      let result = match self.fetch_cached(&self.client, &url, fresh).await {
          Ok(text) => { /* unchanged pagination logic below */ }
          Err(e) => format!("fetch failed: {e}"),
      };
      (result, status)
  }
  ```
  (Keep the existing pagination/line-slicing body unchanged, just fed from `fetch_cached` instead of `fetch_url_text` directly.) Add `"fresh": { "type": "boolean", "description": "bypass the 24h cache and re-fetch live" }` to the `fetch_url` tool's `defs()` schema.
- [ ] **Step 13: Write a test proving the cache is actually consulted**
  Add to `tools.rs` tests (using a real temp-file db, per the `files_toolbox()` pattern):
  ```rust
  #[tokio::test]
  async fn fetch_url_serves_from_cache_when_fresh() {
      let path = std::env::temp_dir().join(format!("nexus-webcache-{}.db", uuid::Uuid::new_v4()));
      let db = crate::db::Db::open(&path).unwrap();
      crate::db::cache_put(db.raw(), "example.com/a", "https://example.com/a", None, "cached body").unwrap();
      let tb = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), Vec::new(), None, None, Some(path));
      // No network reachable in test env — a cache hit must not attempt one.
      let (result, _) = tb.run("fetch_url", r#"{"url":"https://example.com/a"}"#).await;
      assert!(result.contains("cached body"), "{result}");
  }
  ```
  (Adjust the `ToolBox::new` argument order/count to match whatever was finalized in Step 10.)
- [ ] **Step 14: Run test to verify it passes**
  `cargo test fetch_url_serves_from_cache_when_fresh` — expected: 1 passed (proves no network hit was needed since the CI sandbox has no route to `example.com`, and the result is the cached text, not a "fetch failed" error).
- [ ] **Step 15: Run the full test suite**
  `cargo test` — expected: all passing, including every existing `tools.rs`/`db.rs` test updated for the new constructor arities.
- [ ] **Step 16: Manual verification**
  Ask the model to fetch a real URL twice in the same session; confirm (Ctrl+T) the second fetch is fast and the DB's `web_cache` table (via `sqlite3 ~/.local/share/nexus-chat/nexus.db "select url, fetched_at from web_cache"`) shows one row updated once, not duplicated.
- [ ] **Step 17: Commit**
  `git commit -m "$(cat <<'EOF'
  Add a 24h write-through cache for fetch_url

  Repeated fetches of the same page (common across research sub-questions
  and follow-ups) now hit SQLite instead of the network.
  EOF
  )"`

---

### Task 3: Web answer mode toggle

**Files:**
- Modify: `src/db.rs` (`sessions.web_mode` column, `Session.web_mode` field, `create_session`/`list_sessions` updated, `set_session_web_mode`, tests)
- Modify: `src/app/mod.rs` (`App.web_mode` field, init)
- Create/Modify: `src/app/chat.rs` (`/web` command handling helper, `system_prompt()` appends the web-mode instruction block)
- Modify: `src/input.rs` (`COMMANDS` gets `web`)
- Modify: `src/app/sessions.rs` (`new_session`/`confirm_session` sync `web_mode` from the loaded session)
- Modify: `src/ui/mod.rs` (`render_status` shows `🌐 web`)

**Interfaces:**
- Consumes: `Session { id, title, model, slug, created_at, compact_summary, compact_through }`, `Db::insert_message`/`create_session` pattern, `App::system_prompt(&self) -> String` (chat.rs ~L467), `App::run_command` dispatch (mod.rs ~L1082).
- Produces: `Session.web_mode: bool`, `Db::set_session_web_mode(&self, id: &str, on: bool) -> Result<()>`, `App::toggle_web_mode(&mut self)`, `pub(crate) fn web_mode_clause(today: &str) -> String` (pure, unit tested).

- [ ] **Step 1: Add `web_mode` column and field**
  In `src/db.rs`, add to the `ALTER TABLE` list in `migrate()`:
  `"ALTER TABLE sessions ADD COLUMN web_mode INTEGER NOT NULL DEFAULT 0",`
  Add `pub web_mode: bool` to `struct Session`. Update `create_session` to return `web_mode: false`, and `list_sessions`'s SELECT/query_map to include `web_mode` (`SELECT id, title, model, slug, created_at, compact_summary, compact_through, web_mode FROM sessions ...`, `web_mode: r.get::<_, i64>(7)? != 0`).
- [ ] **Step 2: Add `set_session_web_mode`**
  ```rust
  pub fn set_session_web_mode(&self, session_id: &str, on: bool) -> Result<()> {
      self.conn.execute(
          "UPDATE sessions SET web_mode = ?2 WHERE id = ?1",
          (session_id, on as i64),
      )?;
      Ok(())
  }
  ```
- [ ] **Step 3: Write failing test for the db roundtrip**
  ```rust
  #[test]
  fn web_mode_defaults_off_and_toggles() {
      let db = Db::open_in_memory().unwrap();
      let space = db.default_space_id().unwrap();
      let s = db.create_session("t", "a/b", &space).unwrap();
      assert!(!s.web_mode);
      db.set_session_web_mode(&s.id, true).unwrap();
      assert!(db.list_sessions(&space).unwrap()[0].web_mode);
  }
  ```
- [ ] **Step 4: Run test to verify it fails, then passes**
  `cargo test web_mode_defaults_off_and_toggles` — fails on missing field/method, then passes after Steps 1-2.
- [ ] **Step 5: Write failing test for `web_mode_clause`**
  In `src/app/chat.rs`'s existing test module location — since `chat.rs` currently has no `#[cfg(test)] mod tests` of its own (tests for it live in `app/tests.rs`), add the pure function's test to `src/app/tests.rs`:
  ```rust
  #[test]
  fn web_mode_clause_instructs_search_first_and_cites_inline() {
      let c = web_mode_clause("2026-07-06");
      assert!(c.contains("2026-07-06"));
      assert!(c.contains("web_search"));
      assert!(c.contains("[n]"));
      assert!(c.contains("Sources:"));
  }
  ```
- [ ] **Step 6: Run test to verify it fails**
  `cargo test web_mode_clause_instructs_search_first_and_cites_inline` — expected failure: unresolved function/import (`use chat::{code_blocks, pick_greeting, web_mode_clause}` or similar re-export needed in `mod.rs`'s test-only imports).
- [ ] **Step 7: Write minimal `web_mode_clause` + wire into `system_prompt`**
  In `src/app/chat.rs`:
  ```rust
  /// The instruction block appended to the system prompt when web mode is on:
  /// forces search-first, inline `[n]` citations, and a trailing Sources list.
  /// `today` is the current date so the model doesn't answer with stale
  /// "current as of my training" hedging.
  pub(super) fn web_mode_clause(today: &str) -> String {
      format!(
          "Web answer mode is ON for this session. Today's date is {today}. Before \
           answering, you MUST call web_search (and fetch_url on the most promising \
           results) — never answer from memory alone. Cite every claim inline as \
           [n], and end your reply with a line starting exactly with 'Sources:' \
           followed by the numbered list of URLs you used, one per line, matching \
           your [n] citations."
      )
  }
  ```
  In `App::system_prompt` (chat.rs ~L467), after the `parts` vec is built and before `.join`, add:
  ```rust
  if self.session.as_ref().is_some_and(|s| s.web_mode) {
      let today = Utc::now().format("%Y-%m-%d").to_string();
      parts.push(web_mode_clause(&today));
  }
  ```
  Export it for the test: add `use chat::web_mode_clause;` under the existing `#[cfg(test)] use chat::split_inline_reasoning;` line in `src/app/mod.rs`.
- [ ] **Step 8: Run test to verify it passes**
  `cargo test web_mode_clause` — expected: 1 passed.
- [ ] **Step 9: Add `/web` command**
  In `src/input.rs`'s `COMMANDS`, add:
  `Command { name: "web", desc: "toggle web answer mode", aliases: &["websearch"] },`
  In `src/app/mod.rs`'s `run_command` match, add:
  `"web" => self.toggle_web_mode(),`
- [ ] **Step 10: Implement `App::toggle_web_mode`**
  In `src/app/chat.rs` (near `system_prompt`):
  ```rust
  /// `/web`: flip web answer mode for the active (or about-to-be-created)
  /// session. Persisted immediately if a session already exists; otherwise
  /// applied to the session created by the next `send_message`.
  pub(crate) fn toggle_web_mode(&mut self) {
      self.web_mode = !self.web_mode;
      if let Some(session) = self.session.as_mut() {
          session.web_mode = self.web_mode;
          let _ = self.db.set_session_web_mode(&session.id, self.web_mode);
      }
      self.status = if self.web_mode { "🌐 web mode on".to_string() } else { "web mode off".to_string() };
  }
  ```
- [ ] **Step 11: Add `App.web_mode` field and sync points**
  In `src/app/mod.rs`, add `pub(crate) web_mode: bool` to `App` and `false` in `App::new`'s struct literal. In `src/app/sessions.rs`:
  - `new_session`: does NOT reset `self.web_mode` — the toggle is meant to persist across `/new` until explicitly turned off (matches how `current_model` persists). Leave as-is; no change needed there.
  - `confirm_session` (picking an existing session): after `self.session = Some(s);`, add `self.web_mode = self.session.as_ref().unwrap().web_mode;` so switching sessions restores that session's own toggle state.
  - In `send_message` (`src/app/chat.rs`, the "Auto-create a session on the first message" branch), after `self.session = Some(s);`, add:
    ```rust
    if self.web_mode {
        let _ = self.db.set_session_web_mode(&self.session.as_ref().unwrap().id, true);
        self.session.as_mut().unwrap().web_mode = true;
    }
    ```
- [ ] **Step 12: Show `🌐 web` in the status line**
  In `src/ui/mod.rs`'s `render_status`, add next to `space_tag`:
  ```rust
  let web_tag = if app.web_mode { "🌐 web " } else { "" };
  ```
  and splice `web_tag` into both format strings: `format!("{space_tag}{web_tag}{model}  |  {}", app.status)` and the `show_bar` branch's `cols[0]` paragraph text `format!("{web_tag}{model} ")`.
- [ ] **Step 13: Write app-level test for the toggle + persistence**
  In `src/app/tests.rs`:
  ```rust
  #[tokio::test]
  async fn web_mode_toggles_persists_across_session_switch_and_shows_in_system_prompt() {
      let mut a = app_with_key();
      a.current_model = Some("a/one".into());
      assert!(!a.web_mode);
      a.toggle_web_mode();
      assert!(a.web_mode);
      assert!(a.status.contains("web mode on"));

      a.set_input("hi");
      a.submit().unwrap();
      let sid = a.session.as_ref().unwrap().id.clone();
      assert!(a.db.list_sessions(&a.active_space.id).unwrap().iter().find(|s| s.id == sid).unwrap().web_mode);
      assert!(a.system_prompt().contains("Web answer mode is ON"));

      a.toggle_web_mode(); // off, still in this session
      assert!(!a.system_prompt().contains("Web answer mode is ON"));
  }
  ```
- [ ] **Step 14: Run test to verify it passes**
  `cargo test web_mode_toggles_persists` — expected: 1 passed. Then `cargo test` for the whole crate to catch any other test relying on the old `Session`/`list_sessions` column count.
- [ ] **Step 15: Manual verification**
  Launch the app, `/web`, send a question, confirm the status line shows `🌐 web`, the model calls `web_search` before answering, and the reply ends with a `Sources:` list.
- [ ] **Step 16: Commit**
  `git commit -m "$(cat <<'EOF'
  Add /web: per-session forced search-first, cited answer mode

  A per-session toggle that appends a system-prompt instruction requiring
  web_search before answering and inline [n] citations with a Sources list.
  EOF
  )"`

---

### Task 4: Citation rendering + open-keybind

**Files:**
- Create: `src/citations.rs` (pure `parse_citations`, `citation_number_in`, `style_citations` + tests)
- Modify: `src/main.rs` or wherever modules are declared (add `mod citations;`)
- Modify: `src/ui/history.rs` (`push_assistant_stored`/`push_assistant_streaming` call `style_citations` after `crate::markdown::render`)
- Modify: `src/selection.rs` (`HistorySel::owner_at_selection` helper)
- Modify: `src/events.rs` (`o` keybind in `handle_normal`)
- Modify: `src/app/mod.rs` or a new `src/app/citations.rs` (`App::open_citation_under_selection`)

**Interfaces:**
- Consumes: `crate::markdown::Rendered { lines, code, blocks }`, `crate::theme::Theme.accent: Color`, `HistorySel.selected_text(&self) -> Option<String>` (selection.rs ~L265), `HistorySel`'s private `owner: Vec<Option<usize>>` + `sel: Option<(Pos, Pos)>` fields, `App.messages: Vec<Message>`, `open::that_detached`.
- Produces: `pub(crate) fn parse_citations(content: &str) -> Vec<(usize, String)>`, `pub(crate) fn citation_number_in(text: &str) -> Option<usize>`, `pub(crate) fn style_citations(lines: Vec<Line<'static>>, accent: Color) -> Vec<Line<'static>>`, `HistorySel::owner_at_selection_start(&self) -> Option<usize>`, `App::open_citation_under_selection(&mut self)`.

- [ ] **Step 1: Declare the new module**
  Add `mod citations;` to `src/main.rs` (alongside the other top-level `mod` declarations — check the existing list there and insert alphabetically).
- [ ] **Step 2: Write failing tests for `parse_citations`**
  In `src/citations.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn parse_citations_reads_a_sources_heading_section() {
          let content = "# Report\n\nBody text [1] and more [2].\n\n## Sources\n1. https://a.example\n2. https://b.example/page\n";
          assert_eq!(parse_citations(content), vec![(1, "https://a.example".into()), (2, "https://b.example/page".into())]);
      }

      #[test]
      fn parse_citations_reads_a_plain_sources_colon_line() {
          let content = "findings text [1]\nSources:\n1. https://a.example\n";
          assert_eq!(parse_citations(content), vec![(1, "https://a.example".into())]);
      }

      #[test]
      fn parse_citations_returns_empty_when_no_sources_section() {
          assert!(parse_citations("just prose, no citations").is_empty());
      }

      #[test]
      fn citation_number_in_finds_first_bracketed_number() {
          assert_eq!(citation_number_in("supported by research [3] and also [4]"), Some(3));
          assert_eq!(citation_number_in("no citation here"), None);
          assert_eq!(citation_number_in("[not a number]"), None);
      }
  }
  ```
- [ ] **Step 3: Run test to verify it fails**
  `cargo test --lib citations::` — expected failure: module/functions don't exist yet.
- [ ] **Step 4: Write minimal `parse_citations` and `citation_number_in`**
  ```rust
  //! Pure parsing for report/finding citations: the trailing `Sources:` (or
  //! `## Sources`) list every research/web-mode reply ends with (see
  //! `WRITER_PROMPT`/`SEARCHER_PROMPT` in `app/research.rs`), and the `[n]`
  //! inline markers that reference it.

  use ratatui::style::{Color, Modifier, Style};
  use ratatui::text::{Line, Span};

  /// Parse a message's trailing citation list into `(n, url)` pairs, in the
  /// order they're listed. Recognizes a `Sources:` line or a `## Sources`
  /// (any heading level) marker, then reads `N. url` / `N) url` lines until
  /// a blank line or non-matching line ends the section.
  pub(crate) fn parse_citations(content: &str) -> Vec<(usize, String)> {
      let mut out = Vec::new();
      let mut in_section = false;
      for line in content.lines() {
          let t = line.trim();
          if !in_section {
              let heading = t.trim_start_matches('#').trim();
              if t.eq_ignore_ascii_case("Sources:") || heading.eq_ignore_ascii_case("Sources") {
                  in_section = true;
              }
              continue;
          }
          if t.is_empty() {
              continue;
          }
          let Some((num, rest)) = t.split_once(['.', ')']) else { break };
          let Ok(n) = num.trim().parse::<usize>() else { break };
          let url = rest.trim().to_string();
          if url.is_empty() {
              break;
          }
          out.push((n, url));
      }
      out
  }

  /// The first `[n]` (n = 1+ ascii digits) substring in `text`, if any.
  pub(crate) fn citation_number_in(text: &str) -> Option<usize> {
      let start = text.find('[')?;
      let rest = &text[start + 1..];
      let end = rest.find(']')?;
      let inner = &rest[..end];
      (!inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit())).then(|| inner.parse().ok())?
  }
  ```
- [ ] **Step 5: Run test to verify it passes**
  `cargo test --lib citations::` — expected: 4 passed.
- [ ] **Step 6: Write failing test for `style_citations`**
  ```rust
  #[test]
  fn style_citations_restyles_bracketed_numbers_and_preserves_the_rest() {
      let lines = vec![Line::from(vec![Span::raw("supported by "), Span::raw("[1]"), Span::raw(" evidence")])];
      let out = style_citations(lines, Color::Cyan);
      assert_eq!(out.len(), 1);
      let has_accent = out[0].spans.iter().any(|s| s.content.as_ref() == "[1]" && s.style.fg == Some(Color::Cyan));
      assert!(has_accent);
      let plain: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
      assert_eq!(plain, "supported by [1] evidence");
  }

  #[test]
  fn style_citations_ignores_non_numeric_brackets() {
      let lines = vec![Line::from(Span::raw("a [note] here"))];
      let out = style_citations(lines, Color::Cyan);
      assert!(out[0].spans.iter().all(|s| s.style.fg != Some(Color::Cyan)));
  }
  ```
- [ ] **Step 7: Run test to verify it fails**
  `cargo test style_citations` — expected failure: unresolved function.
- [ ] **Step 8: Write minimal `style_citations`**
  ```rust
  /// Re-style every `[n]` citation marker across already-rendered `lines`
  /// with `accent`; everything else keeps its existing style. Splits spans
  /// as needed, so a citation embedded mid-span still gets its own styled
  /// piece.
  pub(crate) fn style_citations(lines: Vec<Line<'static>>, accent: Color) -> Vec<Line<'static>> {
      lines.into_iter().map(|line| style_citations_line(line, accent)).collect()
  }

  fn style_citations_line(line: Line<'static>, accent: Color) -> Line<'static> {
      let alignment = line.alignment;
      let style = line.style;
      let mut spans = Vec::new();
      for span in line.spans {
          spans.extend(split_citation_span(span, accent));
      }
      let mut out = Line::from(spans);
      out.alignment = alignment;
      out.style = style;
      out
  }

  /// Split one span so each `[n]` substring becomes its own accent-styled
  /// span; everything else keeps the original span's style.
  fn split_citation_span(span: Span<'static>, accent: Color) -> Vec<Span<'static>> {
      let text = span.content.to_string();
      let mut out = Vec::new();
      let mut rest = text.as_str();
      loop {
          let Some(start) = rest.find('[') else {
              if !rest.is_empty() {
                  out.push(Span::styled(rest.to_string(), span.style));
              }
              break;
          };
          let Some(end_rel) = rest[start + 1..].find(']') else {
              out.push(Span::styled(rest.to_string(), span.style));
              break;
          };
          let end = start + 1 + end_rel;
          let inner = &rest[start + 1..end];
          if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
              if start > 0 {
                  out.push(Span::styled(rest[..start].to_string(), span.style));
              }
              out.push(Span::styled(
                  rest[start..=end].to_string(),
                  Style::default().fg(accent).add_modifier(Modifier::BOLD),
              ));
              rest = &rest[end + 1..];
          } else {
              out.push(Span::styled(rest[..=start].to_string(), span.style));
              rest = &rest[start + 1..];
          }
      }
      out
  }
  ```
- [ ] **Step 9: Run test to verify it passes**
  `cargo test style_citations` — expected: 2 passed.
- [ ] **Step 10: Wire `style_citations` into the two history render call sites**
  In `src/ui/history.rs`, `push_assistant_stored` (has `theme: &crate::theme::Theme` already):
  ```rust
  let mut rendered = crate::markdown::render(&msg.content, width);
  rendered.lines = crate::citations::style_citations(rendered.lines, theme.accent);
  ```
  and in `push_assistant_streaming` (has `app.theme`), the same pattern around `crate::markdown::render(buf, width)`.
  (Write it as a two-line rebind — `let mut rendered = ...; rendered.lines = ...;` — to avoid partial-move issues with struct-update syntax.)
- [ ] **Step 11: Add `HistorySel::owner_at_selection_start`**
  In `src/selection.rs`, near `selected_text` (~L265):
  ```rust
  /// The message index the active selection's anchor line belongs to, if any
  /// — used to resolve "the citation under the current selection" back to
  /// the message whose content actually holds the Sources list.
  pub fn owner_at_selection_start(&self) -> Option<usize> {
      let (anchor, _) = self.sel?;
      self.owner.get(anchor.0).copied().flatten()
  }
  ```
- [ ] **Step 12: Write a unit test for `owner_at_selection_start`**
  Find the existing test-builder helpers in `selection.rs` (`build`, `sel_with`, `with_owner` near the bottom) and add:
  ```rust
  #[test]
  fn owner_at_selection_start_resolves_the_anchor_lines_message() {
      let mut s = with_owner(&["line a", "line b"], &[Some(0), Some(1)]);
      s.sel = Some(((1, 0), (1, 3)));
      assert_eq!(s.owner_at_selection_start(), Some(1));
      s.sel = None;
      assert_eq!(s.owner_at_selection_start(), None);
  }
  ```
  (Adjust arguments to match `with_owner`'s actual signature, read from `selection.rs` at implementation time — it's already used by nearby tests in the same file.)
- [ ] **Step 13: Run tests to verify they pass**
  `cargo test owner_at_selection_start` — expected: 1 passed.
- [ ] **Step 14: Implement `App::open_citation_under_selection`**
  Add to `src/app/chat.rs` next to `system_prompt` (avoid a near-empty new file):
  ```rust
  /// `o` in the history pane: open the `[n]` citation under the current text
  /// selection (via the `open` crate), resolved against the Sources list of
  /// the message the selection belongs to. No selection, no `[n]` inside it,
  /// or no matching source number all surface as a status message rather
  /// than doing nothing silently.
  pub(crate) fn open_citation_under_selection(&mut self) {
      let Some(selected) = self.sel.selected_text() else {
          self.status = "select a [n] citation, then press o".to_string();
          return;
      };
      let Some(n) = crate::citations::citation_number_in(&selected) else {
          self.status = "no [n] citation in the current selection".to_string();
          return;
      };
      let Some(owner) = self.sel.owner_at_selection_start() else {
          self.status = "no [n] citation in the current selection".to_string();
          return;
      };
      let Some(msg) = self.messages.get(owner) else {
          self.status = "no [n] citation in the current selection".to_string();
          return;
      };
      let citations = crate::citations::parse_citations(&msg.content);
      match citations.iter().find(|(num, _)| *num == n) {
          Some((_, url)) => {
              let _ = open::that_detached(url);
              self.status = format!("opened [{n}]: {url}");
          }
          None => self.status = format!("no source [{n}] in this message"),
      }
  }
  ```
- [ ] **Step 15: Bind `o` in the history pane**
  In `src/events.rs`'s `handle_normal`, add a match arm before the catch-all `_ => { app.input.input(key); ... }`:
  ```rust
  // 'o' opens the [n] citation under the current history selection; typing
  // 'o' in the composer (no active selection) is unaffected — it just types.
  KeyCode::Char('o') if !ctrl && !shift && app.sel.selected_text().is_some() => {
      app.open_citation_under_selection();
  }
  ```
  (Guarding on `selected_text().is_some()` means a bare `o` keystroke with no history selection still falls through to normal typing in the composer — no behavior change for the common case.)
- [ ] **Step 16: Write an app-level test for the keybind's underlying method**
  In `src/app/tests.rs`:
  ```rust
  #[test]
  fn open_citation_under_selection_resolves_against_the_owning_messages_sources() {
      let mut a = app_with_key();
      a.messages.push(Message {
          id: String::new(), role: "assistant".into(),
          content: "claim [1] and another [2].\n\n## Sources\n1. https://a.example\n2. https://b.example\n".into(),
          model: None, reasoning: None, tokens: None, secs: None, phrase: None, images: Vec::new(),
      });
      // Simulate a render + selection covering "[2]" on message index 0.
      a.sel.record_render(
          ratatui::layout::Rect::new(0, 0, 80, 10),
          0,
          vec!["claim [1] and another [2].".to_string()],
          vec![Some(0)],
          vec![None],
          vec![],
      );
      a.sel.on_down((0, 22)); // inside "[2]"
      a.sel.on_drag((0, 25));
      a.open_citation_under_selection();
      assert!(a.status.contains("https://b.example"), "{}", a.status);
  }
  ```
  (Column offsets must be adjusted to whatever `record_render`'s exact signature is once read at implementation time — it's used identically in `ui/history.rs::render_history`, so mirror that call.)
- [ ] **Step 17: Run test to verify it passes**
  `cargo test open_citation_under_selection` — expected: 1 passed.
- [ ] **Step 18: Run the full suite**
  `cargo test` — expected: all passing.
- [ ] **Step 19: Manual verification**
  In a finished research report with `[n]` citations, mouse-drag-select a `[2]` marker in the history pane, press `o`, confirm the browser opens the matching source and the status line shows `opened [2]: <url>`.
- [ ] **Step 20: Commit**
  `git commit -m "$(cat <<'EOF'
  Style [n] citations and add an o keybind to open the cited source

  Citation markers render in the theme accent color; selecting one and
  pressing o opens its Sources-list URL via the open crate.
  EOF
  )"`

---

### Task 5: `academic_search` tool

**Files:**
- Modify: `src/tools.rs` (`defs()`, `run()` new arm, Semantic Scholar client + parsing + tests)
- Modify: `src/app/research.rs` (mention `academic_search` in `PLANNER_PROMPT`)

**Interfaces:**
- Consumes: `send_and_parse<T>(req: reqwest::RequestBuilder) -> anyhow::Result<T>` (existing helper), `ToolBox::run`'s `(String, String)` return convention, `ToolDef` shape.
- Produces: `pub(crate) fn format_papers(papers: &[Paper]) -> String` (pure, unit tested), new `"academic_search"` tool available in both interactive chat and research searchers (i.e. NOT restricted by `research_only`).

- [ ] **Step 1: Write failing test for `format_papers`**
  In `src/tools.rs`'s tests:
  ```rust
  #[test]
  fn formats_papers_as_numbered_list_with_metadata() {
      let papers = vec![
          Paper {
              title: "Attention Is All You Need".into(),
              authors: vec!["A. Vaswani".into(), "N. Shazeer".into()],
              year: Some(2017),
              venue: Some("NeurIPS".into()),
              abstract_snippet: Some("We propose a new architecture...".into()),
              citation_count: Some(90000),
              url: "https://www.semanticscholar.org/paper/abc".into(),
          },
      ];
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
          title: "Untitled Preprint".into(), authors: vec![], year: None, venue: None,
          abstract_snippet: None, citation_count: None, url: "https://x".into(),
      }];
      let out = format_papers(&papers);
      assert!(out.contains("[1] Untitled Preprint"));
      assert!(out.contains("https://x"));
  }
  ```
- [ ] **Step 2: Run test to verify it fails**
  `cargo test formats_papers_as_numbered_list_with_metadata` — expected failure: `Paper`/`format_papers` don't exist.
- [ ] **Step 3: Write minimal `Paper` + `format_papers`**
  Add near `SearchHit` (plain owned struct — deserialization happens via the intermediate Semantic Scholar types in Step 5):
  ```rust
  struct Paper {
      title: String,
      authors: Vec<String>,
      year: Option<i64>,
      venue: Option<String>,
      abstract_snippet: Option<String>,
      citation_count: Option<i64>,
      url: String,
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
              let meta_line = meta.join(" · ");
              let abs = p.abstract_snippet.as_deref().unwrap_or("");
              format!("[{}] {}\n    {meta_line}\n    {abs}\n    {}", i + 1, p.title, p.url)
          })
          .collect::<Vec<_>>()
          .join("\n\n")
  }
  ```
- [ ] **Step 4: Run test to verify it passes**
  `cargo test format_papers` — expected: 2 passed.
- [ ] **Step 5: Write failing test for the response envelope's deserialization**
  ```rust
  #[test]
  fn parses_semantic_scholar_response_json() {
      let json = r#"{"data":[
          {"title":"A","authors":[{"name":"X"}],"year":2020,"venue":"V","abstract":"abs","citationCount":5,"url":"https://s2/a"}
      ]}"#;
      let resp: SemanticScholarResponse = serde_json::from_str(json).unwrap();
      assert_eq!(resp.data.len(), 1);
      assert_eq!(resp.data[0].title, "A");
  }
  ```
  The real API's `authors` is `[{"name": "..."}]` (objects, not bare strings) — use intermediate structs matching the existing `LangsearchResponse`/`LangsearchData`/`LangsearchWebPages` layering pattern already in this file:
  ```rust
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
  ```
- [ ] **Step 6: Run test to verify it fails, then passes**
  `cargo test parses_semantic_scholar_response_json` fails (types don't exist), then passes once Step 5's types are added.
- [ ] **Step 7: Add the Semantic Scholar HTTP call**
  ```rust
  /// Semantic Scholar Graph API (https://api.semanticscholar.org): free,
  /// keyless. 429 (rate limited, no key) surfaces as an error the caller
  /// turns into tool-result text — the model falls back to web_search.
  async fn academic_search(client: &reqwest::Client, query: &str, limit: usize) -> anyhow::Result<Vec<Paper>> {
      let fields = "title,authors,year,venue,abstract,citationCount,url";
      let req = client.get("https://api.semanticscholar.org/graph/v1/paper/search").query(&[
          ("query", query),
          ("limit", &limit.min(20).to_string()),
          ("fields", &fields.to_string()),
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
  ```
- [ ] **Step 8: Add the `"academic_search"` tool def and dispatch arm**
  In `defs()`, after `fetch_url`'s def:
  ```rust
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
  ```
  In `run()`, after the `"fetch_url"` arm:
  ```rust
  "academic_search" => {
      let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
      let query = v.get("query").and_then(|q| q.as_str()).unwrap_or_default().to_string();
      let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(10) as usize;
      let status = "Searching academic literature…".to_string();
      let result = match academic_search(&self.client, &query, limit).await {
          Ok(papers) if papers.is_empty() => "no results".to_string(),
          Ok(papers) => format_papers(&papers),
          Err(e) => format!("academic search failed: {e}"),
      };
      (result, status)
  }
  ```
  `academic_search` must survive the `research_only` filter (searchers get it too): add `"academic_search"` to the `matches!` guard at the top of `run()` and to the retained set in `defs()`'s `research_only` filter.
- [ ] **Step 9: Update the research-toolbox test that asserts an exact tool count**
  In `tests::research_toolbox_only_offers_web_search_and_fetch_url`, bump `assert_eq!(names.len(), 2, ...)` to `3` and add `assert!(names.contains(&"academic_search".to_string()));`, renaming the test to `research_toolbox_offers_web_search_fetch_url_and_academic_search`.
- [ ] **Step 10: Run test to verify it passes**
  `cargo test research_toolbox` — expected: passes with 3 tools.
- [ ] **Step 11: Mention `academic_search` in the Planner prompt**
  In `src/app/research.rs`, extend `PLANNER_PROMPT`'s text (append a clause): `" Note that a searcher agent handling a scholarly sub-question can call academic_search (Semantic Scholar) in addition to web_search."` — no test needed (static string; existing `parse_subquestions` tests don't depend on prompt wording).
- [ ] **Step 12: Run the full suite**
  `cargo test` — expected: all passing.
- [ ] **Step 13: Manual verification**
  Ask the model (or a `/research` run on a scholarly topic) to use `academic_search`; confirm real results with title/authors/year/citations come back, and that hitting the API without a query still degrades gracefully (network path, manual only).
- [ ] **Step 14: Commit**
  `git commit -m "$(cat <<'EOF'
  Add academic_search (Semantic Scholar) for scholarly sources

  Free, keyless Graph API search available to interactive chat and research
  searchers; the Planner is nudged to reach for it on scholarly topics.
  EOF
  )"`

---

### Task 6: Local-first planner input + citation index

**Files:**
- Modify: `src/app/research.rs` (`planner_messages` gains known-chunks context, `run_research_inner` embeds the topic first)
- Modify: `src/db.rs` (`citations` table, `add_citations`, `search_citations`, tests)
- Modify: `src/tools.rs` (new `"list_citations"` tool, always available)
- Modify: `src/app/research.rs::save_research_report` (populate `citations` on save)

**Interfaces:**
- Consumes: `crate::db::semantic_chunks(conn, space_id, query, limit) -> Result<Vec<(String, String, String, f32)>>`, `OpenRouter::embed(&self, model: &str, inputs: Vec<String>) -> Result<Vec<Vec<f32>>>`, `App.embedding_model: String`, `App.provider: Option<OpenRouter>`, `crate::citations::parse_citations` (Task 4), `App::save_research_report`.
- Produces: `fn planner_messages_with_context(topic: &str, known: &[String]) -> Vec<ChatMessage>` (pure, replaces the old `planner_messages` call site), `Db::add_citations(&self, space_id: &str, report_file: &str, citations: &[(String, Option<String>)]) -> Result<()>`, `Db::search_citations(&self, space_id: &str, query: Option<&str>) -> Result<Vec<(String, String, String)>>` (report_file, url, title), new `"list_citations"` tool.

- [ ] **Step 1: Write failing test for `planner_messages_with_context`**
  In `src/app/research.rs`'s tests:
  ```rust
  #[test]
  fn planner_messages_with_context_includes_known_chunks_as_gap_guidance() {
      let msgs = planner_messages_with_context("rust async runtimes", &["Rust's async model uses a Future trait.".to_string()]);
      assert_eq!(msgs[0].role, "system");
      assert!(msgs[1].content.contains("rust async runtimes"));
      assert!(msgs[1].content.contains("already known"));
      assert!(msgs[1].content.contains("Future trait"));
  }

  #[test]
  fn planner_messages_with_context_falls_back_to_plain_prompt_when_empty() {
      let msgs = planner_messages_with_context("topic", &[]);
      assert!(!msgs[1].content.contains("already known"));
      assert_eq!(msgs[1].content, "topic");
  }
  ```
- [ ] **Step 2: Run test to verify it fails**
  `cargo test planner_messages_with_context` — expected failure: unresolved function.
- [ ] **Step 3: Write minimal `planner_messages_with_context`**
  Replace the existing private `planner_messages` with a superset (keep the name change local — `plan()` is the only caller):
  ```rust
  fn planner_messages_with_context(topic: &str, known: &[String]) -> Vec<ChatMessage> {
      let user = if known.is_empty() {
          topic.to_string()
      } else {
          let body = known.join("\n\n");
          format!(
              "Topic: {topic}\n\nAlready known (from the user's own files) — plan \
               sub-questions for the gaps, not what's already covered:\n{body}"
          )
      };
      vec![ChatMessage::text("system", PLANNER_PROMPT), ChatMessage::text("user", user)]
  }
  ```
- [ ] **Step 4: Run test to verify it passes**
  `cargo test planner_messages_with_context` — expected: 2 passed.
- [ ] **Step 5: Thread known-chunks through `plan()` and `run_research_inner`**
  Change `plan`'s signature to accept `known: &[String]`:
  ```rust
  async fn plan(provider: &OpenRouter, model: &str, topic: &str, known: &[String]) -> Result<Vec<String>, String> {
      let text = complete_text(provider, model, planner_messages_with_context(topic, known)).await?;
      let qs = parse_subquestions(&text);
      if qs.is_empty() {
          return Err(format!("planner returned no usable sub-questions (raw reply: {text:.200})"));
      }
      Ok(qs)
  }
  ```
  Add a `known: &[String]` parameter to `run_research_inner` and thread it from `run_research`.
- [ ] **Step 6: Run existing research tests to check for signature breakage**
  `cargo test app::research::` — existing tests only call `start_research`/`on_research_done`, not `plan` directly, so no test changes expected beyond compilation of the new params.
- [ ] **Step 7: Compute local-first context as a free async function**
  Implement (free function, runs inside the spawned `run_research` task so `start_research` never blocks):
  ```rust
  /// Top-k chunks from the active space's files already relevant to `topic`,
  /// for the Planner's "already known" context — silently empty when
  /// embeddings aren't configured or the space has no files (never blocks
  /// `/research` on either).
  async fn local_known_chunks(provider: &OpenRouter, embedding_model: &str, db_path: &std::path::Path, space_id: &str, topic: &str) -> Vec<String> {
      if embedding_model.trim().is_empty() {
          return Vec::new();
      }
      let Ok(mut vecs) = provider.embed(embedding_model, vec![topic.to_string()]).await else {
          return Vec::new();
      };
      if vecs.is_empty() {
          return Vec::new();
      }
      let query = vecs.remove(0);
      let Ok(conn) = rusqlite::Connection::open(db_path) else { return Vec::new() };
      crate::db::semantic_chunks(&conn, space_id, &query, 5)
          .map(|hits| hits.into_iter().map(|(name, loc, text, _)| format!("{name} ({loc}): {text}")).collect())
          .unwrap_or_default()
  }
  ```
  (Adjust `semantic_chunks`'s tuple shape to its actual signature at implementation time.)
- [ ] **Step 8: Update `run_research`/`start_research` signatures end-to-end**
  `run_research` grows two params: `embedding_model: String, db_path: std::path::PathBuf`. Body becomes:
  ```rust
  pub(crate) async fn run_research(
      provider: OpenRouter,
      research_model: String,
      escalation_model: String,
      embedding_model: String,
      db_path: std::path::PathBuf,
      topic: String,
      toolbox: Arc<ToolBox>,
      tx: mpsc::UnboundedSender<ResearchMsg>,
      session_id: String,
      space_id: String,
      space_name: String,
  ) {
      let known = local_known_chunks(&provider, &embedding_model, &db_path, &space_id, &topic).await;
      let ids = (session_id, space_id, space_name);
      let result = run_research_inner(&provider, &research_model, &escalation_model, &topic, &known, &toolbox, &tx, &ids).await;
      let _ = tx.send((ids.0, ids.1, ids.2, ResearchUpdate::Done(result)));
  }
  ```
  `run_research_inner` gains `known: &[String]`, passed to `plan(provider, research_model, topic, known).await?`. In `start_research`, pass `self.embedding_model.clone()` and `self.space.db_path()` into the `tokio::spawn(run_research(...))` call.
- [ ] **Step 9: Run research tests**
  `cargo test app::research::` — expected: existing tests (`start_research_creates_and_switches_into_a_new_session`, etc.) still pass once the new params compile.
- [ ] **Step 10: Add the `citations` table**
  In `src/db.rs`'s `migrate()`, add:
  ```sql
  CREATE TABLE IF NOT EXISTS citations (
      id          INTEGER PRIMARY KEY AUTOINCREMENT,
      space_id    TEXT NOT NULL,
      report_file TEXT NOT NULL,
      url         TEXT NOT NULL,
      title       TEXT
  );
  CREATE INDEX IF NOT EXISTS idx_citations_space ON citations(space_id);
  ```
- [ ] **Step 11: Write failing test for `add_citations`/`search_citations`**
  ```rust
  #[test]
  fn citations_index_stores_and_substring_searches() {
      let db = Db::open_in_memory().unwrap();
      let space = db.default_space_id().unwrap();
      db.add_citations(&space, "research-x-20260706.md", &[
          ("https://nature.com/articles/1".to_string(), Some("Nature paper".to_string())),
          ("https://example.com/blog".to_string(), None),
      ]).unwrap();

      let all = db.search_citations(&space, None).unwrap();
      assert_eq!(all.len(), 2);

      let hits = db.search_citations(&space, Some("nature.com")).unwrap();
      assert_eq!(hits.len(), 1);
      assert_eq!(hits[0].0, "research-x-20260706.md");
      assert_eq!(hits[0].1, "https://nature.com/articles/1");

      let hits = db.search_citations(&space, Some("Nature paper")).unwrap();
      assert_eq!(hits.len(), 1); // title also matches
  }
  ```
- [ ] **Step 12: Run test to verify it fails**
  `cargo test citations_index_stores_and_substring_searches` — expected failure: unresolved methods.
- [ ] **Step 13: Write minimal `add_citations`/`search_citations`**
  Free functions + `Db` delegation (matching the existing `files_missing_embeddings` free-function pattern used by the toolbox's short-lived connections):
  ```rust
  /// Record a report's cited sources for the citation index.
  pub fn add_citations(conn: &Connection, space_id: &str, report_file: &str, citations: &[(String, Option<String>)]) -> Result<()> {
      for (url, title) in citations {
          conn.execute(
              "INSERT INTO citations (space_id, report_file, url, title) VALUES (?1, ?2, ?3, ?4)",
              (space_id, report_file, url, title),
          )?;
      }
      Ok(())
  }

  /// Citations in `space_id` whose url/title/report_file contains `query`
  /// (case-insensitive substring), or every row when `query` is None — as
  /// `(report_file, url, title)`.
  pub fn search_citations(conn: &Connection, space_id: &str, query: Option<&str>) -> Result<Vec<(String, String, String)>> {
      let mut stmt = conn.prepare(
          "SELECT report_file, url, COALESCE(title, '') FROM citations
           WHERE space_id = ?1
             AND (?2 IS NULL OR url LIKE ?2 OR title LIKE ?2 OR report_file LIKE ?2)
           ORDER BY id DESC",
      )?;
      let pattern = query.map(|q| format!("%{q}%"));
      let rows = stmt.query_map((space_id, pattern), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
      Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
  }
  ```
  with `Db` methods delegating: `pub fn add_citations(&self, ...) { add_citations(&self.conn, ...) }` / `pub fn search_citations(&self, ...) { search_citations(&self.conn, ...) }`.
  (SQLite's default `LIKE` is ASCII case-insensitive, matching the test's mixed-case `"Nature paper"` search.)
- [ ] **Step 14: Run test to verify it passes**
  `cargo test citations_index_stores_and_substring_searches` — expected: 1 passed.
- [ ] **Step 15: Populate the index on report save**
  In `src/app/research.rs`'s `save_research_report`, after the successful `std::fs::write(dir.join(&name), report)`, add:
  ```rust
  let citations = crate::citations::parse_citations(report);
  if !citations.is_empty() {
      let rows: Vec<(String, Option<String>)> = citations.into_iter().map(|(_, url)| (url, None)).collect();
      let _ = self.db.add_citations(space_id, &name, &rows);
  }
  ```
  (Titles aren't in the `Sources:`/`## Sources` line format today — `None` is correct; carrying titles through would require a Writer-prompt change, out of scope.)
- [ ] **Step 16: Write a test proving the report-save path populates the index**
  In `research.rs`'s test module:
  ```rust
  #[tokio::test]
  async fn on_research_done_final_report_populates_citation_index() {
      let mut a = test_app();
      a.research_model = "openai/gpt-5-mini".to_string();
      a.start_research("rust async runtimes");
      let session_id = a.session.as_ref().unwrap().id.clone();
      let space_id = a.active_space.id.clone();
      let space_name = a.active_space.name.clone();

      a.on_research_done(Some((
          session_id,
          space_id.clone(),
          space_name,
          ResearchUpdate::Done(Ok("# Report\n\nBody [1].\n\n## Sources\n1. https://example.com/a\n".to_string())),
      )));

      let hits = a.db.search_citations(&space_id, Some("example.com")).unwrap();
      assert_eq!(hits.len(), 1);
      assert_eq!(hits[0].1, "https://example.com/a");
  }
  ```
- [ ] **Step 17: Run test to verify it passes**
  `cargo test on_research_done_final_report_populates_citation_index` — expected: 1 passed.
- [ ] **Step 18: Add the `list_citations` tool**
  In `src/tools.rs`, unconditionally in `defs()` (not gated on `files_count() > 0` — citations exist independent of imported files), reading `self.files.as_ref()` for db/space access:
  ```rust
  defs.push(ToolDef {
      name: "list_citations".to_string(),
      description: "List sources cited in past research reports in this space, optionally filtered by a substring match against url/title/report name. Use to answer 'what have we researched about X' or 'which reports cite <site>'.".to_string(),
      parameters: serde_json::json!({
          "type": "object",
          "properties": { "query": { "type": "string", "description": "substring to match (omit to list everything)" } },
      }),
  });
  ```
  In `run()`:
  ```rust
  "list_citations" => {
      let query = serde_json::from_str::<serde_json::Value>(args)
          .ok()
          .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string));
      let status = "Listing citations…".to_string();
      let result = match &self.files {
          None => "no space context available".to_string(),
          Some(ctx) => match rusqlite::Connection::open(&ctx.db_path) {
              Err(e) => format!("citation lookup failed: {e}"),
              Ok(conn) => match crate::db::search_citations(&conn, &ctx.space_id, query.as_deref()) {
                  Ok(rows) if rows.is_empty() => "no citations recorded yet".to_string(),
                  Ok(rows) => rows
                      .iter()
                      .map(|(report, url, title)| {
                          if title.is_empty() { format!("{report}: {url}") } else { format!("{report}: {url} ({title})") }
                      })
                      .collect::<Vec<_>>()
                      .join("\n"),
                  Err(e) => format!("citation lookup failed: {e}"),
              },
          },
      };
      (result, status)
  }
  ```
- [ ] **Step 19: Write a test for the `list_citations` tool**
  In `src/tools.rs`'s tests, reuse the `files_toolbox()` helper's returned `(tb, db, space)`:
  ```rust
  #[tokio::test]
  async fn list_citations_reports_recorded_sources_and_filters_by_query() {
      let (tb, db, space) = files_toolbox();
      db.add_citations(&space, "research-a.md", &[("https://nature.com/x".to_string(), None)]).unwrap();
      let (result, _) = tb.run("list_citations", r#"{}"#).await;
      assert!(result.contains("research-a.md"), "{result}");
      assert!(result.contains("nature.com"), "{result}");

      let (result, _) = tb.run("list_citations", r#"{"query":"nope"}"#).await;
      assert!(result.contains("no citations"), "{result}");
  }
  ```
- [ ] **Step 20: Run test to verify it passes**
  `cargo test list_citations_reports_recorded_sources` — expected: 1 passed.
- [ ] **Step 21: Run the full suite**
  `cargo test` — expected: all passing.
- [ ] **Step 22: Manual verification**
  Finish a `/research` run, confirm the saved report's file appears in `sqlite3 nexus.db "select * from citations"`; ask the model `list_citations` about a domain from that report and confirm it responds with the report name and URL. Confirm a second `/research` on a topic covered by already-imported files produces sub-questions that visibly avoid re-covering that known material (Planner prompt path — manual, since the model's actual planning choices aren't unit-testable).
- [ ] **Step 23: Commit**
  `git commit -m "$(cat <<'EOF'
  Feed known file chunks into the Planner and index research citations

  /research skips sub-questions already answered by the space's imported
  files, and every report's Sources list is indexed for list_citations.
  EOF
  )"`

---

### Task 7: Plan gate, live feed, drill-down

**Files:**
- Modify: `src/app/research.rs` (`ResearchUpdate::Stage` becomes structured, plan-gate `oneshot`, `/research!` bypass, in-place stage row update, session-source persistence)
- Modify: `src/db.rs` (`session_sources` table, `upsert_research_stage_message`, tests)
- Modify: `src/app/mod.rs` (`App.research_plan_gate` field, `/research!` parsing in `run_command`)
- Modify: `src/events.rs` (`e`/Enter routing when a plan gate is open)
- Modify: `src/ui/history.rs` (`research_plan` role rendering)
- Modify: `src/app/chat.rs` (`build_history` skip-list, research-session system-prompt note)
- Modify: `src/tools.rs` (`search_sources` tool + `cited_url_norms` helper)

**Interfaces:**
- Consumes: everything from Tasks 1-6 (`normalize_url`, `web_cache` via `cache_get`/`cache_put`, `ToolBox::research(...)`, `ResearchMsg`, `db_path` param from Task 6), `tokio::sync::oneshot`, `tokio::time::timeout`.
- Produces: `pub(crate) enum ResearchUpdate { Stage { label: String, detail: String }, PlanReady { questions: Vec<String> }, Done(Result<String, String>) }`, `pub(crate) fn parse_plan_edit(text: &str) -> Vec<String>` (pure, unit tested), `Db::add_session_sources`/`Db::search_session_sources` (+ free-function forms), new `"search_sources"` tool, `App::approve_research_plan` / `App::edit_research_plan` / `App::submit_research_plan_edit`, `pub(crate) fn cited_url_norms(findings: &[String]) -> Vec<String>`.

- [ ] **Step 1: Design note — extend `ResearchUpdate` without breaking existing call sites**
  Add a third variant rather than overloading `Stage`, so the gate's "plan ready, waiting" moment is unambiguous:
  ```rust
  pub(crate) enum ResearchUpdate {
      Stage { label: String, detail: String },
      /// The Planner finished; the pipeline is paused awaiting approval/edit/timeout.
      PlanReady { questions: Vec<String> },
      Done(std::result::Result<String, String>),
  }
  ```
  `send_stage` changes signature; every call site in `run_research_inner` gets a stable `label` (used for update-in-place matching) plus a `detail` (e.g. `label: "searching"`, `detail: format!("round {round}, {done}/{total}")`). Successive updates within a stage share the label and replace the same DB row.
- [ ] **Step 2: Write failing test for `parse_plan_edit`**
  ```rust
  #[test]
  fn parse_plan_edit_reads_one_question_per_line() {
      let qs = parse_plan_edit("what is X\nhow does Y work\n\nis Z true");
      assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string(), "is Z true".to_string()]);
  }

  #[test]
  fn parse_plan_edit_strips_bullet_and_number_prefixes_like_the_planner_parser() {
      let qs = parse_plan_edit("- what is X\n2. how does Y work");
      assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string()]);
  }

  #[test]
  fn parse_plan_edit_caps_at_max_subquestions() {
      let lines: Vec<String> = (0..10).map(|i| format!("q{i}")).collect();
      assert_eq!(parse_plan_edit(&lines.join("\n")).len(), MAX_SUBQUESTIONS);
  }
  ```
- [ ] **Step 3: Run test to verify it fails**
  `cargo test parse_plan_edit` — expected failure: unresolved function.
- [ ] **Step 4: Write minimal `parse_plan_edit`**
  ```rust
  /// Parse the user-edited plan textarea (one sub-question per line, same
  /// bullet/number tolerance as the Planner's own fallback parser) back into
  /// a sub-question list.
  pub(crate) fn parse_plan_edit(text: &str) -> Vec<String> {
      text.lines()
          .map(strip_list_prefix)
          .filter(|l| !l.is_empty())
          .take(MAX_SUBQUESTIONS)
          .collect()
  }
  ```
- [ ] **Step 5: Run test to verify it passes**
  `cargo test parse_plan_edit` — expected: 3 passed.
- [ ] **Step 6: Add `session_sources` table**
  In `src/db.rs`'s `migrate()`:
  ```sql
  CREATE TABLE IF NOT EXISTS session_sources (
      session_id TEXT NOT NULL,
      url_norm   TEXT NOT NULL,
      PRIMARY KEY (session_id, url_norm)
  );
  ```
- [ ] **Step 7: Write failing test for `add_session_sources`/`search_session_sources`**
  ```rust
  #[test]
  fn session_sources_link_to_the_web_cache_and_are_keyword_searchable() {
      let db = Db::open_in_memory().unwrap();
      let space = db.default_space_id().unwrap();
      let s = db.create_session("t", "a/b", &space).unwrap();
      cache_put(db.raw(), "example.com/a", "https://example.com/a", Some("A"), "rust borrow checker deep dive").unwrap();
      cache_put(db.raw(), "example.com/b", "https://example.com/b", Some("B"), "cooking pasta recipes").unwrap();
      db.add_session_sources(&s.id, &["example.com/a".to_string(), "example.com/b".to_string()]).unwrap();

      let hits = db.search_session_sources(&s.id, "borrow checker").unwrap();
      assert_eq!(hits.len(), 1);
      assert!(hits[0].1.contains("borrow checker"));

      assert!(db.search_session_sources(&s.id, "quantum").unwrap().is_empty());
  }
  ```
- [ ] **Step 8: Run test to verify it fails, then write minimal implementation**
  Free functions + `Db` delegation:
  ```rust
  /// Link a research session to the (already-cached) sources its searchers
  /// gathered — the session's "source bundle" for drill-down follow-ups.
  pub fn add_session_sources(conn: &Connection, session_id: &str, url_norms: &[String]) -> Result<()> {
      for u in url_norms {
          conn.execute(
              "INSERT OR IGNORE INTO session_sources (session_id, url_norm) VALUES (?1, ?2)",
              (session_id, u),
          )?;
      }
      Ok(())
  }

  /// Keyword-search (plain substring, case-insensitive) a session's cached
  /// source bundle: `(url, text)` for every cached page whose text contains
  /// `query`. Ponytail: substring match, not FTS — the bundle is a handful
  /// of pages, not a corpus.
  pub fn search_session_sources(conn: &Connection, session_id: &str, query: &str) -> Result<Vec<(String, String)>> {
      let mut stmt = conn.prepare(
          "SELECT web_cache.url, web_cache.text FROM session_sources
           JOIN web_cache ON web_cache.url_norm = session_sources.url_norm
           WHERE session_sources.session_id = ?1",
      )?;
      let rows = stmt.query_map([session_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
      let needle = query.to_lowercase();
      Ok(rows
          .collect::<rusqlite::Result<Vec<_>>>()?
          .into_iter()
          .filter(|(_, text)| text.to_lowercase().contains(&needle))
          .collect())
  }
  ```
  Then `cargo test session_sources_link_to_the_web_cache` — expected: 1 passed.
- [ ] **Step 9: In-place stage row update — DB**
  ```rust
  /// Update the most recent `research_stage` row for `session_id` whose
  /// stored content starts with `label`, or insert one if this is the
  /// stage's first occurrence — keeps one row per named stage instead of
  /// appending on every progress tick (e.g. every searcher finishing).
  pub fn upsert_research_stage_message(&self, session_id: &str, label: &str, detail: &str) -> Result<()> {
      let content = if detail.is_empty() { label.to_string() } else { format!("{label}: {detail}") };
      let existing: Option<String> = self.conn.query_row(
          "SELECT id FROM messages WHERE session_id = ?1 AND role = 'research_stage'
            AND (content = ?2 OR content LIKE ?3)
           ORDER BY created_at DESC LIMIT 1",
          (session_id, label, format!("{label}:%")),
          |r| r.get(0),
      ).ok();
      let now = Utc::now().to_rfc3339();
      match existing {
          Some(id) => {
              self.conn.execute("UPDATE messages SET content = ?2, created_at = ?3 WHERE id = ?1", (&id, &content, &now))?;
          }
          None => {
              self.insert_message(session_id, "research_stage", &content, None, None, None, None, None)?;
          }
      }
      Ok(())
  }
  ```
  (Adjust `insert_message`'s exact arity to its real signature at implementation time.)
- [ ] **Step 10: Write failing test for `upsert_research_stage_message`**
  ```rust
  #[test]
  fn upsert_research_stage_message_replaces_the_same_labels_row() {
      let db = Db::open_in_memory().unwrap();
      let space = db.default_space_id().unwrap();
      let s = db.create_session("t", "a/b", &space).unwrap();
      db.upsert_research_stage_message(&s.id, "searching", "round 1, 1/3").unwrap();
      db.upsert_research_stage_message(&s.id, "searching", "round 1, 2/3").unwrap();
      db.upsert_research_stage_message(&s.id, "planning", "").unwrap();

      let msgs = db.load_messages(&s.id).unwrap();
      let searching: Vec<_> = msgs.iter().filter(|m| m.content.starts_with("searching:")).collect();
      assert_eq!(searching.len(), 1, "expected one row, updated in place");
      assert!(searching[0].content.contains("2/3"));
      assert_eq!(msgs.iter().filter(|m| m.content == "planning").count(), 1);
  }
  ```
- [ ] **Step 11: Run test to verify it fails, then passes**
  `cargo test upsert_research_stage_message_replaces_the_same_labels_row` — fails on missing method, passes after Step 9.
- [ ] **Step 12: Update `send_stage` and every call site in `run_research_inner`**
  ```rust
  fn send_stage(tx: &mpsc::UnboundedSender<ResearchMsg>, ids: &(String, String, String), label: impl Into<String>, detail: impl Into<String>) {
      let _ = tx.send((ids.0.clone(), ids.1.clone(), ids.2.clone(), ResearchUpdate::Stage { label: label.into(), detail: detail.into() }));
  }
  ```
  Call sites: `send_stage(tx, ids, "planning", "")`; searcher progress becomes `send_stage(tx, ids, "searching", format!("round {round}, {done}/{total}"))`; `("synthesizing", "")`; `("critiquing", "")`; `("re-synthesizing", "")`; `("critiquing", "round 2")` (same label as round 1 — intentionally replaces the same row); `("resolving a contradiction", "")`; `("verifying", "")`; `("writing final report", "")`.
- [ ] **Step 13: Update `App::on_research_done`'s `Stage` arm to upsert instead of append**
  ```rust
  ResearchUpdate::Stage { label, detail } => {
      let _ = self.db.upsert_research_stage_message(&session_id, &label, &detail);
      if viewing {
          let text = if detail.is_empty() { label.clone() } else { format!("{label}: {detail}") };
          if let Some(last) = self.messages.iter_mut().rev().find(|m| m.role == "research_stage" && (m.content.starts_with(&format!("{label}:")) || m.content == label)) {
              last.content = text.clone();
          } else {
              self.messages.push(crate::db::Message {
                  id: String::new(), role: "research_stage".to_string(), content: text.clone(),
                  model: None, reasoning: None, tokens: None, secs: None, phrase: None, images: Vec::new(),
              });
          }
          self.status = format!("research: {text}");
      }
  }
  ```
  (Session switches mid-run reload from the DB via `confirm_session`'s `load_messages`, which already reflects the single upserted row.)
- [ ] **Step 14: Update the existing stage-update test**
  Change `on_research_done_stage_update_persists_and_shows_when_viewing`'s `ResearchUpdate::Stage("planning…".to_string())` to `ResearchUpdate::Stage { label: "planning".to_string(), detail: String::new() }` and its content assertions to `"planning"`.
- [ ] **Step 15: Run tests**
  `cargo test on_research_done_stage_update` then `cargo test app::research::` — expected: all passing.
- [ ] **Step 16: Add the plan-gate field to `App`**
  In `src/app/mod.rs`:
  ```rust
  /// A running `/research` job's plan-approval gate: the `oneshot::Sender`
  /// the pipeline is awaiting, keyed by session id, plus the (possibly
  /// user-edited) sub-questions shown while the gate is open.
  pub(crate) research_plan_gate: Option<(String, tokio::sync::oneshot::Sender<Vec<String>>, Vec<String>)>,
  ```
  initialized to `None` in `App::new`.
- [ ] **Step 17-18: Wire the gate channel through `start_research` → `run_research_inner`**
  In `start_research`, before spawning:
  ```rust
  let gate_rx = if gated {
      let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
      self.research_plan_gate = Some((session.id.clone(), gate_tx, Vec::new()));
      Some(gate_rx)
  } else {
      None
  };
  ```
  Pass `gate_rx: Option<oneshot::Receiver<Vec<String>>>` through `run_research` into `run_research_inner`. After `plan(...)` succeeds there:
  ```rust
  if let Some(gate_rx) = gate_rx {
      let _ = tx.send((ids.0.clone(), ids.1.clone(), ids.2.clone(), ResearchUpdate::PlanReady { questions: questions.clone() }));
      questions = match tokio::time::timeout(std::time::Duration::from_secs(60), gate_rx).await {
          Ok(Ok(edited)) if !edited.is_empty() => edited,
          _ => questions, // timeout, or channel dropped/empty edit — auto-continue
      };
  }
  ```
  (`run_research_inner` takes `gate_rx` by value — `Option<Receiver<...>>` is moved in.)
- [ ] **Step 19: Handle `ResearchUpdate::PlanReady` in `on_research_done`**
  ```rust
  ResearchUpdate::PlanReady { questions } => {
      if let Some((gate_session, _, cached)) = self.research_plan_gate.as_mut() {
          if *gate_session == session_id {
              *cached = questions.clone();
          }
      }
      let plan_text = questions.iter().enumerate().map(|(i, q)| format!("{}. {q}", i + 1)).collect::<Vec<_>>().join("\n");
      let content = format!("Research plan ready — [e]dit / Enter to continue (auto-continues in 60s):\n{plan_text}");
      let _ = self.db.insert_message(&session_id, "research_plan", &content, None, None, None, None, None);
      if viewing {
          self.messages.push(crate::db::Message {
              id: String::new(), role: "research_plan".to_string(), content,
              model: None, reasoning: None, tokens: None, secs: None, phrase: None, images: Vec::new(),
          });
          self.status = "research plan ready — [e]dit / Enter to continue".to_string();
      }
  }
  ```
  (`role: "research_plan"` is a new message role — rendered like `research_stage` (Step 35's `push_research_plan`) and excluded from `build_history` (Step 36).)
- [ ] **Step 20: Add `App::approve_research_plan`/`edit_research_plan`/`submit_research_plan_edit`**
  ```rust
  /// Enter on a pending plan gate: continue with the Planner's (possibly
  /// already-cached) questions as-is.
  pub(crate) fn approve_research_plan(&mut self) {
      if let Some((_, tx, cached)) = self.research_plan_gate.take() {
          let _ = tx.send(cached);
          self.status = "continuing research…".to_string();
      }
  }

  /// `e` on a pending plan gate: prefill the composer with one question per
  /// line so the user can edit it like any other message.
  pub(crate) fn edit_research_plan(&mut self) {
      if let Some((_, _, cached)) = &self.research_plan_gate {
          self.set_input(&cached.join("\n"));
          self.status = "edit the plan, one question per line — Enter to submit".to_string();
      }
  }

  /// Submit an edited plan (composer contents) in place of Enter's plain
  /// approval. If the gate already timed out (`research_plan_gate` is
  /// `None`), the edit is ignored with a status message, per the spec.
  pub(crate) fn submit_research_plan_edit(&mut self, text: &str) {
      let Some((_, tx, _)) = self.research_plan_gate.take() else {
          self.status = "plan gate already closed (timed out) — this edit was ignored".to_string();
          return;
      };
      let edited = parse_plan_edit(text);
      let _ = tx.send(edited);
      self.status = "plan updated — continuing research…".to_string();
  }
  ```
- [ ] **Step 21: Parse `/research!` as a gate bypass**
  In `src/app/mod.rs`'s `run_command`, before the `match canonical` dispatch (token `"research!"` won't match `"research"` in `COMMANDS`):
  ```rust
  if let Some(rest) = cmd.strip_prefix("research!") {
      self.start_research_with_gate(rest.trim(), false);
      return Ok(());
  }
  ```
  Rename `start_research` to `start_research_with_gate(&mut self, topic: &str, gated: bool)` (keep a thin `start_research(topic)` = `start_research_with_gate(topic, true)` wrapper if existing tests call it), dispatching `"research" => self.start_research_with_gate(args, true)`.
- [ ] **Step 22: Route the `e`/Enter keys when a plan gate is open**
  In `src/events.rs`'s `handle_normal`, before the existing `KeyCode::Enter => app.submit()?` arm:
  ```rust
  KeyCode::Char('e') if app.research_plan_gate.is_some() && app.input_text().trim().is_empty() => app.edit_research_plan(),
  KeyCode::Enter if app.research_plan_gate.is_some() && app.input_text().trim().is_empty() => {
      app.approve_research_plan();
  }
  KeyCode::Enter if app.research_plan_gate.is_some() => {
      let text = app.input_text();
      app.clear_input();
      app.submit_research_plan_edit(&text);
  }
  ```
  (Plain Enter with an empty composer approves; Enter with composer text — after `e` prefilled it — submits the edit. `e` only intercepts when the composer is empty so typing words containing 'e' still works mid-edit.)
- [ ] **Step 23: Write a test for the plan-gate happy path (approve)**
  ```rust
  #[tokio::test]
  async fn plan_gate_pauses_then_approve_lets_the_cached_questions_through() {
      let mut a = test_app();
      a.research_model = "openai/gpt-5-mini".to_string();
      a.start_research("rust async runtimes");
      let session_id = a.session.as_ref().unwrap().id.clone();
      let space_id = a.active_space.id.clone();
      let space_name = a.active_space.name.clone();

      a.on_research_done(Some((
          session_id.clone(), space_id, space_name,
          ResearchUpdate::PlanReady { questions: vec!["q1".to_string(), "q2".to_string()] },
      )));
      assert!(a.research_plan_gate.is_some());
      assert!(a.messages.iter().any(|m| m.role == "research_plan"));

      a.approve_research_plan();
      assert!(a.research_plan_gate.is_none());
  }
  ```
- [ ] **Step 24: Run test**
  `cargo test plan_gate_pauses_then_approve` — expected: 1 passed. (App-side state machine only; the pipeline's real 60s timeout is manual-only per Global Constraints.)
- [ ] **Step 25: Write a test for the edit path**
  ```rust
  #[tokio::test]
  async fn plan_gate_edit_prefills_composer_and_submit_sends_parsed_questions() {
      let mut a = test_app();
      a.research_model = "openai/gpt-5-mini".to_string();
      a.start_research("rust async runtimes");
      let session_id = a.session.as_ref().unwrap().id.clone();
      let space_id = a.active_space.id.clone();
      let space_name = a.active_space.name.clone();
      a.on_research_done(Some((
          session_id, space_id, space_name,
          ResearchUpdate::PlanReady { questions: vec!["q1".to_string()] },
      )));

      a.edit_research_plan();
      assert_eq!(a.input_text(), "q1");
      a.set_input("edited one\nedited two");
      let text = a.input_text();
      a.submit_research_plan_edit(&text);
      assert!(a.research_plan_gate.is_none());
      assert!(a.status.contains("plan updated"));
  }
  ```
- [ ] **Step 26: Run test**
  `cargo test plan_gate_edit_prefills` — expected: 1 passed.
- [ ] **Step 27: Add `cited_url_norms` helper + test**
  Alongside `dedup_source_lines` in `tools.rs`:
  ```rust
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
                  out.push(normalize_url(url.trim()));
              }
          }
      }
      out
  }
  ```
  Test:
  ```rust
  #[test]
  fn cited_url_norms_extracts_every_sources_url() {
      let f = "text [1]\nSources:\n1. https://a.example/\n2. https://b.example?utm_source=x";
      assert_eq!(cited_url_norms(&[f.to_string()]), vec!["https://a.example", "https://b.example"]);
  }
  ```
- [ ] **Step 28: Persist session sources in the pipeline**
  In `run_research_inner`, after each `run_searchers(...)` call (round-1 and round-2; the cited pages are already in `web_cache` from Task 2's write-through), using the `db_path` param added in Task 6:
  ```rust
  let url_norms = crate::tools::cited_url_norms(&findings);
  if let Ok(conn) = rusqlite::Connection::open(db_path) {
      let _ = crate::db::add_session_sources(&conn, &ids.0, &url_norms);
  }
  ```
- [ ] **Step 29: Write a compose test for findings → session sources**
  ```rust
  #[test]
  fn cited_urls_from_findings_land_in_session_sources_after_persisting() {
      let db = Db::open_in_memory().unwrap();
      let space = db.default_space_id().unwrap();
      let s = db.create_session("t", "a/b", &space).unwrap();
      let finding = "answer [1]\nSources:\n1. https://example.com/a\n".to_string();
      crate::db::cache_put(db.raw(), "example.com/a", "https://example.com/a", None, "cached text").unwrap();
      let norms = crate::tools::cited_url_norms(&[finding]);
      db.add_session_sources(&s.id, &norms).unwrap();
      let hits = db.search_session_sources(&s.id, "cached text").unwrap();
      assert_eq!(hits.len(), 1);
  }
  ```
  Note: `cited_url_norms` yields full normalized URLs (`https://example.com/a`) — the cache_put key here must match (`normalize_url("https://example.com/a")` = `https://example.com/a`); align the test's key with the function's actual output.
- [ ] **Step 30: Run test**
  `cargo test cited_urls_from_findings_land_in_session_sources` — expected: 1 passed.
- [ ] **Step 31: Add the `search_sources` drill-down tool**
  Give `ToolBox` an optional `research_session_id: Option<String>` field (alongside `web_cache_db`), set only for follow-up turns inside a research session. In `defs()`:
  ```rust
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
  ```
  In `run()`:
  ```rust
  "search_sources" => {
      let query = serde_json::from_str::<serde_json::Value>(args)
          .ok()
          .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
          .unwrap_or_default();
      let status = "Searching session sources…".to_string();
      let result = match (&self.research_session_id, &self.web_cache_db) {
          (Some(session_id), Some(db_path)) => match rusqlite::Connection::open(db_path) {
              Err(e) => format!("source search failed: {e}"),
              Ok(conn) => match crate::db::search_session_sources(&conn, session_id, &query) {
                  Ok(hits) if hits.is_empty() => "no matches in this session's sources".to_string(),
                  Ok(hits) => hits.iter().map(|(url, text)| {
                      let cut: String = text.chars().take(500).collect();
                      format!("{url}:\n{cut}")
                  }).collect::<Vec<_>>().join("\n\n"),
                  Err(e) => format!("source search failed: {e}"),
              },
          },
          _ => "no session source bundle available".to_string(),
      };
      (result, status)
  }
  ```
- [ ] **Step 32: Write a test for the `search_sources` tool**
  ```rust
  #[tokio::test]
  async fn search_sources_tool_only_appears_and_works_for_a_research_session_toolbox() {
      let path = std::env::temp_dir().join(format!("nexus-searchsrc-{}.db", uuid::Uuid::new_v4()));
      let db = crate::db::Db::open(&path).unwrap();
      let space = db.default_space_id().unwrap();
      let s = db.create_session("t", "a/b", &space).unwrap();
      crate::db::cache_put(db.raw(), "example.com/a", "https://example.com/a", None, "rust borrow checker notes").unwrap();
      db.add_session_sources(&s.id, &["example.com/a".to_string()]).unwrap();

      let tb = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), Vec::new(), None, None, Some(path.clone()));
      assert!(!tb.defs().iter().any(|d| d.name == "search_sources"));

      let tb = tb.with_research_session(s.id.clone());
      assert!(tb.defs().iter().any(|d| d.name == "search_sources"));
      let (result, _) = tb.run("search_sources", r#"{"query":"borrow checker"}"#).await;
      assert!(result.contains("borrow checker"), "{result}");
  }
  ```
  (Adjust `ToolBox::new` arity to match Tasks 1-2's final constructor; the session-source key must match `normalize_url`'s output as in Step 29.)
- [ ] **Step 33: Add `ToolBox::with_research_session` and wire follow-up-turn toolbox construction**
  ```rust
  /// Attach a research session id, enabling `search_sources` for follow-up
  /// turns in that session's chat.
  pub fn with_research_session(mut self, session_id: String) -> Self {
      self.research_session_id = Some(session_id);
      self
  }
  ```
  In `App::refresh_toolbox`, chain onto the constructor when the active session came from `/research`:
  ```rust
  let mut toolbox = crate::tools::ToolBox::new(/* ...existing args... */);
  if self.is_research_session() && let Some(session) = &self.session {
      toolbox = toolbox.with_research_session(session.id.clone());
  }
  self.toolbox = std::sync::Arc::new(toolbox);
  ```
  where `App::is_research_session(&self) -> bool` is a small private helper (a session is research-born if it has any `research_stage`/`research_plan` rows — check via a cheap DB query or a flag; pick whichever is already derivable without a new column, e.g. `self.db.load_messages(...).iter().any(|m| m.role == "research_stage")`). Call `self.refresh_toolbox()` from `confirm_session`/`new_session` so entering/leaving a research session updates tool availability. Add a system-prompt note in `App::system_prompt`:
  ```rust
  if self.is_research_session() {
      parts.push("This session came from /research — prefer search_sources over web_search for follow-ups; only use web_search on a miss.".to_string());
  }
  ```
- [ ] **Step 34: Run test**
  `cargo test search_sources_tool_only_appears_and_works_for_a_research_session_toolbox` — expected: 1 passed.
- [ ] **Step 35: Add `push_research_plan` in `ui/history.rs`**
  Alongside `push_research_stage`, near-identical renderer for the `"research_plan"` role:
  ```rust
  /// A pending plan-approval message: like `push_research_stage` but with a
  /// distinct marker so it reads as an actionable prompt, not passive progress.
  fn push_research_plan(out: &mut Vec<Line<'static>>, content: &str, width: usize, theme: &crate::theme::Theme) {
      let mut first = true;
      for line in wrap_plain(content, width.saturating_sub(2)) {
          if first {
              out.push(Line::from(vec![Span::styled("📋 ", Style::default().fg(theme.accent)), Span::raw(line)]));
              first = false;
          } else {
              out.push(Line::from(format!("  {line}")));
          }
      }
      out.push(Line::from(""));
  }
  ```
  Add `} else if m.role == "research_plan" { push_research_plan(...); }` into `sync_cache`'s role dispatch chain (between the `research_stage` and `tool_call` arms). Match `push_research_stage`'s actual helper signatures at implementation time.
- [ ] **Step 36: Exclude `research_plan` from `build_history`**
  In `src/app/chat.rs::build_history`, extend the existing skip check: `if m.role == "research_stage" || m.role == "research_plan" { continue; }`.
- [ ] **Step 37: Run the full test suite**
  `cargo test` — expected: all passing across `db.rs`, `tools.rs`, `app/research.rs`, `app/tests.rs`.
- [ ] **Step 38: Manual verification**
  Run `/research some topic`: plan-approval message appears with `[e]dit / Enter to continue`; press `e`, edit a line, submit; pipeline proceeds with edited questions (visible in `searching` stage details). Run `/research! another topic`: no gate. Confirm the `searching` row updates in place. After a run, follow-up turn calls `search_sources` before `web_search` (Ctrl+T). Let a gate sit 60+ s and confirm auto-continue (manual-only per Global Constraints).
- [ ] **Step 39: Commit**
  `git commit -m "$(cat <<'EOF'
  Add research plan approval gate, in-place live stage feed, and drill-down

  /research now pauses for plan approval (edit/continue/60s auto-continue),
  updates one row per stage instead of appending, and follow-up turns in a
  research session can search its own gathered sources before hitting the
  web again.
  EOF
  )"`

### Critical Files for Implementation
- /home/dukunuu/Work/nexus-chat/src/tools.rs
- /home/dukunuu/Work/nexus-chat/src/app/research.rs
- /home/dukunuu/Work/nexus-chat/src/db.rs
- /home/dukunuu/Work/nexus-chat/src/app/mod.rs
- /home/dukunuu/Work/nexus-chat/src/app/chat.rs
