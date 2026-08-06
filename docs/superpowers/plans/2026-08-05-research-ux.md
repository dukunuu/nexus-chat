# Research Flow UX Proposal (rev 4 — prompting section)

> The interaction is **conversational**: a dedicated survey section where the
> AI asks the user what they want, the user answers in chat and approves in
> plain language ("I approve"), and the AI generates the plan — questions with
> suggestions, custom input folded in — for a final approval. No keybindings,
> no file round-trip, no popup.

---

## 1. The flow

```
/research <topic>
  │
  ├─ SURVEY SECTION ────────────────►  AI asks the user what they want
  │                                    (scope, depth, angles, constraints).
  │                                    Rendered as a dedicated section in the
  │                                    transcript. Reusable by other modes.
  │      ▲ user answers in chat (1–3 rounds, AI asks follow-ups)
  │      │
  │      └─ user says "I approve" ──►  (web landscape survey runs
  │                                    concurrently while the user answers;
  │                                    local chunks gathered too)
  │
  ├─ PLAN ──────────────────────────►  AI generates questions with
  │                                    suggestions (angles/sources), custom
  │                                    user input folded in. Shown as a plan
  │                                    section; also written to space files
  │                                    as a record (plan-<slug>-<ts>.md).
  │
  ├─ APPROVAL ──────────────────────►  user replies "approve" (or Enter) →
  │                                    searchers run. A non-approval reply
  │                                    ("also look into X", "drop Q2") gets
  │                                    folded in by the AI, one more round.
  │
  ├─ searchers → synthesis → critic → verifier → writer
  │
  └─ report ────────────────────────►  assistant message + research-<slug>.md
```

## 2. The dedicated survey section (reusable)

**What it is:** a new transcript section — `survey` message role — rendered
like the existing reasoning/tool-call blocks (distinct marker, collapsible),
containing the AI's clarifying questions.

```
╭─ survey ─────────────────────────────────────────────╮
│ For "fine-tuning LLMs":                              │
│  1. Depth vs breadth — one technique, or the field?  │
│  2. Current state only, or history too?              │
│  3. Any constraints (cost, compute, deployment)?     │
│ Answer in chat; I may ask one or two follow-ups,     │
│ then you say "I approve".                            │
╰──────────────────────────────────────────────────────╯
```

**Reusability (the "other modes" part):** the section is generic, not
research-specific:
- `survey` message role + renderer in `ui/history.rs` (mirrors
  `push_research_stage` / `push_research_plan`).
- App-level plumbing: `survey_reply_tx/rx` channel + a small `Clarification`
  state on `App`, owned by whoever starts a survey.
- First consumer: research. Later consumers: swarm (clarify a multi-agent
  brief), `/watch` setup (what "changed" should mean), plain chat (vague
  requests). Same section, same channel, same renderer — the mode only
  supplies the questions and consumes the answers.

## 3. How the conversation works (no custom key handling)

- The survey/pipeline pauses while waiting (same parked-task pattern as the
  plan gate — a oneshot/channel await inside `run_research`).
- The user types in the composer and presses Enter as usual. When a survey is
  active **in the viewed session**, the submit routes to the survey channel
  instead of a chat completion (one scoped intercept; everywhere else Enter is
  a normal send).
- The survey agent is completions inside the pipeline: (a) generate initial
  questions from topic + context; (b) given each user reply, either ask
  follow-ups or declare the survey complete (the AI recognizes "I approve",
  "looks good", "go" — no phrase-matching in app code). Bounded: max 3 rounds.
- Plan approval: same channel pattern. Reply "approve" → questions run.
  Reply with edits/custom questions → the AI folds them in and re-presents
  (1 rework round, capped).

## 4. What stays from earlier revisions

- **Detailed plan content**: questions with suggestions (Why/Angles/Sources),
  and each searcher gets its full block as its prompt — detail is functional.
- **Plan → space files** as a record (`plan-<slug>-<ts>.md` next to the
  report), per the previous decision. It's a byproduct now, not the edit
  surface: the conversation is the edit surface.
- **Web survey + local chunks** still feed the planner ("Known so far"),
  gathered concurrently while the user answers, so the plan is grounded.
- Quick wins: human stage labels (`round 1` not `r1`), critic gaps shown as
  questions, steer queue visible in the live popup.
- The two-line guard: Enter/approval only intercepts in the research session
  (fixes the current cross-session hijack where Enter elsewhere swallows a
  message into the plan gate).

## 5. What's gone

- Plan-gate keybindings (`e` opens editor, Esc stops research via global
  intercept) — approval is a chat reply now.
- Temp-file `$EDITOR` plan editing machinery.
- Composer full-rewrite submit path.
- Any popup plan editor.

## 6. Implementation notes

- `ResearchUpdate` gains `SurveyReady { questions: Vec<String> }` (or the
  pipeline writes the survey message itself); replies flow through a new
  `research_reply_tx` mpsc that the gate reuses for both survey and approval
  phases.
- `run_research_inner` order: user survey (parked wait) ∥ web survey + local
  chunks → planner (topic + answers + known) → PlanReady (parked wait) →
  searchers. Round caps: survey ≤3, plan rework ≤1.
- Pure, tested functions: parse plan blocks (already specified), fold user
  reply into plan prompt. Gate tests adapt to the chat-reply shape.
- `save_plan_file` mirrors `save_research_report` (slug + timestamp, same
  files dir, no citation indexing).
- Renderer: `push_survey_section` collapsible like reasoning blocks
  (existing `Ctrl+R`-style toggle or always-visible with dimmed questions —
  simplest first).
