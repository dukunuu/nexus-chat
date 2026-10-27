//! Text shaping for decks: fonts, inline Markdown and a greedy typesetter that
//! measures with the very font database `resvg` renders with. The layout
//! engine (`layout.rs`) and the SVG renderer (`render.rs`) share it, so a box
//! is as tall as the text that is drawn in it.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use resvg::{tiny_skia, usvg};

use super::model::{Align, ResolvedFont, Theme, Transform};

/// Numbers shared by the SVG renderer here and the HTML renderer in
/// `crates/studio/ui` (`studio.css`, `render.js`). Lengths in `em` are
/// relative to the node's font size.
pub struct LayoutSpec {
    pub paragraph_gap: f64,
    /// Heading levels 1 to 3 as a multiple of the node's font size.
    pub heading_scale: [f64; 3],
    pub heading_line_height: f64,
    pub heading_gap: f64,
    pub heading_weight: u16,
    pub list_indent: f64,
    pub list_item_padding: f64,
    /// Padding around a code block.
    pub code_padding: f64,
    pub cell_padding_y: f64,
    pub cell_padding_x: f64,
    /// Horizontal padding of inline code.
    pub inline_code_padding: f64,
    pub table_border: f64,
    pub chart_width: f64,
    pub chart_height: f64,
    /// Text with a line height below this has no gap between paragraphs
    /// (titles), like version 1's `xl` and `xxl` sizes.
    pub tight_line_height: f64,
}

/// The shared layout spec.
pub const LAYOUT: LayoutSpec = LayoutSpec {
    paragraph_gap: 0.3,
    heading_scale: [1.4, 1.2, 1.05],
    heading_line_height: 1.1,
    heading_gap: 0.35,
    heading_weight: 600,
    list_indent: 1.4,
    list_item_padding: 0.5,
    code_padding: 0.5,
    cell_padding_y: 0.25,
    cell_padding_x: 0.4,
    inline_code_padding: 0.12,
    table_border: 1.0,
    chart_width: 320.0,
    chart_height: 180.0,
    tight_line_height: 1.2,
};

pub(super) const CODE_TINT: &str = "#8d8875";
/// Maximum size of an imported deck font, shared with studio.
pub const MAX_FONT_FILE: usize = 16 * 1024 * 1024;

// -- fonts -----------------------------------------------------------------

/// The system fonts, with the generic families the SVG stacks end with.
pub fn system_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    Arc::clone(FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        let first_installed = |db: &usvg::fontdb::Database, names: &[&'static str]| {
            names.iter().copied().find(|name| {
                db.faces()
                    .any(|face| face.families.iter().any(|(family, _)| family == name))
            })
        };
        // Without these, a machine whose default `sans-serif` face is
        // missing draws nothing.
        if let Some(name) = first_installed(
            &db,
            &[
                "Helvetica Neue",
                "Segoe UI",
                "Inter",
                "DejaVu Sans",
                "Liberation Sans",
                "Noto Sans",
                "Arial",
            ],
        ) {
            db.set_sans_serif_family(name);
        }
        if let Some(name) = first_installed(
            &db,
            &[
                "Georgia",
                "New York",
                "DejaVu Serif",
                "Liberation Serif",
                "Noto Serif",
                "Times New Roman",
            ],
        ) {
            db.set_serif_family(name);
        }
        if let Some(name) = first_installed(
            &db,
            &[
                "Menlo",
                "SFMono-Regular",
                "Consolas",
                "DejaVu Sans Mono",
                "Liberation Mono",
                "Noto Sans Mono",
                "Courier New",
            ],
        ) {
            db.set_monospace_family(name);
        }
        Arc::new(db)
    }))
}

/// Every installed family name, sorted.
#[must_use]
pub fn installed_families() -> Vec<String> {
    let mut names: Vec<String> = system_fonts()
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names
}

/// Installed family metadata for the studio font picker.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FontFamily {
    pub family: String,
    pub weights: Vec<u16>,
    pub italic: bool,
    pub variable: bool,
}

/// Cached families and available weights from the renderer's font database.
#[must_use]
pub fn installed_font_info() -> &'static [FontFamily] {
    static INFO: OnceLock<Vec<FontFamily>> = OnceLock::new();
    INFO.get_or_init(|| font_info(&system_fonts()))
}

