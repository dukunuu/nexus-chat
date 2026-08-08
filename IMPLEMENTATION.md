# Research Flow UX — Implementation Summary

> Implements [`PLAN.md`](./PLAN.md): the research interaction is now fully
> **conversational** — a dedicated survey section where the AI asks what the
> user wants, the user answers in chat, and the plan (questions with
> Why/Angles/Sources briefs) is approved or edited in plain language. No
> keybindings, no file round-trip, no popup editor.

---

## 1. The flow (as built)

```
/research <topic>
  │
  ├─ SURVEY SECTION ──────────────►  scoping agent's clarifying questions,
  │                                  rendered as a `survey` transcript row (❓).
  │      ▲ user answers in chat (≤3 rounds; "I approve"/"go"/… recognized
  │      │ by the agent, not phrase-matched in app code)
  │      │
  │      └─ Enter (or agent declares COMPLETE) ──►  web landscape survey +
  │                                   local chunks gathered CONCURRENTLY
  │                                   while the user answered
  │
  ├─ PLAN ────────────────────────►  planner emits questions with briefs
  │                                  (question/why/angles/sources) — shown as
  │                                  a plan section AND written to
  │                                  plan-<slug>-<ts>.md in the space files.
  │
  ├─ APPROVAL ────────────────────►  reply "approve" (or Enter on empty) →
  │                                  searchers run. Edits ("drop Q2", "also
  │                                  look into X") are folded in by the
  │                                  approval agent and re-presented once
  │                                  (1 rework round, capped). Approval is
  │                                  fail-closed: a second edit, a failed
  │                                  approval call, malformed agent output,
  │                                  or a closed reply channel stops research
  │                                  with a visible error —
  │                                  searchers never run on an unapproved
  │                                  plan.
  │
  ├─ searchers → synthesis → critic → verifier → writer
  │
  └─ report ──────────────────────►  assistant message + research-<slug>.md
```

`/research! <topic>` (and watch re-runs) skip both gates and run straight
through, exactly as before.

---

## 2. New pipeline stages (`src/app/research.rs`)

### `ResearchUpdate` — new/changed variants

```rust
pub(crate) enum ResearchUpdate {
    Stage { label: String, detail: String },
    SurveyReady { questions: Vec<String>, round: u8 },   // NEW
    PlanReady { questions: Vec<PlanQuestion>, rework: bool },  // CHANGED
    Done(std::result::Result<String, String>),
}
```

### New pure, unit-tested functions

| Function | Purpose |
|---|---|
| `parse_survey_reply(text)` | `COMPLETE` marker (case-insensitive, first word, tolerates trailing punctuation/prose) or numbered questions; only list-marked or `?`-ending lines count as questions — empty output or explanatory/error prose is `Malformed` (a contract violation), which fails the survey visibly instead of silently dropping it. Capped at 4 questions/round. |
| `parse_plan_blocks(text)` | JSON array of `{question, why, angles, sources}` (`#[serde(default)]` on all fields), fences stripped; line-by-line fallback to bare questions. Capped at 6. |
| `parse_approval(text)` | `APPROVED` → run as-is; JSON plan array → revision; line-formatted revisions only when they look like a list (bullets/`N.`); anything else → `Malformed`, which fails the phase visibly (never silently approves). |
| `plan_text(&[PlanQuestion])` | Numbered questions with indented `Why:` / `Angles:` / `Sources:` lines — the shared transcript/plan-file format. |
| `PlanQuestion::prompt(topic)` | The full block handed to one Searcher as its prompt — detail is functional. |
| `survey_messages(topic, rounds)` | One prompt for initial questions (empty rounds) and follow-ups (rounds so far); agent replies `COMPLETE` when done. |
| `plan_approval_messages(topic, questions, reply)` | Folds the user's reply into the plan for the approval agent. |

### New prompts

- `SURVEY_AGENT_PROMPT` — ask 1–4 clarifying questions that would change the
  plan (scope/depth/angles/constraints); reply `COMPLETE` when enough.
- `PLAN_APPROVAL_PROMPT` — recognize approvals ("approve", "looks good",
  "go", …) → `APPROVED`; otherwise fold feedback into a revised JSON plan.
- `PLANNER_PROMPT` — now asks for objects with `question`/`why`/`angles`/
  `sources` instead of a bare string array.

### Orchestration (`run_research_inner`)

