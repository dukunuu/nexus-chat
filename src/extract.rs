//! Import-time text extraction for space filesets: plain text as-is, PDF via
//! pdf-extract, office formats (pptx/docx/xlsx) by scanning their zipped XML
//! for text tags — same string-scanning style as tools.rs's DDG HTML parser.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

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
    // &amp; must be last: unescaping it earlier fabricates new entities out of
    // compound escapes like &amp;lt; (the encoding of a literal "&lt;").
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
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

/// Why OCR failed: the tools aren't installed (user-fixable hint) vs a real
/// failure (surfaced as an error status).
#[derive(Debug)]
pub(crate) enum OcrError {
    MissingTools,
    Failed(String),
}

/// OCR a (scanned) PDF with pdftoppm + tesseract. Pages join with `[page N]`
/// marker lines — same inline-marker convention as pptx's `[slide N]`.
/// `Ok("")` means the tools ran but found no text. `progress` is called after
/// each finished page with (pages done, total pages).
pub(crate) fn ocr_pdf(
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
pub(crate) fn render_pdf_pages(
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
pub(crate) fn join_pages(results: &[std::result::Result<String, String>]) -> String {
    let mut text = String::new();
    for (i, r) in results.iter().enumerate() {
        match r {
            Ok(p) if p.trim().is_empty() => {}
            Ok(p) => text.push_str(&format!("[page {}]\n{}\n", i + 1, p.trim())),
            Err(_) => text.push_str(&format!("[page {}: ocr failed]\n", i + 1)),
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
        .map(|n| n.get())
        .unwrap_or(4)
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
            text.push_str(&format!("[page {}]\n{page}\n", i + 1));
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
/// Test fixture shared with app::files tests.
#[cfg(test)]
pub(crate) fn minimal_pdf(text: Option<&str>) -> Vec<u8> {
    match text {
        Some(t) => pdf_with_pages(&[t]),
        None => {
            let objs = vec![
                "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 150] >>".to_string(),
            ];
            serialize_pdf(objs)
        }
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
    serialize_pdf(objs)
}

#[cfg(test)]
fn serialize_pdf(objs: Vec<String>) -> Vec<u8> {
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
        let p = office_fixture(
            "d.docx",
            &[(
                "word/document.xml",
                r#"<w:document><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p><w:p><w:r><w:t>Second &amp; last</w:t></w:r></w:p></w:document>"#,
            )],
        );
        let text = extract_text(&p).unwrap();
        assert_eq!(text, "Hello world\nSecond & last");
    }

    #[test]
    fn pptx_pulls_slide_text_with_slide_markers() {
        let p = office_fixture(
            "s.pptx",
            &[
                (
                    "ppt/slides/slide1.xml",
                    r#"<p:sld><a:t>Title one</a:t><a:t>Bullet</a:t></p:sld>"#,
                ),
                (
                    "ppt/slides/slide2.xml",
                    r#"<p:sld><a:t>Second slide</a:t></p:sld>"#,
                ),
                (
                    "ppt/slides/slide10.xml",
                    r#"<p:sld><a:t>Tenth slide</a:t></p:sld>"#,
                ),
            ],
        );
        let text = extract_text(&p).unwrap();
        assert!(text.contains("[slide 1]"));
        assert!(text.contains("Title one"));
        assert!(text.contains("[slide 2]"));
        assert!(text.contains("Second slide"));
        // Verify numeric ordering (slide 2 before slide 10, not lexicographic)
        let i2 = text.find("[slide 2]").unwrap();
        let i10 = text.find("[slide 10]").unwrap();
        assert!(i2 < i10);
    }

    #[test]
    fn xlsx_pulls_shared_strings_and_cell_values() {
        let p = office_fixture(
            "x.xlsx",
            &[
                (
                    "xl/sharedStrings.xml",
                    r#"<sst><si><t>revenue</t></si><si><t>cost</t></si></sst>"#,
                ),
                (
                    "xl/worksheets/sheet1.xml",
                    r#"<worksheet><c><v>42</v></c><c><v>7</v></c></worksheet>"#,
                ),
            ],
        );
        let text = extract_text(&p).unwrap();
        assert!(text.contains("revenue"));
        assert!(text.contains("cost"));
        assert!(text.contains("42"));
    }

    #[test]
    fn xml_tag_texts_handles_attrs_self_closing_and_entities() {
        let xml = r#"<w:t a="b">one</w:t><w:t/><w:t>two &lt;3</w:t>"#;
        assert_eq!(
            xml_tag_texts(xml, "w:t"),
            vec!["one".to_string(), "two <3".to_string()]
        );
    }

    #[test]
    fn xml_unescape_does_not_double_unescape_compound_entities() {
        assert_eq!(xml_unescape("&amp;amp;lt;"), "&amp;lt;");
        assert_eq!(xml_unescape("a &amp; b &lt;3"), "a & b <3");
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
