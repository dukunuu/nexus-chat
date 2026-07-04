# Model-Created Apps + Transparent Tool Calls — Design

Date: 2026-07-05
Status: approved

## Problem

1. The model has no way to build anything for the user — e.g. "make me a small
   presentation" should produce a served HTML page and a link, editable in
   later turns like Claude Code edits files.
2. Tool activity is invisible: `tool_status` is a transient spinner line, gone
   when the stream ends. The user cannot see what the model searched, read,
   wrote, or edited — neither live nor in history.

## Decisions (user-approved)

| Question | Decision |
|---|---|
| App storage | `spaces/<space>/apps/<app-name>/`, durable, owned by the space. |
| Server | One always-on static server, `127.0.0.1:8642` (fallback: bind `:0`, remember actual port). No exec, no dev servers. |
| Edit tools | `write_file`, `edit_file` (exact-match replace), plus `read_app_file` so older/hand-edited apps stay editable. |
| Transparency | Every tool call renders as its own collapsed one-liner block in the transcript, expandable, persisted to db. Applies to all tools. |

## Architecture

### Static server (`src/appserver.rs`)
- Hand-rolled on `tokio::net::TcpListener`, no new deps. Started once at app
  startup; holds the apps root (`<spaces-root>`) and the bound port.
- `GET /<space>/<app>/<path…>` → `spaces/<space>/apps/<app>/<path…>`;
  empty/dir path serves `index.html`. Everything else: 404.
- Traversal guard: canonicalize the resolved path and require it stays under
  the canonicalized apps root; reject otherwise (404).
- Small extension→MIME map (html, css, js, mjs, json, png, jpg/jpeg, gif,
  svg, webp, ico, wasm, txt, md; default `application/octet-stream`).
- `Cache-Control: no-store` on every response so edits show on refresh.
- Only GET (and HEAD) supported; other methods → 405.
- Bind failure on 8642 → retry with port 0; total failure → app still runs,
  tools report "app server not running" in their results.

### App tools (in `ToolBox`, `src/tools.rs`)
All three take an `app` name and a relative `path`; both components are
validated: reject absolute paths, `..` segments, and empty names. Files are
confined to `spaces/<active-space>/apps/`.

- `write_file(app, path, content)` — creates parent dirs, overwrites. Result:
  `wrote <app>/<path> (<size>) — live at http://127.0.0.1:<port>/<space>/<app>/`.
- `edit_file(app, path, old_string, new_string)` — exact-match replace.
  0 matches → error "old_string not found"; >1 matches → error "old_string
  matches N places — make it unique". Result names the file and match count.
- `read_app_file(app, path, offset?, limit?)` — ranged read like `read_file`
  (200-line cap), for editing apps written in earlier sessions or hand-edited.
- System prompt gains an `## Apps` section: how the tools work, the base URL,
  and the list of existing app names in the active space (from a dir listing).

### Transparent tool blocks
- Provider: new `StreamEvent::ToolCall { name: String, arguments: String,
  result: String }`, sent by `run_chat_loop` after each tool completes.
  The live `StreamEvent::Status` ("Searching…") stays for the in-flight
  spinner.
- Persistence: tool calls are stored as `messages` rows with
  `role = "tool_call"` and `content` = JSON `{"name","arguments","result"}`.
  No schema change. They are appended in order between the surrounding user
  and assistant messages during the streaming turn.
- `build_history` skips `tool_call` rows (the model already consumed tool
  results in-turn; resending would duplicate and confuse).
- Rendering (`src/ui/history.rs`): a `tool_call` message renders as one
  dimmed line: `⚒ <name> <summary>` where summary is tool-specific
  (search_files: the query + hit count; read_file/read_app_file: name +
  range; write_file: path + human size; edit_file: path; web_search: query;
  skill: skill name; fallback: truncated args). Expanded state shows the
  full arguments and result (wrapped, scrollable as normal transcript text).
- Expand/collapse: one global toggle key, Ctrl+O, flips showing tool detail
  for the whole transcript (per-block cursor selection is out of scope v1).
- Live behavior: when a `ToolCall` event arrives mid-stream it is persisted
  and appears in the transcript immediately, above the still-streaming
  answer.

## Out of scope
- Running commands / dev servers; non-static apps.
- Per-block expand cursor; diff rendering for edit_file (expanded view shows
  old/new strings as text).
- Serving across the network (binds localhost only).
- An `apps` db table — the filesystem is the source of truth.

## Verification
- Server unit tests (ephemeral port, real HTTP over reqwest): serves file
  with right MIME, dir → index.html, 404 missing, traversal (`/../`) blocked,
  405 on POST, no-store header present.
- Tool tests: write→read→edit round trip on a temp space; confinement
  rejections; edit 0-match and N-match errors; result strings carry the URL.
- Transparency tests: tool_call row round-trips through db and is skipped by
  build_history; summary line per tool shape; Ctrl+O toggles expanded text.
- Manual: ask for a presentation → link opens in browser; edit request →
  slide changes on refresh; transcript shows ⚒ blocks; Ctrl+O expands.
