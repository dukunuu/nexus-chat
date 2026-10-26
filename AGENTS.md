# AGENTS.md

Guidance for AI coding agents working in this repository. Read this before
making changes — it covers the gates, conventions, and the shape of the code.

## What this is

`nexus-chat` is a local-first terminal chat app (Rust, [ratatui]) for deep
research and multi-agent work. All state lives on the user's machine: a
SQLite database (rows scoped by *space*), files/scripts/apps in space
directories, and
model-created web apps served from `localhost:8642`. It talks to any
OpenAI-wire backend (OpenRouter, OpenAI, OpenCode Zen/Go, Codex) through one
client. The binary is named `nexus` (package name `nexus-chat`).

## Commands

```sh
cargo build                    # build the workspace (binary: nexus)
cargo run                      # run the TUI
cargo test --workspace         # 500+ tests; none touch the network
scripts/check.sh               # THE gate: fmt + clippy + audit + tests
scripts/check.sh --fix         # auto-fix fmt/clippy, then run the gate
```

Pushing to master auto-bumps the patch version, tags `vX.Y.Z`, and releases
(crates.io + GitHub) via `.github/workflows/version-bump.yml`; a manual `v*`
tag push publishes the same way. Workflows live in `.github/workflows/`.
Release notes are generated from conventional commits since the last tag
(`scripts/release-notes.sh`).

Every commit runs `scripts/check.sh` via the local pre-commit hook — a commit
that fails the gate is rejected. **Never disable the hook, and never commit
code that fails `cargo clippy -- -D warnings -W clippy::pedantic`.**

## Conventions

- **Rust edition 2024**, `rustc 1.91+`. `cargo fmt` is mandatory.
- **Clippy at pedantic with warnings denied** — write code that satisfies it
  (explicit lifetimes where needed, `#[must_use]` on pure fns, docs on public
  items, no `unwrap()` on fallible paths). Prefer params structs over
  `too_many_arguments` allows.
- **Tests**: app-level integration tests live in `crates/core/src/app/tests.rs` and
  `crates/tui/src/ui/popups/tests.rs` (ratatui `TestBackend` snapshot style). Keep tests
  hermetic — no network, no real key material, no XDG writes (use temp dirs).
- **Commit style**: lowercase conventional prefix, imperative, em-dash
  detail. Examples from history: `feat:`, `fix:`, `design:`, `docs:`,
  `style:`, `refactor:`, `test:`, `chore:`, `build:`, `license:`. One logical
  change per commit; run `scripts/check.sh` before committing.
- **Secrets**: API keys come from `~/.config/nexus-chat/config.toml` or env
  vars (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `OPENCODE_API_KEY`). Never
  hardcode or commit keys.
- **cargo-audit**: gated in `scripts/check.sh`. `.cargo/audit.toml` ignores
  two advisories with written rationale — only extend that list when a
  warning is provably unreachable, and say why.

## Code map

The README has the full tree; the parts agents touch most:

