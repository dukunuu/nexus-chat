# Roadmap: multi-device nexus

Local-first chat that works across desktop, web, and mobile — with a sync
mesh instead of a 24/7 backend. Single user, one DB per space, sync by
design (append-only union + last-write-wins on a small mutable surface).
No CRDTs, no conflicts to resolve.

## Architecture (the target)

```
                    ┌─ hub (laptop, on when you are) ─┐
                    │ nexus host: API · sync hub ·     │
                    │ worker · watches · key vault ·    │
                    │ apps server · tunnel sidecar      │
                    └──────┬───────────▲──────────┬───┘
              tunnel /     │   sync    │  login / │
            offline (full  │           │  stream  │
            replica)       │           │          │
        ┌──────┴────┐ ┌────┴────┐ ┌────┴────┐
        │ TUI (Rust)│ │ Web UI  │ │ Mobile  │
        │ in-proc or│ │ browser │ │ Flutter │
        │ --remote  │ │ → local │ │ thin →  │
        └───────────┘ │  daemon │ │ full rep│
                      └─────────┘ └─────────┘
```

- **`nexus-core`** (Rust lib): the existing app logic — agent loop, provider
  streaming, tools, research/swarm/watches pipelines, SQLite — minus any UI.
  Runs embedded in the TUI, in the daemon, and (later) on-device in mobile.
- **`nexus host`** (subcommand of the single `nexus` binary): HTTP + SSE
  API, sync hub, worker for heavy tools, watch runner, key vault — plus
  tunnel management (spawns/monitors `cloudflared`) and sleep prevention.
  Runs on whichever node is "up" — the laptop at home, exposed on demand
  via tunnel. Not a 24/7 dependency: `host` is an explicit opt-in mode.
- **Sync**: star topology to the hub when it's reachable; every device keeps
  a full replica and works offline. Append-only tables merge by UUID union;
  the ~10 mutable columns merge by `updated_at` LWW. Files sync by content
  hash.
- **The hands seam**: `ToolExecutor` trait — local executor on capable
  devices, remote executor over the tunnel for devices without python/
  ffmpeg/OCR. Devices advertise capabilities (`python/ffmpeg/tesseract/
  ollama/serve_apps`); the orchestrator routes or degrades with clear
  errors.

## Stack decisions

| Decision | Choice | Why |
|---|---|---|
| Core + daemon | Rust | the entire app already exists in Rust; reuse over rewrite |
| Sync engine | Rust, custom union-merge | schema is 90% append-only with UUID PKs; cr-sqlite is the upgrade path if it ever bites |
| Web UI | thin browser client → local daemon | browsers can't run the core's host tools |
| Mobile | Flutter, thin client first | flutter_rust_bridge keeps the full-replica/offline path open |
| Exposure | named Cloudflare tunnel; `cloudflared` as a managed sidecar, setup automated via the official `cloudflare-rs` v4 API crate | quick tunnels change URL on restart — useless for a configured client; no embeddable official tunnel SDK exists |
| Deployment | laptop as hub (free, data stays home) | VM / home box is a deployment change, not an architecture change |

## Phases

### Phase 1 — Durable/cache split with attached-DB joins (`db.rs`)

One root `cache.db` (device-local, disposable) next to `nexus.db`
(durable/syncable) — not per-space, because `web_cache` and `model_prices`
are space-agnostic:

```
<data root>/
├── nexus.db          # durable/syncable
├── cache.db          # device-local, disposable
└── spaces/           # unchanged
```

- **Split**: `web_cache`, `file_chunks`, `chunk_embeddings`, `model_prices`
  move to `cache.db`, plus a new `file_index_state(file_id, mtime, status)`
  — `files` keeps only `id, space_id, name, hash, size, created_at,
  updated_at`. `mtime`/`status` describe *this device's derived index
  state*; the restore-then-never-reindex trap is fixed by seeding
  `file_index_state` from legacy `files.mtime/status` once, and by `rescan`
  re-extracting any file whose index row is missing (cold cache).
- **Access**: `Db::open` and a new `open_attached` helper open the main db
  and `ATTACH DATABASE '<root>/cache.db' AS cache`. Cross-DB queries keep
  their shape with an explicit `cache.` prefix (`FROM cache.file_chunks
  JOIN files …`); cache-only queries (`web_cache`) stay unqualified so they
  also work on a standalone cache connection. Tool connections open main +
  attach cache (`ToolBox.web_cache_db` renamed `db_path`; `FilesCtx` needs
  no separate cache path).
