# Deep Research Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/research <topic>` command that runs a bounded multi-agent research
pipeline (planner → parallel searchers → synthesis → critic → optional gap-filling round
→ optional escalation → verifier → final writer) in the background, delivering a cited
markdown report as a chat message and a saved file in the space.

**Architecture:** A new `src/app/research.rs` module holds pure parsing/prompt-building
functions (unit tested, no network) plus the async pipeline orchestration (manually
verified, like every other network-calling background job in this codebase — see
`maybe_generate_title`). The pipeline reuses `Provider::complete` for every single-shot
stage and the existing `stream_chat` tool-loop for parallel Searcher agents, restricted to
a new `fetch_url` tool plus the existing `web_search`. Background delivery reuses the
`ocr_rx`/`embed_rx` single-flight channel pattern already established in `app/files.rs`,
and the `unread` set already established for background chat streams.

**Tech Stack:** Rust, tokio (`JoinSet` for searcher fan-out), existing `reqwest` client, no
new crates.

## Global Constraints

- No new dependencies — `fetch_url`'s HTML→text stripping reuses/extends the existing
  hand-rolled `strip_tags` approach in `tools.rs` (no HTML-parser crate).
- Every stage of the pipeline is a single `Provider::complete` call except the Searcher
  stage, which reuses `stream_chat`'s existing tool-loop machinery.
- Hard bounds, never an open-ended loop: ≤6 sub-questions, ≤2 outer rounds, a dedicated
  (smaller) tool-iteration budget for searchers, exactly one escalation call, one verify
  pass, one write pass.
- `research_stage` transcript rows are persisted and rendered but **never** replayed into
  `build_history` — unlike the `tool_call` replay fix landed earlier, these are the
  background job's own scratch work, not something the top-level chat model did.
- Follow existing patterns exactly: model-picker fields mirror `ocr_model`; background
  jobs mirror `ocr_rx`/`embed_rx`; test helpers mirror each module's existing
  `test_app()`/in-memory-db convention (no `tempfile` crate, no mocking library — this
  codebase has neither).

---

### Task 1: Provider — parametrize the tool-loop budget on `stream_chat`

Searcher agents need a much smaller tool-call budget than an interactive chat turn
(`MAX_TOOL_ITERS = 25`). Add a `max_tool_iters: usize` parameter so callers choose.

**Files:**
- Modify: `src/provider/openrouter.rs:16` (make the const `pub(crate)`), `:176-258`
  (`stream_chat`/`run_chat_loop`)
- Modify: `src/app/chat.rs:210` (the one call site)
- Test: `src/provider/openrouter.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub(crate) const MAX_TOOL_ITERS: usize = 25;` (was private `const`), and
  `OpenRouter::stream_chat(model, messages, params, tools, toolbox, max_tool_iters: usize)`
  — one new trailing parameter, otherwise unchanged.

- [ ] **Step 1: Change the const and thread the parameter through**

In `src/provider/openrouter.rs`, change line 16:

```rust
/// Hard cap on tool round-trips per response, so a model that keeps calling
/// tools can't loop forever. The default for interactive chat; background
/// jobs (e.g. deep-research searcher agents) pass their own smaller budget.
pub(crate) const MAX_TOOL_ITERS: usize = 25;
```

Change the `stream_chat` signature (currently at line 176) and its body:

```rust
    pub fn stream_chat(
        &self,
        model: String,
        messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<ToolBox>,
        max_tool_iters: usize,
    ) -> (mpsc::UnboundedReceiver<StreamEvent>, tokio::task::AbortHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let this = self.clone();
        let task = tokio::spawn(async move {
            if let Err(e) =
                this.run_chat_loop(model, messages, params, tools, toolbox, max_tool_iters, &tx).await
            {
                let _ = tx.send(StreamEvent::Error(e.to_string()));
            }
        });
        (rx, task.abort_handle())
    }

    async fn run_chat_loop(
        &self,
        model: String,
        mut messages: Vec<ChatMessage>,
        params: ChatParams,
        tools: Vec<ToolDef>,
        toolbox: Arc<ToolBox>,
        max_tool_iters: usize,
        tx: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        for iter in 0..=max_tool_iters {
            // On the final allowed iteration, omit tools and tell the model
            // why — otherwise it writes the tool call as plain text.
            let send_tools: &[ToolDef] = if iter < max_tool_iters { &tools } else { &[] };
            if iter == max_tool_iters {
                messages.push(ChatMessage::text(
                    "system",
                    "Tool budget exhausted for this turn. Do not attempt further tool calls; \
                     answer now with the information you already have.",
                ));
            }
            match self.run_stream(&model, &messages, &params, send_tools, tx).await? {
                Finish::Errored => return Ok(()),
                Finish::Done => {
                    let _ = tx.send(StreamEvent::Done);
                    return Ok(());
                }
                Finish::ToolCalls(calls, content) => {
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content,
                        tool_calls: Some(calls.clone()),
                        tool_call_id: None,
                        images: Vec::new(),
                    });
                    for call in &calls {
                        let (result, status) = toolbox.run(&call.name, &call.arguments).await;
                        let _ = tx.send(StreamEvent::Status(status));
                        let _ = tx.send(StreamEvent::ToolCall {
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            result: result.clone(),
                        });
                        messages.push(ChatMessage {
                            role: "tool".to_string(),
                            content: result,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                            images: Vec::new(),
                        });
                    }
                    let remaining = max_tool_iters - (iter + 1);
                    if let Some(m) = messages.last_mut() {
                        m.content.push_str(&format!(
                            "\n\n[{remaining} tool round-trips left this turn — plan accordingly]"
                        ));
                    }
                }
            }
        }
        let _ = tx.send(StreamEvent::Done);
        Ok(())
    }
```

(Only the two signatures and the three now-parameterized `MAX_TOOL_ITERS` references
inside the loop body change — replace every bare `MAX_TOOL_ITERS` inside
`run_chat_loop` with `max_tool_iters`.)

- [ ] **Step 2: Update the one call site**

In `src/app/chat.rs`, line 210:

```rust
        let (rx, abort) = provider.stream_chat(
            model,
            history,
            params,
            tools,
            self.toolbox.clone(),
            crate::provider::openrouter::MAX_TOOL_ITERS,
        );
```

- [ ] **Step 3: Build and run the full test suite**

Run: `cargo build && cargo test`
Expected: builds clean, all existing tests still pass (no test called `stream_chat`
directly — it's only exercised through `App`).

- [ ] **Step 4: Commit**

```bash
git add src/provider/openrouter.rs src/app/chat.rs
git commit -m "refactor: parametrize stream_chat's tool-loop budget

Deep-research searcher agents need a much smaller tool budget than an
interactive chat turn; make MAX_TOOL_ITERS a caller-supplied parameter
with the existing constant as interactive chat's default."
```

---

### Task 2: Tools — `fetch_url`, a general-purpose page-fetch tool

`web_search` only returns snippets. Recursive research (read a source, find a new term
inside it, search again) needs actual page bodies. Add `fetch_url` as an always-available
tool (like `web_search`) — useful to the main chat model too, not just research.

**Files:**
- Modify: `src/tools.rs` (add `fetch_url` to `defs()`, `run()`, plus new free functions)
- Test: `src/tools.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: free functions `strip_html_to_text(html: &str) -> String` and
  `drop_tag_blocks(html: &str, tag: &str) -> String` (pure, unit tested); the `run()` arm
  for `"fetch_url"`; a `ToolDef` named `"fetch_url"` always present in `defs()`.
- Consumes: existing `strip_tags`, `number_lines` (both already in `tools.rs`).

- [ ] **Step 1: Write the failing tests for the pure HTML-stripping helpers**

Add to the `#[cfg(test)] mod tests` block in `src/tools.rs` (near
`strip_tags_drops_markup_and_unescapes_entities`):

```rust
    #[test]
    fn drop_tag_blocks_removes_script_and_style_content() {
        let html = "<p>keep</p><script>var x = 1;</script><style>.a{color:red}</style><p>also keep</p>";
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
```

- [ ] **Step 2: Run to verify these fail**

Run: `cargo test --lib tools::tests::drop_tag_blocks_removes_script_and_style_content
tools::tests::drop_tag_blocks_handles_unterminated_tag_by_dropping_the_remainder
tools::tests::strip_html_to_text_drops_tags_scripts_styles_and_blank_lines`
Expected: FAIL — `drop_tag_blocks`/`strip_html_to_text` not found.

- [ ] **Step 3: Implement the pure helpers**

Add just below `strip_tags`/`html_unescape` in `src/tools.rs` (after line ~1072):

```rust
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

/// HTML page body → plain readable text: drop script/style blocks, strip all
/// remaining tags, unescape entities, and collapse blank/whitespace-only
/// lines so paginated output isn't mostly empty lines.
fn strip_html_to_text(html: &str) -> String {
    let no_script = drop_tag_blocks(html, "script");
    let no_style = drop_tag_blocks(&no_script, "style");
    strip_tags(&no_style)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
```

- [ ] **Step 4: Run to verify the three tests pass**

Run: same command as Step 2.
Expected: PASS.

- [ ] **Step 5: Wire the `fetch_url` tool definition and dispatch**

