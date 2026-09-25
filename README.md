# nexus-chat

[![crates.io](https://img.shields.io/crates/v/nexus-chat.svg)](https://crates.io/crates/nexus-chat)
[![docs.rs](https://docs.rs/nexus-chat/badge.svg)](https://docs.rs/nexus-chat)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A local-first terminal chat app for deep research and multi-agent work. Rust +
[ratatui], all state on your machine — one SQLite db scoped by space, files and artifacts in
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
nexus clip                                             # explain text currently in the OS clipboard
nexus clip --web --copy "Explain this and list surprising facts" # browser-text shortcut
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
nexus --continue                                       # launch the TUI inside the latest session
nexus --selection-chat                                 # analyze Linux primary selection, or continue
nexus update                                           # update to the latest release (cargo install)
nexus status                                           # paths, providers configured, db stats
nexus doctor [--network]                               # db integrity, config, tools
nexus host [--port 8643]                               # local HTTP/SSE daemon + gateway
nexus host --tunnel                                     # reuse named tunnel, or quick tunnel + QR
nexus host --setup                                      # provision a named tunnel with CF_API_TOKEN
```

`nexus ask`/`clip`/`chat`/`research` use your most recently used model (or
`--model`), run the same search/tool pipelines as the TUI, and save
conversations as normal sessions — tool status and token usage go to
stderr, answers to stdout. `clip` reads text through the OS clipboard
(Wayland or X11), applies its optional instruction, and uses a built-in
plain-language/facts prompt when no instruction is supplied. `--copy` puts
the answer back on the clipboard. `--selection-chat` is the interactive
Linux shortcut mode: it reads the primary selection without changing either
clipboard, submits a protected web-mode turn into the TUI, and behaves like
`--continue` when the selection is empty. A running TUI accepts later
selection requests over its per-user runtime socket, so the shortcut reuses
one instance. Bind it with a terminal command such as
`foot -e nexus --selection-chat`; use the equivalent `-e` option for your
terminal emulator. `research` without `--approve` parks at the
survey/plan checkpoints: interactive when stdin is a terminal, an error
otherwise (`--approve` runs unattended, like `/research!`). The read-only
commands (`usage`, `sessions`, `spaces`, `export`, `status`, `doctor`,
`backup`, `memory`, …) never touch the network.

### Hosting

`nexus host` runs the same core on a loopback HTTP/SSE daemon. It exposes
`/v1/snapshot`, `/v1/models`, `/v1/backends`, `/v1/events`,
`/v1/sessions/<id>/messages`, `/v1/command`, `/v1/sync`, hash-checked
`GET`/`PUT /v1/sync/blob`, `/v1/tools`, and an
OpenAI-compatible `/v1/chat/completions` gateway. `/v1/tools/run` uses a
JSON-RPC 2.0 envelope, for example
`{"jsonrpc":"2.0","id":1,"method":"tools/run","params":{"name":"read_file","arguments":{...}}}`.
Sync clients POST metadata first, then upload/download each manifest blob by
`space_id`, `name`, and `hash`. The host token is generated once and stored in
`~/.config/nexus-chat/config.toml` as `[provider].host_token`; provider API
keys remain on the machine and are never sent to clients.

```sh
nexus host --port 8643
curl -H "Authorization: Bearer <host-token>" http://127.0.0.1:8643/v1/snapshot
nexus host --tunnel                         # requires cloudflared
CF_API_TOKEN=… nexus host --setup           # creates/reuses named tunnel + DNS CNAME
# non-interactive setup can also set CF_ACCOUNT_ID, CF_ZONE_ID,
# CF_HOSTNAME, and CF_TUNNEL_NAME
```

The command prints an enrollment URI and an ASCII QR code. Public app links
use `/apps/<uuid>/`; the registry UUID is the app capability and the host
bearer token is never embedded in the URL or an app cookie. `/v1/models`
returns an OpenAI-compatible `data` list with backend-qualified ids such as
`openrouter:anthropic/…`. Codex is routed too: the gateway translates the
chat-completions request into a native Responses call and re-emits the
Responses event stream as `chat.completion.chunk` frames, so every listed
backend is reachable through one OpenAI-wire endpoint. Named tunnel setup is persisted and reused by later
`nexus host --tunnel` runs when its local cloudflared files still exist.
`--no-sleep-guard` disables `caffeinate`/`systemd-inhibit` if desired.
For optional per-user startup, use `nexus host --install-service` (or
`--uninstall-service`); it writes a systemd user unit on Linux or a launchd
agent on macOS and prints the activation command.

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

## Local inference (experimental)

Pick a runtime from inside the TUI with `/local` (also the last row of
`/login`), or name one directly: `/ollama`, `/mlx`, `/mlx-serve`,
`/lmstudio`, `/edge0`, `/local mlx-serve http://localhost:11234/v1`, `/local off`. Either way the choice is
written to `~/.config/nexus-chat/config.toml` and the catalog reloads
immediately — no restart.

The same block can be written by hand:

```toml
[local]
provider = "ollama" # or "mlx", "mlx_serve", "lmstudio", "edge0"
# endpoint = "http://localhost:11434/v1" # optional inference URL override
```

Switching runtimes from `/local` drops a custom `endpoint`/`list_command`,
since those are written for one runtime; re-selecting the active runtime keeps
them.

Nexus runs the runtime's discovery command when loading/refreshing `/model`:

| Runtime | Discovery | Default inference URL |
| --- | --- | --- |
| `ollama` | `ollama list` | `http://localhost:11434/v1` |
| `lmstudio` | `lms ls --json` | `http://localhost:1234/v1` |
| `mlx` (mlx-lm) | `python3` running a bundled, offline Hugging Face cache scanner | `http://localhost:8080/v1` |
| `mlx_serve` | `mlx-serve list` | `http://localhost:11234/v1` |
| `edge0` | `edge0 models` | `http://localhost:8000/v1` |

These are **installed models**, not an online download catalog. The legacy
`mlx` runtime uses mlx-lm, which has no list command: the scanner lists cached
repositories with `config.json` and safetensors weights, respecting
`HF_HOME`/`HF_HUB_CACHE`. These are candidates, not a guarantee of MLX
compatibility or a complete download. The separate `mlx-serve` runtime reads
its own `~/.mlx-serve/models` catalog; entries marked `unsupported` (such as
an incomplete download) are not offered in `/model`. Pull models with
`mlx-serve pull <org/repo>` before selecting them. `edge0 models`
lists the tiers its registry knows (`edge0-35b`, `edge0-8b`) — likewise
candidates: the checkpoint itself is located by the server through
`EDGE0_<TIER>_MODEL`, and serving a checkpoint directory instead names the
tier it auto-detects.

Local models have their own **Local** backend filter and `local:` IDs; names also
show the runtime. OpenAI and other cloud backends remain independent. Only one
local runtime is configured at a time. No cloud credentials are sent locally.

For a custom installation, override discovery with an argv array. Its output
must be one inference model ID per line (no header):

```toml
[local]
provider = "mlx"
list_command = ["/absolute/path/to/my-model-list", "--installed"]
```

Commands run without a shell, with a 15-second timeout and 1 MiB stdout cap.
Only put trusted commands in this machine-local config; discovery executes them
automatically. Configuring a different inference endpoint does not change where
the discovery command looks—configure that command/runtime accordingly.

### Managed servers

Inference needs a server running behind the endpoint. Nexus can start one for
you and tell you what it costs:

| Command | What it does |
| --- | --- |
| `/local start [runtime]` | start that runtime's server (`/serve` is the same command) |
| `/local stop [runtime]` | stop a server **Nexus started** |
| `/local restart [runtime]` | stop then start, to pick up a new model |
| `/local status` | probe every runtime: which answer, what they cost, what is over budget |

Omit the runtime to act on the selected one, or name it — `/local stop edge0`
and `/edge0 stop` are the same command. The `/local` picker shows the same
survey live, with `s` start, `x` stop, `r` refresh on the row under the cursor.

What gets launched:

| Runtime | Server command | Needs a model |
| --- | --- | --- |
| `ollama` | `ollama serve` (with `OLLAMA_HOST`) | no — loads on demand |
| `lmstudio` | `lms server start` (a daemon, stopped with `lms server stop`) | no |
| `mlx` (mlx-lm) | `mlx_lm.server --model <selected>` | yes |
| `mlx_serve` | `mlx-serve serve --host 127.0.0.1 --port 11234` | no — loads on demand |
| `edge0` | `edge0 serve <selected>` | yes |

mlx-lm and edge0 serve exactly one model, taken from your `/model` selection at
launch — switching models in Nexus does not restart them, so use
`/local restart`. Ollama, mlx-serve and LM Studio load models through their own API.
If `mlx-serve run <model>` is already serving on port 11234, Nexus can use it
without taking ownership; `mlx-serve serve` is the managed, on-demand mode.

**Ownership is explicit.** A server Nexus started is stopped by `/local stop`
and when Nexus exits, so quitting never strands a multi-gigabyte process. A
server already answering on the endpoint — one you started in another terminal,
or a system service — is detected and used, but never stopped by Nexus.

**Memory is advisory.** `/local status` reports the resident memory of the
listener's whole process tree, which is what you want: Ollama's weights live in
a `runner` child, not the server that holds the port. Going over budget warns
on the status line and marks the row; it never kills a server, so a reply that
is mid-generation is never yanked. The budget defaults to 70% of physical
memory and is configurable:

```toml
[local]
provider = "edge0"
memory_budget_mb = 8000 # optional; default is 70% of physical RAM
```

Reading memory needs `ps`, and `lsof` for servers Nexus did not start; without
them liveness still works and the size column is simply blank.

The server's API must accept the discovered model ID. Model downloads,
authenticated local servers, host gateway forwarding, and capability/context
discovery are not implemented yet. Tool calling depends on the runtime/model.
Local utility fallback uses the selected/installed model rather than a cloud
model name.

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
- **Skills**: Agent Skills-compatible instruction packs (`SKILL.md`) with
  progressive disclosure, project/user/global discovery, bundled resources,
  sandboxed Python virtualenvs, and chat installation
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
| `/skills` | manage skills (app + project/user/global Agent Skills) |
| `/usage` (`analytics`, `costs`) | token/cache/cost analytics by backend & model (←/→ for 24h/7d/30d/all windows) |
| `/compact` (`summarize`) | summarize old messages into a digest |
| `/config` (`settings`, `stats`) | settings, footer toggles, sampling params |
| `/web` | toggle search-first cited answering |
| `/export` (`save-report`) | write the research report + sources to a file |
| `/incognito` | toggle no-persistence mode |
| `/help` (`keys`, `shortcuts`; also `F1`) | keybinding + command reference |
| `/quit` | quit |
| `<skill-name>` | arm a skill for the next message |

## Keybindings

| Keys | Action |
|---|---|
| `Enter` | send (Shift/Ctrl+Enter inserts a newline) |
| `Esc` | stop the streaming response / clear the composer |
| `Ctrl+C` | close popup / stop stream / clear composer; press twice to quit |
| `Ctrl+V` | paste (bracketed paste) |
| `Ctrl+Shift+C` / `Ctrl+X` | copy / cut composer selection |
| `Ctrl+A` | select all in composer |
| `Ctrl+Backspace` | delete previous word |
| `Alt+↑` / `Alt+↓` | recall messages you sent this session |
| `Alt+1`–`Alt+4` | reopen a recent session from the start screen |
| `Ctrl+R` | expand/collapse reasoning traces |
| `Ctrl+T` | expand/collapse tool-call detail blocks |
| `Ctrl+G` | context breakdown (system/memory/skills/conversation) |
| `Ctrl+N` | start a new regular session |
| `Ctrl+Shift+N` | start a new incognito session |
| `Ctrl+O` | open a session-link message under the selection |
| `Ctrl+↑` | live research activity view (Ctrl+X there stops the job) |
| `PageUp` / `PageDown` | scroll a page |
| `Ctrl+Home` / `Ctrl+End` | jump to the start / latest message |
| `F1` | keybinding + command reference |
| mouse drag | select + copy |

While a research survey or plan approval is pending, Enter in that session
answers the gate; a reply with edits is folded in once by the approval agent.

## Architecture

Two-crate workspace: the engine and the terminal frontend.

```
crates/core/        nexus-chat-core (lib crate `nexus_core`) — domain only, zero TUI deps; drives TUI + CLI + the future host API
├── src/app/        the state machine (one module per feature)
│   ├── mod.rs      App struct (domain fields only), gates, commands, boot, snapshot, event stream
│   ├── commands.rs AppCommand seam + the /-command catalog (COMMANDS, fuzzy_score)
│   ├── chat.rs     request lifecycle: history build, streaming, tool loop
│   ├── research.rs the research pipeline: survey → plan → searchers → …
│   ├── swarm.rs    multi-persona roundtable
│   ├── watches.rs  standing research jobs
│   ├── usage.rs    usage/cost analytics
│   ├── sessions.rs, spaces.rs, models.rs, backends.rs, memory.rs
│   ├── files.rs, images.rs, scripts.rs, apps.rs   space artifacts
│   ├── skills_popup.rs, compaction.rs, export.rs, transcribe.rs
│   └── tests.rs    app-level integration tests
├── src/provider/   message shapes, tool-call wire format, events
│   └── openrouter.rs  one client for all OpenAI-wire backends (OR/OpenAI/…)
├── src/tools.rs    the model's tools: search, fetch, python, video, apps…
├── src/skills.rs, extract.rs, citations.rs   tool support
├── src/db.rs       SQLite: sessions, messages, usage, citations, model prefs
├── src/appserver.rs  localhost static server for model-created apps (8642)
├── src/host/         HTTP/SSE daemon, provider gateway, sync/worker routes,
│                     Cloudflare setup, tunnel and sleep-guard lifecycle
├── src/markdown.rs pure to_plain copy path + the shared GFM table splitter
└── src/config.rs, space.rs   credentials, spaces
crates/tui/         nexus-chat — the `nexus` binary
└── src/            main.rs bootstrap, cli.rs subcommands, events.rs loop,
                    app_view.rs (AppView: composer, popup chrome, render state —
                    wraps App via Deref), flows/ (popup flow methods), composer.rs,
                    theme.rs, selection.rs, filter_input.rs, history_cache.rs,
                    ui/ ratatui rendering (history, popups), ui/markdown.rs
```

Data lives under the XDG data dir: `spaces/<space>/` holds the per-space
SQLite db, files, scripts, apps, and generated media. Skills are discovered
from the app-managed `skills/` directory plus project `.agents/skills` (and
Claude/Codex/pi/OpenCode adapter roots), then user roots such as
`~/.agents/skills`, `~/.codex/skills`, and `~/.pi/agent/skills`. Earlier,
more-specific roots win duplicate names; only metadata is loaded until a
skill is explicitly activated.

## Development

```sh
scripts/check.sh          # fmt + clippy (-D warnings, pedantic) + cargo-audit + tests
cargo test --workspace    # 500+ tests, no network needed
npm --prefix web ci
npm --prefix web run build  # production client served by nexus host
npm --prefix web test       # Chromium desktop/mobile + mocked SSE tests
```

Open `http://127.0.0.1:8643` after starting `nexus host` from the repository root.
For installed hosts, set `NEXUS_WEB_DIR` to the absolute web build directory.
See [web setup and API documentation](web/README.md) for development and browser setup.

The pre-commit hook runs `scripts/check.sh` on every commit — a merge-ready
change passes it. See [AGENTS.md](AGENTS.md) for conventions and a deeper
module map.

Pushes to `master` release automatically: a workflow bumps the patch version,
tags `vX.Y.Z`, and runs the publish pipeline (crates.io + GitHub release with
the release binary). Release notes are generated from the conventional commits
since the last tag (`scripts/release-notes.sh`), so the GitHub release always
shows what changed. Manual `v*` tag pushes publish the same way. Details in
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
