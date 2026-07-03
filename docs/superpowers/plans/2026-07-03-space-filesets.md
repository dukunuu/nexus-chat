# Space Filesets + Image Transcription Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each space owns a fileset (PDF, PPTX/DOCX/XLSX, text) the model reads on demand via `search_files`/`read_file` tools, plus clipboard-image paste that transcribes through a small vision model into the composer.

**Architecture:** Files live in `spaces/<name>/files/`; text is extracted once at import (pure Rust) into an FTS5-indexed `file_chunks` table in the existing SQLite db. The system prompt carries only a file list; content reaches the model through two new ToolBox tools that open their own short-lived read-only db connections. Image paste grabs the clipboard via `arboard`, PNG-encodes, and makes one non-streaming OpenRouter vision call; the transcript lands in the composer.

**Tech Stack:** Rust, rusqlite (bundled SQLite w/ FTS5), pdf-extract, zip, sha2, png, base64, arboard (existing), reqwest/OpenRouter (existing).

**Spec:** `docs/superpowers/specs/2026-07-03-space-filesets-design.md`

## Global Constraints

- Minimum visibility rule: every new item gets the narrowest visibility that compiles (private → `pub(super)` → `pub(crate)` → `pub`). Grep real call sites before choosing.
- `cargo build` must stay warning-free; `cargo test` must pass after every task.
- Commit staging: stage only the files you touched by exact path (`git add <paths>`), never `git add -A` / `git add .`. Run `git status` before committing.
- No new UI frameworks or config files; settings persist through the existing `app_settings` kv table.
- `read_file` tool returns at most 200 lines per call. Chunks are 40 lines. Search returns at most 8 hits.
- Default transcriber model: `google/gemini-2.5-flash-lite`.
- No `quick-xml` dependency — office XML text is pulled with the same string-scanning style as `parse_ddg_html` in `src/tools.rs`.
- Follow existing file/module patterns: popup files in `src/ui/popups/`, App state methods in `src/app/<area>.rs`, popup key handling via the `classify_browse_key`/`classify_edit_key`/`classify_confirm_delete_key` helpers in `src/ui/popups/mod.rs`.

---

## Phase 1 — storage + extraction

### Task 1: Dependencies + db schema and file queries

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/db.rs`

**Interfaces:**
- Produces (used by Tasks 3, 4, 5):
  - `pub struct FileRow { pub id: String, pub space_id: String, pub name: String, pub hash: String, pub size: i64, pub status: String }`
  - `Db::upsert_file(&self, space_id: &str, name: &str, hash: &str, size: i64, status: &str) -> Result<String>` (returns file id; replaces chunks-owning row on re-import)
  - `Db::list_files(&self, space_id: &str) -> Result<Vec<FileRow>>`
  - `Db::delete_file(&self, file_id: &str) -> Result<()>`
  - `Db::set_file_chunks(&self, file_id: &str, chunks: &[(String, String)]) -> Result<()>` (chunks are `(location, text)`, seq = index)
  - Free functions on a raw connection (shared with ToolBox, which opens its own):
    - `pub fn search_chunks(conn: &Connection, space_id: &str, query: &str, limit: usize) -> Result<Vec<(String, String, String)>>` → `(file name, location, snippet)`
    - `pub fn file_text(conn: &Connection, space_id: &str, name: &str) -> Result<Option<String>>`
    - `pub fn count_files(conn: &Connection, space_id: &str) -> Result<u64>`
  - `pub fn fts_quote(query: &str) -> String` — sanitizes a user/model query for FTS5 MATCH

- [ ] **Step 1: Add dependencies**

```bash
cargo add pdf-extract zip sha2 png base64
```

Run: `cargo build` — expected: compiles clean (deps only).

- [ ] **Step 2: Write failing tests in `src/db.rs`**

Append to the existing `mod tests`:

```rust
#[test]
fn files_upsert_list_delete_roundtrip() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "notes.md", "h1", 10, "ok").unwrap();
    db.set_file_chunks(&id, &[("lines 1-40".into(), "hello fts world".into())]).unwrap();

    let files = db.list_files(&space).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "notes.md");
    assert_eq!(files[0].hash, "h1");
    assert_eq!(files[0].status, "ok");

    // Re-import with a new hash keeps one row (same id or replaced) and replaces chunks.
    let id2 = db.upsert_file(&space, "notes.md", "h2", 12, "ok").unwrap();
    db.set_file_chunks(&id2, &[("lines 1-40".into(), "goodbye".into())]).unwrap();
    let files = db.list_files(&space).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].hash, "h2");

    db.delete_file(&files[0].id).unwrap();
    assert!(db.list_files(&space).unwrap().is_empty());
}

#[test]
fn chunk_search_ranks_and_scopes_by_space() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let other = db.create_space("other").unwrap();
    let a = db.upsert_file(&space, "a.md", "h", 1, "ok").unwrap();
    let b = db.upsert_file(&other.id, "b.md", "h", 1, "ok").unwrap();
    db.set_file_chunks(&a, &[("lines 1-40".into(), "rust borrow checker".into())]).unwrap();
    db.set_file_chunks(&b, &[("lines 1-40".into(), "rust in other space".into())]).unwrap();

    let hits = search_chunks(&db.conn, &space, "rust", 8).unwrap();
    assert_eq!(hits.len(), 1); // other space's chunk is excluded
    assert_eq!(hits[0].0, "a.md");
    assert_eq!(hits[0].1, "lines 1-40");
    assert!(hits[0].2.contains("rust"));

    // Special characters must not be an FTS syntax error.
    assert!(search_chunks(&db.conn, &space, "c++ \"quoted\" -dash", 8).is_ok());
}

#[test]
fn file_text_joins_chunks_in_order() {
    let db = Db::open_in_memory().unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "doc.txt", "h", 1, "ok").unwrap();
    db.set_file_chunks(&id, &[
        ("lines 1-2".into(), "one\ntwo".into()),
        ("lines 3-4".into(), "three\nfour".into()),
    ]).unwrap();
    let text = file_text(&db.conn, &space, "doc.txt").unwrap().unwrap();
    assert_eq!(text, "one\ntwo\nthree\nfour");
    assert!(file_text(&db.conn, &space, "missing.txt").unwrap().is_none());
    assert_eq!(count_files(&db.conn, &space).unwrap(), 1);
}
```

Note: tests use `db.conn` — keep `conn` private but the tests live in the same module, so field access works as-is.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test files_upsert chunk_search file_text_joins`
Expected: FAIL to compile ("no method named `upsert_file`", etc.)

- [ ] **Step 4: Implement**

In `Db::migrate`, append to the `execute_batch` string (before the ALTER loop):

```sql
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    name TEXT NOT NULL,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(space_id, name)
);
CREATE VIRTUAL TABLE IF NOT EXISTS file_chunks USING fts5(
    file_id UNINDEXED,
    seq UNINDEXED,
    location UNINDEXED,
    text
);
```

Add near `Session`:

```rust
/// A file imported into a space's fileset. `status` is "ok", "no text
/// (scanned?)", "unsupported", or "error: …"; extraction text lives in the
/// `file_chunks` FTS table, not here.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: String,
    pub space_id: String,
    pub name: String,
    pub hash: String,
    pub size: i64,
    pub status: String,
}
```

Methods on `Db` (follow the existing style exactly):

