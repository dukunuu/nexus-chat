//! Space filesets: importing files into `spaces/<name>/files/`, keeping the
//! db index in sync with the directory, and extracting searchable text.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::App;

/// A message from the background OCR batch about one file.
pub(crate) enum OcrUpdate {
    /// A human-readable phase ("rendering pages…") shown while nothing is
    /// countable yet.
    Stage(String),
    /// (pages done, total pages, pages failed so far).
    Progress(usize, usize, usize),
    /// Final outcome: (extracted text, per-page errors as (index, reason)),
    /// or a whole-document error message.
    Done(std::result::Result<(String, Vec<(usize, String)>), String>),
}

/// One row of the file-picker browser.
pub struct PickerEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Which service transcribes a rendered page image.
#[derive(Clone)]
pub(crate) enum OcrBackend {
    /// OpenRouter vision model (`ocr_model`).
    Router(crate::provider::openrouter::OpenRouter, String),
    /// Local Ollama model via its native /api/generate endpoint — the
    /// OpenAI-compatible route mishandles GLM-OCR's vision input.
    Ollama(reqwest::Client, String),
}

impl OcrBackend {
    async fn transcribe(&self, png: &[u8]) -> anyhow::Result<String> {
        match self {
            OcrBackend::Router(provider, model) => {
                let url = crate::app::transcribe::png_bytes_data_url(png);
                provider.ocr_page(model, &url).await
            }
            OcrBackend::Ollama(client, model) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(png);
                let resp = client
                    .post("http://127.0.0.1:11434/api/generate")
                    // CPU inference is legitimately minutes/page; anything past
                    // this is a wedge and should fail into a page placeholder
                    // rather than hanging the whole batch forever.
                    .timeout(std::time::Duration::from_secs(600))
                    .json(&ollama_ocr_body(model, &b64))
                    .send()
                    .await
                    .map_err(|e| {
                        if e.is_timeout() {
                            anyhow::anyhow!("timeout after 600s")
                        } else if e.is_connect() {
                            anyhow::anyhow!(
                                "cannot reach ollama at 127.0.0.1:11434 — is it running? (systemctl start ollama)"
                            )
                        } else {
                            e.into()
                        }
                    })?;
                if resp.status().as_u16() == 404 {
                    anyhow::bail!("model '{model}' not pulled — run /ocr-local");
                }
                let v = resp.error_for_status()?.json::<serde_json::Value>().await?;
                Ok(v.get("response").and_then(|r| r.as_str()).unwrap_or("").to_string())
            }
        }
    }
}

/// First ~90 chars of an error, so a page failure fits in the status column
/// without swallowing the reason.
fn clip_err(e: &str) -> String {
    let mut s: String = e.chars().take(90).collect();
    if s.len() < e.len() {
        s.push('…');
    }
    s
}

/// Request body for Ollama's native generate endpoint: raw base64 in
/// `images`, not an OpenAI-style content part.
fn ollama_ocr_body(model: &str, png_b64: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "prompt": crate::provider::openrouter::OCR_PROMPT,
        "images": [png_b64],
        "stream": false,
        // Ollama defaults to 4096 ctx — page image tokens plus a dense page's
        // transcription overflow that and silently clip the output.
        "options": { "num_ctx": 8192 },
    })
}

