//! Space filesets: importing files into `spaces/<name>/files/`, keeping the
//! db index in sync with the directory, and extracting searchable text.

// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::fmt::Write as _;
use std::path::Path;

use crate::db::FileRow;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::App;

/// A message from the background OCR batch about one file.
#[derive(Clone)]
pub enum OcrUpdate {
    /// A human-readable phase ("rendering pages…") shown while nothing is
    /// countable yet.
    Stage(String),
    /// (pages done, total pages, pages failed so far).
    Progress(usize, usize, usize),
    /// Final outcome: (extracted text, per-page errors as (index, reason)),
    /// or a whole-document error message.
    Done(std::result::Result<(String, Vec<(usize, String)>), String>),
}

/// Which service transcribes a rendered page image.
#[derive(Clone)]
pub enum OcrBackend {
    /// `OpenRouter` vision model (`ocr_model`).
    Router(crate::provider::openrouter::OpenRouter, String),
    /// Local Ollama model via its native /api/generate endpoint — the
    /// OpenAI-compatible route mishandles GLM-OCR's vision input.
    Ollama(reqwest::Client, String),
}

impl OcrBackend {
    async fn transcribe(&self, png: &[u8]) -> anyhow::Result<String> {
        self.transcribe_image(png, "image/png").await
    }

    /// Describe an image (not OCR — uses a description prompt so another model
    /// can reason about the image content). For standalone space-file images.
    async fn describe(&self, bytes: &[u8], mime: &str) -> anyhow::Result<String> {
        match self {
            Self::Router(provider, model) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                let url = format!("data:{mime};base64,{b64}");
                provider.describe_image(model, &url).await
            }
            Self::Ollama(client, model) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                let resp = client
                    .post("http://127.0.0.1:11434/api/generate")
                    .timeout(std::time::Duration::from_mins(10))
                    .json(&serde_json::json!({
                        "model": model,
                        "prompt": "Describe this image so another AI model can reason about \
                                   it without seeing it. Cover: what it is (screenshot, chart, \
                                   photo, diagram…), overall layout and structure, the key \
                                   entities and how they relate, ALL visible text verbatim, \
                                   and any notable visual details. Be thorough but do not \
                                   speculate beyond what is visible.",
                        "images": [b64],
                        "stream": false,
                        "options": { "num_ctx": 8192 },
                    }))
                    .send()
                    .await
                    .map_err(|e| {
                        if e.is_timeout() {
                            anyhow::anyhow!("timeout after 600s")
                        } else if e.is_connect() {
                            anyhow::anyhow!("cannot reach ollama — is it running?")
                        } else {
                            e.into()
                        }
                    })?;
                if resp.status().as_u16() == 404 {
                    anyhow::bail!("model '{model}' not pulled");
                }
                let v = resp.error_for_status()?.json::<serde_json::Value>().await?;
                Ok(v.get("response")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string())
            }
        }
    }

    /// Transcribe an image file with the given MIME type. For standalone images
    /// (not PDF pages) that may be JPEG, PNG, etc.
    async fn transcribe_image(&self, bytes: &[u8], mime: &str) -> anyhow::Result<String> {
        match self {
            Self::Router(provider, model) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                let url = format!("data:{mime};base64,{b64}");
                provider.ocr_page(model, &url).await
            }
            Self::Ollama(client, model) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                let resp = client
                    .post("http://127.0.0.1:11434/api/generate")
                    .timeout(std::time::Duration::from_mins(10))
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
                    anyhow::bail!(
                        "model '{model}' not pulled — cycle OCR engine to 'local' in /config"
                    );
                }
                let v = resp.error_for_status()?.json::<serde_json::Value>().await?;
                Ok(v.get("response")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string())
            }
        }
    }
}

/// First ~90 chars of an error, so a page failure fits in the status column
/// without swallowing the reason.
/// A stem that looks like a UUID (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx).
fn is_uuid_like(stem: &str) -> bool {
    let hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
    let parts: Vec<&str> = stem.split('-').collect();
    parts.len() == 5
        && parts[0].len() == 8
        && hex(parts[0])
        && parts[1].len() == 4
        && hex(parts[1])
        && parts[2].len() == 4
        && hex(parts[2])
        && parts[3].len() == 4
        && hex(parts[3])
        && parts[4].len() == 12
        && hex(parts[4])
}

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
/// Rendered page PNGs are saved permanently to `<files_dir>/<pdf_stem>/` so
/// the model can fetch them later via `files(action=pdf_page)`.
async fn ocr_pdf_vlm(
    backend: &OcrBackend,
    path: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, String, OcrUpdate)>,
    space_id: &str,
    name: &str,
    files_dir: &Path,
) -> std::result::Result<(String, Vec<(usize, String)>), String> {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let page_dir = files_dir.join(stem);
    if let Err(e) = std::fs::create_dir_all(&page_dir) {
        return Err(format!("error: ocr: {e}"));
    }
    ocr_pdf_vlm_in(backend, path, &page_dir, tx, space_id, name).await
}

