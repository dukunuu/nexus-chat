# Scanned-PDF OCR Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scanned PDFs (empty text extraction) get OCR'd in the background via local `pdftoppm` + `tesseract`, so their text lands in the FTS index like any other fileset file.

**Architecture:** `extract::ocr_pdf` shells out to `pdftoppm` (render pages to PNG at 300dpi gray) then `tesseract` per page, joining pages with `[page N]` marker lines (same inline-marker convention as pptx `[slide N]` — `chunk_lines` stays unchanged and locations stay `lines X-Y`). `rescan_files` marks empty-extraction PDFs `"ocr…"` and queues them; a `spawn_blocking` task OCRs sequentially and reports per-file results over an mpsc channel as `AppEvent::Ocr`, handled by `on_ocr_done` which writes chunks/status to db and refreshes the cache if the space is still active.

**Tech Stack:** Rust, tokio (`spawn_blocking`, unbounded mpsc), std::process::Command, rusqlite (existing files/file_chunks tables), external `pdftoppm` + `tesseract` binaries.

## Global Constraints

- OCR engine: local `tesseract` + `pdftoppm`, shelled out. Never an API call.
- Trigger: automatic — a `.pdf` whose extraction returns empty text. Non-PDF empty files keep the existing `"no text (scanned?)"` status.
- OCR must not block the UI thread: background task, sequential within one batch, at most one batch in flight (`ocr_rx.is_some()` = in flight).
- Statuses (exact strings): in-progress `"ocr…"`; OCR found nothing `"no text (ocr found nothing)"`; missing binaries (either one) `"scanned pdf — install tesseract + poppler for ocr"`; other failure `"error: ocr: <message>"`.
- `"ocr…"` is not terminal: rescan re-queues a file stuck at `"ocr…"` when no batch is in flight (app quit mid-OCR).
- Db writes on completion happen regardless of active space; only the in-memory `files_cache` refresh is conditional on the space still being active.
- Page markers: `[page N]` lines inline in the extracted text (locations remain `lines X-Y` from the unchanged `chunk_lines`).
- `pdftoppm` flags exactly: `-r 300 -gray -png`. Temp PNGs go in a per-run temp dir, removed afterwards.
- Tests must pass on machines without tesseract/poppler: real-OCR tests skip (early-return with eprintln) when `tesseract` is absent.
- Zero warnings from `cargo build`. Commit only the exact files touched (never `git add -A`).

---

### Task 1: `ocr_pdf` pipeline in extract.rs

