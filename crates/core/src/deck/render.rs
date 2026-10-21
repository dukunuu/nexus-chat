//! Deck → SVG → PNG, so the model (and the terminal) can see slides.
//!
//! The studio webview draws slides with HTML and CSS; this module draws the
//! same slide with SVG and rasterizes it with `resvg`, measuring text with the
//! very font database `resvg` renders with. Both renderers follow the numbers
//! in [`LAYOUT`]; a studio test checks that the CSS and JS still use them.
//!
//! Deliberate differences from the HTML renderer:
//! - font fallbacks: `system-ui` and `ui-serif` are CSS-only, so the SVG
//!   stacks name the platform's equivalents instead;
//! - borders on shapes and tables are 2 and 1 CSS pixels in HTML (they stay
//!   thin as the slide scales) and 2 and 1 slide units here (they scale);
//! - the inline-code tint is the light-mode tint, because the HTML tint
//!   follows the viewer's light or dark mode, not the deck's theme;
//! - text is placed with real font ascent and descent, so a baseline can sit
//!   a pixel off a browser's rounding of the same metrics.
//!
//! Remote images are only drawn when `remote_image::Fetcher` already holds
//! them; nothing here touches the network.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, Result, ensure};
use base64::Engine as _;
use resvg::{tiny_skia, usvg};

use super::{
    Align, Block, Chart, Content, Deck, Fit, HEIGHT, Shape, Slide, TextSize, Theme, WIDTH,
};

/// Numbers shared by the SVG renderer here and the HTML renderer in
/// `crates/studio/ui` (`studio.css`, `render.js`). Lengths in `em` are
/// relative to the block's font size.
pub struct LayoutSpec {
    /// Slide width in SVG user units; the slide is 16:9.
    pub canvas_width: f64,
    pub canvas_height: f64,
    /// Width of one grid unit in canvas units.
    pub unit: f64,
    /// Font sizes `s`, `m`, `l`, `xl`, `xxl` as a percentage of canvas width.
    pub size_percent: [f64; 5],
    pub line_height: f64,
    /// Line height of `xl` and `xxl` text, which is tighter.
    pub large_line_height: f64,
    pub paragraph_gap: f64,
    /// Heading levels 1 to 3 as a multiple of the block's font size.
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
    pub shape_border: f64,
    pub table_border: f64,
    pub chart_width: f64,
    pub chart_height: f64,
}

/// The shared layout spec.
pub const LAYOUT: LayoutSpec = LayoutSpec {
    canvas_width: 1600.0,
    canvas_height: 900.0,
    unit: 50.0,
    size_percent: [1.35, 2.2, 3.0, 4.2, 6.2],
    line_height: 1.35,
    large_line_height: 1.08,
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
    shape_border: 2.0,
    table_border: 1.0,
    chart_width: 320.0,
    chart_height: 180.0,
};

const _: () = assert!(WIDTH == 32 && HEIGHT == 18);
const CODE_TINT: &str = "#8d8875";
const SHEET_THUMB_WIDTH: f64 = 520.0;
const SHEET_GAP: f64 = 16.0;
const SHEET_LABEL: f64 = 30.0;
const CACHE_WIDTH: u32 = 1200;

// -- fonts -----------------------------------------------------------------

fn fonts() -> Arc<usvg::fontdb::Database> {
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
        // The generic families the SVG stacks end with. Without this, a
        // machine whose default `sans-serif` face is missing draws nothing.
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

fn options() -> usvg::Options<'static> {
    usvg::Options {
        fontdb: fonts(),
        ..usvg::Options::default()
    }
}

/// XML-escape text and attribute values.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A theme role, a hex literal, or the foreground color (as the HTML does).
fn color(theme: &Theme, value: &str) -> String {
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

/// Font stack for a theme font role. Mirrors `fontStack` in `render.js`, with
/// the platform fonts that `system-ui` and `ui-serif` would resolve to.
fn font_stack(theme: &Theme, role: &str) -> String {
    let name: String = theme
        .fonts
        .get(role)
        .map_or("sans-serif", String::as_str)
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '\\' | ';'))
        .collect();
    let lower = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    if has(&["mono", "code", "consolas", "menlo"]) {
        format!("'{name}', SFMono-Regular, Menlo, monospace")
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
        "'New York', Georgia, serif".into()
    } else {
        format!("'{name}', 'Helvetica Neue', 'Segoe UI', sans-serif")
    }
}

/// The font for code, whatever the theme's `mono` role names.
fn mono_stack(theme: &Theme) -> String {
    let stack = font_stack(theme, "mono");
    if stack.ends_with("monospace") {
        stack
    } else {
        "Menlo, SFMono-Regular, monospace".into()
    }
}

fn font_px(size: TextSize) -> f64 {
    let index = match size {
        TextSize::S => 0,
        TextSize::M => 1,
        TextSize::L => 2,
        TextSize::Xl => 3,
        TextSize::Xxl => 4,
    };
    LAYOUT.size_percent[index] / 100.0 * LAYOUT.canvas_width
}

// -- inline Markdown ---------------------------------------------------------

/// How a run of inline text is marked up; the Markdown subset does not nest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mark {
    #[default]
    Plain,
    Bold,
    Italic,
    Code,
    Link,
}