In `src/tools.rs`, in `defs()` (around line 175, right after the `web_search` `ToolDef`
push), add:

```rust
        defs.push(ToolDef {
            name: "fetch_url".to_string(),
            description: "Fetch a web page and return its readable text (HTML stripped), up to 200 lines per call. Use offset to page through longer pages. Use after web_search to read a promising result in full.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "the page URL to fetch" },
                    "offset": { "type": "integer", "description": "1-based first line to read (default 1)" },
                    "limit": { "type": "integer", "description": "lines to read, max 200 (default 200)" },
                },
                "required": ["url"],
            }),
        });
```

In `run()` (around line 497, right after the `"web_search"` arm), add:

```rust
            "fetch_url" => {
                let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
                let url = v.get("url").and_then(|u| u.as_str()).unwrap_or_default().to_string();
                let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
                let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).clamp(1, 200) as usize;
                let status = format!("Fetching {url}…");
                let result = match fetch_url_text(&self.client, &url).await {
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
```

Add the network helper near `duckduckgo_search` (after line ~982):

```rust
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
    let bytes = resp.bytes().await?;
    let capped = &bytes[..bytes.len().min(2_000_000)];
    let html = String::from_utf8_lossy(capped);
    Ok(strip_html_to_text(&html))
}
```

- [ ] **Step 6: Update the existing "empty toolbox has no app tools" test's tool-name list if needed, and add a defs-presence test**

Add to `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn fetch_url_is_always_available() {
        let tb = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), None, None);
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        assert!(names.contains(&"fetch_url".to_string()));
        assert!(names.contains(&"web_search".to_string()));
    }
```

- [ ] **Step 7: Run the full tools test module**

Run: `cargo test --lib tools::`
Expected: all pass, including the new ones.

- [ ] **Step 8: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add fetch_url tool for reading full page text

web_search only returns snippets; recursive research needs actual page
bodies. Reuses the existing hand-rolled HTML stripping, extended to
drop <script>/<style> blocks first. Always available, like web_search."
```

---

### Task 3: Tools — `ToolBox::research()`, a tool-restricted constructor

Searcher agents must only be able to call `web_search`/`fetch_url` — never
`run_python`/`install_packages`/`install_skill`/app tools — even if a rogue completion
hallucinates a call to one. Add an explicit restriction mode rather than relying on
`tools:` (what's offered to the model) matching `toolbox` (what's actually runnable),
which today are only kept in sync by both being derived from the same `files`/`apps`
options.

**Files:**
- Modify: `src/tools.rs` (`ToolBox` struct, `new()`, `defs()`, `run()`)
- Test: `src/tools.rs`

**Interfaces:**
- Produces: `ToolBox::research(searxng_url: Option<String>, langsearch_key: Option<String>, search_provider: String) -> ToolBox`.
- Consumes: nothing new from other tasks.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/tools.rs`:

```rust
    #[test]
    fn research_toolbox_only_offers_web_search_and_fetch_url() {
        let tb = ToolBox::research(None, None, "auto".to_string());
        let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"fetch_url".to_string()));
    }

    #[tokio::test]
    async fn research_toolbox_refuses_to_run_other_tools() {
        let tb = ToolBox::research(None, None, "auto".to_string());
        let (result, _) = tb.run("run_python", r#"{"code":"print(1)"}"#).await;
        assert!(result.contains("not available in research mode"), "{result}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib tools::tests::research_toolbox_only_offers_web_search_and_fetch_url
tools::tests::research_toolbox_refuses_to_run_other_tools`
Expected: FAIL — `ToolBox::research` not found.

- [ ] **Step 3: Add the `research_only` field, constructor, and guards**

In `src/tools.rs`, add a field to the `ToolBox` struct (near `search_provider`):

```rust
    /// When true, `defs()`/`run()` restrict to `web_search`/`fetch_url` only —
    /// used for deep-research searcher agents, which must never reach
    /// run_python/install_packages/app tools even if hallucinated.
    research_only: bool,
```

In `ToolBox::new()`, add `research_only: false,` to the struct literal.

Add the constructor right after `new()`:

```rust
    /// A toolbox restricted to `web_search`/`fetch_url` — for deep-research
    /// searcher agents, which get no filesystem/app/script access.
    pub fn research(
        searxng_url: Option<String>,
        langsearch_key: Option<String>,
        search_provider: String,
    ) -> Self {
        let mut tb = ToolBox::new(PathBuf::new(), searxng_url, langsearch_key, search_provider, None, None);
        tb.research_only = true;
        tb
    }
```

At the end of `defs()`, right before the final `defs` return line, add:

```rust
        if self.research_only {
            defs.retain(|d| d.name == "web_search" || d.name == "fetch_url");
        }
        defs
```

(replacing the current bare `defs` on the last line of the function).

At the very top of `run()`'s body (before the `match name` line), add:

```rust
        if self.research_only && !matches!(name, "web_search" | "fetch_url") {
            return (
                format!("tool '{name}' is not available in research mode"),
                "blocked".to_string(),
            );
        }
```

- [ ] **Step 4: Run to verify the tests pass**

Run: same command as Step 2.
Expected: PASS.

- [ ] **Step 5: Run the full tools test module**

