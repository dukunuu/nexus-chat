//! Text bodies to the deck's Markdown subset plus numeric typography.
//!
//! A deck text node has one font and a Markdown body: bold, italic, links,
//! bullets and numbered lines. A text body therefore becomes one font (taken
//! from its dominant run) and Markdown for the runs that differ in weight or
//! slant; other per-run differences are counted as approximations.

use std::fmt::Write as _;

use crate::deck::{Align, Font, VAlign};

use super::super::xml::El;
use super::color::Rgba;
use super::props::{
    BodyProps, Bullet, Line, ListStyle, ParaProps, RunProps, body_props, list_style, para_props,
    run_props,
};
use super::{Chain, Cx, State};

/// Which master text style a shape inherits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Title,
    Body,
    Other,
}

/// A natural single-spaced line is about this many times the font size.
const NATURAL_LINE: f64 = 1.2;
/// Longest text one node keeps (the deck limit is 8 KB).
const MAX_TEXT_BYTES: usize = 7_800;

pub(super) struct TextOut {
    pub markdown: String,
    /// `title` or `body`.
    pub style: &'static str,
    /// Only the fields that differ from the named style.
    pub font: Font,
    pub body: BodyProps,
}

struct Run {
    text: String,
    props: RunProps,
    /// A soft line break.
    brk: bool,
}

struct Para {
    level: usize,
    bullet: Bullet,
    align: Align,
    line: Option<Line>,
    runs: Vec<Run>,
}

impl Para {
    fn chars(&self) -> usize {
        self.runs.iter().map(|r| r.text.chars().count()).sum()
    }
}

/// The text of `a:t` children, or a field's cached text.
fn run_text(el: &El) -> String {
    el.kid("t").map(|t| t.text.clone()).unwrap_or_default()
}

fn paragraphs(cx: &Cx, body: &El, layers: &[ListStyle]) -> Vec<Para> {
    let mut out = Vec::new();
    for p in body.kids_named("p") {
        let ppr = p.kid("pPr");
        let level = ppr
            .and_then(|e| e.num("lvl"))
            .map_or(0, |v| v.clamp(0.0, 8.0) as usize);
        let mut props = ParaProps::default();
        for layer in layers {
            props.over(layer.level(level));
        }
        let explicit = ppr.map(|e| para_props(e, cx.theme)).unwrap_or_default();
        props.over(&explicit);
        let mut runs = Vec::new();
        for kid in &p.kids {
            let (text, brk) = match kid.name.as_str() {
                "r" | "fld" => (run_text(kid), false),
                "br" => (String::new(), true),
                _ => continue,
            };
            let mut run_props_all = props.run.clone();
            if let Some(r) = kid.kid("rPr") {
                run_props_all.over(&run_props(r, cx.theme));
            }
            runs.push(Run {
                text,
                props: run_props_all,
                brk,
            });
        }
        out.push(Para {
            level,
            bullet: props.bullet.unwrap_or(Bullet::None),
            align: props.align.unwrap_or(Align::Left),
            line: props.line,
            runs,
        });
    }
    out
}

/// Characters that would be read as Markdown if left alone.
fn neutral(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|c| {
            if matches!(c, '*' | '`') {
                '\u{2217}'
            } else {
                c
            }
        })
        .collect();
    // `[label](target)` would become a link.
    if out.contains("](") {
        out = out.replace("](", "]\u{200b}(");
    }
    out
}

/// Whether a line of plain text would be read as a heading or a list item.
fn looks_structural(line: &str) -> bool {
    let trimmed = line.trim_start();
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    (1..=3).contains(&hashes) && line[hashes..].starts_with(char::is_whitespace)
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || {
            let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && trimmed[digits..].starts_with(". ")
        }
}