/// A styled stretch of text inside one paragraph.
#[derive(Clone, Debug, Default, PartialEq)]
struct Run {
    text: String,
    mark: Mark,
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

fn inline(text: &str) -> Vec<Run> {
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
enum Row<'a> {
    Heading(usize, &'a str),
    Bullet(&'a str),
    Numbered(&'a str),
    Paragraph(&'a str),
}

fn classify(line: &str) -> Row<'_> {
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

// -- text layout -------------------------------------------------------------

/// A font choice for one piece of text.
#[derive(Clone, Debug, PartialEq)]
struct Face {
    family: String,
    weight: u16,
    italic: bool,
}

impl Face {
    fn attributes(&self) -> String {
        format!(
            "font-family=\"{}\" font-weight=\"{}\" font-style=\"{}\"",
            escape(&self.family),
            self.weight,
            if self.italic { "italic" } else { "normal" }
        )
    }
}

/// A word (with its trailing whitespace) ready to be placed on a line.
#[derive(Clone, Debug)]
struct Piece {
    text: String,
    face: Face,
    mark: Mark,
    /// Advance including trailing whitespace.
    width: f64,
    /// Advance without trailing whitespace; what must fit on the line.
    fit: f64,
}

/// What a paragraph is set in.
struct Setting<'a> {
    family: &'a str,
    weight: u16,
    em: f64,
    line_height: f64,
    align: Align,
}

type AdvanceKey = (String, u16, bool, u64, String);

struct Typesetter<'a> {
    theme: &'a Theme,
    mono: String,
    advances: HashMap<AdvanceKey, f64>,
    sentinels: HashMap<u64, f64>,
    metrics: HashMap<(String, u16, u64), (f64, f64)>,
}

impl<'a> Typesetter<'a> {
    fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            mono: mono_stack(theme),
            advances: HashMap::new(),
            sentinels: HashMap::new(),
            metrics: HashMap::new(),
        }
    }

    /// Bounding box of `<text>` content in a throwaway SVG. `usvg` lays text
    /// out with the loaded font database, including fallback and kerning.
    fn bounds(body: &str, em: f64) -> Option<tiny_skia::Rect> {
        let source = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40000\" height=\"2000\"><text x=\"0\" y=\"1000\" font-size=\"{em}\" xml:space=\"preserve\">{body}</text></svg>"
        );
        usvg::Tree::from_str(&source, &options())
            .ok()
            .map(|tree| tree.root().abs_bounding_box())
    }

