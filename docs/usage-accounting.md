# Cache and cost accounting

**Status: implemented.** All five phases below have landed; this document is
kept as the rationale for why the code is shaped the way it is, and as the
contract any new backend has to satisfy.

The cache hit rate used to be unfalsifiable: every path that could have exposed
a bad number clamped it into a plausible range instead. The clamps are gone,
replaced by one normalization boundary and one invariant, and the rate is now
something you can move.

## The five defects (fixed)

| # | Defect | Where |
|---|---|---|
| 1 | Two different metrics both labelled "% cached" | `app/chat.rs:393` → `ui/mod.rs:338`, vs `ui/popups/usage.rs:287` |
| 2 | Prompt-token convention never normalized; mismatch hidden by `.clamp()` | `provider/mod.rs:303`, `openrouter.rs:2373` |
| 3 | Anthropic-family models never cache (no `cache_control` anywhere) | request builders, `openrouter.rs:1109`/`:1564` |
| 4 | `merge_usage` maxes fields independently, inventing ratios | `openrouter.rs:2420` |
| 5 | Three usage parsers with different field coverage | `openrouter.rs:2358`, `:2847`, `:1949` |

### Defect 2 in detail — the root cause

`Usage::cache_hit_rate` computes `cache_read / prompt_tokens`. That is correct
only when `prompt_tokens` **includes** the cached tokens:

| Family | Prompt field | Includes cache reads? |
|---|---|---|
| OpenAI, Codex Responses | `prompt_tokens` / `input_tokens` | yes |
| DeepSeek-compatible | `prompt_tokens` (= hit + miss) | yes |
| Anthropic-shaped | `input_tokens` | **no** — reads and writes are separate |

`parse_usage_value` accepted `cache_read_input_tokens` and
`cache_creation_input_tokens`, so Anthropic-shaped payloads reached a ratio
computed under the opposite convention. The result exceeded 1.0 and
`.clamp(0.0, 1.0)` pinned it to a convincing 100%.

The same mismatch corrupted cost. `catalog_request_cost` documents the
assumption — *"Prompt totals include cache reads/writes"* — and then defends it
with arithmetic that silently absorbs violations:

```rust
let reads  = cache_read_tokens.min(prompt_tokens);
let writes = cache_creation_tokens.min(prompt_tokens - reads);
let ordinary = prompt_tokens - reads - writes;
```

Under the exclusive convention `reads` is clamped down, `writes` collapses to
~0, and the remainder is billed at the full prompt rate. Cost comes out high,
cache rate comes out at 100%, and nothing anywhere reports a problem.

## Phase 0 — one normalization boundary

Normalize at the single point where provider JSON becomes a `Usage`, and make
every consumer downstream rely on one documented meaning.

1. Detect the convention from *which field family* supplied the numbers, rather
   than guessing from the values. Anthropic-shaped keys (`cache_read_input_tokens`,
   `cache_creation_input_tokens`) imply an exclusive prompt count; the
   `*_tokens_details.cached_tokens` groups imply an inclusive one.
2. Convert to the inclusive convention at parse time:
   `prompt_tokens = input + cache_read + cache_creation`.
3. Record what was detected. Add `prompt_convention: PromptConvention` to
   `Usage` (`Inclusive` / `NormalizedFromExclusive` / `Unknown`) so a payload
   that matches nothing is visible instead of coerced.
4. **Delete the clamps.** `cache_hit_rate` drops `.clamp(0.0, 1.0)`;
   `catalog_request_cost` drops the `.min()` juggling. Replace both with a
   `debug_assert!(cache_read + cache_creation <= prompt_tokens)` so a future
   provider shape fails loudly in tests instead of quietly in production.

The invariant to hold everywhere after this point:

> `prompt_tokens` is the **total** prompt, of which `cache_read_tokens` were
> served from cache and `cache_creation_tokens` were written to it. Both are
> subsets, never addends.

## Phase 1 — one parser, honest merges

- Fold `codex_usage` and the gateway's `response_usage_to_chat` into
  `parse_usage_value`. This also fixes two latent gaps in the Codex path: it
  uses `as_u64` instead of `json_u64` (a stringly-typed count silently becomes
  0) and never reads `cache_creation`.
- Rework `merge_usage`: instead of per-field `max`, replace the whole object
  when the incoming one is more complete, so the fields that end up in a ratio
  actually co-occurred in one response. Cost-only trailing frames keep merging
  as they do today.

## Phase 2 — say which number is which

The status line and `/usage` answer different questions. Both should keep
answering them, under different labels.

- Status line: switch from *last request in the turn* to the **turn aggregate**
  (`Σ cache_read / Σ prompt` over that turn's requests). That is what the label
  implies to a reader watching a tool loop run.
- Keep the per-request value, but move it to the context popup (Ctrl+G) where
  per-request detail belongs.
- `/usage`: keep the window aggregate, and surface `cache_creation_tokens`
  alongside reads — a high write count with a low read count is the signature
  of a cache that is being rebuilt every turn, which is the failure this whole
  plan exists to make visible.

## Phase 3 — make the number moveable

Measurement alone will show Anthropic-family models sitting at 0%, because the
app never asks for caching on them.

- Add `cache_breakpoint: bool` to `ChatMessage`. When set, `Serialize` forces
  the content-parts shape (the vision path at `provider/mod.rs:242` already
  builds one) and attaches `cache_control: {"type": "ephemeral"}` to the last
  part.
- Placement follows the epoch design already in `app/memory.rs:42`: one
  breakpoint at the end of the system block, one after the last message that is
  stable for the current `cache_epoch`. Those are exactly the boundaries the
  epoch was built to keep byte-identical.
- Gate by model family — only backends that require explicit markers get them;
  auto-caching providers are untouched.
- Send `prompt_cache_key` to OpenAI as well, not only OpenCode Go
  (`openrouter.rs:328`).
- Surface caching mode per backend in `/usage`: *auto*, *explicit*, or *none*,
  so 0% reads as "this model does not cache" instead of "something is broken".

## Phase 4 — proof

- A golden-payload test per provider family — OpenAI, Anthropic-via-OpenRouter,
  DeepSeek, Codex Responses, OpenCode Zen/Go — each asserting one normalized
  `Usage` and one expected rate. This is the regression net that keeps the
  convention from drifting again the next time a backend is added.
- A verification view (`/usage` detail row or `nexus doctor --cache`) listing
  the last N requests per model: prompt, cache read, cache write, rate, and the
  detected convention. The direct answer to "is this number correct".

## Migration: historical rows

`usage_log` rows written before this change were recorded under the ambiguous
convention and cannot be re-derived. Silently folding them into aggregates
keeps the old wrongness alive forever.

Add a nullable `prompt_convention` column: `NULL` marks pre-normalization rows.
`/usage` then either excludes them from cache-rate math or flags the window as
mixed. Token and cost totals stay usable; only the cache ratio is withheld,
which is the only figure the ambiguity actually corrupts.

## What a new backend must satisfy

Adding a provider means adding a case to
`every_provider_family_normalizes_to_one_prompt_convention`. If its usage shape
is not covered by the field families in `parse_usage_value`, the arithmetic
backstop catches it as `NormalizedFromExclusive`, and the `debug_assert` in
`Usage::cache_hit_rate` fails the test run rather than shipping a clamped rate.

## Known gap

`build_history` marks breakpoints for the interactive chat path. The research
and swarm pipelines assemble their own message lists and do not, so
Anthropic-family models still cache nothing there. Their sub-agent prompts vary
per question, so the payoff is smaller than for chat — but it is a real gap, not
a judgement that caching does not apply.
