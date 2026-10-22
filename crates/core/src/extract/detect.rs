//! Content-based file type detection. Magic bytes and inner structure decide;
//! the extension is only a hint for formats whose content cannot say (CSV vs
//! plain text, `.rst` vs `.txt`, which legacy office format an OLE file is).

use std::io::Cursor;

/// A detected file type. `label()` is the short name shown in statuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Odt,
    Ods,
    Odp,
    Epub,
    /// Office formats only `LibreOffice` reads: legacy doc/xls/ppt (OLE2),
    /// iWork, Visio, ODF drawings, …
    Office(&'static str),
    Rtf,
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Tiff,
    Heic,
    Avif,
    Svg,
    Html,
    Xml,
    Json,
    Ipynb,
    Yaml,
    Toml,
    Csv,
    Tsv,
    Eml,
    Mbox,
    Srt,
    Vtt,
    /// Text markup pandoc reads (rst, org, latex, textile, …); readable raw.
    Markup(&'static str),
    /// Anything else that is text: prose, source code, config, logs.
    Text,
    Audio(&'static str),
    Video(&'static str),
    /// A container nothing here opens (zip, gzip, 7z, …).
    Archive(&'static str),
    /// Not text and not recognised (label says what it looks like, if known).
    Binary(&'static str),
}

impl Kind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Pptx => "pptx",
            Self::Xlsx => "xlsx",
            Self::Odt => "odt",
            Self::Ods => "ods",
            Self::Odp => "odp",
            Self::Epub => "epub",
            Self::Rtf => "rtf",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Heic => "heic",
            Self::Avif => "avif",
            Self::Svg => "svg",
            Self::Html => "html",
            Self::Xml => "xml",
            Self::Json => "json",
            Self::Ipynb => "ipynb",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Eml => "eml",
            Self::Mbox => "mbox",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Text => "text",
            Self::Office(l)
            | Self::Markup(l)
            | Self::Audio(l)
            | Self::Video(l)
            | Self::Archive(l)
            | Self::Binary(l) => l,
        }
    }

    /// Still pictures the OCR / caption pipeline can read.
    pub const fn is_ocr_image(self) -> bool {
        matches!(
            self,
            Self::Png | Self::Jpeg | Self::Gif | Self::Webp | Self::Bmp | Self::Tiff
        )
    }

    pub const fn is_image(self) -> bool {
        self.is_ocr_image() || matches!(self, Self::Heic | Self::Avif)
    }

    /// The kinds that are text on disk (decoded, never treated as binary).
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            Self::Svg
                | Self::Html
                | Self::Xml
                | Self::Json
                | Self::Ipynb
                | Self::Yaml
                | Self::Toml
                | Self::Csv
                | Self::Tsv
                | Self::Eml
                | Self::Mbox
                | Self::Srt
                | Self::Vtt
                | Self::Markup(_)
                | Self::Rtf
                | Self::Text
        )
    }

    /// MIME type used to look up a user converter hook.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Odt => "application/vnd.oasis.opendocument.text",
            Self::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Self::Odp => "application/vnd.oasis.opendocument.presentation",
            Self::Epub => "application/epub+zip",
            Self::Rtf => "application/rtf",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Heic => "image/heic",
            Self::Avif => "image/avif",
            Self::Svg => "image/svg+xml",
            Self::Html => "text/html",
            Self::Xml => "application/xml",
            Self::Json | Self::Ipynb => "application/json",
            Self::Yaml => "application/yaml",
            Self::Toml => "application/toml",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
            Self::Eml => "message/rfc822",
            Self::Mbox => "application/mbox",
            Self::Srt => "application/x-subrip",
            Self::Vtt => "text/vtt",
            Self::Text | Self::Markup(_) => "text/plain",
            Self::Office(_) | Self::Archive(_) | Self::Binary(_) => "application/octet-stream",
            Self::Audio(_) => "audio/*",
            Self::Video(_) => "video/*",
        }
    }
}

/// How much of a file the content sniffers look at.
pub const SNIFF_BYTES: usize = 64 * 1024;