/// Family metadata including the deck's imported faces and variable fonts.
#[must_use]
pub fn deck_font_info(theme: &Theme, assets: Option<&Path>) -> Vec<FontFamily> {
    if theme.font_files.is_empty() {
        return installed_font_info().to_vec();
    }
    font_info(&deck_fonts(theme, assets))
}

fn font_info(db: &usvg::fontdb::Database) -> Vec<FontFamily> {
    let mut families = BTreeMap::<String, FontFamily>::new();
    for face in db.faces() {
        let Some((name, _)) = face.families.first() else {
            continue;
        };
        let entry = families.entry(name.clone()).or_insert_with(|| FontFamily {
            family: name.clone(),
            weights: Vec::new(),
            italic: false,
            variable: false,
        });
        entry.weights.push(face.weight.0);
        entry.italic |= face.style != usvg::fontdb::Style::Normal;
        entry.variable |= db.with_face_data(face.id, variable_font).unwrap_or(false);
    }
    let mut result: Vec<_> = families.into_values().collect();
    for family in &mut result {
        family.weights.sort_unstable();
        family.weights.dedup();
    }
    result.sort_by_key(|f| f.family.to_lowercase());
    result
}

// A variable OpenType face has an `fvar` table. No shaping or decompression is
// needed to inspect the sfnt directory (including a face within a collection).
fn variable_font(data: &[u8], index: u32) -> bool {
    let u32_at = |at: usize| -> Option<usize> {
        Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    let offset = if data.starts_with(b"ttcf") {
        let Some(offset) = u32_at(12 + index as usize * 4) else {
            return false;
        };
        offset
    } else {
        0
    };
    let Some(count) = data.get(offset + 4..offset + 6) else {
        return false;
    };
    let count = usize::from(u16::from_be_bytes([count[0], count[1]]));
    (0..count).any(|i| data.get(offset + 12 + i * 16..offset + 16 + i * 16) == Some(b"fvar"))
}

/// Read a font's own family, weight and italic flag, if fontdb can decode it.
#[must_use]
pub fn imported_font_info(bytes: &[u8]) -> Option<(String, u16, bool)> {
    let mut db = usvg::fontdb::Database::new();
    db.load_font_data(bytes.to_vec());
    let face = db.faces().next()?;
    Some((
        face.families.first()?.0.clone(),
        face.weight.0,
        face.style != usvg::fontdb::Style::Normal,
    ))
}

/// Whether `path` is a font file the core renderer can load (`woff2` is for
/// the editor's webview only).
#[must_use]
pub fn loadable_font(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc"))
}

/// The system fonts plus the deck's imported font files, each registered
/// under the family name the theme gives it. Files that are missing, too
/// large, outside `assets` or not loadable are skipped.
#[must_use]
pub fn deck_fonts(theme: &Theme, assets: Option<&Path>) -> Arc<usvg::fontdb::Database> {
    let system = system_fonts();
    let Some(root) = assets.and_then(|a| a.canonicalize().ok()) else {
        return system;
    };
    if theme.font_files.is_empty() {
        return system;
    }
    let mut db = (*system).clone();
    for file in &theme.font_files {
        let relative = file.file.strip_prefix("assets/").unwrap_or(&file.file);
        let Ok(path) = root.join(relative).canonicalize() else {
            continue;
        };
        if !path.starts_with(&root)
            || !loadable_font(&file.file)
            || std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_FONT_FILE as u64)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let ids = db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(bytes)));
        for id in ids {
            let Some(mut info) = db.face(id).cloned() else {
                continue;
            };
            db.remove_face(id);
            info.families = vec![(
                file.family.clone(),
                usvg::fontdb::Language::English_UnitedStates,
            )];
            info.weight = usvg::fontdb::Weight(file.weight);
            info.style = if file.italic {
                usvg::fontdb::Style::Italic
            } else {
                usvg::fontdb::Style::Normal
            };
            db.push_face_info(info);
        }
    }
    Arc::new(db)
}

/// XML-escape text and attribute values.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A theme role, a hex literal, or the foreground color.
pub(crate) fn color(theme: &Theme, value: &str) -> String {
    if let Some(color) = theme.colors.get(value) {
        return color.clone();
    }
    let hex = value.strip_prefix('#').filter(|digits| {
        matches!(digits.len(), 3 | 4 | 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit())
    });
    if hex.is_some() {
        value.into()
    } else {
        theme
            .colors
            .get("fg")
            .cloned()
            .unwrap_or_else(|| "#282923".into())
    }
}

