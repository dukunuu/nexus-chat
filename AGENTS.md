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
cargo build                    # build (binary: nexus)
cargo run                      # run the TUI
cargo test --bin nexus         # 468 tests; none touch the network
scripts/check.sh               # THE gate: fmt + clippy + audit + tests
scripts/check.sh --fix         # auto-fix fmt/clippy, then run the gate
```

Every commit runs `scripts/check.sh` via the local pre-commit hook — a commit
that fails the gate is rejected. **Never disable the hook, and never commit
code that fails `cargo clippy -- -D warnings -W clippy::pedantic`.**

## Conventions

- **Rust edition 2024**, `rustc 1.85+`. `cargo fmt` is mandatory.
- **Clippy at pedantic with warnings denied** — write code that satisfies it
  (explicit lifetimes where needed, `#[must_use]` on pure fns, docs on public
  items, no `unwrap()` on fallible paths). Prefer params structs over
  `too_many_arguments` allows.
- **Tests**: app-level integration tests live in `src/app/tests.rs` and
  `src/ui/popups/tests.rs` (ratatui `TestBackend` snapshot style). Keep tests
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
| `src/app/mod.rs` | `App` struct, popup state, `run_command`, gate plumbing |
| `src/app/chat.rs` | request lifecycle: history build, streaming, tool loop |
| `src/app/research.rs` | research pipeline: survey → plan → searchers → synthesis → critic → verifier → writer |
| `src/app/swarm.rs`, `watches.rs` | roundtable / standing jobs |
| `src/app/usage.rs` | usage & cost analytics (`/usage`) |
| `src/app/sessions.rs`, `spaces.rs`, `models.rs`, `memory.rs` | state backends |
| `src/provider/openrouter.rs` | the single client for all OpenAI-wire backends — message shapes, tool-call wire format, events are in `provider/mod.rs` |
| `src/tools.rs` | the model's tools (search, fetch, batch, files, app, scripts, skills, media) |
| `src/db.rs` | SQLite (rusqlite bundled): sessions, messages, usage, citations, model prefs |
| `src/appserver.rs` | localhost static server for model-created apps (port 8642) |
| `src/ui/` | ratatui rendering; `history.rs` is the main view, `popups/` one module per popup |
| `src/input.rs`, `events.rs` | composer, command catalog, key/mouse handling |
| `src/config.rs`, `space.rs` | credentials + spaces layout (XDG dirs) |

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
- **Model-facing text**: `assets/system-prompt-base.md` is the base prompt
  bundled into every session; changes there affect all backends.
- **Skills**: `assets/find-skills-SKILL.md` is the model-facing skill for
  discovering/installing skills. Skills are SKILL.md packs with sandboxed
  Python virtualenvs (`src/skills.rs`).
- **Don't commit agent tooling**: `.claude/`, `.superpowers/`,
  `.pi-subagents/` are gitignored — leave them out of commits.

[ratatui]: https://github.com/ratatui/ratatui