/// A group of consecutive runs with the same marks, as Markdown.
fn marked(runs: &[(&str, bool, bool, bool)]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < runs.len() {
        let (_, bold, italic, link) = runs[i];
        let mut j = i;
        let mut text = String::new();
        while j < runs.len() && (runs[j].1, runs[j].2, runs[j].3) == (bold, italic, link) {
            text.push_str(runs[j].0);
            j += 1;
        }
        i = j;
        let core = text.trim();
        if core.is_empty() || !(bold || italic) {
            out.push_str(&text);
            continue;
        }
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        let mark = if bold { "**" } else { "*" };
        let _ = write!(out, "{lead}{mark}{core}{mark}{trail}");
    }
    out
}

impl Para {
    fn markdown(&self, cx: &Cx, st: &mut State, base: &RunProps) -> String {
        let bullet = self.bullet != Bullet::None;
        let mut pieces: Vec<(String, bool, bool, bool)> = Vec::new();
        let mut text_runs: Vec<String> = Vec::new();
        for run in &self.runs {
            if run.brk {
                text_runs.push(String::new());
                pieces.push((if bullet { " " } else { "\n" }.into(), false, false, false));
                continue;
            }
            if run.text.is_empty() {
                continue;
            }
            let bold = run.props.bold.unwrap_or(false) && !base.bold.unwrap_or(false);
            let italic = run.props.italic.unwrap_or(false) && !base.italic.unwrap_or(false);
            let mut text = neutral(&run.text);
            if run.props.caps.unwrap_or(false) != base.caps.unwrap_or(false)
                || run.props.size.map(f64::round) != base.size.map(f64::round)
                || run.props.color.map(Rgba::hex) != base.color.map(Rgba::hex)
            {
                st.warn.tally("Runs with their own size or color inside one text box take the box's main style");
            }
            let mut link = false;
            if let Some(url) = run
                .props
                .link
                .as_deref()
                .and_then(|id| cx.rels.get(id))
                .filter(|r| r.external)
                .map(|r| r.target.as_str())
                .filter(|u| {
                    (u.starts_with("https://") || u.starts_with("http://"))
                        && !u.contains(char::is_whitespace)
                        && !u.contains(['(', ')'])
                })
            {
                text = format!("[{}]({url})", text.trim());
                link = true;
            }
            pieces.push((text, bold && !link, italic && !link, link));
        }
        let views: Vec<(&str, bool, bool, bool)> = pieces
            .iter()
            .map(|(t, b, i, l)| (t.as_str(), *b, *i, *l))
            .collect();
        let mut body = marked(&views);
        if !bullet && looks_structural(&body) {
            body.insert(0, '\u{200b}');
        }
        let indent = "  ".repeat(self.level);
        match self.bullet {
            Bullet::None => body,
            Bullet::Char => format!("{indent}- {body}"),
            Bullet::Numbered => format!("{indent}1. {body}"),
        }
    }
}