```rust
// --- space filesets ---

/// Insert or replace a file row (unique per space+name). Returns the row id;
/// an existing row keeps its id, so its chunks can be replaced by file_id.
pub fn upsert_file(&self, space_id: &str, name: &str, hash: &str, size: i64, status: &str) -> Result<String> {
    if let Ok(existing) = self.conn.query_row(
        "SELECT id FROM files WHERE space_id = ?1 AND name = ?2",
        (space_id, name),
        |r| r.get::<_, String>(0),
    ) {
        self.conn.execute(
            "UPDATE files SET hash = ?2, size = ?3, status = ?4 WHERE id = ?1",
            (&existing, hash, size, status),
        )?;
        return Ok(existing);
    }
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    self.conn.execute(
        "INSERT INTO files (id, space_id, name, hash, size, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (&id, space_id, name, hash, size, status, &now),
    )?;
    Ok(id)
}

pub fn list_files(&self, space_id: &str) -> Result<Vec<FileRow>> {
    let mut stmt = self.conn.prepare(
        "SELECT id, space_id, name, hash, size, status FROM files
         WHERE space_id = ?1 ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([space_id], |r| {
        Ok(FileRow {
            id: r.get(0)?, space_id: r.get(1)?, name: r.get(2)?,
            hash: r.get(3)?, size: r.get(4)?, status: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_file(&self, file_id: &str) -> Result<()> {
    self.conn.execute("DELETE FROM file_chunks WHERE file_id = ?1", [file_id])?;
    self.conn.execute("DELETE FROM files WHERE id = ?1", [file_id])?;
    Ok(())
}

/// Replace a file's indexed chunks. `chunks` are `(location, text)` in order.
pub fn set_file_chunks(&self, file_id: &str, chunks: &[(String, String)]) -> Result<()> {
    self.conn.execute("DELETE FROM file_chunks WHERE file_id = ?1", [file_id])?;
    for (seq, (location, text)) in chunks.iter().enumerate() {
        self.conn.execute(
            "INSERT INTO file_chunks (file_id, seq, location, text) VALUES (?1, ?2, ?3, ?4)",
            (file_id, seq as i64, location, text),
        )?;
    }
    Ok(())
}
```

Free functions (bottom of db.rs, above tests). They take a raw `&Connection` because the ToolBox (Task 4) opens its own short-lived connection on the streaming task — `Db` itself stays on the UI task:

```rust
/// Quote a query for FTS5 MATCH: each whitespace token becomes a quoted
/// phrase (inner quotes doubled), so model-supplied text can't be an FTS
/// syntax error. Tokens are implicitly ANDed by FTS5.
pub fn fts_quote(query: &str) -> String {
    query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// BM25-ranked chunk search within one space: `(file name, location, snippet)`.
pub fn search_chunks(
    conn: &Connection,
    space_id: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String, String)>> {
    let q = fts_quote(query);
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT files.name, file_chunks.location,
                snippet(file_chunks, 3, '', '', '…', 24)
         FROM file_chunks JOIN files ON files.id = file_chunks.file_id
         WHERE file_chunks MATCH ?1 AND files.space_id = ?2
         ORDER BY bm25(file_chunks) LIMIT ?3",
    )?;
    let rows = stmt.query_map((q, space_id, limit as i64), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A file's full extracted text (chunks re-joined in order), by display name.
pub fn file_text(conn: &Connection, space_id: &str, name: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT file_chunks.text
         FROM file_chunks JOIN files ON files.id = file_chunks.file_id
         WHERE files.space_id = ?1 AND files.name = ?2
         ORDER BY CAST(file_chunks.seq AS INTEGER) ASC",
    )?;
    let rows = stmt.query_map((space_id, name), |r| r.get::<_, String>(0))?;
    let parts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

pub fn count_files(conn: &Connection, space_id: &str) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE space_id = ?1",
        [space_id],
        |r| r.get(0),
    )?;
    Ok(n as u64)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test` — expected: all pass (106 existing + 3 new), zero warnings.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/db.rs
