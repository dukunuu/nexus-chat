# VLM OCR + Semantic File Search — Design

Date: 2026-07-05
Status: approved

## Problem

Two compounding failures make scanned books (the driving case: a scanned Japanese book
with furigana over kanji) unusable in spaces:

1. **OCR**: the tesseract pipeline mangles furigana-annotated Japanese (ruby text is
   spliced into body text), renders at 200 DPI grayscale (too low for small glyphs),
   and can't handle vertical column order. Tesseract scores ~34% on olmOCR-Bench;
   VLMs score 70–84%.
2. **Search**: `search_files` uses FTS5 MATCH with AND-of-all-terms semantics, no
   stemming, and a tokenizer that cannot segment CJK at all — natural-language or
   Japanese queries return "no matches" almost always.

## Goal

Scanned documents OCR through a vision model (the existing `transcriber_model`
plumbing) and `search_files` becomes semantic: query and chunks are embedded via
OpenRouter's embeddings endpoint, ranked by cosine similarity. Japanese works end to
end. Old data migrates in place — no re-import.

## Design

### 1. OCR engine + model selection

- New setting `ocr_model` (default `google/gemini-2.5-flash-lite`) — its own `/config`
  row with a dedicated model picker (`ModelPickTarget::Ocr`, same pattern as the
  memory/transcriber rows: Enter opens the picker, Backspace clears/disables).
- New setting `ocr_engine`: `auto` (default) | `tesseract` | `vlm`, shown in `/config`
  (Enter cycles the value).
- `auto` = VLM when `ocr_model` is configured, tesseract otherwise. VLM OCR always
  uses `ocr_model`, never `transcriber_model`.
- No per-file choice.

### 2. VLM OCR pipeline

Reuses the existing OCR queue (rescan → jobs → one batch at a time → `on_ocr_done`).

- Render: `pdftoppm -r 300 -png` in color (tesseract path keeps 200 DPI gray).
- Per page: one non-streaming OpenRouter chat call to `ocr_model` with the
  page PNG as a data URL and generous `max_tokens`. Up to 4 pages in flight; one
  retry per page; a page that still fails becomes `[page N: ocr failed]` text
  instead of failing the document.
- Transcription prompt: faithful plain-text transcription, body text only —
  **skip furigana/ruby readings**, preserve reading order (vertical Japanese columns
  right-to-left), transcribe tables as plain text rows, no commentary, no markdown
  fences.
- Pages join with the existing `[page N]` markers; progress reports through the
  existing per-page callback (status bar `ocr 37/214`).
- Cost guard: no confirm dialog (cents per book); `ocr_engine = tesseract` opts out.

### 3. Semantic search (replaces FTS as the `search_files` backend)

- New setting `embedding_model` (default `openai/text-embedding-3-small`), in `/config`.
- New table `chunk_embeddings (file_id TEXT, seq INTEGER, vec BLOB, PRIMARY KEY
  (file_id, seq))` — f32 little-endian vector per chunk. Created by migration;
  existing `file_chunks` FTS table stays as the chunk text store.
- **Index time:** when a file's chunks are (re)written, its embeddings are deleted
  and re-queued. A background embedder (like the OCR queue: sequential batches,
  off the UI thread) embeds ~64 chunk texts per request via
  `POST https://openrouter.ai/api/v1/embeddings` and stores vectors. File status
  shows `embedding…` until done, then `ok`. On rescan, files whose chunks lack
  vectors are re-queued — this backfills pre-upgrade files automatically.
- **Query time:** `search_files` embeds the query (one call), brute-force cosine
  over the space's vectors in Rust, returns top 8 as `name (location): chunk text
  (truncated)`. No vector db, no ANN — thousands of chunks scan in milliseconds.
- **Fallback:** if the query embedding call fails (offline, no key) or the space has
  no vectors yet, fall back to the existing FTS MATCH path, result prefixed
  `(keyword fallback)`.
- Mixed dimensions (user changes `embedding_model`): vectors whose dimension doesn't
  match the query vector are skipped; rescan re-queues nothing automatically —
  changing the model takes effect for new/re-extracted files. (Accepted ceiling;
  a manual re-index can be added later if it chafes.)

### 4. Re-OCR

- `/files` popup: Ctrl+O on a file re-extracts it with the current engine — clears
  chunks + vectors, resets status, requeues OCR if scanned. Lets the tesseract-mangled
  book be redone without re-importing.

## Migrations

- `CREATE TABLE IF NOT EXISTS chunk_embeddings (…)` in the schema init.
- No changes to `files` or `file_chunks` rows; backfill happens via the embedder
  queue on rescan.

## Testing

- Engine selection: auto/tesseract/vlm × transcriber configured or not.
- VLM page prompt/render args; failed page yields `[page N: ocr failed]` placeholder.
- Embedding store/load roundtrip (f32 blob), cosine ranking returns the on-topic
  chunk first, dimension-mismatch vectors skipped.
- Backfill: file with chunks but no vectors gets queued on rescan.
- Fallback: no vectors + failing embedder → FTS path result.
- Re-OCR clears chunks/vectors and requeues.

## Out of scope

- Local embedding/OCR models (GPU); hybrid BM25+vector fusion; ANN index.
- Automatic re-embed on `embedding_model` change.
- Confirm dialogs or spend caps for OCR/embedding calls.
