# Agent mode and a self-improving harness

**Status: design.** Nothing here has landed. This is the plan for turning
nexus from a chat app with sandboxed tools into an agent that can work in a
directory on your machine — read and edit its files, run programs, and keep
long-running and looped jobs going — without losing the chat experience, and
for a harness that measures its own failures and can build its own terminal
UI.

The short version: agent mode is a **mode of a session**, not a second app.
It adds a workspace tool family behind the existing `ToolExecutor` seam, an
effect-aware scheduler for parallel tool calls, a permission layer with a
`Decider` (rules → Jev → LLM fallback → you), and jobs on `nexus host`.
Prompts and tool definitions get one source of truth with tests (§7), and
the harness gets an observe → propose → build loop whose UI output is
terminal-native (§8). Everything works on local models (§9), and every
model call is recorded as training data you control, so the harness can
eventually fine-tune the local model it runs on (§10). Cost is cut by
exact savings first (caching, prefix stability) and lossy context reduction
only behind evals; the evidence is in
[`cost-reduction-research.md`](cost-reduction-research.md).

**Scope: terminal only.** Nothing in this plan touches the web surfaces —
the host's web chat, `appserver.rs`, or the `app` tool's served web apps.
`nexus host` appears only as a headless scheduler and API.

## What exists today

| Piece | Where | Relevance |
|---|---|---|
| Tool loop, 100 iterations | `provider/openrouter.rs` (`MAX_TOOL_ITERS`) | Already multi-step; agent mode reuses it unchanged |
| `ToolExecutor` seam | `tools.rs:470` | Workspace tools are a new executor; loops don't change |
| Parallel calls | `openrouter.rs:1507` | **All-or-nothing**: concurrent only if every call is read-only, else fully sequential |
| Read-only classification | `tools.rs:3288` `is_read_only_tool` | A boolean by tool name; no notion of *which* paths a call touches |
| File confinement | `tools.rs:2219` and siblings | **String checks** (`starts_with('/')`, `contains("..")`); a symlink inside an app dir escapes it |
| Hash-anchored edits | `tools.rs:3535` `line_hash`, `:3585` `apply_hashline_edits` | Survives line shifts; reuse as-is for workspace edits |
| Grep | `tools.rs:3780` `grep_dir` | Naive recursive lowercase substring; skips a fixed dir list, ignores `.gitignore` |
| Process spawn | `tools.rs:3815` `run_cmd` / `run_cmd_env` | No shell, timeout, `kill_on_drop`, buffered `output()` |
| Output shaping | `format_output`, `MAX_TOOL_RESULT_CHARS = 8_000` | Keeps the **head**; for builds and tests the useful part is the tail |
| Approval gates | `app/mod.rs:615` `GateState`, `AppEvent::Gate` | The parking mechanism; today only research uses it |
| Periodic work | `app/watches.rs` | Checked once at startup; no daemon |
| Long-running daemon | `host/` (`nexus host`) | The natural home for a scheduler |

Two of these are bugs worth fixing independently of agent mode:

- **Children inherit the full environment.** `run_cmd_env` never clears env,
  so `run_python` and skill scripts can read `OPENROUTER_API_KEY` /
  `OPENAI_API_KEY` / `OPENCODE_API_KEY`. That breaks the "keys never leave
  the process" rule in CLAUDE.md.
- **Confinement is lexical.** `app/link -> /Users/you` passes the `..`
  check and `write_file` follows the link out of the app dir.

## 1. Modes

```
Session mode = { tools, prompt addendum, permission policy, workspace root }

  chat        today's ToolBox, no workspace
  agent       ToolBox + WorkspaceTools(root), policy = ask
  agent·auto  same, policy = Decider-approved, gate on low confidence
```

- `AppCommand::SetMode { mode, root }` with a `COMMANDS` entry (`/agent
  [path]`, `/chat`, `/auto`), so the CLI and host get it for free. The TUI
  binds Shift+Tab to cycle modes; the status line shows `agent · ~/proj`.
- **The model can ask to escalate.** In chat mode it gets one extra tool,
  `request_workspace { root, reason }`, which raises a `Gate`. "Fix the
  failing test in ~/proj" → the model asks → you approve → same thread,
  now in agent mode. Chat never has to be abandoned to do agent work.
- **Workspace root is device-local.** A path on your laptop means nothing on
  your phone. The session→root binding lives in the `cache` schema keyed by
  session id, not in the synced durable surface. A synced session opened on
  another device comes up in chat mode with a note.
- The system prompt gets an agent addendum (workspace layout, the
  read-before-edit rule, how to run tests) only in agent modes, so chat
  sessions don't pay for it in tokens or cache misses.

## 2. Workspace and file handling

A `Workspace` value owns the root and every path decision. Tools never touch
a raw path; they ask the workspace to resolve one.

```rust
pub struct Workspace {
    root: PathBuf,              // canonicalized once at SetMode
    ignore: ignore::gitignore::Gitignore,
    reads: Mutex<HashMap<PathBuf, FileStamp>>, // what the model has seen
}

pub struct FileStamp { len: u64, mtime: SystemTime, sha256: [u8; 32] }

impl Workspace {
    fn resolve(&self, rel: &str, intent: Access) -> Result<PathBuf, PathError>;
}
```

### Confinement

- `resolve` joins, then **canonicalizes** (for a new file, the nearest
  existing ancestor) and requires the result to sit under `root`. Symlinks
  pointing outside are refused for writes and flagged for reads.