/// Detect by content, with `name`'s extension as a hint. Zip containers are
/// opened to tell docx / pptx / xlsx / odf / epub apart.
pub fn detect(bytes: &[u8], name: &str) -> Kind {
    let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
    let ext = extension(name);
    if (head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06"))
        && let Some(kind) = probe_zip(bytes, &ext)
    {
        return kind;
    }
    detect_head(head, name)
}

/// Like [`detect`] from a file's first bytes only: a zip is reported as an
/// archive (enough to route PDFs and images without reading the file).
pub fn detect_head(head: &[u8], name: &str) -> Kind {
    let ext = extension(name);
    if let Some(kind) = signature(head, &ext) {
        return kind;
    }
    sniff_text(head, &ext)
}

/// Lowercased extension of `name` (no dot).
pub fn extension(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default()
}

/// MIME type for the still-image formats a vision model accepts, judged by
/// content. `None` for everything else (including images models reject).
pub fn vision_mime(bytes: &[u8]) -> Option<&'static str> {
    match detect_head(&bytes[..bytes.len().min(64)], "") {
        Kind::Png => Some("image/png"),
        Kind::Jpeg => Some("image/jpeg"),
        Kind::Gif => Some("image/gif"),
        Kind::Webp => Some("image/webp"),
        _ => None,
    }
}

fn probe_zip(bytes: &[u8], ext: &str) -> Option<Kind> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    let starts = |p: &str| names.iter().any(|x| x.starts_with(p));
    // ODF and EPUB put their MIME type in a stored `mimetype` entry.
    if has("mimetype") {
        let mut mime = String::new();
        if let Ok(mut e) = zip.by_name("mimetype") {
            use std::io::Read as _;
            let _ = e.by_ref().take(256).read_to_string(&mut mime);
        }
        let mime = mime.trim();
        return Some(match mime {
            "application/epub+zip" => Kind::Epub,
            "application/vnd.oasis.opendocument.text" => Kind::Odt,
            "application/vnd.oasis.opendocument.spreadsheet" => Kind::Ods,
            "application/vnd.oasis.opendocument.presentation" => Kind::Odp,
            m if m.starts_with("application/vnd.oasis.opendocument.") => Kind::Office("odf"),
            _ => Kind::Archive("zip"),
        });
    }
    if has("[Content_Types].xml") {
        return Some(if has("word/document.xml") {
            Kind::Docx
        } else if has("ppt/presentation.xml") || starts("ppt/slides/") {
            Kind::Pptx
        } else if has("xl/workbook.xml") || starts("xl/worksheets/") {
            Kind::Xlsx
        } else if starts("visio/") {
            Kind::Office("vsdx")
        } else {
            Kind::Archive("ooxml")
        });
    }
    // iWork bundles are zips with an `Index/` directory.
    if starts("Index/") && matches!(ext, "key" | "pages" | "numbers") {
        return Some(match ext {
            "key" => Kind::Office("key"),
            "pages" => Kind::Office("pages"),
            _ => Kind::Office("numbers"),
        });
    }
    Some(Kind::Archive("zip"))
}