fn clean_family(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '"' | '\'' | '\\' | ';'))
        .collect()
}

/// Font stack for a role or family: the family, its declared fallbacks, then
/// platform fonts of the same flavor. Mirrors `fontStack` in `render.js`.
#[must_use]
pub fn font_stack(theme: &Theme, family: &str) -> String {
    let name = clean_family(theme.family(family));
    let extra: String = theme
        .font_files
        .iter()
        .find(|f| f.family == name)
        .map(|f| {
            f.fallback.iter().fold(String::new(), |mut out, fallback| {
                let _ = write!(out, ", '{}'", clean_family(fallback));
                out
            })
        })
        .unwrap_or_default();
    let lower = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    if has(&["mono", "code", "consolas", "menlo"]) {
        format!("'{name}'{extra}, SFMono-Regular, Menlo, monospace")
    } else if !lower.contains("sans")
        && has(&[
            "serif",
            "georgia",
            "times",
            "baskerville",
            "garamond",
            "new york",
        ])
    {
        format!("'{name}'{extra}, 'New York', Georgia, serif")
    } else {
        format!("'{name}'{extra}, 'Helvetica Neue', 'Segoe UI', sans-serif")
    }
}

/// The stack for code: the family, then monospace platform fonts.
#[must_use]
pub fn code_stack(theme: &Theme, family: &str) -> String {
    let stack = font_stack(theme, family);
    if stack.ends_with("monospace") {
        stack
    } else {
        format!(
            "'{}', SFMono-Regular, Menlo, monospace",
            clean_family(theme.family(family))
        )
    }
}

/// The font for inline code and code blocks' default, from the `mono` role.
fn mono_stack(theme: &Theme) -> String {
    let stack = font_stack(theme, "mono");
    if stack.ends_with("monospace") {
        stack
    } else {
        "Menlo, SFMono-Regular, monospace".into()
    }
}

// -- inline Markdown ---------------------------------------------------------

/// How a run of inline text is marked up; the Markdown subset does not nest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mark {
    #[default]
    Plain,
    Bold,
    Italic,
    Code,
    Link,
}

/// A styled stretch of text inside one paragraph.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Run {
    pub text: String,
    pub mark: Mark,
}

/// `open` inner `close`, where `inner` is non-empty and free of `forbidden`.
fn delimited<'a>(
    rest: &'a str,
    open: &str,
    close: &str,
    forbidden: char,
) -> Option<(&'a str, usize)> {
    let body = rest.strip_prefix(open)?;
    let end = body.find(close)?;
    let inner = &body[..end];
    (!inner.is_empty() && !inner.contains(forbidden))
        .then_some((inner, open.len() + end + close.len()))
}

/// `[label](target)` with a whitespace-free target.
fn link(rest: &str) -> Option<(&str, usize)> {
    let body = rest.strip_prefix('[')?;
    let close = body.find(']')?;
    let label = &body[..close];
    let after = body[close + 1..].strip_prefix('(')?;
    let end = after.find(')')?;
    let target = &after[..end];
    let valid = !label.is_empty() && !target.is_empty() && !target.contains(char::is_whitespace);
    valid.then_some((label, close + end + 4))
}

/// The inline token at the start of `rest`, in the order `inlineTokens` in
/// `helpers.js` tries them: bold, italic, code, link.
fn token(rest: &str) -> Option<(Run, usize)> {
    let styled = |inner: &str, mark| Run {
        text: inner.into(),
        mark,
    };
    if let Some((inner, length)) = delimited(rest, "**", "**", '*') {
        return Some((styled(inner, Mark::Bold), length));
    }
    if let Some((inner, length)) = delimited(rest, "*", "*", '*') {
        return Some((styled(inner, Mark::Italic), length));
    }
    if let Some((inner, length)) = delimited(rest, "`", "`", '`') {
        return Some((styled(inner, Mark::Code), length));
    }
    link(rest).map(|(label, length)| (styled(label, Mark::Link), length))
}

pub(super) fn inline(text: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        if let Some((run, length)) = token(rest) {
            if !plain.is_empty() {
                runs.push(Run {
                    text: std::mem::take(&mut plain),
                    ..Run::default()
                });
            }
            runs.push(run);
            rest = &rest[length..];
        } else {
            plain.push(first);
            rest = &rest[first.len_utf8()..];
        }
    }
    if !plain.is_empty() {
        runs.push(Run {
            text: plain,
            ..Run::default()
        });
    }
    runs
}

