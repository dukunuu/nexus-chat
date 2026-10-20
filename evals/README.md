# Golden tasks

Run these manually against a real backend before shipping prompt or tool changes:

```sh
nexus eval                              # ./evals, most recently used model
nexus eval ./evals --model MODEL
nexus eval ./evals --filter scripts --json
nexus eval --set experiments.mask_tool_results=true
nexus eval --set experiments.mask_tool_results=true --set experiments.mask_keep_rounds=5 --compare
nexus eval --model MODEL --set experiments.cache_ttl_1h=true --compare --json
nexus eval --compare-model anthropic/claude-sonnet-4.5 typesafe/jev-router
```

Keys and network access are required. **This costs real tokens and money.**
Evals run in incognito and never save sessions. They use your default space's
context and tools; chat tasks do not have an isolated filesystem. Agent tasks
use a disposable fixture copy (see below). Evals are not part of CI or
`scripts/check.sh`; unit tests use no model or network.

## Experiments and comparisons

`--set KEY=VALUE` is repeatable and accepts only `experiments.<field>` keys.
Overrides affect each task's app in memory, never the saved configuration.
`--compare` requires at least one `--set`: it runs all selected tasks with the
loaded config first, then the same tasks with the overrides. Each side gets a
fresh session and, for agent tasks, a fresh fixture copy. Every run's header
prints `experiments: <key=value, …>` with all effective values.

The comparison table shows each task's pass/fail, tool calls, prompt tokens,
cached prompt tokens, and cost for both sides, followed by totals and percentage
deltas for prompt tokens and cost, plus the before/after pass count. Percentages
are unknown when the baseline is zero or the cost is unavailable. Compare JSON
contains `baseline` and `variant` objects (each with `experiments`, `tasks`, and
`totals`, plus `model` when specified) and `deltas` with absolute and percentage
changes and both pass counts. Inspect individual task passes too: an unchanged
total can hide a regression.

`--compare-model A B` requires two model ids and is mutually exclusive with
`--compare`. It runs all selected tasks on A, then on B, with the same loaded
experiment config and the same `--set` overrides on both sides. It reuses the
comparison table with columns and totals labeled by model; `--json` emits both
runs as `baseline` (A) and `variant` (B), including their model ids and deltas.

Router ids such as `typesafe/jev-router` and `openrouter/auto` are normal session
model choices, not automatic per-step switches by nexus. Routers can choose a
different underlying model per request, making prompt caching less effective;
the routing evidence is single-turn only. **Use a router for a session only if
its cost per passed task (total cost divided by passes) beats the fixed model
without losing passes.** Unknown cost or zero passes cannot establish a win.

**An experiment becomes a default only after `--compare` shows no lost passes on
the model tiers you use.** The switches are measuring infrastructure; their
context-reduction behavior is implemented separately. All defaults preserve
current behavior. The optional config section is:

```toml
[experiments]
mask_tool_results = false
mask_keep_rounds = 10
mask_trigger_tokens = 0       # use the compaction threshold
compaction_tail_turns = 0      # whole-history digest
compaction_itemized = false
compaction_budget_tokens = 0   # percent-of-window trigger
cache_ttl_1h = false
```

Unknown experiment keys and invalid boolean/unsigned integer values are errors.

## Format

Each `.toml` file contains one task. Other files are ignored. Tasks are sorted by
name; `--filter` selects names containing the given case-sensitive substring.

```toml
name = "lists-scripts-with-the-scripts-tool" # optional; defaults to file stem
prompt = "Which scripts do I have in this space? Use your tools."
web = false                                # optional; defaults to false

[check]
answer_contains = ["scripts"]
answer_not_contains = ["unable to use tools"]
tool_called = ["scripts"]
tool_not_called = ["media"]
tool_args_contains = ["\"action\":\"list\""]
max_tool_calls = 3
no_invalid_tool_calls = true
```

