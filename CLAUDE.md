# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`AGENTS.md` holds the long-form conventions and a per-file module map. This file
is the short version plus the things that only become visible after reading
several files at once.

## Commands

```sh
scripts/check.sh              # THE gate: fmt --check + clippy (-D warnings -W pedantic) + cargo audit + tests
scripts/check.sh --fix        # auto-fix fmt/clippy first, then run the gate
cargo run                     # launch the TUI (binary: nexus)
cargo test --workspace        # full suite; hermetic, no network
cargo test -p nexus-chat-core <name>   # core crate, tests matching <name>
cargo test -p nexus-chat <name>        # TUI crate (package nexus-chat, binary nexus)
cargo test -p nexus-chat-core app::tests::parse_topic_extracts_and_slugifies -- --exact
```

The pre-commit hook runs `scripts/check.sh`; a commit that fails it is rejected.
Never disable the hook and never land code that fails clippy pedantic. CI
(`.github/workflows/ci.yml`) runs the same script, so a green local gate is a
green PR. Pushing to `master` auto-bumps the patch version, tags `vX.Y.Z`, and
publishes to crates.io + GitHub.

Crate naming trips people up: the workspace members are `nexus-chat-core`
(lib target `nexus_core`, so imports are `use nexus_core::…`) and `nexus-chat`
(binary `nexus`).

## Architecture

Two crates with a hard seam: `crates/core` is domain-only with **zero TUI
dependencies**, and `crates/tui` is the terminal frontend plus the headless CLI.
Core drives three frontends — the TUI, `nexus <subcommand>` (`crates/tui/src/cli.rs`),
and the `nexus host` HTTP/SSE daemon (`crates/core/src/host/`). Anything added to
core must work for all three; pulling `ratatui` into core breaks the seam.

Four seams carry everything across that boundary:

- **`App` / `AppView` (Deref)** — `crates/core/src/app/mod.rs` owns domain state;
  `crates/tui/src/app_view.rs` wraps it and owns *all* view state (composer,
  popup chrome, render caches, theme, selection). `AppView: Deref<Target = App>`,
  so `app.foo` resolves to the view first and falls through to the domain. Put
  new state on the side that owns it — a field on `App` that only the terminal
  reads is a regression. A few domain-coupled display fields still live in core
  (`context_total`, `last_cache_rate`, `unread`, `notifications`, the
  files/scripts/sessions caches, research steer/stage state); they're documented
  in `app/mod.rs` and are deliberate.
- **`AppCommand`** (`app/commands.rs`) — every user action is a command. The
  `/`-string front (`run_command`) parses into `AppCommand` and `execute` runs
  it; the CLI and host build commands directly. New user-facing behavior gets a
  variant plus a `COMMANDS` catalog entry, not a TUI-only key handler.
- **`AppEvent`** (`app/mod.rs`) — the return channel. Core never touches the
  screen; it emits `Status`, `ComposerSet`/`ComposerClear`, `ViewportReset`,
  `HistoryInvalidated`, `Gate`, `Stream`, and the async pipeline results
  (`Research`, `Ocr`, `Embed`, …). `AppView::apply_event` is where they land.
- **`ToolExecutor`** (`tools.rs`) — `defs`/`is_read_only`/`run`/`supports_images`,
  held as `Arc<dyn ToolExecutor>` by the chat and research loops. `ToolBox` is
  the local impl; the roadmap's remote executor plugs in here.

Other structural facts worth knowing before editing:

- **One provider client.** `provider/openrouter.rs` serves every OpenAI-wire
  backend (OpenRouter, OpenAI, OpenCode Zen/Go, Codex); `provider/mod.rs` holds
  the message shapes, tool-call wire format, and `StreamEvent`. Backend
  differences are flags in that one client, not new clients.
- **One db, two halves.** There is a single durable `nexus.db` at the data root
  (`space.rs:db_path`) with rows scoped by `space_id` — *not* one db per space,
  whatever the prose in README/AGENTS says. Beside it sits a device-local,
  disposable `cache.db`, ATTACHed as schema `cache` (`db::open_attached`);
  `spaces/<name>/` holds only files, scripts, apps, and media. Device-local
  derived tables (embeddings, extraction caches) belong in `cache`; anything
  that must survive a restore or sync goes in the durable db. `sync.rs` merges
  append-only tables by UUID union and a small mutable surface by `updated_at`
  LWW — a new durable table needs a registry entry there. Every connection is
  opened through `db::open_conn` so it gets WAL plus a busy timeout; the TUI,
  `nexus host`, and per-call tool connections all share the file.
- **`nexus_core::boot()`** is the single bootstrap (credentials → space → db →
  appserver → toolbox) shared by `main.rs`, `cli.rs`, and the host. Don't
  hand-roll app construction in a new frontend path.
- **Research is a staged pipeline** (`app/research.rs`): survey → plan →
  parallel searchers → synthesis → critic → verifier → writer, with gates that
  park for user approval (`/research!` and `--approve` skip them) and `/steer`
  injecting mid-run.

## Constraints that tests enforce

- **Streaming must not jump.** `ui/history.rs` has viewport-pinning tests: when
  reasoning or tool-call blocks grow above the viewport it must stay put.
- **Popup chrome is shared.** New popups reuse `ui/popups/chrome.rs` rather than
  drawing their own borders/hints; snapshot tests live in `ui/popups/tests.rs`
  (ratatui `TestBackend`).
- **One visual vocabulary.** Glyphs, the field separator, section headers, and
  color roles (accent = you/interactive, accent2 = the assistant and its
  agents, dim = metadata, border = chrome) come from `ui/style.rs`. No emoji
  in UI strings — a test scans `src/ui` for them.
- **Tool results are capped** and unchanged results are marked with a note;
  `batch` validates nesting, size, and read-only classification.
- **Tests stay hermetic** — no network, no real keys, no XDG writes (use temp
  dirs; `Db::open_in_memory` and `app::tests::app_with_key` exist for this).
  Host tests must not launch `cloudflared` or other sidecars.
- **cargo-audit ignores** live in `.cargo/audit.toml` with written rationale;
  only extend it when the advisory is provably unreachable, and say why.

## Model-facing text

`crates/core/assets/system-prompt-base.md` is bundled into every session — edits
there change behavior on all backends. `crates/core/assets/find-skills-SKILL.md`
is the skill-discovery pack. Skills are `SKILL.md` directories with sandboxed
Python virtualenvs (`src/skills.rs`), discovered from the app-managed root, then
project `.agents/skills` (plus Claude/Codex/pi/OpenCode adapter roots), then user
roots; earlier, more-specific roots win duplicate names.

## Secrets and agent tooling

Keys come from `~/.config/nexus-chat/config.toml` or `OPENROUTER_API_KEY` /
`OPENAI_API_KEY` / `OPENCODE_API_KEY` — never hardcoded, never in snapshots,
events, or app URLs (registered app UUIDs are the public capability; the host
bearer token is not). `.claude/`, `.superpowers/`, and `.pi-subagents/` are
gitignored — keep them out of commits.

## Commit style

Lowercase conventional prefix, imperative, em-dash detail — e.g.
`feat: support global Agent Skills — enforce strict loading`. One logical change
per commit. Release notes are generated from these (`scripts/release-notes.sh`).
