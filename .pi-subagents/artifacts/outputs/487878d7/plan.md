# Implementation Plan

## Goal
Make every Ratatui modal popup in `nexus-chat` share one visual language and one predictable keyboard interaction model while preserving each popup’s existing domain behavior.

## Review Findings
- medium: `src/ui/popups/model.rs` bypasses `chrome::popup_block`, so the model picker still uses square borders, unstyled titles, reversed selection, and a different selection glyph than most popups.
- medium: `src/ui/popups/swarm.rs` renders a second nested edit popup for persona name/blurb, while session/space/files/skills edit in-place; this creates inconsistent focus and visual hierarchy.
- medium: `src/ui/popups/watches.rs` deletes immediately on plain `d`; destructive actions elsewhere use Ctrl+D to enter a confirmation state and Ctrl+D again to confirm.
- low: `src/ui/popups/copy.rs` uses reversed highlight and `›`, unlike the majority `BOLD` + `▸ ` list selection pattern.
- low: `src/ui/popups/login.rs`, `src/ui/popups/swarm.rs`, `src/ui/popups/research_live.rs`, and several mode titles always show hints even when `app.settings.hide_hints` is enabled.
- low: popup sizing and title wording are ad hoc across `src/ui/popups/*.rs`, making spacing and discoverability feel inconsistent even though `src/ui/popups/chrome.rs` already provides a partial shared chrome layer.

## Tasks
1. **Define the popup design contract in shared chrome helpers**
   - File: `src/ui/popups/chrome.rs`
   - Changes:
     - Keep `popup_block`, `split_with_detail`, and `render_detail`, but extend the module with helpers that every popup can reuse:
       - `render_frame(f, area, title, theme) -> Rect`: calls `Clear`, renders `popup_block`, returns the inner area.
       - `popup_block_focused(title, theme, focused) -> Block`: same rounded border/title treatment as `popup_block`, with focused border using `theme.border` and unfocused border using `theme.border_dim`.
       - `standard_list(items) -> List`: applies the standard list highlight style (`Modifier::BOLD`) and symbol (`"▸ "`). If lifetime ergonomics are awkward, make this `style_list(list) -> List` instead.
       - `danger_title(...)` or `confirm_title(...)`: returns a `Line`/`String` for destructive confirmations using consistent wording: `remove/delete "name"? (Ctrl+D confirm · Esc cancel)`.
       - `input_title(...)`: returns a consistent title for edit/add/search modes with the cursor marker `▏` and `Enter … · Esc cancel` hints.
       - `hinted_title(hide_hints, plain, with_hint)`: mirror `crate::ui::hint_title` but make it available from chrome so popup modules do not hand-roll hint gating.
     - Add unit tests for wrapping/detail sizing and any pure title helper behavior.
   - Acceptance: all popup renderers can clear/render their outer frame through one helper; helper tests pass with `cargo test chrome` or full `cargo test`.

2. **Move model picker onto shared popup chrome without breaking mouse hit testing**
   - File: `src/ui/popups/model.rs`
   - Changes:
     - Replace direct `Block::default().borders(Borders::ALL)` in `panel_list` with `chrome::popup_block_focused`.
     - Use the standard list highlight style and glyph unless there is a strong reason to preserve the current `REVERSED` highlight; if changed, update both Favorites and Available panels.
     - Clear the combined outer area once, or keep clearing each column, but ensure the two rounded panels visually match the rest of the app.
     - Update `list_inner` to compute the inner rect from the same border assumptions used by `chrome::popup_block_focused`; keep its public signature because `src/events.rs` uses it for mouse routing.
     - Gate the available-panel hint string with the same shared hint helper used elsewhere.
   - Acceptance: `/model` shows rounded focused/unfocused panels, standard selection marker, and mouse clicks still select the expected model rows.

3. **Standardize list highlight and empty-state styling across simple list popups**
   - Files:
     - `src/ui/popups/copy.rs`
     - `src/ui/popups/login.rs`
     - `src/ui/popups/apps.rs`
     - `src/ui/popups/watches.rs`
     - `src/ui/popups/files.rs`
     - `src/ui/popups/skills.rs`
     - `src/ui/popups/session.rs`
     - `src/ui/popups/space.rs`
     - `src/ui/popups/settings.rs`
   - Changes:
     - Replace local `.highlight_style(...)` / `.highlight_symbol(...)` pairs with the shared chrome helper.
     - Use `theme.fg` for primary labels, `theme.fg_dim` for metadata, `theme.accent` for active/search/filter values, and `theme.warning`/`theme.error` only for warnings or destructive confirmations.
     - Keep settings/skills detail strips but render the outer frame via `chrome::render_frame` to match every other modal.
     - Ensure empty states are dim and non-selectable-looking: e.g. `no apps yet…`, `no watches yet…`, `no personas yet…`.
   - Acceptance: list popups all use `▸ ` + bold selection, rounded borders, and consistent dim metadata; no popup regresses to `REVERSED` selection except if explicitly documented.

