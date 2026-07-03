# nexus-chat: app/ui/events Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Break up `app.rs` (3164 lines, 8 mixed responsibilities), `ui.rs` (922 lines), and `events.rs` (621 lines) into focused modules that mirror the app's actual subsystems, and eliminate the 9 duplicated-logic instances found in the pre-refactor audit — with zero behavior change at every step.

**Architecture:** This is a pure structural refactor of existing, working, tested Rust code — not new feature work. The `App` struct stays a single type (its ~100 fields and methods are genuinely one piece of runtime state), but its `impl App` blocks move out of one 3164-line file into one file per subsystem (`app/chat.rs`, `app/memory.rs`, etc.), which Rust supports natively via multiple `impl App {}` blocks across files in the same module tree. `ui.rs` and `events.rs` get split the same way, and popup render+handle+mode-enum code (currently split three ways across `app.rs`/`ui.rs`/`events.rs` per popup) gets colocated into `ui/popups/<name>.rs`. Each task is a mechanical move (cut function(s), paste into new file, fix imports) verified by `cargo build` + `cargo test` — the existing 700+ line test suite (currently `app.rs:2430-3164`) is the regression net and must pass unchanged after every task.

**Tech Stack:** Rust, ratatui (TUI), tokio, rusqlite, reqwest.

## Global Constraints

- No behavior change. Every task ends with `cargo build` (no warnings introduced) and `cargo test` passing, matching the state before the task.
- No function signatures change during Phase 1/2 (pure move). Signature changes only happen in the Phase 3 dedup tasks, which are each scoped to one duplication finding.
- Commit after every task. If a task's build/test fails and the fix isn't obvious within the task's own scope, stop and report rather than widening the change.
- Preserve all existing doc comments on moved fields/functions verbatim.
- Baseline commit for rollback: `e577dc1`.

---

## Phase 1 — Split `app.rs` into `app/` module

### Task 1: Widen private `App` field visibility to `pub(crate)`

**Files:**
- Modify: `src/app.rs:277-413` (the `App` struct definition)

**Interfaces:**
- Produces: every `App` field becomes accessible from sibling submodules under `app/` in later tasks. Field names/types unchanged.

- [ ] **Step 1:** In the `App` struct, change these currently-private fields to `pub(crate)` (leave already-`pub` fields as-is): `space`, `provider`, `key`, `memory_rx`, `compact_rx`, `forced_skill`, `toolbox`, `skills_rx`, `models_rx`, `thinking_text`, `stream_rx`, `stream_started`, `stream_usage`, `context_total`, `spinner_frame`, `thinking_idx`, `spinner_color`.
- [ ] **Step 2:** Run `cargo build` — expect success, no new warnings (fields already used within the same file, visibility widening is a no-op for current callers).
- [ ] **Step 3:** Run `cargo test` — expect all existing tests pass, unchanged.
- [ ] **Step 4:** Commit:
```bash
git add src/app.rs
git commit -m "refactor: widen App field visibility for module split"
```

### Task 2: Create `app/mod.rs` — core struct, enums, lifecycle

**Files:**
- Create: `src/app/mod.rs`
- Delete: `src/app.rs` (after content is moved)

**Interfaces:**
- Produces: `pub struct App` and all its fields (unchanged), plus these items moved verbatim from `app.rs`: `Popup` (20-33), `SkillsMode` (35-42), `SpaceMode` (44-50), `CopyOption` (52-56), `ContextBreakdown` (58-69), `SessionMode` (71-77), `MouseTarget` (79-85), `ModelPanel` (87-93), `ModelPickTarget` (95-101), `SettingsField` + `impl SettingsField` (103-155), `Settings` + `impl Default for Settings` (157-188), `verbosity_clause` (190-261), `AppEvent` (263-275), `App` struct (277-413), and from `impl App`: `new` (416-530), `load_prefs` (532-554), `load_settings` (556-586), `refresh_toolbox` (588-601), `init` (603-607), `is_streaming` (609-613), `is_welcome` (615-618), `tick_spinner` (620-623), `spinner_char` (625-628), `spinner_color` (630-633), `thinking_phrase` (635-638), `thinking_text` (640-687), `fetch_models` (689-699), `on_models_result` (701-724).
- Consumes: nothing new — same imports as current `app.rs:1-16`.

- [ ] **Step 1:** Create `src/app/` directory. Copy `src/app.rs` lines 1-16 (the `use` block) into new `src/app/mod.rs`.
- [ ] **Step 2:** Add module declarations right after the `use` block:
```rust
mod chat;
mod memory;
mod compaction;
mod spaces;
mod sessions;
mod models;
mod settings;
mod skills_popup;
mod copy;

#[cfg(test)]
mod tests;
```
- [ ] **Step 3:** Move (cut from `app.rs`, paste into `app/mod.rs` after the `use`/`mod` block, in original order) the items listed in "Produces" above: all 13 enum/struct definitions (lines 20-275 excluding doc comments loss), the `App` struct (277-413), and one `impl App { ... }` block containing only the 14 methods listed (`new` through `on_models_result`). Leave every other method where it is in `app.rs` for now — later tasks will move them.
- [ ] **Step 4:** In `src/main.rs`, no change needed — `mod app;` resolves to `app/mod.rs` automatically once `app.rs` no longer exists at that path.
- [ ] **Step 5:** Delete the old `src/app.rs` only once every remaining item in it (everything not listed in Step 3) has been cut into the files created by Tasks 3-12. For *this* task, leave `app.rs` in place with only the moved items removed — it will still contain the rest of the methods, free functions, and the test module, and will still compile as `impl App` blocks split across `app.rs` and `app/mod.rs` are **not** valid Rust (a file can't be both `app.rs` and `app/mod.rs` for the same module). So instead: keep everything in `app/mod.rs` for this task (don't split yet) — i.e. cut *all* remaining `app.rs` content (the rest of `impl App`, the free functions at 2270-2428, and `#[cfg(test)] mod tests` at 2430-3164) into `app/mod.rs` too, verbatim, in original order, after the 14-method block from Step 3. Then delete `src/app.rs` entirely.
- [ ] **Step 6:** Run `cargo build` — expect success (this is a pure file move, `app/mod.rs` now contains everything `app.rs` used to).
- [ ] **Step 7:** Run `cargo test` — expect all tests pass unchanged.
- [ ] **Step 8:** Commit:
```bash
git add src/app.rs src/app/mod.rs
git commit -m "refactor: move app.rs to app/mod.rs (no content split yet)"
```