| Path | What lives there |
|---|---|
| `crates/core/src/app/mod.rs` | `App` struct (domain fields only), popup state, gate plumbing, `boot()`, `snapshot()` |
| `crates/core/src/app/commands.rs` | `AppCommand` seam + `/`-command catalog (`COMMANDS`, `fuzzy_score`): `parse_command` + `execute` (the `/`-string front is `run_command`) |
| `crates/core/src/app/chat.rs` | request lifecycle: history build, streaming, tool loop |
| `crates/core/src/app/research.rs` | research pipeline: survey → plan → searchers → synthesis → critic → verifier → writer |
| `crates/core/src/app/swarm.rs`, `watches.rs` | roundtable / standing jobs |
| `crates/core/src/app/usage.rs` | usage & cost analytics (`/usage`) |
| `crates/core/src/app/sessions.rs`, `spaces.rs`, `models.rs`, `memory.rs` | state backends |
| `crates/core/src/provider/openrouter.rs` | the single client for all OpenAI-wire backends — message shapes, tool-call wire format, events are in `provider/mod.rs` |
| `crates/core/src/provider/serve.rs` | managed local-provider servers: per-runtime launch commands, endpoint liveness probes, process-tree memory, and the advisory budget behind `/local start\|stop\|status` |
| `crates/core/src/provider/local.rs`, `harmony.rs` | opt-in local runtimes: config + installed-model discovery, and the Harmony channel splitter for `gpt-oss` servers that leave framing in `content` |
| `crates/core/src/provider/embedding.rs` | file-search embeddings: `EmbedInput` (text/image), `Embedder` (OpenAI-wire or Ollama native `/api/embed`), probed `EmbedIdentity` used as the vector cache key, strict response validation; `app/embeddings.rs` selects/probes it automatically (local default only when reachable, no setting), `extract/embedding.rs` is the token-bounded chunker |
| `crates/core/src/tools.rs` | the model's tools + the `ToolExecutor` seam (`defs`/`is_read_only`/`run`) |
| `crates/core/src/connectors/` | MCP connectors (docs/connectors.md): `protocol`/`transport`/`client` speak both protocol families over HTTP and stdio, `registry` tracks status and what the model may see (naming, >30-tool meta-tool, policy, rug-pull pins), `exec` runs a call through the approval flow, `import`/`secrets`/`config` cover other tools' configs, the keychain and `[connectors.*]` |
| `crates/core/src/db.rs` | SQLite (rusqlite bundled): sessions, messages, usage, citations, model prefs |
| `crates/core/src/sync.rs` | Phase 3 merge engine: changeset types, per-table registry (cursor + apply rules), build/apply/ack, file blobs + zip bundles |
| `crates/core/src/panels.rs` | durable nexus-ui panel storage: per-panel saved state (`panel_states`) and panels pinned as space mini-apps (`pinned_panels`), the `panel_app` tool's list/read/patch; both tables sync (`sync.rs`). Records are `interactive::PanelSnapshot` (versioned, ≤48 KiB). Terminal half: `crates/tui/src/interactive/persist.rs` |
| `crates/core/src/appserver.rs` | localhost static server for model-created apps (port 8642) |
| `crates/core/src/host/` | `nexus host` HTTP/SSE API, OpenAI-wire gateway, sync/worker routes, Cloudflare setup, tunnel and sleep guard |
| `crates/tui/src/app_view.rs` | `AppView`: wraps `App` (Deref) with all view state — composer, popup chrome/caches, render state, theme, status line |
| `crates/tui/src/flows/` | popup flow methods moved out of core by 2e (`open_*_popup`, `move_*_selection`, `confirm_*`) |
| `crates/tui/src/composer.rs` | TextArea ops: editing, clipboard, slash/`@` autocomplete (catalog stays core) |
| `crates/tui/src/ui/` | ratatui rendering; `history.rs` is the main view, `popups/` one module per popup, `markdown.rs` the styled renderer |
| `crates/core/src/config.rs`, `space.rs` | credentials + spaces layout (XDG dirs) |

> Phase 2 status: the workspace split (2a), `ToolExecutor` seam (2b),
> command/event seam (2c), CLI on the seam (2d), and the status-event
> conversion are landed. **2e (view state extraction) is done**: `App` is
> domain-only with zero TUI deps; the TUI's `AppView` owns the composer,
> popup chrome, and render caches and feeds view feedback back via
> `AppEvent` (`ComposerSet`/`ComposerClear`/`ViewportReset`/
> `HistoryInvalidated`). Core still carries a few domain-coupled display
> fields (documented in `app/mod.rs`): `context_total`/`last_cache_rate`
> (auto-compaction), `unread`, `notifications`, the files/scripts/sessions
> caches (system-prompt inputs), and the research steer/stage state.
>
> Phase 4 host support lives in `crates/core/src/host/`: the app-actor
> HTTP/SSE API, bearer auth, `/v1` routes, hash-checked sync blob transfer,
> the OpenAI-wire gateway (every backend routes, Codex via the
> chat↔Responses translation in `provider/openrouter.rs`), and opt-in
> `cloudflared`/sleep-guard lifecycle. Registered app UUIDs are public
> capabilities; host/provider secrets must never enter snapshots, events, or
> app URLs. Keep tunnel and Cloudflare setup paths opt-in; tests must remain
> network-free and must not launch sidecars.

## Web search

`search(mode=web)` needs no search key or instance URL. Auto failover order is
LangSearch (configured key) → SearXNG (configured URL) → public keyless Exa →
the session provider's native web search → DuckDuckGo → Brave. Explicit `exa`
and `provider` settings are available; LangSearch/SearXNG remain optional.
Exa calls share a two-permit toolbox budget; tests inject loopback endpoints,
and default search endpoints use dead local ports under `cfg(test)`.