/// Everything needed to turn a shape's text body into a text node.
#[allow(clippy::too_many_lines)]
pub(super) fn build(
    cx: &Cx,
    st: &mut State,
    body_el: &El,
    chain: &Chain,
    role: Role,
    font_color: Option<super::color::Rgba>,
) -> Option<TextOut> {
    let env = cx.env;
    let master_style = match role {
        Role::Title => &cx.master.title,
        Role::Body => &cx.master.body,
        Role::Other => &cx.master.other,
    };
    let mut layers: Vec<ListStyle> = vec![env.default_style.clone(), master_style.clone()];
    if let Some(color) = font_color {
        let mut style = ListStyle::default();
        for level in &mut style.0 {
            level.run.color = Some(color);
        }
        layers.push(style);
    }
    let theme = &cx.theme;
    let inherited = [chain.master, chain.layout];
    for sp in inherited.into_iter().flatten() {
        if let Some(ls) = sp.path(&["txBody", "lstStyle"]) {
            layers.push(list_style(ls, theme));
        }
    }
    if let Some(ls) = body_el.kid("lstStyle") {
        layers.push(list_style(ls, theme));
    }
    let paras = paragraphs(cx, body_el, &layers);
    // Unused trailing and leading empty paragraphs carry no text.
    let first = paras
        .iter()
        .position(|p| p.chars() > 0 || p.runs.iter().any(|r| r.brk))?;
    let last = paras
        .iter()
        .rposition(|p| p.chars() > 0 || p.runs.iter().any(|r| r.brk))?;
    let paras = &paras[first..=last];
    if paras
        .iter()
        .all(|p| p.runs.iter().all(|r| r.text.trim().is_empty()))
    {
        return None;
    }
    // The dominant run sets the node's font.
    let dominant = paras
        .iter()
        .flat_map(|p| p.runs.iter())
        .filter(|r| !r.brk)
        .max_by_key(|r| (r.text.trim().chars().count(), std::cmp::Reverse(0)))
        .map(|r| r.props.clone())
        .unwrap_or_default();
    let anchor_paragraph = paras.iter().find(|p| p.chars() > 0).unwrap_or(&paras[0]);
    if paras
        .iter()
        .filter(|p| p.chars() > 0)
        .any(|p| p.align != anchor_paragraph.align)
    {
        st.warn
            .tally("Text boxes with mixed alignment take one alignment");
    }
    let mut body = BodyProps::default();
    for sp in [
        Some(body_el),
        chain.layout.and_then(|l| l.kid("txBody")),
        chain.master.and_then(|m| m.kid("txBody")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(props) = sp.kid("bodyPr") {
            body = body.or(&body_props(props));
        }
    }
    if body.vertical == Some(true) {
        st.warn.tally("Vertical text is drawn horizontally");
    }
    let mut lines: Vec<String> = paras
        .iter()
        .map(|p| p.markdown(cx, st, &dominant))
        .collect();
    let numbered_zero = lines.is_empty();
    if numbered_zero {
        return None;
    }
    let mut markdown = lines.join("\n");
    if markdown.len() > MAX_TEXT_BYTES {
        st.warn.push(format!(
            "slide {}: a text box longer than 8 KB was cut short",
            cx.slide
        ));
        let mut end = MAX_TEXT_BYTES;
        while !markdown.is_char_boundary(end) {
            end -= 1;
        }
        markdown.truncate(end);
        lines.clear();
    }
    let scale = body.font_scale.unwrap_or(1.0);
    let size_pt = dominant.size.unwrap_or(18.0) * scale;
    let font_px = size_pt * 12_700.0 * env.scale;
    let mut line_factor = match anchor_paragraph.line {
        Some(Line::Pts(points)) => points / size_pt.max(1.0) / NATURAL_LINE,
        Some(Line::Pct(p)) => p,
        None => 1.0,
    };
    line_factor -= body.line_reduction.unwrap_or(0.0);
    let spacing_em = dominant
        .spacing
        .map_or(0.0, |s| s / 100.0 / size_pt.max(1.0));
    let style = if role == Role::Title { "title" } else { "body" };
    let mut font = Font {
        family: dominant.family.clone().or_else(|| cx.theme.minor.clone()),
        size: Some(font_px),
        weight: Some(if dominant.bold.unwrap_or(false) {
            700
        } else {
            400
        }),
        italic: Some(dominant.italic.unwrap_or(false)),
        line_height: Some((line_factor * NATURAL_LINE).clamp(0.5, 5.0)),
        letter_spacing: Some(spacing_em.clamp(-0.5, 2.0)),
        color: Some(
            env.paint(
                dominant
                    .color
                    .or_else(|| cx.theme.scheme("tx1"))
                    .unwrap_or(super::color::Rgba::rgb(0, 0, 0)),
            ),
        ),
        align: Some(anchor_paragraph.align),
        valign: Some(body.anchor.unwrap_or(VAlign::Top)),
        transform: Some(if dominant.caps.unwrap_or(false) {
            crate::deck::Transform::Uppercase
        } else {
            crate::deck::Transform::None
        }),
    };
    font.family = font.family.map(|name| env.family_role(&name));
    let font = env.delta(style, &font);
    Some(TextOut {
        markdown,
        style,
        font,
        body,
    })
}

/// Plain text that is safe to put on one Markdown line.
pub(super) fn plain_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    neutral(&flat)
}