/// One Markdown line, classified as `markdownTokens` in `helpers.js` does.
#[derive(Debug, PartialEq)]
pub(super) enum Row<'a> {
    Heading(usize, &'a str),
    Bullet(&'a str),
    Numbered(&'a str),
    Paragraph(&'a str),
}

pub(super) fn classify(line: &str) -> Row<'_> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=3).contains(&hashes)
        && let Some(rest) = line[hashes..].strip_prefix(char::is_whitespace)
    {
        return Row::Heading(hashes, rest.trim_start());
    }
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix(['-', '*'])
        && let Some(rest) = rest.strip_prefix(char::is_whitespace)
    {
        return Row::Bullet(rest.trim_start());
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) = trimmed[digits..].strip_prefix('.')
        && let Some(rest) = rest.strip_prefix(char::is_whitespace)
    {
        return Row::Numbered(rest.trim_start());
    }
    Row::Paragraph(line)
}

/// Apply a CSS-like `text-transform`.
fn transformed(text: &str, transform: Transform) -> String {
    match transform {
        Transform::None => text.into(),
        Transform::Uppercase => text.to_uppercase(),
        Transform::Lowercase => text.to_lowercase(),
        Transform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
    }
}

// -- text layout -------------------------------------------------------------

/// A font choice for one piece of text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Face {
    pub family: String,
    pub weight: u16,
    pub italic: bool,
    /// Letter spacing in px.
    pub spacing: f64,
}

impl Face {
    pub fn attributes(&self) -> String {
        let mut out = format!(
            "font-family=\"{}\" font-weight=\"{}\" font-style=\"{}\"",
            escape(&self.family),
            self.weight,
            if self.italic { "italic" } else { "normal" }
        );
        if self.spacing != 0.0 {
            let _ = write!(out, " letter-spacing=\"{:.3}\"", self.spacing);
        }
        out
    }
}

/// A word (with its trailing whitespace) ready to be placed on a line.
#[derive(Clone, Debug)]
pub(crate) struct Piece {
    pub text: String,
    pub face: Face,
    pub mark: Mark,
    /// Advance including trailing whitespace.
    pub width: f64,
    /// Advance without trailing whitespace; what must fit on the line.
    pub fit: f64,
}

/// What a paragraph is set in.
pub(crate) struct Setting<'a> {
    pub family: &'a str,
    pub weight: u16,
    pub italic: bool,
    pub em: f64,
    pub line_height: f64,
    pub align: Align,
    pub spacing: f64,
    pub transform: Transform,
}

type AdvanceKey = (String, u16, bool, u64, u64, String);

pub(crate) struct Typesetter<'a> {
    pub theme: &'a Theme,
    mono: String,
    options: usvg::Options<'static>,
    advances: HashMap<AdvanceKey, f64>,
    sentinels: HashMap<u64, f64>,
    metrics: HashMap<(String, u16, u64), (f64, f64)>,
}

/// How a node's text is set, resolved against the theme.
pub(crate) struct TextStyle {
    pub font: ResolvedFont,
    /// Stack for the node's own family.
    pub stack: String,
    /// Stack for Markdown headings inside the node.
    pub heading: String,
}

impl<'a> Typesetter<'a> {
    pub fn new(theme: &'a Theme, assets: Option<&Path>) -> Self {
        Self {
            theme,
            mono: mono_stack(theme),
            options: usvg::Options {
                fontdb: deck_fonts(theme, assets),
                ..usvg::Options::default()
            },
            advances: HashMap::new(),
            sentinels: HashMap::new(),
            metrics: HashMap::new(),
        }
    }

