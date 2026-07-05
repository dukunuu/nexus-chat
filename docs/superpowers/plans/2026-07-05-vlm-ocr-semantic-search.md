# VLM OCR + Semantic Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scanned PDFs OCR through a configurable vision model; `search_files` ranks chunks by embedding cosine similarity (OpenRouter embeddings endpoint) with FTS fallback; existing data backfills in place.

**Architecture:** Three settings (`ocr_model` with a dedicated picker, `ocr_engine`, `embedding_model`). VLM OCR plugs into the existing OCR queue (render pages with pdftoppm at 300 DPI, one chat call per page, 4 in flight). Embeddings live in a new `chunk_embeddings` table filled by a background queue mirroring the OCR queue; `search_files` embeds the query and brute-force cosine-ranks.

**Tech Stack:** Rust, tokio, reqwest, rusqlite. No new crates.

**Spec:** `docs/superpowers/specs/2026-07-05-vlm-ocr-semantic-search-design.md`

## Global Constraints

- No new crates.
- Tests: `Db::open_in_memory()` + temp-dir Space via `std::env::temp_dir().join(format!("nexus-…-{}", uuid::Uuid::new_v4()))`; no `tempfile` crate; no network in tests.
- Commits straight to master, exact files staged, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Full `cargo test` green before every commit.

---

### Task 1: Settings — `ocr_model`, `ocr_engine`, `embedding_model`

**Files:** Modify `src/app/mod.rs` (fields, SettingsField rows, load arms), `src/app/models.rs` (ModelPickTarget::Ocr + pick/clear), `src/app/settings.rs` (persist), `src/ui/popups/settings.rs` (rows). Test: `src/app/tests.rs`.

**Interfaces produced:** `App.ocr_model: String` (default `google/gemini-2.5-flash-lite`), `App.ocr_engine: String` (`auto`), `App.embedding_model: String` (default `openai/text-embedding-3-small`), `App::vlm_ocr_enabled() -> bool` (`engine==vlm || (auto && !ocr_model.empty())`), `ModelPickTarget::Ocr`, `App::open_model_picker_for_ocr()`, `App::clear_ocr_model()`, `App::cycle_ocr_engine()`.

Steps: failing test (`vlm_ocr_enabled` truth table + persistence roundtrip via set_setting/load), implement following the Transcriber pattern exactly (picker returns to Settings popup), full suite, commit.

### Task 2: VLM OCR engine

**Files:** Modify `src/extract.rs` (page render helper split out), create `src/ocr_vlm.rs` (async page-transcribe loop), modify `src/app/files.rs` (`start_ocr` picks engine), `src/app/mod.rs` (`mod ocr_vlm` via main.rs — actually `src/main.rs` module list). Test: `src/extract.rs` / `src/ocr_vlm.rs` unit tests.

**Interfaces produced:**
- `extract::render_pdf_pages(pdftoppm: &str, path: &Path, tmp: &Path, dpi: u32, gray: bool) -> Result<Vec<PathBuf>, OcrError>` — factored from `ocr_pdf_in`, reused by both engines.
- `ocr_vlm::ocr_pdf_vlm(client: &reqwest::Client, api_key: &str, model: &str, path: &Path, progress: impl Fn(usize, usize)) -> Result<String, OcrError>` — renders at 300 DPI color, transcribes pages with ≤4 concurrent chat calls (`futures`-free: chunked `join_all` via tokio::spawn + semaphore-by-chunks), 1 retry, failed page → `[page N: ocr failed]`, joins with `[page N]` markers.
- Prompt constant `OCR_PROMPT`: faithful transcription, body text only, skip furigana/ruby, vertical columns right-to-left, tables as plain rows, no commentary/fences.
- `start_ocr` branches: `vlm_ocr_enabled()` → tokio::spawn(async vlm path) else spawn_blocking tesseract as today; both feed the same `ocr_rx` channel events.

Steps: failing tests (render helper produces zero-padded sorted pages at requested dpi — needs pdftoppm installed, skip-if-missing like existing OCR tests; data-url + request-body builder unit test, no network; placeholder join test with a stubbed transcribe fn), implement, suite, commit.