> Tasks 3-12 now peel individual method groups and the test module out of `app/mod.rs` into sibling files. After Task 2, `app/mod.rs` is a temporary 3164-line holding file — Tasks 3-12 shrink it back down.

### Task 3: Extract `app/chat.rs`

**Files:**
- Create: `src/app/chat.rs`
- Modify: `src/app/mod.rs` (remove moved items)

**Interfaces:**
- Consumes: `App` fields via `self` (all now `pub(crate)` or `pub` per Task 1), `crate::provider::{ChatMessage, ChatParams, StreamEvent}`, `crate::db::Message`.
- Produces: `impl App` methods `submit`, `send_message`, `on_stream_event`, `finish_stream`, `maybe_generate_title`, `on_title_result`, `system_prompt`, `resolved_base_system_prompt`, `reload_base_system_prompt`, `skills_section` — unchanged signatures, callable as `app.submit()` etc. from anywhere, same as before. Free functions `code_blocks`, `pick_greeting`, `pick_flavor`, `title_from`, `split_inline_reasoning` become `pub(super)` (used by sibling submodules/tests) instead of private.

- [ ] **Step 1:** In `app/mod.rs`, locate methods `submit` (726-787), `open_skills_popup`/`reload_skills`/etc. — **skip those, they move in Task 10** — locate and cut: `send_message` (944-1025), `on_stream_event` (1027-1044), `finish_stream` (1046-1115), `maybe_generate_title` (1117-1149), `on_title_result` (1151-1171), `system_prompt` (1173-1192), `resolved_base_system_prompt` (1194-1200), `reload_base_system_prompt` (1202-1209), `skills_section` (1211-1223), and `submit` (726-787). Also cut free functions `code_blocks` (2270-2296), `pick_greeting` (2298-2304), `pick_flavor` (2306-2313), `title_from` (2315-2323), `split_inline_reasoning` (2371-2397). (Line numbers are pre-Task-3 offsets in the still-full `app/mod.rs`; locate by function name, not literal line number, since earlier cuts shift lines.)
- [ ] **Step 2:** In `src/app/chat.rs`, add:
```rust
use anyhow::Result;
use ratatui::style::Color;

use crate::db::Message;
use crate::provider::{ChatMessage, ChatParams, StreamEvent};

use super::App;

impl App {
    // paste submit, send_message, on_stream_event, finish_stream,
    // maybe_generate_title, on_title_result, system_prompt,
    // resolved_base_system_prompt, reload_base_system_prompt, skills_section here
}

// paste code_blocks, pick_greeting, pick_flavor, title_from, split_inline_reasoning here
```
Change the 5 free functions from `fn` to `pub(super) fn` (they're used by `app/mod.rs`'s test module and possibly siblings).
- [ ] **Step 3:** Run `cargo build`. Fix any missing imports reported (e.g. `Color` for spinner-adjacent code, `chrono::Utc` if `title_from` uses it) by adding them to `chat.rs`'s `use` block — do not re-add anything to `app/mod.rs` that isn't still used there.
- [ ] **Step 4:** Run `cargo test` — expect pass, unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/chat.rs
git commit -m "refactor: extract app/chat.rs (send/stream/title/system-prompt)"
```

### Task 4: Extract `app/memory.rs`

**Files:**
- Create: `src/app/memory.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `read_memory`, `maybe_extract_memory`, `on_memory_result`; free functions `parse_fact_line` (`pub(super)`), `parse_memory_ops` (`pub(super)`) — used by `app/mod.rs` tests.

- [ ] **Step 1:** Cut `read_memory` (1225-1232), `maybe_extract_memory` (1234-1273), `on_memory_result` (1275-1317), `parse_fact_line` (2399-2406), `parse_memory_ops` (2408-2428) (locate by name; original line numbers).
- [ ] **Step 2:** In `src/app/memory.rs`:
```rust
use super::App;
use crate::db::MemoryOp;

impl App {
    // paste read_memory, maybe_extract_memory, on_memory_result here
}

// paste parse_fact_line, parse_memory_ops here, mark pub(super)
```
- [ ] **Step 3:** `cargo build`, fix imports (e.g. `crate::provider::ChatMessage` if `maybe_extract_memory` builds a prompt inline).
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/memory.rs
git commit -m "refactor: extract app/memory.rs"
```

### Task 5: Extract `app/compaction.rs`

**Files:**
- Create: `src/app/compaction.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `effective_messages`, `maybe_compact`, `force_compact`, `start_compaction`, `on_compact_result`, `context_breakdown`, `compact_summary_path`, `reload_compact_summary`.