All check fields are optional, but each task must have at least one effective
check (a non-empty list, a call limit, or `no_invalid_tool_calls = true`). Unknown
keys are errors, including inside `[check]`.

### Multi-turn and agent tasks

Give exactly one of `prompt` or a non-empty `turns` array. Turns are sent in
order in one session; answer checks apply **only to the final answer**, while
all tool checks count calls across **every turn**. Usage and cost are summed
across all requests in all turns.

```toml
name = "edit-and-test"
turns = ["Read greet.sh and test.sh", "Change the greeting to Hello and update its test", "Run sh test.sh"]
workspace = "fixtures/release-greeting"
allow_shell = ["sh test.sh", "ls"]

[check]
file_contains = [["greet.sh", "Hello"]]
file_not_contains = [["greet.sh", "Hi,"]]
shell_succeeded = ["sh test.sh"]
no_invalid_tool_calls = true
```

`workspace` is relative to the eval directory and must resolve to a directory
inside it, including when symlinks are involved. The runner copies regular files
and directories to a fresh temp directory, enters agent mode even in incognito,
and deletes the copy and its runtime logs afterwards. Symlinks inside fixtures
are rejected. No session/workspace binding or durable checkpoints are saved.
Filesystem actions are allowed on the confined copy. `allow_shell` defaults to
empty: shell runs require a matching command-text prefix, and commands containing
`; & | $ \` > <` or newlines are denied (no chaining or redirection). Prefixes
are literal starts-with matches, not executable-name matching. Agent eval tasks
with `allow_shell` run only where shell writes can be confined: macOS with
`/usr/bin/sandbox-exec` today. Shell writes are limited to the disposable copy,
with exceptions for the canonical system temp directory (`TMPDIR`), shell logs,
and `/dev/null`, `/dev/tty`, and `/dev/fd`. Other platforms, missing sandbox
support, or an unbuildable profile refuse shell runs and fail the task with
`shell write confinement unavailable · this eval task needs macOS sandbox-exec`.
Only use trusted fixtures and narrow prefixes; shell reads and the permitted
temp directory are not isolated.

File checks require a workspace. Paths are confined to the temp copy, and file
substrings are case-sensitive. Missing/unreadable files fail both kinds of file
check. Each `shell_succeeded` prefix requires some `shell` run with a matching
command prefix whose result starts with `exit 0`; a failed run does not satisfy
it. It checks all turns, not just the last command.

The starter `agent-release-followup` task has four turns and a dependency-free
POSIX shell fixture; the final edit recalls a policy read in turn 1.
`chat-recall-six-turns` recalls two details stated only in turn 1 and needs no
fixture.

Answer substrings are case-insensitive. Every `answer_contains` item must appear;
no `answer_not_contains` item may appear. Tool names are exact and case-sensitive;
each required tool must appear at least once and forbidden tools must never
appear. Each `tool_args_contains` item must occur in some call's **raw argument
string**, case-sensitively, including whitespace as written. No regex is used.
`max_tool_calls` includes every recorded call. `no_invalid_tool_calls` rejects
results classified as `invalid_args` or `unknown_tool`, not other tool failures.

Output includes PASS/FAIL and failure reasons per task, tool-call and invalid-call
counts, prompt/completion tokens, cached prompt tokens, and USD cost, followed by totals. Invalid-call
rate is invalid calls divided by all calls (zero with no calls). `--json` emits a
single object with `experiments`, `tasks`, and `totals` (or both runs and deltas
with `--compare` or `--compare-model`). Missing token/cache usage is reported as zero; unavailable
cost is `unknown` in text and `null` in JSON. Total cost is unknown if any task's
cost is unavailable. Usage is the headless runner's sum over API requests.
Exit status is 0 when every selected task passes, 1 otherwise (including loading
errors or no matching tasks).

Keep held-out tasks separate: **held-out tasks must not be used for training** or
prompt tuning. Use them to measure generalization, not to teach their answers.
