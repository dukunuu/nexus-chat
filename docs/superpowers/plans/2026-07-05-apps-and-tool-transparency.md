# Apps + Transparent Tool Calls Implementation Plan

> Executed inline (user-waived subagents). Spec:
> docs/superpowers/specs/2026-07-05-apps-and-tool-transparency-design.md

**Goal:** Model can write/edit static web apps served at `127.0.0.1:8642`,
and every tool call shows as a persisted, expandable block in the transcript.

## Global Constraints
- No new dependencies (server hand-rolled on tokio).
- Tool files confined to `spaces/<active-space>/apps/`; reject `..`/absolute.
- Tool-call rows: `messages.role = "tool_call"`, content JSON
  `{"name","arguments","result"}`; skipped by `build_history`.
- Server: GET/HEAD only, `Cache-Control: no-store`, traversal 404s.
- Toggle key Ctrl+O for expanded tool detail.

### Task 1: static app server (`src/appserver.rs`)
- `AppServer::start(apps_root: PathBuf) -> AppServer` (bind 8642, fallback :0);
  `port()`, `base_url()`. Request loop: parse request line, map path, serve.
- Tests over real reqwest GETs: mime, index.html, 404, traversal, 405, no-store.

### Task 2: app tools (`src/tools.rs` + system prompt)
- ToolBox gains apps_dir + server base URL + space name.
- `write_file`, `edit_file`, `read_app_file` defs + run arms + validation.
- `## Apps` system-prompt section in chat.rs listing existing apps.
- Tests: round trip, confinement, edit errors, URL in result.

### Task 3: StreamEvent::ToolCall + persistence
- provider: emit after each toolbox.run; app: persist role "tool_call" row
  mid-stream; `build_history` skips role "tool_call".
- Tests: build_history exclusion, db round trip.

### Task 4: transcript rendering + Ctrl+O
- history.rs renders tool_call rows as `⚒ name summary` dimmed line;
  expanded (app.show_tool_detail) shows args + result. events.rs: Ctrl+O.
- Tests: summary per tool shape, toggle.