git commit -m "feat: files table + FTS5 chunk index for space filesets"
```

---

### Task 2: Text extraction module

**Files:**
- Create: `src/extract.rs`
- Modify: `src/main.rs` (add `mod extract;`)

**Interfaces:**
- Produces (used by Task 3):
  - `pub(crate) fn extract_text(path: &Path) -> anyhow::Result<String>` — dispatches by extension; `Err` for unreadable/binary-unknown files; `Ok("")` is legal (e.g. scanned PDF).
  - `pub(crate) fn chunk_lines(text: &str) -> Vec<(String, String)>` — `(location, chunk)` pairs, 40 lines/chunk, locations `"lines 1-40"`, `"lines 41-80"`, …
- Consumes: nothing from other tasks.

- [ ] **Step 1: Write failing tests**

Create `src/extract.rs` with the tests first (implementation stubs come next step):

```rust
//! Import-time text extraction for space filesets: plain text as-is, PDF via
//! pdf-extract, office formats (pptx/docx/xlsx) by scanning their zipped XML
//! for text tags — same string-scanning style as tools.rs's DDG HTML parser.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal office zip in a temp dir: `entries` are (path, xml).
    fn office_fixture(name: &str, entries: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-extract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (entry, xml) in entries {
            zip.start_file(*entry, opts).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn plain_text_reads_as_is() {
        let dir = std::env::temp_dir().join(format!("nexus-extract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("notes.md");
        std::fs::write(&p, "# hi\nbody").unwrap();
        assert_eq!(extract_text(&p).unwrap(), "# hi\nbody");
    }

    #[test]
    fn unknown_binary_is_an_error() {
        let dir = std::env::temp_dir().join(format!("nexus-extract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("blob.bin");
        std::fs::write(&p, [0u8, 159, 146, 150]).unwrap();
        assert!(extract_text(&p).is_err());
    }

    #[test]
    fn docx_pulls_paragraph_text() {
        let p = office_fixture("d.docx", &[(
            "word/document.xml",
            r#"<w:document><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p><w:p><w:r><w:t>Second &amp; last</w:t></w:r></w:p></w:document>"#,
        )]);
        let text = extract_text(&p).unwrap();
        assert_eq!(text, "Hello world\nSecond & last");
    }

    #[test]
    fn pptx_pulls_slide_text_with_slide_markers() {
        let p = office_fixture("s.pptx", &[
            ("ppt/slides/slide1.xml", r#"<p:sld><a:t>Title one</a:t><a:t>Bullet</a:t></p:sld>"#),
            ("ppt/slides/slide2.xml", r#"<p:sld><a:t>Second slide</a:t></p:sld>"#),
        ]);
        let text = extract_text(&p).unwrap();
        assert!(text.contains("[slide 1]"));
        assert!(text.contains("Title one"));
        assert!(text.contains("[slide 2]"));
        assert!(text.contains("Second slide"));
    }

    #[test]
    fn xlsx_pulls_shared_strings_and_cell_values() {
        let p = office_fixture("x.xlsx", &[
            ("xl/sharedStrings.xml", r#"<sst><si><t>revenue</t></si><si><t>cost</t></si></sst>"#),
            ("xl/worksheets/sheet1.xml", r#"<worksheet><c><v>42</v></c><c><v>7</v></c></worksheet>"#),
        ]);
        let text = extract_text(&p).unwrap();
        assert!(text.contains("revenue"));
        assert!(text.contains("cost"));
        assert!(text.contains("42"));
    }

    #[test]
    fn xml_tag_texts_handles_attrs_self_closing_and_entities() {
        let xml = r#"<w:t a="b">one</w:t><w:t/><w:t>two &lt;3</w:t>"#;
        assert_eq!(xml_tag_texts(xml, "w:t"), vec!["one".to_string(), "two <3".to_string()]);
    }

    #[test]
    fn chunks_are_40_lines_with_locations() {
        let text = (1..=90).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let chunks = chunk_lines(&text);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, "lines 1-40");
        assert!(chunks[0].1.starts_with("1\n"));
        assert_eq!(chunks[1].0, "lines 41-80");
        assert_eq!(chunks[2].0, "lines 81-90");
        assert!(chunks[2].1.ends_with("\n90") || chunks[2].1 == "81\n82\n83\n84\n85\n86\n87\n88\n89\n90");
        assert!(chunk_lines("").is_empty());
        assert!(chunk_lines("   \n  ").is_empty());
    }
}
```

Add `mod extract;` to `src/main.rs` (alphabetical order in the mod list).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib extract` (or `cargo test extract`)
Expected: FAIL to compile ("cannot find function `extract_text`").

- [ ] **Step 3: Implement**

Above the tests module:

```rust
const CHUNK_LINES: usize = 40;

/// Extract searchable text from `path`, dispatching on the (lowercased)
/// extension. `Ok("")` means the file parsed but had no text (e.g. a scanned
/// PDF); `Err` means it couldn't be read/parsed at all.
pub(crate) fn extract_text(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => pdf_extract::extract_text(path)
            .map(|t| t.trim().to_string())
            .with_context(|| format!("extracting pdf {}", path.display())),
        "docx" => office_text(path, OfficeKind::Docx),
        "pptx" => office_text(path, OfficeKind::Pptx),
        "xlsx" => office_text(path, OfficeKind::Xlsx),
        // Everything else: treat as text if it looks like text.
        _ => {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let head = &bytes[..bytes.len().min(8192)];
            if head.contains(&0) {
                anyhow::bail!("unsupported binary file");
            }
            Ok(String::from_utf8_lossy(&bytes).trim().to_string())
        }
    }
}

enum OfficeKind {
    Docx,
    Pptx,
    Xlsx,
}

/// Pull text out of an OOXML zip by scanning member XML for text tags.
/// ponytail: tag scanning, not an XML parser — same approach as tools.rs's
/// DDG HTML scraping; swap in a real parser only if a document breaks it.
fn office_text(path: &Path, kind: OfficeKind) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("reading office zip")?;
    let mut out = String::new();

    // Collect entry names first (borrow rules: by_index borrows the archive).
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();
    let mut read_entry = |name: &str| -> Option<String> {
        let mut e = zip.by_name(name).ok()?;
        let mut s = String::new();
        e.read_to_string(&mut s).ok()?;
        Some(s)
    };

    match kind {
        OfficeKind::Docx => {
            if let Some(xml) = read_entry("word/document.xml") {
                // A paragraph's runs join without separators; paragraphs get newlines.
                for para in xml.split("</w:p>") {
                    let line = xml_tag_texts(para, "w:t").join("");
                    if !line.trim().is_empty() {
                        out.push_str(line.trim());
                        out.push('\n');
                    }
                }
            }
        }
        OfficeKind::Pptx => {
            let mut slides: Vec<&String> = names
                .iter()
                .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
                .collect();
            // slide2 sorts before slide10 lexically; sort by the numeric part.
            slides.sort_by_key(|n| {
                n.trim_start_matches("ppt/slides/slide")
                    .trim_end_matches(".xml")
                    .parse::<u32>()
                    .unwrap_or(0)
            });
            for name in slides {
                let n = name
                    .trim_start_matches("ppt/slides/slide")
                    .trim_end_matches(".xml");
                if let Some(xml) = read_entry(name) {
                    let texts = xml_tag_texts(&xml, "a:t");
                    if !texts.is_empty() {
                        out.push_str(&format!("[slide {n}]\n"));
                        out.push_str(&texts.join("\n"));
                        out.push('\n');
                    }
                }
            }
        }
        OfficeKind::Xlsx => {
            // Cell strings live in sharedStrings; numbers inline in each sheet.
            // ponytail: dumps values without cell positions — searchable, not a
            // faithful table; switch to the calamine crate if layout matters.
            if let Some(xml) = read_entry("xl/sharedStrings.xml") {
                out.push_str(&xml_tag_texts(&xml, "t").join("\n"));
                out.push('\n');
            }
            let mut sheets: Vec<&String> = names
                .iter()
                .filter(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
                .collect();
            sheets.sort();
            for name in sheets {
                if let Some(xml) = read_entry(name) {
                    let vals = xml_tag_texts(&xml, "v");
                    if !vals.is_empty() {
                        out.push_str(&vals.join(" "));
                        out.push('\n');
                    }
                }
            }
        }
    }
    Ok(out.trim().to_string())
}

/// Every text content of `<tag ...>text</tag>` occurrences in `xml`, entities
/// unescaped. Skips self-closing `<tag/>`.
fn xml_tag_texts(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        // Must be the exact tag: next char is '>', ' ' or '/'.
        let Some(gt) = after.find('>') else { break };
        let head = &after[..gt];
        rest = &after[gt + 1..];
        if !(head.is_empty() || head.starts_with(' ') || head.starts_with('/')) {
            continue; // a longer tag name that merely starts with `tag`
        }
        if head.ends_with('/') {
            continue; // self-closing
        }
        let Some(end) = rest.find(&close) else { break };
        let text = xml_unescape(&rest[..end]);
        if !text.is_empty() {
            out.push(text);
        }
        rest = &rest[end + close.len()..];
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Split extracted text into ~40-line chunks labeled with their line range.
pub(crate) fn chunk_lines(text: &str) -> Vec<(String, String)> {
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test extract` then `cargo test`
Expected: all pass, zero warnings. (No in-repo PDF fixture: `extract_text` for pdf is a one-line delegation to `pdf_extract::extract_text`; parsing itself is the crate's job.)

- [ ] **Step 5: Commit**

```bash
git add src/extract.rs src/main.rs
git commit -m "feat: import-time text extraction for pdf/office/text files"
```

---

### Task 3: Files dir, import + rescan on App

**Files:**
- Modify: `src/space.rs` (add `files_dir`)
- Create: `src/app/files.rs`
- Modify: `src/app/mod.rs` (App fields, `mod files;`, `FilesMode` enum)
- Modify: `src/app/spaces.rs` (`set_active_space` rescans)

**Interfaces:**
- Consumes: `Db::{upsert_file, list_files, delete_file, set_file_chunks}`, `FileRow` (Task 1); `extract::{extract_text, chunk_lines}` (Task 2).
- Produces (used by Tasks 5, 6, 7):
  - App fields: `pub files_cache: Vec<FileRow>`, `pub files_selected: usize`, `pub files_mode: FilesMode`, `pub files_edit: String`
  - `pub enum FilesMode { Browse, Add, ConfirmDelete }` (in `src/app/mod.rs`, next to the other mode enums)
  - `App::rescan_files(&mut self)` — sync dir with db, extract new/changed, refresh `files_cache`
  - `App::import_file(&mut self, path: &Path) -> Result<String>` — copy into the space's files dir, rescan; returns the imported file name
  - `Space::files_dir(&self, name: &str) -> PathBuf`

- [ ] **Step 1: Add `Space::files_dir`**

In `src/space.rs`, after `instructions_path`:

```rust
/// Directory holding a space's imported fileset (created on demand).
pub fn files_dir(&self, name: &str) -> PathBuf {
    self.space_dir(name).join("files")
}
```

- [ ] **Step 2: Write failing tests**

Create `src/app/files.rs` starting with tests (implementation next step). Test setup mirrors `src/input.rs`'s `test_app()`:

```rust
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
}
```

The search test needs read access to the private `conn`; add to `db.rs`:

```rust
#[cfg(test)]
pub fn conn_for_test(&self) -> &Connection {
    &self.conn
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test files::tests`
Expected: FAIL to compile (missing `import_file`, `rescan_files`, App fields).

- [ ] **Step 4: Implement**

In `src/app/mod.rs`:
- add `mod files;` to the module list (alphabetical),
- next to `SkillsMode`:

```rust
/// What the files popup is doing: browsing the fileset, typing a path to
/// import, or confirming removal of the highlighted file.
#[derive(PartialEq, Clone, Copy)]
pub enum FilesMode {
    Browse,
    Add,
    ConfirmDelete,
}
```

- App struct fields (near the skills popup block) + `App::new` initializers:

```rust
/// The active space's imported files (refreshed by `rescan_files`).
pub files_cache: Vec<crate::db::FileRow>,
pub files_selected: usize,
pub files_mode: FilesMode,
/// Path being typed/pasted in the files popup's Add mode.
pub files_edit: String,
```

```rust
files_cache: Vec::new(),
files_selected: 0,
files_mode: FilesMode::Browse,
files_edit: String::new(),
```

Implementation in `src/app/files.rs`:

```rust
//! Space filesets: importing files into `spaces/<name>/files/`, keeping the
//! db index in sync with the directory, and extracting searchable text.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::App;

impl App {
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

        let entries = std::fs::read_dir(&dir).map(|rd| rd.flatten().collect::<Vec<_>>()).unwrap_or_default();
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            seen.push(name.clone());
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let hash = format!("{:x}", Sha256::digest(&bytes));
            if known.iter().any(|f| f.name == name && f.hash == hash) {
                continue; // unchanged
            }
            let size = bytes.len() as i64;
            let (status, chunks) = match crate::extract::extract_text(&path) {
                Ok(text) if text.trim().is_empty() => ("no text (scanned?)".to_string(), Vec::new()),
                Ok(text) => ("ok".to_string(), crate::extract::chunk_lines(&text)),
                Err(e) => (format!("error: {e}"), Vec::new()),
            };
            if let Ok(id) = self.db.upsert_file(&self.active_space.id, &name, &hash, size, &status) {
                let _ = self.db.set_file_chunks(&id, &chunks);
            }
        }
        for gone in known.iter().filter(|f| !seen.contains(&f.name)) {
            let _ = self.db.delete_file(&gone.id);
        }
        self.files_cache = self.db.list_files(&self.active_space.id).unwrap_or_default();
        self.files_selected = self.files_selected.min(self.files_cache.len().saturating_sub(1));
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
}
```

(The `#[cfg(test)] mod tests` from Step 2 sits below this in the same file.)

In `src/app/spaces.rs`, `set_active_space` — after `self.scroll = 0;` add:

```rust
self.rescan_files();
```

(`rescan_files` sets no status, so the "space: …" status line set right after stays.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add src/space.rs src/app/files.rs src/app/mod.rs src/app/spaces.rs src/db.rs
git commit -m "feat: fileset import + rescan wired to spaces"
```

---

## Phase 2 — model access

### Task 4: `search_files` + `read_file` tools

**Files:**
- Modify: `src/tools.rs`
- Modify: `src/app/mod.rs` (both `ToolBox::new` call sites pass the files context)

**Interfaces:**
- Consumes: `db::{search_chunks, file_text, count_files, fts_quote}` (Task 1).
- Produces (used by model at runtime; Task 5 relies on the tool names):
  - `ToolBox::new` gains a final parameter `files: Option<FilesCtx>`;
    `pub struct FilesCtx { pub db_path: std::path::PathBuf, pub space_id: String }`
  - `defs()` includes `search_files` and `read_file` when the space has ≥1 file
  - `run("search_files", ...)` / `run("read_file", ...)`

- [ ] **Step 1: Write failing tests**

Append to `src/tools.rs` tests (existing `ToolBox::new(...)` call sites in tests gain a trailing `None`):

```rust
fn files_toolbox() -> (ToolBox, crate::db::Db, String) {
    // A real temp-file db (the toolbox opens its own connection by path).
    let path = std::env::temp_dir().join(format!("nexus-tools-{}.db", uuid::Uuid::new_v4()));
    let db = crate::db::Db::open(&path).unwrap();
    let space = db.default_space_id().unwrap();
    let id = db.upsert_file(&space, "report.md", "h", 1, "ok").unwrap();
    let text: String = (1..=250).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    db.set_file_chunks(&id, &crate::extract::chunk_lines(&text)).unwrap();
    let tb = ToolBox::new(
        PathBuf::new(),
        None,
        None,
        "auto".to_string(),
        Some(FilesCtx { db_path: path, space_id: space.clone() }),
    );
    (tb, db, space)
}

#[test]
fn defs_include_file_tools_only_when_files_exist() {
    let (tb, ..) = files_toolbox();
    let names: Vec<String> = tb.defs().iter().map(|d| d.name.clone()).collect();
    assert!(names.contains(&"search_files".to_string()));
    assert!(names.contains(&"read_file".to_string()));

    let empty = ToolBox::new(PathBuf::new(), None, None, "auto".to_string(), None);
    let names: Vec<String> = empty.defs().iter().map(|d| d.name.clone()).collect();
    assert!(!names.contains(&"search_files".to_string()));
}

#[tokio::test]
async fn search_files_returns_ranked_snippets() {
    let (tb, ..) = files_toolbox();
    let (result, status) = tb.run("search_files", r#"{"query":"line 42"}"#).await;
    assert!(status.contains("Searching files"));
    assert!(result.contains("report.md"));
    assert!(result.contains("lines 41-80"));
}

#[tokio::test]
async fn read_file_is_ranged_and_capped() {
    let (tb, ..) = files_toolbox();
    let (result, _) = tb.run("read_file", r#"{"name":"report.md"}"#).await;
    assert!(result.contains("line 1"));
    assert!(result.contains("line 200"));
    assert!(!result.contains("line 201")); // 200-line cap

    let (result, _) = tb.run("read_file", r#"{"name":"report.md","offset":201}"#).await;
    assert!(result.contains("line 201"));
    assert!(result.contains("line 250"));

    let (result, _) = tb.run("read_file", r#"{"name":"nope.md"}"#).await;
    assert!(result.contains("unknown file"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tools`
Expected: FAIL to compile (`FilesCtx` missing, `new` arity).

- [ ] **Step 3: Implement**

In `src/tools.rs`:

```rust
/// Where the file tools read from: the shared db plus the space to scope to.
/// The toolbox opens its own short-lived connection per call — the app's
/// `Db` handle stays on the UI task and is never shared with the stream task.
pub struct FilesCtx {
    pub db_path: std::path::PathBuf,
    pub space_id: String,
}
```

- `ToolBox` gains field `files: Option<FilesCtx>`; `new` gains the trailing parameter and stores it.
- `defs()` — after the `web_search` def:

```rust
if self.files_count() > 0 {
    defs.push(ToolDef {
        name: "search_files".to_string(),
        description: "Full-text search the space's imported files; returns ranked snippets with file name and line location.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "query": { "type": "string", "description": "keywords to search for" } },
            "required": ["query"],
        }),
    });
    defs.push(ToolDef {
        name: "read_file".to_string(),
        description: "Read the extracted text of an imported file, up to 200 lines per call. Use offset to page through longer files.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "the file's name as listed in the system prompt" },
                "offset": { "type": "integer", "description": "1-based first line to read (default 1)" },
                "limit": { "type": "integer", "description": "lines to read, max 200 (default 200)" },
            },
            "required": ["name"],
        }),
    });
}
```

Helpers + run arms:

```rust
fn files_count(&self) -> u64 {
    let Some(ctx) = &self.files else { return 0 };
    rusqlite::Connection::open(&ctx.db_path)
        .ok()
        .and_then(|conn| crate::db::count_files(&conn, &ctx.space_id).ok())
        .unwrap_or(0)
}
```

In `run()`, before the `other =>` arm (arg parsing mirrors the existing `skill`/`web_search` style):

```rust
"search_files" => {
    let query = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
        .unwrap_or_default();
    let status = "Searching files…".to_string();
    let result = match &self.files {
        None => "no files imported".to_string(),
        Some(ctx) => match rusqlite::Connection::open(&ctx.db_path)
            .map_err(anyhow::Error::from)
            .and_then(|conn| crate::db::search_chunks(&conn, &ctx.space_id, &query, 8))
        {
            Ok(hits) if hits.is_empty() => "no matches".to_string(),
            Ok(hits) => hits
                .iter()
                .map(|(name, loc, snip)| format!("{name} ({loc}): {snip}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("file search failed: {e}"),
        },
    };
    (result, status)
}
"read_file" => {
    let v = serde_json::from_str::<serde_json::Value>(args).unwrap_or_default();
    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
    let offset = v.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
    let limit = v.get("limit").and_then(|l| l.as_u64()).unwrap_or(200).clamp(1, 200) as usize;
    let status = format!("Reading {name}…");
    let result = match &self.files {
        None => "no files imported".to_string(),
        Some(ctx) => match rusqlite::Connection::open(&ctx.db_path)
            .map_err(anyhow::Error::from)
            .and_then(|conn| crate::db::file_text(&conn, &ctx.space_id, &name))
        {
            Ok(Some(text)) => {
                let lines: Vec<&str> = text.lines().collect();
                let total = lines.len();
                let start = (offset - 1).min(total);
                let slice = &lines[start..(start + limit).min(total)];
                if slice.is_empty() {
                    format!("{name}: offset {offset} is past the end ({total} lines)")
                } else {
                    format!(
                        "{name} (lines {}-{} of {total}):\n{}",
                        start + 1,
                        start + slice.len(),
                        slice.join("\n"),
                    )
                }
            }
            Ok(None) => format!("unknown file: {name}"),
            Err(e) => format!("file read failed: {e}"),
        },
    };
    (result, status)
}
```

In `src/app/mod.rs`, both `ToolBox::new` call sites (in `App::new` and `refresh_toolbox`) gain:

```rust
Some(crate::tools::FilesCtx {
    db_path: self.space.db_path(),
    space_id: self.active_space.id.clone(),
})
```

(In `App::new` the toolbox is built before `App` exists — use `space.db_path()` and `active_space.id.clone()` from the locals there.) Because the ctx captures the space id, `set_active_space` must rebuild the toolbox — Task 5 wires that.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/tools.rs src/app/mod.rs
git commit -m "feat: search_files + read_file tools over the fileset index"
```

---

### Task 5: System-prompt fileset section + toolbox refresh on space switch

**Files:**
- Modify: `src/app/chat.rs` (`files_section`, wired into `system_prompt`)
- Modify: `src/app/spaces.rs` (`set_active_space` refreshes toolbox)
- Modify: `src/app/mod.rs` (`init` rescans once at startup)

**Interfaces:**
- Consumes: `files_cache` (Task 3), tool names (Task 4).
- Produces: system prompt gains a `## Files` section when the space has files.

- [ ] **Step 1: Write failing test**

In `src/app/tests.rs` (uses the existing test-app helper in that file — reuse whatever constructor pattern its other tests use):

```rust
#[test]
fn system_prompt_lists_files_but_not_their_content() {
    let mut a = test_app();
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plan.md"), "SECRET-CONTENT-MARKER inside").unwrap();
    a.rescan_files();

    let sp = a.system_prompt();
    assert!(sp.contains("## Files"));
    assert!(sp.contains("plan.md"));
    assert!(sp.contains("search_files"));
    assert!(!sp.contains("SECRET-CONTENT-MARKER")); // names only, never content

    // No files → no section.
    std::fs::remove_file(dir.join("plan.md")).unwrap();
    a.rescan_files();
    assert!(!a.system_prompt().contains("## Files"));
}
```

(If `src/app/tests.rs` has no shared `test_app()` helper, add one mirroring `src/input.rs`'s, with a unique temp-dir `Space` root.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test system_prompt_lists_files`
Expected: FAIL — no `## Files` in the prompt.

- [ ] **Step 3: Implement**

In `src/app/chat.rs`, after `skills_section`:

```rust
/// Names/types/sizes of the space's imported files — content stays off the
/// wire until the model calls `search_files`/`read_file`.
fn files_section(&self) -> Option<String> {
    if self.files_cache.is_empty() {
        return None;
    }
    let mut s = "## Files\nThe user has imported these files into this space. Do not guess \
                 their contents: call `search_files` to find relevant passages, or \
                 `read_file` to read one (200 lines per call, use offset to page).\n"
        .to_string();
    for f in &self.files_cache {
        let kind = std::path::Path::new(&f.name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("file")
            .to_lowercase();
        s.push_str(&format!("- {} ({kind}, {}, {})\n", f.name, human_size(f.size), f.status));
    }
    Some(s.trim_end().to_string())
}
```

And a free function at the bottom of chat.rs:

```rust
/// Compact byte counts: 940 B, 1.2 KB, 3.4 MB.
pub(super) fn human_size(bytes: i64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}
```

Wire into `system_prompt()` after the skills part:

```rust
if let Some(files) = self.files_section() {
    parts.push(files);
}
```

In `src/app/spaces.rs`, `set_active_space`: after the `rescan_files()` call added in Task 3, add `self.refresh_toolbox();` (rebuilds the toolbox with the new space's `FilesCtx`; it also reloads skills, which is harmless). Note ordering: rescan first, then refresh, then the `status = format!("space: …")` line stays last.

In `src/app/mod.rs`, `init()`: add `self.rescan_files();` (startup sync so `files_cache`/system prompt are correct before the first `/files` open).

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/app/chat.rs src/app/spaces.rs src/app/mod.rs src/app/tests.rs
git commit -m "feat: fileset section in system prompt, toolbox tracks active space"
```

---

## Phase 3 — UI

### Task 6: `/files` command + popup state methods

**Files:**
- Modify: `src/app/mod.rs` (`Popup::Files` variant, `run_command` arm)
- Modify: `src/app/files.rs` (popup state methods)
- Modify: `src/input.rs` (COMMANDS entry)

**Interfaces:**
- Consumes: `FilesMode`, `files_*` fields (Task 3).
- Produces (used by Task 7):
  - `Popup::Files`
  - `App::open_files_popup(&mut self)`
  - `App::move_files_selection(&mut self, delta: i32)`
  - `App::start_files_add(&mut self)`, `App::confirm_files_add(&mut self) -> Result<()>`
  - `App::open_selected_file(&mut self)` (Enter in Browse: open in system viewer)
  - (`confirm_files_delete` exists from Task 3)

- [ ] **Step 1: Write failing tests**

Append to `src/app/files.rs` tests:

```rust
#[test]
fn files_command_opens_popup_and_rescans() {
    let mut a = test_app();
    let dir = a.space.files_dir(&a.active_space.name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("seen.txt"), "content").unwrap();
    a.run_command("files").unwrap();
    assert!(a.popup == crate::app::Popup::Files);
    assert_eq!(a.files_cache.len(), 1);
    assert!(a.files_mode == super::FilesMode::Browse);
}

#[test]
fn confirm_files_add_imports_typed_path() {
    let mut a = test_app();
    let src = std::env::temp_dir().join(format!("nexus-add-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&src, "typed in").unwrap();
    a.start_files_add();
    assert!(a.files_mode == super::FilesMode::Add);
    a.files_edit = src.to_string_lossy().to_string();
    a.confirm_files_add().unwrap();
    assert!(a.files_mode == super::FilesMode::Browse);
    assert_eq!(a.files_cache.len(), 1);

    // A bad path reports in status and stays recoverable.
    a.start_files_add();
    a.files_edit = "/definitely/not/a/file".to_string();
    a.confirm_files_add().unwrap();
    assert!(a.status.contains("not a file"));
    assert_eq!(a.files_cache.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test files::tests`
Expected: FAIL to compile (missing methods / `Popup::Files`).

- [ ] **Step 3: Implement**

`src/app/mod.rs`: add `Files` to `enum Popup`; in `run_command` add arm `"files" => self.open_files_popup(),`.

`src/input.rs`: add to `COMMANDS` (after "skills"):

```rust
Command { name: "files", desc: "space files", aliases: &["file", "attach", "upload", "docs"] },
```

`src/app/files.rs` methods (inside the existing `impl App`):

```rust
pub(super) fn open_files_popup(&mut self) {
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

/// Open the highlighted file in the system viewer (Enter in Browse).
pub fn open_selected_file(&mut self) {
    if let Some(f) = self.files_cache.get(self.files_selected) {
        let path = self.space.files_dir(&self.active_space.name).join(&f.name);
        let _ = open::that_detached(&path);
        self.status = format!("opened {}", f.name);
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs src/app/files.rs src/input.rs
git commit -m "feat: /files command + files popup state methods"
```

---

### Task 7: Files popup render + key handling + dispatch

**Files:**
- Create: `src/ui/popups/files.rs`
- Modify: `src/ui/popups/mod.rs` (declare module)
- Modify: `src/ui/mod.rs` (render dispatch arm)
- Modify: `src/events.rs` (key dispatch arm)
- Modify: `src/input.rs` (`App::paste` routes into `files_edit` in Add mode)

**Interfaces:**
- Consumes: everything from Tasks 3+6; `classify_browse_key`/`classify_edit_key`/`classify_confirm_delete_key` and their action enums from `src/ui/popups/mod.rs`; `crate::ui::centered`, `crate::ui::fmt_created` style helpers as needed; `human_size` (Task 5 — widen it from `pub(super)` in `app/chat.rs` to `pub(crate)` ONLY if the render actually uses it; grep first).
- Produces: `ui::popups::files::{render, handle_key}`.

- [ ] **Step 1: Create `src/ui/popups/files.rs`**

Follow `session.rs`'s shape exactly (`Clear` + `centered` area + `List` + title-bar-as-mode-prompt):

```rust
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::app::App;

pub(crate) fn render(f: &mut Frame, app: &App) {
    use crate::app::FilesMode;
    let area = crate::ui::centered(f.area(), 64, 60);
    f.render_widget(Clear, area);

    let dim = Style::default().fg(Color::DarkGray);
    let items: Vec<ListItem> = app
        .files_cache
        .iter()
        .map(|file| {
            let ok = file.status == "ok";
            let status_style = if ok { dim } else { Style::default().fg(Color::Yellow) };
            ListItem::new(Line::from(vec![
                Span::styled(file.name.clone(), Style::default().fg(Color::White)),
                Span::styled(format!("  {}", crate::app::human_size(file.size)), dim),
                Span::styled(format!("  {}", file.status), status_style),
            ]))
        })
        .collect();

    let title = match app.files_mode {
        FilesMode::Add => format!(" import path: {}▏  (Enter import · Esc cancel) ", app.files_edit),
        FilesMode::ConfirmDelete => {
            let name = app.files_cache.get(app.files_selected).map(|f| f.name.clone()).unwrap_or_default();
            format!(" remove \"{name}\"? (Ctrl+D confirm · Esc cancel) ")
        }
        FilesMode::Browse => crate::ui::hint_title(
            app,
            " files ",
            "files — Enter open · Ctrl+N add · Ctrl+D remove (or drop files into the space dir)",
        ),
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !app.files_cache.is_empty() {
        state.select(Some(app.files_selected.min(app.files_cache.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    use super::{BrowseAction, ConfirmDeleteAction, EditAction, classify_browse_key, classify_confirm_delete_key, classify_edit_key};
    use crate::app::FilesMode;
    match app.files_mode {
        FilesMode::Add => match classify_edit_key(key) {
            Some(EditAction::Cancel) => app.files_mode = FilesMode::Browse,
            Some(EditAction::Save) => app.confirm_files_add()?,
            Some(EditAction::Backspace) => {
                app.files_edit.pop();
            }
            Some(EditAction::Push(c)) => app.files_edit.push(c),
            None => {}
        },
        FilesMode::ConfirmDelete => match classify_confirm_delete_key(key) {
            Some(ConfirmDeleteAction::Yes) => app.confirm_files_delete()?,
            Some(ConfirmDeleteAction::No) => app.files_mode = FilesMode::Browse,
            None => {}
        },
        FilesMode::Browse => {
            if key.code == KeyCode::Enter {
                app.open_selected_file();
                return Ok(());
            }
            // Create = Add (Ctrl+N); no rename; no browse text filter (small lists).
            match classify_browse_key(key, true, false) {
                Some(BrowseAction::Close) => app.popup = crate::app::Popup::None,
                Some(BrowseAction::MoveUp) => app.move_files_selection(-1),
                Some(BrowseAction::MoveDown) => app.move_files_selection(1),
                Some(BrowseAction::Create) => app.start_files_add(),
                Some(BrowseAction::ConfirmDelete) if !app.files_cache.is_empty() => {
                    app.files_mode = FilesMode::ConfirmDelete;
                }
                _ => {}
            }
        }
    }
    Ok(())
}
```

`human_size` currently lives in `app/chat.rs` as `pub(super)`; this render calls it from outside `app` — re-export it: in `src/app/mod.rs` add `pub(crate) use chat::human_size;` and keep the fn's own visibility `pub(super)`.

- [ ] **Step 2: Wire dispatch**

- `src/ui/popups/mod.rs`: add `pub(crate) mod files;` (alphabetical).
- `src/ui/mod.rs` `render()` match: `Popup::Files => popups::files::render(f, app),`
- `src/events.rs` `handle_key()` match: `Popup::Files => ui::popups::files::handle_key(app, k)?,` (use the bare `ui::popups::` prefix — the file imports `crate::ui` as `ui`).
- `src/input.rs` `App::paste`: add arm

```rust
Popup::Files if self.files_mode == crate::app::FilesMode::Add => self.files_edit.push_str(text),
```

- [ ] **Step 3: Add a paste-routing test**

In `src/input.rs` tests:

```rust
#[test]
fn paste_goes_into_files_add_field() {
    let mut a = test_app();
    a.popup = crate::app::Popup::Files;
    a.files_mode = crate::app::FilesMode::Add;
    a.paste("/tmp/report.pdf");
    assert_eq!(a.files_edit, "/tmp/report.pdf");
}
```

- [ ] **Step 4: Build + test**

Run: `cargo build && cargo test`
Expected: compiles with zero warnings (the match arms are exhaustive, so a missed dispatch is a compile error); all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/ui/popups/files.rs src/ui/popups/mod.rs src/ui/mod.rs src/events.rs src/input.rs src/app/mod.rs
git commit -m "feat: files popup UI + key handling"
```

---

### Task 8: Path-paste detection in the composer

**Files:**
- Modify: `src/input.rs` (`App::paste`'s `Popup::None` arm + helper + tests)

**Interfaces:**
- Consumes: `open_files_popup`, `start_files_add`, `files_edit` (Task 6).
- Produces: pasting an absolute existing-file path into the composer opens the files popup in Add mode, prefilled — Enter imports, Esc cancels.

- [ ] **Step 1: Write failing tests**

In `src/input.rs` tests:

```rust
#[test]
fn pasting_a_file_path_offers_import() {
    let mut a = test_app();
    let src = std::env::temp_dir().join(format!("nexus-paste-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&src, "x").unwrap();

    a.paste(&src.to_string_lossy());
    assert!(a.popup == crate::app::Popup::Files);
    assert!(a.files_mode == crate::app::FilesMode::Add);
    assert_eq!(a.files_edit, src.to_string_lossy());
    assert!(a.input_text().is_empty()); // path did not land in the composer

    // file:// URIs and quoted paths (file-manager drag/drop) work too.
    let mut a = test_app();
    a.paste(&format!("file://{}", src.to_string_lossy()));
    assert!(a.popup == crate::app::Popup::Files);

    // Ordinary text is untouched.
    let mut a = test_app();
    a.paste("/not/a/real/path and some prose");
    assert!(a.popup == crate::app::Popup::None);
    assert_eq!(a.input_text(), "/not/a/real/path and some prose");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test pasting_a_file_path`
Expected: FAIL — popup stays `None`.

- [ ] **Step 3: Implement**

In `src/input.rs`, a free helper above `impl App`:

```rust
/// If pasted text is a single absolute path to an existing regular file
/// (optionally `file://`-prefixed or quoted, as file managers produce on
/// drag/drop), return the cleaned path.
fn pasted_file_path(text: &str) -> Option<std::path::PathBuf> {
    let t = text.trim().trim_matches('"').trim_matches('\'');
    let t = t.strip_prefix("file://").unwrap_or(t);
    if t.contains('\n') || !t.starts_with('/') {
        return None;
    }
    let path = std::path::PathBuf::from(t);
    path.is_file().then_some(path)
}
```

In `App::paste`, replace the `Popup::None` arm:

```rust
Popup::None => {
    // A dropped/pasted file path becomes an import offer instead of text.
    if let Some(path) = pasted_file_path(text) {
        self.open_files_popup();
        self.start_files_add();
        self.files_edit = path.to_string_lossy().to_string();
        self.status = "import this file? Enter to confirm · Esc to cancel".to_string();
        return;
    }
    self.input.insert_str(text);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/input.rs
git commit -m "feat: pasted file paths offer import into the space fileset"
```

---

## Phase 4 — image transcription

### Task 9: One-shot vision call on the provider

**Files:**
- Modify: `src/provider/openrouter.rs`

**Interfaces:**
- Produces (used by Task 11):
  - `OpenRouter::transcribe_image(&self, model: &str, image_data_url: &str) -> Result<String>` — non-streaming; sends a fixed transcription prompt with an `image_url` content part.
- Consumes: nothing new.

- [ ] **Step 1: Write failing test**

The request body shape is the testable unit (no network in tests). Extract the body builder as a free function and test it:

```rust
#[test]
fn vision_body_has_image_url_content_part() {
    let body = vision_body("google/gemini-2.5-flash-lite", "data:image/png;base64,AAAA");
    assert_eq!(body["model"], "google/gemini-2.5-flash-lite");
    assert_eq!(body["stream"], false);
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "text");
    assert!(content[0]["text"].as_str().unwrap().to_lowercase().contains("transcribe"));
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test vision_body`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

Free function near the other parsers:

```rust
/// Request body for a one-shot image-transcription call: a text part with the
/// instruction plus the image as a data-URL content part (OpenAI vision shape).
fn vision_body(model: &str, image_data_url: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text",
                  "text": "Transcribe this image faithfully. Reproduce all visible text verbatim \
                           (preserve code, tables, and structure as markdown). If parts are not \
                           text, describe them briefly in [brackets]. Output only the transcription." },
                { "type": "image_url", "image_url": { "url": image_data_url } },
            ],
        }],
    })
}
```

Method on `OpenRouter` — refactor `complete` minimally by extracting its POST+parse tail into a private helper both use:

```rust
/// POST a completions body and pull the first choice's message text.
async fn post_completion(&self, body: serde_json::Value) -> Result<String> {
    let v = self
        .client
        .post(format!("{BASE}/chat/completions"))
        .bearer_auth(&self.key)
        .json(&body)
        .send()
        .await
        .context("completion request")?
        .error_for_status()
        .context("completion failed")?
        .json::<serde_json::Value>()
        .await
        .context("parsing completion")?;
    Ok(v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string())
}

/// One-shot, non-streaming vision call: transcribe `image_data_url` with `model`.
pub async fn transcribe_image(&self, model: &str, image_data_url: &str) -> Result<String> {
    self.post_completion(vision_body(model, image_data_url)).await
}
```

Rewrite `complete` to build its body then `self.post_completion(body).await` (behavior unchanged).

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/provider/openrouter.rs
git commit -m "feat: one-shot vision transcription call on OpenRouter"
```

---

### Task 10: `transcriber_model` setting

**Files:**
- Modify: `src/app/mod.rs` (`SettingsField::TranscriberModel`, `ModelPickTarget::Transcriber`, App field `transcriber_model`, `load_settings` arm)
- Modify: `src/app/models.rs` (`open_model_picker_for_transcriber`, `pick_model` arm, `clear_transcriber_model`)
- Modify: `src/app/settings.rs` (`save_settings` persists it)
- Modify: `src/ui/popups/settings.rs` (render value + picked-not-typed key handling)

**Interfaces:**
- Consumes: existing MemoryModel picker pattern (mirror it exactly).
- Produces (used by Task 11): `App::transcriber_model: String` (empty = disabled), default `"google/gemini-2.5-flash-lite"`.

- [ ] **Step 1: Write failing test**

In `src/app/tests.rs`:

```rust
#[test]
fn transcriber_model_defaults_and_persists() {
    let mut a = test_app();
    assert_eq!(a.transcriber_model, "google/gemini-2.5-flash-lite");
    a.transcriber_model = "some/vision-model".to_string();
    a.save_settings().unwrap();
    let kv = a.db.load_settings().unwrap();
    assert!(kv.iter().any(|(k, v)| k == "transcriber_model" && v == "some/vision-model"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test transcriber_model`
Expected: FAIL to compile (no field).

- [ ] **Step 3: Implement — mirror MemoryModel everywhere**

`src/app/mod.rs`:
- App field `pub transcriber_model: String`, initialized to `"google/gemini-2.5-flash-lite".to_string()` in `new`.
- `SettingsField::TranscriberModel` variant; `ALL` becomes `[SettingsField; 13]` with it appended; `label()` arm: `"transcriber model (Enter to pick, Backspace clears)"`.
- `text_index()`: add `TranscriberModel` to the `None` arm (picked, not typed).
- `ModelPickTarget::Transcriber` variant.
- `load_settings()`: `"transcriber_model" => self.transcriber_model = v,`.

`src/app/models.rs`:
- `open_model_picker_for_transcriber` (copy of `open_model_picker_for_memory` with the target swapped, `pub(crate)`).
- `pick_model` arm:

```rust
ModelPickTarget::Transcriber => {
    self.transcriber_model = id.clone();
    self.db.set_setting("transcriber_model", &id)?;
    self.status = format!("transcriber model: {id}");
    self.popup = Popup::Settings;
}
```

- `clear_transcriber_model` (copy of `clear_memory_model`, status `"transcriber model cleared — image paste disabled"`).

`src/app/settings.rs` `save_settings`:

```rust
self.transcriber_model = self.transcriber_model.trim().to_string();
self.db.set_setting("transcriber_model", &self.transcriber_model)?;
```

`src/ui/popups/settings.rs`:
- `value()` arm: `SettingsField::TranscriberModel => numeric(&app.transcriber_model),`
- `handle_key`: generalize the MemoryModel special case to both picker rows:

```rust
let picker = matches!(app.settings_field(), SettingsField::MemoryModel | SettingsField::TranscriberModel);
if picker {
    match key.code {
        KeyCode::Esc => app.save_settings()?,
        KeyCode::Enter => match app.settings_field() {
            SettingsField::MemoryModel => app.open_model_picker_for_memory(),
            _ => app.open_model_picker_for_transcriber(),
        },
        KeyCode::Backspace => match app.settings_field() {
            SettingsField::MemoryModel => app.clear_memory_model()?,
            _ => app.clear_transcriber_model()?,
        },
        KeyCode::Up => app.move_settings_selection(-1),
        KeyCode::Down | KeyCode::Tab => app.move_settings_selection(1),
        _ => {}
    }
    return Ok(());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass, zero warnings. (If any test constructs `settings_inputs` by length, it's `[String; 6]` and unchanged — TranscriberModel is picked, not typed.)

- [ ] **Step 5: Commit**

```bash
git add src/app/mod.rs src/app/models.rs src/app/settings.rs src/ui/popups/settings.rs src/app/tests.rs
git commit -m "feat: transcriber_model setting with picker"
```

---

### Task 11: Clipboard image → transcriber → composer

**Files:**
- Create: `src/app/transcribe.rs`
- Modify: `src/app/mod.rs` (`mod transcribe;`, `transcript_rx` field, `AppEvent::Transcript`, `next_event` arm)
- Modify: `src/input.rs` (`paste_from_clipboard` checks for an image first)
- Modify: `src/events.rs` (`AppEvent::Transcript` arm)

**Interfaces:**
- Consumes: `OpenRouter::transcribe_image` (Task 9), `transcriber_model` (Task 10), existing `clipboard: Option<arboard::Clipboard>`.
- Produces:
  - `App::transcribe_clipboard_image(&mut self, img: arboard::ImageData)` — spawns the call, sets status
  - `App::on_transcript_result(&mut self, r: Option<Result<String, String>>)` — inserts into composer
  - `AppEvent::Transcript(Option<Result<String, String>>)`
  - free `fn png_data_url(width, height, rgba: &[u8]) -> anyhow::Result<String>`

- [ ] **Step 1: Write failing tests**

In `src/app/transcribe.rs` (tests at the bottom of the new file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_rgba_as_png_data_url() {
        // 2x1 image: red pixel, transparent pixel.
        let rgba = [255, 0, 0, 255, 0, 0, 0, 0];
        let url = png_data_url(2, 1, &rgba).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        // Round-trip: the payload decodes as a real 2x1 PNG.
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, 2);
        assert_eq!(reader.info().height, 1);
    }

    #[test]
    fn transcript_result_lands_in_composer() {
        let mut a = crate::app::tests::test_app();
        a.set_input("see: ");
        a.on_transcript_result(Some(Ok("hello from image".into())));
        assert_eq!(a.input_text(), "see: hello from image");
        a.on_transcript_result(Some(Err("model exploded".into())));
        assert!(a.status.contains("model exploded"));
    }
}
```

(This reuses `test_app` from `src/app/tests.rs` — make that helper `pub(super)` within the `#[cfg(test)]` module tree if it isn't already reachable; both are under `crate::app`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test transcribe`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`src/app/transcribe.rs`:

```rust
//! Clipboard-image transcription: encode the pasted image as a PNG data URL,
//! send it to the configured small vision model, and drop the transcript into
//! the composer — the main chat model never sees the image itself.

use anyhow::{Context, Result};
use base64::Engine;
use tokio::sync::mpsc;

use super::App;

/// Encode raw RGBA pixels as a `data:image/png;base64,…` URL.
pub(super) fn png_data_url(width: usize, height: usize, rgba: &[u8]) -> Result<String> {
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().context("png header")?;
        writer.write_image_data(rgba).context("png data")?;
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

impl App {
    /// Kick off transcription of a clipboard image (from Ctrl+V). Runs in the
    /// background; the transcript arrives as `AppEvent::Transcript`.
    pub fn transcribe_clipboard_image(&mut self, img: arboard::ImageData) {
        let Some(provider) = self.provider.clone() else {
            self.status = "set your API key first with /key".to_string();
            return;
        };
        let model = self.transcriber_model.trim().to_string();
        if model.is_empty() {
            self.status = "no transcriber model set — pick one in /config".to_string();
            return;
        }
        let url = match png_data_url(img.width, img.height, &img.bytes) {
            Ok(u) => u,
            Err(e) => {
                self.status = format!("could not encode image: {e}");
                return;
            }
        };
        self.status = format!("transcribing image ({}x{})…", img.width, img.height);
        let (tx, rx) = mpsc::unbounded_channel();
        self.transcript_rx = Some(rx);
        tokio::spawn(async move {
            let result = provider
                .transcribe_image(&model, &url)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// Insert a finished transcript at the composer cursor.
    pub fn on_transcript_result(&mut self, result: Option<Result<String, String>>) {
        self.transcript_rx = None;
        match result {
            Some(Ok(text)) if !text.trim().is_empty() => {
                self.input.insert_str(text.trim());
                self.status = "image transcribed".to_string();
            }
            Some(Ok(_)) => self.status = "transcriber returned nothing".to_string(),
            Some(Err(e)) => self.status = format!("transcription failed: {e}"),
            None => {}
        }
    }
}
```

`src/app/mod.rs`:
- `mod transcribe;` in the module list.
- App field + init:

```rust
/// Background image-transcription result channel.
pub(crate) transcript_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
```

(Note: this is `std::result::Result<String, String>`, not `anyhow::Result` — write it as `std::result::Result<String, String>` if `Result` resolves to the anyhow alias in scope.) Initialize `transcript_rx: None` in `new`.
- `AppEvent` variant:

```rust
/// A clipboard-image transcript (or the error), headed for the composer.
Transcript(Option<std::result::Result<String, String>>),
```

- `next_event()` gains the matching select arm (copy the `skills_rx` arm's shape).

`src/input.rs` `paste_from_clipboard`:

```rust
pub fn paste_from_clipboard(&mut self) {
    // An image on the clipboard (screenshot, copied picture) beats text —
    // but only for the composer; popup fields are text-only.
    if self.popup == crate::app::Popup::None
        && let Some(img) = self.clipboard.as_mut().and_then(|cb| cb.get_image().ok())
    {
        self.transcribe_clipboard_image(img);
        return;
    }
    let Some(cb) = self.clipboard.as_mut() else { return };
    let Ok(text) = cb.get_text() else { return };
    self.paste(&text);
}
```

`src/events.rs`: `AppEvent::Transcript(r) => app.on_transcript_result(r),`

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all pass, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add src/app/transcribe.rs src/app/mod.rs src/input.rs src/events.rs src/app/tests.rs
git commit -m "feat: clipboard image paste transcribes via vision model into composer"
```

---

### Task 12: Whole-feature verification pass

**Files:**
- Modify: none expected (fixes only if verification finds problems)

- [ ] **Step 1: Full build + test**

Run: `cargo build 2>&1 | tail -5 && cargo test 2>&1 | tail -5`
Expected: zero warnings, all tests pass.

- [ ] **Step 2: Static sanity checks**

- `grep -rn "search_files\|read_file" src/tools.rs src/app/chat.rs` — tool names in defs, run, and the system-prompt section all match.
- `grep -rn "rescan_files" src/` — called from: `init`, `set_active_space`, `open_files_popup`, `import_file`, `confirm_files_delete`. No other stale entry points.
- `grep -rn "transcriber_model" src/` — field, default, load, save, picker arm, clear, render all present.
- Confirm no `pub ` was introduced where `pub(crate)`/`pub(super)`/private suffices (spot-check the new items in db.rs, extract.rs, tools.rs).

- [ ] **Step 3: Manual smoke checklist (needs a human + API key — record as instructions in the final report, do not attempt in CI)**

1. `/files` → Ctrl+N → type a PDF path → Enter → file listed with "ok" (or "no text (scanned?)" for a scan).
2. Drop a .md file into `~/.local/share/nexus-chat/spaces/default/files/`, reopen `/files` → it appears.
3. Paste an absolute path into the composer → import offer appears.
4. Ask "what does <file> say about X?" → spinner shows "Searching files…" / "Reading …", answer cites file content; `/config` → Ctrl+G shows system prompt unchanged apart from the short file list.
5. Copy a screenshot, Ctrl+V in the composer → "transcribing image…", transcript appears at the cursor.
6. `/config` → transcriber model row: Enter opens picker, Backspace clears, cleared state blocks image paste with a helpful status.

- [ ] **Step 4: Commit (only if fixes were needed)**

```bash
git add <exact files touched>
git commit -m "fix: post-verification fixes for space filesets"
```