**Files:**
- Modify: `src/extract.rs` (append after `chunk_lines`, tests in the existing `mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces (Task 2 relies on these exact names):
  - `pub(crate) enum OcrError { MissingTools, Failed(String) }`
  - `pub(crate) fn ocr_pdf(path: &Path) -> std::result::Result<String, OcrError>`
  - `#[cfg(test)] pub(crate) fn minimal_pdf(text: Option<&str>) -> Vec<u8>` — test fixture generator, reused by Task 2's tests.

- [ ] **Step 1: Write the failing tests**

Add at `src/extract.rs` top level (outside `mod tests`, so Task 2's tests in another module can reuse it):

```rust
/// Build a minimal valid PDF: one page, optionally with `text` drawn in
/// Helvetica. Offsets are computed at runtime so the xref is always correct.
/// Test fixture shared with app::files tests.
#[cfg(test)]
pub(crate) fn minimal_pdf(text: Option<&str>) -> Vec<u8> {
    let mut objs: Vec<String> = Vec::new();
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    objs.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into());
    if let Some(t) = text {
        objs.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 150] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".into());
        let stream = format!("BT /F1 32 Tf 20 60 Td ({t}) Tj ET");
        objs.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
        objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    } else {
        objs.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 150] >>".into());
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, o).as_bytes());
    }
    let xref_pos = out.len();
    let n = objs.len() + 1;
    out.extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n").as_bytes(),
    );
    out
}
```

Then append inside `mod tests`:

```rust
/// True when tesseract + pdftoppm are runnable (real-OCR tests skip otherwise).
fn ocr_tools_present() -> bool {
    std::process::Command::new("tesseract").arg("--version").output().is_ok()
        && std::process::Command::new("pdftoppm").arg("-v").output().is_ok()
}

#[test]
fn ocr_pdf_reads_rendered_text() {
    if !ocr_tools_present() {
        eprintln!("skipping ocr_pdf_reads_rendered_text: tesseract/pdftoppm not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("nexus-ocr-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("scan.pdf");
    std::fs::write(&pdf, minimal_pdf(Some("HELLO NEXUS OCR"))).unwrap();

    let text = ocr_pdf(&pdf).unwrap();
    assert!(text.contains("HELLO"), "ocr text was: {text:?}");
    assert!(text.contains("[page 1]"), "ocr text was: {text:?}");
}

#[test]
fn ocr_pdf_blank_page_yields_empty_text() {
    if !ocr_tools_present() {
        eprintln!("skipping ocr_pdf_blank_page_yields_empty_text: tools not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("nexus-ocr-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("blank.pdf");
    std::fs::write(&pdf, minimal_pdf(None)).unwrap();
    assert_eq!(ocr_pdf(&pdf).unwrap(), "");
}

#[test]
fn ocr_pdf_missing_tools_is_distinguishable() {
    let dir = std::env::temp_dir().join(format!("nexus-ocr-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("x.pdf");
    std::fs::write(&pdf, minimal_pdf(None)).unwrap();
    let err = ocr_pdf_with("nexus-definitely-not-a-binary", "tesseract", &pdf).unwrap_err();
    assert!(matches!(err, OcrError::MissingTools));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ocr_pdf 2>&1 | tail -20`
Expected: compile error — `ocr_pdf`, `ocr_pdf_with`, `OcrError` not found.

- [ ] **Step 3: Implement**

Append to `src/extract.rs` (after `chunk_lines`, before `mod tests`):

```rust
/// Why OCR failed: the tools aren't installed (user-fixable hint) vs a real
/// failure (surfaced as an error status).
pub(crate) enum OcrError {
    MissingTools,
    Failed(String),
}

/// OCR a (scanned) PDF with pdftoppm + tesseract. Pages join with `[page N]`
/// marker lines — same inline-marker convention as pptx's `[slide N]`.
/// `Ok("")` means the tools ran but found no text.
pub(crate) fn ocr_pdf(path: &Path) -> std::result::Result<String, OcrError> {
    ocr_pdf_with("pdftoppm", "tesseract", path)
}

/// Binary names are parameters so tests can exercise the missing-tools path
/// without mutating the process-global PATH.
fn ocr_pdf_with(
    pdftoppm: &str,
    tesseract: &str,
    path: &Path,
) -> std::result::Result<String, OcrError> {
    let tmp = std::env::temp_dir().join(format!("nexus-ocr-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).map_err(|e| OcrError::Failed(e.to_string()))?;
    let result = ocr_pdf_in(pdftoppm, tesseract, path, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn ocr_pdf_in(
    pdftoppm: &str,
    tesseract: &str,
    path: &Path,
    tmp: &Path,
) -> std::result::Result<String, OcrError> {
    fn run(cmd: &mut std::process::Command, name: &str) -> std::result::Result<Vec<u8>, OcrError> {
        let out = cmd.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                OcrError::MissingTools
            } else {
                OcrError::Failed(e.to_string())
            }
        })?;
        if !out.status.success() {
            return Err(OcrError::Failed(format!(
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }

    run(
        std::process::Command::new(pdftoppm)
            .args(["-r", "300", "-gray", "-png"])
            .arg(path)
            .arg(tmp.join("page")),
        "pdftoppm",
    )?;

    // pdftoppm zero-pads page numbers, so a lexical sort is page order.
    let mut pages: Vec<std::path::PathBuf> = std::fs::read_dir(tmp)
        .map_err(|e| OcrError::Failed(e.to_string()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    pages.sort();

    let mut text = String::new();
    for (i, png) in pages.iter().enumerate() {
        let stdout = run(
            std::process::Command::new(tesseract).arg(png).arg("stdout"),
            "tesseract",
        )?;
        let page = String::from_utf8_lossy(&stdout);
        let page = page.trim();
        if !page.is_empty() {
            text.push_str(&format!("[page {}]\n{page}\n", i + 1));
        }
    }
    Ok(text.trim().to_string())
}
```

Also change the missing-tools test's `ocr_pdf_with` call if visibility complains: `ocr_pdf_with` is a private fn in the same module, and `mod tests` is a child module — `use super::*` (already present) covers it.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --quiet 2>&1 | tail -5` and `cargo build 2>&1 | grep -c warning || true`
Expected: all tests pass (3 new); build has zero warnings. Note: `ocr_pdf` itself has no production caller until Task 2 — if the compiler flags it, add `#[allow(dead_code)] // used from Task 2 of the scanned-pdf-ocr plan; remove with first caller` on `OcrError` and `ocr_pdf` (tests calling them may already silence it; only add if `cargo build` warns).

- [ ] **Step 5: Commit**

```bash
git add src/extract.rs
git commit -m "feat: ocr_pdf — pdftoppm + tesseract pipeline with [page N] markers"
```

---

### Task 2: Background OCR wiring — rescan statuses, AppEvent::Ocr, on_ocr_done

**Files:**
- Modify: `src/app/files.rs` (`rescan_files`, new `start_ocr`/`on_ocr_done`, tests)
- Modify: `src/app/mod.rs` (`ocr_rx` field + init, `AppEvent::Ocr` variant, `next_event` arm)
- Modify: `src/events.rs` (dispatch arm)
- Modify: `src/db.rs` (`set_file_status`)

**Interfaces:**
- Consumes (from Task 1): `crate::extract::{ocr_pdf, OcrError}`, `crate::extract::chunk_lines(&str) -> Vec<(String, String)>`, test helper `crate::extract::minimal_pdf(Option<&str>) -> Vec<u8>` (`#[cfg(test)] pub(crate)`, defined at extract.rs top level).
- Produces:
  - `App.ocr_rx: Option<mpsc::UnboundedReceiver<(String, String, std::result::Result<String, String>)>>` — (space_id, file_name, result)
  - `AppEvent::Ocr(Option<(String, String, std::result::Result<String, String>)>)`
  - `App::start_ocr(&mut self, jobs: Vec<(String, String, std::path::PathBuf)>)` — (space_id, name, path)
  - `App::on_ocr_done(&mut self, r: Option<(String, String, std::result::Result<String, String>)>)`
  - `Db::set_file_status(&self, file_id: &str, status: &str) -> Result<()>`

- [ ] **Step 1: Add `Db::set_file_status`**

In `src/db.rs`, next to `set_file_chunks` (match the file's existing `params!` style):

```rust
pub fn set_file_status(&self, file_id: &str, status: &str) -> Result<()> {
    self.conn.execute("UPDATE files SET status = ?2 WHERE id = ?1", params![file_id, status])?;
    Ok(())
}
```

- [ ] **Step 2: Write the failing tests**

Append inside `mod tests` in `src/app/files.rs` (reuses the existing `test_app()` helper; import `minimal_pdf` per the Interfaces note above):

```rust
#[tokio::test]
async fn rescan_marks_empty_pdf_ocr_and_spawns_batch() {
    let mut a = test_app();
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("scan.pdf"), crate::extract::minimal_pdf(None)).unwrap();

    a.rescan_files();
    assert_eq!(a.files_cache[0].status, "ocr…");
    assert!(a.ocr_rx.is_some(), "an ocr batch should be in flight");

    // A second rescan while the batch is in flight does not re-queue.
    a.rescan_files();
    assert_eq!(a.files_cache[0].status, "ocr…");
}

#[tokio::test]
async fn rescan_requeues_stale_ocr_status_when_idle() {
    let mut a = test_app();
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("scan.pdf"), crate::extract::minimal_pdf(None)).unwrap();
    a.rescan_files();

    // Simulate an app restart mid-OCR: status stuck at "ocr…", no batch in flight.
    a.ocr_rx = None;
    a.rescan_files();
    assert!(a.ocr_rx.is_some(), "stale ocr… should re-queue");
}

#[test]
fn on_ocr_done_ok_indexes_and_marks_ok() {
    let mut a = test_app();
    let id = a.db.upsert_file(&a.active_space.id, "scan.pdf", "h", 9, "ocr…").unwrap();
    let _ = id;
    a.on_ocr_done(Some((
        a.active_space.id.clone(),
        "scan.pdf".to_string(),
        Ok("[page 1]\nquarterly revenue table".to_string()),
    )));
    assert_eq!(a.files_cache[0].status, "ok");
    let hits =
        crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn on_ocr_done_empty_and_err_statuses() {
    let mut a = test_app();
    a.db.upsert_file(&a.active_space.id, "blank.pdf", "h1", 9, "ocr…").unwrap();
    a.db.upsert_file(&a.active_space.id, "bad.pdf", "h2", 9, "ocr…").unwrap();

    a.on_ocr_done(Some((a.active_space.id.clone(), "blank.pdf".to_string(), Ok(String::new()))));
    a.on_ocr_done(Some((
        a.active_space.id.clone(),
        "bad.pdf".to_string(),
        Err("scanned pdf — install tesseract + poppler for ocr".to_string()),
    )));

    let by_name = |a: &App, n: &str| {
        a.files_cache.iter().find(|f| f.name == n).unwrap().status.clone()
    };
    assert_eq!(by_name(&a, "blank.pdf"), "no text (ocr found nothing)");
    assert_eq!(by_name(&a, "bad.pdf"), "scanned pdf — install tesseract + poppler for ocr");
}

#[test]
fn on_ocr_done_for_inactive_space_writes_db_but_not_cache() {
    let mut a = test_app();
    let other = a.db.create_space("other").unwrap();
    a.db.upsert_file(&other.id, "scan.pdf", "h", 9, "ocr…").unwrap();

    a.on_ocr_done(Some((other.id.clone(), "scan.pdf".to_string(), Ok("found text".to_string()))));

    assert!(a.files_cache.is_empty(), "active-space cache must not show other space's file");
    let rows = a.db.list_files(&other.id).unwrap();
    assert_eq!(rows[0].status, "ok");

    // Deleted-mid-OCR: result for a row that no longer exists is a no-op.
    a.on_ocr_done(Some((other.id.clone(), "gone.pdf".to_string(), Ok("x".to_string()))));
}

#[test]
fn on_ocr_done_none_clears_channel() {
    let mut a = test_app();
    let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
    a.ocr_rx = Some(rx);
    a.on_ocr_done(None);
    assert!(a.ocr_rx.is_none());
}
```

Note for `create_space`: check `src/db.rs` for the actual space-creation fn name/signature used by existing tests (e.g. how `test_app`/db tests create a second space) and use that; the assertion content stays the same.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --quiet 2>&1 | tail -20`
Expected: compile errors — `ocr_rx`, `on_ocr_done` not found.

- [ ] **Step 4: Implement the wiring**

`src/app/mod.rs`:
1. Field, next to `describe_rx`:
```rust
/// Background OCR results: (space_id, file name, extracted text or status message).
pub(crate) ocr_rx:
    Option<mpsc::UnboundedReceiver<(String, String, std::result::Result<String, String>)>>,
```
2. Initialize `ocr_rx: None,` in `App::new` next to `describe_rx: None,`.
3. `AppEvent` variant:
```rust
/// One OCR result per scanned PDF, or `None` when the batch's channel closed.
Ocr(Option<(String, String, std::result::Result<String, String>)>),
```
4. `next_event` arm (append inside the `tokio::select!`, same shape as the others):
```rust
r = async {
    match self.ocr_rx.as_mut() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
} => AppEvent::Ocr(r),
```

`src/events.rs`, after the `Described` arm:
```rust
AppEvent::Ocr(r) => app.on_ocr_done(r),
```

`src/app/files.rs` — `rescan_files` changes:
1. Before the entry loop: `let mut ocr_jobs: Vec<(String, String, std::path::PathBuf)> = Vec::new();`
2. Replace the unchanged-file check:
```rust
if let Some(f) = known.iter().find(|f| f.name == name && f.hash == hash) {
    // Stale "ocr…" (app quit mid-OCR) re-queues once no batch is in flight.
    if f.status == "ocr…" && self.ocr_rx.is_none() {
        ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
    }
    continue;
}
```
3. In the extraction match, split the empty case:
```rust
let (status, chunks) = match crate::extract::extract_text(&path) {
    Ok(text) if text.trim().is_empty() => {
        if name.to_lowercase().ends_with(".pdf") {
            ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
            ("ocr…".to_string(), Vec::new())
        } else {
            ("no text (scanned?)".to_string(), Vec::new())
        }
    }
    Ok(text) => ("ok".to_string(), crate::extract::chunk_lines(&text)),
    Err(e) => (format!("error: {e}"), Vec::new()),
};
```
4. After the deleted-files loop, before refreshing `files_cache`: `self.start_ocr(ocr_jobs);`

New methods on `App` in `src/app/files.rs`:
```rust
/// OCR queued scanned PDFs sequentially off the UI thread. One batch at a
/// time: jobs arriving while a batch runs stay at "ocr…" and re-queue on a
/// later rescan.
pub(crate) fn start_ocr(&mut self, jobs: Vec<(String, String, std::path::PathBuf)>) {
    if jobs.is_empty() || self.ocr_rx.is_some() {
        return;
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    self.ocr_rx = Some(rx);
    tokio::task::spawn_blocking(move || {
        for (space_id, name, path) in jobs {
            let result = match crate::extract::ocr_pdf(&path) {
                Ok(text) => Ok(text),
                Err(crate::extract::OcrError::MissingTools) => {
                    Err("scanned pdf — install tesseract + poppler for ocr".to_string())
                }
                Err(crate::extract::OcrError::Failed(e)) => Err(format!("error: ocr: {e}")),
            };
            if tx.send((space_id, name, result)).is_err() {
                return;
            }
        }
    });
}

/// A finished OCR job: persist chunks/status, refresh the cache only if the
/// file's space is still active. `None` = batch done (channel closed).
pub fn on_ocr_done(
    &mut self,
    r: Option<(String, String, std::result::Result<String, String>)>,
) {
    let Some((space_id, name, result)) = r else {
        self.ocr_rx = None;
        return;
    };
    let Ok(files) = self.db.list_files(&space_id) else { return };
    let Some(f) = files.iter().find(|f| f.name == name) else {
        return; // deleted mid-OCR
    };
    match result {
        Ok(text) if text.trim().is_empty() => {
            let _ = self.db.set_file_status(&f.id, "no text (ocr found nothing)");
        }
        Ok(text) => {
            let _ = self.db.set_file_chunks(&f.id, &crate::extract::chunk_lines(&text));
            let _ = self.db.set_file_status(&f.id, "ok");
        }
        Err(msg) => {
            let _ = self.db.set_file_status(&f.id, &msg);
        }
    }
    if space_id == self.active_space.id {
        self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
        self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
    }
}
```

Existing tests that call `rescan_files` synchronously (`#[test]`, no runtime) never hit `spawn_blocking` because they use non-PDF files — `start_ocr` returns early on empty jobs. Do not convert them.

- [ ] **Step 5: Run the full suite and warning check**

Run: `cargo test --quiet 2>&1 | tail -5` then `cargo build 2>&1 | tail -3`
Expected: all tests pass (6 new); zero warnings (remove any Task 1 `#[allow(dead_code)]` markers on `ocr_pdf`/`OcrError` now that production callers exist).

- [ ] **Step 6: Commit**

```bash
git add src/app/files.rs src/app/mod.rs src/events.rs src/db.rs src/extract.rs
git commit -m "feat: background OCR for scanned pdfs via AppEvent::Ocr"
```