4. **Normalize title wording and hint visibility**
   - Files:
     - `src/ui/popups/session.rs`
     - `src/ui/popups/space.rs`
     - `src/ui/popups/files.rs`
     - `src/ui/popups/skills.rs`
     - `src/ui/popups/apps.rs`
     - `src/ui/popups/watches.rs`
     - `src/ui/popups/swarm.rs`
     - `src/ui/popups/research_live.rs`
     - `src/ui/popups/login.rs`
     - `src/ui/popups/key.rs`
     - `src/ui/popups/context.rs`
     - `src/ui/popups/settings.rs`
   - Changes:
     - Use one title grammar:
       - Browse mode: `" <name> — <primary action/context> "` when hints are visible, short `" <name> "` when hidden.
       - Search/filter mode: `" <name> — search: <filter>▏  (<actions>) "`.
       - Edit/add mode: `" <verb>: <value>▏  (Enter <save/create/import> · Esc cancel) "`.
       - Destructive mode: `" remove/delete \"<name>\"? (Ctrl+D confirm · Esc cancel) "`.
     - Replace hard-coded hints in `login.rs`, `swarm.rs`, and `research_live.rs` with shared hide-hints-aware titles.
     - Keep `key.rs` special behavior clear: Esc returns to `Popup::Login`, so title should say `Esc back`, not `Esc close`.
     - Fix wording mismatches, e.g. context says `v views digest`; make it `v view/edit digest` or similar, matching the external-editor behavior in `src/events.rs`.
   - Acceptance: toggling `HideHints` in `/config` visibly shortens every popup title; no popup continues to show long keybinding strings in hidden-hints mode.

5. **Make destructive actions consistent, especially watches**
   - Files:
     - `src/app/mod.rs`
     - `src/app/watches.rs`
     - `src/ui/popups/watches.rs`
     - `src/app/tests.rs`
   - Changes:
     - Add `WatchMode { Browse, ConfirmDelete }` near existing popup mode enums in `src/app/mod.rs`.
     - Add `watch_mode: WatchMode` to `App`, initialize it to `WatchMode::Browse`, and reset it in `App::open_watch_picker` (`src/app/watches.rs`).
     - Update `src/ui/popups/watches.rs`:
       - Browse: Esc closes, Up/Down navigate, Enter opens selected watch session, Ctrl+D enters `ConfirmDelete` if there is a selected watch.
       - ConfirmDelete: Ctrl+D calls `delete_selected_watch()` and returns to Browse; Esc returns to Browse.
       - Remove immediate plain-`d` deletion from the popup UI.
     - Add tests that one `Ctrl+D` does not delete and second `Ctrl+D` in confirm mode does delete; Esc from confirm mode cancels.
   - Acceptance: watch deletion behaves like apps/files/session/space/skills/swarm delete flows and tests cover the changed behavior.

6. **Remove nested swarm edit popup and reuse edit-mode conventions**
   - File: `src/ui/popups/swarm.rs`
   - Changes:
     - Delete the secondary centered `edit_area` overlay in `render`.
     - Render `SwarmPopupMode::EditName` and `EditBlurb` using the main popup frame/title convention, just as session/space/files edit modes do.
     - Show the current `app.swarm_edit` value with the cursor marker in the title or as the first body row; prefer the same approach chosen by `input_title` in `chrome.rs`.
     - Keep existing key handling via `classify_edit_key`.
   - Acceptance: editing a swarm persona no longer creates a popup-on-popup; Esc/Enter behavior stays unchanged.

7. **Make text-input popups visually consistent**
   - Files:
     - `src/ui/popups/key.rs`
     - `src/ui/popups/research_live.rs`
     - `src/ui/popups/files.rs`
     - `src/ui/popups/session.rs`
     - `src/ui/popups/space.rs`
     - `src/ui/popups/skills.rs`
     - `src/ui/popups/swarm.rs`
   - Changes:
     - Use the same cursor glyph (`▏`), title grammar, and hint ordering for typed fields.
     - Masked API key input in `key.rs` should remain masked, but its surrounding frame/title should use the same helper.
     - `research_live.rs` should render a clear “steer” input affordance and a dim empty state; keep Enter-to-send and Esc-to-close.
   - Acceptance: every mode where typing is accepted has the same visual cursor and Enter/Esc hint order.