- **Mutation sweep**: `updated_at` on model_prefs, app_settings, spaces,
  watches, files, session_sources, usage_log — bumped on every mutation
  path; sessions gets explicit bumps on compaction, web/swarm mode,
  title/slug, model, space reassignment, research parent. `swarm_personas`
  has no per-row LWW (DELETE roster → INSERT roster); version it by bumping
  the owning session. messages/citations stay append-only.
- **Settings scope registry**: `app_settings.scope` column + one registry
  fn `setting_is_local(key)` classifying every key, so new local keys
  can't silently default to sync.
- **Sync identity + tombstones**: `sync_id` UUID columns on citations and
  usage_log (their AUTOINCREMENT ids are device-local);
  `sync_tombstones(table, row_id, deleted_at)` written on every physical
  delete (spaces, sessions, messages, files, watches, roster deletes) —
  application tables stay clean; `device_meta(device_id)` separate from
  `sync_state(peer_id, table_name, pull_cursor, push_cursor,
  last_synced_at)` with opaque string cursors (sessions/messages cursor is
  a `(created_at, id)` tuple, not a naked timestamp). Phase 3 note:
  LWW ties break on `updated_at + device_id`; clock skew is accepted.
- **Backup/restore/doctor**: `backup` excludes `cache.db`; `restore` deletes
  `cache.db` before unzip (and again after, for old backups that contain
  one) so stale chunks/embeddings keyed to the pre-restore DB can't
  resurface; `nexus doctor` integrity-checks both DBs and reports an empty
  cache as healthy ("rebuilds on demand").
- **Migrations**: the ignore-all-errors `ALTER TABLE` loop becomes a
  `PRAGMA user_version`-gated runner that fails loudly on real errors,
  tolerating only genuinely-optional "column already present" skips (via
  `PRAGMA table_info`, not swallowed errors). No `CURRENT_TIMESTAMP`
  defaults — all timestamps come from Rust `Utc::now().to_rfc3339()` so
  lexical cursors stay consistent. Legacy `files.mtime/status` stay as dead
  columns in old DBs (no `DROP COLUMN` churn); fresh DBs never get them.
- **Exit**: behavior identical, fresh DBs fully split, existing DBs migrate
  cleanly, sync groundwork (identity, tombstones, cursors, versions)
  complete for Phase 3; all tests green, `scripts/check.sh` clean.

### Phase 2 — `nexus-core` extraction

Library crate with zero TUI deps; one seam: `AppCommand` in, `AppEvent` +
`CoreSnapshot` out. The `AppEvent` enum and headless CLI boot already
exist — this formalizes them (audit: `cli.rs::build_app` already boots the
same `App`; `AppEvent` already carries Stream/Models/Title/Research/Swarm…).

- **2a Workspace split**: `crates/core` (app/ logic, provider, tools, db,
  space, config, appserver, extract, citations, skills, update) +
  `crates/tui` (bin `nexus`: ui/, input, events, theme, markdown,
  selection, main). Dep triage: ratatui/crossterm/tui-*/arboard/
  unicode-width → TUI; `ratatui::style::Color` in chat.rs → plain enum in
  core; `open`/clipboard → TUI. Tests move with code.
- **2b Seam**: `AppCommand` enum (Send/Cancel/Steer/AnswerGate/SwitchSpace/
  SetModel/…) with `run_command` parsing `/`-strings into it; `AppEvent`
  formalized + `Status` and `GateRequested` events (research survey/plan
  gates become explicit events answered via `AnswerGate` — the plumbing
  mobile/web need); `CoreSnapshot` (serde: sessions, models, settings,
  tasks) designed now, consumed by the Phase 4 API unchanged; bootstrap
  consolidated into one `core::boot()` for TUI and CLI.
- **2c UI-state extraction (last)**: composer, popup/modes, scroll,
  status→event, mouse/selection state move TUI-side; core keeps domain
  state only.
- **2d `ToolExecutor` trait**: defs/is_read_only/run; `ToolBox` +
  research-restricted impls; agent loops call the trait — remote impl
  lands in Phase 4.
- **2e CLI on the seam** (canary): ask/chat/research/watch-run rebuilt on
  commands/events; `app/tests.rs` (2.2k lines) re-wired to the seam;
  popup snapshot tests against a thin TUI wrapper (`Core` + view state).
- **2f Docs/CI/publish**: `check.sh` builds the workspace; `nexus-chat-core`
  crate metadata for publishing (mobile FFI path later); README/AGENTS
  tree updates.
- **Sequencing**: 2a → 2d → 2b → 2e → 2c → 2f; each step lands green, TUI
  is the canary throughout.