async fn ocr_pdf_vlm_in(
    backend: &OcrBackend,
    path: &Path,
    page_dir: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, String, OcrUpdate)>,
    space_id: &str,
    name: &str,
) -> std::result::Result<(String, Vec<(usize, String)>), String> {
    let _ = tx.send((
        space_id.to_string(),
        name.to_string(),
        OcrUpdate::Stage("rendering pages (300 dpi)…".to_string()),
    ));
    let (pdf, dir) = (path.to_path_buf(), page_dir.to_path_buf());
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
    let _ = tx.send((
        space_id.to_string(),
        name.to_string(),
        OcrUpdate::Progress(0, total, 0),
    ));
    let mut results: Vec<std::result::Result<String, String>> =
        vec![Err("not transcribed".to_string()); total];
    let mut set = tokio::task::JoinSet::new();
    let spawn_page =
        |set: &mut tokio::task::JoinSet<(usize, std::result::Result<String, String>)>, i: usize| {
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

    // ponytail: 16 concurrent pages — the bottleneck is API latency, not
    // local CPU, so a wider window reduces wall-clock time significantly.
    // Tune this down if the backend rate-limits you.
    let window = (16_usize).min(total);
    let mut next = 0;
    while next < window {
        spawn_page(&mut set, next);
        next += 1;
    }
    let mut done = 0;
    let mut failed = 0;
    while let Some(joined) = set.join_next().await {
        let (i, r) = joined.unwrap_or_else(|_| (usize::MAX, Err("page task panicked".to_string())));
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
    // Rename pdftoppm output to stable page-<N>.png names
    let _ = std::fs::create_dir_all(page_dir);
    for (i, p) in pages.iter().enumerate() {
        let stable = page_dir.join(format!("page-{}.png", i + 1));
        let _ = std::fs::rename(p, &stable);
    }
    let errors: Vec<(usize, String)> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.as_ref().err().map(|e| (i, e.clone())))
        .collect();
    Ok((crate::extract::join_pages(&results), errors))
}

/// OCR a standalone image file through a vision backend: read the file,
/// transcribe it directly (no page rendering), return OCR text. Reuses the
/// same `OcrUpdate` channel as `pdf_vlm` for status/progress.
async fn ocr_image_vlm(
    backend: &OcrBackend,
    path: &Path,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, String, OcrUpdate)>,
    space_id: &str,
    name: &str,
) -> std::result::Result<(String, Vec<(usize, String)>), String> {
    let _ = tx.send((
        space_id.to_string(),
        name.to_string(),
        OcrUpdate::Stage("transcribing image…".to_string()),
    ));
    let Ok(bytes) = std::fs::read(path) else {
        return Err(format!("cannot read {name}"));
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    };
    let _ = tx.send((
        space_id.to_string(),
        name.to_string(),
        OcrUpdate::Progress(0, 1, 0),
    ));
    match backend.describe(&bytes, mime).await {
        Ok(text) => {
            let _ = tx.send((
                space_id.to_string(),
                name.to_string(),
                OcrUpdate::Progress(1, 1, 0),
            ));
            Ok((text, Vec::new()))
        }
        Err(e) => {
            let err = e.to_string();
            Err(format!("error: ocr: {err}"))
        }
    }
}

