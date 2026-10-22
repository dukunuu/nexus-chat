//! Turning any file into text the index and the model can read.
//!
//! [`to_text`] is the one entry point: it detects the file's type from its
//! content (the extension is only a hint), converts it to Markdown-ish text
//! with a built-in converter, and says how it was read. Indexing, the
//! `files` tool, the workspace `fs` tool and web fetches all go through it,
//! so what the model reads is what was indexed — and a binary file is never
//! decoded as text.
//!
//! Built-in converters (no external tools): plain text, HTML, SVG, CSV/TSV,
//! RTF, OOXML (docx/pptx/xlsx), ODF, EPUB, mail (eml/mbox), notebooks,
//! subtitles and PDF text. Slower or heavier conversions are *planned* here
//! and run later in the background ([`external`]): pandoc, antiword/catdoc,
//! `LibreOffice`, ffprobe/whisper, and the user's `[extract.converters]`
//! hooks. Scanned PDFs and images are handed to the OCR pipeline.

mod detect;
pub mod external;
pub mod formats;
pub mod html;
mod markup;

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

pub use detect::{Kind, detect, detect_head, extension, vision_mime};
pub use external::{Converted, ExtractConfig, Hook};

const CHUNK_LINES: usize = 40;

/// Bumped whenever extraction output changes for files already indexed: the
/// rescan re-extracts any file indexed by an older extractor, once.
pub const EXTRACTOR_VERSION: i64 = 1;

/// Files up to this size are read whole; bigger ones are only sniffed (text
/// is read up to the limit, media is handled by path, the rest is metadata).
const MAX_READ: u64 = 64 * 1024 * 1024;

/// Documents (PDF, office, EPUB) are read whole up to this size.
const MAX_DOCUMENT: u64 = 1024 * 1024 * 1024;

/// What happened when a file was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `Extracted::text` is the file's text (possibly empty = nothing in it).
    Text,
    /// A scanned PDF or an image: the OCR / caption pipeline takes over.
    NeedsOcr,
    /// A background converter (named) will run; `text` is what the built-in
    /// converter already produced, if anything.
    Pending(&'static str),
    /// No text can be had; only metadata is indexed. Holds the status.
    Metadata(String),
    /// Reading failed.
    Failed(String),
}

/// The result of reading one file.
#[derive(Debug, Clone)]
pub struct Extracted {
    /// Markdown-ish text. Never raw bytes of a binary file.
    pub text: String,
    /// The type, detected from content.
    pub kind: Kind,
    /// Human-readable extra facts (zip members, printable strings, …) that
    /// go into the metadata chunk when there is no text.
    pub notes: Vec<String>,
    /// How the text was produced (`html → text`); `None` = read as is.
    pub via: Option<String>,
    pub outcome: Outcome,
}

impl Extracted {
    fn new(kind: Kind) -> Self {
        Self {
            text: String::new(),
            kind,
            notes: Vec::new(),
            via: None,
            outcome: Outcome::Text,
        }
    }

    fn with_text(kind: Kind, text: String, via: Option<&str>) -> Self {
        Self {
            text,
            via: via.map(str::to_string),
            ..Self::new(kind)
        }
    }

    fn metadata(kind: Kind, status: String) -> Self {
        Self {
            outcome: Outcome::Metadata(status),
            ..Self::new(kind)
        }
    }

    /// The status for the files list and the chunks to index, for a file
    /// called `name` of `size` bytes.
    pub fn index(&self, name: &str, size: u64) -> (String, Vec<(String, String)>) {
        let metadata = |reason: &str| metadata_chunk(name, self, size, reason);
        match &self.outcome {
            Outcome::NeedsOcr => ("ocr…".to_string(), Vec::new()),
            Outcome::Failed(e) => (
                format!("error: {e}"),
                vec![metadata(&format!("error: {e}"))],
            ),
            Outcome::Metadata(status) => (status.clone(), vec![metadata(status)]),
            Outcome::Pending(tool) => {
                let chunks = chunk_lines(&self.text);
                let chunks = if chunks.is_empty() {
                    vec![metadata("waiting for a converter")]
                } else {
                    chunks
                };
                (format!("convert: {tool}…"), chunks)
            }
            Outcome::Text => {
                let chunks = chunk_lines(&self.text);
                if chunks.is_empty() {
                    let status = if self.kind == Kind::Text {
                        "no text (scanned?)".to_string()
                    } else {
                        format!("no text ({})", self.kind.label())
                    };
                    (status.clone(), vec![metadata(&status)])
                } else {
                    let status = self
                        .via
                        .as_ref()
                        .map_or_else(|| "ok".to_string(), |v| format!("ok · {v}"));
                    (status, chunks)
                }
            }
        }
    }
}

/// Whether a files-list status means "read fine" (`ok`, `ok · html → text`,
/// `ok — 2 pages failed …`).
pub fn status_is_ok(status: &str) -> bool {
    status == "ok" || status.starts_with("ok ")
}

/// Whether a status marks background work in flight: OCR or a converter.
pub fn status_is_queued(status: &str) -> bool {
    status.starts_with("ocr") || status.starts_with("convert:")
}

/// Read `path` and convert it to text, using the real environment's
/// converters (`PATH` tools and `[extract.converters]` hooks).
pub fn to_text(path: &Path) -> Extracted {
    to_text_with(path, &ExtractConfig::load())
}