/// Binary signatures. `None` means "not a recognised binary format".
#[allow(clippy::too_many_lines)]
fn signature(h: &[u8], ext: &str) -> Option<Kind> {
    let at = |off: usize, pat: &[u8]| h.len() >= off + pat.len() && &h[off..off + pat.len()] == pat;
    // PDFs may carry a few junk bytes before the header.
    if h.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return Some(Kind::Pdf);
    }
    if at(0, b"\x89PNG\r\n\x1a\n") {
        return Some(Kind::Png);
    }
    if at(0, &[0xFF, 0xD8, 0xFF]) {
        return Some(Kind::Jpeg);
    }
    if at(0, b"GIF87a") || at(0, b"GIF89a") {
        return Some(Kind::Gif);
    }
    if at(0, b"RIFF") {
        if at(8, b"WEBP") {
            return Some(Kind::Webp);
        }
        if at(8, b"WAVE") {
            return Some(Kind::Audio("wav"));
        }
        if at(8, b"AVI ") {
            return Some(Kind::Video("avi"));
        }
    }
    if at(0, b"BM") && h.len() >= 18 && h[6..10] == [0, 0, 0, 0] {
        let dib = u32::from_le_bytes([h[14], h[15], h[16], h[17]]);
        if matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124) {
            return Some(Kind::Bmp);
        }
    }
    if at(0, b"II*\0") || at(0, b"MM\0*") {
        return Some(Kind::Tiff);
    }
    if at(4, b"ftyp") && h.len() >= 12 {
        return Some(match &h[8..12] {
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1" => {
                Kind::Heic
            }
            b"avif" | b"avis" => Kind::Avif,
            b"M4A " | b"M4B " | b"M4P " => Kind::Audio("m4a"),
            b"qt  " => Kind::Video("mov"),
            _ => Kind::Video("mp4"),
        });
    }
    if at(0, b"{\\rtf") {
        return Some(Kind::Rtf);
    }
    if at(0, b"OggS") && h.get(4) == Some(&0) {
        return Some(Kind::Audio("ogg"));
    }
    if at(0, b"fLaC") {
        return Some(Kind::Audio("flac"));
    }
    if at(0, b"MThd") && at(4, &[0, 0, 0, 6]) {
        return Some(Kind::Audio("midi"));
    }
    if at(0, b"FORM") && at(8, b"AIFF") {
        return Some(Kind::Audio("aiff"));
    }
    if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        let webm = h.windows(4).take(64).any(|w| w == b"webm");
        return Some(Kind::Video(if webm { "webm" } else { "mkv" }));
    }
    if at(0, &[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        return Some(Kind::Video("wmv"));
    }
    if at(0, b"FLV\x01") {
        return Some(Kind::Video("flv"));
    }
    if id3_header(h) || mpeg_audio_frame(h) {
        return Some(Kind::Audio("mp3"));
    }
    if at(0, &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Some(Kind::Office(match ext {
            "doc" | "dot" | "wbk" => "doc",
            "xls" | "xlt" | "xla" => "xls",
            "ppt" | "pps" | "pot" => "ppt",
            "msg" => "msg",
            _ => "ole",
        }));
    }
    if at(0, &[0x1F, 0x8B]) {
        return Some(Kind::Archive("gzip"));
    }
    if at(0, b"BZh") && h.get(3).is_some_and(|b| (b'1'..=b'9').contains(b)) && at(4, b"1AY&SY") {
        return Some(Kind::Archive("bzip2"));
    }
    if at(0, &[0xFD, b'7', b'z', b'X', b'Z', 0]) {
        return Some(Kind::Archive("xz"));
    }
    if at(0, &[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(Kind::Archive("zstd"));
    }
    if at(0, &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(Kind::Archive("7z"));
    }
    if at(0, b"Rar!\x1a\x07") {
        return Some(Kind::Archive("rar"));
    }
    if at(257, b"ustar") {
        return Some(Kind::Archive("tar"));
    }
    if at(0, b"PK\x03\x04") || at(0, b"PK\x05\x06") {
        return Some(Kind::Archive("zip"));
    }
    if at(0, b"SQLite format 3\0") {
        return Some(Kind::Binary("sqlite"));
    }
    if at(0, &[0x7F, b'E', b'L', b'F']) {
        return Some(Kind::Binary("elf"));
    }
    if at(0, &[0xCF, 0xFA, 0xED, 0xFE])
        || at(0, &[0xCE, 0xFA, 0xED, 0xFE])
        || at(0, &[0xCA, 0xFE, 0xBA, 0xBE])
    {
        return Some(Kind::Binary("mach-o"));
    }
    if at(0, b"MZ") && is_pe(h) {
        return Some(Kind::Binary("exe"));
    }
    if at(0, b"\0asm") {
        return Some(Kind::Binary("wasm"));
    }
    if at(0, b"wOFF") || at(0, b"wOF2") || at(0, b"OTTO") || at(0, &[0, 1, 0, 0, 0]) {
        return Some(Kind::Binary("font"));
    }
    if at(0, b"8BPS") && (at(4, &[0, 1]) || at(4, &[0, 2])) {
        return Some(Kind::Binary("photoshop"));
    }
    if at(0, &[0, 0, 1, 0]) {
        return Some(Kind::Binary("ico"));
    }
    None
}

/// An `ID3v2` tag: `ID3`, a version byte, flags, then a sync-safe size.
fn id3_header(h: &[u8]) -> bool {
    h.len() >= 10
        && h.starts_with(b"ID3")
        && h[3] < 10
        && h[4] != 0xFF
        && h[6..10].iter().all(|b| b & 0x80 == 0)
}

/// An MPEG layer III frame header (`FF Ex`) with sane version / bitrate /
/// sample-rate bits. Layer III only: `FF FE`, a UTF-16 byte-order mark, must
/// never read as audio.
fn mpeg_audio_frame(h: &[u8]) -> bool {
    h.len() >= 3
        && h[0] == 0xFF
        && h[1] & 0xE0 == 0xE0
        && (h[1] >> 3) & 3 != 1
        && (h[1] >> 1) & 3 == 1
        && h[2] >> 4 != 0xF
        && h[2] >> 4 != 0
        && (h[2] >> 2) & 3 != 3
}

/// A DOS `MZ` header whose PE pointer lands on `PE\0\0` (or beyond what we
/// sniffed): rules out text that merely starts with those letters.
fn is_pe(h: &[u8]) -> bool {
    if h.len() < 0x40 {
        return false;
    }
    let off = u32::from_le_bytes([h[0x3C], h[0x3D], h[0x3E], h[0x3F]]) as usize;
    match h.get(off..off + 4) {
        Some(sig) => sig == b"PE\0\0",
        None => (0x40..(1 << 20)).contains(&off),
    }
}

/// How a text file is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// The encoding of a text file, or `None` when the bytes are binary.
/// UTF-16 requires a BOM so arbitrary binary is never read as source code.
pub fn text_encoding(h: &[u8]) -> Option<Encoding> {
    if h.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(Encoding::Utf8);
    }
    if h.starts_with(&[0xFF, 0xFE]) {
        return Some(Encoding::Utf16Le);
    }
    if h.starts_with(&[0xFE, 0xFF]) {
        return Some(Encoding::Utf16Be);
    }
    if h.contains(&0) {
        return None;
    }
    let control = h
        .iter()
        .filter(|b| **b < 0x20 && !matches!(**b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1B))
        .count();
    // A few stray control bytes are fine (logs); a stream of them is binary.
    (control * 50 <= h.len()).then_some(Encoding::Utf8)
}