Native search uses the session backend's auto helper (`default_helper_for` +
`newest_listed`, else its session model), never another provider or a helper
picked on another backend. OpenRouter uses its server search tool ($0.007 per
request plus tokens); record response-reported usage/cost. Codex uses the
ChatGPT subscription Responses route; OpenAI uses billed Responses web search
(tool and token rates). Go/local skip native search. Exa ignores recency and
reports that in the result header when a filter was requested.

## Things to know before changing code

- **One UI pass**: the popups share a chrome system (`ui/popups/chrome.rs`) —
  new popups should reuse it rather than re-implementing borders/hints.
- **Streaming must not jump**: `ui/history.rs` has viewport-pinning tests for
  reasoning/tool-call growth — the viewport must stay put while content
  streams above it. There are dedicated tests for this; don't regress them.
- **Tool results are capped**: `tools.rs` caps result sizes and marks
  unchanged results with a note — keep that behavior when editing tools.
- **Batch discipline**: models are encouraged to use the `batch` tool for
  independent operations; `batch` validates nesting, sizes, and read-only
  classification (see `tools.rs` tests).
- **Model-facing text**: `crates/core/assets/prompts/base.md` is the base prompt
  bundled into every session; changes there affect all backends.
- **Skills**: `crates/core/assets/find-skills-SKILL.md` is the model-facing skill for
  discovering/installing skills. Skills are SKILL.md packs with sandboxed
  Python virtualenvs (`src/skills.rs`).
- **Local embeddings**: `local:embeddinggemma-2[:270m]` talks to Ollama's native
  `/api/embed` (`brew install ollama`, `ollama serve`, `ollama pull
  embeddinggemma-2:270m`, or the multimodal `embeddinggemma-2`). Vectors live in
  `cache.chunk_embeddings` with the `EmbedIdentity` key plus their own
  location/text, so any identity change re-embeds lazily and a vector from
  another identity is never ranked. Prefixes are applied once, inside
  `Embedder` (`document`/`query`), never in transport code. Images are embedded
  directly when the probe shows the runtime accepts them; then
  `App::automatic_image_ocr` is false and rescans do not queue OCR/captions for
  images (`ocr_images` overrides; `reocr_file` is the explicit action). Tests
  use `provider::embedding::tests::MockOllama`; never hit a real Ollama.
- **Past-chat search**: `db/chats.rs` maintains rebuildable FTS5 message chunks
  and `(message_id, seq, EmbedIdentity)` vectors in device-local `cache.db` only.
  Index persisted user/assistant content, never tool/system/reasoning text or
  incognito. Synced messages enter the same newest-first backfill; connection-local
  TEMP triggers remove cached rows on message/session deletion or message edits,
  including sync deletes. `app/chats.rs` uses the existing `AppEvent::Embed`
  channel and exactly the file embedder (local files means local chats), behind
  files, with bounded batches and progress in `/files`. `tools/chats.rs` implements
  `search mode=chats` (read-only hybrid RRF, active space only) and the `chat:` URL
  transcript that `fetch_url` pages; a separate tool would not fit the chat prompt
  token ceiling. Never in research searcher toolboxes.
- **One model setting, the rest automatic**: `/config` has one model row,
  `helper_model` (blank = auto). `App::helper_backend` is where every background
  job (memory, compaction, image descriptions, vision OCR, the `LlmDecider`)
  gets its model: the pick, else `OpenRouter::default_helper_for(tag)` on the
  session's backend, else the session's own model there (local runtimes, or a
  catalog that dropped the default); it never crosses backends. Embeddings
  (`app/embeddings.rs`: local EmbeddingGemma 2 → backend default → none), OCR
  (`App::ocr_backend`: tesseract if installed, else the helper) and media
  generation (`App::generation_backend`, the backend's `default_*_gen_model`)
  are computed, not configured. The removed keys (`settings::is_removed_setting`)
  are ignored on read and rejected on set; `memory_model` migrates once into
  `helper_model` (`settings::legacy_helper_model`). `App::embedding_model` is
  runtime state, not a setting. New background jobs call `helper_backend()`;
  do not add a per-feature model row.
- **Don't commit agent tooling**: `.claude/`, `.superpowers/`,
  `.pi-subagents/` are gitignored — leave them out of commits.

[ratatui]: https://github.com/ratatui/ratatui