- [ ] **Step 1:** Cut `effective_messages` (1325-1335), `maybe_compact` (1337-1350), `force_compact` (1352-1378), `start_compaction` (1380-1440), `on_compact_result` (1442-1462), `context_breakdown` (1464-1498), `compact_summary_path` (1500-1509), `reload_compact_summary` (1511-1528).
- [ ] **Step 2:** In `src/app/compaction.rs`:
```rust
use anyhow::Result;

use super::App;
use crate::db::Message;

impl App {
    // paste all 8 methods here
}
```
- [ ] **Step 3:** `cargo build`, fix imports.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/compaction.rs
git commit -m "refactor: extract app/compaction.rs"
```

### Task 6: Extract `app/spaces.rs`

**Files:**
- Create: `src/app/spaces.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `open_space_picker`, `filtered_spaces`, `selected_space`, `move_space_selection`, `space_filter_push`, `space_filter_pop`, `start_space_create`, `start_space_rename`, `confirm_space_create`, `confirm_space_rename`, `confirm_space_delete`, `switch_to_default_space`, `set_active_space`, `instructions_path_for_selected`, `memory_path_for_selected`, `confirm_space`.

- [ ] **Step 1:** Cut lines 1530-1710 (all methods from `open_space_picker` through `confirm_space`, per the grep boundaries: 1530, 1545, 1560, 1564, 1574, 1579, 1584, 1590, 1597, 1609, 1628, 1644, 1658, 1670, 1685, 1697 — ending before `new_session` at 1711).
- [ ] **Step 2:** In `src/app/spaces.rs`:
```rust
use anyhow::Result;

use super::App;
use crate::db::Space as SpaceRow;

impl App {
    // paste all 16 methods here
}
```
- [ ] **Step 3:** `cargo build`, fix imports.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/spaces.rs
git commit -m "refactor: extract app/spaces.rs"
```

### Task 7: Extract `app/sessions.rs`

**Files:**
- Create: `src/app/sessions.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `new_session`, `open_session_picker`, `filtered_sessions`, `selected_session`, `move_session_selection`, `session_filter_push`, `session_filter_pop`, `start_rename`, `confirm_rename`, `confirm_delete`, `confirm_session`; free functions `session_score` (`pub(super)`), `parse_topic` (`pub(super)`), `slugify` (`pub(super)`).

- [ ] **Step 1:** Cut methods 1711-1820 (`new_session` through `confirm_delete`) and `confirm_session` (2252-2268), plus free functions `session_score` (2325-2340), `parse_topic` (2342-2354), `slugify` (2356-2369).
- [ ] **Step 2:** In `src/app/sessions.rs`:
```rust
use anyhow::Result;

use super::App;
use crate::db::Session;

impl App {
    // paste all 11 methods here
}

// paste session_score, parse_topic, slugify here, mark pub(super)
```
- [ ] **Step 3:** `cargo build`, fix imports.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/sessions.rs
git commit -m "refactor: extract app/sessions.rs"
```

### Task 8: Extract `app/models.rs`

**Files:**
- Create: `src/app/models.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `open_model_picker`, `open_model_picker_for_memory`, `open_model_picker_impl`, `reset_model_selection`, `open_key_prompt`, `confirm_key`, `favorite_models`, `available_models`, `filtered_panel`, `panel_len`, `state_mut`, `id_at`, `context_limit`, `context_used`, `model_supports_reasoning`, `reasoning_of`, `cycle_reasoning_focused`, `move_model_selection`, `toggle_model_focus`, `toggle_favorite_focused`, `clamp_selection`, `confirm_model`, `pick_model_at`, `pick_model`, `clear_memory_model`.

- [ ] **Step 1:** Cut lines 1821-2019 and 2148-2251 (per grep boundaries: 1821, 1828, 1833, 1854, 1861, 1868, 1886, 1891, 1895, 1916, 1923, 1930, 1939, 1953, 1977, 1983, 1989 — ending before `open_settings` at 2020 — then 2148, 2159, 2168, 2186, 2198, 2208, 2220, 2245 — ending before `confirm_session` at 2252, which already moved to `sessions.rs` in Task 7).
- [ ] **Step 2:** In `src/app/models.rs`:
```rust
use anyhow::Result;
use ratatui::widgets::ListState;

use super::{App, ModelPanel};
use crate::provider::Model;

impl App {
    // paste all 25 methods here
}
```
- [ ] **Step 3:** `cargo build`, fix imports.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/models.rs
git commit -m "refactor: extract app/models.rs"
```

### Task 9: Extract `app/settings.rs`

**Files:**
- Create: `src/app/settings.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `open_settings`, `settings_field`, `move_settings_selection`, `toggle_settings_field`, `toggle_reasoning_view`, `settings_input_char`, `settings_input_backspace`, `save_settings`.

- [ ] **Step 1:** Cut lines 2020-2147 (`open_settings` through `save_settings`, per grep: 2020, 2034, 2038, 2043, 2063, 2079, 2091, 2116 — ending before `move_model_selection` at 2148, already moved in Task 8).
- [ ] **Step 2:** In `src/app/settings.rs`:
```rust
use anyhow::Result;

use super::{App, SettingsField};

impl App {
    // paste all 8 methods here
}
```
- [ ] **Step 3:** `cargo build`, fix imports.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/settings.rs
git commit -m "refactor: extract app/settings.rs"
```

### Task 10: Extract `app/skills_popup.rs`

**Files:**
- Create: `src/app/skills_popup.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `open_skills_popup`, `reload_skills`, `move_skills_selection`, `start_skill_install`, `start_skill_remove`, `confirm_skill_install`, `on_skill_install_result`, `confirm_skill_remove`, `skill_edit_path_for_selected`.
- Note: this file is distinct from the existing top-level `src/skills.rs` (skill file I/O) — this one is only the popup UI state machine.

- [ ] **Step 1:** Cut lines 789-870 (per grep: 789, 795, 801, 811, 816, 824, 844, 856, 867 — ending before `copy_text` at 872).
- [ ] **Step 2:** In `src/app/skills_popup.rs`:
```rust
use super::App;

impl App {
    // paste all 9 methods here
}
```
- [ ] **Step 3:** `cargo build`, fix imports (likely `crate::skills`).
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/skills_popup.rs
git commit -m "refactor: extract app/skills_popup.rs"
```

### Task 11: Extract `app/copy.rs`

**Files:**
- Create: `src/app/copy.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Produces: `impl App` methods `copy_text`, `copy_message`, `open_copy_menu`, `confirm_copy`, `move_copy_selection`.