- Outside-root access is not impossible, it's a different **effect**
  (§4): `fs.read ~/.zshrc` is allowed to exist as a request and is gated,
  never silently refused or silently done.
- Hard denies regardless of mode: `.git/` internals (use git tools),
  `.env*` and key files for writes, nexus's own data root.
- The same `resolve` replaces the string checks in the app/script tools,
  which fixes the symlink escape there too.

### Tools

| Tool | Behavior |
|---|---|
| `fs.list { path, depth }` | gitignore-aware tree (the `ignore` crate's walker); counts, not contents, for big dirs |
| `fs.glob { pattern }` | gitignore-aware; capped list with a "N more" tail |
| `fs.grep { pattern, regex?, path?, context? }` | uses `rg` when present (fast, respects ignore files), falls back to an improved `grep_dir`; results grouped by file with line numbers |
| `fs.read { path, offset?, limit? }` | line-numbered with hashline anchors; binary detection; files over ~2k lines are paged, never truncated silently; PDFs/images go through the existing extract/vision path |
| `fs.edit { path, edits[] }` | `apply_hashline_edits` unchanged; returns the diff |
| `fs.write { path, content }` | create or full rewrite |
| `fs.move` / `fs.delete` | always at least write-tier; delete of tracked files is exec-tier (§4) |

### Write safety

- **Read-before-write.** `fs.edit`/`fs.write` on an existing file require
  a `FileStamp` from an earlier `fs.read` in this session. Missing stamp →
  "read it first".
- **Stale-write guard.** If the file's stamp no longer matches (you edited
  it in your editor, or a job did), the write is refused with a short diff
  of what changed. This is the main thing that makes "agent working in the
  directory I'm also working in" safe.
- **Atomic writes.** Temp file in the same dir, `fsync`, `rename`. Preserve
  permissions and line endings; never re-encode a file the model didn't
  change.
- **Checkpoints.** Before the first mutating call of each turn, snapshot the
  touched files into a shadow git repo under the cache dir
  (`cache/checkpoints/<session>/`, `GIT_DIR` pointed there, `GIT_WORK_TREE`
  at the root). This never touches the user's own `.git`, works in non-git
  dirs, and gives `/undo` (one turn) and `/rewind <n>`. Cost is
  proportional to files touched, not repo size.
- **Diff cards.** Every edit result carries the diff; `ui/cards.rs` renders
  it as a collapsed card with `+N −M`, expandable. No new event: the diff is
  in the tool result.

## 3. Running programs

### One-shot commands

`shell.run { cmd, cwd?, timeout_s?, env? }`

- Runs `sh -c` (user's `$SHELL` is tempting but makes behavior
  machine-dependent; `sh` plus a login-PATH capture at startup is the
  predictable choice). `cwd` resolves through the workspace.
- **Own process group** (`setsid` via `pre_exec`/`process_group(0)`), and
  timeout or cancel kills the *group* — `kill_on_drop` alone leaves
  grandchildren (`npm` → `node`) running.
- **stdin is `/dev/null`**, and `CI=1`, `GIT_TERMINAL_PROMPT=0`,
  `PAGER=cat`, `GIT_PAGER=cat` are set, so nothing blocks on a prompt.
- **Scrubbed environment.** Start from an allowlist (`PATH`, `HOME`,
  `LANG`, `TERM`, `USER`, `SHELL`, `TMPDIR`, toolchain vars like `CARGO_HOME`,
  `RUSTUP_HOME`, `NVM_DIR`, `VIRTUAL_ENV`), then add per-workspace extras
  the user configured. Nexus's own keys are never passed. The same scrub
  applies to `run_python`.
- **Streaming.** Output streams to the TUI as it arrives (a live tail in the
  tool card), while the model gets a shaped result at the end.
- **Shaped results.** Exit code first, then head (~40 lines) + tail
  (~160 lines) with the gap marked, under the existing 8k cap. The full log
  is saved to `cache/runs/<id>.log` and the model can page it with
  `shell.log { id, offset }`.
- Default timeout 120s, max 600s; past that, use a background process.

### Background processes

Dev servers, watchers, and long builds can't block a turn.

| Tool | Behavior |
|---|---|
| `proc.start { cmd, name?, ready_when? }` | spawns detached from the turn; `ready_when` is a regex on output ("listening on") or a port; returns once ready or after a short grace period |
| `proc.read { id, since? }` | new output since the last read |
| `proc.list` | running processes with uptime and last line |
| `proc.kill { id }` | kills the group |

Processes belong to the session and are killed when the session's app exits,
unless they were started by a host job (§6). A `procs` popup lists them; the
status line shows a count.

### Sandboxing

Approval decides *whether* a command runs; a sandbox limits *what it can do*
if the approval was wrong. Tiers, configurable per workspace:

| Tier | macOS | Linux |
|---|---|---|
| `none` | plain spawn | plain spawn |
| `workspace-write` (default for auto) | `sandbox-exec` profile: write only under root + tmp, no writes to `~` | `bwrap` or Landlock: same rule |
| `+no-network` | profile denies network | network namespace |

`sandbox-exec` is deprecated but still what every macOS agent uses; if it is
missing, the tier degrades to `none` **and the UI says so** — never silently.
Commands run under `ask` mode default to `none`, since you approved each one.

### Privilege escalation is locked