Run: `cargo test --lib tools::`
Expected: all pass (including `defs_include_app_tools_only_with_apps_ctx`, which is
unaffected — it doesn't use `research_only`).

- [ ] **Step 6: Commit**

```bash
git add src/tools.rs
git commit -m "feat: add ToolBox::research(), restricted to web_search/fetch_url

Deep-research searcher agents must never be able to reach
run_python/install_packages/app tools, even on a hallucinated call —
explicit allowlist on both defs() and run(), not just what's offered."
```

---

### Task 4: DB + chat.rs — `research_stage` message role

Background-research progress lines need their own persisted, renderable, but
never-replayed-to-the-model message role — same shape as `tool_call` rows, but simpler
(plain text, no JSON) and, unlike `tool_call`, never reconstructed into history (research
stages are the job's own scratch work, not something the top-level chat model did).

**Files:**
- Modify: `src/db.rs` (new method near `add_tool_call_message`, line ~464)
- Modify: `src/app/chat.rs` (`build_history`, around line 137)
- Test: `src/db.rs`, `src/app/tests.rs`

**Interfaces:**
- Produces: `Db::add_research_stage_message(&self, session_id: &str, content: &str) -> Result<String>`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/db.rs` (find an existing test near
`add_tool_call_message` usage for the pattern, e.g. search `fn insert_message` tests):

```rust
    #[test]
    fn research_stage_messages_round_trip() {
        let db = Db::open_in_memory().unwrap();
        let space = db.default_space_id().unwrap();
        let s = db.create_session("t", "a/b", &space).unwrap();
        db.add_research_stage_message(&s.id, "planning…").unwrap();
        let msgs = db.load_messages(&s.id).unwrap();
        assert_eq!(msgs.last().unwrap().role, "research_stage");
        assert_eq!(msgs.last().unwrap().content, "planning…");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib db::tests::research_stage_messages_round_trip`
Expected: FAIL — method not found.

- [ ] **Step 3: Implement it**

In `src/db.rs`, right after `add_tool_call_message` (line ~466):

```rust
    /// Insert a background-research stage/progress line: plain text, shown in
    /// the transcript but never sent back to the model (unlike `tool_call`
    /// rows, never replayed into build_history either — this is the job's
    /// own scratch work, not something the chat model did).
    pub fn add_research_stage_message(&self, session_id: &str, content: &str) -> Result<String> {
        self.insert_message(session_id, "research_stage", content, None, None, None, None, None)
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: same command as Step 2.
Expected: PASS.

- [ ] **Step 5: Exclude `research_stage` from `build_history`, with a test**

Add to `src/app/tests.rs` (near `tool_call_events_persist_and_replay_into_history`):

```rust
#[test]
fn research_stage_rows_are_never_replayed_into_history() {
    let mut a = app_with_key();
    a.current_model = Some("a/one".into());
    let space = a.active_space.id.clone();
    let s = a.db.create_session("t", "a/one", &space).unwrap();
    a.session = Some(s.clone());
    a.db.add_research_stage_message(&s.id, "planning…").unwrap();
    a.messages = a.db.load_messages(&s.id).unwrap();

    let h = a.build_history();
    assert!(h.iter().all(|m| m.role != "research_stage"));
    assert!(h.iter().all(|m| !m.content.contains("planning…")));
}
```

Run it to verify it fails first: `cargo test --lib app::tests::research_stage_rows_are_never_replayed_into_history`
(FAIL — the row isn't filtered yet, it falls through to the `else` branch and gets sent
as a plain message with role `"research_stage"`, which the wire format will happily
serialize, so the content leaks through).

In `src/app/chat.rs`, in `build_history` (around line 137), add a new early check right
before the existing `tool_call` handling:

```rust
            if m.role == "research_stage" {
                continue;
            }
            if m.role == "tool_call" {
```

- [ ] **Step 6: Run both new tests**

Run: `cargo test --lib db::tests::research_stage_messages_round_trip app::tests::research_stage_rows_are_never_replayed_into_history`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/db.rs src/app/chat.rs src/app/tests.rs
git commit -m "feat: research_stage message role, persisted but never replayed

Background-research progress lines get their own role: rendered in the
transcript like tool_call rows, but unlike tool_call, never reconstructed
into build_history — this is the job's own scratch work, not something
the chat model did."
```

---

### Task 5: App state — settings, model-pick target, event plumbing

Add the state surface the rest of the feature hangs off: `research_model`/
`escalation_model` settings, a `ModelPickTarget`/`SettingsField` pair for each, the
`AppEvent::Research` variant, and the background-job channel/tracking fields.

**Files:**
- Modify: `src/app/mod.rs` (many small additions, detailed below)
- Test: `src/app/tests.rs` (settings load/save round trip)

**Interfaces:**
- Produces: `App.research_model: String`, `App.escalation_model: String`,
  `App.research_rx: Option<mpsc::UnboundedReceiver<ResearchMsg>>`,
  `App.research_running: Option<(String, String)>` (session id, topic),
  `pub type ResearchMsg = (String, String, String, research::ResearchUpdate);`
  (session id, space id, space name, update), `ModelPickTarget::Research`,
  `ModelPickTarget::Escalation`, `SettingsField::ResearchModel`,
  `SettingsField::EscalationModel`, `AppEvent::Research(Option<ResearchMsg>)`.
- Consumes: `research::ResearchUpdate` (defined in Task 8) — declare
  `mod research;` in `src/app/mod.rs`'s module list (alongside `mod files;` etc, line
  ~22) now so this task compiles once Task 8 lands; until then, add a minimal
  placeholder `src/app/research.rs` containing just:

  ```rust
  //! Deep research: a background multi-agent pipeline triggered by `/research`.

  /// A background research pipeline update: a phase label, or the final
  /// report/error.
  pub(crate) enum ResearchUpdate {
      Stage(String),
      Done(std::result::Result<String, String>),
  }
  ```

  (Task 8 replaces this file with the full implementation — same file, so no conflict.)

- [ ] **Step 1: Add the module declaration and placeholder file**

In `src/app/mod.rs`, add `mod research;` to the module list (alphabetically, after
`mod models;` and before `mod sessions;`, matching the existing alphabetical ordering):

```rust
mod models;
mod research;
mod sessions;
```

Create `src/app/research.rs` with exactly the placeholder content shown above.

- [ ] **Step 2: Add the `ResearchMsg` type alias and `AppEvent::Research` variant**

Right after `pub type EmbedMsg = ...` (line 347), add:

```rust
/// A background research pipeline update: (session id, space id, space name,
/// stage update or final result).
pub type ResearchMsg = (String, String, String, research::ResearchUpdate);
```

In the `AppEvent` enum, right after the `OcrPull` variant (line ~370), add:

```rust
    /// A deep-research pipeline update, or `None` when its channel closed.
    Research(Option<ResearchMsg>),
```

- [ ] **Step 3: Add `ModelPickTarget` variants**

In the `ModelPickTarget` enum (line ~163), add two variants after `Ocr`:

```rust
pub enum ModelPickTarget {
    #[default]
    Session,
    Memory,
    Transcriber,
    Ocr,
    Research,
    Escalation,
}
```

- [ ] **Step 4: Add `SettingsField` variants**

In the `SettingsField` enum (line ~173) and its `ALL` array (line ~193), add
`ResearchModel` and `EscalationModel` after `OcrModel`/before `OcrEngine` (grouping the
model-picker fields together):

```rust
pub enum SettingsField {
    ShowStats,
    ShowReasoning,
    HideHints,
    Temperature,
    TopP,
    MaxTokens,
    MemoryModel,
    CompactThreshold,
    SearxngUrl,
    Verbosity,
    LangsearchKey,
    SearchProvider,
    TranscriberModel,
    OcrModel,
    ResearchModel,
    EscalationModel,
    OcrEngine,
    EmbeddingModel,
}

impl SettingsField {
    pub const ALL: [SettingsField; 18] = [
        SettingsField::ShowStats,
        SettingsField::ShowReasoning,
        SettingsField::HideHints,
        SettingsField::Temperature,
        SettingsField::TopP,
        SettingsField::MaxTokens,
        SettingsField::MemoryModel,
        SettingsField::CompactThreshold,
        SettingsField::SearxngUrl,
        SettingsField::Verbosity,
        SettingsField::LangsearchKey,
        SettingsField::SearchProvider,
        SettingsField::TranscriberModel,
        SettingsField::OcrModel,
        SettingsField::ResearchModel,
        SettingsField::EscalationModel,
        SettingsField::OcrEngine,
        SettingsField::EmbeddingModel,
    ];
```

Add to `label()`:

```rust
            SettingsField::ResearchModel => "research model (Enter to pick, Backspace clears)",
            SettingsField::EscalationModel => "escalation model (Enter to pick, Backspace clears; blank = same as research model)",
```

Add to `text_index()` (both new fields are picker-only, like `OcrModel`):

```rust
            SettingsField::ShowStats
            | SettingsField::ShowReasoning
            | SettingsField::HideHints
            | SettingsField::MemoryModel
            | SettingsField::Verbosity
            | SettingsField::SearchProvider
            | SettingsField::TranscriberModel
            | SettingsField::OcrModel
            | SettingsField::ResearchModel
            | SettingsField::EscalationModel
            | SettingsField::OcrEngine => None,
```

- [ ] **Step 5: Add the `App` fields**

Near `pub ocr_model: String` (line ~391), add:

```rust
    /// Model used for every deep-research pipeline stage except escalation
    /// (empty = /research disabled).
    pub research_model: String,
    /// Model used only for the deep-research escalation (contradiction
    /// resolution) stage; empty = falls back to `research_model`.
    pub escalation_model: String,
```

Near `pub(crate) ocr_pull_rx` (line ~436), add:

```rust
    /// A running `/research` job's channel, or None when idle.
    pub(crate) research_rx: Option<mpsc::UnboundedReceiver<ResearchMsg>>,
    /// (session id, topic) of the `/research` job currently running, if any —
    /// cleared when its channel closes.
    pub(crate) research_running: Option<(String, String)>,
```

- [ ] **Step 6: Wire defaults, load, save**

In `Default for Settings`... no — these two fields live on `App` directly (like
`ocr_model`), not on `Settings`. In `App::new()`'s struct literal (near
`ocr_model: "google/gemini-2.5-flash-lite".to_string(),`, line ~646), add:

```rust
            research_model: "google/gemini-2.5-flash".to_string(),
            escalation_model: "anthropic/claude-sonnet-4.5".to_string(),
```

Near `ocr_pull_rx: None,` (line ~624), add:

```rust
            research_rx: None,
            research_running: None,
```

In `load_settings` (near `"ocr_model" => self.ocr_model = v,`, line ~758), add:

```rust
                "research_model" => self.research_model = v,
                "escalation_model" => self.escalation_model = v,
```

- [ ] **Step 7: Wire the `next_event()` select arm**

In `next_event()` (right after the `ocr_pull_rx` arm, line ~929), add:

```rust
            r = async {
                match self.research_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Research(r),
```

- [ ] **Step 8: Write and run a settings round-trip test**

Add to `src/app/tests.rs`, mirroring `searxng_url_setting_persists_and_enables_web_search_tool`'s
exact shape (same file, ~line 397):

```rust
#[test]
fn research_and_escalation_model_settings_persist() {
    let db = Db::open_in_memory().unwrap();
    let mut a = App::new(db, Some("k".into()), test_space());
    a.research_model = "openai/gpt-5-mini".to_string();
    a.db.set_setting("research_model", &a.research_model).unwrap();
    a.escalation_model = "anthropic/claude-sonnet-4.5".to_string();
    a.db.set_setting("escalation_model", &a.escalation_model).unwrap();

    let reloaded = a.db.load_settings().unwrap();
    assert!(reloaded.iter().any(|(k, v)| k == "research_model" && v == "openai/gpt-5-mini"));

    // Reloading a fresh App from the same db picks it back up.
    let mut b = App::new(a.db, Some("k".into()), test_space());
    b.load_settings();
    assert_eq!(b.research_model, "openai/gpt-5-mini");
    assert_eq!(b.escalation_model, "anthropic/claude-sonnet-4.5");
}
```

Run: `cargo test --lib app::tests::research_and_escalation_model_settings_persist`
Expected: PASS.

- [ ] **Step 9: Full build + test**

Run: `cargo build && cargo test`
Expected: builds clean, all pass.

- [ ] **Step 10: Commit**

```bash
git add src/app/mod.rs src/app/research.rs src/app/tests.rs
git commit -m "feat: app state for deep research — settings, events, channels

research_model/escalation_model settings, ModelPickTarget::{Research,
Escalation}, SettingsField::{ResearchModel,EscalationModel},
AppEvent::Research, and the research_rx/research_running background-job
fields. Pipeline itself lands in the next tasks."
```

---

### Task 6: Model picker wiring

Wire the two new settings fields into the existing model-picker popup, exactly mirroring
`OcrModel`.

**Files:**
- Modify: `src/app/models.rs`
- Modify: `src/ui/popups/settings.rs`
- Modify: `src/ui/popups/model.rs`
- Modify: `src/app/settings.rs` (`save_settings`, trims + persists)

**Interfaces:**
- Consumes: `ModelPickTarget::{Research,Escalation}`, `SettingsField::{ResearchModel,EscalationModel}` (Task 5).
- Produces: `App::open_model_picker_for_research()`, `App::open_model_picker_for_escalation()`,
  `App::clear_research_model()`, `App::clear_escalation_model()`.

- [ ] **Step 1: Add picker-opening methods**

In `src/app/models.rs`, right after `open_model_picker_for_ocr` (line ~35), add:

```rust
    /// Open the same model picker, but a confirmed pick sets the research
    /// model (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_research(&mut self) {
        self.model_pick_target = ModelPickTarget::Research;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the escalation
    /// model (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_escalation(&mut self) {
        self.model_pick_target = ModelPickTarget::Escalation;
        self.open_model_picker_impl();
    }
```

- [ ] **Step 2: Add `pick_model` match arms**

In `pick_model` (line ~302), add two arms after the `ModelPickTarget::Ocr` arm:

```rust
            ModelPickTarget::Research => {
                self.research_model = id.clone();
                self.db.set_setting("research_model", &id)?;
                self.status = format!("research model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Escalation => {
                self.escalation_model = id.clone();
                self.db.set_setting("escalation_model", &id)?;
                self.status = format!("escalation model: {id}");
                self.popup = Popup::Settings;
            }
```

- [ ] **Step 3: Add `clear_*` methods**

Right after `clear_ocr_model` (near line ~359), add:

```rust
    /// Reset the research model to blank (Backspace on its row in
    /// `/config`) — disables `/research`.
    pub(crate) fn clear_research_model(&mut self) -> Result<()> {
        self.research_model.clear();
        self.db.set_setting("research_model", "")?;
        self.status = "research model cleared — /research disabled".to_string();
        Ok(())
    }

    /// Reset the escalation model to blank (Backspace on its row in
    /// `/config`) — /research falls back to the research model for its
    /// contradiction-resolution stage.
    pub(crate) fn clear_escalation_model(&mut self) -> Result<()> {
        self.escalation_model.clear();
        self.db.set_setting("escalation_model", "")?;
        self.status = "escalation model cleared — falls back to research model".to_string();
        Ok(())
    }
```

- [ ] **Step 4: Wire the settings popup's picker group**

In `src/ui/popups/settings.rs`, `handle_key` (line ~109), extend the `picker` match:

```rust
    let picker = matches!(
        app.settings_field(),
        SettingsField::MemoryModel
            | SettingsField::TranscriberModel
            | SettingsField::OcrModel
            | SettingsField::ResearchModel
            | SettingsField::EscalationModel
    );
```

Extend the `Enter`/`Backspace` match arms inside that block:

```rust
            KeyCode::Enter => match app.settings_field() {
                SettingsField::MemoryModel => app.open_model_picker_for_memory(),
                SettingsField::OcrModel => app.open_model_picker_for_ocr(),
                SettingsField::ResearchModel => app.open_model_picker_for_research(),
                SettingsField::EscalationModel => app.open_model_picker_for_escalation(),
                _ => app.open_model_picker_for_transcriber(),
            },
            KeyCode::Backspace => match app.settings_field() {
                SettingsField::MemoryModel => app.clear_memory_model()?,
                SettingsField::OcrModel => app.clear_ocr_model()?,
                SettingsField::ResearchModel => app.clear_research_model()?,
                SettingsField::EscalationModel => app.clear_escalation_model()?,
                _ => app.clear_transcriber_model()?,
            },
```

- [ ] **Step 5: Render the value cells**

In the same file's `value` closure (line ~45), add:

```rust
            SettingsField::ResearchModel => numeric(&app.research_model),
            SettingsField::EscalationModel => numeric(&app.escalation_model),
```

- [ ] **Step 6: Wire the model-picker popup's title and Esc-back behavior**

In `src/ui/popups/model.rs`, extend the `fav_title` match (line ~19):

```rust
    let fav_title = match app.model_pick_target {
        crate::app::ModelPickTarget::Memory => " ★ Favorites — picking memory model ",
        crate::app::ModelPickTarget::Transcriber => " ★ Favorites — picking image model ",
        crate::app::ModelPickTarget::Ocr => " ★ Favorites — picking OCR model ",
        crate::app::ModelPickTarget::Research => " ★ Favorites — picking research model ",
        crate::app::ModelPickTarget::Escalation => " ★ Favorites — picking escalation model ",
        crate::app::ModelPickTarget::Session => " ★ Favorites ",
    };
```

Extend the `Esc` arm's `popup = match ...` (line ~107):

```rust
            app.popup = match app.model_pick_target {
                crate::app::ModelPickTarget::Memory
                | crate::app::ModelPickTarget::Transcriber
                | crate::app::ModelPickTarget::Ocr
                | crate::app::ModelPickTarget::Research
                | crate::app::ModelPickTarget::Escalation => Popup::Settings,
                crate::app::ModelPickTarget::Session => Popup::None,
            };
```

- [ ] **Step 7: Update `save_settings`**

In `src/app/settings.rs`, `save_settings` (line ~99), add near the `ocr_model` lines:

```rust
        self.research_model = self.research_model.trim().to_string();
        self.db.set_setting("research_model", &self.research_model)?;
        self.escalation_model = self.escalation_model.trim().to_string();
        self.db.set_setting("escalation_model", &self.escalation_model)?;
```

- [ ] **Step 8: Build and run the full suite**

Run: `cargo build && cargo test`
Expected: builds clean (this task only adds match arms/rendering, no new tests
required — behavior is exercised by the Task 5 settings-persistence test plus manual
verification in Task 13).

- [ ] **Step 9: Commit**

```bash
git add src/app/models.rs src/app/settings.rs src/ui/popups/settings.rs src/ui/popups/model.rs
git commit -m "feat: model-picker wiring for research/escalation models

Mirrors the existing ocr_model picker pattern exactly: /config rows,
Enter to pick, Backspace to clear, picker title and Esc-back behavior."
```

---

### Task 7: `/research` command wiring

**Files:**
- Modify: `src/input.rs` (`COMMANDS`)
- Modify: `src/app/mod.rs` (`run_command`)

**Interfaces:**
- Consumes: `App::start_research(&mut self, topic: &str)` (defined in Task 10 — until
  then, this task's dispatch line won't compile; land Task 7 and Task 10 as one
  reviewed unit if your workflow requires every intermediate commit to build, or accept
  the temporary non-build between them if your workflow allows within-plan staging).
  **To keep every commit green, do this task's Step 1 now, and Step 2 (the dispatch
  line) as part of Task 10 instead** — see Task 10 Step 6.

- [ ] **Step 1: Add the command to `COMMANDS`**

In `src/input.rs`, in the `COMMANDS` array (near the `"ocr-local"` entry, line ~58), add:

```rust
    Command { name: "research", desc: "deep multi-agent research on a topic (background)", aliases: &["deep-research"] },
```

- [ ] **Step 2: Build and run the full suite**

Run: `cargo build && cargo test`
Expected: builds clean (the command is listed/discoverable in the `/` autocomplete but
not yet dispatched — `run_command`'s `other =>` fallback arm will currently treat
`/research topic` as an attempted skill invocation, which is fine as a temporary state
since Task 10 wires the real dispatch).

- [ ] **Step 3: Commit**

```bash
git add src/input.rs
git commit -m "feat: list /research in the command palette (dispatch lands with the pipeline)"
```

---

### Task 8: `research.rs` — pure parsing and prompt-building functions

These are the pieces that can be TDD'd without network: turning a Planner's raw text
into sub-questions, a Critic's raw text into a structured decision, and topic/findings
into the message lists sent to each stage. This task **replaces** the Task 5 placeholder
file with real content — same file path, so it's an in-place edit, not a new file.

**Files:**
- Modify: `src/app/research.rs` (was a placeholder from Task 5)
- Test: `src/app/research.rs` (new `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::provider::{ChatMessage}` (existing).
- Produces (used by Task 9): `parse_subquestions(text: &str) -> Vec<String>`,
  `Critique` enum + `parse_critique(text: &str) -> Critique`, prompt constants
  `PLANNER_PROMPT`/`SEARCHER_PROMPT`/`SYNTHESIZER_PROMPT`/`CRITIC_PROMPT`/
  `ESCALATION_PROMPT`/`VERIFIER_PROMPT`/`WRITER_PROMPT`, message-builders
  `planner_messages`/`synthesizer_messages`/`critic_messages`/`escalation_messages`/
  `verifier_messages`/`writer_messages`, constants `MAX_SUBQUESTIONS: usize = 6` and
  `RESEARCH_SEARCHER_MAX_ITERS: usize = 6`. Keeps the Task 5 `ResearchUpdate` enum
  (unchanged).

- [ ] **Step 1: Write the failing tests for `parse_subquestions`**

Replace `src/app/research.rs` with the header/enum from Task 5 plus this test module
(the rest of the file's real content is added in the following steps — write the whole
file once, tests included, since this is a single edit):

```rust
//! Deep research: a background multi-agent pipeline triggered by `/research`.
//! Every stage but the Searcher fan-out is a single `Provider::complete`
//! call; parsing/prompt-building here is pure and unit tested. The async
//! orchestration (Task 9) calls real network endpoints and is exercised
//! manually, like every other network-calling background job in this
//! codebase (`maybe_generate_title`, image description, embedding).

use crate::provider::ChatMessage;

/// A background research pipeline update: a phase label, or the final
/// report/error.
pub(crate) enum ResearchUpdate {
    Stage(String),
    Done(std::result::Result<String, String>),
}

/// Hard cap on Planner-generated sub-questions per outer round.
const MAX_SUBQUESTIONS: usize = 6;
/// Tool-call budget for a single Searcher agent — a few search→fetch hops,
/// not a whole interactive conversation's worth.
pub(crate) const RESEARCH_SEARCHER_MAX_ITERS: usize = 6;

const PLANNER_PROMPT: &str = "You are the planning stage of an automated research pipeline. Given a research topic, decompose it into 3 to 6 focused sub-questions that together cover the topic thoroughly (different angles: definitions, current state, evidence/data, controversies, practical implications — whichever apply). Respond with ONLY a JSON array of strings, no prose, no markdown fences. Example: [\"question one\", \"question two\"]";

pub(crate) const SEARCHER_PROMPT: &str = "You are a research searcher agent. You will be given one focused sub-question. Use the web_search and fetch_url tools to investigate it thoroughly: search, then fetch and read the most promising pages, and search again with new terms you learn from them if needed. When you have enough to answer well, write a concise findings summary (a few paragraphs, prose, no headers) that directly answers the sub-question, citing sources inline as [n]. End your answer with a line starting exactly with 'Sources:' followed by the numbered list of URLs you used, one per line, matching your [n] citations.";

const SYNTHESIZER_PROMPT: &str = "You are the synthesis stage of a research pipeline. You'll be given the original topic and findings from several searcher agents, each already citing their own sources. Combine them into a single coherent draft report on the topic: organize by theme (not by sub-question), resolve obvious overlaps, keep every citation but you may renumber them consistently as you merge. Do not invent facts not present in the findings. Output the draft report in markdown, no preamble.";

const CRITIC_PROMPT: &str = "You are the critic stage of a research pipeline. Given the original topic and a draft report, decide if it's ready. Respond in exactly one of these forms:\n- the single word SATISFIED, if the draft thoroughly covers the topic with no notable gaps or contradictions.\n- GAPS: followed by a newline-separated bullet list (each line starting with '- ') of specific missing sub-topics or unanswered angles, each phrased as a searchable question.\n- CONTRADICTION: followed by one line describing a specific factual contradiction between sources in the draft that isn't resolved.\nUse CONTRADICTION only for an actual conflict between sources, not a missing angle — missing angles are always GAPS. Respond with nothing else.";

const ESCALATION_PROMPT: &str = "You are resolving a contradiction found in a research draft. You are given the topic, the draft, the full set of source findings gathered so far, and a description of the contradiction. Determine which claim the evidence better supports (or that both apply in different contexts) and write one paragraph resolving it, citing the [n] sources involved. Output only that paragraph.";

const VERIFIER_PROMPT: &str = "You are the verifier stage. Given the topic, the gathered source findings (with their citations), and a draft report, check every factual claim in the draft against the source findings. Rewrite the draft unchanged except: remove or mark with '⚠ unverifiable:' any claim not actually supported by the gathered findings. Output the corrected draft in markdown, nothing else.";

const WRITER_PROMPT: &str = "You are the final writer stage. Given the topic and a verified draft report (with inline [n] citations and prose from earlier stages, possibly including a contradiction-resolution paragraph to fold in), produce the final report: clean markdown, a short introductory paragraph, organized sections with headers, inline [n] citations preserved/renumbered consistently, and a trailing '## Sources' section listing every cited URL as 'n. url'. Output only the final report markdown, nothing else — it will be saved and shown to the user as-is.";

/// The Critic stage's structured decision.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Critique {
    Satisfied,
    Gaps(Vec<String>),
    Contradiction(String),
}

/// Parse the Planner's raw reply into sub-questions: a JSON string array, or
/// (if the model didn't follow instructions) a best-effort line-by-line
/// fallback stripping bullet/number prefixes. Always capped at
/// `MAX_SUBQUESTIONS`.
pub(crate) fn parse_subquestions(text: &str) -> Vec<String> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Vec<String>>(trimmed) {
        return v
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .take(MAX_SUBQUESTIONS)
            .collect();
    }
    trimmed
        .lines()
        .map(strip_list_prefix)
        .filter(|l| !l.is_empty())
        .take(MAX_SUBQUESTIONS)
        .collect()
}

/// Strip a leading `-`, `*`, or `N.`/`N)` list-item marker, if present.
fn strip_list_prefix(line: &str) -> String {
    let s = line.trim().trim_start_matches(['-', '*']).trim();
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end > 0 {
        if let Some(rest) = s[digits_end..].strip_prefix(['.', ')']) {
            return rest.trim().to_string();
        }
    }
    s.to_string()
}

/// Parse the Critic's raw reply into a `Critique`. Anything that doesn't
/// match one of the three expected shapes is treated as `Satisfied` — an
/// unparseable critique shouldn't loop the pipeline forever on garbage.
pub(crate) fn parse_critique(text: &str) -> Critique {
    let t = text.trim();
    if t.eq_ignore_ascii_case("SATISFIED") {
        return Critique::Satisfied;
    }
    if let Some(rest) = t.strip_prefix("CONTRADICTION:") {
        let desc = rest.trim();
        if !desc.is_empty() {
            return Critique::Contradiction(desc.to_string());
        }
    }
    if let Some(rest) = t.strip_prefix("GAPS:") {
        let gaps: Vec<String> = rest
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix('-'))
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(MAX_SUBQUESTIONS)
            .collect();
        if !gaps.is_empty() {
            return Critique::Gaps(gaps);
        }
    }
    Critique::Satisfied
}

fn planner_messages(topic: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::text("system", PLANNER_PROMPT), ChatMessage::text("user", topic)]
}

fn synthesizer_messages(topic: &str, findings: &[String]) -> Vec<ChatMessage> {
    let body = findings
        .iter()
        .enumerate()
        .map(|(i, f)| format!("--- Searcher {} findings ---\n{f}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    vec![
        ChatMessage::text("system", SYNTHESIZER_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\n{body}")),
    ]
}

fn critic_messages(topic: &str, draft: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", CRITIC_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nDraft:\n{draft}")),
    ]
}

fn escalation_messages(topic: &str, draft: &str, findings: &[String], contradiction: &str) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text("system", ESCALATION_PROMPT),
        ChatMessage::text(
            "user",
            format!("Topic: {topic}\n\nContradiction: {contradiction}\n\nDraft:\n{draft}\n\nSource findings:\n{body}"),
        ),
    ]
}