/// Decode a whole text file: BOMs stripped, UTF-16 transcoded, invalid UTF-8
/// replaced rather than rejected.
pub fn decode_text(bytes: &[u8], enc: Encoding) -> String {
    match enc {
        Encoding::Utf8 => String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .to_string(),
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let le = enc == Encoding::Utf16Le;
            let units: Vec<u16> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| {
                    if le {
                        u16::from_le_bytes(*p)
                    } else {
                        u16::from_be_bytes(*p)
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
    }
}

/// Extensions of template / source files that merely contain markup; they
/// are indexed as the code they are.
const CODE_EXTS: &[&str] = &[
    "php",
    "vue",
    "svelte",
    "jsx",
    "tsx",
    "erb",
    "ejs",
    "hbs",
    "handlebars",
    "jinja",
    "j2",
    "njk",
    "liquid",
    "astro",
    "cshtml",
    "razor",
    "aspx",
    "jsp",
    "mustache",
    "twig",
    "tpl",
    "xsl",
    "xslt",
];

/// Text markups pandoc can read; the extension is the only signal.
fn markup_format(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rst" => "rst",
        "org" => "org",
        "tex" | "latex" => "latex",
        "textile" => "textile",
        "adoc" | "asciidoc" => "asciidoc",
        "wiki" | "mediawiki" => "mediawiki",
        "dokuwiki" => "dokuwiki",
        "opml" => "opml",
        "fb2" => "fb2",
        "bib" | "bibtex" => "bibtex",
        "ris" => "ris",
        "typ" => "typst",
        "creole" => "creole",
        "t2t" => "t2t",
        "muse" => "muse",
        _ => return None,
    })
}