- **Exit**: core builds with zero TUI deps; TUI behavior identical
  (snapshot tests green); `nexus ask/chat/research` work through the seam;
  `CoreSnapshot` shape documented.

### Phase 3 — Merge engine + changeset sync

- Changeset format: per-table id-diff (union for append-only, `updated_at`
  LWW for the mutable surface), files by content hash.
- Incremental push/pull with `sync_state` bookkeeping.
- `nexus sync <peer>` CLI (transport-agnostic: Tailscale/SSH/file).
- **Exit**: desktop ↔ laptop sync both directions, both work offline, no
  data loss in a week of real use.

### Phase 4 — `nexus host` (the daemon, in one subcommand)

One command turns the current machine into the hub. Runs headless-core +
HTTP/SSE API, then manages everything around it:

- HTTP API: sessions/messages/models/spaces; `POST message` → SSE stream
  of `StreamEvent`; `/v1/events` global feed; cancel/steer; watches;
  `/apps/*`.
- Worker mode: JSON-RPC tool execution + capability advertisement (the
  remote `ToolExecutor` impl).
- Auth: bearer token; tunnel exposes the port, the token gates it.
- **Tunnel management**: spawns the official `cloudflared` binary as a
  sidecar (config written by the daemon), health-checks the URL, restarts
  it on failure. Not embedded — updates come from brew/apt independently.
- **`nexus host --setup`**: non-interactive provisioning via the official
  `cloudflare-rs` v4 API crate — API token → create named tunnel + DNS
  record + credentials file; no `cloudflared tunnel login` needed.
- **Sleep guard**: pmset/caffeinate while hosting, released on exit; warns
  on battery.
- **QR enrollment**: prints an ASCII QR encoding
  `nexus://host=<url>&token=…` — the mobile app scans once, done.
- Laptop service setup: launchd/systemd wrapper; charge cap note.
- TUI gets `--remote <url>` mode — dogfoods the API.
- **Exit**: browser on the phone reaches the laptop's daemon through the
  tunnel, streams a chat, runs python via the worker.

### Phase 5 — Web UI

- Thin client: sessions list, chat view, SSE streaming, markdown rendering.
- **Exit**: usable chat + research view from any browser.

### Phase 6 — Mobile (Flutter)

- Start **thin**: login with token → hub, stream, sync via the API.
- Later flag: full replica + flutter_rust_bridge embedding the core for
  offline standalone.
- **Exit**: chat from the couch; works when the laptop is on; graceful
  "hub unreachable" state.

### Phase 7 — Optional hardening (only if needed)

- Watches at 3am become real → same daemon on a home box / €5 VM
  (deployment change, not architecture).
- Encrypt hub data at rest; cr-sqlite upgrade if the merge engine bites.

## Guardrails

- Every phase compiles and passes `scripts/check.sh`; one logical change
  per conventional commit.
- Desktop TUI never regresses — it is the canary for every refactor.
- No network in tests; hermetic temp-dir DBs as today.

## Open decisions

- Laptop-as-hub sleep policy: `pmset` config, charge cap, what "on" means.
- Whether `usage_log` (append-only, large) syncs or stays per-device and
  aggregates later.
- Mobile: Flutter confirmed, but thin-vs-full-replica is a Phase 6 decision.

## Decided

- **Deletes sync via tombstones, not soft-deletes** — decided with Phase 1.
  Application tables stay clean; `sync_tombstones` propagates physical
  deletes (spaces, sessions, messages, files, watches, swarm roster) and
  the merge engine prunes them.
- **`files.mtime`/`status` are device-local index state** — decided with
  Phase 1. They move to `cache.file_index_state`; legacy DBs keep the old
  columns as dead columns until a later cleanup.
- **Exposure: named Cloudflare tunnel** — decided 2025-08; requires a domain
  (~$10/yr) with a zone on Cloudflare DNS. `trycloudflare.com` quick tunnels
  are rejected: the URL changes on restart.
- **Tunnel client = `cloudflared` binary as a managed sidecar** — decided
  2025-08. The official tunnel client is Go and actively maintained; there
  is no official embeddable Rust SDK. Evaluated and rejected: the
  `cloudflared` crate on crates.io (KABBOUCHI, v0.0.3, last pushed
  Feb 2024, 0 stars) — abandoned and unproven.
- **Control plane = official `cloudflare-rs` crate** (cloudflare/cloudflare-rs,
  Rust library for the Cloudflare v4 API) — used by `nexus host --setup` to
  provision the named tunnel + DNS record + credentials non-interactively.