The model can never run anything as root, in any mode, at any sandbox tier.
This is a `RuleDecider` hard deny (§5) that no approval, Decider verdict,
or setting overrides. A string check on `sudo` alone isn't enough —
`echo x | sudo tee`, `env sudo`, `bash -c "sudo …"`, `/usr/bin/sudo`, or a
`make install` that calls sudo inside all get past it — so the lock is
layered, and each layer covers what the one before misses:

| Layer | Catches | How |
|---|---|---|
| 1. Parse | sudo anywhere in the command line | Tokenize the `sh -c` string (pipes, `;`, `&&`, `$( )`, backticks), unwrap launchers (`env`, `command`, `exec`, `nohup`, `time`, `xargs`, nested `sh -c`/`bash -c`) and deny any command word in the escalation set: `sudo`, `doas`, `su`, `pkexec`, `run0`, `login`, `osascript … with administrator privileges`. A command that can't be parsed is **asked about**, never allowed. |
| 2. Shim | sudo called by a script or build | A nexus-owned dir prepended to `PATH` with stub `sudo`/`doas`/`su`/`pkexec` that exit 1 with "privilege escalation is blocked — ask the user to run it with `!`". |
| 3. Kernel | absolute paths and anything else | Linux: `prctl(PR_SET_NO_NEW_PRIVS)` in `pre_exec`, so setuid binaries can't gain privileges at all. macOS: the sandbox profile denies `process-exec` of the escalation binaries, and runs even at tier `none` with only that rule. |
| 4. No credentials | a cached sudo ticket | stdin is `/dev/null` and there is no controlling TTY, so sudo can't prompt; `SUDO_ASKPASS` is never passed through the env scrub. |

A blocked call returns an error that tells the model what to do instead:
propose the command to you (below).

### Your own commands

You can run anything yourself, including `sudo`. Commands you type are
yours: no Decider, no gate, no sandbox.

- **`!cmd` in the composer** runs `cmd` in the session's workspace root (or
  the space dir in chat mode) as an `AppCommand::RunUserCommand`. Output is
  captured and added to the conversation as a user-run block, so the model
  sees it on its next turn: `! cargo test`, then "fix that".
- **Interactive commands get the real terminal.** If the command is
  interactive — anything in the escalation set, or prefixed `!tty` (for
  `ssh`, `vim`, `htop`) — the TUI suspends exactly the way Ctrl+E does for
  `$EDITOR` (`edit_in_external_editor`, `events.rs:391`): restore the
  terminal, run the child with inherited stdio, re-init. **sudo's password
  prompt goes straight to your terminal; nexus never sees the password.**
  Only the command and exit code go into the conversation, with a note that
  output wasn't captured. You can paste the relevant output in afterwards.