```
spawn gather task  ──►  local_known_chunks ∥ web landscape survey, and both
        │                        ∥ the conversational survey (the gather task
        │                        is itself abort-on-drop guarded, so Ctrl+X
        │                        cancels its web calls and DB writes too)
        ▼
run_user_survey  ──►  park on reply_rx per round (≤3), collect (questions, answer);
        │                  request failures, malformed agent output, and a closed
        │                  reply channel propagate as `Err` (never silently skipped)
        ▼
await gather ──►  planning context = known chunks + web survey + user answers;
        │                  a panic/cancellation in the gather task terminates the job
        ▼
plan(topic, answers, known) ──►  PlanQuestion blocks
        ▼
await_plan_approval ──►  park on reply_rx; empty reply or APPROVED → run;
                         revised → re-present once (`rework`), then run only
                         on explicit approval; agent failure / malformed
                         verdict / second edit / closed reply channel →
                         visible `Err`, plan not run (never fails open)
        ▼
searchers (full block prompts, short display labels, "round 1") → steers
        (one stage row per steer) → critic → round 2 → …
```

Both gates park on the **same** `mpsc::UnboundedReceiver<String>` — a single
chat-reply channel reused across phases, dropped when the job ends.

---

## 3. App-side plumbing (`src/app/mod.rs`, `src/app/research.rs`)

### Gate state

```rust
pub(crate) struct SurveyGate {
    pub session_id: String,
    pub reply_tx: mpsc::UnboundedSender<String>,
    pub phase: SurveyPhase,
    pub prompt_role: String,
    pub prompt_content: String,
}
pub(crate) enum SurveyPhase {
    Clarify { round: u8 },
    Approve { rework: bool },
}
```

Generic, mode-agnostic state (per PLAN.md §2): `SurveyGate`/`SurveyPhase` are
just "a parked conversation awaiting a chat reply" — the research survey and
plan-approval phases ride them, and swarm/watch/plain-chat can arm the same
gate without touching research-specific types.

Replaces the old `research_plan_gate: Option<(String, oneshot::Sender, Vec)>`.

- `survey_reply_tx: Option<mpsc::UnboundedSender<String>>` — created at job
  start (gated jobs only), cleared on stop/job end.
- `survey_gate: Option<SurveyGate>` — **armed only while a
  SurveyReady/PlanReady is actually pending**, so a gate in another session
  can never swallow typing (fixes the old cross-session hijack).
- `research_steer_log: Vec<(usize, String)>` — every `/steer` queued, as
  `(1-based queue position, text)`; acknowledged entries are dropped on the
  next queue and the log clears when the job stops/ends, so retained steer
  text stays bounded per job.
- `research_steer_acked: HashSet<usize>` — positions the pipeline has
  drained (parsed from `steer #N` stage updates); the retained log is
  pruned immediately on acknowledgment and hard-bounded (64 queued — beyond
  that `/steer` is refused), so retained steer text can't grow unbounded.
- `research_stage_rows: Vec<String>` — the job's stage rows, kept in sync by
  `mirror_stage` regardless of which session is viewed; the live popup
  renders from here instead of re-reading the db per frame.
- `research_incognito: bool` — incognito captured when the job started:
  survey rows, gate replies, plan messages, and artifact persistence all
  follow this captured mode, never a mid-job toggle.

### Key methods