    pub fn options(&self) -> &usvg::Options<'static> {
        &self.options
    }

    pub fn text_style(&self, font: ResolvedFont, code: bool) -> TextStyle {
        let stack = if code {
            code_stack(self.theme, &font.family)
        } else {
            font_stack(self.theme, &font.family)
        };
        let heading = if font.family == "body" {
            font_stack(self.theme, "heading")
        } else {
            stack.clone()
        };
        TextStyle {
            font,
            stack,
            heading,
        }
    }

    /// Bounding box of `<text>` content in a throwaway SVG. `usvg` lays text
    /// out with the loaded font database, including fallback and kerning.
    fn bounds(&self, body: &str, em: f64) -> Option<tiny_skia::Rect> {
        let source = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40000\" height=\"2000\"><text x=\"0\" y=\"1000\" font-size=\"{em}\" xml:space=\"preserve\">{body}</text></svg>"
        );
        usvg::Tree::from_str(&source, &self.options)
            .ok()
            .map(|tree| tree.root().abs_bounding_box())
    }

    /// Advance of `text`, trailing spaces included: a sentinel glyph after
    /// the text keeps them inside the measured box and is subtracted again.
    pub fn advance(&mut self, face: &Face, em: f64, text: &str) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let key = (
            face.family.clone(),
            face.weight,
            face.italic,
            em.to_bits(),
            face.spacing.to_bits(),
            text.to_string(),
        );
        if let Some(width) = self.advances.get(&key) {
            return *width;
        }
        let sentinel = if let Some(width) = self.sentinels.get(&em.to_bits()) {
            *width
        } else {
            let width = self
                .bounds("<tspan>|</tspan>", em)
                .map_or(0.0, |b| f64::from(b.width()));
            self.sentinels.insert(em.to_bits(), width);
            width
        };
        let body = format!(
            "<tspan {}>{}</tspan><tspan>|</tspan>",
            face.attributes(),
            escape(text)
        );
        let measured = self
            .bounds(&body, em)
            .map_or(0.0, |b| f64::from(b.width()) - sentinel);
        // No usable font at all (a bare container): estimate, so layout stays sane.
        let width = if measured <= 0.0 && !text.trim().is_empty() {
            text.chars().count() as f64 * (em * 0.55 + face.spacing)
        } else {
            measured.max(0.0)
        };
        self.advances.insert(key, width);
        width
    }

    /// Font ascent and descent in pixels, from the face the stack resolves to.
    pub fn ascent_descent(&mut self, face: &Face, em: f64) -> (f64, f64) {
        let key = (face.family.clone(), face.weight, em.to_bits());
        if let Some(found) = self.metrics.get(&key) {
            return *found;
        }
        let body = format!("<tspan {}>x</tspan>", face.attributes());
        let found = self.bounds(&body, em).map_or((em * 0.8, em * 0.2), |b| {
            (f64::from(1000.0 - b.top()), f64::from(b.bottom() - 1000.0))
        });
        self.metrics.insert(key, found);
        found
    }

    fn face(&self, setting: &Setting<'_>, run: &Run) -> Face {
        Face {
            family: if run.mark == Mark::Code {
                self.mono.clone()
            } else {
                setting.family.to_string()
            },
            weight: if run.mark == Mark::Bold {
                (setting.weight + 300).min(900)
            } else {
                setting.weight
            },
            italic: setting.italic || run.mark == Mark::Italic,
            spacing: setting.spacing,
        }
    }

    fn piece(&mut self, face: &Face, run: &Run, em: f64, text: &str) -> Piece {
        let width = self.advance(face, em, text);
        let trimmed = text.trim_end();
        let fit = if trimmed.len() == text.len() {
            width
        } else {
            self.advance(face, em, trimmed)
        };
        Piece {
            text: text.into(),
            face: face.clone(),
            mark: run.mark,
            width,
            fit,
        }
    }

    /// Greedy word wrap into `width`. A word wider than the line is broken
    /// between characters (`overflow-wrap: anywhere`); trailing whitespace
    /// hangs past the line end like it does in a browser.
    pub fn wrap(&mut self, runs: &[Run], setting: &Setting<'_>, width: f64) -> Vec<Vec<Piece>> {
        // A hair of slack, so a box sized to its text (rounded) never re-wraps it.
        let width = width + 0.05;
        let mut lines: Vec<Vec<Piece>> = vec![Vec::new()];
        let mut used = 0.0;
        for run in runs {
            let face = self.face(setting, run);
            let text = transformed(&run.text, setting.transform);
            for word in text.split_inclusive(char::is_whitespace) {
                let whole = self.piece(&face, run, setting.em, word);
                if used + whole.fit > width && used > 0.0 {
                    lines.push(Vec::new());
                    used = 0.0;
                }
                if whole.fit <= width {
                    used += whole.width;
                    lines.last_mut().expect("a line").push(whole);
                    continue;
                }
                for character in word.chars() {
                    let part = self.piece(&face, run, setting.em, &character.to_string());
                    if used + part.fit > width && used > 0.0 {
                        lines.push(Vec::new());
                        used = 0.0;
                    }
                    used += part.width;
                    lines.last_mut().expect("a line").push(part);
                }
            }
        }
        lines
    }

    /// Width of the longest unbreakable word in `runs`.
    fn longest_word(&mut self, runs: &[Run], setting: &Setting<'_>) -> f64 {
        let mut longest = 0.0_f64;
        for run in runs {
            let face = self.face(setting, run);
            let text = transformed(&run.text, setting.transform);
            for word in text.split_whitespace() {
                longest = longest.max(self.advance(&face, setting.em, word));
            }
        }
        longest
    }

    /// Set one paragraph at `(x, y)` in a box of `width`. Returns its height,
    /// where each line starts and the widest line.
    pub fn paragraph(
        &mut self,
        out: &mut String,
        runs: &[Run],
        at: (f64, f64, f64),
        setting: &Setting<'_>,
        fill: &str,
    ) -> (f64, Vec<f64>, f64) {
        let (x, y, width) = at;
        let lines = self.wrap(runs, setting, width.max(1.0));
        let primary = Face {
            family: setting.family.into(),
            weight: setting.weight,
            italic: false,
            spacing: 0.0,
        };
        let (ascent, descent) = self.ascent_descent(&primary, setting.em);
        let line_box = setting.em * setting.line_height;
        let mut starts = Vec::new();
        let mut widest = 0.0_f64;
        for (index, line) in lines.iter().enumerate() {
            let hang = line.last().map_or(0.0, |piece| piece.width - piece.fit);
            let total = line.iter().map(|piece| piece.width).sum::<f64>() - hang;
            widest = widest.max(total);
            let offset = match setting.align {
                Align::Left => 0.0,
                Align::Center => (width - total) / 2.0,
                Align::Right => width - total,
            };
            let top = y + index as f64 * line_box;
            let baseline = top + (line_box - (ascent + descent)) / 2.0 + ascent;
            let mut cursor = x + offset;
            starts.push(cursor);
            for (position, piece) in line.iter().enumerate() {
                let last = position + 1 == line.len();
                if piece.mark == Mark::Code {
                    let _ = write!(
                        out,
                        "<rect x=\"{cursor:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" rx=\"{:.2}\" fill=\"{CODE_TINT}\" fill-opacity=\".1\"/>",
                        baseline - ascent,
                        if last { piece.fit } else { piece.width },
                        ascent + descent,
                        setting.em * LAYOUT.inline_code_padding,
                    );
                }
                if !piece.text.trim().is_empty() {
                    let _ = write!(
                        out,
                        "<text x=\"{cursor:.2}\" y=\"{baseline:.2}\" font-size=\"{}\" fill=\"{fill}\" {}{} xml:space=\"preserve\">{}</text>",
                        setting.em,
                        piece.face.attributes(),
                        if piece.mark == Mark::Link {
                            " text-decoration=\"underline\""
                        } else {
                            ""
                        },
                        escape(piece.text.trim_end()),
                    );
                }
                cursor += piece.width;
            }
        }
        (lines.len() as f64 * line_box, starts, widest)
    }

    fn base_setting<'s>(style: &'s TextStyle, family: &'s str, weight: u16) -> Setting<'s> {
        Setting {
            family,
            weight,
            italic: style.font.italic,
            em: style.font.size,
            line_height: style.font.line_height,
            align: style.font.align,
            spacing: style.font.letter_spacing * style.font.size,
            transform: style.font.transform,
        }
    }

    /// A text node: the Markdown subset of `markdownTokens`. Returns the
    /// height and the widest line (list indent included).
    pub fn markdown(
        &mut self,
        out: &mut String,
        style: &TextStyle,
        source: &str,
        at: (f64, f64, f64),
        fill: &str,
    ) -> (f64, f64) {
        let (x, y0, width) = at;
        let mut y = y0;
        let mut widest = 0.0_f64;
        let em = style.font.size;
        let tight = style.font.line_height < LAYOUT.tight_line_height;
        let mut number = 0_u32;
        // The margin after the last heading or paragraph is not part of the box.
        let mut trailing = 0.0;
        for line in source.split('\n') {
            let row = classify(line);
            if !matches!(row, Row::Numbered(_)) {
                number = 0;
            }
            trailing = 0.0;
            match row {
                Row::Heading(level, text) => {
                    let size = em * LAYOUT.heading_scale[level - 1];
                    let setting = Setting {
                        family: &style.heading,
                        weight: LAYOUT.heading_weight,
                        em: size,
                        line_height: LAYOUT.heading_line_height,
                        spacing: style.font.letter_spacing * size,
                        ..Self::base_setting(style, &style.heading, LAYOUT.heading_weight)
                    };
                    let (height, _, w) =
                        self.paragraph(out, &inline(text), (x, y, width), &setting, fill);
                    widest = widest.max(w);
                    trailing = size * LAYOUT.heading_gap;
                    y += height + trailing;
                }
                Row::Bullet(text) | Row::Numbered(text) => {
                    let ordered = matches!(row, Row::Numbered(_));
                    number += u32::from(ordered);
                    let setting = Self::base_setting(style, &style.stack, style.font.weight);
                    let indent = em * LAYOUT.list_indent;
                    y += em * LAYOUT.list_item_padding;
                    let (height, starts, w) = self.paragraph(
                        out,
                        &inline(text),
                        (x + indent, y, width - indent),
                        &setting,
                        fill,
                    );
                    widest = widest.max(w + indent);
                    // The marker sits outside the text, right-aligned to it.
                    let marker = if ordered {
                        format!("{number}.")
                    } else {
                        "\u{2022}".into()
                    };
                    let face = Face {
                        family: style.stack.clone(),
                        weight: style.font.weight,
                        italic: false,
                        spacing: 0.0,
                    };
                    let (ascent, descent) = self.ascent_descent(&face, em);
                    let baseline =
                        y + (em * style.font.line_height - (ascent + descent)) / 2.0 + ascent;
                    let _ = write!(
                        out,
                        "<text x=\"{:.2}\" y=\"{baseline:.2}\" font-size=\"{em}\" fill=\"{fill}\" text-anchor=\"end\" {}>{marker}</text>",
                        starts.first().copied().unwrap_or(x + indent) - em * 0.4,
                        face.attributes(),
                    );
                    y += height + em * LAYOUT.list_item_padding;
                }
                Row::Paragraph(text) => {
                    let setting = Self::base_setting(style, &style.stack, style.font.weight);
                    let (height, _, w) =
                        self.paragraph(out, &inline(text), (x, y, width), &setting, fill);
                    widest = widest.max(w);
                    trailing = if tight {
                        0.0
                    } else {
                        em * LAYOUT.paragraph_gap
                    };
                    y += height + trailing;
                }
            }
        }
        (y - y0 - trailing, widest)
    }

    /// The longest whitespace-delimited word in `source` (one line per line).
    pub fn plain_min_width(&mut self, style: &TextStyle, source: &str) -> f64 {
        let setting = Self::base_setting(style, &style.stack, style.font.weight);
        let runs = [Run {
            text: source.into(),
            ..Run::default()
        }];
        self.longest_word(&runs, &setting)
    }

    /// Narrowest table: each column as wide as its longest word plus padding.
    pub fn table_min_width(&mut self, style: &TextStyle, rows: &[Vec<String>]) -> f64 {
        let em = style.font.size;
        let setting = Self::base_setting(style, &style.stack, style.font.weight);
        let padding = 2.0 * em * LAYOUT.cell_padding_x + 1.0;
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let mut widths = vec![padding; columns];
        for row in rows {
            for (column, cell) in row.iter().enumerate() {
                let runs = [Run {
                    text: cell.clone(),
                    ..Run::default()
                }];
                widths[column] = widths[column].max(self.longest_word(&runs, &setting) + padding);
            }
        }
        widths.iter().sum()
    }

    /// The longest unbreakable word of a text node, plus list indent.
    pub fn markdown_min_width(&mut self, style: &TextStyle, source: &str) -> f64 {
        let em = style.font.size;
        let mut longest = 0.0_f64;
        for line in source.split('\n') {
            let (text, indent, family, size) = match classify(line) {
                Row::Heading(level, text) => (
                    text,
                    0.0,
                    &style.heading,
                    em * LAYOUT.heading_scale[level - 1],
                ),
                Row::Bullet(text) | Row::Numbered(text) => {
                    (text, em * LAYOUT.list_indent, &style.stack, em)
                }
                Row::Paragraph(text) => (text, 0.0, &style.stack, em),
            };
            let setting = Setting {
                em: size,
                ..Self::base_setting(style, family, style.font.weight)
            };
            longest = longest.max(indent + self.longest_word(&inline(text), &setting));
        }
        longest
    }

    /// A code block: monospace, wrapped (`pre-wrap`), on a tinted box at least
    /// `min_height` tall. Returns the box height and the widest line.
    pub fn code(
        &mut self,
        out: &mut String,
        style: &TextStyle,
        source: &str,
        at: (f64, f64, f64),
        min_height: f64,
        fill: &str,
    ) -> (f64, f64) {
        let (x, y, width) = at;
        let em = style.font.size;
        let pad = em * LAYOUT.code_padding;
        let setting = Self::base_setting(style, &style.stack, style.font.weight);
        let mut text = String::new();
        let mut cursor = y + pad;
        let mut widest = 0.0_f64;
        for line in source.split('\n') {
            let runs = [Run {
                text: line.into(),
                ..Run::default()
            }];
            let (height, _, w) = self.paragraph(
                &mut text,
                &runs,
                (x + pad, cursor, width - 2.0 * pad),
                &setting,
                fill,
            );
            widest = widest.max(w);
            cursor += height;
        }
        let height = (cursor + pad - y).max(min_height);
        let _ = write!(
            out,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{width}\" height=\"{height:.2}\" rx=\"{:.2}\" fill=\"{CODE_TINT}\" fill-opacity=\".1\"/>",
            em * 0.2,
        );
        out.push_str(&text);
        (height, widest + 2.0 * pad)
    }

    /// Natural (max-content) width of each table column.
    fn table_columns(&mut self, style: &TextStyle, rows: &[Vec<String>]) -> Vec<f64> {
        let em = style.font.size;
        let face = Face {
            family: style.stack.clone(),
            weight: style.font.weight,
            italic: style.font.italic,
            spacing: style.font.letter_spacing * em,
        };
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let padding_x = em * LAYOUT.cell_padding_x;
        let mut natural = vec![padding_x * 2.0 + 1.0; columns];
        for row in rows {
            for (column, cell) in row.iter().enumerate() {
                let text = transformed(cell, style.font.transform);
                natural[column] =
                    natural[column].max(self.advance(&face, em, &text) + padding_x * 2.0);
            }
        }
        natural
    }

    /// The natural width of a whole table.
    pub fn table_width(&mut self, style: &TextStyle, rows: &[Vec<String>]) -> f64 {
        self.table_columns(style, rows).iter().sum()
    }

    /// A table with auto-sized columns (width shared in proportion to each
    /// column's longest cell, as browsers do) and thin borders. Returns its
    /// height.
    pub fn table(
        &mut self,
        out: &mut String,
        style: &TextStyle,
        rows: &[Vec<String>],
        at: (f64, f64, f64),
        fill: &str,
    ) -> f64 {
        let (x, y, width) = at;
        let em = style.font.size;
        let setting = Self::base_setting(style, &style.stack, style.font.weight);
        let padding_x = em * LAYOUT.cell_padding_x;
        let padding_y = em * LAYOUT.cell_padding_y;
        let natural = self.table_columns(style, rows);
        let total: f64 = natural.iter().sum();
        let widths: Vec<f64> = natural.iter().map(|w| w / total * width).collect();
        let mut top = y;
        for row in rows {
            let mut height = em * style.font.line_height + 2.0 * padding_y;
            let mut cells = String::new();
            let mut left = x;
            for (column, cell) in row.iter().enumerate() {
                let runs = [Run {
                    text: cell.clone(),
                    ..Run::default()
                }];
                let inner = (widths[column] - 2.0 * padding_x).max(1.0);
                let (cell_height, _, _) = self.paragraph(
                    &mut cells,
                    &runs,
                    (left + padding_x, top + padding_y, inner),
                    &setting,
                    fill,
                );
                height = height.max(cell_height + 2.0 * padding_y);
                left += widths[column];
            }
            let mut left = x;
            for width in widths.iter().take(row.len()) {
                let _ = write!(
                    out,
                    "<rect x=\"{left:.2}\" y=\"{top:.2}\" width=\"{width:.2}\" height=\"{height:.2}\" fill=\"none\" stroke=\"{fill}\" stroke-width=\"{}\"/>",
                    LAYOUT.table_border,
                );
                left += width;
            }
            out.push_str(&cells);
            top += height;
        }
        top - y
    }
}