fn verifier_messages(topic: &str, draft: &str, findings: &[String]) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text("system", VERIFIER_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nSource findings:\n{body}\n\nDraft:\n{draft}")),
    ]
}

fn writer_messages(topic: &str, verified_draft: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", WRITER_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nVerified draft:\n{verified_draft}")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_subquestions_reads_a_clean_json_array() {
        let qs = parse_subquestions(r#"["what is X", "how does Y work"]"#);
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string()]);
    }

    #[test]
    fn parse_subquestions_strips_markdown_fences() {
        let qs = parse_subquestions("```json\n[\"a\", \"b\"]\n```");
        assert_eq!(qs, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_subquestions_falls_back_to_bullet_lines() {
        let qs = parse_subquestions("- what is X\n- how does Y work\n* a third one");
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string(), "a third one".to_string()]);
    }

    #[test]
    fn parse_subquestions_falls_back_to_numbered_lines() {
        let qs = parse_subquestions("1. what is X\n2) how does Y work");
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string()]);
    }

    #[test]
    fn parse_subquestions_caps_at_max() {
        let lines: Vec<String> = (0..10).map(|i| format!("- q{i}")).collect();
        let qs = parse_subquestions(&lines.join("\n"));
        assert_eq!(qs.len(), MAX_SUBQUESTIONS);
    }

    #[test]
    fn parse_critique_recognizes_satisfied() {
        assert_eq!(parse_critique("SATISFIED"), Critique::Satisfied);
        assert_eq!(parse_critique("  satisfied  "), Critique::Satisfied);
    }

    #[test]
    fn parse_critique_recognizes_gaps() {
        let c = parse_critique("GAPS:\n- what about pricing?\n- any recent incidents?");
        assert_eq!(
            c,
            Critique::Gaps(vec!["what about pricing?".to_string(), "any recent incidents?".to_string()])
        );
    }

    #[test]
    fn parse_critique_recognizes_contradiction() {
        let c = parse_critique("CONTRADICTION: source A says X, source B says not-X");
        assert_eq!(c, Critique::Contradiction("source A says X, source B says not-X".to_string()));
    }

    #[test]
    fn parse_critique_falls_back_to_satisfied_on_garbage() {
        assert_eq!(parse_critique("uh, looks fine I guess?"), Critique::Satisfied);
        assert_eq!(parse_critique("GAPS:\n"), Critique::Satisfied);
    }

    #[test]
    fn synthesizer_messages_includes_topic_and_all_findings() {
        let msgs = synthesizer_messages("rust async runtimes", &["finding one".to_string(), "finding two".to_string()]);
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[1].content.contains("rust async runtimes"));
        assert!(msgs[1].content.contains("finding one"));
        assert!(msgs[1].content.contains("finding two"));
    }

    #[test]
    fn critic_messages_includes_topic_and_draft() {
        let msgs = critic_messages("topic X", "draft text");
        assert!(msgs[1].content.contains("topic X"));
        assert!(msgs[1].content.contains("draft text"));
    }

    #[test]
    fn escalation_messages_includes_contradiction_description() {
        let msgs = escalation_messages("t", "draft", &["f1".to_string()], "A vs B");
        assert!(msgs[1].content.contains("A vs B"));
        assert!(msgs[1].content.contains("f1"));
    }

    #[test]
    fn writer_messages_includes_verified_draft() {
        let msgs = writer_messages("t", "verified content");
        assert!(msgs[1].content.contains("verified content"));
    }
}
```

- [ ] **Step 2: Run to verify all new tests pass**

Run: `cargo test --lib app::research::`
Expected: PASS for every test in Step 1 (this step is "write then run", combined,
since the functions are pure and were written alongside their tests above — if you're
executing this plan literally task-by-task rather than trusting the combined listing,
write the non-test code first, confirm the tests fail on a stub, then confirm they pass;
either way, end state is: all tests in this module pass).

- [ ] **Step 3: Full build + test**

Run: `cargo build && cargo test`
Expected: builds clean (note: `verifier_messages` is unused until Task 9 — expect an
"unused function" warning; that's fine, Task 9 consumes it within the same module).

- [ ] **Step 4: Commit**

```bash
git add src/app/research.rs
git commit -m "feat: deep-research prompt/parsing layer, unit tested

