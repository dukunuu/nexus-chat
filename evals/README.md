# Golden tasks

Run these manually against a real backend before shipping prompt or tool changes:

```sh
nexus eval                              # ./evals, most recently used model
nexus eval ./evals --model MODEL
nexus eval ./evals --filter scripts --json
```

Keys and network access are required. **This costs real tokens and money.**
Evals run in incognito and never save sessions. They use your default space's
context and tools, not an isolated filesystem. They are not part of CI or
`scripts/check.sh`; the unit tests only parse tasks and exercise pure checkers.

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

Answer substrings are case-insensitive. Every `answer_contains` item must appear;
no `answer_not_contains` item may appear. Tool names are exact and case-sensitive;
each required tool must appear at least once and forbidden tools must never
appear. Each `tool_args_contains` item must occur in some call's **raw argument
string**, case-sensitively, including whitespace as written. No regex is used.
`max_tool_calls` includes every recorded call. `no_invalid_tool_calls` rejects
results classified as `invalid_args` or `unknown_tool`, not other tool failures.

Output includes PASS/FAIL and failure reasons per task, tool-call and invalid-call
counts, prompt/completion tokens, and USD cost, followed by totals. Invalid-call
rate is invalid calls divided by all calls (zero with no calls). `--json` emits a
single object with `tasks` and `totals`. Missing token usage is reported as zero;
unavailable cost is `unknown` in text and `null` in JSON. Total cost is unknown if
any task's cost is unavailable. Usage is what the headless runner reports; it
currently retains only the latest API request in a multi-request tool loop.
Exit status is 0 when every selected task passes, 1 otherwise (including loading
errors or no matching tasks).

Keep held-out tasks separate: **held-out tasks must not be used for training** or
prompt tuning. Use them to measure generalization, not to teach their answers.
