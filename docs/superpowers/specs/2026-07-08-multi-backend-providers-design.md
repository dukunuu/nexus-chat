# Multi-backend model providers — design

Date: 2026-07-08. Triggered by a bug report ("codex models not in /model"):
codex login was found to fully replace `self.provider`, so it (and any other
single auth method) always evicts whatever was configured before it. Fix
requires letting OpenRouter key, OpenAI key, and Codex login coexist, each
contributing models to one merged picker, each request routed to whichever
backend actually owns the picked model.

## Current state (why this needs more than a one-line fix)

- `App.provider: Option<OpenRouter>` is a single slot. `/key` and `/login`
  both build a fresh `OpenRouter` and stomp this field.
- `config.rs::save_key` rewrites the whole `[provider]` TOML section from
  just the one key it was given, discarding the other key field *and* any
  saved Codex creds. `save_codex_credentials` is the only writer that
  preserves the other fields.
- `OpenRouter::list_models` for the Codex flavor already reaches into the
  OpenRouter key file and appends OpenRouter's catalog (one-way, Codex only).
  No other flavor merges anything.
- Favorites/last-used/current-model/config-model-settings are keyed by the
  bare model id (`Model.id`), which is also literally the string sent to the
  provider's API. There is no concept of "which backend does this id belong
  to" beyond the currently-active single `self.provider`.

## Target behavior

All three credentials (OpenRouter key, OpenAI key, Codex login) can be
configured at once. The model picker shows the union of whatever each
configured backend reports. Every place a model gets used for a request
(session chat, memory, transcriber, OCR, research, escalation, embedding)
resolves the model id back to the specific backend that owns it and sends
the request there — not to a single "active" provider.

## 1. Config/storage layer (`config.rs`)

- `save_key(key: &str)`: load the existing config first (openrouter_key,
  openai_key, codex creds), overwrite only the field matching the new key's
  shape (`sk-or-` prefix ⇒ openrouter_key, else ⇒ openai_key), and pass the
  untouched codex creds straight through to `write_provider_config`. No
  longer zeroes the sibling key or drops codex creds.
- New `load_all_providers() -> Result<(Option<String>, Option<String>,
  Option<CodexCredentials>)>` (async, since it may refresh Codex's token):
  reads the config once, returns openrouter key / openai key / refreshed
  codex creds independently. Replaces `load_key()` as the thing `main.rs`
  calls at startup. `load_key()` itself is deleted — nothing else should
  need "just give me one key" once callers hold a full `Backends`.
- `write_provider_config` is unchanged (already takes all three).

## 2. Runtime backend registry (`app/mod.rs`)

```rust
pub struct Backends {
    pub openrouter: Option<OpenRouter>,
    pub openai: Option<OpenRouter>,
    pub codex: Option<OpenRouter>,
}

impl Backends {
    pub fn any(&self) -> bool { ... }               // replaces provider.is_some() checks
    pub fn get(&self, tag: BackendTag) -> Option<&OpenRouter> { ... }
    pub fn resolve(&self, composite_id: &str) -> Option<(&OpenRouter, &str)> { ... }
}
```

`App.provider: Option<OpenRouter>` is replaced by `App.backends: Backends`.
`/key` populates `backends.openrouter` or `backends.openai` (by key shape);
`/login` populates `backends.codex`. Neither touches the other two slots.

## 3. Model tagging + id scheme

```rust
pub enum BackendTag { OpenRouter, OpenAi, Codex }

pub struct Model {
    pub id: String,           // unchanged: raw id, exactly what the API expects
    pub backend: BackendTag,  // new
    // ...existing fields unchanged
}
```

Composite id (used for favorites, last-used, current-model, and the
utility/research/escalation/embedding model settings — anywhere a model
choice is persisted or compared):

- OpenRouter models: bare id, unprefixed (`"anthropic/claude-sonnet-4.5"`) —
  preserves every existing user's saved favorites/current model with zero
  migration.
- OpenAI-direct models: `"openai:<id>"` (e.g. `"openai:gpt-4.1"`).
- Codex models: `"codex:<id>"` (e.g. `"codex:gpt-5.5"`).

Two helpers do the round-trip:
- `composite_id(&Model) -> String` — builds the storage key from a fetched
  `Model` per the rule above.
- `Backends::resolve(&self, composite_id: &str) -> Option<(&OpenRouter, &str)>`
  — strips a recognized `openai:`/`codex:` prefix (bare ⇒ OpenRouter),
  returns the matching backend plus the raw id to send in the request.

## 4. Fetching & call-site refactor

- `App::fetch_models` fetches concurrently from every populated backend
  slot, tags each result with its `BackendTag`, concatenates into
  `self.models`. The one-way Codex→OpenRouter merge inside
  `OpenRouter::list_models` is removed — each flavor's `list_models` goes
  back to reporting only its own catalog (Codex keeps its 4 hardcoded
  entries); merging across backends happens once, at the `App` level.
- `on_login_result` (Codex) and `confirm_key` (`/key`) no longer clear
  `self.models` wholesale. Each refreshes only its own backend's slice:
  drop existing entries with that `BackendTag`, refetch that backend, splice
  the fresh entries back into `self.models`. Logging into Codex no longer
  blanks out already-loaded OpenRouter models, and vice versa.
- Every call site that today does `self.provider.clone()` plus a bare model
  id (`chat.rs`, `memory.rs`, `transcribe.rs`, `files.rs`, `compaction.rs`,
  `research.rs`, the embedder wiring in `mod.rs`) changes to
  `self.backends.resolve(&model_id)`, clones the returned provider, sends
  the request with the returned raw id. Same call shape, resolved per-model
  instead of assumed-global.
- `model_provider_filter` is reset to `None` whenever any backend's slot is
  (re)populated (covers the original bug report: a stale filter surviving a
  credential change and silently hiding the new backend's models).

## 5. Defaults & picker cosmetics

- Startup defaults for utility/research/escalation/embedding models (before
  anything's been explicitly picked): use the first configured backend in
  priority order **openrouter → openai → codex**, call its existing
  `default_*_model()`, composite-prefix per the scheme in §3.
- The model-picker's Ctrl+P provider filter (groups by catalog vendor like
  `google`/`anthropic`, derived from the `/` in OpenRouter ids) is unchanged
  — it's a finer-grained, orthogonal split. Known cosmetic overlap: OpenAI-
  direct and Codex models both fall into the same `"openai"` filter bucket
  (neither id has a `/`), same as before this change. Not addressed here.
- Favorites/last-used continue to key off the composite id, so a favorited
  Codex model and a same-named OpenAI-direct model stay distinct entries
  even though their raw ids could collide.

## Out of scope

- No UI indicator of which backend a model belongs to beyond the existing
  vision glyph / reasoning badge — the composite id prefix is invisible in
  the picker (only used internally for storage/routing), per the earlier
  decision to keep the picker's displayed text unprefixed.
- No changes to the actual OpenRouter/OpenAI/Codex HTTP request formats.
