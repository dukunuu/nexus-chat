# Backend switching (OpenRouter / OpenAI / Codex) — design

Date: 2026-07-08. Triggered by a bug report ("codex models not in /model"):
Codex login was found to fully replace `self.provider`, silently discarding
an already-configured OpenRouter (or OpenAI) key, and `/key` does the same
in reverse. Fix: let all configured credentials persist simultaneously, and
add an explicit way to switch which one is active. This is **not** a merged
cross-backend model list — one backend is active at a time, same as today;
the only change is that switching no longer requires re-authenticating.

## Current state (the actual bugs)

- `config.rs::save_key` rewrites the whole `[provider]` TOML section from
  just the one key it was given — wipes the sibling key field *and* any
  saved Codex creds. `save_codex_credentials` is the only writer that
  preserves the other fields.
- `App.provider: Option<OpenRouter>` is a single slot in memory too. `/key`
  and `/login` both build a fresh `OpenRouter` and overwrite it — there's no
  way to get back to a previously-active backend without re-entering a key
  or re-running the OAuth device flow.
- `model_provider_filter` (the Ctrl+P catalog-vendor filter inside the model
  picker) isn't reset when the active provider changes, so a stale filter
  value can silently hide every model of the newly-active backend. (Already
  fixed in this branch — see `on_login_result` / `confirm_key` in
  `src/app/models.rs`.)

## Target behavior

- Configuring OpenRouter, OpenAI, or Codex never erases the others on disk.
  All three can sit configured at once.
- A new `/backend` command (aliases: `provider`, `providers`) cycles the
  active provider among whichever of the three are actually configured,
  skipping empty slots. Switching rebuilds `self.provider` from the stored
  credential (no re-auth), clears `self.models`, resets
  `model_provider_filter`, and re-fetches models for the newly-active
  backend — mirroring exactly what `/key`/`/login` already do today when
  they set a fresh provider.
- Everything downstream (model picker, favorites, last-used, current-model,
  chat/memory/transcribe/research/etc. request sending) is untouched: it
  already only ever deals with "the current `self.provider`" and one flat
  `Model.id`. No id-prefixing, no per-model backend tagging, no merged list.

## 1. Config/storage layer (`config.rs`)

- `save_key(key: &str)`: load the existing config first (openrouter_key,
  openai_key, codex creds via a shared internal loader), overwrite only the
  field matching the new key's shape (`sk-or-` prefix ⇒ openrouter_key, else
  ⇒ openai_key), pass the untouched sibling key and codex creds straight
  through to `write_provider_config`.
- New `load_all_providers() -> Result<SavedCreds>` (async — refreshes Codex's
  token if stale, reusing `refresh_codex_if_needed`) where:
  ```rust
  pub struct SavedCreds {
      pub openrouter_key: Option<String>,
      pub openai_key: Option<String>,
      pub codex: Option<CodexCredentials>,
  }
  ```
  Replaces `load_key()` as what `main.rs` calls at startup. `load_key()` is
  deleted once nothing needs "just give me the one active key" — `App::new`
  picks an initial active provider itself (see §3).
- `write_provider_config` is unchanged (already takes all three).

## 2. Runtime state (`app/mod.rs`)

- `App` gains `saved: config::SavedCreds` (populated at startup from
  `load_all_providers`, and updated in place whenever `/key` or `/login`
  succeeds, so a fresh credential is immediately switchable-to within the
  same run).
- `App.provider: Option<OpenRouter>` stays exactly as it is — still the one
  active backend. Nothing about how it's used downstream changes.

## 3. `/backend` command

- New command in `input.rs`: `name: "backend"`, `aliases: &["provider",
  "providers"]`.
- Handler cycles a fixed order — OpenRouter → OpenAI → Codex → (back to
  OpenRouter) — skipping any slot that's `None` in `self.saved`. If only one
  slot is populated, status message says so (`"only one backend
  configured — /key or /login to add another"`) and does nothing else.
- On an actual switch: rebuild `self.provider` from the corresponding saved
  credential (`OpenRouter::openrouter`, `OpenRouter::openai`, or
  `OpenRouter::openai_codex`), clear `self.models`, reset
  `model_provider_filter` to `None`, set status
  `"switched to {name} — loading models…"`, call `fetch_models()`.
- `App::new`'s initial active provider: whichever of the three
  `load_all_providers` returned is present, in priority order OpenRouter →
  OpenAI → Codex (matches existing default-picking order used for the
  utility/research/escalation/embedding model defaults).

## Out of scope

- No merged model list across backends, no per-model backend tagging, no id
  scheme changes. `Model.id` and all persisted favorites/last-used/
  current-model keys are untouched.
- Favorites/last-used are not scoped per-backend — a favorite saved while
  OpenRouter was active still shows in the favorites panel after switching
  to Codex, same ambient behavior as today (favorites has never been
  provider-aware). Not addressed here; out of scope for this fix.
- No UI element showing "which backends are configured" beyond the
  `/backend` status-line messages and whatever `/config` already surfaces.
