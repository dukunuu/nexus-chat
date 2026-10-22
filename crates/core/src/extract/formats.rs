//! Built-in converters: office and ODF documents, EPUB, RTF, mail, notebooks,
//! subtitles, delimited tables and SVG. No external tools, no new crates —
//! zipped XML is scanned with the shared tokenizer, the rest is plain text.

use std::fmt::Write as _;
use std::io::{Cursor, Read};

use anyhow::{Context, Result, bail};

use super::html;
use super::markup::{Tok, Tokens, attr, decode_entities};

/// Largest single zip entry read into memory (zip-bomb guard).
const MAX_ENTRY: u64 = 64 * 1024 * 1024;
/// Largest converted text a built-in converter returns.
pub const MAX_TEXT: usize = 48 * 1024 * 1024;

type Zip<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

fn open_zip(bytes: &[u8]) -> Result<Zip<'_>> {
    zip::ZipArchive::new(Cursor::new(bytes)).context("reading zip container")
}

fn read_entry(zip: &mut Zip<'_>, name: &str) -> Option<String> {
    let entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.take(MAX_ENTRY).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn entry_names(zip: &mut Zip<'_>) -> Vec<String> {
    (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .collect()
}

// --- OOXML ---------------------------------------------------------------

pub enum OfficeKind {
    Docx,
    Pptx,
    Xlsx,
}

/// Pull text out of an OOXML zip by scanning member XML for text tags.
/// ponytail: tag scanning, not an XML parser — swap in a real parser only if
/// a document breaks it.
// ZIP entry names are case-sensitive by spec, so these prefix/extension
// comparisons must stay exact (the lint targets display-extension matches).
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn office_text(bytes: &[u8], kind: &OfficeKind) -> Result<String> {
    let mut zip = open_zip(bytes)?;
    let mut out = String::new();
    let names = entry_names(&mut zip);
    match kind {
        OfficeKind::Docx => {
            if let Some(xml) = read_entry(&mut zip, "word/document.xml") {
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
                if let Some(xml) = read_entry(&mut zip, name) {
                    let texts = xml_tag_texts(&xml, "a:t");
                    if !texts.is_empty() {
                        let _ = writeln!(out, "[slide {n}]");
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
            if let Some(xml) = read_entry(&mut zip, "xl/sharedStrings.xml") {
                out.push_str(&xml_tag_texts(&xml, "t").join("\n"));
                out.push('\n');
            }
            let mut sheets: Vec<&String> = names
                .iter()
                .filter(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
                .collect();
            sheets.sort();
            for name in sheets {
                if let Some(xml) = read_entry(&mut zip, name) {
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
pub fn xml_tag_texts(xml: &str, tag: &str) -> Vec<String> {
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

pub fn xml_unescape(s: &str) -> String {
    // &amp; must be last: unescaping it earlier fabricates new entities out of
    // compound escapes like &amp;lt; (the encoding of a literal "&lt;").
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

// --- ODF -----------------------------------------------------------------

/// Text of an `OpenDocument` file (odt / ods / odp) from its `content.xml`:
/// headings and paragraphs, `[slide N]` markers, spreadsheet rows as
/// ` | `-joined lines under a `[sheet]` marker.
#[allow(clippy::too_many_lines)]
pub fn odf_text(bytes: &[u8]) -> Result<String> {
    let mut zip = open_zip(bytes)?;
    let xml = read_entry(&mut zip, "content.xml").context("no content.xml in the document")?;
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut row: Option<Vec<String>> = None;
    let mut cell: Option<String> = None;
    let mut slide = 0;
    let mut bullet = false;
    let flush = |line: &mut String, lines: &mut Vec<String>| {
        let t = line.trim();
        if !t.is_empty() {
            lines.push(t.to_string());
        }
        line.clear();
    };
    for tok in Tokens::new(&xml) {
        match tok {
            Tok::Text(t) => {
                let t = decode_entities(t);
                let target = cell.as_mut().unwrap_or(&mut line);
                let t = t.trim_matches(|c: char| c == '\n' || c == '\r');
                if !t.trim().is_empty() || target.ends_with(|c: char| !c.is_whitespace()) {
                    target.push_str(&collapse_spaces(t));
                }
            }
            Tok::Start { name, attrs, .. } => match name.as_str() {
                "text:h" => {
                    flush(&mut line, &mut lines);
                    if cell.is_none() {
                        let level: usize = attr(attrs, "text:outline-level")
                            .and_then(|l| l.parse().ok())
                            .unwrap_or(1);
                        line.push_str(&"#".repeat(level.clamp(1, 6)));
                        line.push(' ');
                    }
                }
                "text:p" => {
                    flush(&mut line, &mut lines);
                    if bullet && cell.is_none() {
                        line.push_str("- ");
                    }
                    bullet = false;
                }
                "text:list-item" => {
                    flush(&mut line, &mut lines);
                    bullet = true;
                }
                "text:s" => {
                    let n: usize = attr(attrs, "text:c")
                        .and_then(|c| c.parse().ok())
                        .unwrap_or(1);
                    cell.as_mut()
                        .unwrap_or(&mut line)
                        .push_str(&" ".repeat(n.min(64)));
                }
                "text:tab" => cell.as_mut().unwrap_or(&mut line).push('\t'),
                "text:line-break" => {
                    if cell.is_none() {
                        flush(&mut line, &mut lines);
                    }
                }
                "draw:page" => {
                    flush(&mut line, &mut lines);
                    slide += 1;
                    lines.push(format!("[slide {slide}]"));
                }
                "table:table" => {
                    flush(&mut line, &mut lines);
                    if let Some(n) = attr(attrs, "table:name") {
                        lines.push(format!("[sheet {n}]"));
                    }
                }
                "table:table-row" => row = Some(Vec::new()),
                "table:table-cell" | "table:covered-table-cell" => cell = Some(String::new()),
                _ => {}
            },
            Tok::End { name } => match name.as_str() {
                "text:p" | "text:h" => {
                    if let Some(c) = cell.as_mut() {
                        if !c.is_empty() && !c.ends_with(' ') {
                            c.push(' ');
                        }
                    } else {
                        flush(&mut line, &mut lines);
                    }
                }
                "table:table-cell" | "table:covered-table-cell" => {
                    if let (Some(c), Some(r)) = (cell.take(), row.as_mut()) {
                        r.push(c.trim().to_string());
                    }
                }
                "table:table-row" => {
                    if let Some(mut r) = row.take() {
                        while r.last().is_some_and(String::is_empty) {
                            r.pop();
                        }
                        if !r.is_empty() {
                            lines.push(r.join(" | "));
                        }
                    }
                }
                _ => {}
            },
        }
    }
    flush(&mut line, &mut lines);
    Ok(lines.join("\n"))
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

// --- EPUB ----------------------------------------------------------------

/// An EPUB's text: the OPF spine in reading order, each XHTML document
/// converted like any other HTML page.
pub fn epub_text(bytes: &[u8]) -> Result<String> {
    let mut zip = open_zip(bytes)?;
    let names = entry_names(&mut zip);
    let mut order: Vec<String> = Vec::new();
    let mut title = String::new();
    let opf_path = read_entry(&mut zip, "META-INF/container.xml").and_then(|xml| {
        Tokens::new(&xml).find_map(|t| match t {
            Tok::Start { name, attrs, .. } if name == "rootfile" => attr(attrs, "full-path"),
            _ => None,
        })
    });
    if let Some(opf_path) = opf_path
        && let Some(opf) = read_entry(&mut zip, &opf_path)
    {
        let base = opf_path.rsplit_once('/').map_or("", |(d, _)| d);
        let mut manifest = std::collections::HashMap::new();
        let mut spine = Vec::new();
        let mut in_title = false;
        for tok in Tokens::new(&opf) {
            match tok {
                Tok::Start { name, attrs, .. } => match name.as_str() {
                    "item" => {
                        if let (Some(id), Some(href)) = (attr(attrs, "id"), attr(attrs, "href")) {
                            manifest.insert(id, href);
                        }
                    }
                    "itemref" => {
                        if let Some(id) = attr(attrs, "idref") {
                            spine.push(id);
                        }
                    }
                    "dc:title" => in_title = title.is_empty(),
                    _ => {}
                },
                Tok::Text(t) if in_title => title.push_str(&decode_entities(t)),
                Tok::End { name } if name == "dc:title" => in_title = false,
                _ => {}
            }
        }
        for id in spine {
            if let Some(href) = manifest.get(&id) {
                let href = href.split('#').next().unwrap_or(href);
                order.push(resolve_path(base, href));
            }
        }
    }
    if order.is_empty() {
        // No readable package: every markup document, in name order.
        order = names
            .iter()
            .filter(|n| {
                std::path::Path::new(n)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| {
                        ["xhtml", "html", "htm"]
                            .iter()
                            .any(|m| e.eq_ignore_ascii_case(m))
                    })
            })
            .cloned()
            .collect();
        order.sort();
    }
    let mut out = String::new();
    if !title.trim().is_empty() {
        let _ = writeln!(out, "# {}", title.trim());
    }
    for path in order {
        let Some(doc) = read_entry(&mut zip, &path) else {
            continue;
        };
        let md = html::to_markdown(&doc);
        if !md.is_empty() {
            out.push_str(&md);
            out.push('\n');
        }
        if out.len() > MAX_TEXT {
            break;
        }
    }
    Ok(out.trim().to_string())
}

/// `href` relative to the directory `base`, with `.` / `..` folded.
fn resolve_path(base: &str, href: &str) -> String {
    let href = href.replace("%20", " ");
    let mut parts: Vec<&str> = if href.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|p| !p.is_empty()).collect()
    };
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

// --- RTF -----------------------------------------------------------------

/// Destinations whose content is not document text.
const RTF_SKIP: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "footnote",
    "fldinst",
    "themedata",
    "colorschememapping",
    "datastore",
    "latentstyles",
    "listtable",
    "listoverridetable",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "private",
    "revtbl",
    "pgptbl",
    "operator",
    "title",
    "author",
    "company",
];

fn cp1252(b: u8) -> char {
    match b {
        0x80 => '\u{20ac}',
        0x82 => '\u{201a}',
        0x83 => '\u{192}',
        0x84 => '\u{201e}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{2c6}',
        0x89 => '\u{2030}',
        0x8A => '\u{160}',
        0x8B => '\u{2039}',
        0x8C => '\u{152}',
        0x8E => '\u{17d}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201c}',
        0x94 => '\u{201d}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{2dc}',
        0x99 => '\u{2122}',
        0x9A => '\u{161}',
        0x9B => '\u{203a}',
        0x9C => '\u{153}',
        0x9E => '\u{17e}',
        0x9F => '\u{178}',
        b => char::from(b),
    }
}

#[derive(Clone, Copy)]
struct RtfGroup {
    skip: bool,
    uc: usize,
}

/// Plain text of an RTF document: control words dropped, `\par` / `\tab`
/// honoured, `\'hh` and `\uN` escapes decoded, non-text destinations
/// (fonts, styles, pictures, headers) skipped.
#[allow(clippy::too_many_lines)]
pub fn rtf_text(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::new();
    let mut stack: Vec<RtfGroup> = Vec::new();
    let mut cur = RtfGroup { skip: false, uc: 1 };
    let mut skip_chars = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => {
                stack.push(cur);
                i += 1;
                // A new group starts a destination only if a control word follows.
                if b.get(i) == Some(&b'\\') && b.get(i + 1) == Some(&b'*') {
                    cur.skip = true;
                }
            }
            b'}' => {
                cur = stack.pop().unwrap_or(cur);
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&c) = b.get(i) else { break };
                match c {
                    b'\\' | b'{' | b'}' => {
                        if skip_chars > 0 {
                            skip_chars -= 1;
                        } else if !cur.skip {
                            out.push(char::from(c));
                        }
                        i += 1;
                    }
                    b'\'' => {
                        let hex = src
                            .get(i + 1..i + 3)
                            .and_then(|h| u8::from_str_radix(h, 16).ok());
                        i += 3;
                        if skip_chars > 0 {
                            skip_chars -= 1;
                        } else if let Some(byte) = hex
                            && !cur.skip
                        {
                            out.push(cp1252(byte));
                        }
                    }
                    b'~' => {
                        i += 1;
                        if !cur.skip {
                            out.push(' ');
                        }
                    }
                    b'_' => {
                        i += 1;
                        if !cur.skip {
                            out.push('-');
                        }
                    }
                    b'*' => {
                        cur.skip = true;
                        i += 1;
                    }
                    c if c.is_ascii_alphabetic() => {
                        let start = i;
                        while i < b.len() && b[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word = &src[start..i];
                        let num_start = i;
                        if i < b.len() && b[i] == b'-' {
                            i += 1;
                        }
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                        let param: Option<i64> = src[num_start..i].parse().ok();
                        if b.get(i) == Some(&b' ') {
                            i += 1; // the delimiter space belongs to the word
                        }
                        if RTF_SKIP.contains(&word) {
                            cur.skip = true;
                            continue;
                        }
                        if cur.skip {
                            continue;
                        }
                        match word {
                            "par" | "line" | "sect" | "page" | "row" => out.push('\n'),
                            "tab" | "cell" => out.push('\t'),
                            "emdash" => out.push('\u{2014}'),
                            "endash" => out.push('\u{2013}'),
                            "lquote" => out.push('\u{2018}'),
                            "rquote" => out.push('\u{2019}'),
                            "ldblquote" => out.push('\u{201c}'),
                            "rdblquote" => out.push('\u{201d}'),
                            "bullet" => out.push('\u{2022}'),
                            "emspace" | "enspace" => out.push(' '),
                            "uc" => cur.uc = usize::try_from(param.unwrap_or(1)).unwrap_or(1),
                            "u" => {
                                let code = param.unwrap_or(0);
                                let code =
                                    u32::try_from(if code < 0 { code + 65536 } else { code })
                                        .unwrap_or(0xFFFD);
                                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                                skip_chars = cur.uc;
                            }
                            "bin" => {
                                i += usize::try_from(param.unwrap_or(0)).unwrap_or(0);
                            }
                            _ => {}
                        }
                    }
                    _ => i += 1,
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                // One whole character (the source is valid UTF-8).
                let ch = src[i..].chars().next().unwrap_or('\u{fffd}');
                i += ch.len_utf8();
                if skip_chars > 0 {
                    skip_chars -= 1;
                } else if !cur.skip {
                    out.push(ch);
                }
            }
        }
    }
    tidy_lines(&out)
}

/// Trim every line and collapse runs of blank lines to one.
fn tidy_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            blank += 1;
            if blank == 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(t);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

// --- mail ----------------------------------------------------------------

struct Mail<'a> {
    headers: Vec<(String, String)>,
    body: &'a [u8],
}

fn split_mail(raw: &[u8]) -> Mail<'_> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4))
        .into_iter()
        .chain(raw.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2)))
        .min_by_key(|(p, _)| *p);
    let (head, body) = match sep {
        Some((p, n)) => (&raw[..p], &raw[p + n..]),
        None => (raw, &raw[raw.len()..]),
    };
    let head = String::from_utf8_lossy(head);
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = headers.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    Mail { headers, body }
}

fn header<'a>(m: &'a Mail<'_>, key: &str) -> Option<&'a str> {
    m.headers
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A parameter (`boundary`, `charset`, `filename`) of a structured header.
fn header_param(value: &str, name: &str) -> Option<String> {
    let lower = value.to_lowercase();
    let at = lower.find(&format!("{name}="))?;
    let rest = &value[at + name.len() + 1..];
    let rest = rest.trim_start();
    let v = if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next().unwrap_or("")
    } else {
        rest.split([';', ' ', '\t']).next().unwrap_or("")
    };
    Some(v.to_string())
}

fn decode_transfer(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding.trim().to_lowercase().as_str() {
        "base64" => {
            use base64::Engine as _;
            let clean: Vec<u8> = body
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(&clean)
                .unwrap_or_else(|_| body.to_vec())
        }
        "quoted-printable" => decode_qp(body),
        _ => body.to_vec(),
    }
}

fn decode_qp(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'=' {
            if body.get(i + 1) == Some(&b'\r') && body.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if body.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if let Some(hex) = body
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(body[i]);
        i += 1;
    }
    out
}

fn decode_charset(bytes: &[u8], charset: &str) -> String {
    match charset.trim().to_lowercase().as_str() {
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" | "iso-8859-15" => {
            bytes.iter().map(|b| cp1252(*b)).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Decode RFC 2047 encoded words (`=?utf-8?B?…?=`) in a header value.
fn decode_words(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("=?") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let parts: Vec<&str> = tail.splitn(3, '?').collect();
        let decoded = (parts.len() == 3)
            .then(|| {
                let end = parts[2].find("?=")?;
                let (charset, enc, text) = (parts[0], parts[1], &parts[2][..end]);
                let bytes = if enc.eq_ignore_ascii_case("b") {
                    decode_transfer(text.as_bytes(), "base64")
                } else {
                    decode_qp(text.replace('_', " ").as_bytes())
                };
                Some((
                    decode_charset(&bytes, charset),
                    start + 2 + parts[0].len() + 1 + parts[1].len() + 1 + end + 2,
                ))
            })
            .flatten();
        if let Some((text, used)) = decoded {
            out.push_str(&text);
            rest = &rest[used..];
            // Whitespace between adjacent encoded words is dropped.
            if rest.trim_start().starts_with("=?") {
                rest = rest.trim_start();
            }
        } else {
            out.push_str("=?");
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

#[derive(Default)]
struct MailText {
    plain: String,
    html: String,
    attachments: Vec<String>,
}

fn mail_parts(m: &Mail<'_>, depth: usize, acc: &mut MailText) {
    let ctype = header(m, "content-type").unwrap_or("text/plain");
    let ctype_lower = ctype.to_lowercase();
    let disposition = header(m, "content-disposition").unwrap_or("");
    let filename = header_param(disposition, "filename").or_else(|| header_param(ctype, "name"));
    if depth < 6
        && ctype_lower.starts_with("multipart/")
        && let Some(boundary) = header_param(ctype, "boundary")
    {
        let body = String::from_utf8_lossy(m.body);
        let marker = format!("--{boundary}");
        for part in body.split(&marker).skip(1).take(60) {
            if part.starts_with("--") {
                break;
            }
            let part = part.trim_start_matches(['\r', '\n']);
            mail_parts(&split_mail(part.as_bytes()), depth + 1, acc);
        }
        return;
    }
    if disposition.to_lowercase().starts_with("attachment")
        || (filename.is_some() && !ctype_lower.starts_with("text/"))
    {
        acc.attachments
            .push(filename.unwrap_or_else(|| ctype_lower.clone()));
        return;
    }
    if ctype_lower.starts_with("message/rfc822") {
        let inner = decode_transfer(m.body, header(m, "content-transfer-encoding").unwrap_or(""));
        mail_parts(&split_mail(&inner), depth + 1, acc);
        return;
    }
    if !ctype_lower.starts_with("text/") {
        acc.attachments
            .push(filename.unwrap_or_else(|| ctype_lower.clone()));
        return;
    }
    let raw = decode_transfer(m.body, header(m, "content-transfer-encoding").unwrap_or(""));
    let text = decode_charset(&raw, &header_param(ctype, "charset").unwrap_or_default());
    if ctype_lower.starts_with("text/html") {
        acc.html.push_str(&text);
    } else {
        acc.plain.push_str(&text);
    }
}

/// One RFC 822 message: the interesting headers, then the text body (plain
/// part preferred, HTML converted otherwise) and the names of attachments.
pub fn eml_text(raw: &[u8]) -> String {
    let m = split_mail(raw);
    let mut out = String::new();
    for key in ["from", "to", "cc", "date", "subject"] {
        if let Some(v) = header(&m, key) {
            let mut label = key.to_string();
            label[..1].make_ascii_uppercase();
            let _ = writeln!(out, "{label}: {}", decode_words(v));
        }
    }
    let mut parts = MailText::default();
    mail_parts(&m, 0, &mut parts);
    let body = if parts.plain.trim().is_empty() {
        html::to_markdown(&parts.html)
    } else {
        parts.plain.replace("\r\n", "\n").trim().to_string()
    };
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
        out.push('\n');
    }
    for name in &parts.attachments {
        let _ = writeln!(out, "[attachment: {}]", decode_words(name));
    }
    out.trim().to_string()
}

/// A mailbox: each message rendered like [`eml_text`].
pub fn mbox_text(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut messages: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev_blank = true;
    for line in text.split_inclusive('\n') {
        if prev_blank && line.starts_with("From ") && line.contains('@') {
            if !current.is_empty() {
                messages.push(std::mem::take(&mut current));
            }
            prev_blank = false;
            continue;
        }
        prev_blank = line.trim().is_empty();
        current.push_str(line);
    }
    if !current.is_empty() {
        messages.push(current);
    }
    let total = messages.len();
    let mut out = String::new();
    for (i, msg) in messages.iter().take(500).enumerate() {
        let _ = writeln!(out, "[message {}/{total}]", i + 1);
        out.push_str(&eml_text(msg.as_bytes()));
        out.push_str("\n\n");
        if out.len() > MAX_TEXT {
            break;
        }
    }
    out.trim().to_string()
}

// --- notebooks -----------------------------------------------------------

fn source_text(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<String>(),
        _ => String::new(),
    }
}

/// A Jupyter notebook as Markdown: markdown cells verbatim, code cells in
/// fences with their text / error outputs (images are noted, not inlined).
pub fn ipynb_text(json: &str) -> Result<String> {
    let nb: serde_json::Value = serde_json::from_str(json).context("parsing notebook JSON")?;
    let cells = nb
        .get("cells")
        .and_then(serde_json::Value::as_array)
        .or_else(|| {
            nb.pointer("/worksheets/0/cells")
                .and_then(serde_json::Value::as_array)
        })
        .context("not a notebook (no cells)")?;
    let lang = nb
        .pointer("/metadata/language_info/name")
        .or_else(|| nb.pointer("/metadata/kernelspec/language"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("python");
    let mut out = String::new();
    for cell in cells {
        let src = source_text(cell.get("source").or_else(|| cell.get("input")));
        match cell.get("cell_type").and_then(serde_json::Value::as_str) {
            Some("code") => {
                let _ = writeln!(out, "```{lang}\n{}\n```", src.trim_end());
                if let Some(outputs) = cell.get("outputs").and_then(serde_json::Value::as_array) {
                    for o in outputs {
                        let text = match o.get("output_type").and_then(serde_json::Value::as_str) {
                            Some("stream") => source_text(o.get("text")),
                            Some("error") => format!(
                                "{}: {}",
                                o.get("ename")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("error"),
                                o.get("evalue")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("")
                            ),
                            Some("execute_result" | "display_data") => {
                                let data = o.get("data");
                                let plain = source_text(data.and_then(|d| d.get("text/plain")));
                                let has_image = data
                                    .and_then(serde_json::Value::as_object)
                                    .is_some_and(|d| d.keys().any(|k| k.starts_with("image/")));
                                match (plain.is_empty(), has_image) {
                                    (true, true) => "[image output omitted]".to_string(),
                                    _ => plain,
                                }
                            }
                            _ => String::new(),
                        };
                        let lines: Vec<&str> = text.lines().collect();
                        if !lines.is_empty() {
                            let _ = writeln!(out, "output:");
                            for l in lines.iter().take(40) {
                                let _ = writeln!(out, "    {l}");
                            }
                            if lines.len() > 40 {
                                let _ = writeln!(out, "    … {} more lines", lines.len() - 40);
                            }
                        }
                    }
                }
            }
            _ => {
                if !src.trim().is_empty() {
                    let _ = writeln!(out, "{}", src.trim_end());
                }
            }
        }
        out.push('\n');
    }
    Ok(out.trim().to_string())
}

// --- subtitles -----------------------------------------------------------

/// SRT / `WebVTT` as `[hh:mm:ss] cue text` lines: indices, styling tags and
/// header blocks dropped, multi-line cues joined.
pub fn subtitles_text(text: &str, vtt: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut ts = String::new();
    let mut cue = String::new();
    let mut in_note = false;
    let mut lines = text.trim_start_matches('\u{feff}').lines().peekable();
    let flush = |ts: &mut String, cue: &mut String, out: &mut Vec<String>| {
        let t = cue.trim();
        if !t.is_empty() && out.last().is_none_or(|l| !l.ends_with(&format!("] {t}"))) {
            out.push(format!("[{ts}] {t}"));
        }
        cue.clear();
    };
    if vtt {
        // Skip the header block.
        for l in lines.by_ref() {
            if l.trim().is_empty() {
                break;
            }
        }
    }
    while let Some(line) = lines.next() {
        let l = line.trim();
        if l.is_empty() {
            flush(&mut ts, &mut cue, &mut out);
            in_note = false;
            continue;
        }
        if in_note || (vtt && (l.starts_with("NOTE") || l.starts_with("STYLE"))) {
            in_note = true;
            continue;
        }
        if let Some((start, _)) = l.split_once("-->") {
            flush(&mut ts, &mut cue, &mut out);
            ts = clock(start.trim());
            continue;
        }
        // A cue index (SRT) or cue identifier (VTT) precedes its timing line.
        if lines.peek().is_some_and(|n| n.contains("-->")) {
            continue;
        }
        let stripped = strip_cue_markup(l);
        if !stripped.is_empty() {
            if !cue.is_empty() {
                cue.push(' ');
            }
            cue.push_str(&stripped);
        }
    }
    flush(&mut ts, &mut cue, &mut out);
    out.join("\n")
}

/// `00:01:02,500` / `01:02.500` as `hh:mm:ss`.
fn clock(start: &str) -> String {
    let t = start.split([',', '.']).next().unwrap_or(start);
    if t.matches(':').count() == 1 {
        format!("00:{t}")
    } else {
        t.to_string()
    }
}

fn strip_cue_markup(line: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '<' | '{' => depth += 1,
            '>' | '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim()).into_owned()
}

// --- delimited tables ------------------------------------------------------

/// Quote-aware records of a delimited file.
fn records(text: &str, delim: char) -> impl Iterator<Item = Vec<String>> + '_ {
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    std::iter::from_fn(move || {
        chars.peek()?;
        let mut row = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        loop {
            let Some(c) = chars.next() else {
                row.push(std::mem::take(&mut field));
                return Some(row);
            };
            if quoted {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        field.push('"');
                        chars.next();
                    } else {
                        quoted = false;
                    }
                } else {
                    field.push(c);
                }
            } else if c == '"' && field.is_empty() {
                quoted = true;
            } else if c == delim {
                row.push(std::mem::take(&mut field));
            } else if c == '\n' || c == '\r' {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                return Some(row);
            } else {
                field.push(c);
            }
        }
    })
}

/// A CSV / TSV file: a Markdown preview of the first rows, then the full
/// text, so both a glance and exact values are searchable.
pub fn delimited_text(text: &str, delim: char) -> String {
    const PREVIEW_ROWS: usize = 11; // header + 10
    const MAX_COLS: usize = 20;
    let mut preview: Vec<Vec<String>> = Vec::new();
    let mut total = 0usize;
    let mut cols = 0usize;
    for rec in records(text, delim).filter(|r| !(r.len() == 1 && r[0].trim().is_empty())) {
        total += 1;
        cols = cols.max(rec.len());
        if preview.len() < PREVIEW_ROWS {
            preview.push(rec);
        }
    }
    let raw = text.trim_start_matches('\u{feff}').trim();
    if total == 0 {
        return raw.to_string();
    }
    let shown_cols = cols.min(MAX_COLS);
    let render = |r: &Vec<String>| {
        let mut cells: Vec<String> = r
            .iter()
            .take(shown_cols)
            .map(|c| {
                let flat = c.split_whitespace().collect::<Vec<_>>().join(" ");
                let cut: String = flat.chars().take(80).collect();
                cut.replace('|', "\\|")
            })
            .collect();
        cells.resize(shown_cols, String::new());
        format!("| {} |", cells.join(" | "))
    };
    let mut out = format!(
        "table preview: {total} rows, {cols} columns (first {} rows{})\n",
        preview.len(),
        if cols > MAX_COLS {
            ", first 20 columns"
        } else {
            ""
        }
    );
    out.push_str(&render(&preview[0]));
    out.push('\n');
    let _ = writeln!(out, "| {} |", vec!["---"; shown_cols].join(" | "));
    for r in &preview[1..] {
        out.push_str(&render(r));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(raw);
    out
}

// --- SVG -----------------------------------------------------------------

/// The text nodes of an SVG: `<text>`, `<title>` and `<desc>` content.
pub fn svg_text(svg: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for tok in Tokens::new(svg) {
        match tok {
            Tok::Start {
                name, self_closing, ..
            } if matches!(name.as_str(), "text" | "title" | "desc") && !self_closing => {
                depth += 1;
            }
            Tok::End { name } if matches!(name.as_str(), "text" | "title" | "desc") => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let t = cur.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !t.is_empty() {
                        lines.push(t);
                    }
                    cur.clear();
                }
            }
            Tok::Text(t) if depth > 0 => {
                cur.push_str(&decode_entities(t));
                cur.push(' ');
            }
            _ => {}
        }
    }
    lines.join("\n")
}

/// Reject a converter result that is empty so callers can fall through.
pub fn non_empty(text: String, what: &str) -> Result<String> {
    if text.trim().is_empty() {
        bail!("{what} has no text");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
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
    fn docx_pulls_paragraph_text() {
        let z = zip_bytes(&[(
            "word/document.xml",
            r#"<w:document><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p><w:p><w:r><w:t>Second &amp; last</w:t></w:r></w:p></w:document>"#,
        )]);
        assert_eq!(
            office_text(&z, &OfficeKind::Docx).unwrap(),
            "Hello world\nSecond & last"
        );
    }

    #[test]
    fn pptx_pulls_slide_text_in_numeric_slide_order() {
        let z = zip_bytes(&[
            (
                "ppt/slides/slide1.xml",
                r"<p:sld><a:t>Title one</a:t><a:t>Bullet</a:t></p:sld>",
            ),
            (
                "ppt/slides/slide2.xml",
                r"<p:sld><a:t>Second slide</a:t></p:sld>",
            ),
            (
                "ppt/slides/slide10.xml",
                r"<p:sld><a:t>Tenth slide</a:t></p:sld>",
            ),
        ]);
        let text = office_text(&z, &OfficeKind::Pptx).unwrap();
        assert!(text.contains("[slide 1]") && text.contains("Title one"));
        assert!(text.find("[slide 2]").unwrap() < text.find("[slide 10]").unwrap());
    }

    #[test]
    fn xlsx_pulls_shared_strings_and_cell_values() {
        let z = zip_bytes(&[
            (
                "xl/sharedStrings.xml",
                r"<sst><si><t>revenue</t></si><si><t>cost</t></si></sst>",
            ),
            (
                "xl/worksheets/sheet1.xml",
                r"<worksheet><c><v>42</v></c><c><v>7</v></c></worksheet>",
            ),
        ]);
        let text = office_text(&z, &OfficeKind::Xlsx).unwrap();
        assert!(text.contains("revenue") && text.contains("cost") && text.contains("42"));
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
    fn odt_keeps_headings_paragraphs_lists_and_spacing() {
        let z = zip_bytes(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            (
                "content.xml",
                r#"<office:document-content><office:body><office:text>
                <text:h text:outline-level="2">Intro</text:h>
                <text:p>Hello<text:s text:c="2"/><text:span>world</text:span> &amp; more</text:p>
                <text:list><text:list-item><text:p>one</text:p></text:list-item></text:list>
                </office:text></office:body></office:document-content>"#,
            ),
        ]);
        let text = odf_text(&z).unwrap();
        assert_eq!(text, "## Intro\nHello  world & more\n- one");
    }

    #[test]
    fn ods_rows_and_odp_slides() {
        let ods = zip_bytes(&[(
            "content.xml",
            r#"<office:spreadsheet><table:table table:name="Q1">
            <table:table-row><table:table-cell><text:p>item</text:p></table:table-cell><table:table-cell><text:p>cost</text:p></table:table-cell></table:table-row>
            <table:table-row><table:table-cell><text:p>pen</text:p></table:table-cell><table:table-cell><text:p>3</text:p></table:table-cell></table:table-row>
            </table:table></office:spreadsheet>"#,
        )]);
        assert_eq!(odf_text(&ods).unwrap(), "[sheet Q1]\nitem | cost\npen | 3");
        let odp = zip_bytes(&[(
            "content.xml",
            r#"<office:presentation><draw:page><text:p>First</text:p></draw:page><draw:page><text:p>Second</text:p></draw:page></office:presentation>"#,
        )]);
        assert_eq!(
            odf_text(&odp).unwrap(),
            "[slide 1]\nFirst\n[slide 2]\nSecond"
        );
    }

    #[test]
    fn epub_follows_the_spine_and_converts_xhtml() {
        let z = zip_bytes(&[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
            ),
            (
                "OEBPS/content.opf",
                r#"<package><metadata><dc:title>My Book</dc:title></metadata>
                <manifest><item id="a" href="ch1.xhtml"/><item id="b" href="text/ch2.xhtml"/></manifest>
                <spine><itemref idref="b"/><itemref idref="a"/></spine></package>"#,
            ),
            (
                "OEBPS/ch1.xhtml",
                "<html><body><h1>One</h1><p>first</p></body></html>",
            ),
            (
                "OEBPS/text/ch2.xhtml",
                "<html><body><h1>Two</h1><p>second</p></body></html>",
            ),
        ]);
        assert_eq!(
            epub_text(&z).unwrap(),
            "# My Book\n# Two\nsecond\n# One\nfirst"
        );
    }

    #[test]
    fn rtf_strips_control_words_and_decodes_escapes() {
        let rtf = r"{\rtf1\ansi\deff0{\fonttbl{\f0 Times;}}{\colortbl;\red0\green0\blue0;}{\*\generator Word;}
\pard Hello \b bold\b0  world\par
Caf\'e9 \u8364? and \u233?\tab done\par
{\info{\title Secret}}Second line\par}";
        let text = rtf_text(rtf);
        assert_eq!(
            text,
            "Hello bold world\nCaf\u{e9} \u{20ac} and \u{e9}\tdone\nSecond line"
        );
        assert!(!text.contains("Times") && !text.contains("Secret") && !text.contains("generator"));
    }

    #[test]
    fn eml_decodes_headers_parts_and_attachments() {
        let eml = "From: =?utf-8?B?Sm9zw6k=?= <jose@example.com>\r\nTo: a@b.c\r\nSubject: =?utf-8?Q?Caf=C3=A9_menu?=\r\nDate: Mon, 1 Jan 2024 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n--XX\r\nContent-Type: multipart/alternative; boundary=\"YY\"\r\n\r\n--YY\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHello =E2=9C=93 plain\r\n--YY\r\nContent-Type: text/html\r\n\r\n<p>Hello html</p>\r\n--YY--\r\n--XX\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0=\r\n--XX--\r\n";
        let text = eml_text(eml.as_bytes());
        assert!(
            text.contains("From: Jos\u{e9} <jose@example.com>"),
            "{text}"
        );
        assert!(text.contains("Subject: Caf\u{e9} menu"), "{text}");
        assert!(text.contains("Hello \u{2713} plain"), "{text}");
        assert!(!text.contains("Hello html"), "{text}");
        assert!(text.contains("[attachment: report.pdf]"), "{text}");
        assert!(!text.contains("JVBER"), "{text}");
    }

    #[test]
    fn html_only_mail_is_converted_and_mbox_splits_messages() {
        let one =
            "From: a@b.c\nSubject: One\nContent-Type: text/html\n\n<p>first <b>body</b></p>\n";
        let two = "From: d@e.f\nSubject: Two\n\nsecond body\n";
        let mbox = format!(
            "From a@b.c Sat Jan  1 00:00:00 2022\n{one}\nFrom d@e.f Sat Jan  1 00:00:01 2022\n{two}"
        );
        let text = mbox_text(mbox.as_bytes());
        assert!(
            text.contains("[message 1/2]") && text.contains("[message 2/2]"),
            "{text}"
        );
        assert!(
            text.contains("Subject: One") && text.contains("first body"),
            "{text}"
        );
        assert!(text.contains("second body"), "{text}");
    }

    #[test]
    fn ipynb_renders_cells_and_outputs() {
        let nb = r##"{"cells":[
            {"cell_type":"markdown","source":["# Title\n","text"]},
            {"cell_type":"code","source":"print(1)","outputs":[
                {"output_type":"stream","name":"stdout","text":["1\n"]},
                {"output_type":"display_data","data":{"image/png":"AAAA","text/plain":["<Figure>"]}},
                {"output_type":"error","ename":"ValueError","evalue":"bad"}]}],
            "metadata":{"language_info":{"name":"python"}},"nbformat":4}"##;
        let text = ipynb_text(nb).unwrap();
        assert!(text.starts_with("# Title\ntext\n"), "{text}");
        assert!(text.contains("```python\nprint(1)\n```"), "{text}");
        assert!(
            text.contains("    1\n")
                && text.contains("    <Figure>")
                && text.contains("ValueError: bad")
        );
        assert!(!text.contains("AAAA"));
        assert!(ipynb_text("{\"a\":1}").is_err());
    }

    #[test]
    fn subtitles_become_timestamped_lines() {
        let srt = "1\n00:00:01,000 --> 00:00:02,500\n<i>Hello</i>\nthere\n\n2\n00:00:03,000 --> 00:00:04,000\n{\\an8}Bye\n";
        assert_eq!(
            subtitles_text(srt, false),
            "[00:00:01] Hello there\n[00:00:03] Bye"
        );
        let vtt = "WEBVTT\n\nNOTE a comment\n\nintro\n00:01.000 --> 00:02.000 align:start\nHi <c.x>you</c>\n";
        assert_eq!(subtitles_text(vtt, true), "[00:00:01] Hi you");
    }

    #[test]
    fn csv_gets_a_table_preview_then_the_full_text() {
        let csv = "name,note\nann,\"a, b\"\nbob,\"two\nlines\"\n";
        let text = delimited_text(csv, ',');
        assert!(
            text.starts_with("table preview: 3 rows, 2 columns"),
            "{text}"
        );
        assert!(text.contains("| name | note |") && text.contains("| --- | --- |"));
        assert!(text.contains("| ann | a, b |") && text.contains("| bob | two lines |"));
        assert!(
            text.ends_with("two\nlines\"") || text.ends_with("lines\""),
            "{text}"
        );
        let tsv = delimited_text("a\tb\n1\t2\n", '\t');
        assert!(tsv.contains("| a | b |") && tsv.contains("| 1 | 2 |"));
    }

    #[test]
    fn svg_text_nodes_only() {
        let svg = "<svg><title>Chart</title><rect/><text x=\"1\">Revenue <tspan>up</tspan></text><text>Q&amp;A</text></svg>";
        assert_eq!(svg_text(svg), "Chart\nRevenue up\nQ&A");
        assert_eq!(svg_text("<svg><path d=\"M0\"/></svg>"), "");
    }
}
