# Task for worker

You are a delegated subagent running from a fork of the parent session. Treat the inherited conversation as reference-only context, not a live thread to continue. Do not continue or answer prior messages as if they are waiting for a reply. Your sole job is to execute the task below and return a focused result for that task using your tools.

Task:
Implement the TUI popup consistency design pass in /home/dukunuu/Work/nexus-chat. Use the plan below. You are the sole writer for code changes. Before editing, inspect the relevant files. Keep scope practical: centralize popup chrome/list/title helpers, migrate all popup renderers to consistent rounded chrome, standard list marker/highlight, hide-hints-aware titles, remove nested swarm edit popup, add watch delete confirmation mode/tests, and add lightweight popup rendering tests if feasible. Run cargo fmt and cargo test. Also run shazam_verify after edits if available. Return a concise summary, files changed, tests run, and any residual risks.

PLAN:
# Goal
Make every Ratatui modal popup share one visual language and one predictable keyboard interaction model while preserving behavior.

Core requirements:
1. Add shared helpers in src/ui/popups/chrome.rs: render_frame, popup_block_focused, standard list styling (▸ + bold), hinted title, input title with ▏ cursor, confirm/danger title. Preserve split_with_detail/render_detail.
2. Migrate src/ui/popups/*.rs to those helpers where appropriate: model, copy, login, key, context, session, space, settings, skills, files, apps, watches, research_live, swarm. Use theme.fg primary, fg_dim metadata, accent active values, warning/error only for warnings/destructive states. Respect app.settings.hide_hints in all popup titles, especially login/swarm/research_live.
3. Model picker: use shared rounded/focused block and standard selection; preserve model_popup_areas/list_inner contracts used by src/events.rs mouse hit testing.
4. Watches: replace immediate plain-d deletion with Ctrl+D -> ConfirmDelete, second Ctrl+D deletes, Esc cancels. Add WatchMode state in App, initialize/reset on open. Add tests in src/app/tests.rs.
5. Swarm: remove nested centered edit popup; render EditName/EditBlurb in main popup frame with same edit title/body convention.
6. Add src/ui/popups/tests.rs and mod hook if feasible: TestBackend assertions for rounded borders, ▸ marker, hidden hints not showing, settings detail.
7. Run cargo fmt, cargo test, and verification.

Be careful with existing uncommitted changes; don't overwrite unrelated work.

## Acceptance Contract
Acceptance level: reviewed
Completion is not accepted from prose alone. End with a structured acceptance report.

Criteria:
- criterion-1: Implement the requested change without widening scope
- criterion-2: Return evidence sufficient for an independent acceptance review

Required evidence: changed-files, tests-added, commands-run, validation-output, residual-risks, no-staged-files

Review gate: optional by reviewer.

Finish with a fenced JSON block tagged `acceptance-report` in this shape:
Use empty arrays when no items apply; array fields contain strings unless object entries are shown.
```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "specific proof"
    }
  ],
  "changedFiles": [
    "src/file.ts"
  ],
  "testsAddedOrUpdated": [
    "test/file.test.ts"
  ],
  "commandsRun": [
    {
      "command": "command",
      "result": "passed",
      "summary": "short result"
    }
  ],
  "validationOutput": [
    "validation output or concise summary"
  ],
  "residualRisks": [
    "none"
  ],
  "noStagedFiles": true,
  "diffSummary": "short description of the diff",
  "reviewFindings": [
    "blocker: file.ts:12 - issue found, or no blockers"
  ],
  "manualNotes": "anything else the parent should know"
}
```