- [ ] **Step 1:** Cut lines 872-943 (per grep: 872, 891, 905, 930, 937 — ending before `send_message` at 944, already moved in Task 3).
- [ ] **Step 2:** In `src/app/copy.rs`:
```rust
use super::App;

impl App {
    // paste all 5 methods here
}
```
- [ ] **Step 3:** `cargo build`, fix imports (`arboard::Clipboard` if referenced directly).
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/copy.rs
git commit -m "refactor: extract app/copy.rs"
```

### Task 12: Extract `app/tests.rs`

**Files:**
- Create: `src/app/tests.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Consumes: everything now spread across `app/mod.rs` + the 9 submodules — the test module uses `App` and its methods exactly as before, so a blanket `use super::*;` covers it since all submodule methods are still inherent `impl App` methods visible on any `App` value.

- [ ] **Step 1:** Cut the entire `#[cfg(test)] mod tests { ... }` block (originally lines 2430-3164) out of `app/mod.rs`.
- [ ] **Step 2:** In `src/app/tests.rs`, start with `use super::*;` followed by the full pasted test module body (drop the outer `mod tests { }` wrapper — the file itself is the module body now, since `app/mod.rs` already declares `#[cfg(test)] mod tests;` from Task 2 Step 2).
- [ ] **Step 3:** `cargo build`.
- [ ] **Step 4:** `cargo test` — expect the exact same test names to run and pass as before (`cargo test --lib -- --list` before and after this task should show an identical list).
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/tests.rs
git commit -m "refactor: extract app/tests.rs"
```

**End of Phase 1 checkpoint:** `app/mod.rs` should now be roughly 400-500 lines (struct + enums + 14 core methods + module declarations). Run `wc -l src/app/*.rs` and confirm no single file exceeds ~350 lines.

---

## Phase 2 — Small over-engineering cuts from the audit

### Task 13: Remove dead `Session.space_id` field

**Files:**
- Modify: `src/db.rs:36-37` (and wherever `space_id` is `SELECT`ed/bound in the surrounding session-row mapping code in the same file)

**Interfaces:**
- Produces: `Session` struct with `space_id` removed.

- [ ] **Step 1:** Grep the codebase for `space_id` to confirm it's genuinely unread outside its own field/column definition: `grep -rn space_id src/`.
- [ ] **Step 2:** If confirmed dead (only appears in the struct field, the `#[allow(dead_code)]` attribute, and the SQL `SELECT`/row-mapping for that one column), remove the struct field, its attribute, and drop the column from the `SELECT`/`query_row` mapping in `db.rs`. Do not touch the actual SQLite table schema/migration (removing a column from a live schema is out of scope for this refactor).
- [ ] **Step 3:** `cargo build` — fix any compile errors from the removed field (should be none, since it was unread).
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/db.rs
git commit -m "refactor: remove dead Session.space_id field"
```

### Task 14: De-duplicate `line_text`

**Files:**
- Modify: `src/markdown.rs:466-468` (delete)
- Modify: `src/ui.rs:227-229` (make `pub(crate)`)
- Modify: any call site inside `markdown.rs` that used its local copy

**Interfaces:**
- Produces: single `pub(crate) fn line_text(line: &Line) -> String` in `ui.rs`, used by both `ui.rs` and `markdown.rs`.

- [ ] **Step 1:** In `src/ui.rs`, change `fn line_text` (227) to `pub(crate) fn line_text`.
- [ ] **Step 2:** In `src/markdown.rs`, delete the duplicate `line_text` definition (466-468) and add `use crate::ui::line_text;` near the top of the file.
- [ ] **Step 3:** `cargo build` — fix any visibility/import errors.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/ui.rs src/markdown.rs
git commit -m "refactor: de-duplicate line_text between ui.rs and markdown.rs"
```

### Task 15: Replace hand-rolled `percent_decode` with `reqwest::Url`

**Files:**
- Modify: `src/tools.rs:322` (the `percent_decode` function and its call site)

**Interfaces:**
- Produces: same behavior (decoding one query param), implemented via `reqwest::Url::parse(...).query_pairs()` instead of hand-rolled percent-decoding.

- [ ] **Step 1:** Read the current `percent_decode` function and its single call site in `src/tools.rs` to see exactly which query param it decodes and from what URL string.
- [ ] **Step 2:** Replace the call site to construct a `reqwest::Url` from the full URL string and read the target param via `.query_pairs().find(|(k, _)| k == "paramname").map(|(_, v)| v.into_owned())`, matching the original function's return type/behavior exactly (same `Option<String>` or `String` shape — check the original signature first).
- [ ] **Step 3:** Delete the now-unused `percent_decode` function.
- [ ] **Step 4:** `cargo build`, `cargo test` — pass unchanged. If `tools.rs` has a unit test covering percent-decoding specifically, verify it still passes against the new implementation.
- [ ] **Step 5:** Commit:
```bash
git add src/tools.rs
git commit -m "refactor: use reqwest::Url::query_pairs instead of hand-rolled percent_decode"
```

---

## Phase 3 — Dedup shared helpers

### Task 16: Shared bounded-cursor helper replacing 5 `move_*_selection` functions

**Files:**
- Modify: `src/app/spaces.rs` (`move_space_selection`), `src/app/sessions.rs` (`move_session_selection`), `src/app/skills_popup.rs` (`move_skills_selection`), `src/app/copy.rs` (`move_copy_selection`), `src/app/settings.rs` (`move_settings_selection`)
- Create: helper in `src/app/mod.rs` (private, crate-internal utility — not a new file, since it's a 3-line function used only by `impl App` methods)

**Interfaces:**
- Produces: `fn clamp_cursor(current: usize, len: usize, delta: i32) -> usize` in `app/mod.rs`, `pub(super)` so all submodules can call it.

- [ ] **Step 1:** Read all 5 `move_*_selection` bodies to confirm they share the exact same clamp/wrap arithmetic (per the audit: `clamp(0, len-1)` or `rem_euclid(n)` over an index bounded by a `Vec` length). If any diverge (e.g. one wraps and another clamps), keep that one's behavior distinct and only unify the ones that genuinely match — do not change behavior to force unification.
- [ ] **Step 2:** In `src/app/mod.rs`, add:
```rust
pub(super) fn clamp_cursor(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    ((current as i32 + delta).rem_euclid(len as i32)) as usize
}
```
(Adjust the exact formula to match whichever behavior Step 1 confirmed is shared — if it's a hard clamp rather than wraparound, use `.clamp(0, len as i32 - 1)` instead of `.rem_euclid`.)
- [ ] **Step 3:** Rewrite each of the 5 `move_*_selection` methods to a one-liner calling `super::clamp_cursor(self.x_selected, self.x_list.len(), delta)` and assigning the result back to the selection field, preserving any extra side effects (e.g. `state_mut` refresh) each one currently has after the index update.
- [ ] **Step 4:** `cargo build`, `cargo test` — all selection-movement tests (fav/available panel cycling, settings field cycling, etc.) must pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/spaces.rs src/app/sessions.rs src/app/skills_popup.rs src/app/copy.rs src/app/settings.rs
git commit -m "refactor: unify bounded-cursor selection movement into clamp_cursor"
```

### Task 17: Shared clipboard-copy helper

**Files:**
- Modify: `src/app/copy.rs` (`copy_text`)
- Modify: `src/input.rs` (`copy_selection`, `copy_selection_live`)

**Interfaces:**
- Produces: `pub(crate) fn copy_to_clipboard(clipboard: &mut Option<arboard::Clipboard>, text: &str) -> String` in `src/input.rs` (or a new small `src/clipboard.rs` if `input.rs` isn't a natural home — prefer `input.rs` since it already owns `copy_selection`), returning the status-line message (`"copied {n} chars"` / `"clipboard unavailable"`), used by all 3 call sites.

- [ ] **Step 1:** Read `copy_text` (app/copy.rs), `copy_selection`, and `copy_selection_live` (input.rs) to confirm the shared shape: empty-check → char count → `clipboard.as_mut().is_some_and(|cb| cb.set_text(text).is_ok())` → format message.
- [ ] **Step 2:** In `src/input.rs`, add:
```rust
pub(crate) fn copy_to_clipboard(clipboard: &mut Option<arboard::Clipboard>, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let n = text.chars().count();
    if clipboard.as_mut().is_some_and(|cb| cb.set_text(text.to_string()).is_ok()) {
        format!("copied {n} chars")
    } else {
        "clipboard unavailable".to_string()
    }
}
```
(Match the exact empty-input behavior of the original 3 functions — check whether they return early with a specific message or empty string before finalizing this signature.)
- [ ] **Step 3:** Rewrite `copy_text`, `copy_selection`, `copy_selection_live` to call `copy_to_clipboard(&mut self.clipboard, &text)` and assign the result to `self.status`, keeping each function's own text-selection logic (what text to copy) intact — only the copy+status-format tail is shared.
- [ ] **Step 4:** `cargo build`, `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/input.rs src/app/copy.rs
git commit -m "refactor: unify clipboard-copy-and-status-message into copy_to_clipboard"
```

### Task 18: Shared fuzzy-filter-and-sort helper

**Files:**
- Modify: `src/app/spaces.rs` (`filtered_spaces`), `src/app/sessions.rs` (`filtered_sessions`, `session_score`)

**Interfaces:**
- Produces: a generic helper in `src/app/mod.rs`: `pub(super) fn fuzzy_filter_sorted<'a, T>(items: &'a [T], needle: &str, score_fn: impl Fn(&T) -> Option<i32>) -> Vec<&'a T>` that filters by `score_fn` returning `Some`, sorts descending by score, used by both `filtered_spaces` and `filtered_sessions`.

- [ ] **Step 1:** Read `filtered_spaces` and `filtered_sessions` (with `session_score`) to confirm both do: map each item through a scoring function, keep `Some` scores, sort descending by score.
- [ ] **Step 2:** In `src/app/mod.rs`:
```rust
pub(super) fn fuzzy_filter_sorted<'a, T>(
    items: &'a [T],
    score_fn: impl Fn(&T) -> Option<i32>,
) -> Vec<&'a T> {
    let mut scored: Vec<(&T, i32)> = items
        .iter()
        .filter_map(|item| score_fn(item).map(|s| (item, s)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().map(|(item, _)| item).collect()
}
```
- [ ] **Step 3:** Rewrite `filtered_spaces` to call `super::fuzzy_filter_sorted(&self.spaces_cache, |s| crate::selection::fuzzy_score(&s.name, &self.space_filter))` (match the actual existing scoring call — check what `filtered_spaces` currently calls for scoring before finalizing this line). Rewrite `filtered_sessions` similarly using `session_score`.
- [ ] **Step 4:** `cargo build`, `cargo test` — space/session filter tests must pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add src/app/mod.rs src/app/spaces.rs src/app/sessions.rs
git commit -m "refactor: unify space/session fuzzy-filter-and-sort into fuzzy_filter_sorted"
```

### Task 19: Shared request→parse→map pipeline for search backends

**Files:**
- Modify: `src/tools.rs` (`searxng_search`, `langsearch_search`, `duckduckgo_search`)

**Interfaces:**
- Produces: no new shared function if response shapes genuinely differ per backend (confirm first) — otherwise a helper `async fn fetch_and_parse<T: DeserializeOwned>(client: &reqwest::Client, req: reqwest::RequestBuilder) -> Result<T>` doing `.send().await?.error_for_status()?.json::<T>().await` (or `.text()` if not all are JSON), with each backend keeping its own response-to-`Vec<SearchHit>` mapping.

- [ ] **Step 1:** Read all 3 functions (`tools.rs:155`, `200`, `225`) to confirm exactly what's shared (send + error_for_status + parse) vs. backend-specific (request construction, response shape, hit mapping).
- [ ] **Step 2:** Extract only the confirmed-shared send+status-check+parse span into a small `async fn send_and_parse<T: serde::de::DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T>` in `tools.rs`, calling `req.send().await?.error_for_status()?.json::<T>().await.map_err(Into::into)`.
- [ ] **Step 3:** Rewrite each of the 3 search functions to build their request, call `send_and_parse::<TheirResponseType>(req).await?`, then keep their own existing hit-mapping code unchanged.
- [ ] **Step 4:** `cargo build`, `cargo test` — pass unchanged. If these functions aren't covered by unit tests (network calls), leave them as-is functionally and verify only via `cargo build`.
- [ ] **Step 5:** Commit:
```bash
git add src/tools.rs
git commit -m "refactor: unify search-backend request/parse pipeline"
```

**End of Phase 3 checkpoint:** re-run the audit's duplication list mentally — items 1, 2, 7, 8 remain (popup state machine, popup renderer, CRUD pattern) and are addressed in Phase 4 alongside the ui.rs/events.rs split, since colocating the popup code is a prerequisite for writing that shared helper cleanly.

---

## Phase 4 — Colocate popup code into `ui/popups/`

This phase moves each popup's `render_*` (from `ui.rs`), `handle_*_popup` (from `events.rs`), and mode enum (already in `app/mod.rs` from Phase 1 — stays there, since `Popup`/`SessionMode`/etc. are `App` state, not UI-only) into one file per popup under `src/ui/popups/`.

### Task 20: Create `ui/mod.rs` + `ui/history.rs` skeleton

**Files:**
- Create: `src/ui/mod.rs`, `src/ui/history.rs`
- Delete: `src/ui.rs` (after content moved)

**Interfaces:**
- Produces: `pub fn render(f: &mut Frame, app: &mut App)` stays the public entry point in `ui/mod.rs`, calling into `history::render_history`/`history::render_welcome` and `popups::*::render_*`.

- [ ] **Step 1:** Create `src/ui/` directory and `src/ui/popups/` subdirectory.
- [ ] **Step 2:** Move `src/ui.rs` wholesale to `src/ui/mod.rs` first (same reasoning as Task 2 — Rust can't have both `ui.rs` and `ui/mod.rs`). `cargo build`, `cargo test` — pass unchanged. Commit: `git add -A && git commit -m "refactor: move ui.rs to ui/mod.rs"`.
- [ ] **Step 3:** Cut `render_history` (165-199), `render_welcome` (201-225), `wrap_conversation` (237-257), `push_user` (271-291), `push_assistant_stored` (293-341), `push_assistant_streaming` (343-370), `push_rendered` (372-384), `wrap_plain` (386-395) into `src/ui/history.rs`, with `use super::{line_text, dim, dot};` (those 3 stay in `ui/mod.rs` as shared small helpers used by both history and popups) and `use crate::app::App;`.
- [ ] **Step 4:** In `ui/mod.rs`, add `mod history;` and `pub(crate) use history::{render_history, render_welcome};` (or call `history::render_history(...)` directly from `render()` with full path — either works, pick whichever needs fewer import changes elsewhere).
- [ ] **Step 5:** `cargo build`, fix imports, `cargo test` — pass unchanged.
- [ ] **Step 6:** Commit:
```bash
git add -A
git commit -m "refactor: extract ui/history.rs"
```

### Task 21: Colocate the session popup into `ui/popups/session.rs`

**Files:**
- Create: `src/ui/popups/session.rs`
- Modify: `src/ui/mod.rs` (remove `render_session_popup`, `fmt_created`, `truncate` if only used here — check `truncate` isn't shared before moving it)
- Modify: `src/events.rs` (remove `handle_session_popup`)

**Interfaces:**
- Produces: `pub(crate) fn render(f: &mut Frame, app: &App)` and `pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()>` in `ui/popups/session.rs`, called from `ui/mod.rs`'s dispatcher and `events.rs`'s key-routing respectively as `popups::session::render(f, app)` / `popups::session::handle_key(app, key)`.

- [ ] **Step 1:** Cut `render_session_popup` (ui.rs/mod.rs: 632-691) and `fmt_created` (694-699) into `src/ui/popups/session.rs`. Check whether `truncate` (701-708) is used only by the session popup or also by other popups/history — grep `truncate(` across `ui/mod.rs`; if only the session popup uses it, move it too, otherwise leave it in `ui/mod.rs` as a shared helper.
- [ ] **Step 2:** Cut `handle_session_popup` (events.rs: 476-510) into the same `src/ui/popups/session.rs` file.
- [ ] **Step 3:** In `src/ui/popups/session.rs`, rename `render_session_popup` → `pub(crate) fn render` and `handle_session_popup` → `pub(crate) fn handle_key`, add needed imports (`ratatui::Frame`, `crossterm::event::KeyEvent`, `anyhow::Result`, `crate::app::App`, and whatever `super::{centered, panel_list, hint_title}` shared UI helpers it calls).
- [ ] **Step 4:** In `src/ui/mod.rs`, add `pub mod popups;` (or `mod popups;` with `pub(crate)` items inside — since only `events.rs` and `ui/mod.rs` itself need to reach in, `pub(crate) mod popups;` is enough) and update the `render()` dispatcher's call site from `render_session_popup(f, app)` to `popups::session::render(f, app)`.
- [ ] **Step 5:** In `src/events.rs`, update the call site from `handle_session_popup(app, key)` to `crate::ui::popups::session::handle_key(app, key)`.
- [ ] **Step 6:** `cargo build`, fix visibility (`centered`, `panel_list`, `hint_title`, `dim`, `dot` likely need `pub(super)` or `pub(crate)` in `ui/mod.rs` since popups now live one level deeper).
- [ ] **Step 7:** `cargo test` — pass unchanged.
- [ ] **Step 8:** Commit:
```bash
git add -A
git commit -m "refactor: colocate session popup into ui/popups/session.rs"
```

### Task 22: Colocate the space popup into `ui/popups/space.rs`

**Files:**
- Create: `src/ui/popups/space.rs`
- Modify: `src/ui/mod.rs`, `src/events.rs`

**Interfaces:**
- Produces: `pub(crate) fn render(f: &mut Frame, app: &App)` and `pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()>` in `ui/popups/space.rs`.

- [ ] **Step 1:** Cut `render_space_popup` (ui/mod.rs: 710-768) and `handle_space_popup` (events.rs: 512-553) into `src/ui/popups/space.rs`, renamed to `render`/`handle_key` as in Task 21.
- [ ] **Step 2:** Update call sites in `ui/mod.rs`'s `render()` dispatcher and `events.rs`'s key routing, same pattern as Task 21 Steps 4-5.
- [ ] **Step 3:** `cargo build`, fix imports/visibility.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add -A
git commit -m "refactor: colocate space popup into ui/popups/space.rs"
```

### Task 23: Colocate the skills popup into `ui/popups/skills.rs`

**Files:**
- Create: `src/ui/popups/skills.rs`
- Modify: `src/ui/mod.rs`, `src/events.rs`

**Interfaces:**
- Produces: `pub(crate) fn render(f: &mut Frame, app: &App)` and `pub(crate) fn handle_key(app: &mut App, key: KeyEvent)` (note: `handle_skills_popup` returns `()` not `Result`, per events.rs:555 — keep that signature) in `ui/popups/skills.rs`.

- [ ] **Step 1:** Cut `render_skills_popup` (ui/mod.rs: 770-816) and `handle_skills_popup` (events.rs: 555-582) into `src/ui/popups/skills.rs`, renamed `render`/`handle_key`.
- [ ] **Step 2:** Update call sites, same pattern as Task 21.
- [ ] **Step 3:** `cargo build`, fix imports/visibility.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add -A
git commit -m "refactor: colocate skills popup into ui/popups/skills.rs"
```

### Task 24: Colocate the model popup into `ui/popups/model.rs`

**Files:**
- Create: `src/ui/popups/model.rs`
- Modify: `src/ui/mod.rs`, `src/events.rs`

**Interfaces:**
- Produces: `pub(crate) fn render(f: &mut Frame, app: &mut App)`, `pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()>` in `ui/popups/model.rs`. Also moves `model_items`, `panel_list`, `model_popup_areas`, `list_inner` here if solely used by the model popup (check `panel_list` isn't reused by other popups' rendering before moving — per the audit finding #2, other popups build their own `List` inline rather than via `panel_list`, so it's likely model-popup-only; confirm with grep before moving).

- [ ] **Step 1:** Grep `panel_list(` and `model_popup_areas(` and `list_inner(` across `ui/mod.rs` to confirm which are model-popup-exclusive vs shared.
- [ ] **Step 2:** Cut `render_model_popup` (567-595), `model_items` (597-617), and any of `panel_list`/`model_popup_areas`/`list_inner` confirmed exclusive to the model popup, plus `handle_model_popup` (events.rs: 297-328), into `src/ui/popups/model.rs`, renamed `render`/`handle_key`.
- [ ] **Step 3:** Update call sites.
- [ ] **Step 4:** `cargo build`, fix imports/visibility.
- [ ] **Step 5:** `cargo test` — pass unchanged.
- [ ] **Step 6:** Commit:
```bash
git add -A
git commit -m "refactor: colocate model popup into ui/popups/model.rs"
```

### Task 25: Colocate the settings popup into `ui/popups/settings.rs`

**Files:**
- Create: `src/ui/popups/settings.rs`
- Modify: `src/ui/mod.rs`, `src/events.rs`

**Interfaces:**
- Produces: `pub(crate) fn render(f: &mut Frame, app: &App)`, `pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()>` in `ui/popups/settings.rs`.

- [ ] **Step 1:** Cut `render_settings_popup` (49-138) and `handle_settings_popup` (events.rs: 584-609) into `src/ui/popups/settings.rs`, renamed `render`/`handle_key`.
- [ ] **Step 2:** Update call sites.
- [ ] **Step 3:** `cargo build`, fix imports/visibility.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add -A
git commit -m "refactor: colocate settings popup into ui/popups/settings.rs"
```

### Task 26: Colocate the copy menu into `ui/popups/copy.rs`

**Files:**
- Create: `src/ui/popups/copy.rs`
- Modify: `src/ui/mod.rs`, `src/events.rs`

**Interfaces:**
- Produces: `pub(crate) fn render(f: &mut Frame, app: &App)`, `pub(crate) fn handle_key(app: &mut App, key: KeyEvent)` in `ui/popups/copy.rs`.

- [ ] **Step 1:** Cut `render_copy_popup` (870-893) and `handle_copy_popup` (events.rs: 466-474) into `src/ui/popups/copy.rs`, renamed `render`/`handle_key`.
- [ ] **Step 2:** Update call sites.
- [ ] **Step 3:** `cargo build`, fix imports/visibility.
- [ ] **Step 4:** `cargo test` — pass unchanged.
- [ ] **Step 5:** Commit:
```bash
git add -A
git commit -m "refactor: colocate copy menu into ui/popups/copy.rs"
```

### Task 27: Colocate the key prompt and context popup into `ui/popups/key.rs` and `ui/popups/context.rs`

**Files:**
- Create: `src/ui/popups/key.rs`, `src/ui/popups/context.rs`
- Modify: `src/ui/mod.rs`, `src/events.rs`

**Interfaces:**
- Produces: `render`/`handle_key` pairs in each new file, same pattern as prior tasks.

- [ ] **Step 1:** Cut `render_key_popup` (152-163) and `handle_key_popup` (events.rs: 611-621, if present — verify exact end line by reading the file, since it was the last function in the earlier grep and its end wasn't bounded by a following `fn`) into `src/ui/popups/key.rs`, renamed `render`/`handle_key`.
- [ ] **Step 2:** Cut `render_context_popup` (818-868) and `handle_context_popup` (events.rs: 179-186) into `src/ui/popups/context.rs`, renamed `render`/`handle_key`.
- [ ] **Step 3:** Update call sites for both in `ui/mod.rs` and `events.rs`.
- [ ] **Step 4:** `cargo build`, fix imports/visibility.
- [ ] **Step 5:** `cargo test` — pass unchanged.
- [ ] **Step 6:** Commit:
```bash
git add -A
git commit -m "refactor: colocate key prompt and context popup into ui/popups/"
```

**End of Task 27 checkpoint:** `ui/mod.rs` should now contain only `render()`, the shared small helpers (`centered`, `truncate` if shared, `line_text`, `dim`, `dot`, `hint_title`, `gradient`, `humanize`, `context_label`), and the input/status/context-bar rendering (`render_input`, `render_command_popup`, `render_context_bar`, `render_status`) that isn't popup-specific. `events.rs` should contain only `run`, `handle_key`, `handle_normal`, `handle_input_mouse`, `composer_jump`, `handle_mouse`, and the 3 `*_edit_target` helpers — all the `handle_*_popup` functions have moved out.

### Task 28: Shared browse/edit/confirm-delete state-machine helper

**Files:**
- Modify: `src/ui/popups/session.rs`, `src/ui/popups/space.rs`, `src/ui/popups/skills.rs`

**Interfaces:**
- Produces: a helper in a new small `src/ui/popups/mod.rs` addition (the `popups` module's own `mod.rs`, created alongside Task 21 if it wasn't already — check whether `src/ui/popups/` needs an explicit `mod.rs` or whether `ui/mod.rs`'s `mod popups;` + `src/ui/popups.rs` suffices; since this task adds shared code *within* the popups module, prefer `src/ui/popups/mod.rs` with `mod session; mod space; mod skills; mod model; mod settings; mod copy; mod key; mod context;` and the shared helper defined there): `pub(super) enum BrowseAction { Close, Filter(char), Backspace, MoveUp, MoveDown, Create, Rename, ConfirmDelete, DeleteYes, DeleteNo, Edit(char), EditBackspace, EditSave, EditCancel }` plus `pub(super) fn classify_key(key: KeyEvent, supports_create: bool, supports_rename: bool) -> Option<BrowseAction>` that maps the shared key bindings (Esc/arrows/Ctrl+N/Ctrl+R/Ctrl+D/char) identically to what `handle_session_popup`/`handle_space_popup`/`handle_skills_popup` currently do.

- [ ] **Step 1:** Diff the 3 popup handlers' key-matching logic side by side (now colocated as `handle_key` in `session.rs`/`space.rs`/`skills.rs`) to write down the exact shared key→action mapping and where session/space/skills diverge (space has Ctrl+N create, skills has no rename, per the audit).
- [ ] **Step 2:** If `src/ui/popups/` doesn't yet have a `mod.rs` (check after Task 21-27 — `ui/mod.rs`'s `mod popups;` may point at a bare `popups.rs` file instead of a directory with `mod.rs`; if so, convert it to `src/ui/popups/mod.rs` first, moving its current 1-line content), add the `BrowseAction` enum and `classify_key` function there, `pub(super)` so the 3 popup files (siblings under `popups/`) can use it via `use super::{BrowseAction, classify_key};`.
- [ ] **Step 3:** Rewrite each of `session.rs`/`space.rs`/`skills.rs`'s `handle_key` to call `classify_key(key, supports_create, supports_rename)` and `match` on the returned `BrowseAction`, replacing their own inline key-matching for the shared cases while keeping any popup-specific post-action side effects (e.g. what `Close` actually does differs — session clears `session_filter`, space clears `space_filter`, etc. — that stays in each file's own match arms).
- [ ] **Step 4:** `cargo build`, `cargo test` — every existing popup-navigation test (rename, delete, filter, create) must pass unchanged; this is the highest-risk task in the plan since it touches control flow, not just code location, so re-read the diff carefully before committing.
- [ ] **Step 5:** Commit:
```bash
git add -A
git commit -m "refactor: unify session/space/skills popup key handling via classify_key"
```

---

## Final checkpoint

- [ ] Run `wc -l src/*.rs src/app/*.rs src/ui/*.rs src/ui/popups/*.rs` and confirm no file exceeds ~500 lines (down from `app.rs` at 3164).
- [ ] Run `cargo build 2>&1 | grep -i warning` — should be empty or only pre-existing warnings (compare against a build from the `e577dc1` baseline if unsure).
- [ ] Run `cargo test` one final time — full pass.
- [ ] Run `cargo run` briefly and smoke-test by hand: open a session, switch spaces, open the model picker, open settings, run `/copy` on a message — confirm no regression in any popup (this project is a TUI; automated tests don't cover rendering, so this manual pass is the only check on the `ui/popups/*.rs` split actually looking right on screen).
- [ ] Diff `git log --oneline e577dc1..HEAD` against this plan's task list to confirm every task landed as its own commit (useful for `git bisect` if a later regression surfaces).
