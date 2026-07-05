# Deep Research — Design

Date: 2026-07-06
Status: approved

## Problem

The chat model can `web_search` for snippets and reason about them in one pass, but has
no way to do broad, multi-angle, self-correcting research: decompose a topic, chase
leads across several sources in parallel, notice contradictions, verify its own claims
against sources, and produce a cited report — all without the user babysitting a single
long conversation.

## Goal

A `/research <topic>` command that runs a bounded multi-agent pipeline (planner →
parallel searchers → synthesis → critic → optional gap-filling round → optional
escalation → verifier → final writer) in the background, and delivers a cited markdown
report as a chat message plus a saved file in the space, using the existing
background-session/notify infrastructure.

## Design

### Pipeline

```
Planner (research_model, completion, no tools)
  → 3-6 sub-questions
  → [parallel] Searcher × N (research_model, stream_chat, web_search + fetch_url tools,
     bounded tool-loop) → findings + sources, per sub-question
  → Synthesizer (research_model, completion) → draft report

Critic (research_model, completion) → "satisfied" | list of gaps/contradictions

  if gaps found (round 1 only):
    → [parallel] targeted Searcher × M, one per flagged gap
    → Synthesizer (re-run over all findings, old + new) → revised draft
    → Critic (re-run) → "satisfied" | remaining issues
    [loop bound: 2 rounds total — after round 2, proceed regardless]

  if Critic still flags a genuine contradiction (not a coverage gap):
    → Escalation (escalation_model, completion, no tools) resolves it directly
      from the gathered source excerpts already in context

Verifier (research_model, completion) → checks each claim in the draft against
  gathered source excerpts; drops or flags (⚠) unsupported claims

Final Writer (research_model, completion) → markdown report, inline [n] citations,
  trailing "## Sources" list (n → url)
```

Every stage is a single request/response (`Provider::complete`, already exists) except
the Searcher stage, which reuses the existing `stream_chat` tool-loop machinery
(`run_chat_loop`) so each searcher gets its own bounded multi-round tool use — just with
a system prompt fixing its one sub-question and a `ToolBox` restricted to
`web_search`/`fetch_url`. Searcher tool budget is a separate, smaller constant
(`RESEARCH_SEARCHER_MAX_ITERS`, e.g. 4) from the interactive-chat `MAX_TOOL_ITERS` —
research searchers don't need a whole conversation's budget, just a few search→fetch
hops.

No new orchestration primitive is needed beyond `tokio::spawn` + `JoinSet` for the
parallel searcher fan-out (same pattern already used for concurrent OCR page
transcription in `extract.rs`).

### New tool: `fetch_url`

`web_search` only returns snippets. Recursive research (read a source, find a new term
inside it, search again) needs actual page bodies. New `ToolBox` tool:

- `fetch_url(url, offset?, limit?)` — GETs the URL, strips HTML via the existing
  `strip_tags` helper (`tools.rs`), returns plain text, ranged/capped like `read_file`
  (~200 lines/call). Available to the main chat model too (not research-only) — it's a
  generally useful capability and costs nothing extra to expose.

### Models & settings

Two new `Settings`-adjacent fields (same pattern as `ocr_model`/`embedding_model`: a
plain string, empty allowed, dedicated `ModelPickTarget` variant, persisted via
`db.load_settings`/`save_setting`):

- `research_model` — used for every pipeline stage except escalation. Default
  `google/gemini-2.5-flash`.
- `escalation_model` — used only for the contradiction-resolution stage. Default a
  fixed frontier model, `anthropic/claude-sonnet-4.5` (not "whatever the active
  session's model is" — independent of chat state, since research can be kicked off
  from any session).

`/research` is disabled (with a clear status message) if `research_model` is empty,
mirroring how OCR/embedding features gate on their model settings.

### Session & UI integration

- `/research <topic>` creates a new session (title derived from the topic, same as
  today's auto-title flow) and immediately tags it as the origin of a background job,
  reusing `stream_session`/`unread`/notify-on-completion exactly as background chat
  streams do today — the user can switch away and gets `✓ response ready in: {title}`
  when it lands.
- Progress is visible the same way OCR's `Stage`/`Progress` updates are: a
  `ResearchUpdate` enum (`Stage(String)` for named phases — "planning…", "searching (3/5
  sub-questions)…", "critiquing…", "verifying…", "writing…" — sent over a new
  `AppEvent::Research` channel) updates `self.status` when viewing the session, and is
  persisted as a `research_stage` message role for transcript display (rendered like
  `tool_call` rows: collapsed one-liners, expandable). `research_stage` rows are
  **never** replayed into `build_history` (unlike the `tool_call` replay fix landed
  today) — they're the background job's own scratch work, not something the top-level
  chat model did; only the final report belongs in the conversation the model sees on
  follow-up turns.
- Final report: posted as a normal assistant message in the research session (so
  follow-up questions in that session have it in context), and saved as
  `spaces/<name>/files/research-<slug>-<date>.md` via the same import path `/files` uses,
  so it's picked up by the existing rescan and immediately searchable via
  `search_files`/`read_file`.

### Error handling & bounds

- Any single stage failing (network error, empty completion) aborts the pipeline with a
  clear `research_stage` failure row and a final assistant message explaining what
  failed and how far it got — never silently drops the job.
- Hard caps prevent runaway cost/time: 6 sub-questions max, 2 outer rounds max,
  `RESEARCH_SEARCHER_MAX_ITERS` per searcher, exactly one escalation call, one verify
  pass, one write pass. Worst case is bounded and enumerable, not an open-ended loop.
- `fetch_url` failures (404, timeout, non-HTML) return an error string to the calling
  searcher agent (same shape as existing tool error returns) rather than aborting the
  whole pipeline — a dead link shouldn't kill the research job.

### Testing

- Unit tests: `fetch_url` HTML→text stripping and pagination (same shape as existing
  `read_file` tests); `ResearchUpdate` stage/progress persistence and status-bar
  wiring (mirrors the OCR status tests landed today); pipeline stage functions tested
  individually with a fake/mock provider returning canned completions, verifying
  prompt construction, sub-question parsing, and the round-2/escalation branch logic
  without live network calls.
- Manual: run `/research` on a real topic, confirm parallel searcher fan-out in logs,
  confirm the report lands as a chat message + a file in `/files`, confirm background
  notification fires when switched away.

### Explicitly out of scope (v1)

- True multi-provider model diversity per role (e.g. different providers per stage) —
  both `research_model` and `escalation_model` go through the existing single
  `OpenRouter` provider.
- O-Researcher-style distillation (recording swarm traces to fine-tune a cheaper
  imitation model) — noted in the user's research as "advanced, save for later."
- Cancelling a research job mid-flight (Esc during a research session behaves like any
  other background stream: it stops the stream but there's no partial-report save path
  beyond what's already written by the time of cancellation).
