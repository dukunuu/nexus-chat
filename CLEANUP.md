# Codebase Cleanup — Plan

> Scope: repo hygiene, lint debt, structural debt, legacy shims, docs. No new
> features. This doc is a working plan — delete it when the phases are done.

## Done already (this commit)

- `git rm` stale root files: `PLAN.md`, `IMPLEMENTATION.md` (completed research
  feature records — the 3 known limitations they carried are preserved in
  Phase 4 below), `.cargo-test.log` (28 KB committed test log).
- `git rm -r .pi-subagents/` — 30 tracked files / 4.7 MB of old subagent
  transcripts.
- `.gitignore` consolidated: `*.log`, `**/.pi-subagents/`, `**/.superpowers/`,
  `.claude/` state — previously these lived only in the machine-local
  `.git/info/exclude`, so they were invisible to anyone else cloning.

## Baseline (verified)

- `cargo build` / `cargo test --no-run`: **zero warnings** — rustc's
  `dead_code` lint finds no orphaned methods, unused fields, or unused enum
  variants in the binary or the test build. "Orphaned methods" in the strict
  sense don't exist; the real debt is style, structure, and compat shims.
- 414 tests pass in 4 s.
- `cargo clippy --bin nexus-chat`: **32 warnings** (24 auto-fixable).

## Phase 1 — Clippy debt (32 warnings)

### 1a. Auto-fixable (24) — `cargo clippy --fix --bin nexus-chat`

Then manually review the diff: 11 × collapsible `if`/`match`, 4 × needless
borrows, 2 × `map_or` simplify, 2 × immediate deref, 2 × redundant closure,
1 × useless `format!`, 1 × `let…else` → `?`, 1 × elidable lifetime.

### 1b. Manual (8) — judgment calls

| Location | Lint | Fix |
|---|---|---|
| `tools.rs:2051` | `unwrap` after `is_some` on `image_id` | Restructure to avoid the double check |
| `tools.rs:2964` | `sort_by` → `sort_by_key` | Mechanical |
| `tools.rs:2604` | `map_or` → `is_none_or` or match | Mechanical |
| `openrouter.rs:436` | constructor `openrouter` same name as type | Rename (e.g. `OpenRouter::new` / `from_key`) — check callers |
| `openrouter.rs:485`, `tools.rs:299` | too many arguments (8/7, 15/7) | Introduce a params struct (or `#[allow]` if the struct is worse) |
| `research.rs:839` | too many arguments (12/7) | Same — likely a `ResearchOptions` struct |
| `app/mod.rs:64` | elidable lifetime `'a` | Mechanical |

### 1c. Gate it

No CI exists. Add `scripts/check.sh`: `cargo fmt --check && cargo clippy --bin
nexus-chat -- -D warnings && cargo test --bin nexus-chat`. Reference it from
the README (Phase 4).

## Phase 2 — Structural debt: `#[allow(clippy::too_many_arguments)]`

7 existing allows that 1a/1b don't touch. Each is a real refactor:

- `research.rs:641, 944, 1002, 1067` — four pipeline fns
- `swarm.rs:394` — swarm orchestration
- `db.rs:769, 818` — two insert fns

For each: group parameters into a small struct (e.g. `InsertCtx`, `SearcherCtx`)
or split the fn. Only if a struct makes the call sites worse, keep the `allow`
with a one-line justification. All 7 are test-covered, so behavior is
verifiable.

## Phase 3 — Legacy shims & dead branches (invisible to the compiler)

| Location | Shim | Verdict |
|---|---|---|
| `research.rs:269` | `parse_plan_blocks` accepts a legacy JSON array of strings + bare-line fallback | **Keep** — robustness against model output drift, ~10 lines, tested. Add a comment noting it's intentional |
| `models.rs:83` | Legacy OpenRouter id normalization for old stored settings | **Keep** — functional; old settings rows exist in user DBs |
| `openrouter.rs:1082` | DALL-E legacy pixel-size param | **Keep** — provider compat |
| `openrouter.rs:2807` | `"c/legacy"` id in tests | **Keep** — tests legacy resolution path |
| `mod.rs:1415` | "(untagged streams count as viewed — legacy/test paths)" | Verify the untagged-stream path is still reachable in production; if test-only, simplify |

Also sweep stale doc comments that reference removed features (the git log
shows several removed: `/gen` command, `o` citations keybind, `PendingEditor::
ResearchPlan`, `research_plan_gate`, `$EDITOR` plan round-trip). Grep for those
names as comments.

## Phase 4 — Documentation

- **Write `README.md`** (none exists): what it is, build/run, keybindings,
  `/` commands, architecture map (src/ tree with one-liners), testing,
  `scripts/check.sh`.
- **Carry the 3 research limitations** from the deleted IMPLEMENTATION.md into
  README's "Known limitations" so they're not lost:
  1. Survey section is always visible (not collapsible).
  2. Survey round 1 is generated from topic alone — the concurrent
     known-chunks/web-survey context arrives after round 1.
  3. A plan rework presented within the same second overwrites the plan file
     (same timestamped name).

## Phase 5 — Verification

1. `cargo fmt --check`
2. `scripts/check.sh` (fmt + clippy `-D warnings` + 414 tests)
3. Manual smoke: launch, open a session, `/research` gated run, image paste.
4. Delete this file (`CLEANUP.md`), commit.

## Deferred (feature work, not cleanup)

From the earlier roadmap discussion: finish research loose ends (round-1
context, plan-file collision), local backend (Ollama), stateful apps
(POST + KV on the appserver), voice input, MCP client.
