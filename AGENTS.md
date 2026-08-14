# AGENTS.md

Guidance for AI coding agents working in this repository. Read this before
making changes — it covers the gates, conventions, and the shape of the code.

## What this is

`nexus-chat` is a local-first terminal chat app (Rust, [ratatui]) for deep
research and multi-agent work. All state lives on the user's machine: a
SQLite database per *space*, files/scripts/apps in space directories, and
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
| `crates/core/src/tools.rs` | the model's tools + the `ToolExecutor` seam (`defs`/`is_read_only`/`run`) |
| `crates/core/src/db.rs` | SQLite (rusqlite bundled): sessions, messages, usage, citations, model prefs |
| `crates/core/src/sync.rs` | Phase 3 merge engine: changeset types, per-table registry (cursor + apply rules), build/apply/ack, file blobs + zip bundles |
| `crates/core/src/appserver.rs` | localhost static server for model-created apps (port 8642) |
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
- **Model-facing text**: `crates/core/assets/system-prompt-base.md` is the base prompt
  bundled into every session; changes there affect all backends.
- **Skills**: `crates/core/assets/find-skills-SKILL.md` is the model-facing skill for
  discovering/installing skills. Skills are SKILL.md packs with sandboxed
  Python virtualenvs (`src/skills.rs`).
- **Don't commit agent tooling**: `.claude/`, `.superpowers/`,
  `.pi-subagents/` are gitignored — leave them out of commits.

[ratatui]: https://github.com/ratatui/ratatui