/// [`to_text`] with explicit converter settings.
pub fn to_text_with(path: &Path, cfg: &ExtractConfig) -> Extracted {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let size = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) => return failed(Kind::Binary("unknown"), format!("reading {name}: {e}")),
    };
    if size <= MAX_READ {
        return match std::fs::read(path) {
            Ok(bytes) => to_text_bytes(&bytes, name, cfg),
            Err(e) => failed(Kind::Binary("unknown"), format!("reading {name}: {e}")),
        };
    }
    // Too big to hold: sniff the head and handle by kind.
    let mut head = vec![0u8; detect::SNIFF_BYTES];
    let read = std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    head.truncate(read);
    let kind = detect_head(&head, name);
    let ext = extension(name);
    // Documents are parsed whole anyway (a PDF's xref sits at its end, a
    // zip's directory too), so a big book or scan is still read, as before.
    let document = kind == Kind::Pdf
        || matches!(
            ext.as_str(),
            "pdf" | "docx" | "pptx" | "xlsx" | "odt" | "ods" | "odp" | "epub"
        );
    if document && size <= MAX_DOCUMENT {
        return match std::fs::read(path) {
            Ok(bytes) => to_text_bytes(&bytes, name, cfg),
            Err(e) => failed(kind, format!("reading {name}: {e}")),
        };
    }
    if kind.is_text() {
        let mut bytes = Vec::new();
        let ok = std::fs::File::open(path)
            .and_then(|f| {
                std::io::Read::read_to_end(&mut std::io::Read::take(f, MAX_READ), &mut bytes)
            })
            .is_ok();
        if ok {
            let mut ex = to_text_bytes(&bytes, name, cfg);
            ex.notes
                .push(format!("only the first {} MiB were read", MAX_READ >> 20));
            return ex;
        }
    }
    let mut ex = Extracted::metadata(
        kind,
        format!(
            "metadata only: {} too large to convert ({} MiB)",
            kind.label(),
            size >> 20
        ),
    );
    if matches!(kind, Kind::Audio(_) | Kind::Video(_))
        && let Some(tool) = external::plan(kind, &ext, true, cfg)
    {
        ex.outcome = Outcome::Pending(tool);
    }
    ex
}

fn failed(kind: Kind, message: String) -> Extracted {
    Extracted {
        outcome: Outcome::Failed(message),
        ..Extracted::new(kind)
    }
}

/// Convert in-memory file contents. `name` is only an extension hint.
#[allow(clippy::too_many_lines)]
pub fn to_text_bytes(bytes: &[u8], name: &str, cfg: &ExtractConfig) -> Extracted {
    use formats::OfficeKind;
    let kind = detect(bytes, name);
    let ext = extension(name);
    let text_of = |k: Kind| {
        detect::text_encoding(&bytes[..bytes.len().min(detect::SNIFF_BYTES)])
            .map(|enc| detect::decode_text(bytes, enc))
            .ok_or_else(|| anyhow::anyhow!("not text ({})", k.label()))
    };
    let converted = |r: Result<String>, via: &str| match r {
        Ok(text) => Extracted::with_text(kind, text, Some(via)),
        Err(e) => failed(kind, format!("{e:#}")),
    };
    let mut ex = match kind {
        Kind::Pdf => match pdf_text(bytes) {
            Ok(text) if text.trim().is_empty() => Extracted {
                outcome: Outcome::NeedsOcr,
                ..Extracted::new(kind)
            },
            Ok(text) => Extracted::with_text(kind, text, None),
            Err(e) => failed(kind, format!("{e:#}")),
        },
        Kind::Docx => converted(
            formats::office_text(bytes, &OfficeKind::Docx),
            "docx → text",
        ),
        Kind::Pptx => converted(
            formats::office_text(bytes, &OfficeKind::Pptx),
            "pptx → text",
        ),
        Kind::Xlsx => converted(
            formats::office_text(bytes, &OfficeKind::Xlsx),
            "xlsx → text",
        ),
        Kind::Odt | Kind::Ods | Kind::Odp => converted(
            formats::odf_text(bytes),
            &format!("{} → text", kind.label()),
        ),
        Kind::Epub => converted(formats::epub_text(bytes), "epub → text"),
        Kind::Rtf => converted(text_of(kind).map(|t| formats::rtf_text(&t)), "rtf → text"),
        Kind::Html => converted(text_of(kind).map(|t| html::to_markdown(&t)), "html → text"),
        Kind::Svg => converted(text_of(kind).map(|t| formats::svg_text(&t)), "svg → text"),
        Kind::Csv | Kind::Tsv => converted(
            text_of(kind).map(|t| {
                formats::delimited_text(
                    &t,
                    if kind == Kind::Tsv {
                        '\t'
                    } else {
                        delimiter(&t)
                    },
                )
            }),
            &format!("{} → table", kind.label()),
        ),
        Kind::Eml => Extracted::with_text(kind, formats::eml_text(bytes), Some("eml → text")),
        Kind::Mbox => Extracted::with_text(kind, formats::mbox_text(bytes), Some("mbox → text")),
        Kind::Ipynb => match text_of(kind).and_then(|t| formats::ipynb_text(&t)) {
            Ok(text) => Extracted::with_text(kind, text, Some("notebook → text")),
            // Not a notebook after all: it is still JSON text.
            Err(_) => Extracted::with_text(
                Kind::Json,
                text_of(kind).unwrap_or_default().trim().to_string(),
                None,
            ),
        },
        Kind::Srt | Kind::Vtt => converted(
            text_of(kind).map(|t| formats::subtitles_text(&t, kind == Kind::Vtt)),
            "subtitles → text",
        ),
        Kind::Json | Kind::Xml | Kind::Yaml | Kind::Toml | Kind::Markup(_) | Kind::Text => {
            match text_of(kind) {
                Ok(text) => Extracted::with_text(kind, text.trim().to_string(), None),
                Err(e) => failed(kind, format!("{e:#}")),
            }
        }
        k if k.is_ocr_image() => Extracted {
            outcome: Outcome::NeedsOcr,
            ..Extracted::new(kind)
        },
        Kind::Heic | Kind::Avif => Extracted::metadata(
            kind,
            format!(
                "metadata only: {} image, no converter — convert it to png or jpeg",
                kind.label()
            ),
        ),
        Kind::Office(label) => Extracted::metadata(
            kind,
            format!(
                "metadata only: {label} document, no converter — install libreoffice (or antiword/catdoc for doc) or add [extract.converters]"
            ),
        ),
        Kind::Audio(c) | Kind::Video(c) => Extracted::metadata(
            kind,
            format!(
                "metadata only: {} ({c}), no transcriber — install ffmpeg and whisper or add [extract.converters]",
                if matches!(kind, Kind::Video(_)) {
                    "video"
                } else {
                    "audio"
                }
            ),
        ),
        Kind::Archive(l) | Kind::Binary(l) => {
            let mut e = Extracted::metadata(
                kind,
                format!(
                    "binary: {l}, no converter — install pandoc/libreoffice or add [extract.converters]"
                ),
            );
            if matches!(kind, Kind::Archive("zip" | "ooxml")) {
                e.notes.extend(zip_listing(bytes));
            }
            if matches!(kind, Kind::Binary(_)) {
                e.notes.extend(printable_strings(bytes));
            }
            e
        }
        _ => failed(kind, "unsupported file".to_string()),
    };
    // A configured hook or an installed converter takes over (or finishes
    // what the built-in one could not).
    let base_empty = ex.text.trim().is_empty();
    if matches!(
        ex.outcome,
        Outcome::Text | Outcome::Metadata(_) | Outcome::NeedsOcr
    ) && let Some(tool) = external::plan(kind, &ext, base_empty, cfg)
    {
        ex.outcome = Outcome::Pending(tool);
    }
    ex
}