### Task 3: chunk_embeddings storage + cosine

**Files:** Modify `src/db.rs`. Test: db tests in `src/db.rs`.

**Interfaces produced:**
- Migration: `CREATE TABLE IF NOT EXISTS chunk_embeddings (file_id TEXT NOT NULL, seq INTEGER NOT NULL, vec BLOB NOT NULL, PRIMARY KEY (file_id, seq))`.
- `db::vec_to_blob(&[f32]) -> Vec<u8>` / `db::blob_to_vec(&[u8]) -> Vec<f32>` (LE).
- `Db::set_chunk_embeddings(file_id, &[(seq, Vec<f32>)])`, `Db::delete_chunk_embeddings(file_id)`; `set_file_chunks` also deletes that file's embeddings (chunks changed → vectors stale).
- `db::files_missing_embeddings(conn, space_id) -> Vec<String>` (file ids having chunks but fewer embedding rows than chunk rows).
- `db::semantic_chunks(conn, space_id, query_vec, limit) -> Vec<(name, location, text, score)>` — loads space vectors, cosine, skips dimension mismatches, top-N by score.

Steps: failing tests (roundtrip blob, semantic_chunks ranks the on-topic chunk first among hand-made vectors, dimension mismatch skipped, set_file_chunks invalidates embeddings, files_missing_embeddings detects backfill), implement, suite, commit.

### Task 4: background embedder queue + OpenRouter embeddings call

**Files:** Modify `src/provider/openrouter.rs` (`embed` fn), `src/app/files.rs` (queue driving), `src/app/mod.rs` (`embed_rx` field + AppEvent), `src/events.rs` (event arm). Test: `src/app/tests.rs` / files tests.

**Interfaces produced:**
- `Provider::embed(&self, model: &str, inputs: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>>` — `POST /embeddings`, OpenAI format `{model, input: [...]}` → `data[i].embedding`.
- `App::start_embedding()` — no-op if `embed_rx` busy or `embedding_model`/provider missing; takes next file id from `files_missing_embeddings`, spawns task: load that file's chunks, embed in batches of 64, store, send done event. On done: rescan-style requeue for the next file (chaining like `on_ocr_done`).
- File status: `embedding` (set when queued, `ok` when vectors stored) — reuses existing status column/UI (yellow like `ocr…`).
- Hooks: `rescan_files` end calls `start_embedding()`; extraction/OCR completion re-queues naturally since chunks were rewritten.

Steps: failing tests (start_embedding no-ops without provider/model; files_missing chain: fake vectors inserted directly → file leaves the missing list; status transitions), implement (network path untested), suite, commit.

### Task 5: semantic `search_files`

**Files:** Modify `src/tools.rs` (FilesCtx gains `api_key: Option<String>`, `embedding_model: String`; search arm), `src/app/mod.rs` (FilesCtx construction sites ×2). Test: `src/tools.rs`.

**Interfaces produced:** `search_files` arm: embed query via `Provider::embed`-equivalent call with FilesCtx creds → `semantic_chunks` top 8 → `name (location): text` (text truncated ~300 chars). On embed failure or zero stored vectors → existing `search_chunks` FTS path with `(keyword fallback)\n` prefix. Tool description updated: natural-language queries welcome.

Steps: failing test (no key / no vectors → fallback prefix + FTS hit still returned; with hand-inserted vectors and a stubbed query vector path — factor `semantic_search(conn, space, vec)` so the ranking is testable without network), implement, suite, commit.

### Task 6: re-OCR (Ctrl+O) in /files

**Files:** Modify `src/app/files.rs` (`reextract_selected_file`), `src/ui/popups/files.rs` (key + hint), `src/ui/popups/mod.rs` (classifier if needed). Test: files tests.

**Interfaces produced:** Ctrl+O on the selected file: delete chunks + embeddings, reset stored hash/mtime (forces the rescan slow path), rescan → re-extract with current engine, OCR requeues if scanned. Status message `re-extracting: {name}`.

Steps: failing test (file with sentinel chunks: reextract clears chunks and file goes back through extraction → chunks differ / status resets), implement, suite, commit.