8. **Preserve and verify settings/skills detail-strip behavior under the new frame helper**
   - Files:
     - `src/ui/popups/settings.rs`
     - `src/ui/popups/skills.rs`
     - `src/ui/popups/chrome.rs`
   - Changes:
     - Continue to call `split_with_detail` and `render_detail` after the shared frame is rendered.
     - Ensure the detail strip divider uses `theme.border_dim` and wrapped text uses `theme.fg_dim`.
     - Verify long descriptions still wrap to at most `MAX_DETAIL_ROWS` plus divider.
   - Acceptance: settings and skills selected-row descriptions still appear, wrap, and do not consume list rows when empty.

9. **Add lightweight UI regression tests for shared popup appearance**
   - New File: `src/ui/popups/tests.rs`
   - File: `src/ui/popups/mod.rs`
   - File: `src/app/tests.rs`
   - Changes:
     - In `src/ui/popups/mod.rs`, add `#[cfg(test)] mod tests;`.
     - In `src/ui/popups/tests.rs`, create a minimal `App` using `Db::open_in_memory()` and a temp `Space`, then render representative popups with `ratatui::backend::TestBackend`:
       - `Popup::Copy` or `Popup::Login` asserts rounded border characters are present and standard selection marker `▸` is present.
       - `Popup::Model` asserts both panels render rounded borders and no raw square-border-only styling remains.
       - `Popup::Settings` with a selected field that has a description asserts the detail divider/text render.
       - A hidden-hints case sets `app.settings.hide_hints = true` and asserts a long hint such as `Ctrl+D` is not in the buffer for at least one popup that previously hard-coded it.
     - Put behavior-only watch deletion tests in `src/app/tests.rs` if easier to access app internals.
   - Acceptance: tests fail against the current inconsistent implementation and pass after the design pass.

10. **Manual verification pass for every popup**
   - Files: no code changes; verification only.
   - Changes: Run the app and open these UI states:
     - `/model`
     - `/session` browse/rename/delete confirm
     - `/space` browse/create/rename/delete confirm
     - `/login` and an API key prompt
     - `/config`
     - `/copy`
     - context popup (`Ctrl+G` per existing binding)
     - `/skills` browse/install/remove confirm
     - `/files` browse/pick/add/rename/remove confirm
     - `/apps` browse/remove confirm
     - `/watch` browse/remove confirm
     - `/swarm` browse/edit name/edit blurb/remove confirm
     - research live popup while a research job is active, if practical
   - Acceptance: each popup has rounded borders, consistent title grammar, standard list selection, Esc behavior that either closes or cancels one level, and hidden hints are respected.

## Files to Modify
- `src/ui/popups/chrome.rs` - add reusable frame, focused block, list styling, title, input, and confirmation helpers.
- `src/ui/popups/mod.rs` - optionally expose/add tests for shared popup helpers; keep existing key classifiers.
- `src/ui/popups/model.rs` - migrate two-column picker to shared chrome and standard list style while preserving mouse hit areas.
- `src/ui/popups/copy.rs` - use standard list highlight/symbol and shared frame helper.
- `src/ui/popups/login.rs` - use hide-hints-aware title and shared list styling.
- `src/ui/popups/key.rs` - use shared frame/input title while preserving masked value and Esc-to-login behavior.
- `src/ui/popups/context.rs` - use shared title wording and verify context digest hint matches `src/events.rs`.
- `src/ui/popups/session.rs` - use shared title/input/confirm/list helpers.
- `src/ui/popups/space.rs` - use shared title/input/confirm/list helpers.
- `src/ui/popups/settings.rs` - use shared frame/list helpers while preserving detail strip.
- `src/ui/popups/skills.rs` - use shared frame/list/input/confirm helpers while preserving detail strip.
- `src/ui/popups/files.rs` - use shared title/input/confirm/list helpers for Browse/Add/Rename/Pick modes.
- `src/ui/popups/apps.rs` - use shared confirm/list helpers.
- `src/ui/popups/watches.rs` - add confirmation mode handling and shared confirm/list helpers.
- `src/ui/popups/research_live.rs` - use shared frame/input title and hidden-hints-aware title.
- `src/ui/popups/swarm.rs` - remove nested edit popup and use shared edit/list/confirm conventions.
- `src/app/mod.rs` - add `WatchMode`, `watch_mode` state, and initialization.
- `src/app/watches.rs` - reset watch popup mode on open and keep delete helper behavior reusable.
- `src/app/tests.rs` - add watch delete-confirm behavior tests; optionally add helper coverage if not placed under `src/ui/popups/tests.rs`.
- `src/events.rs` - only update if model picker hit testing needs an adjusted helper call after `model.rs` changes.