/// Comma, or semicolon when the first lines use semicolons.
fn delimiter(text: &str) -> char {
    let head: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
    if head.matches(';').count() > head.matches(',').count() {
        ';'
    } else {
        ','
    }
}

/// The type of the file at `path`, judged from its first bytes only (a zip
/// is reported as an archive). `Binary("unknown")` if it cannot be read.
pub fn detect_path(path: &Path) -> Kind {
    let mut head = vec![0u8; detect::SNIFF_BYTES];
    let Ok(read) =
        std::fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut head))
    else {
        return Kind::Binary("unknown");
    };
    head.truncate(read);
    detect_head(
        &head,
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default(),
    )
}

/// Run the background converter for the file at `path` (see
/// [`Outcome::Pending`]).
pub fn convert_file(path: &Path, cfg: &ExtractConfig) -> std::result::Result<Converted, String> {
    let mut head = vec![0u8; detect::SNIFF_BYTES];
    let read = std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .map_err(|e| e.to_string())?;
    head.truncate(read);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    // Zip containers need the whole archive to be told apart.
    let kind = if head.starts_with(b"PK") {
        std::fs::metadata(path)
            .ok()
            .filter(|m| m.len() <= MAX_READ)
            .and_then(|_| std::fs::read(path).ok())
            .map_or_else(|| detect_head(&head, name), |b| detect(&b, name))
    } else {
        detect_head(&head, name)
    };
    external::convert(path, kind, cfg)
}

/// Text for a fetched web resource: its type comes from the bytes, the
/// `Content-Type` is only a hint. Binary content becomes a one-line note
/// instead of mojibake.
pub fn from_fetched(bytes: &[u8], content_type: &str) -> String {
    let ct = content_type.to_lowercase();
    let mut ex = to_text_bytes(bytes, "", &ExtractConfig::none());
    if ct.contains("html") && ex.kind == Kind::Text && matches!(ex.outcome, Outcome::Text) {
        ex.text = html::to_markdown(&String::from_utf8_lossy(bytes));
    }
    match ex.outcome {
        Outcome::Failed(e) if ex.kind == Kind::Pdf => format!("[could not extract PDF text: {e}]"),
        Outcome::Text | Outcome::Pending(_) if !ex.text.trim().is_empty() => {
            ex.text.trim().to_string()
        }
        Outcome::NeedsOcr if ex.kind == Kind::Pdf => {
            "[scanned PDF: no text layer to extract]".to_string()
        }
        _ if ct.contains("application/pdf") && ex.kind != Kind::Pdf && !ex.kind.is_text() => {
            "[could not extract PDF text: the response is not a PDF]".to_string()
        }
        _ => format!(
            "[binary content: {}, {} bytes — not readable as text]",
            ex.kind.label(),
            bytes.len()
        ),
    }
}

