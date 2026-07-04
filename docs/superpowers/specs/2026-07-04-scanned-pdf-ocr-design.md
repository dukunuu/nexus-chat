# Scanned-PDF OCR — Design

Date: 2026-07-04
Status: approved

## Problem

Fileset import extracts PDF text with `pdf-extract`. Scanned PDFs yield empty
text → status `"no text (scanned?)"`, zero FTS chunks, and the model cannot
read the file at all. The user wants scanned PDFs to work like any other
fileset file.

## Decisions (user-approved)

| Question | Decision |
|---|---|
| OCR engine | Local `tesseract` + `pdftoppm` (poppler), shelled out. No API cost, offline. |
| Trigger | Automatic at import/rescan when a `.pdf` extracts to empty text. |
| Execution | Background tokio task (same pattern as image describing) — `rescan_files` is synchronous on the UI thread and OCR of a multi-page scan takes tens of seconds. |
| Language | Whatever tessdata packs are installed (`eng` default). No language setting in v1. |

## Architecture

### Detection & trigger
- `rescan_files` / `import_file` unchanged for files that extract normally.
- A `.pdf` whose extraction returns empty text gets status `"ocr…"` (written
  to db, visible in `/files`) and a background OCR task is spawned for it.
- Sequential: one OCR task at a time per rescan (a rescan that finds several
  scanned PDFs queues them in one spawned task, processed in order).
- The stored content hash means a later rescan does not re-OCR an unchanged
  file: files already at a terminal status keep it; only hash changes
  re-enter extraction. Exception: a file still at `"ocr…"` with no in-flight
  OCR task (e.g. the app quit mid-OCR) is re-queued on rescan — `"ocr…"` is
  not a terminal status.

### OCR pipeline (`ocr_pdf` — new fn, `src/extract.rs` or small `src/ocr.rs`)
Per PDF:
1. `pdftoppm -r 300 -gray -png <file> <tmpdir>/page` renders page PNGs into a
   temp dir (cleaned up after).
2. For each page PNG in page order: `tesseract <png> stdout` → page text.
3. Pages join with `[page N]` marker lines so chunk locations read
   `file.pdf: page 3` (same convention as pptx `slide N` markers).
4. Whole-document empty OCR output → status `"no text (ocr found nothing)"`.
5. Missing binaries (spawn returns NotFound — checked on first command):
   status `"scanned pdf — install tesseract + poppler for ocr"`. No crash;
   file stays listed. Same status if either binary is missing.
6. Any other failure (non-zero exit, unreadable output) → status
   `"error: <message>"`.

### Completion
- New `AppEvent::OcrDone { space: String, file_name: String, result: Result<String, String> }`.
- Handler `on_ocr_done`:
  - Ok(text): chunk with the existing chunking (page markers become chunk
    locations), write chunks + status `"ok"` to db for that space's file row.
  - Err(msg): write the error/hint status to db.
  - Db writes happen regardless of the active space (keyed by space + file);
    the in-memory `files_cache` refreshes only if the space is still active.

### UI
- `/files` popup already renders a status column; `"ocr…"` and the failure
  statuses render via the existing non-"ok" yellow styling. No new keys.

## Out of scope
- Non-English language packs (tesseract picks up installed tessdata; a
  language setting can come later).
- OCR of image files imported into the fileset (images remain unsupported
  there).
- Manual re-OCR command; page caps; DPI configuration (300 dpi gray fixed).

## Verification
- Unit: page-marker location chunking; missing-binary → hint status (run with
  a PATH that lacks the tools); OcrDone handling for Ok/Err including
  inactive-space cache behavior.
- Integration: fixture scanned PDF OCR'd end-to-end, `#[ignore]`d or skipped
  when tesseract is absent so CI without the tools still passes.
- Manual: import a scanned PDF → status `"ocr…"` → `"ok"`; ask the model a
  question answerable only from that PDF and watch `search_files` hit it.