/// OCR a scanned PDF through a vision backend: render pages at 300 DPI color,
/// transcribe up to 4 pages concurrently (one retry each), and join with
/// `[page N]` markers — a page that fails twice becomes a `[page N: ocr
/// failed]` marker instead of sinking the document.
async fn ocr_pdf_vlm(
    backend: &OcrBackend,
    path: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, String, OcrUpdate)>,
    space_id: &str,
    name: &str,
) -> std::result::Result<(String, Vec<(usize, String)>), String> {
    let tmp = std::env::temp_dir().join(format!("nexus-vlm-ocr-{}", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::create_dir_all(&tmp) {
        return Err(format!("error: ocr: {e}"));
    }
    let result = ocr_pdf_vlm_in(backend, path, &tmp, tx, space_id, name).await;
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

async fn ocr_pdf_vlm_in(
    backend: &OcrBackend,
    path: &Path,
    tmp: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, String, OcrUpdate)>,
    space_id: &str,
    name: &str,
) -> std::result::Result<(String, Vec<(usize, String)>), String> {
    // Text glyphs (and furigana especially) need more resolution than the
    // tesseract path's 200 DPI gray; VLMs also want the color signal.
    let _ = tx.send((
        space_id.to_string(),
        name.to_string(),
        OcrUpdate::Stage("rendering pages (300 dpi)…".to_string()),
    ));
    let (pdf, dir) = (path.to_path_buf(), tmp.to_path_buf());
    let pages = tokio::task::spawn_blocking(move || {
        crate::extract::render_pdf_pages("pdftoppm", &pdf, &dir, 300, false)
    })
    .await
    .map_err(|e| format!("error: ocr: {e}"))?
    .map_err(|e| match e {
        crate::extract::OcrError::MissingTools => {
            "scanned pdf — install poppler (pdftoppm) for ocr".to_string()
        }
        crate::extract::OcrError::Failed(m) => format!("error: ocr: {m}"),
    })?;

    let total = pages.len();
    // Show "0/N pages" immediately — on CPU the first page can take minutes,
    // and a frozen "ocr…" reads as stuck.
    let _ = tx.send((space_id.to_string(), name.to_string(), OcrUpdate::Progress(0, total, 0)));
    let mut results: Vec<std::result::Result<String, String>> =
        vec![Err("not transcribed".to_string()); total];
    let mut set = tokio::task::JoinSet::new();
    let spawn_page = |set: &mut tokio::task::JoinSet<(usize, std::result::Result<String, String>)>,
                      i: usize| {
        let (backend, png) = (backend.clone(), pages[i].clone());
        set.spawn(async move {
            let Ok(bytes) = std::fs::read(&png) else {
                return (i, Err("page image unreadable".to_string()));
            };
            let mut last = String::new();
            for _ in 0..2 {
                match backend.transcribe(&bytes).await {
                    Ok(text) => return (i, Ok(text)),
                    Err(e) => last = e.to_string(),
                }
            }
            (i, Err(last))
        });
    };

    let window = 4.min(total);
    let mut next = 0;
    while next < window {
        spawn_page(&mut set, next);
        next += 1;
    }
    let mut done = 0;
    let mut failed = 0;
    while let Some(joined) = set.join_next().await {
        let (i, r) = joined.unwrap_or((usize::MAX, Err("page task panicked".to_string())));
        if r.is_err() {
            failed += 1;
        }
        if let Some(slot) = results.get_mut(i) {
            *slot = r;
        }
        done += 1;
        let _ = tx.send((
            space_id.to_string(),
            name.to_string(),
            OcrUpdate::Progress(done, total, failed),
        ));
        if next < total {
            spawn_page(&mut set, next);
            next += 1;
        }
    }
    let errors: Vec<(usize, String)> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.as_ref().err().map(|e| (i, e.clone())))
        .collect();
    Ok((crate::extract::join_pages(&results), errors))
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
        let mut entries: Vec<PickerEntry> = match std::fs::read_dir(&self.picker_dir) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let is_dir = e.file_type().ok()?.is_dir();
                    Some(PickerEntry { name, is_dir })
                })
                .collect(),
            Err(e) => {
                self.status = format!("cannot read {}: {e}", self.picker_dir.display());
                Vec::new()
            }
        };
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
        let filtered = self.filtered_picker_entries();
        let Some(entry) = filtered.get(self.picker_selected) else {
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

    /// Sync the active space's files directory with the db: new or changed
    /// files (by sha256) are re-extracted and re-indexed, rows for deleted
    /// files are dropped, and `files_cache` is refreshed. Best-effort: a
    /// single bad file gets an "error: …" status instead of failing the scan.
    /// ponytail: runs synchronously on the UI task — extraction of a huge PDF
    /// blocks a beat; move to a blocking task if that ever hurts.
    pub fn rescan_files(&mut self) {
        let dir = self.space.files_dir(&self.active_space.name);
        let known = self.db.list_files(&self.active_space.id).unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();
        let mut ocr_jobs: Vec<(String, String, std::path::PathBuf)> = Vec::new();

        let entries = std::fs::read_dir(&dir).map(|rd| rd.flatten().collect::<Vec<_>>()).unwrap_or_default();
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            seen.push(name.clone());
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let disk_size = entry.metadata().map(|m| m.len() as i64).unwrap_or(0);
            let existing = known.iter().find(|f| f.name == name);
            // Unchanged by stat: skip entirely — no read, no hash. This is what
            // keeps /files and space switches snappy with big filesets.
            if let Some(f) = existing
                && f.size == disk_size
                && f.mtime == mtime
                && mtime != 0
            {
                // Stale "ocr…"/"ocr N/M" (app quit mid-OCR) re-queues once no
                // batch is in flight.
                if f.status.starts_with("ocr") && self.ocr_rx.is_none() {
                    ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                }
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let hash = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
            if let Some(f) = existing.filter(|f| f.hash == hash) {
                // Content unchanged (touched, or indexed before mtimes were
                // tracked): just record the stat for next time.
                let _ = self.db.set_file_mtime(&f.id, mtime);
                if f.status.starts_with("ocr") && self.ocr_rx.is_none() {
                    ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                }
                continue;
            }
            let size = bytes.len() as i64;
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
            if let Ok(id) = self.db.upsert_file(&self.active_space.id, &name, &hash, size, &status) {
                let _ = self.db.set_file_chunks(&id, &chunks);
                let _ = self.db.set_file_mtime(&id, mtime);
            }
        }
        for gone in known.iter().filter(|f| !seen.contains(&f.name)) {
            let _ = self.db.delete_file(&gone.id);
        }
        self.start_ocr(ocr_jobs);
        // Backfill vectors for anything whose chunks changed (or that predates
        // semantic search entirely).
        self.start_embedding();
        self.files_cache = self.db.list_files(&self.active_space.id).unwrap_or_default();
        self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
    }

    /// OCR queued scanned PDFs sequentially off the UI thread. One batch at a
    /// time: jobs arriving while a batch runs stay at "ocr…" and re-queue on a
    /// later rescan.
    pub(crate) fn start_ocr(&mut self, jobs: Vec<(String, String, std::path::PathBuf)>) {
        if jobs.is_empty() || self.ocr_rx.is_some() {
            return;
        }
        let backend = self.ocr_backend();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.ocr_rx = Some(rx);
        if let Some(backend) = backend {
            tokio::spawn(async move {
                for (space_id, name, path) in jobs {
                    let result = ocr_pdf_vlm(&backend, &path, &tx, &space_id, &name).await;
                    if tx.send((space_id, name, OcrUpdate::Done(result))).is_err() {
                        return;
                    }
                }
            });
            return;
        }
        tokio::task::spawn_blocking(move || {
            for (space_id, name, path) in jobs {
                let progress_tx = tx.clone();
                let (sid, fname) = (space_id.clone(), name.clone());
                let progress = move |done: usize, total: usize| {
                    let _ = progress_tx.send((sid.clone(), fname.clone(), OcrUpdate::Progress(done, total, 0)));
                };
                let result = match crate::extract::ocr_pdf(&path, &progress) {
                    Ok(text) => Ok((text, Vec::new())),
                    Err(crate::extract::OcrError::MissingTools) => {
                        Err("scanned pdf — install tesseract + poppler for ocr".to_string())
                    }
                    Err(crate::extract::OcrError::Failed(e)) => Err(format!("error: ocr: {e}")),
                };
                if tx.send((space_id, name, OcrUpdate::Done(result))).is_err() {
                    return;
                }
            }
        });
    }

    /// The vision backend scanned PDFs OCR through, or None for tesseract:
    /// "local" → Ollama; "vlm"/"auto" with an OCR model + provider → OpenRouter.
    pub(crate) fn ocr_backend(&self) -> Option<OcrBackend> {
        if self.ocr_engine == "local" {
            let model = self.local_ocr_model.trim();
            let model = if model.is_empty() { "glm-ocr" } else { model };
            return Some(OcrBackend::Ollama(reqwest::Client::new(), model.to_string()));
        }
        if self.vlm_ocr_enabled() {
            return self
                .provider
                .clone()
                .map(|p| OcrBackend::Router(p, self.ocr_model.trim().to_string()));
        }
        None
    }

    /// `/ocr-local [model]`: pull a local OCR model through Ollama in the
    /// background and switch the OCR engine to it when the pull succeeds.
    /// Defaults to glm-ocr (0.9B — the current open OCR benchmark leader).
    pub(crate) fn ocr_local_install(&mut self, arg: &str) {
        if self.ocr_pull_rx.is_some() {
            self.status = "an /ocr-local pull is already running".to_string();
            return;
        }
        let model = if arg.is_empty() { "glm-ocr".to_string() } else { arg.to_string() };
        self.local_ocr_model = model.clone();
        let _ = self.db.set_setting("local_ocr_model", &model);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.ocr_pull_rx = Some(rx);
        self.status = format!("pulling {model} via ollama… (keeps running in background)");
        tokio::spawn(async move {
            let result = match tokio::process::Command::new("ollama")
                .args(["pull", &model])
                .output()
                .await
            {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Err("ollama not installed — get it from https://ollama.com (pacman -S ollama), then retry".to_string())
                }
                Err(e) => Err(format!("ollama pull failed: {e}")),
                Ok(out) if !out.status.success() => {
                    let err = String::from_utf8_lossy(&out.stderr);
                    let hint = if err.contains("could not connect") || err.contains("connection refused") {
                        " — is the ollama server running? (systemctl start ollama, or `ollama serve`)"
                    } else {
                        ""
                    };
                    Err(format!("ollama pull failed: {}{hint}", err.trim()))
                }
                Ok(_) => Ok(model),
            };
            let _ = tx.send(result);
        });
    }

    /// `/ocr-local` pull finished: point the OCR engine at the local model.
    pub fn on_ocr_pull(&mut self, r: Option<Result<String, String>>) {
        let Some(result) = r else {
            self.ocr_pull_rx = None;
            return;
        };
        self.ocr_pull_rx = None;
        match result {
            Ok(model) => {
                self.ocr_engine = "local".to_string();
                let _ = self.db.set_setting("ocr_engine", "local");
                self.status =
                    format!("local OCR ready: {model} via ollama — Ctrl+O a file in /files to re-run it");
            }
            Err(e) => self.status = e,
        }
    }

    /// Ctrl+O in /files: throw away the selected file's extracted text (and
    /// vectors, via set_file_chunks) and re-index it from disk with the
    /// current OCR engine — how a tesseract-mangled book gets redone after
    /// configuring a VLM, without re-importing.
    pub(crate) fn reextract_selected_file(&mut self) {
        let Some(f) = self.files_cache.get(self.files_selected).cloned() else {
            return;
        };
        let _ = self.db.set_file_chunks(&f.id, &[]);
        // Zeroing hash + size guarantees the rescan takes the re-extract path
        // (a real file is never 0 bytes with an empty hash).
        let _ = self.db.upsert_file(&self.active_space.id, &f.name, "", 0, "re-extracting");
        self.status = format!("re-extracting: {}", f.name);
        self.rescan_files();
    }

    /// Embed the next file whose chunks lack vectors, one file per job (the
    /// done-handler chains the next). No-op without a provider, without an
    /// embedding model, or while a job is already in flight.
    pub(crate) fn start_embedding(&mut self) {
        if self.embed_rx.is_some() {
            return;
        }
        let Some(provider) = self.provider.clone() else { return };
        let model = self.embedding_model.trim().to_string();
        if model.is_empty() {
            return;
        }
        let space_id = self.active_space.id.clone();
        let Ok(missing) = self.db.files_missing_embeddings(&space_id) else { return };
        let Some(file_id) = missing.first().cloned() else { return };
        let chunks = self.db.file_chunk_texts(&file_id).unwrap_or_default();
        if chunks.is_empty() {
            return;
        }
        // Embedding is best-effort background work; outside a runtime (sync
        // unit tests) there's nowhere to run it, so just skip.
        let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
        let _ = self.db.set_file_status(&file_id, "embedding…");
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.embed_rx = Some(rx);
        handle.spawn(async move {
            let mut out: Vec<(i64, Vec<f32>)> = Vec::with_capacity(chunks.len());
            let mut err = None;
            for batch in chunks.chunks(64) {
                let inputs: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
                match provider.embed(&model, inputs).await {
                    Ok(vecs) => out.extend(batch.iter().zip(vecs).map(|((seq, _), v)| (*seq, v))),
                    Err(e) => {
                        err = Some(e.to_string());
                        break;
                    }
                }
            }
            let result = match err {
                Some(e) => Err(e),
                None => Ok(out),
            };
            let _ = tx.send((space_id, file_id, result));
        });
    }

    /// One embedding job finished: store vectors and chain the next file, or
    /// surface the error and stop (a dead endpoint shouldn't be hammered —
    /// the next rescan retries). Either way the file's status returns to "ok";
    /// search falls back to keywords while vectors are missing.
    pub fn on_embed_done(&mut self, r: Option<crate::app::EmbedMsg>) {
        let Some((space_id, file_id, result)) = r else {
            self.embed_rx = None;
            return;
        };
        self.embed_rx = None;
        match result {
            Ok(vecs) => {
                let _ = self.db.set_chunk_embeddings(&file_id, &vecs);
                let _ = self.db.set_file_status(&file_id, "ok");
                self.start_embedding();
            }
            Err(e) => {
                let _ = self.db.set_file_status(&file_id, "ok");
                self.status = format!("embedding failed: {e}");
            }
        }
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
        }
    }

    /// A finished OCR job: persist chunks/status, refresh the cache only if the
    /// file's space is still active. `None` = batch done (channel closed).
    pub fn on_ocr_done(&mut self, r: Option<(String, String, OcrUpdate)>) {
        let Some((space_id, name, update)) = r else {
            self.ocr_rx = None;
            // PDFs imported mid-batch sat at "ocr…" unqueued; this rescan
            // chains them into a fresh batch instead of stalling until the
            // user reopens /files.
            self.rescan_files();
            return;
        };
        let Ok(files) = self.db.list_files(&space_id) else { return };
        let Some(f) = files.iter().find(|f| f.name == name) else {
            return; // deleted mid-OCR
        };
        if !f.status.starts_with("ocr") {
            return; // re-imported mid-OCR — this result is for stale content
        }
        match update {
            OcrUpdate::Stage(s) => {
                // Keep the "ocr" prefix — the stale-check above depends on it.
                let _ = self.db.set_file_status(&f.id, &format!("ocr: {s}"));
                if space_id == self.active_space.id {
                    self.status = format!("ocr {name}: {s}");
                }
            }
            OcrUpdate::Progress(done, total, failed) => {
                let tail = if failed > 0 { format!(" ({failed} failed)") } else { String::new() };
                let _ = self.db.set_file_status(&f.id, &format!("ocr {done}/{total}{tail}"));
                if space_id == self.active_space.id {
                    self.status = format!("ocr {name}: {done}/{total} pages{tail}");
                }
            }
            OcrUpdate::Done(Ok((text, errors))) if text.trim().is_empty() => {
                // Nothing usable came back; say exactly why if we know.
                let status = match errors.first() {
                    Some((i, e)) => {
                        format!("all pages failed (p{}: {})", i + 1, clip_err(e))
                    }
                    None => "no text (ocr found nothing)".to_string(),
                };
                let _ = self.db.set_file_status(&f.id, &status);
                if space_id == self.active_space.id {
                    self.status = format!("ocr {name}: {status}");
                }
            }
            OcrUpdate::Done(Ok((text, errors))) => {
                let _ = self.db.set_file_chunks(&f.id, &crate::extract::chunk_lines(&text));
                let status = match errors.first() {
                    None => "ok".to_string(),
                    Some((i, e)) => format!(
                        "ok — {} page{} failed (p{}: {})",
                        errors.len(),
                        if errors.len() == 1 { "" } else { "s" },
                        i + 1,
                        clip_err(e),
                    ),
                };
                let _ = self.db.set_file_status(&f.id, &status);
                if space_id == self.active_space.id {
                    self.status = format!("ocr done: {name}");
                }
            }
            OcrUpdate::Done(Err(msg)) => {
                let _ = self.db.set_file_status(&f.id, &msg);
                if space_id == self.active_space.id {
                    self.status = format!("ocr {name}: {msg}");
                }
            }
        }
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
            self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
        }
    }

    /// Copy `path` into the active space's files dir and index it. Returns
    /// the imported file's name. An existing file with the same name is
    /// overwritten (the rescan re-extracts it).
    pub fn import_file(&mut self, path: &Path) -> Result<String> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .context("path has no file name")?;
        let dir = self.space.files_dir(&self.active_space.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::copy(path, dir.join(&name))
            .with_context(|| format!("copying {} into the space", path.display()))?;
        self.rescan_files();
        Ok(name)
    }

    /// Delete the highlighted file: disk copy and index rows both go.
    pub fn confirm_files_delete(&mut self) -> Result<()> {
        if let Some(f) = self.files_cache.get(self.files_selected).cloned() {
            let disk = self.space.files_dir(&self.active_space.name).join(&f.name);
            if disk.exists() {
                std::fs::remove_file(&disk).with_context(|| format!("removing {}", disk.display()))?;
            }
            self.db.delete_file(&f.id)?;
            self.status = format!("removed {}", f.name);
            self.rescan_files();
        }
        self.files_mode = super::FilesMode::Browse;
        Ok(())
    }

    pub(crate) fn open_files_popup(&mut self) {
        self.rescan_files();
        self.files_mode = super::FilesMode::Browse;
        self.popup = super::Popup::Files;
    }

    pub fn move_files_selection(&mut self, delta: i32) {
        self.files_selected = super::clamp_cursor(self.files_selected, self.files_cache.len(), delta);
    }

    pub fn start_files_add(&mut self) {
        self.files_edit.clear();
        self.files_mode = super::FilesMode::Add;
    }

    /// Import the path typed/pasted in Add mode. Bad paths report in the status
    /// line and return to Browse (nothing to roll back).
    pub fn confirm_files_add(&mut self) -> Result<()> {
        let raw = self.files_edit.trim().to_string();
        self.files_mode = super::FilesMode::Browse;
        if raw.is_empty() {
            return Ok(());
        }
        let path = std::path::PathBuf::from(&raw);
        if !path.is_file() {
            self.status = format!("not a file: {raw}");
            return Ok(());
        }
        match self.import_file(&path) {
            Ok(name) => self.status = format!("imported {name}"),
            Err(e) => self.status = format!("import failed: {e}"),
        }
        Ok(())
    }

    /// Ctrl+R in Browse: pre-fill the edit line with the current name.
    pub fn start_files_rename(&mut self) {
        if let Some(f) = self.files_cache.get(self.files_selected) {
            self.files_edit = f.name.clone();
            self.files_mode = super::FilesMode::Rename;
        }
    }

    /// Rename the highlighted file on disk; the rescan swaps the index rows
    /// (old name dropped, new name re-extracted).
    pub fn confirm_files_rename(&mut self) -> Result<()> {
        let new = self.files_edit.trim().to_string();
        self.files_mode = super::FilesMode::Browse;
        let Some(f) = self.files_cache.get(self.files_selected).cloned() else { return Ok(()) };
        if new.is_empty() || new == f.name {
            return Ok(());
        }
        if new.contains(['/', '\\']) || new == "." || new == ".." {
            self.status = format!("invalid name: {new}");
            return Ok(());
        }
        let dir = self.space.files_dir(&self.active_space.name);
        if dir.join(&new).exists() {
            self.status = format!("{new} already exists");
            return Ok(());
        }
        std::fs::rename(dir.join(&f.name), dir.join(&new))
            .with_context(|| format!("renaming {} to {new}", f.name))?;
        self.rescan_files();
        self.files_selected =
            self.files_cache.iter().position(|f| f.name == new).unwrap_or(self.files_selected);
        self.status = format!("renamed {} to {new}", f.name);
        Ok(())
    }

    /// Open the highlighted file in the system viewer (Enter in Browse).
    pub fn open_selected_file(&mut self) {
        if let Some(f) = self.files_cache.get(self.files_selected) {
            let path = self.space.files_dir(&self.active_space.name).join(&f.name);
            let _ = open::that_detached(&path);
            self.status = format!("opened {}", f.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root = std::env::temp_dir().join(format!("nexus-files-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k".into()), space)
    }

    #[tokio::test]
    async fn embedder_queue_backfills_chains_and_stops_on_error() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let id = a.db.upsert_file(&space, "b.txt", "h", 1, "ok").unwrap();
        a.db.set_file_chunks(&id, &[("l".into(), "text".into())]).unwrap();

        // No provider → no-op.
        let saved = a.provider.take();
        a.start_embedding();
        assert!(a.embed_rx.is_none());
        a.provider = saved;

        // Blank embedding model → no-op.
        let m = std::mem::take(&mut a.embedding_model);
        a.start_embedding();
        assert!(a.embed_rx.is_none());
        a.embedding_model = m;

        // Missing vectors + provider → queued, status flips.
        a.start_embedding();
        assert!(a.embed_rx.is_some());
        let files = a.db.list_files(&space).unwrap();
        assert!(files[0].status.starts_with("embedding"), "{}", files[0].status);

        // Success: vectors stored, status ok, file leaves the missing list.
        a.on_embed_done(Some((space.clone(), id.clone(), Ok(vec![(0, vec![1.0f32, 0.0])]))));
        assert!(a.db.files_missing_embeddings(&space).unwrap().is_empty());
        assert_eq!(a.db.list_files(&space).unwrap()[0].status, "ok");

        // Error: status restored, no re-queue (don't hammer a dead endpoint).
        a.db.set_file_chunks(&id, &[("l".into(), "new".into())]).unwrap();
        a.on_embed_done(Some((space.clone(), id.clone(), Err("offline".into()))));
        assert!(a.embed_rx.is_none());
        assert!(a.status.contains("embedding failed"));
        assert_eq!(a.db.list_files(&space).unwrap()[0].status, "ok");
    }

    #[test]
    fn import_copies_extracts_and_indexes() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-src-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&src, "# quarterly report\nrevenue up").unwrap();

        let name = a.import_file(&src).unwrap();
        assert_eq!(name, src.file_name().unwrap().to_string_lossy());
        assert_eq!(a.files_cache.len(), 1);
        assert_eq!(a.files_cache[0].status, "ok");
        // Copied into the space's files dir.
        assert!(a.space.files_dir(&a.active_space.name).join(&name).exists());
        // Indexed: searchable.
        let hits = crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn ollama_ocr_body_uses_native_generate_shape() {
        let body = ollama_ocr_body("glm-ocr", "QUFB");
        assert_eq!(body["model"], "glm-ocr");
        assert_eq!(body["stream"], false);
        assert_eq!(body["images"][0], "QUFB"); // raw base64, not a data URL
        assert!(body["prompt"].as_str().unwrap().contains("furigana"));
        assert!(body.get("messages").is_none(), "must not be OpenAI chat shape");
    }

    #[test]
    fn ocr_backend_routes_by_engine() {
        let mut a = test_app();
        // auto + model + provider → OpenRouter.
        assert!(matches!(a.ocr_backend(), Some(OcrBackend::Router(..))));
        // local → Ollama regardless of provider/ocr_model.
        a.ocr_engine = "local".to_string();
        a.local_ocr_model = String::new(); // blank falls back to glm-ocr
        match a.ocr_backend() {
            Some(OcrBackend::Ollama(_, model)) => assert_eq!(model, "glm-ocr"),
            other => panic!("expected ollama backend, got {}", other.is_some()),
        }
        // tesseract → none.
        a.ocr_engine = "tesseract".to_string();
        assert!(a.ocr_backend().is_none());
        // auto without provider → none (tesseract fallback).
        a.ocr_engine = "auto".to_string();
        a.provider = None;
        assert!(a.ocr_backend().is_none());
    }

    #[tokio::test]
    async fn ocr_pull_success_switches_engine_to_local() {
        let mut a = test_app();
        a.on_ocr_pull(Some(Ok("glm-ocr".to_string())));
        assert_eq!(a.ocr_engine, "local");
        assert!(a.status.contains("local OCR ready"));
        a.on_ocr_pull(Some(Err("ollama not installed — get it".to_string())));
        assert_eq!(a.ocr_engine, "local"); // engine untouched on failure
        assert!(a.status.contains("ollama not installed"));
    }

    #[test]
    fn ocr_statuses_surface_stages_failures_and_reasons() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let id = a.db.upsert_file(&space, "scan.pdf", "h", 1, "ocr…").unwrap();

        // Stage → visible phase, still "ocr"-prefixed (stale-check depends on it).
        a.on_ocr_done(Some((space.clone(), "scan.pdf".into(), OcrUpdate::Stage("rendering pages (300 dpi)…".into()))));
        let status = a.db.list_files(&space).unwrap()[0].status.clone();
        assert_eq!(status, "ocr: rendering pages (300 dpi)…");

        // Progress with failures shows the count.
        a.on_ocr_done(Some((space.clone(), "scan.pdf".into(), OcrUpdate::Progress(5, 10, 2))));
        assert_eq!(a.db.list_files(&space).unwrap()[0].status, "ocr 5/10 (2 failed)");
        assert!(a.status.contains("5/10 pages (2 failed)"));

        // Partial success keeps the first failure's reason in the status.
        a.on_ocr_done(Some((
            space.clone(),
            "scan.pdf".into(),
            OcrUpdate::Done(Ok((
                "[page 1]\ntext".to_string(),
                vec![(2, "timeout after 600s".to_string()), (4, "boom".to_string())],
            ))),
        )));
        let status = a.db.list_files(&space).unwrap()[0].status.clone();
        assert_eq!(status, "ok — 2 pages failed (p3: timeout after 600s)");

        // All pages failed → the reason, not a bland "no text".
        let _ = a.db.set_file_status(&id, "ocr…");
        a.on_ocr_done(Some((
            space.clone(),
            "scan.pdf".into(),
            OcrUpdate::Done(Ok((
                String::new(),
                vec![(0, "cannot reach ollama at 127.0.0.1:11434 — is it running? (systemctl start ollama)".to_string())],
            ))),
        )));
        let status = a.db.list_files(&space).unwrap()[0].status.clone();
        assert!(status.starts_with("all pages failed (p1: cannot reach ollama"), "{status}");
        // Must NOT start with "ocr" — that prefix means "queued" to the rescan.
        assert!(!status.starts_with("ocr"), "{status}");
    }

    #[test]
    fn reextract_clears_stale_chunks_and_reindexes_from_disk() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("doc.txt"), "real content on disk").unwrap();
        a.rescan_files();
        let id = a.files_cache[0].id.clone();

        // Simulate a bad old extraction (e.g. tesseract-mangled OCR).
        a.db.set_file_chunks(&id, &[("p1".into(), "garbage".into())]).unwrap();

        a.files_selected = 0;
        a.reextract_selected_file();
        assert!(a.status.contains("re-extracting"), "{}", a.status);
        let texts = a.db.file_chunk_texts(&id).unwrap();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].1.contains("real content"), "{texts:?}");
        assert_eq!(a.files_cache[0].status, "ok");
    }

    #[test]
    fn rescan_picks_up_dropped_and_deleted_files() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dropped.txt"), "hello dropped").unwrap();

        a.rescan_files();
        assert_eq!(a.files_cache.len(), 1);
        assert_eq!(a.files_cache[0].name, "dropped.txt");

        // Changing content re-extracts (hash change), deleting drops the row.
        std::fs::write(dir.join("dropped.txt"), "hello again").unwrap();
        a.rescan_files();
        assert_eq!(a.files_cache.len(), 1);
        std::fs::remove_file(dir.join("dropped.txt")).unwrap();
        a.rescan_files();
        assert!(a.files_cache.is_empty());
    }

    #[test]
    fn empty_extraction_gets_no_text_status() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("empty.txt"), "   ").unwrap();
        a.rescan_files();
        assert_eq!(a.files_cache[0].status, "no text (scanned?)");
    }

    #[test]
    fn rescan_skips_stat_unchanged_files_without_rehashing() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("book.txt"), "big content").unwrap();
        a.rescan_files();
        let f = a.files_cache[0].clone();
        assert!(f.mtime > 0, "mtime recorded on index");

        // Plant a wrong hash; a stat-unchanged rescan must not correct it —
        // proof the file wasn't re-read/re-hashed.
        a.db.upsert_file(&a.active_space.id, "book.txt", "sentinel", f.size, "ok").unwrap();
        a.rescan_files();
        assert_eq!(a.files_cache[0].hash, "sentinel");

        // A size change busts the stat check and re-hashes for real.
        std::fs::write(dir.join("book.txt"), "big content grew").unwrap();
        a.rescan_files();
        assert_ne!(a.files_cache[0].hash, "sentinel");
        assert_eq!(a.files_cache[0].status, "ok");
    }

    #[test]
    fn rename_moves_disk_file_and_reindexes() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.txt"), "searchable content").unwrap();
        std::fs::write(dir.join("taken.txt"), "x").unwrap();
        a.rescan_files();
        a.files_selected = a.files_cache.iter().position(|f| f.name == "old.txt").unwrap();

        // Collides with an existing name: rejected, nothing moves.
        a.start_files_rename();
        assert_eq!(a.files_edit, "old.txt");
        a.files_edit = "taken.txt".to_string();
        a.confirm_files_rename().unwrap();
        assert!(a.status.contains("already exists"));
        assert!(dir.join("old.txt").exists());

        // Bad name rejected.
        a.files_selected = a.files_cache.iter().position(|f| f.name == "old.txt").unwrap();
        a.start_files_rename();
        a.files_edit = "../evil.txt".to_string();
        a.confirm_files_rename().unwrap();
        assert!(a.status.contains("invalid name"));

        // Valid rename: disk moves, index follows, cursor tracks the file.
        a.files_selected = a.files_cache.iter().position(|f| f.name == "old.txt").unwrap();
        a.start_files_rename();
        a.files_edit = "new.txt".to_string();
        a.confirm_files_rename().unwrap();
        assert!(!dir.join("old.txt").exists());
        assert!(dir.join("new.txt").exists());
        assert!(a.files_cache.iter().any(|f| f.name == "new.txt"));
        assert_eq!(a.files_cache[a.files_selected].name, "new.txt");
    }

    #[test]
    fn delete_removes_disk_file_and_row() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gone.txt"), "bye").unwrap();
        a.rescan_files();
        a.files_selected = 0;
        a.confirm_files_delete().unwrap();
        assert!(a.files_cache.is_empty());
        assert!(!dir.join("gone.txt").exists());
    }

    #[test]
    fn files_command_opens_popup_and_rescans() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seen.txt"), "content").unwrap();
        a.run_command("files").unwrap();
        assert!(a.popup == crate::app::Popup::Files);
        assert_eq!(a.files_cache.len(), 1);
        assert!(a.files_mode == crate::app::FilesMode::Browse);
    }

    #[test]
    fn confirm_files_add_imports_typed_path() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-add-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&src, "typed in").unwrap();
        a.start_files_add();
        assert!(a.files_mode == crate::app::FilesMode::Add);
        a.files_edit = src.to_string_lossy().to_string();
        a.confirm_files_add().unwrap();
        assert!(a.files_mode == crate::app::FilesMode::Browse);
        assert_eq!(a.files_cache.len(), 1);

        // A bad path reports in status and stays recoverable.
        a.start_files_add();
        a.files_edit = "/definitely/not/a/file".to_string();
        a.confirm_files_add().unwrap();
        assert!(a.status.contains("not a file"));
        assert_eq!(a.files_cache.len(), 1);
    }

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
            OcrUpdate::Done(Ok(("[page 1]\nquarterly revenue table".to_string(), Vec::new()))),
        )));
        assert_eq!(a.files_cache[0].status, "ok");
        let hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn on_ocr_progress_updates_status_and_status_line() {
        let mut a = test_app();
        a.db.upsert_file(&a.active_space.id, "scan.pdf", "h", 9, "ocr…").unwrap();
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "scan.pdf".to_string(),
            OcrUpdate::Progress(3, 10, 0),
        )));
        assert_eq!(a.files_cache[0].status, "ocr 3/10");
        assert!(a.status.contains("3/10"), "{}", a.status);

        // Progress for a file mid-way is still non-terminal: a Done after it applies.
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "scan.pdf".to_string(),
            OcrUpdate::Done(Ok(("[page 1]\nfound".to_string(), Vec::new()))),
        )));
        assert_eq!(a.files_cache[0].status, "ok");
    }

    #[test]
    fn on_ocr_done_empty_and_err_statuses() {
        let mut a = test_app();
        a.db.upsert_file(&a.active_space.id, "blank.pdf", "h1", 9, "ocr…").unwrap();
        a.db.upsert_file(&a.active_space.id, "bad.pdf", "h2", 9, "ocr…").unwrap();

        a.on_ocr_done(Some((a.active_space.id.clone(), "blank.pdf".to_string(), OcrUpdate::Done(Ok((String::new(), Vec::new()))))));
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "bad.pdf".to_string(),
            OcrUpdate::Done(Err("scanned pdf — install tesseract + poppler for ocr".to_string())),
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

        a.on_ocr_done(Some((other.id.clone(), "scan.pdf".to_string(), OcrUpdate::Done(Ok(("found text".to_string(), Vec::new()))))));

        assert!(a.files_cache.is_empty(), "active-space cache must not show other space's file");
        let rows = a.db.list_files(&other.id).unwrap();
        assert_eq!(rows[0].status, "ok");

        // Deleted-mid-OCR: result for a row that no longer exists is a no-op.
        a.on_ocr_done(Some((other.id.clone(), "gone.pdf".to_string(), OcrUpdate::Done(Ok(("x".to_string(), Vec::new()))))));
    }

    #[tokio::test]
    async fn on_ocr_done_none_clears_channel_and_requeues_stragglers() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        // A scanned PDF stuck at "ocr…" (imported while a batch was running).
        std::fs::write(dir.join("scan.pdf"), crate::extract::minimal_pdf(None)).unwrap();
        a.rescan_files();
        assert!(a.ocr_rx.is_some());
        // Batch finishes: channel clears, and the straggler chains into a new batch.
        a.on_ocr_done(None);
        assert!(a.ocr_rx.is_some(), "straggler should re-queue on batch end");
    }
}