| Method | Behavior |
|---|---|
| `survey_gate_targets_current_session()` | True only when a gate is armed **and** the viewed session is the gated one — the single scoped intercept. |
| `reply_to_survey_gate(text)` | In a normal job, **persists the `gate_reply` first** (a db failure re-arms the gate and restores the composer); incognito uses the job-start mode and never writes it. It then sends through the channel. If delivery fails, rollback errors are surfaced explicitly and the composer is restored. The reply renders like a user message but is **never replayed to the model**. |
| `restore_survey_gate_prompt()` | Restores the pending actionable row after its session is loaded. Normal jobs already have the durable DB row; incognito jobs recover `prompt_role`/`prompt_content` from `SurveyGate`, so an off-screen gate is visible without persisting private content. |
| `save_space_artifact(...)` | Shared artifact writer (`{prefix}-<slug>-<ts>.md`): creates the space files dir, writes, rescans when the space is active, returns the path — and **never writes in incognito** (the mode captured at job start; the plan folds in survey replies, so "nothing persists" must not leave it on disk). Failures surface via `mirror_stage` as a transcript/popup stage row. |
| `mirror_stage(...)` | Upserts a stage row (db + in-memory transcript when the job's session is viewed + the job-level `research_stage_rows` mirror) — error rows from failed artifact saves appear immediately, not only after a reload. |

### `on_research_done` — new handlers

- `SurveyReady` → composes the section (`For "<topic>"` / `Follow-up
  (round N of 3) for "<topic>"` + numbered questions + guidance footer).
  Normal jobs persist the `survey` row **before** arming; any write failure
  stops visibly. Incognito stores the row only in `SurveyGate`. Off-screen
  sessions are marked unread, and opening one restores the pending row.
- `PlanReady` → follows the same persist-before-arm rule and composes
  `Research plan — reply to approve, or tell me what to change...` for the
  initial plan. The single revised presentation asks only for approval,
  matching the one-rework cap. Incognito keeps the prompt in memory and
  skips both the DB row and `save_space_artifact`; normal artifact failures
  surface through `mirror_stage`.
- `Done` (both arms) / channel close → clears the gate; channel close also
  closes the live popup rather than leaving an empty "waiting" view. Job
  start clears any lingering gate/`reply_tx` for hygiene.

---

## 4. UI

### Survey section (`src/ui/history.rs`)

`push_survey_section` — same family as `push_research_plan`:

```
❓ For "fine-tuning LLMs":
   1. Depth vs breadth — one technique, or the field?
   2. Current state only, or history too?
   3. Any constraints (cost, compute, deployment)?

   Answer in chat — I may ask follow-ups (up to 3 rounds), then say "I approve". (Enter on an empty input skips ahead.)
```

Accent ❓ marker + bold header, questions plain, the guidance footer dimmed.
Always visible (simplest first — no collapse toggle).

### Input handling (`src/events.rs`)

- `Enter` while `survey_gate_targets_current_session()` → routes to
  `reply_to_survey_gate` instead of a chat completion. Empty input =
  "approve" (plan) / "skip ahead" (survey). Everywhere else Enter is a
  normal send.
- Removed: `e`-opens-editor, Esc-stops-gated-research, `$EDITOR` plan
  round-trip (`PendingEditor::ResearchPlan`, `edit_research_plan`,
  `apply_research_plan_editor`, `submit_research_plan_edit`,
  `parse_plan_edit`). Stopping a parked job: Ctrl+↑ → Ctrl+X in the live
  view.

### Live popup (`src/ui/popups/research_live.rs`)

- **Queued steers section**: steers not yet drained at a round boundary are
  listed (`● <text>`) above the agent rows. The pipeline emits **one stage
  row per drained steer**, keyed by a job-global sequence number (`steer #N`
  — the steer's 1-based queue position), and `on_research_done` records the
  position in `research_steer_acked` (pruning the retained log immediately)
  so every picked-up steer stays visible and is removed from the queued
  list — earlier steers no longer look pending after a later one is picked
  up, steer text (duplicates, prefixes of each other, `%`/`_` LIKE
  wildcards) can never collapse rows, and the view renders from the
  job-level `research_stage_rows` mirror — no db read per frame, and it
  works from any session.
- Multi-line stage details (e.g. critic gaps) render one indented row per
  line instead of one clipped line.

### Quick wins (from the plan)

- Human stage labels: `round 1` / `round 2` (was `r1`/`r2`).
- Critic gaps shown as questions: `done — found N coverage gaps:` followed by
  the numbered gap questions (they're what round-2 searchers investigate).
- Steer queue visible in the live popup (above).

### DB (`src/db.rs`)

- `add_survey_message` — `survey` role, never replayed to the model
  (`build_history` skips it alongside `research_stage`/`research_plan`).
- `add_gate_reply_message` — `gate_reply` role: renders in the transcript
  like a user message, also never replayed to the model.

---

## 5. What was removed (per PLAN.md §5)

- Plan-gate keybindings (`e` opens editor; Esc stopped research via global
  intercept) — approval is a chat reply now.
- Temp-file `$EDITOR` plan-editing machinery.
- The composer full-rewrite submit path (`approve_research_plan`,
  `submit_research_plan_edit`).
- `oneshot` plan gate — replaced by the shared mpsc reply channel.

---

## 6. Files touched

| File | Change |
|---|---|
| `src/app/research.rs` | Survey + approval phases, durable/incognito gate handling, `PlanQuestion`, parsers, concurrent gather, shared artifact writer, tests |
| `src/app/mod.rs` | `SurveyGate`/`SurveyPhase` (including pending prompt state), steer log/ack fields, `PendingEditor::ResearchPlan` removed |
| `src/app/sessions.rs`, `src/app/watches.rs` | Restore an in-memory pending gate prompt after loading its session |
| `src/events.rs` | Scoped Enter intercept, editor branch + Esc gate branch removed |
| `src/app/chat.rs` | Shared model-history exclusion plus request-time stale reasoning-preference cleanup |
| `src/app/models.rs`, `src/provider/mod.rs`, `src/provider/openrouter.rs` | Catalog-driven per-model reasoning efforts, including sparse/minimal/xhigh/max/explicit-off sets |
| `src/app/compaction.rs` | Compaction digest tail applies the same role exclusions as `build_history` |
| `src/db.rs` | `add_survey_message`, `add_gate_reply_message`, rollback delete, and round-trip tests |
| `src/ui/history.rs` | `push_survey_section` + cache branch + render test |
| `src/ui/popups/model.rs` | Exact accepted-effort hint and explicit-off badge |
| `src/ui/popups/research_live.rs` | Queued-steers view, multiline details |
| `src/ui/popups/tests.rs` | Steer-queue popup test |

## 7. Test coverage

414 tests pass (`cargo test --bin nexus-chat`). New/adapted:

- `parse_survey_reply` — markers (incl. `COMPLETE.`/`COMPLETE!`), numbered
  questions, mixed prose skipped, cap; empty/prose output is `Malformed`
  (a contract violation that fails the survey, never a silent completion).
- `parse_plan_blocks` — full JSON, missing-field defaults, fences, line
  fallback, empty-question filtering, cap; malformed/unusable JSON
  (`[{}]`, wrong field types, bare objects) fails without a line fallback,
  and JSON wrapped in model prose ("Here is the plan:\n[…]") is still
  parsed as JSON; a legacy JSON array of strings still works.
- `parse_approval` — APPROVED variants, JSON revision, list-marker
  requirement, malformed prose/empty → `Malformed` (never silent approval),
  incl. structured JSON with no usable questions.
- `closed_reply_channel_fails_plan_approval_closed` — a gate whose reply
  channel closed fails the approval phase instead of running unapproved
  searchers; `undelivered_gate_reply_is_rolled_back_and_restored_to_composer`
  — a persisted reply whose channel delivery fails is rolled back so a
  retry can't duplicate it.
- `steer_queue_is_hard_bound` — `/steer` past the 64-entry bound is refused;
  the ack-driven prune also happens immediately on the `steer #N` stage
  update, not only on the next steer.
- `plan_ready_in_incognito_mode_writes_no_plan_file` — incognito (captured
  at job start) writes neither the plan file nor the plan message; the
  in-memory transcript still shows it while viewed.
- `incognito_gate_rows_follow_the_mode_captured_at_job_start` and
  `off_screen_incognito_plan_is_restored_when_its_session_opens` — survey,
  reply, and plan persistence cannot change after a mid-job mode toggle, and
  a private off-screen prompt remains actionable without a DB write.
- `reasoning_efforts_prefer_catalog_metadata_then_use_backend_fallbacks`,
  `reasoning_cycle_uses_explicit_none_and_clears_stale_preferences`, and
  `starting_a_request_clears_an_unsupported_reasoning_preference` — exact
  sparse/max/xhigh sets, real provider disable semantics, and DB/UI/request
  synchronization when capabilities change.
- `plan_question_prompt_includes_topic_and_full_brief`, `plan_text` rendering,
  `survey_messages`/`plan_approval_messages` prompt builders,
  `planner_messages_with_context_folds_user_answers_into_the_prompt`.
- Gate behavior: PlanReady/SurveyReady arm the gate; replies route through
  the channel and are recorded as `gate_reply` rows that `build_history`
  excludes; **cross-session guard** (gate in another session never
  intercepts).
- `plan_ready_saves_a_plan_file_record_in_the_space` — `plan-*.md` lands in
  the space with topic header + questions;
  `plan_ready_in_incognito_mode_writes_no_plan_file` — "nothing persists"
  never writes the plan (which folds in survey replies) to disk.
- `off_screen_gate_marks_the_session_unread_and_notifies` — a SurveyReady
  that arrives while another session is viewed marks the job session unread
  and says where input is needed.
- `compaction_tail_skips_rows_that_must_never_reach_the_model` — digests
  exclude the same roles as `build_history`, so contextless "drop Q2" can't
  leak into later history via compaction.
- `research_live_popup_uses_the_job_sessions_rows_from_any_session` — the
  live view uses the job-level row mirror when opened elsewhere; channel
  closure now closes the popup instead of showing an empty waiting state.
- `multiple_drained_steers_each_keep_their_own_stage_row`,
  `steer_rows_do_not_collide_on_duplicate_prefix_or_wildcard_text`, and
  `steer_log_drops_acknowledged_entries_and_clears_on_stop` — drained
  steers persist as sequence-keyed (`steer #N`) rows and are acknowledged
  job-globally; the retained log drops picked-up entries and clears on stop.
- DB `survey`/`gate_reply` round-trips; history renderer survey section;
  popup steer queue.

## 8. Known limitations / deferred

- Survey section is always visible, not collapsible ("simplest first").
- The survey's clarifying questions are generated from the topic alone — the
  concurrent known-chunks/web-survey context arrives after the first round.
- A rework presentation within the same second overwrites the plan file (same
  timestamped name) — the record reflects the latest presented plan.