- **Your environment, scrubbed output.** A user command runs with your full
  environment (it's you), but its captured output passes through the
  secret scrubber (§10) before it reaches the context or the trace store.
- **The model can hand a command to you.** A `propose_command { cmd, reason
  }` tool puts `! cmd` into your composer via `AppEvent::ComposerSet` and
  shows the reason in the status line. Nothing runs until you read it,
  optionally edit it, and press Enter. This is the model's only route to a
  privileged command.
- **TUI only.** `RunUserCommand` is refused over `nexus host` and the CLI's
  headless paths: over the host it would be remote shell access behind a
  bearer token. Jobs can't issue it either.
- **Recorded as yours.** Traces store user-run commands as user actions,
  never as model output, so training never teaches the model to run sudo.

## 4. Effects, parallelism, and the scheduler

Today parallelism is all-or-nothing: one write in a batch of six reads makes
all six sequential. Replace the boolean with an **effect** that each call
declares before it runs.

```rust
pub struct Effect {
    reads:   Vec<PathBuf>,  // canonical, from Workspace::resolve
    writes:  Vec<PathBuf>,
    exec:    bool,          // runs a program: conflicts with everything
    barrier: bool,          // mutates state not describable by path
    network: bool,          // informational
}

trait ToolExecutor {
    fn is_read_only(&self, name: &str, args: &str) -> bool;
    fn effect(&self, name: &str, args: &str) -> Effect; // default: read-only or barrier
    // …
}
```

`effect()` has a default built from `is_read_only`, so research, swarm and
test executors keep working unchanged; `ToolBox` overrides it with paths for
`fs`, `exec` for `shell run`, the union for `batch`, and barriers for every
other mutating tool.

### Scheduling

The calls from one model response go through a small scheduler:

1. Compute every call's `Effect`.
2. **Approve the whole wave up front.** Collect every call that needs
   approval into *one* gate ("4 edits, 1 command — approve all / review /
   deny"), so you aren't asked five times in a row. The `Decider` (§5) runs
   over all of them in one Jev request.
3. Build conflict edges: two calls conflict if one writes a path the other
   reads or writes, or if either is `exec` (a program can touch anything
   under root, so exec is a barrier).
4. Run in waves: each wave is a maximal set of non-conflicting calls in
   original order; waves run one after another, calls within a wave
   concurrently (`join_all`, bounded at 8).
5. Zip results back in call order — unchanged from today, so the transcript
   and cache prefix stay stable.

So six reads + edits to three different files run as one concurrent wave;
two edits to the same file stay ordered; a `cargo test` waits for both.

`batch` gets the same scheduler for its sub-calls instead of running them
strictly in order, and its read-only classification uses `Effect`.

### Parallel sub-agents (later)

A `task { prompt, tools: read-only | workspace }` tool spawns a nested loop
with its own context and returns only its final report — useful for "find
every caller of X" or exploring three directories at once. Same machinery as
swarm personas (`app/swarm.rs`); read-only sub-agents run concurrently,
writing sub-agents get a git worktree each. Deferred until the base loop has
shipped, because it multiplies cost.

## 5. Permissions and the Decider

```rust
// Boxed future, like ToolExecutor::run, so it stays dyn-compatible
// without pulling in async-trait.
pub trait Decider: Send + Sync {
    fn decide<'a>(&'a self, req: &'a DecisionRequest)
        -> Pin<Box<dyn Future<Output = Decision> + Send + 'a>>;
}
pub struct Decision { verdict: Verdict, p: f64, by: &'static str }
pub enum Verdict { Allow, Deny, Ask }
```

Chained, first non-`Ask` wins:

1. **`RuleDecider`**: hard denies (writes outside root without a grant,
   `rm -rf` outside root, `git push`, `curl … | sh`, reading key files,
   any privilege escalation — §3),
   plus user allow rules (`allow shell "cargo test*"`). Deterministic,
   always first, cannot be overridden by a model.
2. **`JevDecider`**: one `POST /api/alpha/decisions` call per wave,
   `typesafe/jev-1.13` pinned. A compact state (task, root, last few steps,
   the calls) and per call a `noul` "serves the task?" plus a `score` risk
   scale. Allow only if `p(in_scope)` and the risk position clear
   thresholds set by tier. Only available with an OpenRouter key; it is a
   `decide()` method on the existing client, not a new client.
3. **`LlmDecider`**: the same questions as structured output from a cheap
   chat model, for non-OpenRouter backends.
4. Otherwise **you** — a `Gate`, showing the probabilities
   (`in_scope 0.94 · risk: reversible edit`), not an invented explanation.

Policy by mode:

| Effect tier | `agent` (ask) | `agent·auto` |
|---|---|---|
| Read (in root) | allow | allow |
| Write (in root) | ask, "allow for session" option | Decider |
| Exec | ask, remember exact command or prefix | Decider + sandbox |
| Outside root / network exec | ask | ask |
| Destructive | ask, no remember | ask, no remember |

**Every decision is logged** (`decisions` table in `cache`: request, Jev
probabilities, verdict, and your answer when it went to a gate). Your
approvals and denials are labeled data for tuning thresholds, which is
exactly what TypeSafe's guidance asks for. Jev confidence describes the
spread of answers, not safety; thresholds come from what each mistake costs.

## 6. Jobs

Watches generalize into jobs that run on `nexus host`, so they survive the
TUI closing.

```
Job {
  id, session_id, device_id,
  prompt, root, mode: agent | agent·auto,
  trigger:  interval | cron | file-change(glob) | until-done | once-at,
  budget:   max_iters, max_cost_usd, max_wall,
  stop:     decider-judged condition ("tests pass") | iteration cap,
  notify:   always | on-change | on-failure,
}
```

- Runs headlessly (`app/headless.rs`) through the same loop, same tools, same
  Decider. Writing jobs work in a **git worktree** (or a copy for non-git
  dirs) so they never fight with your working tree; results come back as a
  branch/diff you merge from a review card.
- **Until-done loops**: after each iteration a Jev `choice` {continue, done,
  ask_user, abort} over a state that includes hard evidence where it exists
  (the exit code of the test command). Hard evidence wins over the model.
- **Unattended approvals park.** A call that would gate parks the job and
  notifies; nothing blocks forever and nothing proceeds unapproved.
- **Notifications** use a Jev `noul` "meaningful change since last run?" for
  `on-change`, which is what makes interval jobs tolerable.
- Data: `jobs` and `job_runs` are durable (they show up on every device), so
  each needs a `sync.rs` registry entry; runs are pinned to `device_id`
  because their paths are local. Run logs live in `cache`.
- Watches become a job with `trigger: interval` and the research pipeline as
  its body; `due_watches` moves to the host scheduler.

## 7. Prompts and tool definitions

### What's wrong today

| # | Problem | Where |
|---|---|---|
| 1 | **The base prompt freezes at first run.** `load_system_prompt` copies the bundled default into `~/.config/nexus-chat/system_prompt.md` once and reads that copy forever. Every later fix to `system-prompt-base.md` never reaches an existing user, and an old copy silently lacks newer placeholders. | `config.rs:123` |
| 2 | **Stale facts in the prompt.** "You run over OpenRouter" is false on the OpenAI, Zen/Go, and Codex backends. "Use the web-search tool" names a tool that doesn't exist (it's `search` with `mode=web`). | `assets/system-prompt-base.md` |
| 3 | **The model doesn't know what the terminal can render.** Mermaid fences now render as diagrams and `/play` mini-apps exist, but the prompt only says "this is a terminal, not a browser". | base prompt vs `diagrams.rs` |
| 4 | **Action-multiplexed tools are one run-on sentence.** `app` explains ten actions in a ~900-character paragraph; `media` exposes 27 flat parameters with no indication which belong to which action. The model guesses, and the schema can't say what each action requires. | `tools.rs:742`– |
| 5 | **Errors don't say how to recover.** `invalid path: "x"` or `missing required field: name` — no hint of which action needed it or what to do next. | `tools.rs:115`, call sites |
| 6 | **Prompts are scattered string constants.** Eight research prompts are one-line `const`s in `research.rs`, others live in swarm, compaction, OCR. Hard to review, diff, or test as text. | `research.rs:154`– |

### One source of truth for prompt text

- **Bundled base is always current; the user file becomes an overlay.**
  `system_prompt.md` changes meaning from "the whole prompt" to "your
  additions", appended after the base. Migration on startup: if the file is
  byte-identical to any previously shipped default (keep a list of their
  SHA-256s), delete it; if it was edited, keep it as the overlay and post a
  one-time status note. Ctrl+E keeps working.
- **Prompts move to `crates/core/assets/prompts/*.md`**, loaded with
  `include_str!`: `base.md`, `agent.md`, `research/planner.md`, and so on.
  Markdown diffs in review, and the "model-facing text" rule in CLAUDE.md
  then covers one directory.
- **Facts are generated, not written.** An `## Environment` section built in
  code: backend and model name, mode, workspace root, OS, and which feature
  sections are live. It can't go stale, because nothing about it is
  hand-written.
- **A rendering-capabilities section** generated from what the TUI actually
  supports (Mermaid subset, `nexus-ui` blocks from §8), so prompt and
  renderer can't drift apart.

### Assembly order

Ordered by how often each part changes, so the provider's prompt cache
reuses the longest possible prefix — across sessions, not just within one:

```
base (static)  →  mode addendum  →  rendering capabilities
  →  feature sections (files/apps/scripts, only when live)
  →  space instructions  →  user overlay  →  skills  →  memory
  →  volatile: datetime, web-mode clause, research-session note
```

Today `{{datetime}}` sits in the first paragraph of the base prompt, so no
two sessions share even the static prefix. Moving it to the tail costs
nothing (it's already frozen per cache epoch) and makes the static part
cacheable everywhere.

### Tool definitions

A small table per tool generates both the schema text and the validation:

```rust
struct ActionSpec {
    name: &'static str,          // "patch"
    summary: &'static str,       // one line, imperative
    required: &'static [&'static str],
    optional: &'static [&'static str],
    read_only: bool,             // feeds Effect (§4)
}
```

- The description renders as: one-line purpose, *use when / don't use
  when*, then one line per action (`patch — apply hash-line edits; requires
  app, path, edits`). Params not used by any live action are dropped.
- Validation checks the action's own `required` list and names it in the
  error: `app.patch needs "edits" (hash-line edits from your latest read)`.
- Error template everywhere: **what failed · why · what to do next**
  (`stale write: README.md changed since your read · re-read it and
  re-apply`).
- `is_read_only_tool`'s hand-maintained match goes away; read-only-ness
  comes from the same table, so a new action can't be misclassified.

### Tests that keep prompts honest

All hermetic, all in `scripts/check.sh`:

- **Snapshots of the assembled prompt + tool defs** for representative
  configurations (chat, agent, research, each backend). A behavior change
  becomes a visible diff in review.
- **Every tool name a prompt mentions exists** in the defs for that
  configuration — this would have caught "web-search tool".
- **Every placeholder resolves**; no `{{` survives assembly.
- **Token budget**: base + defs for the chat configuration stay under a
  fixed ceiling, so prompt growth is a deliberate choice.

### Evals (outside the gate)

"Better" needs a number. `nexus eval` runs golden tasks from `evals/*.toml`
(prompt, tools enabled, a checker: regex on answer, file state, or tool-call
shape) against a real backend and reports task success, invalid-tool-call
rate, retries, tokens, and cost. It needs keys and network, so it is a manual
or scheduled job, never part of `check.sh`. Every prompt or tool-def change
lands with a before/after eval run in the PR description.

## 8. Self-improving harness

Three loops, each one useful without the next, in increasing order of power.

### Loop 1: Observe

Tool results become structured internally — `ToolOutcome { ok, error_kind,
latency, tokens }` alongside the text the model still sees — and land in a
`tool_events` table in `cache` with the tool, action, and whether the model
retried. Together with the decision log (§5) and user corrections
(`/retry`, edit-and-resend, a denied gate) this answers "where does the
harness fail?": `app.patch: hash mismatch ×14 this week`, `scripts.run:
missing field "skill" ×9`.

`/harness` opens a popup (chrome.rs) ranking failure modes, each linking to
the transcripts where it happened.

### Loop 2: Propose

A `harness-review` job (§6), weekly or on demand, reads those stats and the
offending transcripts and proposes **data changes only**: prompt overlay
edits, tool-description rewrites, new skills, Decider threshold tweaks. Each
proposal shows as a diff card with the eval run (before/after) attached. You
approve or reject; nothing applies on its own. Rejections are logged, so the
same proposal doesn't come back next week.

### Loop 3: Build terminal UI

The model gets three levels of UI authorship, cheapest and safest first.

**Level A — declarative blocks (`nexus-ui`).** The model emits a fenced
`nexus-ui` JSON block (or calls a `ui.show` tool), rendered by a fixed set
of components drawn in the `ui/style.rs` vocabulary:

| Component | Use |
|---|---|
| `card` | titled box with body and footer metadata |
| `table` | sortable columns, row selection |
| `kv` | key-value summary |
| `progress`, `sparkline`, `bars` | job status, small charts |
| `checklist` | plans and task lists, toggleable |
| `choices`, `buttons` | ask the user to pick |
| `form` | text, select, toggle fields |

Inline blocks reuse the fixed-height slot work from the Mermaid previews, so
viewport pinning holds; larger ones open as a popup on chrome.rs.
Interactive elements emit `AppCommand::UiEvent { block, action, value }`,
which becomes a turn the model sees, so a button press continues the
conversation. No code runs; the model can only arrange known components.
Invalid JSON falls back to the source as a code block, like Mermaid errors.

**Level B — scripted terminal apps.** For things a static block can't do
(games, live dashboards, editors), the model writes a small script in the
space (`tui_apps/<name>/app.rhai`) exporting `init()`, `update(state,
event)`, and `view(state)`. `view` returns the Level A component tree plus a
`canvas` primitive (ratatui `Canvas`) for free drawing. It runs inside the
existing `MiniApp` popup and tick loop (`events.rs`), so there is still only
one event loop.

- **Embedded interpreter, no machine runtime.** Rhai is pure Rust, has no
  file, network, or process access unless a function is registered, and
  enforces operation and memory limits. Capabilities (call a model, read a
  space file) are opt-in through a manifest and granted at a gate.
- **The model sees its own UI.** `tui_app { action: preview }` renders
  `view()` to a ratatui `TestBackend` buffer and returns the text grid, with
  script errors and the event trace. That closes the loop: write → preview →
  fix, without a human in between. The same mechanism turns into snapshot
  tests for saved apps.
- **Proof of sufficiency:** port rock-paper-scissors to a script and keep
  both. If the script version can't match the native one, the runtime is
  missing something.
- Hash-line editing, `list`/`write`/`patch`/`run` actions, same as scripts.

**Level C — changing the harness itself.** Agent mode pointed at the nexus
repo: a worktree, `scripts/check.sh` as the acceptance test, the result a
branch you review. This is how a Level B app that proved itself becomes a
native popup, or a recurring Loop 2 proposal becomes code. The running
binary is never patched in place.

### Guardrails

- Model-authored UI text goes through the same sanitizing as history:
  control characters and escape sequences stripped, clipped to its slot.
  A script can't write raw bytes to the terminal.
- The emoji ban and color roles apply to the component renderer, not to
  what the model writes, so components look native whatever the model does.
- Scripts never see keys, env, or paths outside their app directory.
- None of this goes through the host's web UI or `appserver.rs`; the `app`
  tool and its web apps are unchanged.

## 9. Local models

The `Local` backend already exists (`/local ollama|mlx|mlx-serve|lmstudio|
edge0`, `backends.rs`, `openrouter.rs:54`) and goes through the one client.
What's missing is that the rest of the harness assumes a large hosted model.

### Model profiles

Local models differ from each other far more than hosted ones do, so the
harness needs to know what the active model can handle:

```rust
pub struct ModelProfile {
    tools: ToolSupport,        // Native | Prompted | None
    parallel_calls: bool,      // today `parallel_tool_calls: true` is hardcoded (openrouter.rs:1898)
    json_schema: bool,         // server supports constrained output
    context: usize,            // drives caps and compaction
    tier: Capability,          // Small | Medium | Large, from evals, not size
}
```

Profiles come from three places, later overriding earlier: built-in
defaults per server (Ollama, LM Studio, mlx-lm), what the server reports
(context length), and `nexus eval` results for that exact model. A profile
can be edited in the model picker.

### What changes for a local model

- **Fewer, shorter tools.** The `ActionSpec` table (§7) renders a compact
  description variant, and tools irrelevant to the mode are dropped: in
  offline agent mode there's no `search`, `fetch_url`, or `media`. Every
  tool definition is prompt tokens a small model must prefill and attend to.
- **Constrained output where the server supports it.** llama.cpp, LM Studio
  and Ollama accept a JSON schema for output; use it for the Decider and for
  `Prompted` tool calling, so a small model can't produce a malformed call.
- **One repair attempt.** A malformed tool call gets its parse error fed
  back once ("arguments were not valid JSON: …") before the turn fails.
  Both the bad and the repaired call are recorded (§10) — they're the most
  useful training examples there are.
- **Caps scale with context.** The 8,000-character result cap and the
  compaction threshold become fractions of `profile.context`. 8k characters
  is a quarter of an 8k-token window.
- **Prefix stability matters more locally.** llama.cpp and mlx-lm reuse
  the KV cache for an identical prefix, and on a laptop prefill is most of
  the latency. The static-first assembly order (§7) is what makes that
  reuse possible.
- **The Decider degrades honestly.** Jev needs the network. Offline,
  `agent·auto` uses `RuleDecider` plus an `LlmDecider` on the local model
  only when the profile tier is Medium or higher; otherwise auto falls back
  to ask, and the status line says why.
- **Environment section (§7) says "offline"**, so the model doesn't promise
  a web search it can't do.

## 10. Training data

Three different things are easy to conflate:

| Record | What it holds | Today |
|---|---|---|
| History | what you see; editable, deletable, synced | `messages`, `sessions` |
| What the model saw | the exact request: assembled prompt, tool defs, history *after* compaction, capping, and "unchanged result" notes | **not stored** — rebuilt on the fly by `build_history` |
| What happened next | tool outcomes, your approvals and denials, retries, whether the task succeeded | **mostly not captured**: there is no retry/regenerate command to produce a rejected sample, gate answers aren't linked to the call they judged, and deleting a session removes everything (`db.rs:1196`) |

Training needs the second and third, so they get their own store instead of
being reconstructed from history after the fact.

### The trace store

An append-only record per model call (a **step**):

- the exact request: system prompt, tool defs, messages, params — prompts and
  tool defs are content-addressed blobs, since they repeat on every step
- the raw response: content, reasoning, tool calls *including malformed
  ones*, finish reason
- model id, backend, the provider's dated revision, prompt-version hash,
  `ModelProfile`, tokens, latency
- per tool call: the full result (uncapped) and the capped text the model
  actually saw
- labels, attached as they arrive: the `ToolOutcome` (§8), the gate
  decision and Decider probabilities (§5), `/retry` (this step is a
  rejected sample), edit-and-resend, a job's until-done verdict, an eval
  checker's pass/fail

It lives in `traces/` beside `nexus.db`: an SQLite index plus compressed blob
files. It is **device-local and never synced** — large, private, and not
something your phone needs — and it is **not disposable** like `cache.db`, so
`/backup` includes it on request.

### Consent and privacy

- **Off by default, per space.** `training: off | on` in space settings;
  the status line shows a marker while recording. Incognito is never
  recorded. A workspace can opt out on its own (work code).
- **Scrubbed at write time.** Exact-match against every key nexus holds in
  memory (OpenRouter, OpenAI, OpenCode, Codex tokens, the host bearer), plus
  patterns for common secrets (private keys, `sk-…`, bearer headers,
  `.env` contents). Scrubbed again at export, because patterns improve.
- **Deleting a session asks** whether to delete its traces too; the default
  is yes. A `/retry` keeps the rejected step in traces, because that's a
  preference pair, not something you deleted.

### Whose outputs you can train on

Every step records which model produced it, and export filters on that:

- **Open-weight local models**: allowed, subject to the model's license
  (Qwen and Mistral releases are typically Apache-2.0; Llama has its own
  terms).
- **OpenRouter**: models flagged distillable (`is_trainable_text`). Spaces
  with training on can set OpenRouter's `enforce_distillable_text: true`
  provider preference, which restricts routing to models whose authors
  permit distillation and errors rather than falling back. OpenRouter calls
  its distillability data best-effort, so the license check stays yours.
- **OpenAI, Codex, and other proprietary outputs**: excluded by default.
  Their terms restrict using outputs to build competing models. That's a
  reading of terms, not legal advice; an explicit allowlist overrides.

### Export

```
nexus train export --format chat-jsonl|sharegpt|dpo \
                   --filter success --models local:*,distillable \
                   --since 2026-10-01 --out data/
```

- **Chat JSONL with tools**, the format `mlx_lm.lora`, Unsloth, and axolotl
  read; **DPO pairs** from `/retry`, denied gates, and repaired calls.
- **Filter on outcome**, not volume: trajectories that ended in success
  (approved, tests passed, eval checker passed, no correction).
- **Stale tool schemas are rewritten or dropped.** Tool definitions will
  change (§7); training on old tool names teaches a model to call tools that
  no longer exist. Each step carries its prompt-version hash; export maps
  old names through a migration table or drops the step.
- **Held-out evals stay held out.** Anything matching an `evals/` task is
  excluded, or `nexus eval` stops meaning anything.

### What fine-tuning is good for

- **Tool-calling reliability and your conventions**: high value, and the
  main reason to do this. Small local models fail at tool calls far more
  often than at prose.
- **Preferences** (style, when to ask vs. act): DPO on your own
  retries and denials.
- **Distillation**: run agent tasks on a strong distillable model, record,
  train the local model on the successful ones.
- **Knowledge: use retrieval, not fine-tuning.** Fine-tuning facts in is
  unreliable and makes models confidently wrong. Knowledge belongs in
  memory, embedded files, and a `history` search tool over past sessions and
  traces — all of which work immediately and stay correct when facts change.

### Loop 4: Learn

```
traces → export → mlx_lm.lora (job, §6) → adapter
      → served by mlx-serve / Ollama → appears as a local model
      → nexus eval: base vs tuned vs teacher → promote if better
```

Nexus produces the data and judges the result; training itself is an
external program the harness runs as a job (in agent mode, through
`shell.run`). No training code in core.

## 11. Surfaces

| Surface | Additions |
|---|---|
| TUI | mode in status line, Shift+Tab, diff cards, live command tail, wave-approval gate, `/undo` `/rewind`, `procs` `jobs` `harness` popups (chrome.rs), `nexus-ui` blocks, scripted terminal apps, recording marker, model-profile editor |
| CLI | `nexus agent --cwd . [--auto] "…"`, `nexus jobs ls|add|stop|logs`, `nexus eval`, `nexus train export` |
| Host | headless only: `/jobs` endpoints, SSE for job progress, gate approval over the API (phone can approve a parked job). No web UI changes. |

## 12. Phases

Each phase ships on its own and passes `scripts/check.sh`.

1. **Fix the security bugs.** Env scrub for every spawn; canonical
   confinement for app/script tools. Small, independent, worth doing first.
2. **Exact cache fixes** ([cost research](cost-reduction-research.md) §3,
   "do now" #1–#3 and #5). The Anthropic breakpoint follows the tool loop
   instead of staying where `build_history` put it; OpenRouter gets a
   `session_id` for sticky routing; research and swarm loops get the same
   caching; a forced `/skill` moves to the tail. Same tokens, same model, so
   no quality risk, and the largest cost win in the plan.
3. **Prompt foundations.** Base-prompt overlay and migration, prompts moved
   to `assets/prompts/`, generated environment and rendering sections,
   static-first ordering with `{{datetime}}` at the tail, the prompt tests.
   Fixes stale facts for every user immediately.
4. **Tool-definition tables.** `ActionSpec` for every tool, generated
   descriptions (full and compact), per-action validation,
   recovery-oriented errors, `ToolOutcome` and `tool_events` (Loop 1).
5. **Trace store and `nexus eval`.** Recording, scrubbing, per-space
   consent, a `/retry` command (an `AppCommand`) whose replaced answer is
   kept as a rejected step, and the eval runner. Early on purpose: data not
   recorded now is gone, and every lossy change after this needs eval
   numbers before it ships.
6. **Model profiles and local runtime.** Per-model tool support, parallel
   calls, context; Ollama launched with a real context length; caps and
   compaction scaled to context with head + tail capping; compact tool
   defs; one-shot repair of malformed calls; q8_0 KV cache as an option.
7. **Workspace + `agent` mode.** `Workspace`, `fs.*`, `shell.run`, mode
   switching, read-before-write and stale-write guards, checkpoints and
   `/undo`, ask-every-time approvals, the privilege lock, and `!` user
   commands with `propose_command`. This alone is a usable coding agent.
8. **Effects + scheduler.** `effect()` replaces `is_read_only`; wave
   execution; one gate per wave; `batch` on the scheduler.
9. **Context-reduction experiments**, each shipped only if `nexus eval`
   shows no loss for that model tier (cost research "experiment first"
   #8–#10, #12): bulk observation masking of old tool results, compaction
   that keeps a verbatim tail and an itemized digest, an absolute token
   trigger, a 1-hour cache TTL for slow sessions.
10. **`nexus-ui` Level A blocks** and the `/harness` popup.
11. **Decider + `agent·auto`.** Rule, Jev, LLM implementations, offline
    degradation; decisions log; sandbox tiers.
12. **Background processes.** `proc.*`, process-group kill, streaming tails.
13. **Jobs on the host.** Scheduler, worktrees, until-done, notifications,
    watches migrated, `harness-review` (Loop 2).
14. **Export and Loop 4.** `nexus train export`, license filtering, schema
    migration, eval decontamination, an MLX LoRA job recipe.
15. **Scripted terminal apps (Level B)** with preview, RPS port.
16. **Sub-agents, Level C self-modification, Jev Router.** The router is
    evaluated per session, never switched per step: the routing evidence is
    single-turn only, and every model switch starts a cold cache.

### Testing

Hermetic as always: temp-dir workspaces, `Db::open_in_memory`, no network.

- Confinement: symlink escapes, `..` after canonicalization, new-file
  ancestors, case-insensitive filesystems.
- Stale-write: modify after read → refused; after re-read → allowed.
- Scheduler: conflict graphs and wave order as pure-function tests;
  results always in call order.
- Process: group kill leaves no grandchild (spawn `sh -c 'sleep 60 &
  wait'`); env scrub (child prints env, assert no `*_API_KEY`).
- Privilege lock: a table of evasions (`env sudo`, `bash -c "sudo …"`,
  `$(which sudo)`, `/usr/bin/sudo`, a script calling sudo) all blocked by
  some layer; unparseable commands gate; `RunUserCommand` refused from the
  host and from jobs.
- Decider: Jev responses from a mock server (like the existing host test
  mocks); rule precedence over model allow.
- Jobs: scheduler with an injected clock; park-on-gate.
- Prompts: the snapshot, tool-name, placeholder, and budget tests in §7.
- `nexus-ui`: each component snapshot-tested on `TestBackend` like the
  popups; malformed JSON falls back to source; viewport pinning holds with a
  block streaming in above the viewport.
- Scripted apps: op-limit and memory-limit enforcement, no registered I/O
  without a manifest grant, RPS script parity with the native game.
- Traces: a configured key's value never appears in any stored blob;
  incognito and training-off spaces record nothing; a retried step
  survives with a rejected label; export golden files per format; eval
  tasks never appear in an export.
- Profiles: caps and compaction thresholds follow `context`;
  `parallel_tool_calls` is omitted when the profile says so; a malformed
  call gets exactly one repair round.

## Open questions

- **Remote workspaces.** Should agent mode also target another machine
  through `nexus host` and the tunnel? The roadmap's remote `ToolExecutor`
  is exactly that seam; the `Workspace` API should not assume local paths
  in its signatures.
- **Default sandbox for `agent·auto`**: `workspace-write` with network on
  (package installs work) or off (safer)?
- **Jev price and latency.** Not published on the docs pages; a Decider call
  sits in front of every risky wave, so measure before making it the default.
  The endpoint is `/api/alpha/`, so expect shape changes.
- **Script language for Level B.** Rhai is pure Rust and sandboxed by
  default, but models have seen little of it. Luau (via `mlua`, vendored)
  has a built-in sandbox mode and models know Lua well, at the cost of a
  C build dependency and another `cargo audit` surface. Settle with a
  spike: have two models build the same small app in each and compare
  first-try success.
- **Which local stack and base model** to fine-tune: MLX on this Mac is the
  obvious trainer, but the base model decides license, context, and
  tool-calling quality. Pick it from `nexus eval` runs, not size.
- **Trace storage budget.** Full requests per step add up fast, even
  content-addressed; set a cap and a pruning rule (oldest unlabeled steps
  go first).
- **Should `UiEvent` turns be visible** in the transcript, or folded into
  the next assistant turn's context only?
- **Windows.** Process groups, sandboxing, and `sh -c` all differ; out of
  scope until someone needs it.