impl App {
    /// Sync the active space's files directory with the db: new or changed
    /// files (by sha256) are re-extracted and re-indexed, rows for deleted
    /// files are dropped, and `files_cache` is refreshed. Best-effort: a
    /// single bad file gets an "error: …" status instead of failing the scan.
    /// ponytail: runs synchronously on the UI task — extraction of a huge PDF
    /// blocks a beat; move to a blocking task if that ever hurts.
    #[allow(clippy::too_many_lines)]
    pub fn rescan_files(&mut self) {
        let dir = self.space.files_dir(&self.active_space.name);
        let known = self
            .db
            .list_files(&self.active_space.id)
            .unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();
        let mut ocr_jobs: Vec<(String, String, std::path::PathBuf)> = Vec::new();

        let entries = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
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
                .map_or(0, |d| d.as_secs() as i64);
            let disk_size = entry.metadata().map_or(0, |m| m.len() as i64);
            let existing = known.iter().find(|f| f.name == name);
            // Unchanged by stat: skip entirely — no read, no hash. This is what
            // keeps /files and space switches snappy with big filesets.
            if let Some(f) = existing
                && f.size == disk_size
                && f.mtime == mtime
                && mtime != 0
            {
                // Stale "ocr…"/"ocr N/M" (app quit mid-OCR) re-queues once no
                // batch is in flight.  Do not let the stat fast path hide a
                // file imported by an older build that has no chunks yet.
                if f.status.starts_with("ocr") {
                    if self.ocr_rx.is_none() {
                        ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                    }
                    continue;
                }
                if self.db.file_has_chunks(&f.id).unwrap_or(false) {
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let hash = Sha256::digest(&bytes)
                .iter()
                .fold(String::new(), |mut h, b| {
                    let _ = write!(h, "{b:02x}");
                    h
                });
            if let Some(f) = existing.filter(|f| f.hash == hash)
                && self.db.file_indexed(&f.id).unwrap_or(false)
                && self.db.file_has_chunks(&f.id).unwrap_or(false)
            {
                // Content unchanged (touched, or indexed before mtimes were
                // tracked): just record the stat for next time.
                let _ = self.db.set_file_mtime(&f.id, mtime);
                if f.status.starts_with("ocr") && self.ocr_rx.is_none() {
                    ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                }
                continue;
            }
            // Cold cache (fresh restore, wiped cache.db): the durable row
            // survives but this device's index state is gone — fall through
            // to re-extract so chunks/embeddings rebuild here.
            let size = bytes.len() as i64;
            let (status, chunks) = match crate::extract::extract_text(&path) {
                Ok(text) if text.trim().is_empty() => {
                    let ext = std::path::Path::new(&name)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if ext == "pdf" || crate::extract::is_image_ext(&ext) {
                        ocr_jobs.push((self.active_space.id.clone(), name.clone(), path.clone()));
                        ("ocr…".to_string(), Vec::new())
                    } else {
                        (
                            "no text (scanned?)".to_string(),
                            crate::extract::metadata_chunks(&name),
                        )
                    }
                }
                Ok(text) => {
                    let chunks = crate::extract::chunk_lines(&text);
                    if chunks.is_empty() {
                        (
                            "no text (scanned?)".to_string(),
                            crate::extract::metadata_chunks(&name),
                        )
                    } else {
                        ("ok".to_string(), chunks)
                    }
                }
                Err(e) => (
                    format!("error: {e}"),
                    crate::extract::metadata_chunks(&name),
                ),
            };
            if let Ok(id) = self
                .db
                .upsert_file(&self.active_space.id, &name, &hash, size, &status)
            {
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
        self.files_cache = self
            .db
            .list_files(&self.active_space.id)
            .unwrap_or_default();
    }

    /// OCR queued scanned PDFs sequentially off the UI thread. One batch at a
    /// time: jobs arriving while a batch runs stay at "ocr…" and re-queue on a
    /// later rescan.
    pub fn start_ocr(&mut self, jobs: Vec<(String, String, std::path::PathBuf)>) {
        if jobs.is_empty() || self.ocr_rx.is_some() {
            return;
        }
        let backend = self.ocr_backend();
        let files_dir = self.space.files_dir(&self.active_space.name);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.ocr_rx = Some(rx);
        if let Some(backend) = backend {
            tokio::spawn(async move {
                for (space_id, name, path) in jobs {
                    let is_image = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(crate::extract::is_image_ext);
                    let result = if is_image {
                        ocr_image_vlm(&backend, &path, &tx, &space_id, &name).await
                    } else {
                        ocr_pdf_vlm(&backend, &path, &tx, &space_id, &name, &files_dir).await
                    };
                    if tx.send((space_id, name, OcrUpdate::Done(result))).is_err() {
                        return;
                    }
                }
            });
            return;
        }
        tokio::task::spawn_blocking(move || {
            for (space_id, name, path) in jobs {
                let is_image = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(crate::extract::is_image_ext);
                if is_image {
                    // Images can't be OCR'd via tesseract — skip, it'll re-queue
                    // on next rescan if a VLM backend is configured.
                    let _ = tx.send((
                        space_id,
                        name,
                        OcrUpdate::Done(Err("no vlm backend for image ocr".to_string())),
                    ));
                    continue;
                }
                let progress_tx = tx.clone();
                let (sid, fname) = (space_id.clone(), name.clone());
                let progress = move |done: usize, total: usize| {
                    let _ = progress_tx.send((
                        sid.clone(),
                        fname.clone(),
                        OcrUpdate::Progress(done, total, 0),
                    ));
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
    /// "local" → Ollama; "vlm"/"auto" with an OCR model + provider → `OpenRouter`.
    pub fn ocr_backend(&self) -> Option<OcrBackend> {
        if self.ocr_engine == "local" {
            let model = self.local_ocr_model.trim();
            let model = if model.is_empty() { "glm-ocr" } else { model };
            return Some(OcrBackend::Ollama(
                reqwest::Client::new(),
                model.to_string(),
            ));
        }
        if self.vlm_ocr_enabled() {
            let model = self.ocr_model.trim().to_string();
            return self
                .resolve_model_backend(&model)
                .map(|(p, raw_model)| OcrBackend::Router(p, raw_model));
        }
        None
    }

    /// Cycling the OCR engine to "local" (in /config) pulls a local OCR model
    /// through Ollama in the background and switches the engine to it when
    /// the pull succeeds. Defaults to glm-ocr (0.9B — the current open OCR
    /// benchmark leader).
    pub fn ocr_local_install(&mut self, arg: &str) {
        if self.ocr_pull_rx.is_some() {
            self.push_status("an OCR model pull is already running".to_string());
            return;
        }
        let model = if arg.is_empty() {
            "glm-ocr".to_string()
        } else {
            arg.to_string()
        };
        self.local_ocr_model.clone_from(&model);
        let _ = self.db.set_setting("local_ocr_model", &model);
        // Under `cargo test` there's no reactor to spawn onto and no real
        // ollama to pull from — just take the switch synchronously so the
        // settings-cycle test doesn't need a Tokio runtime.
        #[cfg(test)]
        {
            self.ocr_engine = "local".to_string();
            let _ = self.db.set_setting("ocr_engine", "local");
            self.push_status(format!("(test) local OCR: {model}"));
        }
        #[cfg(not(test))]
        {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            self.ocr_pull_rx = Some(rx);
            self.push_status(format!(
                "pulling {model} via ollama… (keeps running in background)"
            ));
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
    }

    /// The local-OCR-model pull finished: point the OCR engine at the local model.
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
                self.push_status(format!(
                    "local OCR ready: {model} via ollama — Ctrl+O a file in /files to re-run it"
                ));
            }
            Err(e) => self.push_status(e),
        }
    }

    /// The `reextract`/`reocr`/delete popup flows live in the view layer;
    /// this is the re-extract half: zero the selected file's chunks and
    /// hash/size so the next rescan re-indexes from disk.
    pub fn reextract_file(&mut self, name: &str) {
        let Some(f) = self.files_cache.iter().find(|f| f.name == name).cloned() else {
            return;
        };
        let _ = self.db.set_file_chunks(&f.id, &[]);
        // Zeroing hash + size guarantees the rescan takes the re-extract path
        // (a real file is never 0 bytes with an empty hash).
        let _ = self
            .db
            .upsert_file(&self.active_space.id, &f.name, "", 0, "re-extracting");
        self.push_status(format!("re-extracting: {}", f.name));
        self.rescan_files();
    }

    /// The `reocr` popup flow lives in the view layer; this is the OCR half:
    /// force an OCR pass on one file, bypassing text extraction entirely.
    /// Useful when `pdf_extract` gives unreliable text and you want VLM OCR
    /// output instead.
    pub fn reocr_file(&mut self, name: &str) {
        let Some(f) = self.files_cache.iter().find(|f| f.name == name).cloned() else {
            return;
        };
        let ext = std::path::Path::new(&f.name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "pdf" && !crate::extract::is_image_ext(&ext) {
            self.push_status(format!("only PDFs and images support OCR: {}", f.name));
            return;
        }
        let path = self.space.files_dir(&self.active_space.name).join(&f.name);
        // Force-cancel any in-progress OCR batch so our job isn't silently dropped
        self.ocr_rx = None;
        let _ = self.db.set_file_status(&f.id, "ocr…");
        self.start_ocr(vec![(self.active_space.id.clone(), f.name.clone(), path)]);
        self.files_cache = self
            .db
            .list_files(&self.active_space.id)
            .unwrap_or_default();
        self.push_status(format!("ocr queued: {}", f.name));
    }

    /// Embed the next imported file whose chunks lack vectors, one file per
    /// job (the done-handler chains the next). Files with no extractable text
    /// receive a small filename metadata chunk so they are still searchable.
    /// No-op without a provider, without an embedding model, or while a job is
    /// already in flight.
    pub fn start_embedding(&mut self) {
        if self.embed_rx.is_some() {
            return;
        }
        let model = self.embedding_model.trim().to_string();
        if model.is_empty() {
            return;
        }
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            return;
        };
        let space_id = self.active_space.id.clone();
        let Ok(missing) = self.db.files_missing_embeddings(&space_id) else {
            return;
        };
        let Some(file) = self.db.list_files(&space_id).ok().and_then(|files| {
            files.into_iter().find(|file| {
                missing.iter().any(|id| id == &file.id) && !file.status.starts_with("ocr")
            })
        }) else {
            return;
        };
        // OCR owns files in this state.  They remain in the database work
        // queue, but must not receive a placeholder chunk while OCR is still
        // running.  The ready-file filter above prevents an OCR row from
        // blocking all other files.
        let file_id = file.id.clone();
        let Ok(mut chunks) = self.db.file_chunk_texts(&file_id) else {
            return;
        };
        if chunks.is_empty() {
            if self
                .db
                .set_file_chunks(&file_id, &crate::extract::metadata_chunks(&file.name))
                .is_err()
            {
                return;
            }
            chunks = match self.db.file_chunk_texts(&file_id) {
                Ok(chunks) => chunks,
                Err(_) => return,
            };
        }
        // Embedding is best-effort background work; outside a runtime (sync
        // unit tests) there's nowhere to run it, so just skip.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // Keep extraction/OCR status text for metadata-only and failed files;
        // the transient embedding marker is only safe to replace with "ok"
        // when the original index status was already "ok".
        let show_embedding_status = matches!(file.status.as_str(), "ok" | "embedding…");
        if show_embedding_status {
            let _ = self.db.set_file_status(&file_id, "embedding…");
        }
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
        }
        let file_name = file.name;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.embed_rx = Some(rx);
        handle.spawn(async move {
            let mut out: Vec<(i64, Vec<f32>)> = Vec::with_capacity(chunks.len());
            let mut err = None;
            for batch in chunks.chunks(64) {
                let inputs: Vec<String> = batch
                    .iter()
                    .map(|(_, text)| format!("file: {file_name}\n{text}"))
                    .collect();
                match provider.embed(&raw_model, inputs).await {
                    Ok(vecs) if vecs.len() == batch.len() => out.extend(
                        batch
                            .iter()
                            .zip(vecs)
                            .map(|((seq, _), vector)| (*seq, vector)),
                    ),
                    Ok(vecs) => {
                        err = Some(format!(
                            "embedding returned {} vectors for {} chunks",
                            vecs.len(),
                            batch.len()
                        ));
                        break;
                    }
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
    /// the next rescan retries). Search falls back to keywords while vectors
    /// are missing.
    pub fn on_embed_done(&mut self, r: Option<crate::app::EmbedMsg>) {
        let Some((space_id, file_id, result)) = r else {
            self.embed_rx = None;
            return;
        };
        self.embed_rx = None;
        let restore_status = self
            .db
            .list_files(&space_id)
            .ok()
            .and_then(|files| files.into_iter().find(|file| file.id == file_id))
            .is_some_and(|file| file.status == "embedding…");
        match result {
            Ok(vecs) => {
                let _ = self.db.set_chunk_embeddings(&file_id, &vecs);
                if restore_status {
                    let _ = self.db.set_file_status(&file_id, "ok");
                }
                self.start_embedding();
            }
            Err(e) => {
                if restore_status {
                    let _ = self.db.set_file_status(&file_id, "ok");
                }
                self.push_status(format!("embedding failed: {e}"));
            }
        }
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
        }
    }

    /// A finished OCR job: persist chunks/status, refresh the cache only if the
    /// file's space is still active. `None` = batch done (channel closed).
    #[allow(clippy::too_many_lines)]
    pub fn on_ocr_done(&mut self, r: Option<(String, String, OcrUpdate)>) {
        let Some((space_id, name, update)) = r else {
            self.ocr_rx = None;
            // PDFs imported mid-batch sat at "ocr…" unqueued; this rescan
            // chains them into a fresh batch instead of stalling until the
            // user reopens /files.
            self.rescan_files();
            return;
        };
        let Ok(files) = self.db.list_files(&space_id) else {
            return;
        };
        let Some(f) = files.iter().find(|f| f.name == name) else {
            return; // deleted mid-OCR
        };
        if !f.status.starts_with("ocr") {
            return; // re-imported mid-OCR — this result is for stale content
        }
        let completed = matches!(update, OcrUpdate::Done(_));
        match update {
            OcrUpdate::Stage(s) => {
                // Keep the "ocr" prefix — the stale-check above depends on it.
                let _ = self.db.set_file_status(&f.id, &format!("ocr: {s}"));
                if space_id == self.active_space.id {
                    self.push_status(format!("ocr {name}: {s}"));
                }
            }
            OcrUpdate::Progress(done, total, failed) => {
                let tail = if failed > 0 {
                    format!(" ({failed} failed)")
                } else {
                    String::new()
                };
                let _ = self
                    .db
                    .set_file_status(&f.id, &format!("ocr {done}/{total}{tail}"));
                if space_id == self.active_space.id {
                    self.push_status(format!("ocr {name}: {done}/{total} pages{tail}"));
                }
            }
            OcrUpdate::Done(Ok((text, errors))) if text.trim().is_empty() => {
                // Nothing usable came back; keep a filename metadata chunk so
                // even an image/scanned document remains searchable.
                let _ = self
                    .db
                    .set_file_chunks(&f.id, &crate::extract::metadata_chunks(&name));
                // Nothing usable came back; say exactly why if we know.
                let status = match errors.first() {
                    Some((i, e)) => {
                        format!("all pages failed (p{}: {})", i + 1, clip_err(e))
                    }
                    None => "no text (ocr found nothing)".to_string(),
                };
                let _ = self.db.set_file_status(&f.id, &status);
                if space_id == self.active_space.id {
                    self.push_status(format!("ocr {name}: {status}"));
                }
            }
            OcrUpdate::Done(Ok((text, errors))) => {
                let chunks = crate::extract::chunk_lines(&text);
                let chunks = if chunks.is_empty() {
                    crate::extract::metadata_chunks(&name)
                } else {
                    chunks
                };
                let _ = self.db.set_file_chunks(&f.id, &chunks);
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

                // Rename pasted images (uuid.ext) to uuid-<slug>.ext for @-completion.
                if let Some(new_name) = Self::descriptive_paste_name(f, &text) {
                    let dir = self.space.files_dir(&self.active_space.name);
                    let old_path = dir.join(&f.name);
                    let new_path = dir.join(&new_name);
                    if old_path.exists() && std::fs::rename(&old_path, &new_path).is_ok() {
                        let _ = self.db.rename_file(&f.id, &new_name);
                        let _ = self
                            .db
                            .replace_file_ref_in_messages(&space_id, &f.name, &new_name);
                        if space_id == self.active_space.id {
                            self.push_status(format!("ocr done: {new_name}"));
                        }
                        // f.name needs the updated name for the message below.
                    } else if space_id == self.active_space.id {
                        self.push_status(format!("ocr done: {name}"));
                    }
                } else if space_id == self.active_space.id {
                    self.push_status(format!("ocr done: {name}"));
                }
            }
            OcrUpdate::Done(Err(msg)) => {
                // A failed first OCR pass has no chunks to embed. Preserve old
                // extracted text when re-OCRing an already indexed file, but
                // create metadata for a file that has never been indexed.
                if self
                    .db
                    .file_chunk_texts(&f.id)
                    .unwrap_or_default()
                    .is_empty()
                {
                    let _ = self
                        .db
                        .set_file_chunks(&f.id, &crate::extract::metadata_chunks(&name));
                }
                let _ = self.db.set_file_status(&f.id, &msg);
                if space_id == self.active_space.id {
                    self.push_status(format!("ocr {name}: {msg}"));
                }
            }
        }
        if completed && space_id == self.active_space.id {
            self.start_embedding();
        }
        if space_id == self.active_space.id {
            self.files_cache = self.db.list_files(&space_id).unwrap_or_default();
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

    /// Domain half of the files popup's delete: remove the disk copy and
    /// index rows, refresh the cache. The view owns the mode/selection state.
    pub fn delete_file(&mut self, name: &str) -> Result<()> {
        let Some(f) = self.files_cache.iter().find(|f| f.name == name).cloned() else {
            return Ok(());
        };
        let disk = self.space.files_dir(&self.active_space.name).join(&f.name);
        if disk.exists() {
            std::fs::remove_file(&disk).with_context(|| format!("removing {}", disk.display()))?;
        }
        self.db.delete_file(&f.id)?;
        self.push_status(format!("removed {}", f.name));
        self.rescan_files();
        Ok(())
    }

    /// Domain half of the files popup's rename: move the file on disk; the
    /// rescan swaps the index rows (old name dropped, new name re-extracted).
    /// Returns an error message string when the target already exists or the
    /// name is invalid; the view turns it into a status line.
    pub fn rename_file(&mut self, name: &str, new: &str) -> Result<()> {
        if new.is_empty() || new == name {
            return Ok(());
        }
        if new.contains(['/', '\\']) || new == "." || new == ".." {
            anyhow::bail!("invalid name: {new}");
        }
        let dir = self.space.files_dir(&self.active_space.name);
        if dir.join(new).exists() {
            anyhow::bail!("{new} already exists");
        }
        std::fs::rename(dir.join(name), dir.join(new))
            .with_context(|| format!("renaming {name} to {new}"))?;
        self.rescan_files();
        self.push_status(format!("renamed {name} to {new}"));
        Ok(())
    }

    /// If `f` is a pasted image (UUID.ext), generate a descriptive name
    /// `uuid-<slug>.ext` from OCR text. Returns None for non-pasted files.
    fn descriptive_paste_name(f: &FileRow, ocr_text: &str) -> Option<String> {
        let stem = std::path::Path::new(&f.name).file_stem()?.to_str()?;
        let ext = std::path::Path::new(&f.name).extension()?.to_str()?;
        // Only rename files whose stem is a UUID (pasted images).
        if !is_uuid_like(stem) {
            return None;
        }
        let slug = Self::slug_from_ocr(ocr_text)?;
        Some(format!("{stem}-{slug}.{ext}"))
    }

    /// Generate a `snake_case` name from OCR text. Takes first N meaningful words
    /// and slugifies them. Returns None if text is empty or has no words.
    fn slug_from_ocr(text: &str) -> Option<String> {
        let words: Vec<&str> = text
            .split_whitespace()
            .filter(|w| {
                let w = w.trim_matches(|c: char| !c.is_alphanumeric());
                w.len() > 2 && w.chars().any(char::is_alphanumeric)
            })
            .collect();
        if words.is_empty() {
            return None;
        }
        let slug: String = words
            .iter()
            .take(5)
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .collect::<Vec<_>>()
            .join("_");
        if slug.is_empty() { None } else { Some(slug) }
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
        App::new(db, Some("k"), space)
    }

    #[tokio::test]
    async fn embedder_queue_backfills_chains_and_stops_on_error() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let id = a.db.upsert_file(&space, "b.txt", "h", 1, "ok").unwrap();
        a.db.set_file_chunks(&id, &[("l".into(), "text".into())])
            .unwrap();

        // No provider → no-op.
        let saved = a.backends.clone();
        a.backends = crate::app::Backends::default();
        a.start_embedding();
        assert!(a.embed_rx.is_none());
        a.backends = saved;

        // Blank embedding model → no-op.
        let m = std::mem::take(&mut a.embedding_model);
        a.start_embedding();
        assert!(a.embed_rx.is_none());
        a.embedding_model = m;

        // Missing vectors + provider → queued, status flips.
        a.start_embedding();
        assert!(a.embed_rx.is_some());
        let files = a.db.list_files(&space).unwrap();
        assert!(
            files[0].status.starts_with("embedding"),
            "{}",
            files[0].status
        );

        // Success: vectors stored, status ok, file leaves the missing list.
        a.on_embed_done(Some((
            space.clone(),
            id.clone(),
            Ok(vec![(0, vec![1.0f32, 0.0])]),
        )));
        assert!(a.db.files_missing_embeddings(&space).unwrap().is_empty());
        assert_eq!(a.db.list_files(&space).unwrap()[0].status, "ok");

        // Error: status restored, no re-queue (don't hammer a dead endpoint).
        a.db.set_file_chunks(&id, &[("l".into(), "new".into())])
            .unwrap();
        a.on_embed_done(Some((space.clone(), id.clone(), Err("offline".into()))));
        assert!(a.embed_rx.is_none());
        let (_, status) = a.drain_ui_events();
        assert!(status.contains("embedding failed"));
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
        let hits = crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn generic_code_and_binary_files_get_index_chunks() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page.html"), "<main>searchable markup</main>").unwrap();
        std::fs::write(dir.join("job.py"), "def searchable_code():\n    return 42").unwrap();
        // A binary file cannot provide content to a text embedding model, but
        // its metadata still makes the upload searchable by name.
        std::fs::write(dir.join("asset.bin"), [0u8, 1, 2, 3]).unwrap();

        a.rescan_files();
        for name in ["page.html", "job.py", "asset.bin"] {
            let file = a.files_cache.iter().find(|file| file.name == name).unwrap();
            assert!(a.db.file_has_chunks(&file.id).unwrap(), "{name}");
        }
        let hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "searchable", 8)
                .unwrap();
        assert_eq!(hits.len(), 2, "code files should be FTS indexed");
        let binary_hits =
            crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "asset.bin", 8)
                .unwrap();
        assert_eq!(
            binary_hits.len(),
            1,
            "binary upload should be name-searchable"
        );
    }

    #[test]
    fn cold_cache_after_restore_reindexes_from_disk() {
        // A real file db: the cold-cache scenario only exists when the
        // sibling cache.db can be deleted underneath the durable db (a
        // restore does exactly that — backup excludes cache.db).
        let root = std::env::temp_dir().join(format!("nexus-cold-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        let db = Db::open(&space.db_path()).unwrap();
        let mut a = App::new(db, Some("k"), space);

        let src = std::env::temp_dir().join(format!("nexus-cold-src-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&src, "# report\nresilience is a property").unwrap();
        let name = a.import_file(&src).unwrap();
        let id = a.files_cache[0].id.clone();
        assert!(a.db.file_indexed(&id).unwrap());
        assert!(
            crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, &name)
                .unwrap()
                .is_some()
        );
        // A restore runs with the app closed — drop the connections so the
        // cache file can actually go away.
        let root = a.space.root.clone();
        drop(a);

        // Restore: durable db survives, cache.db is dropped.
        let cache_path = root.join("cache.db");
        assert!(cache_path.exists());
        std::fs::remove_file(&cache_path).unwrap();

        let mut a = App::new(
            Db::open(&root.join("nexus.db")).unwrap(),
            Some("k"),
            Space { root },
        );
        assert!(!a.db.file_indexed(&id).unwrap());

        // The rescan must not trust the stat skip on an unchanged file — it
        // re-extracts and rewrites the index state.
        a.rescan_files();
        assert!(a.db.file_indexed(&id).unwrap());
        assert_eq!(
            crate::db::file_text(a.db.conn_for_test(), &a.active_space.id, &name)
                .unwrap()
                .as_deref(),
            Some("# report\nresilience is a property")
        );
        assert_eq!(a.db.list_files(&a.active_space.id).unwrap()[0].status, "ok");
    }

    #[test]
    fn ollama_ocr_body_uses_native_generate_shape() {
        let body = ollama_ocr_body("glm-ocr", "QUFB");
        assert_eq!(body["model"], "glm-ocr");
        assert_eq!(body["stream"], false);
        assert_eq!(body["images"][0], "QUFB"); // raw base64, not a data URL
        assert!(body["prompt"].as_str().unwrap().contains("furigana"));
        assert!(
            body.get("messages").is_none(),
            "must not be OpenAI chat shape"
        );
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
        a.backends = crate::app::Backends::default();
        assert!(a.ocr_backend().is_none());
    }

    #[tokio::test]
    async fn ocr_pull_success_switches_engine_to_local() {
        let mut a = test_app();
        a.on_ocr_pull(Some(Ok("glm-ocr".to_string())));
        assert_eq!(a.ocr_engine, "local");
        let (_, status) = a.drain_ui_events();
        assert!(status.contains("local OCR ready"));
        a.on_ocr_pull(Some(Err("ollama not installed — get it".to_string())));
        assert_eq!(a.ocr_engine, "local"); // engine untouched on failure
        let (_, status) = a.drain_ui_events();
        assert!(status.contains("ollama not installed"));
    }

    #[test]
    fn ocr_statuses_surface_stages_failures_and_reasons() {
        let mut a = test_app();
        let space = a.active_space.id.clone();
        let id =
            a.db.upsert_file(&space, "scan.pdf", "h", 1, "ocr…")
                .unwrap();

        // Stage → visible phase, still "ocr"-prefixed (stale-check depends on it).
        a.on_ocr_done(Some((
            space.clone(),
            "scan.pdf".into(),
            OcrUpdate::Stage("rendering pages (300 dpi)…".into()),
        )));
        let status = a.db.list_files(&space).unwrap()[0].status.clone();
        assert_eq!(status, "ocr: rendering pages (300 dpi)…");

        // Progress with failures shows the count.
        a.on_ocr_done(Some((
            space.clone(),
            "scan.pdf".into(),
            OcrUpdate::Progress(5, 10, 2),
        )));
        assert_eq!(
            a.db.list_files(&space).unwrap()[0].status,
            "ocr 5/10 (2 failed)"
        );
        assert!(a.last_status().contains("5/10 pages (2 failed)"));

        // Partial success keeps the first failure's reason in the status.
        a.on_ocr_done(Some((
            space.clone(),
            "scan.pdf".into(),
            OcrUpdate::Done(Ok((
                "[page 1]\ntext".to_string(),
                vec![
                    (2, "timeout after 600s".to_string()),
                    (4, "boom".to_string()),
                ],
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
        assert!(
            status.starts_with("all pages failed (p1: cannot reach ollama"),
            "{status}"
        );
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
        a.db.set_file_chunks(&id, &[("p1".into(), "garbage".into())])
            .unwrap();

        a.reextract_file("doc.txt");
        assert!(
            a.last_status().contains("re-extracting"),
            "{}",
            a.last_status()
        );
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
        a.db.upsert_file(&a.active_space.id, "book.txt", "sentinel", f.size, "ok")
            .unwrap();
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

        // Collides with an existing name: rejected, nothing moves.
        assert!(a.rename_file("old.txt", "taken.txt").is_err());
        assert!(dir.join("old.txt").exists());

        // Bad name rejected.
        assert!(a.rename_file("old.txt", "../evil.txt").is_err());

        // Valid rename: disk moves, index follows.
        a.rename_file("old.txt", "new.txt").unwrap();
        assert!(!dir.join("old.txt").exists());
        assert!(dir.join("new.txt").exists());
        assert!(a.files_cache.iter().any(|f| f.name == "new.txt"));
    }

    #[test]
    fn delete_removes_disk_file_and_row() {
        let mut a = test_app();
        let dir = a.space.files_dir(&a.active_space.name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gone.txt"), "bye").unwrap();
        a.rescan_files();
        a.delete_file("gone.txt").unwrap();
        assert!(a.files_cache.is_empty());
        assert!(!dir.join("gone.txt").exists());
    }

    #[test]
    fn import_file_copies_and_indexes_typed_path() {
        let mut a = test_app();
        let src = std::env::temp_dir().join(format!("nexus-add-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&src, "typed in").unwrap();
        let name = a.import_file(&src).unwrap();
        assert_eq!(a.files_cache.len(), 1);
        assert_eq!(a.files_cache[0].name, name);

        // A bad path is an error, and the cache stays unchanged.
        assert!(
            a.import_file(std::path::Path::new("/definitely/not/a/file"))
                .is_err()
        );
        assert_eq!(a.files_cache.len(), 1);
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
        let id =
            a.db.upsert_file(&a.active_space.id, "scan.pdf", "h", 9, "ocr…")
                .unwrap();
        let _ = id;
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "scan.pdf".to_string(),
            OcrUpdate::Done(Ok((
                "[page 1]\nquarterly revenue table".to_string(),
                Vec::new(),
            ))),
        )));
        assert_eq!(a.files_cache[0].status, "ok");
        let hits = crate::db::search_chunks(a.db.conn_for_test(), &a.active_space.id, "revenue", 8)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn on_ocr_progress_updates_status_and_status_line() {
        let mut a = test_app();
        a.db.upsert_file(&a.active_space.id, "scan.pdf", "h", 9, "ocr…")
            .unwrap();
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "scan.pdf".to_string(),
            OcrUpdate::Progress(3, 10, 0),
        )));
        assert_eq!(a.files_cache[0].status, "ocr 3/10");
        assert!(a.last_status().contains("3/10"), "{}", a.last_status());

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
        a.db.upsert_file(&a.active_space.id, "blank.pdf", "h1", 9, "ocr…")
            .unwrap();
        a.db.upsert_file(&a.active_space.id, "bad.pdf", "h2", 9, "ocr…")
            .unwrap();

        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "blank.pdf".to_string(),
            OcrUpdate::Done(Ok((String::new(), Vec::new()))),
        )));
        a.on_ocr_done(Some((
            a.active_space.id.clone(),
            "bad.pdf".to_string(),
            OcrUpdate::Done(Err(
                "scanned pdf — install tesseract + poppler for ocr".to_string()
            )),
        )));

        let by_name = |a: &App, n: &str| {
            a.files_cache
                .iter()
                .find(|f| f.name == n)
                .unwrap()
                .status
                .clone()
        };
        assert_eq!(by_name(&a, "blank.pdf"), "no text (ocr found nothing)");
        assert_eq!(
            by_name(&a, "bad.pdf"),
            "scanned pdf — install tesseract + poppler for ocr"
        );
    }

    #[test]
    fn on_ocr_done_for_inactive_space_writes_db_but_not_cache() {
        let mut a = test_app();
        let other = a.db.create_space("other").unwrap();
        a.db.upsert_file(&other.id, "scan.pdf", "h", 9, "ocr…")
            .unwrap();

        a.on_ocr_done(Some((
            other.id.clone(),
            "scan.pdf".to_string(),
            OcrUpdate::Done(Ok(("found text".to_string(), Vec::new()))),
        )));

        assert!(
            a.files_cache.is_empty(),
            "active-space cache must not show other space's file"
        );
        let rows = a.db.list_files(&other.id).unwrap();
        assert_eq!(rows[0].status, "ok");

        // Deleted-mid-OCR: result for a row that no longer exists is a no-op.
        a.on_ocr_done(Some((
            other.id.clone(),
            "gone.pdf".to_string(),
            OcrUpdate::Done(Ok(("x".to_string(), Vec::new()))),
        )));
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
