# Exporting traces and tuning a local model

Nexus records data and evaluates models. It does **not** train models, download
training packages, or automatically promote adapters.

## Record with consent

In a space, run `/training on` before chatting. Recording is off by default,
per space, and always disabled in incognito. `/training off` stops recording.
Calls go to the device-local `traces/` directory beside `nexus.db`: requests,
responses, tool definitions, full and model-visible tool outputs, and labels.
`/retry` labels the replaced turn rejected and records its replacement normally.

Traces and exports can contain private workspace content. Recording scrubs
credentials; export scrubs again using current credentials and patterns. Review
the output before moving it off the device. Redaction is not a privacy guarantee.

## Export offline

```sh
nexus train export --format chat-jsonl --filter success \
  --models 'local:*,distillable' --since 2026-10-01 \
  --space default --evals ./evals --out ./data --split
```

`--format` and `--out` are required. Formats are `chat-jsonl`, `sharegpt`, and
`dpo`. Without `--space`, all spaces are considered; `--since` is an inclusive
UTC date and selects complete turns. Model patterns are comma-separated globs
(`*` and `?`), not filesystem paths. Quote them in the shell.

The exporter writes `<format>.jsonl` and `report.json`, and prints the same
report to stdout. Counts include steps read, turns kept, mutually exclusive
first-failure drop reasons, and scrub-flagged kept turns. A scrub-flagged turn
is **redacted and kept**, not dropped. DPO also counts turns without a usable
retry pair. Export overwrites these output files.

The default success filter requires a final `stop`, no rejected label, and no
denied, blocked, refused, or confinement tool result anywhere in the turn.
Recoverable tool errors are allowed. `--filter all` relaxes the outcome filter,
not license, schema, eval, or redaction checks. Rejected turns are never chosen;
DPO replacements always require success, even with `--filter all`.

Each conversation is the final call's request plus its response, including the
system prompt and that call's own tool definitions. Prior tool calls and results
are already interleaved in the request; full tool outputs are never substituted
for the shortened text the model saw. Retired tool aliases (including batch
children) are rewritten to current names and selectors. Unknown tool names in
any call or recorded definition drop the whole turn.

### Licenses: your responsibility

**License checking is the user's responsibility**, including local model
licenses and provider terms. The defaults are a conservative selection policy,
not legal advice or proof of permission:

- `local:*` selects local-runtime calls, subject to the model's actual license.
- `distillable` requires an explicitly true OpenRouter catalog
  `is_trainable_text` flag. Missing or false metadata never grants permission.
  The current catalog is only cached in the running app's memory, without this
  flag; the offline CLI therefore reports
  `no distillable metadata in the model catalog · pass explicit model patterns`.
- OpenAI, Codex and OpenCode are excluded by default. An explicit pattern, such
  as `--models 'openrouter:qwen/*'` or `--models 'openai:gpt-*'`, overrides that
  exclusion. Supply one only after verifying that you may train on the outputs.

**Every step** in a turn must pass, not just its final model. For DPO, both the
replacement and rejected trajectory must pass license/schema/eval safety.
Retry labels do not link turn IDs: DPO pairs the next same-session turn only
when its request prefix through the user message matches the rejected turn.
Unmatched turns produce no preference pair.

### Keep evals held out

`--evals` defaults to `./evals`. The directory must exist; use an explicitly
empty directory only if you really have no held-out tasks. Missing directories
and invalid tasks fail the export rather than bypassing decontamination.

Every task prompt, including every prompt in multi-turn tasks, is normalized by
lowercasing and collapsing whitespace. A turn is dropped if any of its user
messages contains an eval prompt or shares at least half of its unique word
8-grams with one. This is a lexical safeguard, not a semantic contamination
detector; keep evaluation workloads separate from recorded training sessions.

## Formats and split

- **chat-jsonl:** `{"messages":[...],"tools":[...]}` in OpenAI chat shape.
  Tool definitions use `type: function`; assistant calls carry
  `function.name`/`arguments`; tool messages retain `tool_call_id`.
- **sharegpt:** `{"conversations":[{"from":"human","value":"..."},...]}`,
  with `system`, `human`, `gpt`, and `tool` roles. GPT messages containing tool
  calls are serialized as JSON in `value`.
- **dpo:** `{"prompt":[...],"chosen":"...","rejected":"..."}` from `/retry`.
  The prompt ends at the user message; chosen/rejected are final assistant
  content, not tool trajectories. Feed this to an external DPO trainer.

With `--split`, chat-jsonl also writes `train.jsonl` and `valid.jsonl` for MLX.
The chronological split reserves `floor(N / 10)` conversations for validation
and the rest for training; fewer than ten conversations leave validation empty.
The unsplit `chat-jsonl.jsonl` is still written. Split is not supported for the
other formats. Inspect for overlapping conversation history across the split;
this mechanical split is not an independent benchmark.

## Train outside Nexus

Install `mlx-lm` in a separate environment on a supported Apple Silicon machine.
Use a base model whose tokenizer/chat template supports the exported tool-call
shape. Check that package's version-specific data/template support before a
long run. With at least ten kept turns and the split above:

```sh
mlx_lm.lora --model mlx-community/YOUR-BASE-MODEL \
  --train --data ./data --adapter-path ./adapters/notes
```

`--data` points to the directory containing `train.jsonl` and `valid.jsonl`,
not the unsplit file. Training parameters, GPU memory budgets and evaluation
frequency belong to the external trainer, not Nexus. For Unsloth or axolotl,
configure its chat/tools template and dataset loader against `chat-jsonl.jsonl`.

Optionally run the same external command from an opt-in phase-13 agent job:
use a disposable training workspace, set the job's working directory there,
and explicitly approve the shell command under your normal agent rules. Nexus
does not provision the training environment or schedule Loop-4 automation.

## Serve and compare

Serve the adapter with a version of `mlx-serve` supporting your base and LoRA
adapter, or fuse it externally into a separate model directory first. For an
Ollama-compatible adapter, import it using a Modelfile (architecture and adapter
format must be supported by your Ollama version):

```text
FROM your-base-model
ADAPTER /absolute/path/to/compatible/adapter
```

Create a distinct model name with `ollama create notes-tuned -f Modelfile`,
start the chosen runtime, and select `/local mlx-serve` or `/local ollama` in
Nexus. Refresh `/model`; the server's catalog must advertise a distinct tuned
model ID so the base and tuned model can both be selected. Follow the serving
runtime's adapter/fusion instructions; an MLX adapter is not automatically
portable to Ollama.

Run the same held-out tasks against both advertised local model IDs:

```sh
nexus eval ./evals --model local:BASE-ID --json > base-eval.json
nexus eval ./evals --model local:TUNED-ID --json > tuned-eval.json
```

Compare task pass rates, invalid calls, token use and cost before choosing the
adapter. Neither export nor evaluation automatically trains or promotes it.
