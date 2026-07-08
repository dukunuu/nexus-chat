# Swarm personalities — design

Date: 2026-07-08. New feature: a session can run a "roundtable" turn where
several personas — each its own model + personality blurb — discuss the
user's message from different angles before a synthesis reply is produced.
Reuses the parallel-fan-out/staged-completion orchestration style already
established in `2026-07-06-deep-research-design.md` (Planner → Searcher×N →
Synthesizer → Critic → Verifier → Final Writer).

## Goal

- Pick a model per persona, write a short personality blurb per persona.
- Send a message; personas respond independently each round, see the whole
  discussion so far starting round 2, for several rounds.
- A moderator decides when the discussion has converged (bounded by a hard
  cap), then a synthesis stage writes the one reply that continues the chat.
- If you haven't set up a roster, the app suggests one from your first
  message once swarm mode is turned on.

## 1. Data model & persistence

- `sessions.swarm_mode INTEGER NOT NULL DEFAULT 0` — new column, identical
  pattern to the existing `web_mode` (`ALTER TABLE`, `Session.swarm_mode:
  bool`, `set_session_swarm_mode(session_id, bool)`, read back in
  `list_sessions`/session-load queries next to `web_mode`).
- New table `swarm_personas(session_id TEXT, ord INTEGER, name TEXT, model
  TEXT, persona TEXT)`. Access:
  - `list_swarm_personas(session_id) -> Vec<Persona>` (ordered by `ord`)
  - `save_swarm_personas(session_id, Vec<Persona>)` — replace-all (delete
    then insert), simplest correct semantics for "the roster is whatever's
    in the popup right now."
  - `clear_swarm_personas(session_id)`
  ```rust
  pub struct Persona { pub name: String, pub model: String, pub blurb: String }
  ```
- `Message` gains `pub persona: Option<String>` (new nullable
  `messages.persona` column, same `ALTER TABLE` + migration approach as
  other optional `Message` fields like `phrase`). `None` for ordinary
  messages and for the swarm turn's final synthesis message; `Some(name)`
  for a persona's round reply.

## 2. `/swarm` command + roster popup

- New command `/swarm` (aliases: `swarms`, `personas`, `panel`) opens
  `Popup::Swarm` for the active session.
- Popup layout: a cursor-navigable list of persona rows (name · model ·
  blurb), same interaction shape as `/apps`/`/files`:
  - add a row (blank name/blurb, model defaults to the session's
    `current_model`)
  - edit a row's name/blurb (small text-edit, reusing `FilterInput`-style
    editing already used elsewhere) or its model (opens the existing model
    picker via a new `ModelPickTarget::SwarmPersona(usize)` variant,
    confirming writes back into that row instead of `current_model`)
  - remove a row
  - a toggle line — "swarm mode: ON/OFF for this session" — flips
    `swarm_mode` immediately via `set_session_swarm_mode`
- Every add/edit/remove writes through to `swarm_personas` immediately (no
  separate "confirm" step), same immediacy as toggling a favorite in the
  model picker. Esc closes the popup; `swarm_mode` stays whatever it was
  last set to (closing the popup doesn't imply turning it off).

## 3. Turn execution pipeline (`src/app/swarm.rs`, new file)

Triggered from `send_message` when the active session's `swarm_mode` is
true, replacing the normal single-model completion for that turn:

```
if swarm_personas(session) is empty:
    Suggest (meta-model, single completion, no tools) → propose 3 personas
      (name + blurb; model defaults to current_model) from the user's
      message → save_swarm_personas

Round 1 [parallel, JoinSet — same fan-out pattern as research's
  Searcher×N]: every persona replies to the user's message independently
  (its own model, a system prompt carrying its blurb, no tool access —
  plain discussion, not a tool-using turn). Each reply is stored as
  Message { role: "assistant", persona: Some(name), model: Some(persona's
  model), .. } immediately as it lands (so a crash mid-round doesn't lose
  earlier replies).

Moderator (meta-model, single completion) reads the transcript so far
  (user message + all persona replies through the just-finished round) →
  "converged" | "continue".

  while "continue" and completed rounds < 4 (hard cap):
    Round k+1 [parallel]: every persona replies again, now with the full
      transcript through round k in context → stored the same way
    Moderator re-checks
  (cap reached ⇒ proceed to synthesis regardless of the moderator's answer)

Synthesis (meta-model, single completion) reads the entire discussion →
  one final markdown reply reconciling the perspectives → stored as
  Message { role: "assistant", persona: None, .. } — the turn's canonical
  reply; this is what session continuity, compaction, and title generation
  treat as "the" assistant message, same as any normal turn.
```

"Meta-model" (suggest / moderate / synthesize) = `research_model` if set,
else the session's `current_model`. No new global model setting.

## 4. Transcript display & progress feedback

- No live token streaming for persona replies — each is a plain
  `Provider::complete()` call (not `stream_chat`), appearing as a finished
  block once done. This sidesteps needing N concurrent live streams in a
  TUI built around a single `self.streaming` slot; the existing research
  pipeline's non-searcher stages work the same way.
- While a round runs, `self.status` shows progress, e.g. `"swarm round
  2/4 — 2 of 3 personas replied…"`.
- Rendering: a persona message gets one extra header line above its
  content — `**{persona name}** · {model id}` — otherwise reusing the
  existing message-block renderer as-is. The synthesis message renders
  exactly like any normal assistant reply (no header), since it's the
  canonical turn.

## Out of scope

- No mid-round intervention/steering (the existing `/research` steer
  mechanism is not reused here — swarm rounds run to completion once
  started).
- No persona reuse across sessions (rosters are per-session; copying a
  roster from one session to another isn't supported in this version).
- No tool access for persona replies (web search, file tools, etc.) — pure
  discussion. Could be added later by giving personas a restricted
  `ToolBox` the way research Searchers get one, if wanted.