/// Names inside a zip, as metadata notes.
fn zip_listing(bytes: &[u8]) -> Vec<String> {
    let Ok(zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let names: Vec<&str> = zip.file_names().take(40).collect();
    if names.is_empty() {
        return Vec::new();
    }
    let more = zip.len().saturating_sub(names.len());
    let mut line = format!("contains: {}", names.join(", "));
    if more > 0 {
        let _ = write!(line, " (+{more} more)");
    }
    vec![line]
}

/// Printable runs of at least six characters, kept only when they look like
/// real words (mostly letters, not base64 or hex), capped. Gives an
/// unknown binary a few searchable hints (embedded titles, paths, names).
pub fn printable_strings(bytes: &[u8]) -> Vec<String> {
    const MAX_STRINGS: usize = 20;
    const MAX_CHARS: usize = 1500;
    let mut out: Vec<String> = Vec::new();
    let mut total = 0;
    let mut run = String::new();
    let consider = |run: &mut String, out: &mut Vec<String>, total: &mut usize| {
        let s = run.trim().to_string();
        run.clear();
        let letters = s.chars().filter(|c| c.is_alphabetic()).count();
        let has_word_break = s.contains(' ') || s.contains('.') || s.contains('/');
        let looks_real = s.chars().count() >= 6
            && letters * 10 >= s.chars().count() * 7
            && (has_word_break || s.chars().count() <= 24)
            && !s.chars().all(|c| c.is_ascii_hexdigit());
        if looks_real && !out.contains(&s) && *total + s.len() <= MAX_CHARS {
            *total += s.len();
            out.push(s);
        }
    };
    for &b in bytes.iter().take(1 << 20) {
        if (0x20..0x7F).contains(&b) || b == b'\t' {
            run.push(char::from(b));
        } else {
            consider(&mut run, &mut out, &mut total);
        }
        if out.len() >= MAX_STRINGS {
            break;
        }
    }
    consider(&mut run, &mut out, &mut total);
    if out.is_empty() {
        out
    } else {
        vec![format!("strings: {}", out.join(" | "))]
    }
}

/// A searchable fallback for a file whose contents are empty, binary, or
/// otherwise not extractable.  Keeping this in the FTS/embedding input means
/// every imported file still has an index entry and can be found by name.
pub fn metadata_chunks(name: &str) -> Vec<(String, String)> {
    vec![(
        "file metadata".to_string(),
        format!(
            "uploaded file: {name}\nNo text could be extracted from this file; search by its file name."
        ),
    )]
}

/// The metadata chunk for a file read as `ex`: name, detected type, size and
/// why there is no text, plus any notes (zip members, printable strings).
fn metadata_chunk(name: &str, ex: &Extracted, size: u64, reason: &str) -> (String, String) {
    let mut text = format!(
        "uploaded file: {name}\ntype: {} ({})\nsize: {} bytes\nNo text could be extracted from this file; search by its file name.\nstatus: {reason}",
        ex.kind.label(),
        ex.kind.mime(),
        size,
    );
    for note in &ex.notes {
        text.push('\n');
        text.push_str(note);
    }
    ("file metadata".to_string(), text)
}

/// A PDF's text with its outline (table of contents) in front. `Ok("")`
/// means a PDF without a text layer (scanned).
pub fn pdf_text(bytes: &[u8]) -> Result<String> {
    let toc = pdf_toc(bytes);
    let text = pdf_extract::extract_text_from_mem(bytes)
        .map(|t| t.trim().to_string())
        .context("extracting pdf text")?;
    if toc.is_empty() {
        Ok(text)
    } else if text.is_empty() {
        Ok(toc)
    } else {
        Ok(format!("{toc}\n\n{text}"))
    }
}

/// Extract the PDF outline/bookmarks tree as indented text, or empty string
/// if the PDF has no outline (most scanned PDFs won't).
fn pdf_toc(bytes: &[u8]) -> String {
    use pdf_extract::{Document, Object};
    let Ok(doc) = Document::load_mem(bytes) else {
        return String::new();
    };
    let Ok(catalog) = doc.catalog() else {
        return String::new();
    };
    let Ok(outlines_obj) = catalog.get(b"Outlines") else {
        return String::new();
    };
    let first_id = match outlines_obj {
        Object::Reference(id) => *id,
        _ => return String::new(),
    };
    let Ok(outlines) = doc.get_object(first_id) else {
        return String::new();
    };
    let Ok(dict) = outlines.as_dict() else {
        return String::new();
    };
    let first = match dict.get(b"First") {
        Ok(Object::Reference(id)) => *id,
        _ => return String::new(),
    };
    let mut out = String::from("=== Table of Contents ===\n");
    walk_outline(&doc, first, 0, &mut out);
    if out == "=== Table of Contents ===\n" {
        return String::new();
    }
    out
}

fn decode_pdf_string(bytes: &[u8]) -> String {
    // UTF-16BE with BOM
    if bytes.len() >= 2
        && bytes[0] == 0xFE
        && bytes[1] == 0xFF
        && let Ok(s) = String::from_utf16(
            &bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_be_bytes(*chunk))
                .collect::<Vec<_>>(),
        )
    {
        return s;
    }
    // Fallback: PDFDocEncoding / Latin-1
    bytes.iter().map(|&b| b as char).collect()
}

fn walk_outline(
    doc: &pdf_extract::Document,
    id: pdf_extract::ObjectId,
    depth: usize,
    out: &mut String,
) {
    use pdf_extract::Object;
    let Ok(item) = doc.get_object(id) else {
        return;
    };
    let Ok(dict) = item.as_dict() else {
        return;
    };
    if let Ok(Object::String(title_bytes, _)) = dict.get(b"Title") {
        let title = decode_pdf_string(title_bytes);
        if !title.is_empty() {
            let indent = "  ".repeat(depth);
            // ponytail: no page number extraction from Dest — the PDF
            // outline's Dest can be a named destination, an explicit
            // page reference, or absent. Page numbers from the outline
            // would require resolving the Dest against the page tree,
            // which is complex and rarely critical.
            let _ = writeln!(out, "{indent}{title}");
        }
    }
    // Children
    if let Ok(Object::Reference(child_id)) = dict.get(b"First") {
        walk_outline(doc, *child_id, depth + 1, out);
    }
    // Siblings
    if let Ok(Object::Reference(next_id)) = dict.get(b"Next") {
        walk_outline(doc, *next_id, depth, out);
    }
}

/// Split extracted text into ~40-line chunks labeled with their line range.
pub fn chunk_lines(text: &str) -> Vec<(String, String)> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    lines
        .chunks(CHUNK_LINES)
        .enumerate()
        .map(|(i, chunk)| {
            let first = i * CHUNK_LINES + 1;
            let last = first + chunk.len() - 1;
            (format!("lines {first}-{last}"), chunk.join("\n"))
        })
        .collect()
}

/// Whether a lowercased file extension is a supported image type.
pub fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp")
}

/// Why OCR failed: the tools aren't installed (user-fixable hint) vs a real
/// failure (surfaced as an error status).
#[derive(Debug)]
pub enum OcrError {
    MissingTools,
    Failed(String),
}

