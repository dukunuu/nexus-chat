# Files Popup Directory Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the files popup's type-a-path Add flow with a navigable, fuzzy-filterable directory browser (Ctrl+N opens it at `~`); path-paste import keeps the existing prefilled Add mode.

**Architecture:** A new `FilesMode::Pick` drives a directory listing cached on `App` (`picker_dir`, `picker_entries`, `picker_filter`, `picker_selected`). Navigation logic lives in `src/app/files.rs`, rendering/keys in `src/ui/popups/files.rs`, reusing `fuzzy_score`, `fuzzy_filter_sorted`, `clamp_cursor`, and the existing popup list style. No new dependencies (ratatui-explorer rejected: version-lag risk vs ratatui 0.30, and the codebase already has list+fuzzy machinery).

**Tech Stack:** Rust, ratatui (existing), std::fs.

**User-approved design (from chat):** Enter descends into a dir or imports a file; Backspace erases filter, or goes up a directory when the filter is empty; typing fuzzy-filters entries; Esc returns to Browse; starts at the home directory; entries listed dirs-first.

## Global Constraints

- Minimum visibility that compiles per item (grep call sites before choosing).
- `cargo build` warning-free; `cargo test` passing after every task.
- Stage only exact touched files; never `git add -A`.
- No new dependencies.
- Path-paste detection (Add mode prefill + Enter imports) must keep working unchanged.
- Existing `import_file` is the single import entry point — the picker calls it, never copies files itself.

---

### Task 1: Picker state + navigation logic

**Files:**
- Modify: `src/app/mod.rs` (add `Pick` to `FilesMode`; App fields + initializers)
- Modify: `src/app/files.rs` (picker methods + tests)

**Interfaces:**
- Consumes: `import_file(&Path) -> Result<String>`, `fuzzy_score` (`crate::input`), `fuzzy_filter_sorted`/`clamp_cursor` (`super`), `FilesMode`.
- Produces (used by Task 2):
  - App fields: `pub picker_dir: std::path::PathBuf`, `pub picker_entries: Vec<PickerEntry>`, `pub picker_filter: String`, `pub picker_selected: usize`
  - `pub struct PickerEntry { pub name: String, pub is_dir: bool }` (in `src/app/files.rs`, `pub(crate)` if that compiles — grep)
  - `App::open_file_picker(&mut self)` — enter Pick mode at `picker_dir` (first open: home dir; afterwards: remembered)
  - `App::filtered_picker_entries(&self) -> Vec<&PickerEntry>` — fuzzy-filtered by `picker_filter`, stable dirs-first order when filter empty
  - `App::move_picker_selection(&mut self, delta: i32)`
  - `App::picker_filter_push(&mut self, c: char)` / `App::picker_backspace(&mut self)` (erase filter char; if filter empty, ascend to parent dir and reload)
  - `App::picker_enter(&mut self) -> anyhow::Result<()>` — descend into dir (clear filter, reload, selection 0) or `import_file` the file (status set by import; return to `FilesMode::Browse`)

- [ ] **Step 1: State**

In `src/app/mod.rs`: add `Pick` to `FilesMode`. Add fields (next to the other files popup fields) and initializers in `App::new`:

```rust
/// Directory the file-picker browser is showing (remembered across opens).
pub picker_dir: std::path::PathBuf,
pub picker_entries: Vec<crate::app::files::PickerEntry>,
pub picker_filter: String,
pub picker_selected: usize,
```

```rust
picker_dir: std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
picker_entries: Vec::new(),
picker_filter: String::new(),
picker_selected: 0,
```

Note: `std::env::home_dir()` is un-deprecated as of Rust 2024/1.85+ (edition 2024 is in use here). If the compiler still warns on this toolchain, use `directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())` — `directories` is already a dependency (see `src/config.rs`). Zero-warning build decides which.