    /// Advance of `text`, trailing spaces included: a sentinel glyph after
    /// the text keeps them inside the measured box and is subtracted again.
    fn advance(&mut self, face: &Face, em: f64, text: &str) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let key = (
            face.family.clone(),
            face.weight,
            face.italic,
            em.to_bits(),
            text.to_string(),
        );
        if let Some(width) = self.advances.get(&key) {
            return *width;
        }
        let sentinel = *self.sentinels.entry(em.to_bits()).or_insert_with(|| {
            Self::bounds("<tspan>|</tspan>", em).map_or(0.0, |b| f64::from(b.width()))
        });
        let body = format!(
            "<tspan {}>{}</tspan><tspan>|</tspan>",
            face.attributes(),
            escape(text)
        );
        let measured = Self::bounds(&body, em).map_or(0.0, |b| f64::from(b.width()) - sentinel);
        // No usable font at all (a bare container): estimate, so layout stays sane.
        let width = if measured <= 0.0 && !text.trim().is_empty() {
            text.chars().count() as f64 * em * 0.55
        } else {
            measured.max(0.0)
        };
        self.advances.insert(key, width);
        width
    }

    /// Font ascent and descent in pixels, from the face the stack resolves to.
    fn ascent_descent(&mut self, face: &Face, em: f64) -> (f64, f64) {
        let key = (face.family.clone(), face.weight, em.to_bits());
        if let Some(found) = self.metrics.get(&key) {
            return *found;
        }
        let body = format!("<tspan {}>x</tspan>", face.attributes());
        let found = Self::bounds(&body, em).map_or((em * 0.8, em * 0.2), |b| {
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
                700
            } else {
                setting.weight
            },
            italic: run.mark == Mark::Italic,
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
    fn wrap(&mut self, runs: &[Run], setting: &Setting<'_>, width: f64) -> Vec<Vec<Piece>> {
        let mut lines: Vec<Vec<Piece>> = vec![Vec::new()];
        let mut used = 0.0;
        for run in runs {
            let face = self.face(setting, run);
            for word in run.text.split_inclusive(char::is_whitespace) {
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

    /// Set one paragraph at `(x, y)` in a box of `width`. Returns its height
    /// and where each line starts.
    fn paragraph(
        &mut self,
        out: &mut String,
        runs: &[Run],
        at: (f64, f64, f64),
        setting: &Setting<'_>,
    ) -> (f64, Vec<f64>) {
        let (x, y, width) = at;
        let lines = self.wrap(runs, setting, width.max(1.0));
        let primary = Face {
            family: setting.family.into(),
            weight: setting.weight,
            italic: false,
        };
        let (ascent, descent) = self.ascent_descent(&primary, setting.em);
        let line_box = setting.em * setting.line_height;
        let mut starts = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let hang = line.last().map_or(0.0, |piece| piece.width - piece.fit);
            let total = line.iter().map(|piece| piece.width).sum::<f64>() - hang;
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
                        "<text x=\"{cursor:.2}\" y=\"{baseline:.2}\" font-size=\"{}\" {}{} xml:space=\"preserve\">{}</text>",
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
        (lines.len() as f64 * line_box, starts)
    }

    /// A text block: the Markdown subset of `markdownTokens`.
    fn markdown(&mut self, out: &mut String, block: &Block, source: &str, at: (f64, f64, f64)) {
        let (x, mut y, width) = at;
        let em = font_px(block.style.size);
        let large = matches!(block.style.size, TextSize::Xl | TextSize::Xxl);
        let body_height = if large {
            LAYOUT.large_line_height
        } else {
            LAYOUT.line_height
        };
        let body = font_stack(self.theme, "body");
        let heading = font_stack(self.theme, "heading");
        let mut number = 0_u32;
        for line in source.split('\n') {
            let row = classify(line);
            if !matches!(row, Row::Numbered(_)) {
                number = 0;
            }
            match row {
                Row::Heading(level, text) => {
                    let size = em * LAYOUT.heading_scale[level - 1];
                    let setting = Setting {
                        family: &heading,
                        weight: LAYOUT.heading_weight,
                        em: size,
                        line_height: LAYOUT.heading_line_height,
                        align: block.style.align,
                    };
                    let (height, _) = self.paragraph(out, &inline(text), (x, y, width), &setting);
                    y += height + size * LAYOUT.heading_gap;
                }
                Row::Bullet(text) | Row::Numbered(text) => {
                    let ordered = matches!(row, Row::Numbered(_));
                    number += u32::from(ordered);
                    let setting = Setting {
                        family: &body,
                        weight: 400,
                        em,
                        line_height: body_height,
                        align: block.style.align,
                    };
                    let indent = em * LAYOUT.list_indent;
                    y += em * LAYOUT.list_item_padding;
                    let (height, starts) = self.paragraph(
                        out,
                        &inline(text),
                        (x + indent, y, width - indent),
                        &setting,
                    );
                    // The marker sits outside the text, right-aligned to it.
                    let marker = if ordered {
                        format!("{number}.")
                    } else {
                        "\u{2022}".into()
                    };
                    let face = Face {
                        family: body.clone(),
                        weight: 400,
                        italic: false,
                    };
                    let (ascent, descent) = self.ascent_descent(&face, em);
                    let baseline = y + (em * body_height - (ascent + descent)) / 2.0 + ascent;
                    let _ = write!(
                        out,
                        "<text x=\"{:.2}\" y=\"{baseline:.2}\" font-size=\"{em}\" text-anchor=\"end\" {}>{marker}</text>",
                        starts.first().copied().unwrap_or(x + indent) - em * 0.4,
                        face.attributes(),
                    );
                    y += height + em * LAYOUT.list_item_padding;
                }
                Row::Paragraph(text) => {
                    let setting = Setting {
                        family: &body,
                        weight: 400,
                        em,
                        line_height: body_height,
                        align: block.style.align,
                    };
                    let (height, _) = self.paragraph(out, &inline(text), (x, y, width), &setting);
                    y += height
                        + if large {
                            0.0
                        } else {
                            em * LAYOUT.paragraph_gap
                        };
                }
            }
        }
    }

    /// A code block: monospace, wrapped (`pre-wrap`), on a tinted box that
    /// hugs its content like the HTML `<pre>`.
    fn code(&mut self, out: &mut String, block: &Block, source: &str, at: (f64, f64, f64)) {
        let (x, y, width) = at;
        let em = font_px(block.style.size);
        let pad = em * LAYOUT.code_padding;
        let family = self.mono.clone();
        let setting = Setting {
            family: &family,
            weight: 400,
            em,
            line_height: LAYOUT.line_height,
            align: block.style.align,
        };
        let mut text = String::new();
        let mut cursor = y + pad;
        for line in source.split('\n') {
            let runs = [Run {
                text: line.into(),
                ..Run::default()
            }];
            let (height, _) = self.paragraph(
                &mut text,
                &runs,
                (x + pad, cursor, width - 2.0 * pad),
                &setting,
            );
            cursor += height;
        }
        let _ = write!(
            out,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{width}\" height=\"{:.2}\" rx=\"{:.2}\" fill=\"{CODE_TINT}\" fill-opacity=\".1\"/>",
            cursor + pad - y,
            em * 0.2,
        );
        out.push_str(&text);
    }

    /// A table with auto-sized columns (width shared in proportion to each
    /// column's longest cell, as browsers do) and thin borders.
    fn table(
        &mut self,
        out: &mut String,
        block: &Block,
        rows: &[Vec<String>],
        at: (f64, f64, f64),
        stroke: &str,
    ) {
        let (x, y, width) = at;
        let em = font_px(block.style.size);
        let family = font_stack(self.theme, "body");
        let setting = Setting {
            family: &family,
            weight: 400,
            em,
            line_height: LAYOUT.line_height,
            align: block.style.align,
        };
        let face = Face {
            family: family.clone(),
            weight: 400,
            italic: false,
        };
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let padding_x = em * LAYOUT.cell_padding_x;
        let padding_y = em * LAYOUT.cell_padding_y;
        let mut natural = vec![padding_x * 2.0 + 1.0; columns];
        for row in rows {
            for (column, cell) in row.iter().enumerate() {
                natural[column] =
                    natural[column].max(self.advance(&face, em, cell) + padding_x * 2.0);
            }
        }
        let total: f64 = natural.iter().sum();
        let widths: Vec<f64> = natural.iter().map(|w| w / total * width).collect();
        let mut top = y;
        for row in rows {
            let mut height = em * LAYOUT.line_height + 2.0 * padding_y;
            let mut cells = String::new();
            let mut left = x;
            for (column, cell) in row.iter().enumerate() {
                let runs = [Run {
                    text: cell.clone(),
                    ..Run::default()
                }];
                let inner = (widths[column] - 2.0 * padding_x).max(1.0);
                let (cell_height, _) = self.paragraph(
                    &mut cells,
                    &runs,
                    (left + padding_x, top + padding_y, inner),
                    &setting,
                );
                height = height.max(cell_height + 2.0 * padding_y);
                left += widths[column];
            }
            let mut left = x;
            for width in widths.iter().take(row.len()) {
                let _ = write!(
                    out,
                    "<rect x=\"{left:.2}\" y=\"{top:.2}\" width=\"{width:.2}\" height=\"{height:.2}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"{}\"/>",
                    LAYOUT.table_border,
                );
                left += width;
            }
            out.push_str(&cells);
            top += height;
        }
    }
}

// -- non-text blocks -----------------------------------------------------------

fn chart(out: &mut String, block: &Block, theme: &Theme) {
    let Content::Chart {
        chart,
        labels,
        series,
    } = &block.content
    else {
        return;
    };
    let palette = [
        color(theme, "accent"),
        color(theme, "fg"),
        color(theme, "muted"),
    ];
    let max = series
        .iter()
        .flat_map(|s| &s.values)
        .fold(1.0_f64, |m, v| m.max(v.abs()));
    let count = labels.len().max(1) as f64;
    if *chart == Chart::Pie {
        let data = series.first().map_or(&[][..], |s| s.values.as_slice());
        let total = data.iter().map(|v| v.max(0.0)).sum::<f64>().max(1.0);
        let mut angle = -std::f64::consts::FRAC_PI_2;
        for (index, value) in data.iter().enumerate() {
            let value = value.max(0.0);
            let end = angle + value / total * std::f64::consts::TAU;
            let fill = &palette[index % palette.len()];
            if value / total >= 0.999_999 {
                let _ = write!(
                    out,
                    "<circle cx=\"160\" cy=\"80\" r=\"65\" fill=\"{fill}\"/>"
                );
            } else if value > 0.0 {
                let _ = write!(
                    out,
                    "<path d=\"M160 80 L{} {} A65 65 0 {} 1 {} {} Z\" fill=\"{fill}\"/>",
                    160.0 + 65.0 * angle.cos(),
                    80.0 + 65.0 * angle.sin(),
                    u8::from(end - angle > std::f64::consts::PI),
                    160.0 + 65.0 * end.cos(),
                    80.0 + 65.0 * end.sin(),
                );
            }
            angle = end;
        }
    } else {
        let per_series = series.len().max(1) as f64;
        for (s, one) in series.iter().enumerate() {
            let fill = &palette[s % palette.len()];
            let mut points = String::new();
            for (index, value) in one.values.iter().enumerate() {
                let x = 10.0 + (index as f64 + 0.5) * 300.0 / count;
                let y = 140.0 - value / max * 120.0;
                let _ = write!(points, "{x},{y} ");
                if *chart == Chart::Bar {
                    let _ = write!(
                        out,
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\"/>",
                        10.0 + index as f64 * 300.0 / count + s as f64 * 260.0 / count / per_series,
                        y.min(140.0),
                        250.0 / count / per_series,
                        (140.0 - y).abs(),
                    );
                }
            }
            if *chart == Chart::Line {
                let _ = write!(
                    out,
                    "<polyline points=\"{points}\" fill=\"none\" stroke=\"{fill}\" stroke-width=\"3\"/>"
                );
            }
        }
    }
    let fg = color(theme, "fg");
    let family = escape(&font_stack(theme, "body"));
    for (index, label) in labels.iter().enumerate() {
        let _ = write!(
            out,
            "<text x=\"{}\" y=\"170\" text-anchor=\"middle\" font-size=\"10\" font-family=\"{family}\" fill=\"{fg}\">{}</text>",
            10.0 + (index as f64 + 0.5) * 300.0 / count,
            escape(label),
        );
    }
}

/// The deck image as a PNG data URL, from the deck's own `assets` folder or
/// the remote-image cache. Never fetches.
fn image_data(src: &str, assets: Option<&Path>) -> Option<String> {
    let root = assets?;
    let path = if src.starts_with("https://") {
        crate::remote_image::Fetcher::new(root).cached(src)?
    } else {
        let path = root
            .join(src.strip_prefix("assets/").unwrap_or(src))
            .canonicalize()
            .ok()?;
        path.starts_with(root.canonicalize().ok()?)
            .then_some(path)?
    };
    let image = image::load_from_memory(&std::fs::read(path).ok()?).ok()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(png.into_inner()))
}

fn shape(out: &mut String, kind: Shape, rect: (f64, f64, f64, f64), fill: &str, stroke: &str) {
    let (x, y, w, h) = rect;
    let border = LAYOUT.shape_border;
    match kind {
        Shape::Rect => {
            let _ = write!(
                out,
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"{border}\"/>",
                x + border / 2.0,
                y + border / 2.0,
                w - border,
                h - border,
            );
        }
        Shape::Ellipse => {
            let _ = write!(
                out,
                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" fill=\"{fill}\" stroke=\"{stroke}\" stroke-width=\"{border}\"/>",
                x + w / 2.0,
                y + h / 2.0,
                (w - border) / 2.0,
                (h - border) / 2.0,
            );
        }
        Shape::Line | Shape::Arrow => {
            let path = if kind == Shape::Arrow {
                "M2 50 H97 M77 25 L97 50 L77 75"
            } else {
                "M2 50 H98"
            };
            let _ = write!(
                out,
                "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 100 100\" preserveAspectRatio=\"none\" overflow=\"visible\"><path d=\"{path}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"2\"/></svg>"
            );
        }
    }
}

fn slide_body(
    deck: &Deck,
    slide: &Slide,
    assets: Option<&Path>,
    text: &mut Typesetter<'_>,
) -> String {
    let theme = &deck.theme;
    let mut out = format!(
        "<rect width=\"{}\" height=\"{}\" fill=\"{}\"/>",
        LAYOUT.canvas_width,
        LAYOUT.canvas_height,
        color(theme, "bg")
    );
    for block in &slide.blocks {
        let rect = (
            f64::from(block.x) * LAYOUT.unit,
            f64::from(block.y) * LAYOUT.unit,
            f64::from(block.w) * LAYOUT.unit,
            f64::from(block.h) * LAYOUT.unit,
        );
        let (x, y, w, h) = rect;
        let fg = color(theme, block.style.color.as_deref().unwrap_or("fg"));
        let _ = write!(out, "<g fill=\"{fg}\">");
        match &block.content {
            Content::Text { text: source } => text.markdown(&mut out, block, source, (x, y, w)),
            Content::Code { source, .. } => text.code(&mut out, block, source, (x, y, w)),
            Content::Table { rows } => text.table(&mut out, block, rows, (x, y, w), &fg),
            Content::Shape {
                shape: kind,
                fill,
                stroke,
            } => shape(
                &mut out,
                *kind,
                rect,
                &color(theme, fill),
                &color(theme, stroke),
            ),
            Content::Chart { .. } => {
                let _ = write!(
                    out,
                    "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {} {}\" preserveAspectRatio=\"none\">",
                    LAYOUT.chart_width, LAYOUT.chart_height
                );
                chart(&mut out, block, theme);
                out.push_str("</svg>");
            }
            Content::Image { src, fit } => {
                if let Some(data) = image_data(src, assets) {
                    let _ = write!(
                        out,
                        "<svg x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\"><image width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"xMidYMid {}\" href=\"data:image/png;base64,{data}\"/></svg>",
                        if *fit == Fit::Cover { "slice" } else { "meet" },
                    );
                } else {
                    let _ = write!(
                        out,
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"#dddcd5\"/><text x=\"{}\" y=\"{}\" font-size=\"24\" fill=\"#6f6d66\">Image not available</text>",
                        x + 12.0,
                        y + 32.0,
                    );
                }
            }
        }
        out.push_str("</g>");
    }
    out
}

fn raster(body: &str, width: u32, height: u32, view: (f64, f64)) -> Result<Vec<u8>> {
    let source = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {} {}\">{body}</svg>",
        view.0, view.1
    );
    let tree = usvg::Tree::from_str(&source, &options())?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("invalid render size")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap.encode_png()?)
}

/// Render a slide to a PNG `width_px` wide. Local images are skipped; use
/// [`render_slide_at`] to draw a deck's own assets.
pub fn render_slide(deck: &Deck, slide_id: &str, width_px: u32) -> Result<Vec<u8>> {
    render_slide_at(deck, slide_id, width_px, None)
}

/// Render using the deck's already-local `assets` folder. Never downloads.
pub fn render_slide_at(
    deck: &Deck,
    slide_id: &str,
    width_px: u32,
    assets: Option<&Path>,
) -> Result<Vec<u8>> {
    ensure!(
        (32..=3200).contains(&width_px),
        "render width must be 32 to 3200"
    );
    let slide = deck
        .slides
        .iter()
        .find(|s| s.id == slide_id)
        .context("unknown slide")?;
    let body = slide_body(deck, slide, assets, &mut Typesetter::new(&deck.theme));
    let height = (f64::from(width_px) * LAYOUT.canvas_height / LAYOUT.canvas_width).round() as u32;
    raster(
        &body,
        width_px,
        height,
        (LAYOUT.canvas_width, LAYOUT.canvas_height),
    )
}

/// A grid of numbered thumbnails, in the requested order.
pub fn render_deck_sheet(deck: &Deck, slides: &[String], cols: usize) -> Result<Vec<u8>> {
    sheet_at(deck, slides, cols, None)
}

fn sheet_at(deck: &Deck, slides: &[String], cols: usize, assets: Option<&Path>) -> Result<Vec<u8>> {
    ensure!(
        !slides.is_empty() && slides.len() <= 200 && (1..=12).contains(&cols),
        "a contact sheet needs 1 to 200 slides and 1 to 12 columns"
    );
    let cols = cols.min(slides.len());
    let rows = slides.len().div_ceil(cols);
    let thumb_height = SHEET_THUMB_WIDTH * LAYOUT.canvas_height / LAYOUT.canvas_width;
    let cell_w = SHEET_THUMB_WIDTH + SHEET_GAP;
    let cell_h = thumb_height + SHEET_LABEL + SHEET_GAP;
    let mut text = Typesetter::new(&deck.theme);
    let mut body = String::from("<rect width=\"100%\" height=\"100%\" fill=\"#eeede7\"/>");
    for (index, id) in slides.iter().enumerate() {
        let (number, slide) = deck
            .slides
            .iter()
            .enumerate()
            .find(|(_, s)| s.id == *id)
            .with_context(|| format!("unknown slide {id}"))?;
        let x = (index % cols) as f64 * cell_w + SHEET_GAP;
        let y = (index / cols) as f64 * cell_h + SHEET_GAP;
        let _ = write!(
            body,
            "<svg x=\"{x}\" y=\"{y}\" width=\"{SHEET_THUMB_WIDTH}\" height=\"{thumb_height}\" viewBox=\"0 0 {} {}\">{}</svg>\
             <rect x=\"{x}\" y=\"{y}\" width=\"{SHEET_THUMB_WIDTH}\" height=\"{thumb_height}\" fill=\"none\" stroke=\"#b9b7ac\"/>\
             <text x=\"{x}\" y=\"{}\" fill=\"#282923\" font-size=\"18\" font-family=\"'Helvetica Neue', 'Segoe UI', sans-serif\">Slide {}</text>",
            LAYOUT.canvas_width,
            LAYOUT.canvas_height,
            slide_body(deck, slide, assets, &mut text),
            y + thumb_height + 22.0,
            number + 1,
        );
    }
    let width = (cols as f64 * cell_w + SHEET_GAP).ceil();
    let height = (rows as f64 * cell_h + SHEET_GAP).ceil();
    raster(&body, width as u32, height as u32, (width, height))
}

// -- render cache ----------------------------------------------------------------

/// Hex-encode a slide id or sheet key so deck data is never a path component.
fn file_key(key: &str) -> String {
    key.bytes().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// The cache folder for one deck, `<files>/decks/.renders/<stem>`, created on
/// demand. Refuses a path that passes through a symlink.
fn render_dir(deck_path: &Path, files: &Path) -> Result<PathBuf> {
    let name = deck_path
        .file_name()
        .context("deck filename")?
        .to_string_lossy();
    let stem = name.strip_suffix(".deck.json").unwrap_or(&name);
    let directory = files.join("decks/.renders").join(stem);
    let mut ancestor = directory.as_path();
    while ancestor.starts_with(files) && ancestor != files {
        ensure!(
            !ancestor.is_symlink(),
            "render cache must not contain symlinks"
        );
        ancestor = ancestor.parent().context("render cache root")?;
    }
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn relative(files: &Path, output: &Path) -> Result<String> {
    Ok(output
        .strip_prefix(files)?
        .to_string_lossy()
        .replace('\\', "/"))
}

/// Write a render to `<files>/decks/.renders/<deck-stem>/<slide-id>-<revision8>.png`
/// (a contact sheet when `sheet`, covering `ids`) and return its path relative
/// to `files`, ready for a `![alt](path)` reference. Older revisions of the
/// same slide are removed. The folder is a device-local cache.
pub fn render_cached(
    deck: &Deck,
    deck_path: &Path,
    files: &Path,
    ids: &[String],
    sheet: bool,
) -> Result<String> {
    ensure!(!ids.is_empty(), "no slides to render");
    let directory = render_dir(deck_path, files)?;
    let key = file_key(&if sheet {
        format!("sheet-{}", &super::hash(&ids)[..8])
    } else {
        ids[0].clone()
    });
    let name = format!("{key}-{}.png", &deck.revision()[..8]);
    let output = directory.join(&name);
    if !output.is_file() {
        let assets = deck_path.parent().context("deck folder")?.join("assets");
        let bytes = if sheet {
            sheet_at(deck, ids, 3, Some(&assets))?
        } else {
            render_slide_at(deck, &ids[0], CACHE_WIDTH, Some(&assets))?
        };
        super::write_atomic(&output, &bytes)?;
    }
    for entry in std::fs::read_dir(&directory)?.flatten() {
        let old = entry.file_name().to_string_lossy().into_owned();
        if old.starts_with(&format!("{key}-"))
            && Path::new(&old).extension().is_some_and(|ext| ext == "png")
            && old != name
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    relative(files, &output)
}

/// A durable render of one slide for a conversation message. Unlike
/// [`render_cached`] it is never pruned, because a past message keeps
/// referring to it.
pub fn render_snapshot(
    deck: &Deck,
    deck_path: &Path,
    files: &Path,
    slide_id: &str,
) -> Result<String> {
    let directory = render_dir(deck_path, files)?;
    let name = format!("snap-{}-{}.png", file_key(slide_id), &deck.revision()[..8]);
    let output = directory.join(&name);
    if !output.is_file() {
        let assets = deck_path.parent().context("deck folder")?.join("assets");
        let bytes = render_slide_at(deck, slide_id, CACHE_WIDTH, Some(&assets))?;
        super::write_atomic(&output, &bytes)?;
    }
    relative(files, &output)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::deck::Op;

    fn rgba(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).expect("png").to_rgba8()
    }

    fn has_fonts() -> bool {
        !fonts().is_empty()
    }

    fn op(value: serde_json::Value) -> Op {
        serde_json::from_value(value).expect("op")
    }

    #[test]
    fn fixture_renders_at_size_with_ink_in_every_block() {
        let deck = Deck::classic_fixture("Renderer fixture");
        for slide in &deck.slides {
            let image = rgba(&render_slide(&deck, &slide.id, 1600).unwrap());
            assert_eq!(image.dimensions(), (1600, 900));
            let background = *image.get_pixel(1599, 899);
            for block in &slide.blocks {
                let (x0, y0) = (block.x as u32 * 50, block.y as u32 * 50);
                let (x1, y1) = (
                    (block.x + block.w) as u32 * 50,
                    (block.y + block.h) as u32 * 50,
                );
                let inked =
                    (y0..y1).any(|y| (x0..x1).any(|x| *image.get_pixel(x, y) != background));
                assert!(inked, "block {} is blank", block.id);
            }
        }
        let half = rgba(&render_slide(&deck, "s1", 800).unwrap());
        assert_eq!(half.dimensions(), (800, 450));
        assert!(render_slide(&deck, "nope", 800).is_err());
        assert!(render_slide(&deck, "s1", 10).is_err());
    }

    #[test]
    fn every_block_kind_draws() {
        let mut deck = Deck::classic_fixture("Kinds");
        let blocks = json!([
            {"id": "t", "kind": "table", "x": 2, "y": 1, "w": 12, "h": 4, "rows": [["Name", "Value"], ["alpha", "1"]], "style": {"size": "s"}},
            {"id": "c", "kind": "code", "x": 16, "y": 1, "w": 12, "h": 4, "language": "rs", "source": "fn main() {}\nlet x = 1;", "style": {"size": "s"}},
            {"id": "p", "kind": "chart", "x": 2, "y": 8, "w": 8, "h": 6, "chart": "pie", "labels": ["a", "b"], "series": [{"name": "s", "values": [1, 3]}], "style": {}},
            {"id": "l", "kind": "chart", "x": 12, "y": 8, "w": 8, "h": 6, "chart": "line", "labels": ["a", "b"], "series": [{"name": "s", "values": [1, 3]}], "style": {}},
            {"id": "e", "kind": "shape", "x": 22, "y": 8, "w": 4, "h": 4, "shape": "ellipse", "fill": "accent", "stroke": "muted", "style": {}},
            {"id": "a", "kind": "shape", "x": 22, "y": 14, "w": 6, "h": 2, "shape": "arrow", "fill": "accent", "stroke": "accent", "style": {}},
            {"id": "i", "kind": "image", "x": 28, "y": 8, "w": 3, "h": 3, "src": "assets/missing.png", "fit": "cover", "style": {}}
        ]);
        let ops: Vec<Op> = blocks
            .as_array()
            .unwrap()
            .iter()
            .map(|block| op(json!({"op": "insert_block", "slide": "s2", "block": block})))
            .collect();
        deck = crate::deck::apply(&deck, &ops).unwrap();
        let image = rgba(&render_slide(&deck, "s2", 1600).unwrap());
        let background = *image.get_pixel(1599, 899);
        for block in &deck.slides[1].blocks {
            let (x0, y0) = (block.x as u32 * 50, block.y as u32 * 50);
            let (x1, y1) = (
                ((block.x + block.w) as u32 * 50).min(1600),
                ((block.y + block.h) as u32 * 50).min(900),
            );
            let inked = (y0..y1).any(|y| (x0..x1).any(|x| *image.get_pixel(x, y) != background));
            assert!(inked, "block {} is blank", block.id);
        }
    }

    #[test]
    fn narrower_boxes_wrap_into_more_lines_using_real_glyph_widths() {
        if !has_fonts() {
            return;
        }
        let theme = Theme::default();
        let mut text = Typesetter::new(&theme);
        let family = font_stack(&theme, "body");
        let setting = Setting {
            family: &family,
            weight: 400,
            em: 35.2,
            line_height: 1.35,
            align: Align::Left,
        };
        let runs = inline("A sentence with **bold words** and a longer ending that has to wrap.");
        let wide = text.wrap(&runs, &setting, 1400.0).len();
        let narrow = text.wrap(&runs, &setting, 400.0).len();
        let narrower = text.wrap(&runs, &setting, 200.0).len();
        assert_eq!(wide, 1);
        assert!(
            narrow > wide && narrower > narrow,
            "{wide} {narrow} {narrower}"
        );
        let face = Face {
            family: family.clone(),
            weight: 400,
            italic: false,
        };
        assert!(text.advance(&face, 35.2, "W") > text.advance(&face, 35.2, "i"));
        assert!(text.advance(&face, 35.2, " ") > 0.0);
        assert!(text.advance(&face, 35.2, "ab ") > text.advance(&face, 35.2, "ab"));
        // A single unbreakable word is split between characters, not lost.
        let long = [Run {
            text: "w".repeat(200),
            ..Run::default()
        }];
        assert!(text.wrap(&long, &setting, 300.0).len() > 2);
    }

    #[test]
    fn overflow_stays_visible_below_the_block_box() {
        if !has_fonts() {
            return;
        }
        let deck = Deck::classic_fixture("Overflow");
        let long = "overflowing words ".repeat(60);
        let update = op(json!({
            "op": "update_block", "slide": "s2", "id": "b3",
            "fields": {"text": long}, "base_hash": deck.slides[1].blocks[1].hash()
        }));
        let deck = crate::deck::apply(&deck, &[update]).unwrap();
        let image = rgba(&render_slide(&deck, "s2", 1600).unwrap());
        let background = *image.get_pixel(1599, 899);
        let block = &deck.slides[1].blocks[1];
        let bottom = (block.y + block.h) as u32 * 50;
        let below = (bottom..900).any(|y| (0..1600).any(|x| *image.get_pixel(x, y) != background));
        assert!(below, "text beyond the box must still be drawn");
    }

    #[test]
    fn markdown_runs_and_lines_match_the_html_subset() {
        let runs = inline("a **b** *c* `d` [e](https://x.y) *f");
        let kinds: Vec<_> = runs.iter().map(|r| (r.text.as_str(), r.mark)).collect();
        assert_eq!(
            kinds,
            [
                ("a ", Mark::Plain),
                ("b", Mark::Bold),
                (" ", Mark::Plain),
                ("c", Mark::Italic),
                (" ", Mark::Plain),
                ("d", Mark::Code),
                (" ", Mark::Plain),
                ("e", Mark::Link),
                (" *f", Mark::Plain),
            ]
        );
        assert_eq!(classify("## Hi"), Row::Heading(2, "Hi"));
        assert_eq!(classify("####   x"), Row::Paragraph("####   x"));
        assert_eq!(classify("  - item"), Row::Bullet("item"));
        assert_eq!(classify("* item"), Row::Bullet("item"));
        assert_eq!(classify("**bold** x"), Row::Paragraph("**bold** x"));
        assert_eq!(classify("12. item"), Row::Numbered("item"));
        assert_eq!(classify("12.item"), Row::Paragraph("12.item"));
    }

    #[test]
    fn sheets_show_every_requested_slide_and_validate_input() {
        let deck = Deck::classic_fixture("Sheet");
        let ids: Vec<String> = deck.slides.iter().map(|s| s.id.clone()).collect();
        let one = rgba(&render_deck_sheet(&deck, &ids, 1).unwrap());
        let two = rgba(&render_deck_sheet(&deck, &ids, 2).unwrap());
        assert!(two.width() > one.width() && two.height() < one.height());
        assert!(render_deck_sheet(&deck, &[], 2).is_err());
        assert!(render_deck_sheet(&deck, &ids, 0).is_err());
        assert!(render_deck_sheet(&deck, &["missing".into()], 2).is_err());
    }

    #[test]
    fn cache_names_by_revision_prunes_old_ones_and_keeps_snapshots() {
        let root = std::env::temp_dir().join(format!("nexus-render-{}", uuid::Uuid::new_v4()));
        let files = root.join("files");
        let path = files.join("decks/a.deck.json");
        let mut deck = Deck::classic_fixture("Cache");
        let first = render_cached(&deck, &path, &files, &["s1".into()], false).unwrap();
        assert!(
            first.starts_with("decks/.renders/a/") && first.ends_with(".png"),
            "{first}"
        );
        assert!(files.join(&first).is_file());
        let snapshot = render_snapshot(&deck, &path, &files, "s1").unwrap();
        assert!(snapshot.contains("snap-"));
        deck.title = "Changed".into();
        let second = render_cached(&deck, &path, &files, &["s1".into()], false).unwrap();
        assert_ne!(first, second);
        assert!(!files.join(&first).exists(), "old revision is pruned");
        assert!(files.join(&second).is_file());
        assert!(files.join(&snapshot).is_file(), "snapshots are durable");
        let sheet = render_cached(&deck, &path, &files, &["s1".into(), "s2".into()], true).unwrap();
        assert!(files.join(&sheet).is_file() && files.join(&second).is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// `NEXUS_RENDER_OUT=<dir> cargo test -p nexus-chat-core write_fixture_renders`
    /// writes the title slide and a content slide with a chart, to compare with
    /// the HTML renderer. Does nothing unless the variable is set.
    #[test]
    fn write_fixture_renders() {
        let Some(out) = std::env::var_os("NEXUS_RENDER_OUT") else {
            return;
        };
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let deck = Deck::classic_fixture("A better story");
        let narrow = op(json!({
            "op": "update_block", "slide": "s2", "id": "b3",
            "fields": {"w": 14}, "base_hash": deck.slides[1].blocks[1].hash()
        }));
        let chart = op(json!({"op": "insert_block", "slide": "s2", "block": {
            "id": "chart", "kind": "chart", "x": 18, "y": 6, "w": 11, "h": 8, "chart": "bar",
            "labels": ["Start", "Build", "Grow"],
            "series": [{"name": "Illustrative", "values": [3, 5, 8]}], "style": {}
        }}));
        let deck = crate::deck::apply(&deck, &[narrow, chart]).unwrap();
        let title = render_slide(&deck, "s1", 1600).unwrap();
        let content = render_slide(&deck, "s2", 1600).unwrap();
        std::fs::write(out.join("core-classic-title.png"), title).unwrap();
        std::fs::write(out.join("core-classic-content-chart.png"), content).unwrap();
    }
}
