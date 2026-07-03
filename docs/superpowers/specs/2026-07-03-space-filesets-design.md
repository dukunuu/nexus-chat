# Space Filesets + Image Transcription — Design

Date: 2026-07-03
Status: approved

## Problem

Spaces carry only `memory.md` + `instructions.md`. Users want each space to own
a set of reference files (PDF, PPTX, DOCX, XLSX, CSV, plain text) the model can
consult — without flooding the context window with file content. Separately:
pasting a clipboard image should produce editable text via a small transcriber
model, so the main chat model never receives image data.

## Decisions (user-approved)

| Question | Decision |
|---|---|
| How content reaches the model | On-demand tools (`search_files`, `read_file`); system prompt lists file names/type/size only |
| Extraction | At import, pure Rust; cached in db chunks; re-extract on hash change |
| Search | SQLite FTS5 (BM25) over chunks — no embeddings in v1; schema leaves room to add an embedding column later |
| Image paste | Transcribe into composer, editable before send |
| Add UX | `/files` popup + composer path-paste detection + manual drop into dir (rescan on `/files` open and space switch) |
| Formats | PDF, PPTX/DOCX/XLSX, txt/md/csv/code; not old binary .doc/.ppt/.xls, no OCR |

## Architecture

### Storage & import
- Files live at `spaces/<name>/files/<original-name>`; import copies the file
  in. `Space` (src/space.rs) gains `files_dir(name)`.
- DB (src/db.rs):
  - `files` table: id, space, name, hash (sha256), size, status
    (`ok` | `no text` | `error: …`).
  - `file_chunks` FTS5 virtual table: file_id (UNINDEXED), location, text.
    rusqlite's bundled SQLite includes FTS5.
- Rescan (on `/files` open and space switch): diff directory contents against
  `files` rows by name+hash; new/changed files re-extract, deleted files drop
  rows.

### Extraction (import time, pure Rust)
- txt/md/csv/code: read as-is (UTF-8, lossy).
- PDF: `pdf-extract` crate. Empty output → status "no text (scanned?)".
- PPTX/DOCX/XLSX: `zip` + `quick-xml`; slide/paragraph/cell text with
  slide/sheet location markers.
- Chunking: ~40 lines per chunk, location label per chunk
  (e.g. `report.pdf:120-160`, `deck.pptx:slide 4`).
- New deps: `pdf-extract`, `zip`, `quick-xml`, `sha2`.

### Model access
- System prompt gains one short fileset section (name, type, size per file) —
  same pattern as the skills section in src/app/chat.rs.
- `ToolBox` (src/tools.rs) gains, mirroring the existing `skill` tool:
  - `search_files(query)` → BM25-ranked FTS5 snippets with `file:location` refs.
  - `read_file(name, offset?, limit?)` → ranged read of extracted text,
    capped at 200 lines per call.
- ToolBox is used from the streaming task; file search/read goes through a
  dedicated read-only rusqlite connection (implementation plan decides the
  exact handoff — ToolBox currently owns no db handle).

### Image paste → transcriber
- Keybind reads `arboard::Clipboard::get_image()` → encode PNG → base64 data
  URL → single non-streaming OpenRouter chat call to the configured vision
  model with a faithful-transcription prompt (`image_url` content part —
  needs a multimodal request path in src/provider/openrouter.rs).
- Status-bar spinner while transcribing; transcript inserted into the composer
  at the cursor.
- New dep: `png` (or `image`) for encoding raw clipboard RGBA.

### Config
- `Settings` + settings popup gain `transcriber_model`
  (default `google/gemini-2.5-flash-lite`).

## Out of scope (v1)
- Semantic embeddings / RAG.
- OCR for scanned PDFs.
- Old binary Office formats.
- Sending images to the main model.

## Verification
- Unit tests: extractors against small fixtures, chunking, FTS5 search,
  path-paste detection.
- Manual: import each format via all three add paths; ask questions answerable
  only from a file and observe tool calls; paste a screenshot and see the
  transcript land in the composer; confirm the system prompt carries only the
  file list.
