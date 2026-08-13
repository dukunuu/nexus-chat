# nexus-chat

[![crates.io](https://img.shields.io/crates/v/nexus-chat.svg)](https://crates.io/crates/nexus-chat)
[![docs.rs](https://docs.rs/nexus-chat/badge.svg)](https://docs.rs/nexus-chat)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A local-first terminal chat app for deep research and multi-agent work. Rust +
[ratatui], all state on your machine — SQLite per space, files and artifacts in
space directories, model-created web apps served from localhost.

## Install

```sh
cargo install nexus-chat
```

This provides the `nexus` command. Requires Rust 1.91+ (edition 2024).

To run from a checkout instead:

```sh
cargo build --release
./target/release/nexus
```

### CLI

`nexus` with no arguments launches the TUI. Subcommands work headless
from the shell — everything they write (sessions, usage, reports) lands in
the same local state the TUI reads, so you can mix both freely:

```sh
nexus ask "summarize the EU AI Act in 5 bullets"      # one-shot chat, streams to stdout
nexus ask --model deepseek/deepseek-v3 --web "..."    # pick a model, search-grounded
cat brief.md | nexus ask "summarize this"             # prompt from stdin
nexus ask --space new:research "..."                  # create-and-use a space
nexus ask --json --quiet "..."                        # structured output for scripting
nexus chat                                             # bare REPL, one session
nexus research "impact of EU AI Act on startups"       # deep research (survey + plan gates)
nexus research --approve "..."                        # skip the gates, run unattended
nexus watch list                                       # standing research watches
nexus watch run                                        # run the due watches (cron-friendly)
nexus watch run <id> --all                             # force-run one / every watch
nexus usage [--range 24h|7d|30d|all] [--by-day]        # token/cache/cost analytics
nexus usage --json                                     # same, machine-readable
nexus sessions [--space <name>] [--json]               # list sessions
nexus sessions rm <id|slug>                            # delete one session
nexus sessions prune --keep 20 --days 90 --dry-run     # delete old sessions
nexus spaces [--json]                                  # list spaces
nexus export <id|slug>                                 # print a session's latest report + sources
nexus export <id> --transcript -o chat.md              # the whole conversation
nexus backup [-o file.zip]                             # zip db + spaces + skills
nexus restore file.zip --yes                           # restore a backup (overwrites)
nexus memory [--space] [--edit]                        # print/edit a space's memory
nexus instructions [--space] [--edit]                  # print/edit a space's instructions
nexus files [--space]                                  # list imported files
nexus models [--backend openrouter]                    # fetch + list model catalogs
nexus login openrouter sk-... [--check]                # save a provider key
nexus skills list                                      # installed skills
nexus skills install owner/repo[/path]                 # install a skill from GitHub
nexus open <id|slug>                                   # launch the TUI inside that session
nexus update                                           # check for a newer release
nexus status                                           # paths, providers configured, db stats
nexus doctor [--network]                               # db integrity, config, tools
```

`nexus ask`/`chat`/`research` use your most recently used model (or
`--model`), run the same search/tool pipelines as the TUI, and save
conversations as normal sessions — tool status and token usage go to
stderr, answers to stdout. `research` without `--approve` parks at the
survey/plan checkpoints: interactive when stdin is a terminal, an error
otherwise (`--approve` runs unattended, like `/research!`). The read-only
commands (`usage`, `sessions`, `spaces`, `export`, `status`, `doctor`,
`backup`, `memory`, …) never touch the network.

### Requirements

- Rust 1.91+ (only needed to build/install — no runtime dependency)
- A modern terminal (truecolor recommended)
- Extra tooling, all optional:
  - `tesseract` — local OCR
  - `ffmpeg` — video transforms for `media`
  - `ollama` — local embeddings/OCR

### Configuration

Keys come from the config file or env (`OPENROUTER_API_KEY`,
`OPENAI_API_KEY`, `OPENCODE_API_KEY`), or `/login` in-app. On first launch a
key is enough; models are fetched from the catalogs.

| What | Where |
|---|---|
| credentials & settings | `~/.config/nexus-chat/config.toml` |
| system prompt overrides | `~/.config/nexus-chat/system_prompt.md` |
| custom banner | `~/.config/nexus-chat/banner.txt` |
| spaces (db, files, scripts, apps, media) | `~/.local/share/nexus-chat/spaces/<space>/` |

## Features

- **Chat** over any configured backend: OpenRouter, OpenAI, OpenCode Zen/Go,
  and Codex — one merged model list, per-model reasoning-effort control
- **Deep research** (`/research <topic>`): a conversational scoping survey →
  a plan of questions with why/angles/sources briefs → parallel searcher
  agents → synthesis → critic → verifier → writer, with a live activity view
  and `/steer` mid-run. `/research! <topic>` runs ungated
- **Swarm** (`/swarm`): a moderator-conducted multi-persona roundtable that
  iterates toward consensus
- **Watches** (`/watch`): standing research jobs that re-run daily
- **Spaces**: per-project context (instructions, memory, imported files,
  scripts, apps) with embeddings-backed semantic file search
- **Skills**: reusable instruction packs (SKILL.md) with sandboxed Python
  virtualenvs, installable from chat
- **Tools for the model**: nine consolidated tools — `search` (web/academic/
  discussion), `fetch_url` (with PDF/YouTube extraction), `batch` (multi-op
  calls), `research_lookup`, `files`, `app` (build & edit web apps served on
  `http://localhost:8642`; `init` scaffolds Astro+React / Vite+React
  starters, `build` compiles them with the framework's static build), `scripts`, `skills`, and `media` (image/video
  generation with ffmpeg transforms)
- **Usage analytics**: `/usage` shows token/cache/cost analytics by backend
  and model, priced from a synced catalog
- **Terminal ergonomics**: markdown rendering, image display, @-file
  autocomplete, mouse selection → copy, context breakdown, compaction,
  incognito mode, per-session history

## Commands

Type `/` in the composer for autocomplete. Aliases in parentheses.

| Command | What it does |
|---|---|
| `/new` (`chat`, `clear`) | start a new session |
| `/session` (`history`, `resume`, `switch`) | browse/switch sessions |
| `/space` (`project`, `workspace`) | switch spaces |
| `/model` (`llm`) | pick a model, set reasoning effort |
| `/login` (`key`) | log into a backend |
| `/research <topic>` | conversational deep research (see above) |
| `/research! <topic>` | same, no survey/approval gates |
| `/watch` | standing research, re-runs every 24h |
| `/swarm` (`panel`) | multi-persona roundtable |
| `/files` (`images`, `scripts`, …) | browse space files / images / scripts |
| `/apps` (`webapps`) | view model-created web apps |
| `/skills` | manage skills |
| `/usage` (`analytics`, `costs`) | token/cache/cost analytics by backend & model (←/→ for 24h/7d/30d/all windows) |
| `/compact` (`summarize`) | summarize old messages into a digest |
| `/config` (`settings`, `stats`) | settings, footer toggles, sampling params |
| `/web` | toggle search-first cited answering |
| `/export` (`save-report`) | write the research report + sources to a file |
| `/incognito` | toggle no-persistence mode |
| `/quit` | quit |
| `<skill-name>` | arm a skill for the next message |

## Keybindings

| Keys | Action |
|---|---|
| `Enter` | send (Shift/Ctrl+Enter inserts a newline) |
| `Esc` | stop the streaming response / clear the composer |
| `Ctrl+C` | quit |
| `Ctrl+V` | paste (bracketed paste) |
| `Ctrl+Shift+C` / `Ctrl+X` | copy / cut composer selection |
| `Ctrl+A` | select all in composer |
| `Ctrl+Backspace` | delete previous word |
| `Ctrl+R` | expand/collapse reasoning traces |
| `Ctrl+T` | expand/collapse tool-call detail blocks |
| `Ctrl+G` | context breakdown (system/memory/skills/conversation) |
| `Ctrl+N` | toggle incognito |
| `Ctrl+O` | open a session-link message under the selection |
| `Ctrl+↑` | live research activity view (Ctrl+X there stops the job) |
| `PageUp` / `PageDown` | scroll |
| mouse drag | select + copy; `p` / `x` pin / discard the cited source under the selection |

While a research survey or plan approval is pending, Enter in that session
answers the gate; a reply with edits is folded in once by the approval agent.

## Architecture

```
src/
├── main.rs          bootstrap: config, space, db, App, event loop
├── app/             the state machine (one module per feature)
│   ├── mod.rs       App struct, popups, settings, run_command, gate plumbing
│   ├── chat.rs      request lifecycle: history build, streaming, tool loop
│   ├── research.rs  the research pipeline: survey → plan → searchers → …
│   ├── swarm.rs     multi-persona roundtable
│   ├── watches.rs   standing research jobs
│   ├── usage.rs     usage/cost analytics
│   ├── sessions.rs, spaces.rs, models.rs, backends.rs, memory.rs
│   ├── files.rs, images.rs, scripts.rs, apps.rs   space artifacts
│   ├── skills_popup.rs, compaction.rs, export.rs, copy.rs, transcribe.rs
│   └── tests.rs     app-level integration tests
├── provider/
│   ├── mod.rs       message shapes, tool-call wire format, events
│   └── openrouter.rs  one client for all OpenAI-wire backends (OR/OpenAI/…)
├── tools.rs         the model's tools: search, fetch, python, video, apps…
├── skills.rs, extract.rs, citations.rs   tool support
├── db.rs            SQLite: sessions, messages, usage, citations, model prefs
├── appserver.rs     localhost static server for model-created apps (port 8642)
├── ui/              ratatui rendering: history, popups, theme, markdown
├── input.rs         composer, @-autocomplete, command catalog
├── events.rs        key/mouse handling, clipboard
└── config.rs, space.rs   credentials, spaces layout
```

Data lives under the XDG data dir: `spaces/<space>/` holds the per-space
SQLite db, files, scripts, apps, and generated media.

## Development

```sh
scripts/check.sh          # fmt + clippy (-D warnings, pedantic) + cargo-audit + tests
cargo test --bin nexus   # 468 tests, no network needed
```

The pre-commit hook runs `scripts/check.sh` on every commit — a merge-ready
change passes it. See [AGENTS.md](AGENTS.md) for conventions and a deeper
module map.

Pushes to `master` release automatically: a workflow bumps the patch version,
tags `vX.Y.Z`, and runs the publish pipeline (crates.io + GitHub release with
the release binary). Manual `v*` tag pushes publish the same way. Details in
`.github/workflows/`.

## Roadmap

Multi-device (web + mobile) via a sync mesh, no 24/7 backend — see
[`docs/roadmap.md`](docs/roadmap.md).

## Known limitations

- The research survey section is always visible, not collapsible.
- The survey's first-round questions are generated from the topic alone — the
  concurrent known-chunks/web-survey context arrives after round 1.
- A plan rework presented within the same second overwrites the plan file
  (same timestamped name).

[ratatui]: https://github.com/ratatui/ratatui