/// OCR a (scanned) PDF with pdftoppm + tesseract. Pages join with `[page N]`
/// marker lines — same inline-marker convention as pptx's `[slide N]`.
/// `Ok("")` means the tools ran but found no text. `progress` is called after
/// each finished page with (pages done, total pages).
pub fn ocr_pdf(
    path: &Path,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> std::result::Result<String, OcrError> {
    ocr_pdf_with("pdftoppm", "tesseract", path, progress)
}

/// Binary names are parameters so tests can exercise the missing-tools path
/// without mutating the process-global PATH.
fn ocr_pdf_with(
    pdftoppm: &str,
    tesseract: &str,
    path: &Path,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> std::result::Result<String, OcrError> {
    let tmp = std::env::temp_dir().join(format!("nexus-ocr-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).map_err(|e| OcrError::Failed(e.to_string()))?;
    let result = ocr_pdf_in(pdftoppm, tesseract, path, &tmp, progress);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Run a command, mapping a missing binary to `OcrError::MissingTools` and a
/// non-zero exit to `Failed` with its stderr.
fn run_ocr_cmd(
    cmd: &mut std::process::Command,
    name: &str,
) -> std::result::Result<Vec<u8>, OcrError> {
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

/// Render a PDF's pages to PNGs in `tmp` with pdftoppm, returned in document
/// order (pdftoppm zero-pads page numbers, so a lexical sort is page order).
pub fn render_pdf_pages(
    pdftoppm: &str,
    path: &Path,
    tmp: &Path,
    dpi: u32,
    gray: bool,
) -> std::result::Result<Vec<std::path::PathBuf>, OcrError> {
    let mut cmd = std::process::Command::new(pdftoppm);
    cmd.args(["-r", &dpi.to_string()]);
    if gray {
        cmd.arg("-gray");
    }
    run_ocr_cmd(cmd.arg("-png").arg(path).arg(tmp.join("page")), "pdftoppm")?;
    let mut pages: Vec<std::path::PathBuf> = std::fs::read_dir(tmp)
        .map_err(|e| OcrError::Failed(e.to_string()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    pages.sort();
    Ok(pages)
}

/// Join per-page OCR results with `[page N]` markers: blank pages are
/// dropped, failed pages leave a `[page N: ocr failed]` marker so the rest
/// of the document still lands.
pub fn join_pages(results: &[std::result::Result<String, String>]) -> String {
    let mut text = String::new();
    for (i, r) in results.iter().enumerate() {
        match r {
            Ok(p) if p.trim().is_empty() => {}
            Ok(p) => {
                let _ = write!(text, "[page {}]\n{}\n", i + 1, p.trim());
            }
            Err(_) => {
                let _ = writeln!(text, "[page {}: ocr failed]", i + 1);
            }
        }
    }
    text.trim().to_string()
}

fn ocr_pdf_in(
    pdftoppm: &str,
    tesseract: &str,
    path: &Path,
    tmp: &Path,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> std::result::Result<String, OcrError> {
    let run = run_ocr_cmd;

    // 200 dpi is ~2x faster to render and OCR than 300 with negligible
    // accuracy loss on normal print.
    let pages = render_pdf_pages(pdftoppm, path, tmp, 200, true)?;

    // Recognize with every installed language pack (eng+jpn+…): tesseract
    // then picks the right script per line itself, so installing a tessdata
    // pack is all it takes to support a language. None = listing failed;
    // fall back to tesseract's default (eng).
    let langs = installed_langs(tesseract);

    // OCR pages in parallel — one tesseract process per core, pulling page
    // indices off a shared counter; results re-ordered by index afterwards.
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(pages.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::with_capacity(pages.len()));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(png) = pages.get(i) else { return };
                    let mut cmd = std::process::Command::new(tesseract);
                    cmd.arg(png).arg("stdout");
                    if let Some(l) = &langs {
                        cmd.args(["-l", l]);
                    }
                    let r = run(&mut cmd, "tesseract");
                    let done = {
                        let mut res = results.lock().unwrap();
                        res.push((i, r));
                        res.len()
                    };
                    progress(done, pages.len());
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);

    let mut text = String::new();
    for (i, r) in results {
        let stdout = r?;
        let page = String::from_utf8_lossy(&stdout);
        let page = page.trim();
        if !page.is_empty() {
            let _ = writeln!(text, "[page {}]\n{page}", i + 1);
        }
    }
    Ok(text.trim().to_string())
}

/// All installed tesseract language packs joined as "eng+jpn+…" (minus the
/// osd script-detection pack), or None if listing fails. Older tesseracts
/// print the list to stderr, newer to stdout — scan both; language codes
/// never contain spaces, which filters the header line.
fn installed_langs(tesseract: &str) -> Option<String> {
    let out = std::process::Command::new(tesseract)
        .arg("--list-langs")
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let langs: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(' ') && *l != "osd")
        .collect();
    (!langs.is_empty()).then(|| langs.join("+"))
}

/// Build a minimal valid PDF: one page, optionally with `text` drawn in
/// Helvetica. Offsets are computed at runtime so the xref is always correct.
/// Test fixture shared with `app::files` tests.
#[cfg(test)]
pub(crate) fn minimal_pdf(text: Option<&str>) -> Vec<u8> {
    if let Some(t) = text {
        pdf_with_pages(&[t])
    } else {
        let objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 150] >>".to_string(),
        ];
        serialize_pdf(&objs)
    }
}

/// A PDF with one page per entry in `texts`, each drawn in Helvetica.
#[cfg(test)]
pub(crate) fn pdf_with_pages(texts: &[&str]) -> Vec<u8> {
    let font_obj = 2 + 2 * texts.len() + 1; // catalog, pages, (page, content)*, font
    let mut objs: Vec<String> = Vec::new();
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..texts.len())
        .map(|i| format!("{} 0 R", 3 + 2 * i))
        .collect();
    objs.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        texts.len()
    ));
    for (i, t) in texts.iter().enumerate() {
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 150] /Contents {} 0 R /Resources << /Font << /F1 {font_obj} 0 R >> >> >>",
            4 + 2 * i
        ));
        let stream = format!("BT /F1 32 Tf 20 60 Td ({t}) Tj ET");
        objs.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    serialize_pdf(&objs)
}