fn sniff_text(h: &[u8], ext: &str) -> Kind {
    let Some(enc) = text_encoding(h) else {
        return Kind::Binary("unknown");
    };
    if h.is_empty() {
        return Kind::Text;
    }
    // Look at the start of the decoded text (lossy is fine for sniffing).
    let decoded = decode_text(&h[..h.len().min(8192)], enc);
    let start = decoded.trim_start();
    let lower: String = start.chars().take(4096).collect::<String>().to_lowercase();
    if CODE_EXTS.contains(&ext) {
        return Kind::Text;
    }
    if lower.starts_with("{\\rtf") {
        return Kind::Rtf;
    }
    if lower.starts_with("<svg") || (lower.starts_with("<?xml") && lower.contains("<svg")) {
        return Kind::Svg;
    }
    let doc = strip_leading_comments(&lower);
    let htmlish = doc.starts_with("<!doctype html")
        || doc.starts_with("<html")
        || (doc.starts_with("<?xml") && doc.contains("<html"))
        || (matches!(ext, "html" | "htm" | "xhtml" | "shtml")
            && (lower.contains("</")
                || [
                    "<body", "<div", "<p>", "<p ", "<head", "<table", "<h1", "<span", "<a ",
                ]
                .iter()
                .any(|t| lower.contains(t))));
    if htmlish {
        return Kind::Html;
    }
    if lower.starts_with("webvtt") {
        return Kind::Vtt;
    }
    if is_srt(start) {
        return Kind::Srt;
    }
    if is_mbox(start) {
        return Kind::Mbox;
    }
    if is_eml(start) {
        return Kind::Eml;
    }
    if lower.contains("\"nbformat\"") && (lower.starts_with('{') || ext == "ipynb") {
        return Kind::Ipynb;
    }
    match ext {
        "json" | "jsonl" | "ndjson" | "geojson" => return Kind::Json,
        "yaml" | "yml" => return Kind::Yaml,
        "toml" => return Kind::Toml,
        "xml" | "rss" | "atom" | "plist" | "xsd" | "kml" | "gpx" | "xhtml" => return Kind::Xml,
        "ipynb" => return Kind::Ipynb,
        "srt" => return Kind::Srt,
        "vtt" => return Kind::Vtt,
        "eml" => return Kind::Eml,
        "mbox" => return Kind::Mbox,
        "csv" => return Kind::Csv,
        "tsv" | "tab" => return Kind::Tsv,
        _ => {}
    }
    if let Some(fmt) = markup_format(ext) {
        return Kind::Markup(fmt);
    }
    if lower.starts_with("<?xml") {
        return Kind::Xml;
    }
    if matches!(ext, "" | "txt" | "dat" | "text" | "log")
        && let Some(kind) = delimited(start)
    {
        return kind;
    }
    Kind::Text
}

/// `s` without leading whitespace and `<!-- … -->` comments.
fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        match s.strip_prefix("<!--") {
            Some(rest) => match rest.find("-->") {
                Some(end) => s = &rest[end + 3..],
                None => return "",
            },
            None => return s,
        }
    }
}

fn is_srt(text: &str) -> bool {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('\u{feff}');
    let second = lines.next().unwrap_or("");
    !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && second.contains(" --> ")
        && second.bytes().filter(|b| *b == b':').count() >= 2
}

fn is_mbox(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("");
    first.starts_with("From ") && first.contains('@') && first.contains(':')
}

fn is_eml(text: &str) -> bool {
    const STARTS: &[&str] = &[
        "received:",
        "return-path:",
        "delivered-to:",
        "mime-version:",
        "message-id:",
        "from:",
        "x-",
    ];
    let head: String = text.chars().take(2048).collect::<String>().to_lowercase();
    STARTS.iter().any(|s| head.starts_with(s))
        && head.lines().take_while(|l| !l.trim().is_empty()).any(|l| {
            l.starts_with("subject:") || l.starts_with("from:") || l.starts_with("message-id:")
        })
        && (head.contains("\nto:") || head.contains("\nsubject:") || head.contains("\ndate:"))
}