## New Files
- `src/ui/popups/tests.rs` - lightweight Ratatui `TestBackend` regression tests for shared popup appearance and hidden-hints behavior.

## Dependencies
- Task 1 blocks Tasks 2-8 because the shared helper API should be stable before migrating renderers.
- Task 2 must preserve the `model_popup_areas` and `list_inner` contracts used by `src/events.rs` mouse handling.
- Task 5 requires the `WatchMode` app state before `src/ui/popups/watches.rs` can implement confirmation rendering/keys.
- Task 6 should happen after Task 1 so swarm edit mode can reuse the new input/title helper.
- Task 9 should be added after representative popup migrations, but the watch behavior tests can be added immediately after Task 5.
- Task 10 depends on all code changes and tests compiling.

## Risks
- Ratatui lifetime types can make generic `standard_list` helpers awkward; prefer simple helper functions that take and return `List<'a>` if constructors become noisy.
- `src/ui/popups/model.rs` is the only popup with mouse hit testing; changing borders/layout without updating `list_inner` can misalign clicks.
- Adding `WatchMode` is a small behavior change: users lose immediate plain-`d` deletion, but gain consistent destructive confirmation.
- Long titles can overflow narrow terminals; use hidden-hints mode and concise wording to keep titles readable.
- Unicode rounded borders and glyphs (`╭`, `▸`, `▏`) are already used elsewhere, but manual verification on the target terminal is still needed.
- Some popups are difficult to reach without runtime state (research live, file picker, apps); tests should cover representative rendering, and manual verification should cover stateful modes.

## Verification Steps
1. Run `cargo fmt`.
2. Run `cargo test`.
3. If available in this environment, run `shazam_verify` after edits and `shazam_verify --preCommit` before committing, because the project overview flags existing uncommitted changes.
4. Run the manual popup pass from Task 10 and specifically check hidden-hints mode from `/config`.
5. Verify no unexpected user data/state changes occur when testing destructive confirmations; use temp/sample data where possible.

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Plan includes concrete severity-tagged review findings and file-specific implementation tasks for src/ui/popups/*.rs, src/app/mod.rs, src/app/watches.rs, src/app/tests.rs, and src/events.rs where applicable."
    }
  ],
  "changedFiles": [
    ".pi-subagents/artifacts/outputs/487878d7/plan.md"
  ],
  "testsAddedOrUpdated": [],
  "commandsRun": [],
  "validationOutput": [
    "Inspected popup renderer modules, shared chrome helpers, app popup state, watch behavior, UI render entrypoint, and existing tests; no code changes were made."
  ],
  "residualRisks": [
    "The plan changes watch deletion from immediate plain-d deletion to Ctrl+D confirmation for consistency; this should be accepted as an intentional UX change before implementation.",
    "Model picker mouse hit testing must be carefully verified after shared chrome migration.",
    "Ratatui helper lifetimes may require helper API adjustment during implementation."
  ],
  "noStagedFiles": true,
  "diffSummary": "Planning artifact only; repository source files were not modified.",
  "reviewFindings": [
    "medium: src/ui/popups/model.rs - model picker bypasses shared chrome and uses square/unfocused styling unlike other popups.",
    "medium: src/ui/popups/swarm.rs - edit modes render a nested centered popup instead of the common in-place edit convention.",
    "medium: src/ui/popups/watches.rs - destructive deletion happens immediately on plain d rather than Ctrl+D confirmation.",
    "low: src/ui/popups/copy.rs - selection style and glyph differ from the standard list popups.",
    "low: src/ui/popups/login.rs, src/ui/popups/swarm.rs, src/ui/popups/research_live.rs - long hints are hard-coded and do not consistently respect hidden-hints mode."
  ],
  "manualNotes": "This is an implementation-ready design pass plan for a coding worker; no source edits or tests were run by this planning subagent."
}
```