Pure functions only: parse_subquestions, parse_critique, and the
per-stage message builders. No network — the orchestration that calls
these lands next."
```

---

### Task 9: `research.rs` — pipeline orchestration

The async pipeline itself: Planner → parallel Searchers → Synthesis → Critic →
(optional round 2) → (optional escalation) → Verifier → Final Writer. Calls real
`Provider::complete`/`stream_chat` — exercised manually (Task 13), like every other
network-calling background job in this codebase.

**Files:**
- Modify: `src/app/research.rs` (append to the file from Task 8)

**Interfaces:**
- Consumes: `crate::provider::openrouter::OpenRouter`, `crate::provider::{ChatParams, StreamEvent}`,
  `crate::tools::ToolBox`, `ResearchMsg` (Task 5), everything from Task 8.
- Produces: `pub(crate) async fn run_research(provider: OpenRouter, research_model: String, escalation_model: String, topic: String, toolbox: std::sync::Arc<ToolBox>, tx: tokio::sync::mpsc::UnboundedSender<ResearchMsg>, session_id: String, space_id: String, space_name: String)`.

- [ ] **Step 1: Add the imports and orchestration code**

Append to `src/app/research.rs` (after the `#[cfg(test)] mod tests` block — Rust
allows this; the test module can stay where it is or move to the very end, either
compiles the same, but for a single clean diff, insert this new code **before** the
`#[cfg(test)] mod tests` line from Task 8):