/// A consistent delimiter across the first lines marks CSV / TSV even
/// without a telling extension.
fn delimited(text: &str) -> Option<Kind> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(12)
        .collect();
    if lines.len() < 3 {
        return None;
    }
    for (delim, kind, min) in [
        ('\t', Kind::Tsv, 1),
        (',', Kind::Csv, 2),
        (';', Kind::Csv, 2),
    ] {
        let counts: Vec<usize> = lines.iter().map(|l| l.matches(delim).count()).collect();
        if counts[0] >= min && counts.iter().all(|c| *c == counts[0]) {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(entries: &[(&str, &str)]) -> Vec<u8> {
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

    fn png_header() -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0; 32]);
        v
    }

    #[test]
    fn binary_signatures_are_recognised_whatever_the_name() {
        let cases: Vec<(Vec<u8>, Kind)> = vec![
            (b"%PDF-1.7\n".to_vec(), Kind::Pdf),
            (b"\n\n  junk%PDF-1.4".to_vec(), Kind::Pdf),
            (png_header(), Kind::Png),
            (vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10], Kind::Jpeg),
            (b"GIF89a\x01\x00".to_vec(), Kind::Gif),
            (b"RIFF\x24\0\0\0WEBPVP8 ".to_vec(), Kind::Webp),
            (b"RIFF\x24\0\0\0WAVEfmt ".to_vec(), Kind::Audio("wav")),
            (b"RIFF\x24\0\0\0AVI LIST".to_vec(), Kind::Video("avi")),
            (b"II*\0\x08\0\0\0".to_vec(), Kind::Tiff),
            (b"\0\0\0\x18ftypheic\0\0\0\0".to_vec(), Kind::Heic),
            (b"\0\0\0\x18ftypM4A \0\0\0\0".to_vec(), Kind::Audio("m4a")),
            (b"\0\0\0\x18ftypisom\0\0\0\0".to_vec(), Kind::Video("mp4")),
            (b"{\\rtf1\\ansi hi}".to_vec(), Kind::Rtf),
            (b"OggS\0\x02".to_vec(), Kind::Audio("ogg")),
            (b"fLaC\0\0".to_vec(), Kind::Audio("flac")),
            (b"ID3\x04\0\0\0\0\0\x10".to_vec(), Kind::Audio("mp3")),
            (vec![0x1A, 0x45, 0xDF, 0xA3, 0, 0], Kind::Video("mkv")),
            (vec![0x1F, 0x8B, 8, 0], Kind::Archive("gzip")),
            (
                vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
                Kind::Office("ole"),
            ),
            (b"\x7fELF\x02\x01".to_vec(), Kind::Binary("elf")),
            (b"SQLite format 3\0".to_vec(), Kind::Binary("sqlite")),
        ];
        for (bytes, kind) in cases {
            assert_eq!(detect(&bytes, "mystery.txt"), kind, "{kind:?}");
        }
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 40, 0, 0, 0]);
        assert_eq!(detect(&bmp, "x"), Kind::Bmp);
    }

    #[test]
    fn a_wrong_extension_does_not_fool_detection() {
        assert_eq!(detect(&png_header(), "report.pdf"), Kind::Png);
        assert_eq!(detect(b"%PDF-1.4 body", "photo.png"), Kind::Pdf);
        assert_eq!(
            detect(b"<!DOCTYPE html><html><body>hi</body></html>", "notes.txt"),
            Kind::Html
        );
        assert_eq!(detect(b"plain words", "scan.pdf"), Kind::Text);
        assert_eq!(
            detect(&[0, 159, 146, 150], "doc.docx"),
            Kind::Binary("unknown")
        );
    }

    #[test]
    fn zip_containers_are_told_apart_by_their_inner_structure() {
        let docx = zip_with(&[
            ("[Content_Types].xml", "<Types/>"),
            ("word/document.xml", "<w/>"),
        ]);
        let pptx = zip_with(&[
            ("[Content_Types].xml", "<Types/>"),
            ("ppt/slides/slide1.xml", "<p/>"),
        ]);
        let xlsx = zip_with(&[
            ("[Content_Types].xml", "<Types/>"),
            ("xl/workbook.xml", "<w/>"),
        ]);
        let odt = zip_with(&[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            ("content.xml", "<o/>"),
        ]);
        let ods = zip_with(&[("mimetype", "application/vnd.oasis.opendocument.spreadsheet")]);
        let odp = zip_with(&[(
            "mimetype",
            "application/vnd.oasis.opendocument.presentation",
        )]);
        let epub = zip_with(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", "<c/>"),
        ]);
        let plain = zip_with(&[("a.txt", "hi")]);
        for (bytes, name, kind) in [
            (&docx, "x.zip", Kind::Docx),
            (&pptx, "x", Kind::Pptx),
            (&xlsx, "x.docx", Kind::Xlsx),
            (&odt, "x.bin", Kind::Odt),
            (&ods, "x", Kind::Ods),
            (&odp, "x", Kind::Odp),
            (&epub, "x", Kind::Epub),
            (&plain, "x.docx", Kind::Archive("zip")),
        ] {
            assert_eq!(detect(bytes, name), kind, "{name}");
        }
        // Head-only detection cannot open the archive.
        assert_eq!(detect_head(&docx, "x.docx"), Kind::Archive("zip"));
    }

    #[test]
    fn utf8_and_utf16_boms_decode_to_text() {
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("print(42)".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(detect(&utf16, "script.py"), Kind::Text);
        let enc = text_encoding(&utf16).unwrap();
        assert_eq!(decode_text(&utf16, enc), "print(42)");
        let mut be = vec![0xFE, 0xFF];
        be.extend("hi".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode_text(&be, text_encoding(&be).unwrap()), "hi");
        let utf8 = b"\xEF\xBB\xBFhello";
        assert_eq!(decode_text(utf8, text_encoding(utf8).unwrap()), "hello");
        // No BOM + NULs is binary, not UTF-16.
        assert!(text_encoding(b"h\0i\0").is_none());
    }

    #[test]
    fn text_subtypes_come_from_content_with_extension_hints() {
        let cases: &[(&str, &str, Kind)] = &[
            ("<svg xmlns=\"x\"><text>a</text></svg>", "a.bin", Kind::Svg),
            ("<?xml version=\"1.0\"?><svg></svg>", "a", Kind::Svg),
            ("<?xml version=\"1.0\"?><root/>", "a", Kind::Xml),
            ("WEBVTT\n\n00:00.000 --> 00:01.000\nhi", "a.txt", Kind::Vtt),
            ("1\n00:00:01,000 --> 00:00:02,000\nhi\n", "a.txt", Kind::Srt),
            ("{\"nbformat\": 4, \"cells\": []}", "a.json", Kind::Ipynb),
            ("{\"a\": 1}", "a.json", Kind::Json),
            ("a: 1", "a.yml", Kind::Yaml),
            ("a = 1", "a.toml", Kind::Toml),
            ("Title\n=====\n", "a.rst", Kind::Markup("rst")),
            (
                "From: a@b.c\nTo: d@e.f\nSubject: Hi\n\nbody",
                "mail",
                Kind::Eml,
            ),
            (
                "From a@b.c Sat Jan  1 00:00:00 2022\nSubject: x\n\nbody",
                "m",
                Kind::Mbox,
            ),
            ("a,b,c\n1,2,3\n4,5,6\n", "data.txt", Kind::Csv),
            ("a\tb\n1\t2\n3\t4\n", "", Kind::Tsv),
            ("<div>fragment</div>", "frag.html", Kind::Html),
            ("<div>not html here</div>", "frag.txt", Kind::Text),
            ("<!DOCTYPE html><title>t</title>", "page.php", Kind::Text),
            ("fn main() {}", "main.rs", Kind::Text),
            ("", "empty.txt", Kind::Text),
        ];
        for (body, name, kind) in cases {
            assert_eq!(detect(body.as_bytes(), name), *kind, "{name}: {body:?}");
        }
    }

    #[test]
    fn vision_mime_only_accepts_formats_models_read() {
        assert_eq!(vision_mime(&png_header()), Some("image/png"));
        assert_eq!(vision_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(vision_mime(b"%PDF-1.4"), None);
        assert_eq!(vision_mime(b"just text"), None);
        assert_eq!(vision_mime(b"II*\0\x08\0\0\0"), None);
    }

    #[test]
    fn legacy_office_label_follows_the_extension_hint() {
        let ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0];
        assert_eq!(detect(&ole, "a.DOC"), Kind::Office("doc"));
        assert_eq!(detect(&ole, "a.xls"), Kind::Office("xls"));
        assert_eq!(detect(&ole, "a.ppt"), Kind::Office("ppt"));
    }
}
