# Implementation Plan

## Goal
Make deep-research progress transparent by presenting every pipeline agent as a live, persistent activity row with clear running/completed/error states and readable tool activity.

## Tasks
1. **Standardize lifecycle updates for every research agent**: Emit a start update, meaningful progress detail, and a terminal success/error update for Planner, every Searcher, Synthesizer, Critic, contradiction resolver, Verifier, and Writer.
   - File: `src/app/research.rs`
   - Changes:
     - Keep `ResearchUpdate::Stage`, but standardize `detail` prefixes such as `working —`, `done —`, and `error —` so the UI can style state without a database migration.
     - Include the round in searcher labels (for example, `searcher r1 2/5`) so round-two agents do not overwrite round-one rows through the existing label-based upsert.
     - After planning, report the number of generated questions; after synthesis/writing, report completion; after criticism, report `satisfied`, gap count, or contradiction; after each searcher, report completion/error rather than leaving its last tool call looking active.
     - Replace raw truncated tool-call JSON with the existing concise tool summary helper where possible, while retaining the active sub-question in the row.
     - Extend `verify_with_quote_check` to receive the research update channel and IDs, forwarding its `Status`, `ToolCall`, `Error`, and completion activity instead of silently swallowing everything except output tokens.
   - Acceptance: A simulated research run produces one independently identifiable row per agent, and every started row eventually reads `done` or `error`.

2. **Ensure live updates actually repaint inline**: Invalidate the wrapped transcript cache whenever an existing `research_stage` message is updated in place.
   - File: `src/app/research.rs`
   - Changes: In `App::on_research_done`, after mutating a matching stage row, reset `self.history_cache` (or expose a small cache-invalidation helper) so unchanged message count does not leave stale activity text on screen.
   - Acceptance: Two successive `Stage` updates with the same label visibly replace the transcript text without adding a duplicate row.

3. **Turn the live research popup into an agent dashboard**: Render lifecycle state, agent label, and current action with a stronger visual hierarchy and keyboard navigation.
   - Files: `src/ui/popups/research_live.rs`, `src/app/mod.rs`
   - Changes:
     - Add `research_live_selected: usize` to `App`, initialize/reset it when opening or starting research, and support Up/Down navigation.
     - Parse persisted stage content at the first `:` into label/detail.
     - Render a state glyph and color (`●` accent for working, `✓` success for done, `×` error for failed), bold agent name, and the detail on an indented second line so long queries/tool actions are more readable than the current one-line list.
     - Keep the steer input, but retitle the popup to `research agents` and include discoverable hints for Up/Down, Enter-to-steer, and Esc.
     - Clamp selection when rows appear or disappear.
   - Acceptance: With planner, several searchers, and verifier rows present, the popup visibly distinguishes their states and navigating the list keeps the selected multi-line item in view.

4. **Make the dashboard discoverable during research**: Advertise and optionally open the live view at the right moments without blocking the plan gate.
   - Files: `src/app/research.rs`, `src/ui/mod.rs`
   - Changes:
     - Include `Ctrl+Space: research agents` in the research-running status text after the plan is approved (and immediately for ungated `/research!`).
     - Preserve the plan-approval transcript flow; do not auto-open the dashboard before the user approves/edits the plan.
     - If auto-opening is desired, do it only after approval and retain Esc as an immediate return to the transcript; otherwise the explicit status hint is the safer minimal behavior.
   - Acceptance: A user starting research can discover how to open the live activity view without knowing the shortcut beforehand.

5. **Add regression coverage for activity updates and rendering**: Verify state transitions, cache refresh, round-specific identity, and dashboard presentation.
   - Files: `src/app/tests.rs`, `src/app/research.rs` (existing `#[cfg(test)]` module), `src/ui/popups/tests.rs`
   - Changes:
     - Add an app test sending two `ResearchUpdate::Stage` values for one label and assert one message row contains the newest detail and the history cache is invalidated/rebuilt.
     - Add tests for standardized Searcher/Verifier tool detail formatting and round-specific labels.
     - Add a Ratatui buffer test with working/done/error rows asserting state glyphs, agent names, current details, and navigation hints are rendered.
     - Retain existing tests that assert DB upsert behavior and stage rows are excluded from model history.
   - Acceptance: Focused activity/UI tests and `cargo check` pass; full tests have no new failures.

## Files to Modify
- `src/app/research.rs` - emit complete per-agent lifecycle/tool updates, forward Verifier activity, invalidate transcript cache, and expose the dashboard shortcut.
- `src/ui/popups/research_live.rs` - render a navigable, stateful agent dashboard.
- `src/app/mod.rs` - store and initialize live-dashboard selection.
- `src/ui/mod.rs` - show a discoverable live-research shortcut while a job runs, if the status rendering is centralized here.
- `src/app/tests.rs` - cover in-place stage updates and cache invalidation.
- `src/ui/popups/tests.rs` - cover dashboard rendering and hints.

## New Files
- None.

## Dependencies
- Task 3 depends on Task 1's standardized lifecycle prefixes and labels.
- Task 4 depends on the dashboard behavior in Task 3.
- Task 5 depends on Tasks 1–4.
- Task 2 is independent but should land before UI validation because stale cache behavior can hide otherwise-correct updates.

## Risks
- **High**: `src/app/research.rs` currently mutates a stage message in place while `src/ui/history.rs` caches by message count; without explicit invalidation, inline progress can remain stale and make the user appear “in the dark.”
- **High**: `verify_with_quote_check` currently discards tool status and tool-call events, so Verifier work is entirely opaque.
- **Medium**: Searcher labels currently omit the round, so second-round agents can overwrite first-round activity rows with the same `searcher N/total` label.
- **Medium**: The current popup is only reachable through an undisclosed `Ctrl+Space` binding and renders long activity as clipped one-line items with no navigation.
- **Medium**: Inferring state from detail prefixes is intentionally migration-free but requires all stage emitters to follow the convention. A future structured `ResearchStageState` can replace this if lifecycle logic grows.
- **Validation caveat**: Network-backed end-to-end research remains manually testable; unit tests should target event construction, state handling, and Ratatui rendering without live provider calls.

## Review Findings
- **high** — `src/app/research.rs` / `src/ui/history.rs`: in-place stage updates do not change message count, so the transcript cache may not repaint updated activity.
- **high** — `src/app/research.rs`: Verifier streaming events are consumed without being forwarded to the UI.
- **medium** — `src/app/research.rs`: Searchers emit starts/tool activity but no terminal completion event, leaving their final state ambiguous.
- **medium** — `src/ui/popups/research_live.rs`: the existing view is flat, non-navigable, and likely clips long sub-questions/tool arguments.
- **medium** — `src/events.rs`: `Ctrl+Space` opens the view, but the shortcut is not surfaced where a new research user is likely to see it.