`mod files;` must expose the entry type: declare `pub struct PickerEntry` in files.rs and reference it as `crate::app::files::PickerEntry` (adjust `mod files;` to `pub(crate) mod files;` ONLY if the field type's visibility forces it — try private first; the field is `pub` on `App`, so the type must be at least as visible as the field's users need: Task 2's ui code reads `picker_entries`, so `PickerEntry` needs `pub(crate)` reachability — likely `pub(crate) mod files;` or a re-export `pub(crate) use files::PickerEntry;` in mod.rs. Prefer the re-export, keep `mod files;` private.)

- [ ] **Step 2: Failing tests** (append to `src/app/files.rs` tests; reuse the existing `test_app()` helper in that file)

```rust
#[test]
fn picker_lists_dirs_first_descends_and_imports() {
    let mut a = test_app();
    let root = std::env::temp_dir().join(format!("nexus-pick-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("subdir")).unwrap();
    std::fs::write(root.join("bbb.txt"), "file b").unwrap();
    std::fs::write(root.join("aaa.txt"), "file a").unwrap();

    a.picker_dir = root.clone();
    a.open_file_picker();
    assert!(a.files_mode == crate::app::FilesMode::Pick);
    let names: Vec<&str> = a.filtered_picker_entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["subdir", "aaa.txt", "bbb.txt"]); // dirs first, then alpha

    // Enter on a dir descends and reloads.
    a.picker_selected = 0;
    a.picker_enter().unwrap();
    assert_eq!(a.picker_dir, root.join("subdir"));
    assert!(a.filtered_picker_entries().is_empty());

    // Backspace with empty filter ascends.
    a.picker_backspace();
    assert_eq!(a.picker_dir, root);

    // Enter on a file imports it and returns to Browse.
    let idx = a.filtered_picker_entries().iter().position(|e| e.name == "aaa.txt").unwrap();
    a.picker_selected = idx;
    a.picker_enter().unwrap();
    assert!(a.files_mode == crate::app::FilesMode::Browse);
    assert!(a.files_cache.iter().any(|f| f.name == "aaa.txt"));
}

#[test]
fn picker_filter_fuzzy_matches_and_backspace_edits_filter_first() {
    let mut a = test_app();
    let root = std::env::temp_dir().join(format!("nexus-pick-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("report-2026.pdf"), "x").unwrap();
    std::fs::write(root.join("notes.md"), "y").unwrap();
    a.picker_dir = root.clone();
    a.open_file_picker();

    a.picker_filter_push('r');
    a.picker_filter_push('p');
    a.picker_filter_push('t');
    let names: Vec<&str> = a.filtered_picker_entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["report-2026.pdf"]); // fuzzy subsequence "rpt"

    // Backspace edits the filter (does NOT ascend while filter non-empty).
    a.picker_backspace();
    assert_eq!(a.picker_filter, "rp");
    assert_eq!(a.picker_dir, root);
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test picker` → compile FAIL (missing methods/fields).

- [ ] **Step 4: Implement** (in `src/app/files.rs`)

```rust
/// One row of the file-picker browser.
pub struct PickerEntry {
    pub name: String,
    pub is_dir: bool,
}

impl App {
    /// Enter the picker at `picker_dir` (home on first open, remembered after).
    pub(crate) fn open_file_picker(&mut self) {
        self.picker_filter.clear();
        self.picker_selected = 0;
        self.reload_picker_entries();
        self.files_mode = super::FilesMode::Pick;
    }

    /// Re-read the current directory: dirs first, then files, both alphabetical.
    /// Unreadable dirs just yield an empty list (status explains).
    fn reload_picker_entries(&mut self) {
        let mut entries: Vec<PickerEntry> = std::fs::read_dir(&self.picker_dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = e.file_type().ok()?.is_dir();
                        Some(PickerEntry { name, is_dir })
                    })
                    .collect()
            })
            .unwrap_or_else(|e| {
                self.status = format!("cannot read {}: {e}", self.picker_dir.display());
                Vec::new()
            });
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        self.picker_entries = entries;
    }

    /// Entries matching the fuzzy filter (all of them, dirs first, when empty).
    pub fn filtered_picker_entries(&self) -> Vec<&PickerEntry> {
        let needle = self.picker_filter.trim();
        if needle.is_empty() {
            return self.picker_entries.iter().collect();
        }
        use crate::input::fuzzy_score;
        super::fuzzy_filter_sorted(&self.picker_entries, |e| fuzzy_score(&e.name, needle))
    }

    pub fn move_picker_selection(&mut self, delta: i32) {
        self.picker_selected =
            super::clamp_cursor(self.picker_selected, self.filtered_picker_entries().len(), delta);
    }

    pub fn picker_filter_push(&mut self, c: char) {
        self.picker_filter.push(c);
        self.picker_selected = 0;
    }

    /// Backspace erases the filter first; on an empty filter it goes up a level.
    pub fn picker_backspace(&mut self) {
        if !self.picker_filter.is_empty() {
            self.picker_filter.pop();
            self.picker_selected = 0;
            return;
        }
        if let Some(parent) = self.picker_dir.parent().map(|p| p.to_path_buf()) {
            self.picker_dir = parent;
            self.picker_selected = 0;
            self.reload_picker_entries();
        }
    }

    /// Enter descends into a directory, or imports the selected file.
    pub fn picker_enter(&mut self) -> Result<()> {
        let Some(entry) = self.filtered_picker_entries().get(self.picker_selected) else {
            return Ok(());
        };
        let name = entry.name.clone();
        let is_dir = entry.is_dir;
        let path = self.picker_dir.join(&name);
        if is_dir {
            self.picker_dir = path;
            self.picker_filter.clear();
            self.picker_selected = 0;
            self.reload_picker_entries();
            return Ok(());
        }
        match self.import_file(&path) {
            Ok(n) => self.status = format!("imported {n}"),
            Err(e) => self.status = format!("import failed: {e}"),
        }
        self.files_mode = super::FilesMode::Browse;
        Ok(())
    }
}
```

Re-export in `src/app/mod.rs` near the other use lines: `pub(crate) use files::PickerEntry;` (narrow if a tighter level compiles).

- [ ] **Step 5: Run tests** — `cargo test` all pass, `cargo build` zero warnings. (`FilesMode::Pick` and the new methods are exercised by tests only until Task 2 — if dead_code fires on any of them, use the sanctioned `#[allow(dead_code)] // used from Task 2 of the file-picker plan; remove with first caller`.)

- [ ] **Step 6: Commit**

```bash
git add src/app/mod.rs src/app/files.rs
git commit -m "feat: file-picker navigation state for the files popup"
```

---

### Task 2: Picker UI + key wiring

**Files:**
- Modify: `src/ui/popups/files.rs` (render Pick mode; handle_key Pick arm; Ctrl+N opens picker)
- Modify: `src/input.rs` (paste in Pick mode appends to `picker_filter`)

**Interfaces:**
- Consumes: everything Task 1 produces; `crate::ui::hint_title`; existing `classify_edit_key` NOT used here (picker keys are bespoke: plain chars filter, Backspace is dual-purpose).

- [ ] **Step 1: Render** — in `render()`, add a Pick branch. Entries list replaces the files list while picking:

```rust
FilesMode::Pick => {
    let entries = app.filtered_picker_entries();
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            if e.is_dir {
                ListItem::new(Line::from(Span::styled(
                    format!("{}/", e.name),
                    Style::default().fg(Color::Cyan),
                )))
            } else {
                ListItem::new(Line::from(Span::styled(e.name.clone(), Style::default().fg(Color::White))))
            }
        })
        .collect();
    let title = format!(
        " {} — filter: {}▏  (Enter open/import · Backspace up · Esc cancel) ",
        app.picker_dir.display(),
        app.picker_filter,
    );
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !entries.is_empty() {
        state.select(Some(app.picker_selected.min(entries.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
    return;
}
```

Structure note: the existing `render` builds the files list unconditionally then matches on mode for the title — restructure minimally so Pick short-circuits before the files-list build (e.g. move the Pick branch to the top after `area`/`Clear`). Keep the Add/ConfirmDelete/Browse paths byte-identical in behavior.

- [ ] **Step 2: Keys** — in `handle_key`, add the Pick arm (before or after the Add arm):

```rust
FilesMode::Pick => match key.code {
    KeyCode::Esc => app.files_mode = FilesMode::Browse,
    KeyCode::Enter => app.picker_enter()?,
    KeyCode::Backspace => app.picker_backspace(),
    KeyCode::Up => app.move_picker_selection(-1),
    KeyCode::Down => app.move_picker_selection(1),
    KeyCode::Char(c) if !key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
        app.picker_filter_push(c)
    }
    _ => {}
},
```

And switch Browse's `BrowseAction::Create` arm from `app.start_files_add()` to `app.open_file_picker()`. `start_files_add` keeps its caller in path-paste detection (src/input.rs) — do NOT remove it; verify with grep + zero-warning build.

- [ ] **Step 3: Paste routing** — in `src/input.rs` `App::paste`, add above the Add arm:

```rust
Popup::Files if self.files_mode == crate::app::FilesMode::Pick => {
    for c in text.chars().filter(|c| !c.is_control()) {
        self.picker_filter_push(c);
    }
}
```

Plus test:

```rust
#[test]
fn paste_in_picker_mode_feeds_the_filter() {
    let mut a = test_app();
    a.popup = crate::app::Popup::Files;
    a.files_mode = crate::app::FilesMode::Pick;
    a.paste("doc");
    assert_eq!(a.picker_filter, "doc");
}
```

- [ ] **Step 4: Remove any Task-1 temporary allows** now that the UI calls the picker methods. `cargo build` zero warnings; `cargo test` all pass.

- [ ] **Step 5: Commit**

```bash
git add src/ui/popups/files.rs src/input.rs src/app/files.rs src/app/mod.rs
git commit -m "feat: directory browser replaces typed paths in the files popup"
```

---

### Task 3: Verification pass

- [ ] `cargo build` (zero warnings) + `cargo test` (all passing; report count).
- [ ] Greps: `start_files_add` still has exactly one production caller (path-paste in input.rs); no leftover `used from Task N` allows from this plan.
- [ ] Manual smoke (needs human): `/files` → Ctrl+N → browse from home, type to filter, Enter into a dir, Backspace up, Enter on a file → imported with status; paste a path into the composer → Add-mode prefill still works.