```rust
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatParams, StreamEvent};
use crate::tools::ToolBox;

use super::ResearchMsg;

/// Send the `(session_id, space_id, space_name)` triple's stage update.
fn send_stage(tx: &mpsc::UnboundedSender<ResearchMsg>, ids: &(String, String, String), s: impl Into<String>) {
    let _ = tx.send((ids.0.clone(), ids.1.clone(), ids.2.clone(), ResearchUpdate::Stage(s.into())));
}

async fn complete_text(provider: &OpenRouter, model: &str, messages: Vec<ChatMessage>) -> Result<String, String> {
    provider.complete(model, messages).await.map(|s| s.trim().to_string()).map_err(|e| e.to_string())
}

async fn plan(provider: &OpenRouter, model: &str, topic: &str) -> Result<Vec<String>, String> {
    let text = complete_text(provider, model, planner_messages(topic)).await?;
    let qs = parse_subquestions(&text);
    if qs.is_empty() {
        return Err(format!("planner returned no usable sub-questions (raw reply: {text:.200})"));
    }
    Ok(qs)
}

/// One Searcher agent: given a single sub-question, runs the normal
/// tool-loop (restricted to web_search/fetch_url) and returns its final
/// prose findings (including its own "Sources:" citation list). Never
/// returns an `Err` — a dead search/fetch/model call becomes a placeholder
/// finding string so one bad sub-question can't sink the whole pipeline.
async fn run_searcher(provider: &OpenRouter, model: &str, sub_question: &str, toolbox: Arc<ToolBox>) -> String {
    let messages = vec![
        ChatMessage::text("system", SEARCHER_PROMPT),
        ChatMessage::text("user", sub_question),
    ];
    let tools = toolbox.defs();
    let (mut rx, _abort) = provider.stream_chat(
        model.to_string(),
        messages,
        ChatParams::default(),
        tools,
        toolbox,
        RESEARCH_SEARCHER_MAX_ITERS,
    );
    let mut buf = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Error(e) => return format!("[search agent error on \"{sub_question}\": {e}]"),
            StreamEvent::Done => break,
            _ => {}
        }
    }
    let text = buf.trim();
    if text.is_empty() {
        format!("[no findings for \"{sub_question}\"]")
    } else {
        text.to_string()
    }
}

/// Fan out one Searcher per question in parallel, sending a running
/// `{done}/{total}` stage update as each finishes. Order of the returned
/// findings doesn't matter (synthesis treats them as an unordered set).
async fn run_searchers(
    provider: &OpenRouter,
    model: &str,
    toolbox: &Arc<ToolBox>,
    questions: &[String],
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    round: usize,
) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    for q in questions.iter().cloned() {
        let provider = provider.clone();
        let model = model.to_string();
        let toolbox = toolbox.clone();
        set.spawn(async move { run_searcher(&provider, &model, &q, toolbox).await });
    }
    let total = questions.len();
    let mut done = 0usize;
    let mut findings = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        done += 1;
        send_stage(tx, ids, format!("searching (round {round}, {done}/{total})…"));
        findings.push(res.unwrap_or_else(|e| format!("[search agent panicked: {e}]")));
    }
    findings
}

/// Run the full pipeline and send exactly one final `Done` on `tx` (the
/// caller's channel then closes naturally when this function returns and
/// `tx` is dropped).
pub(crate) async fn run_research(
    provider: OpenRouter,
    research_model: String,
    escalation_model: String,
    topic: String,
    toolbox: Arc<ToolBox>,
    tx: mpsc::UnboundedSender<ResearchMsg>,
    session_id: String,
    space_id: String,
    space_name: String,
) {
    let ids = (session_id, space_id, space_name);
    let result = run_research_inner(&provider, &research_model, &escalation_model, &topic, &toolbox, &tx, &ids).await;
    let _ = tx.send((ids.0, ids.1, ids.2, ResearchUpdate::Done(result)));
}

async fn run_research_inner(
    provider: &OpenRouter,
    research_model: &str,
    escalation_model: &str,
    topic: &str,
    toolbox: &Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> Result<String, String> {
    send_stage(tx, ids, "planning…");
    let questions = plan(provider, research_model, topic).await?;

    let mut findings = run_searchers(provider, research_model, toolbox, &questions, tx, ids, 1).await;

    send_stage(tx, ids, "synthesizing…");
    let mut draft = complete_text(provider, research_model, synthesizer_messages(topic, &findings)).await?;

    send_stage(tx, ids, "critiquing…");
    let mut critique = parse_critique(&complete_text(provider, research_model, critic_messages(topic, &draft)).await?);

    if let Critique::Gaps(gaps) = &critique {
        let more = run_searchers(provider, research_model, toolbox, gaps, tx, ids, 2).await;
        findings.extend(more);
        send_stage(tx, ids, "re-synthesizing…");
        draft = complete_text(provider, research_model, synthesizer_messages(topic, &findings)).await?;
        send_stage(tx, ids, "critiquing (round 2)…");
        critique = parse_critique(&complete_text(provider, research_model, critic_messages(topic, &draft)).await?);
    }

    if let Critique::Contradiction(desc) = &critique {
        send_stage(tx, ids, "resolving a contradiction…");
        let resolution =
            complete_text(provider, escalation_model, escalation_messages(topic, &draft, &findings, desc)).await?;
        draft.push_str("\n\n");
        draft.push_str(&resolution);
    }

    send_stage(tx, ids, "verifying…");
    let verified = complete_text(provider, research_model, verifier_messages(topic, &draft, &findings)).await?;

    send_stage(tx, ids, "writing final report…");
    complete_text(provider, research_model, writer_messages(topic, &verified)).await
}
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: builds clean. `run_research`/`run_research_inner` are not yet called from
anywhere (Task 10 wires that), so expect a "never used" warning until then — acceptable
mid-plan, same as Task 7/8's temporary states.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: all pass (no new tests in this step — this task is orchestration glue over
already-tested pure functions and already-tested `stream_chat`/`complete`; it's
exercised end-to-end manually in Task 13, matching how `maybe_generate_title`'s network
call is treated).

- [ ] **Step 4: Commit**

```bash
git add src/app/research.rs
git commit -m "feat: deep-research pipeline orchestration