#[cfg(test)]
fn serialize_pdf(objs: &[String]) -> Vec<u8> {
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

/// OCR a still image with tesseract. `Ok("")` means the tool ran and found
/// no text.
pub fn ocr_image(path: &Path) -> std::result::Result<String, OcrError> {
    ocr_image_with("tesseract", path)
}

fn ocr_image_with(tesseract: &str, path: &Path) -> std::result::Result<String, OcrError> {
    let mut args = vec![path.to_string_lossy().to_string(), "stdout".to_string()];
    if let Some(langs) = installed_langs(tesseract) {
        args.push("-l".to_string());
        args.push(langs);
    }
    let out = external::run_capped(
        Path::new(tesseract),
        &args,
        &std::env::temp_dir(),
        std::time::Duration::from_secs(180),
        8 * 1024 * 1024,
    )
    .map_err(|e| match e {
        external::RunError::Missing => OcrError::MissingTools,
        other => OcrError::Failed(other.to_string()),
    })?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-extract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `bytes` as `name` in a fresh temp dir and read it back.
    fn read(name: &str, bytes: &[u8]) -> Extracted {
        let path = temp_dir().join(name);
        std::fs::write(&path, bytes).unwrap();
        to_text_with(&path, &ExtractConfig::none())
    }

    fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            for (name, body) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn plain_text_reads_as_is() {
        let ex = read("notes.md", b"# hi\nbody");
        assert_eq!(ex.text, "# hi\nbody");
        assert_eq!(ex.kind, Kind::Text);
        assert_eq!(ex.index("notes.md", 9).0, "ok");
    }

    #[test]
    fn unknown_binary_never_becomes_text_and_gets_a_metadata_chunk() {
        // Replaces `unknown_binary_is_an_error`: a binary is no longer an
        // error, it is indexed by metadata with a status that says why.
        let ex = read("blob.bin", &[0u8, 159, 146, 150]);
        assert!(ex.text.is_empty(), "binary must never be decoded as text");
        assert_eq!(ex.kind, Kind::Binary("unknown"));
        let (status, chunks) = ex.index("blob.bin", 4);
        assert_eq!(
            status,
            "binary: unknown, no converter — install pandoc/libreoffice or add [extract.converters]"
        );
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "file metadata");
        assert!(chunks[0].1.contains("uploaded file: blob.bin"));
        assert!(chunks[0].1.contains("size: 4 bytes"));
    }

    #[test]
    fn html_is_converted_not_indexed_as_markup() {
        // Replaces the html half of `generic_code_files_are_plain_text`.
        let dir = temp_dir();
        for (name, body, expected) in [
            ("page.html", "<main>hello</main>", "hello"),
            ("job.py", "print(42)", "print(42)"),
            (
                "view.php",
                "<!DOCTYPE html><p>x</p>",
                "<!DOCTYPE html><p>x</p>",
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            assert_eq!(
                to_text_with(&path, &ExtractConfig::none()).text,
                expected,
                "{name}"
            );
        }
        let ex = read("page.html", b"<h1>T</h1><script>x()</script><p>body</p>");
        assert_eq!(ex.index("page.html", 1).0, "ok · html → text");
    }

    #[test]
    fn utf16_code_files_are_decoded_before_binary_detection() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend("print(42)".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(read("script.py", &bytes).text, "print(42)");
    }

    #[test]
    fn metadata_chunk_makes_empty_files_searchable_by_name() {
        let chunks = metadata_chunks("archive.zip");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "file metadata");
        assert!(chunks[0].1.contains("archive.zip"));
    }

    #[test]
    fn empty_extraction_gets_no_text_status() {
        let ex = read("empty.txt", b"   ");
        assert_eq!(ex.index("empty.txt", 3).0, "no text (scanned?)");
        let ex = read("empty.html", b"<html><script>x</script></html>");
        assert_eq!(ex.index("empty.html", 3).0, "no text (html)");
    }

    #[test]
    fn pdf_is_detected_by_content_whatever_the_name() {
        let pdf = pdf_with_pages(&["HELLO PDF"]);
        for name in ["doc.pdf", "download", "renamed.txt", "SCAN.PDF"] {
            let ex = read(name, &pdf);
            assert_eq!(ex.kind, Kind::Pdf, "{name}");
            assert!(ex.text.contains("HELLO PDF"), "{name}: {:?}", ex.text);
            assert_eq!(ex.index(name, 1).0, "ok");
        }
        // No text layer: handed to OCR, not decoded.
        let ex = read("scan.pdf", &minimal_pdf(None));
        assert_eq!(ex.outcome, Outcome::NeedsOcr);
        assert_eq!(ex.index("scan.pdf", 1), ("ocr…".to_string(), Vec::new()));
        // A broken PDF is an error with a metadata chunk, never garbage text.
        let ex = read("broken.pdf", b"%PDF-1.7\nnot really a pdf \x00\x01\x02");
        let (status, chunks) = ex.index("broken.pdf", 20);
        assert!(status.starts_with("error:"), "{status}");
        assert!(ex.text.is_empty() && chunks.len() == 1);
    }

    #[test]
    fn office_documents_are_told_apart_by_structure_not_extension() {
        let docx = zip_bytes(&[
            ("[Content_Types].xml", "<Types/>"),
            ("word/document.xml", "<w:p><w:t>quarterly plan</w:t></w:p>"),
        ]);
        let ex = read("plan.zip", &docx);
        assert_eq!(ex.kind, Kind::Docx);
        assert_eq!(ex.text, "quarterly plan");
        assert_eq!(ex.index("plan.zip", 1).0, "ok · docx → text");
        let odt = zip_bytes(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            ("content.xml", "<text:p>odf body</text:p>"),
        ]);
        assert_eq!(read("x.docx", &odt).text, "odf body");
    }

    #[test]
    fn text_formats_convert_with_a_status_naming_the_conversion() {
        let cases: Vec<(&str, Vec<u8>, &str, &str)> = vec![
            ("a.rtf", br"{\rtf1 Hello\par world}".to_vec(), "Hello\nworld", "ok · rtf → text"),
            ("a.csv", b"a,b\n1,2\n".to_vec(), "table preview", "ok · csv → table"),
            ("a.srt", b"1\n00:00:01,000 --> 00:00:02,000\nhi\n".to_vec(), "[00:00:01] hi", "ok · subtitles → text"),
            (
                "a.ipynb",
                br##"{"cells":[{"cell_type":"markdown","source":"# nb"}],"metadata":{},"nbformat":4}"##.to_vec(),
                "# nb",
                "ok · notebook → text",
            ),
            ("a.svg", b"<svg><text>label</text></svg>".to_vec(), "label", "ok · svg → text"),
            ("a.eml", b"From: a@b.c\nSubject: Hi\n\nbody text\n".to_vec(), "body text", "ok · eml → text"),
        ];
        for (name, bytes, needle, status) in cases {
            let ex = read(name, &bytes);
            assert!(ex.text.contains(needle), "{name}: {:?}", ex.text);
            assert_eq!(ex.index(name, 1).0, status, "{name}");
        }
        // JSON stays as it is.
        let ex = read("a.json", br#"{"a": 1}"#);
        assert_eq!(ex.text, r#"{"a": 1}"#);
        assert_eq!(ex.index("a.json", 1).0, "ok");
        // Not actually a notebook: kept as JSON text.
        let ex = read("fake.ipynb", br#"{"not": "a notebook"}"#);
        assert_eq!(ex.text, r#"{"not": "a notebook"}"#);
    }

    #[test]
    fn images_go_to_ocr_and_unreadable_images_are_metadata_only() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0; 16]);
        let ex = read("shot.dat", &png);
        assert_eq!(
            (ex.kind, ex.outcome.clone()),
            (Kind::Png, Outcome::NeedsOcr)
        );
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0";
        let (status, chunks) = read("p.heic", heic).index("p.heic", 20);
        assert!(status.starts_with("metadata only: heic image"), "{status}");
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn archives_and_media_without_tools_are_metadata_only_with_a_clear_status() {
        let zip = zip_bytes(&[("a.txt", "x"), ("dir/b.bin", "y")]);
        let ex = read("bundle.docx", &zip);
        let (status, chunks) = ex.index("bundle.docx", 1);
        assert!(status.starts_with("binary: zip, no converter"), "{status}");
        assert!(
            chunks[0].1.contains("contains: a.txt, dir/b.bin"),
            "{}",
            chunks[0].1
        );
        assert!(ex.text.is_empty());
        let mp3 = b"ID3\x04\0\0\0\0\0\x10junk";
        let (status, _) = read("song.mp3", mp3).index("song.mp3", 1);
        assert!(
            status.starts_with("metadata only: audio (mp3), no transcriber"),
            "{status}"
        );
        let legacy = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        let (status, _) = read("old.doc", &legacy).index("old.doc", 1);
        assert!(
            status.starts_with("metadata only: doc document, no converter"),
            "{status}"
        );
    }

    #[test]
    fn printable_strings_keep_words_and_drop_noise() {
        let mut blob = vec![0u8, 1, 2];
        blob.extend_from_slice(b"Quarterly Report 2024\0");
        blob.extend_from_slice(b"AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHH0123456789+/==\0");
        blob.extend_from_slice(b"short\0");
        blob.extend_from_slice(b"/usr/share/fonts/readable\0");
        blob.extend_from_slice(&[0xFF; 4]);
        let notes = printable_strings(&blob);
        assert_eq!(notes.len(), 1);
        let line = &notes[0];
        assert!(
            line.contains("Quarterly Report 2024") && line.contains("/usr/share/fonts/readable"),
            "{line}"
        );
        assert!(
            !line.contains("AAAABBBB") && !line.contains("short"),
            "{line}"
        );
        assert!(printable_strings(&[0u8; 100]).is_empty());
    }

    #[test]
    fn oversized_files_are_sniffed_not_slurped() {
        let path = temp_dir().join("huge.bin");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(MAX_READ + 1).unwrap();
        let ex = to_text_with(&path, &ExtractConfig::none());
        assert!(ex.text.is_empty());
        let (status, chunks) = ex.index("huge.bin", MAX_READ + 1);
        assert!(
            status.starts_with("metadata only: unknown too large to convert (64 MiB)"),
            "{status}"
        );
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn fetched_content_is_text_or_a_clear_placeholder() {
        let pdf = pdf_with_pages(&["FETCHED PDF"]);
        assert!(from_fetched(&pdf, "").contains("FETCHED PDF"));
        assert!(from_fetched(&pdf, "application/pdf").contains("FETCHED PDF"));
        assert_eq!(
            from_fetched(b"<html><body><p>hi there</p></body></html>", "text/html"),
            "hi there"
        );
        assert_eq!(
            from_fetched(b"<p>fragment</p>", "text/html; charset=utf-8"),
            "fragment"
        );
        assert_eq!(
            from_fetched(br#"{"ok":true}"#, "application/json"),
            r#"{"ok":true}"#
        );
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\0\0\0\0\0";
        let placeholder = from_fetched(png, "image/png");
        assert!(
            placeholder.starts_with("[binary content: png, 16 bytes"),
            "{placeholder}"
        );
        let not_pdf = from_fetched(&[1, 0, 2, 0, 3], "application/pdf");
        assert!(not_pdf.contains("not a PDF"), "{not_pdf}");
        let zipped = zip_bytes(&[
            ("word/document.xml", "<w:p><w:t>fetched docx</w:t></w:p>"),
            ("[Content_Types].xml", "<T/>"),
        ]);
        assert_eq!(
            from_fetched(&zipped, "application/octet-stream"),
            "fetched docx"
        );
    }

    #[test]
    fn image_ocr_reports_a_missing_tool_distinctly() {
        let path = temp_dir().join("a.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(matches!(
            ocr_image_with("nexus-definitely-not-a-binary", &path),
            Err(OcrError::MissingTools)
        ));
    }

    #[test]
    fn status_helpers_classify_known_statuses() {
        for ok in ["ok", "ok · html → text", "ok — 2 pages failed (p1: x)"] {
            assert!(status_is_ok(ok), "{ok}");
        }
        for not_ok in [
            "ocr…",
            "error: x",
            "no text (scanned?)",
            "okay",
            "embedding…",
        ] {
            assert!(!status_is_ok(not_ok), "{not_ok}");
        }
        for q in [
            "ocr…",
            "ocr 3/10",
            "ocr: rendering",
            "convert: pandoc…",
            "convert: soffice",
        ] {
            assert!(status_is_queued(q), "{q}");
        }
        for not_q in ["ok", "conversion failed: x", "error: ocr: x"] {
            assert!(!status_is_queued(not_q), "{not_q}");
        }
    }

    #[cfg(unix)]
    mod with_tools {
        use super::*;
        use crate::extract::external::fake_tools::Bin;

        #[test]
        fn an_installed_converter_makes_the_file_pending_with_built_in_text_as_a_stand_in() {
            let bin = Bin::new();
            bin.tool("pandoc", "echo '# converted'");
            let dir = temp_dir();
            let path = dir.join("doc.rst");
            std::fs::write(&path, "Title\n=====\nbody").unwrap();
            let cfg = bin.cfg();
            let ex = to_text_with(&path, &cfg);
            assert_eq!(ex.outcome, Outcome::Pending("pandoc"));
            let (status, chunks) = ex.index("doc.rst", 16);
            assert_eq!(status, "convert: pandoc…");
            assert!(
                chunks[0].1.contains("body"),
                "built-in text is searchable meanwhile"
            );
            assert!(status_is_queued(&status));
            let done = convert_file(&path, &cfg).unwrap();
            assert_eq!(
                (done.via.as_str(), done.text.as_str()),
                ("pandoc", "# converted")
            );
        }

        #[test]
        fn a_user_hook_overrides_the_built_in_converter() {
            let bin = Bin::new();
            bin.tool("tomd", "echo hooked");
            let mut cfg = bin.cfg();
            cfg.hooks.push(Hook {
                key: "rtf".into(),
                argv: vec!["tomd".into(), "{in}".into()],
            });
            let path = temp_dir().join("a.rtf");
            std::fs::write(&path, r"{\rtf1 built-in}").unwrap();
            let ex = to_text_with(&path, &cfg);
            assert_eq!(ex.outcome, Outcome::Pending("hook"));
            assert_eq!(ex.text, "built-in");
            assert_eq!(convert_file(&path, &cfg).unwrap().text, "hooked");
        }

        #[test]
        fn audio_with_ffprobe_is_pending_and_without_tools_is_metadata_only() {
            let bin = Bin::new();
            bin.tool("ffprobe", "echo '{}'");
            let path = temp_dir().join("a.mp3");
            std::fs::write(&path, b"ID3\x04\0\0\0\0\0\x10junk").unwrap();
            assert_eq!(
                to_text_with(&path, &bin.cfg()).outcome,
                Outcome::Pending("ffprobe")
            );
            assert!(matches!(
                to_text_with(&path, &ExtractConfig::none()).outcome,
                Outcome::Metadata(_)
            ));
        }
    }

    #[test]
    fn chunks_are_40_lines_with_locations() {
        let text = (1..=90)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_lines(&text);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, "lines 1-40");
        assert!(chunks[0].1.starts_with("1\n"));
        assert_eq!(chunks[1].0, "lines 41-80");
        assert_eq!(chunks[2].0, "lines 81-90");
        assert!(
            chunks[2].1.ends_with("\n90")
                || chunks[2].1 == "81\n82\n83\n84\n85\n86\n87\n88\n89\n90"
        );
        assert!(chunk_lines("").is_empty());
        assert!(chunk_lines("   \n  ").is_empty());
    }

    /// True when tesseract + pdftoppm are runnable (real-OCR tests skip otherwise).
    fn ocr_tools_present() -> bool {
        std::process::Command::new("tesseract")
            .arg("--version")
            .output()
            .is_ok()
            && std::process::Command::new("pdftoppm")
                .arg("-v")
                .output()
                .is_ok()
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

        let text = ocr_pdf(&pdf, &|_, _| {}).unwrap();
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
        assert_eq!(ocr_pdf(&pdf, &|_, _| {}).unwrap(), "");
    }

    #[test]
    fn render_pdf_pages_renders_sorted_pages_at_dpi() {
        if !ocr_tools_present() {
            eprintln!("skipping render_pdf_pages_renders_sorted_pages_at_dpi: tools not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("nexus-render-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("multi.pdf");
        std::fs::write(&pdf, pdf_with_pages(&["ONE", "TWO", "THREE"])).unwrap();
        let tmp = dir.join("pages");
        std::fs::create_dir_all(&tmp).unwrap();

        let pages = render_pdf_pages("pdftoppm", &pdf, &tmp, 120, false).unwrap();
        assert_eq!(pages.len(), 3);
        let mut sorted = pages.clone();
        sorted.sort();
        assert_eq!(pages, sorted, "pages must come back in document order");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn join_pages_marks_failures_and_skips_blank_pages() {
        let joined = join_pages(&[
            Ok("first".to_string()),
            Err("boom".to_string()),
            Ok("   ".to_string()),
            Ok("fourth".to_string()),
        ]);
        assert!(joined.contains("[page 1]\nfirst"), "{joined:?}");
        assert!(joined.contains("[page 2: ocr failed]"), "{joined:?}");
        assert!(!joined.contains("[page 3]"), "{joined:?}");
        assert!(joined.contains("[page 4]\nfourth"), "{joined:?}");
    }

    #[test]
    fn ocr_pdf_multipage_keeps_page_order() {
        if !ocr_tools_present() {
            eprintln!("skipping ocr_pdf_multipage_keeps_page_order: tools not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("nexus-ocr-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("multi.pdf");
        std::fs::write(
            &pdf,
            pdf_with_pages(&["ALPHA BRAVO", "CHARLIE DELTA", "ECHO FOXTROT"]),
        )
        .unwrap();

        let text = ocr_pdf(&pdf, &|_, _| {}).unwrap();
        // Parallel OCR must still emit pages in document order.
        let a = text.find("ALPHA").expect(&text);
        let c = text.find("CHARLIE").expect(&text);
        let e = text.find("ECHO").expect(&text);
        assert!(a < c && c < e, "pages out of order: {text:?}");
        assert!(text.contains("[page 3]"), "{text:?}");
    }

    #[test]
    fn installed_langs_lists_packs_without_osd() {
        if !ocr_tools_present() {
            eprintln!("skipping installed_langs_lists_packs_without_osd: tools not installed");
            return;
        }
        let langs = installed_langs("tesseract").expect("langs should list");
        assert!(langs.split('+').any(|l| l == "eng"), "{langs}");
        assert!(!langs.split('+').any(|l| l == "osd"), "{langs}");
    }

    #[test]
    fn installed_langs_missing_binary_is_none() {
        assert!(installed_langs("nexus-definitely-not-a-binary").is_none());
    }

    #[test]
    fn ocr_pdf_missing_tools_is_distinguishable() {
        let dir = std::env::temp_dir().join(format!("nexus-ocr-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("x.pdf");
        std::fs::write(&pdf, minimal_pdf(None)).unwrap();
        let err = ocr_pdf_with(
            "nexus-definitely-not-a-binary",
            "tesseract",
            &pdf,
            &|_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, OcrError::MissingTools));
    }
}