Planner -> parallel Searchers -> Synthesis -> Critic -> optional
round-2 gap search -> optional escalation -> Verifier -> Final Writer.
Bounded: <=6 sub-questions, <=2 outer rounds, one escalation call.
Wired to App in the next task."
```

---

### Task 10: `App::start_research` / `App::on_research_done`

Wires the pipeline into the app: `/research <topic>` creates a new session, switches
into it, spawns the pipeline, and background updates land via `research_rx` exactly like
`ocr_rx`/`embed_rx`.

**Files:**
- Modify: `src/app/research.rs` (append `impl App` block)
- Modify: `src/app/mod.rs` (`run_command` dispatch — completes Task 7)
- Modify: `src/app/sessions.rs` (make `slugify` visible to `research.rs`)
- Test: `src/app/research.rs`

**Interfaces:**
- Produces: `App::start_research(&mut self, topic: &str)`, `App::on_research_done(&mut self, r: Option<ResearchMsg>)`.
- Consumes: `super::title_from` (chat.rs, already `pub(super)`), `super::sessions::slugify`
  (made `pub(super)` in this task), `Db::add_research_stage_message`/`add_assistant_message`
  (existing), `ToolBox::research` (Task 3), `run_research` (Task 9).

- [ ] **Step 1: Make `slugify` visible to sibling modules**

In `src/app/sessions.rs` (line ~167), change:

```rust
/// Normalise to a short kebab-case slug: lowercase, `[a-z0-9-]`, max 5 words.
pub(super) fn slugify(s: &str) -> String {
```

(was a bare private `fn slugify`).

- [ ] **Step 2: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/app/research.rs` (extending the one
from Task 8). That module currently opens with `use super::*;` only, which brings in
`research.rs`'s own items but not `App` (referenced inside `research.rs` as
`super::App`, never `use`d locally) — add two more imports at the top of the test
module:

```rust
    use crate::app::App;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root = std::env::temp_dir().join(format!("nexus-research-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k".into()), space)
    }

    #[test]
    fn start_research_rejects_blank_topic_and_missing_model() {
        let mut a = test_app();
        a.start_research("  ");
        assert!(a.status.contains("usage:"));
        assert!(a.research_rx.is_none());

        a.research_model.clear();
        a.start_research("rust async runtimes");
        assert!(a.status.contains("no research model configured"));
        assert!(a.research_rx.is_none());
    }

    #[test]
    fn start_research_creates_and_switches_into_a_new_session() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        assert!(a.research_rx.is_some());
        assert!(a.research_running.is_some());
        let session = a.session.as_ref().expect("switched into the research session");
        assert!(session.title.contains("rust async runtimes"));
        assert!(a.messages.iter().any(|m| m.content.contains("/research rust async runtimes")));
    }

    #[test]
    fn start_research_refuses_a_second_concurrent_job() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("topic one");
        assert!(a.research_rx.is_some());
        a.start_research("topic two");
        assert!(a.status.contains("already running"));
        // Still the first job's session.
        assert!(a.session.as_ref().unwrap().title.contains("topic one"));
    }

    #[test]
    fn on_research_done_stage_update_persists_and_shows_when_viewing() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::Stage("planning…".to_string()),
        )));

        assert!(a.messages.iter().any(|m| m.role == "research_stage" && m.content == "planning…"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "research_stage" && m.content == "planning…"));
        assert!(a.status.contains("planning…"));
    }

    #[test]
    fn on_research_done_final_report_posts_message_saves_file_and_notifies_when_away() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        // Simulate the user navigating away before the job finishes.
        a.session = None;
        a.messages.clear();

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name.clone(),
            ResearchUpdate::Done(Ok("# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string())),
        )));

        assert!(a.unread.contains(&session_id));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "assistant" && m.content.contains("Rust Async Runtimes")));

        // Saved into the space's files dir and picked up by a rescan.
        let dir = a.space.files_dir(&space_name);
        let saved = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).count();
        assert_eq!(saved, 1, "expected exactly one saved report file in {dir:?}");
    }

    #[test]
    fn on_research_done_failure_posts_error_message() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        a.on_research_done(Some((session_id.clone(), space_id, space_name, ResearchUpdate::Done(Err("planner: network down".to_string())))));

        assert!(a.status.contains("network down"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "assistant" && m.content.contains("network down")));
    }

    #[test]
    fn on_research_done_none_clears_channel_and_running_state() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("t");
        assert!(a.research_rx.is_some());
        a.on_research_done(None);
        assert!(a.research_rx.is_none());
        assert!(a.research_running.is_none());
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test --lib app::research::`
Expected: FAIL — `App::start_research`/`App::on_research_done` not found.

- [ ] **Step 4: Implement `start_research` and `on_research_done`**

Append to `src/app/research.rs` (an `impl App` block, after the pipeline code and
before `#[cfg(test)]`):

```rust
impl super::App {
    /// `/research <topic>`: run the multi-agent research pipeline in a new
    /// background session. One job at a time.
    pub(crate) fn start_research(&mut self, topic: &str) {
        let topic = topic.trim().to_string();
        if topic.is_empty() {
            self.status = "usage: /research <topic>".to_string();
            return;
        }
        if self.research_model.trim().is_empty() {
            self.status = "no research model configured — set one in /config".to_string();
            return;
        }
        if self.research_rx.is_some() {
            self.status = "a research job is already running".to_string();
            return;
        }
        let Some(provider) = self.provider.clone() else {
            self.open_key_prompt();
            return;
        };
        let research_model = self.research_model.trim().to_string();
        let escalation_model = if self.escalation_model.trim().is_empty() {
            research_model.clone()
        } else {
            self.escalation_model.trim().to_string()
        };
        let title = super::title_from(&topic);
        let session = match self.db.create_session(&title, &research_model, &self.active_space.id) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("could not start research session: {e}");
                return;
            }
        };
        let _ = self.db.add_user_message(&session.id, &format!("/research {topic}"));

        let searxng_url = (!self.searxng_url.trim().is_empty()).then(|| self.searxng_url.trim().to_string());
        let langsearch_key = (!self.langsearch_key.trim().is_empty()).then(|| self.langsearch_key.trim().to_string());
        let toolbox = Arc::new(ToolBox::research(searxng_url, langsearch_key, self.search_provider.clone()));

        let (tx, rx) = mpsc::unbounded_channel();
        self.research_rx = Some(rx);
        self.research_running = Some((session.id.clone(), topic.clone()));
        self.status = format!("researching: {topic}");

        let space_id = self.active_space.id.clone();
        let space_name = self.active_space.name.clone();
        self.messages = self.db.load_messages(&session.id).unwrap_or_default();
        self.session = Some(session.clone());
        self.context_total = None;
        self.scroll = 0;

        tokio::spawn(run_research(
            provider,
            research_model,
            escalation_model,
            topic,
            toolbox,
            tx,
            session.id,
            space_id,
            space_name,
        ));
    }

    /// A research pipeline update: a stage label, or the final report/error.
    /// `None` = the job's channel closed (fires once, right after `Done`).
    pub fn on_research_done(&mut self, r: Option<ResearchMsg>) {
        let Some((session_id, space_id, space_name, update)) = r else {
            self.research_rx = None;
            self.research_running = None;
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            ResearchUpdate::Stage(s) => {
                let _ = self.db.add_research_stage_message(&session_id, &s);
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "research_stage".to_string(),
                        content: s.clone(),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                    });
                    self.status = format!("research: {s}");
                }
            }
            ResearchUpdate::Done(Ok(report)) => {
                let _ = self.db.add_assistant_message(&session_id, &report, None, None, None, None, None);
                let topic = self.research_running.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
                self.save_research_report(&space_id, &space_name, &topic, &report);
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "assistant".to_string(),
                        content: report,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: Some("Researched".to_string()),
                        images: Vec::new(),
                    });
                    self.status = "research complete".to_string();
                } else {
                    self.unread.insert(session_id);
                    if let Some((_, topic)) = &self.research_running {
                        self.status = format!("✓ research ready: {topic}");
                    }
                }
            }
            ResearchUpdate::Done(Err(e)) => {
                let msg = format!("research failed: {e}");
                let _ = self.db.add_assistant_message(&session_id, &msg, None, None, None, None, None);
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "assistant".to_string(),
                        content: msg.clone(),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                    });
                }
                self.status = msg;
            }
        }
    }

    /// Save the finished report into the job's own space (not necessarily
    /// the currently active one — the user may have switched spaces while
    /// the job ran), named `research-<slug>-<timestamp>.md`. Only refreshes
    /// the files cache / triggers a rescan if that space is still active;
    /// otherwise the file sits on disk and gets picked up next time that
    /// space's /files is opened, same as any externally-dropped file.
    fn save_research_report(&mut self, space_id: &str, space_name: &str, topic: &str, report: &str) {
        let dir = self.space.files_dir(space_name);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let slug = super::sessions::slugify(topic);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = format!("research-{slug}-{stamp}.md");
        if std::fs::write(dir.join(&name), report).is_err() {
            return;
        }
        if space_id == self.active_space.id {
            self.rescan_files();
        }
    }
}
```

- [ ] **Step 5: Run to verify the tests pass**

Run: `cargo test --lib app::research::`
Expected: PASS. If `start_research`/`on_research_done` need to be `pub(crate)` rather
than `pub(crate)`/`pub` as written to compile from `events.rs` (Task 12) or
`run_command` (Step 6 below), adjust visibility — match the existing convention where
`on_ocr_done` is `pub fn` (called from `events.rs`, a sibling top-level module) and
`start_ocr` is `pub(crate) fn` (called only from within `app::`).

- [ ] **Step 6: Wire the `run_command` dispatch (completes Task 7)**

`run_command` (line ~970) matches on `canonical`, which alias resolution (line ~973-977)
already maps `"deep-research"` to the canonical name `"research"` before the match runs —
so only one arm is needed. In `src/app/mod.rs`, `run_command` (near the `"ocr-local"`
arm, line ~1000), add:

```rust
            "research" => self.start_research(cmd[token.len()..].trim()),
```

- [ ] **Step 7: Full build + test**

Run: `cargo build && cargo test`
Expected: builds clean, all pass.

- [ ] **Step 8: Commit**

```bash
git add src/app/research.rs src/app/mod.rs src/app/sessions.rs
git commit -m "feat: wire /research into App — session, background job, notify

start_research creates a session, switches into it, and spawns the
pipeline; on_research_done persists stages/final report, saves the
report into the job's own space (correct even if the user switched
spaces mid-job), and reuses the existing unread-notification path."
```

---

### Task 11: UI — render `research_stage` rows and a running indicator

**Files:**
- Modify: `src/ui/history.rs` (render `research_stage` messages)
- Modify: `src/ui/popups/session.rs` (picker glyph)
- Modify: `src/ui/mod.rs` (input hint)

**Interfaces:**
- Consumes: `App.research_running: Option<(String, String)>` (Task 5).

- [ ] **Step 1: Render `research_stage` rows in the transcript**

In `src/ui/history.rs`, `sync_cache` (line ~111), add a branch before the `tool_call`
check:

```rust
        if m.role == "user" {
            ...
        } else if m.role == "research_stage" {
            push_research_stage(&mut c.lines, &m.content, width);
        } else if m.role == "tool_call" {
```

Add the render function near `push_tool_call` (after line ~198):

```rust
/// A background-research progress line: a dim one-liner with a 🔎 marker,
/// no expand/collapse (unlike tool_call — there's no arguments/result pair,
/// just a phase label).
fn push_research_stage(out: &mut Vec<Line<'static>>, content: &str, width: usize) {
    let mut first = true;
    for line in wrap_plain(content, width.saturating_sub(2)) {
        if first {
            out.push(Line::from(vec![
                Span::styled("🔎 ", Style::default().fg(Color::Magenta)),
                dim(line),
            ]));
            first = false;
        } else {
            out.push(Line::from(dim(format!("  {line}"))));
        }
    }
    out.push(Line::from(""));
}
```

- [ ] **Step 2: Add a picker glyph for the running research job**

In `src/ui/popups/session.rs`, the marker logic (lines ~26-35) currently reads:

```rust
            // ⟳ = a response is streaming here; ● = finished while unviewed.
            let streaming_here =
                app.stream_session.as_ref().is_some_and(|(id, _)| *id == s.id);
            let marker = if streaming_here {
                Some(Span::styled("⟳ ", Style::default().fg(Color::Cyan)))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled("● ", Style::default().fg(Color::Yellow)))
            } else {
                None
            };
```

Change it to:

```rust
            // ⟳ = a response is streaming here; 🔎 = a research job is running
            // here; ● = finished while unviewed.
            let streaming_here =
                app.stream_session.as_ref().is_some_and(|(id, _)| *id == s.id);
            let researching_here =
                app.research_running.as_ref().is_some_and(|(id, _)| *id == s.id);
            let marker = if streaming_here {
                Some(Span::styled("⟳ ", Style::default().fg(Color::Cyan)))
            } else if researching_here {
                Some(Span::styled("🔎 ", Style::default().fg(Color::Magenta)))
            } else if app.unread.contains(&s.id) {
                Some(Span::styled("● ", Style::default().fg(Color::Yellow)))
            } else {
                None
            };
```

(`mlen`, a few lines below, already just checks `marker.is_some()`, so it needs no
change — both glyphs are 2 display columns wide, same as the existing two.)

- [ ] **Step 3: Add an input hint for a research job running elsewhere**

In `src/ui/mod.rs`, `render_input` (lines ~69-78) currently reads:

```rust
    let hint = if app.settings.hide_hints {
        String::new()
    } else if app.viewing_stream() {
        " …working (Esc to stop) ".to_string()
    } else if let Some((_, title)) = app.stream_session.as_ref().filter(|_| app.is_streaming()) {
        format!(" ⟳ streaming in: {title} ")
    } else {
        " message (Enter to send, /help) ".to_string()
    };
```

Change it to add a research-elsewhere branch right before the final fallback (after the
streaming-elsewhere check, so a stream takes priority if somehow both are running):

```rust
    let hint = if app.settings.hide_hints {
        String::new()
    } else if app.viewing_stream() {
        " …working (Esc to stop) ".to_string()
    } else if let Some((_, title)) = app.stream_session.as_ref().filter(|_| app.is_streaming()) {
        format!(" ⟳ streaming in: {title} ")
    } else if let Some((id, topic)) = app.research_running.as_ref().filter(|(id, _)| {
        app.session.as_ref().is_none_or(|s| &s.id != id)
    }) {
        let _ = id;
        format!(" 🔎 researching: {topic} ")
    } else {
        " message (Enter to send, /help) ".to_string()
    };
```

(The `filter` suppresses the hint while the user is actually looking at the research
session itself — the 🔎 stage lines already streaming into the transcript are enough
there; the hint is only useful as an away-from-it reminder, same reasoning as the
streaming hint being gated by `viewing_stream()` taking precedence over it.)

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: builds clean. UI rendering has no dedicated automated tests in this codebase
beyond the `HistoryCache` unit tests already present — this task is verified manually in
Task 13.

- [ ] **Step 5: Commit**

```bash
git add src/ui/history.rs src/ui/popups/session.rs src/ui/mod.rs
git commit -m "feat: render research_stage transcript rows and a running indicator

Progress lines show as dim one-liners with a 🔎 marker; the session
picker and input hint gain a researching-elsewhere indicator, mirroring
the existing streaming-elsewhere ⟳ pattern."
```

---

### Task 12: Wire `AppEvent::Research` dispatch

**Files:**
- Modify: `src/events.rs`

**Interfaces:**
- Consumes: `AppEvent::Research` (Task 5), `App::on_research_done` (Task 10).

- [ ] **Step 1: Add the dispatch arm**

In `src/events.rs`, near the other `AppEvent::*` arms (line ~120), add:

```rust
                AppEvent::Research(r) => app.on_research_done(r),
```

- [ ] **Step 2: Build and run the full suite**

Run: `cargo build && cargo test`
Expected: builds clean, all pass. This is the last piece of wiring — the feature is now
fully connected end to end.

- [ ] **Step 3: Commit**

```bash
git add src/events.rs
git commit -m "feat: dispatch AppEvent::Research to on_research_done"
```

---

### Task 13: Full verification pass

**Files:** none (verification only).

- [ ] **Step 1: Full automated suite**

Run: `cargo build && cargo test`
Expected: 0 warnings, all tests pass.

- [ ] **Step 2: Manual smoke test**

Run the app (`cargo run`), set `research_model` in `/config` to a real cheap model (e.g.
`google/gemini-2.5-flash`) via the picker, then run `/research <a real topic>`. Confirm:
- A new session appears and becomes active immediately, showing the `/research <topic>`
  user line.
- `🔎` stage lines appear in order: planning…, searching (round 1, N/M)…, synthesizing…,
  critiquing…, (possibly round 2 lines), (possibly a contradiction-resolution line),
  verifying…, writing final report….
- The final report appears as a normal assistant message with `[n]` citations and a
  `## Sources` section.
- Switch to a different session mid-run (before it finishes) and confirm: the `🔎`
  indicator/hint shows it's still running elsewhere, and when it finishes, the status
  bar shows `✓ research ready: <topic>` and the session picker marks it unread.
- Open `/files` in the space the job ran in and confirm `research-<slug>-<date>.md`
  is listed with status `ok` and is searchable via `search_files`/`read_file` in a normal
  chat turn.
- Try `/research` with no topic — confirm the `usage:` status message.
- Try starting a second `/research` while one is running — confirm the "already
  running" status message and that the first job's session stays active.

- [ ] **Step 3: Report back**

Summarize what was verified and any deviations from the plan (e.g. if the manual
`run_command` token-matching in Task 10 Step 6 needed the second `"deep-research"` arm
after all, or if `slugify`'s or `start_research`'s visibility needed adjusting from what
the plan specified).
