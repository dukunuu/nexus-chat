//! The `deck` tool: confined, atomic deck actions. Studio and the model share
//! the same `*.deck.json` and `*.deck.json.comments.json` files, so every
//! write here shows up in an open studio window within half a second.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};

use crate::deck::feedback::{self, Anchor, Edit, Message, Role, Severity, Thread};
use crate::deck::suggest::{self, RawSuggestion, Suggestion};
use crate::deck::{CheckError, Deck, Dim, Font, Kind, Mode, Node, Op, Position, render};

use super::ToolBox;

/// Slides shown as individual images; more become one contact sheet.
const MAX_SEPARATE_IMAGES: usize = 4;
/// Slides on one contact sheet.
const MAX_SHEET_SLIDES: usize = 12;
const MAX_COMMENT_CHARS: usize = 500;
const MAX_THREADS: usize = 200;
/// How much of a block's text `read` returns.
const READ_TEXT_CHARS: usize = 400;
const SHORT_REVISION: usize = 12;

fn slug(title: &str) -> String {
    let words = title
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("-");
    if words.is_empty() {
        "untitled".into()
    } else {
        words.chars().take(80).collect()
    }
}

/// Resolve `input` (relative to the space files folder) to a deck file under
/// `<files>/decks/`, rejecting escapes, odd characters and symlinks. Also used
/// by the terminal when it opens a linked canvas.
pub(crate) fn confined(files: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "path must stay inside the space files, under decks/"
    );
    let name = relative.file_name().map(|n| n.to_string_lossy());
    ensure!(
        relative.starts_with("decks") && name.is_some_and(|n| n.ends_with(".deck.json")),
        "path must look like decks/name.deck.json"
    );
    ensure!(
        input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')),
        "use only letters, digits, - _ . and / in deck paths"
    );
    ensure!(
        !input.split('/').any(|part| part.starts_with('.')),
        "deck paths cannot contain hidden folders"
    );
    std::fs::create_dir_all(files)?;
    let mut path = files.canonicalize()?;
    for part in relative.components() {
        path.push(part);
        ensure!(
            !path.is_symlink(),
            "path escapes the space files through a symlink"
        );
    }
    Ok(path)
}

fn located(errors: &[CheckError]) -> String {
    errors
        .iter()
        .map(|e| format!("{}: {}", e.path, e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn load(path: &Path) -> Result<Deck> {
    let bytes = std::fs::read(path).with_context(|| {
        format!(
            "no deck at {}; create it first",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })?;
    ensure!(
        bytes.len() <= crate::deck::MAX_SOURCE,
        "deck file is too large"
    );
    let source = String::from_utf8(bytes).context("deck file is not UTF-8")?;
    Deck::check(&source)
        .map_err(|errors| anyhow::anyhow!("deck file is invalid: {}", located(&errors)))
}

fn save(path: &Path, deck: &Deck) -> Result<()> {
    crate::deck::write_atomic(path, &serde_json::to_vec_pretty(deck)?)
}

/// Fill hashes and insertion ids, check every operation against `deck`, and
/// return the prepared edit; no write happens here.
fn prepare(deck: &Deck, ops: &Value) -> Result<Suggestion> {
    let ops = ops.as_array().context("ops must be an array")?.clone();
    ensure!(!ops.is_empty(), "ops must contain at least one operation");
    let (mut valid, errors) = suggest::prepare(
        deck,
        vec![RawSuggestion {
            why: "Suggested edit".into(),
            ops,
        }],
    );
    valid.pop().ok_or_else(|| {
        anyhow::anyhow!(
            "{}. Nothing was written; read the deck and correct that operation.",
            located(&errors)
        )
    })
}

/// Slides whose content differs, in deck order. A theme change touches all.
fn changed_slides(before: &Deck, after: &Deck) -> Vec<String> {
    after
        .slides
        .iter()
        .filter(|slide| {
            before.theme != after.theme
                || before.slides.iter().find(|old| old.id == slide.id) != Some(*slide)
        })
        .map(|slide| slide.id.clone())
        .collect()
}

fn slide_number(deck: &Deck, id: &str) -> Result<usize> {
    deck.slides
        .iter()
        .position(|slide| slide.id == id)
        .map(|index| index + 1)
        .with_context(|| {
            let known = deck
                .slides
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown slide {id:?}; slides are: {known}")
        })
}

/// `1 slide`, `3 slides`.
fn slides(count: usize) -> String {
    format!("{count} slide{}", if count == 1 { "" } else { "s" })
}

fn short(revision: &str) -> &str {
    &revision[..SHORT_REVISION]
}

fn clip(text: &str, chars: usize) -> String {
    let flat = text.replace('\n', " / ");
    if flat.chars().count() > chars {
        format!("{}…", flat.chars().take(chars).collect::<String>())
    } else {
        flat
    }
}

/// A lowercase enum name (`SpaceBetween` becomes `spacebetween`).
fn name(value: &impl std::fmt::Debug) -> String {
    format!("{value:?}").to_lowercase()
}

/// One line per operation, for the edit result.
fn describe(op: &Op, before: &Deck) -> String {
    let number =
        |id: &str| slide_number(before, id).map_or_else(|_| id.to_string(), |n| n.to_string());
    let on = |id: &str| {
        before
            .slide_of_node(id)
            .map_or(String::new(), |s| format!(" on slide {}", number(&s.id)))
    };
    match op {
        Op::AddSlide { slide, .. } => {
            format!("add_slide {} ({} nodes)", slide.id, slide.nodes().len())
        }
        Op::DeleteSlide { id } => format!("delete_slide {id} (was slide {})", number(id)),
        Op::MoveSlide { id, to } => format!("move_slide {id} to position {}", to + 1),
        Op::SetNotes { slide, .. } => format!("set_notes slide {}", number(slide)),
        Op::UpdateSlide { id, fields } => format!(
            "update_slide {} : {}",
            number(id),
            fields.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
        Op::InsertNode { parent, node, .. } => {
            format!("insert_node {} {} into {parent}", node.kind.name(), node.id)
        }
        Op::UpdateNode { id, fields, .. } => format!(
            "update_node {id}{}: {}",
            on(id),
            fields.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
        Op::DeleteNode { id } => format!("delete_node {id}{}", on(id)),
        Op::MoveNode { id, parent, .. } => format!("move_node {id} into {parent}"),
        Op::Wrap { ids, layout, .. } => {
            format!("wrap {} in a {} frame", ids.join(", "), name(&layout.mode))
        }
        Op::Unwrap { id } => format!("unwrap {id}"),
        Op::SetLayout { target, layout } => {
            format!("set_layout {target} to {}", name(&layout.mode))
        }
        Op::SetTextStyle { name, .. } => format!("set_text_style {name}"),
        Op::SetTheme { .. } => "set_theme".into(),
    }
}

fn px(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

fn dim(d: Dim) -> String {
    match d {
        Dim::Fixed(n) => px(n),
        Dim::Hug => "hug".into(),
        Dim::Fill => "fill".into(),
    }
}

/// Only the fields a font overrides: `size=58 color=accent`.
fn font_line(font: &Font) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &font.family {
        parts.push(format!("family={v}"));
    }
    if let Some(v) = font.size {
        parts.push(format!("size={}", px(v)));
    }
    if let Some(v) = font.weight {
        parts.push(format!("weight={v}"));
    }
    if font.italic == Some(true) {
        parts.push("italic".into());
    }
    if let Some(v) = font.line_height {
        parts.push(format!("line_height={v}"));
    }
    if let Some(v) = font.letter_spacing {
        parts.push(format!("letter_spacing={v}"));
    }
    if let Some(v) = &font.color {
        parts.push(format!("color={v}"));
    }
    if let Some(v) = font.align {
        parts.push(format!("align={}", name(&v)));
    }
    if let Some(v) = font.valign {
        parts.push(format!("valign={}", name(&v)));
    }
    if let Some(v) = font.transform {
        parts.push(format!("transform={}", name(&v)));
    }
    parts.join(" ")
}

fn layout_line(layout: &crate::deck::AutoLayout) -> String {
    let mut out = name(&layout.mode);
    if layout.mode != Mode::None {
        if layout.wrap {
            out.push_str(" wrap");
        }
        let _ = write!(out, " gap {}", px(layout.gap));
        if let Some(cross) = layout.gap_cross {
            let _ = write!(out, "/{}", px(cross));
        }
        let _ = write!(
            out,
            " main {} cross {}",
            name(&layout.main),
            name(&layout.cross)
        );
    }
    if layout.padding.iter().any(|p| *p != 0.0) {
        let [t, r, b, l] = layout.padding;
        let _ = write!(out, " padding {},{},{},{}", px(t), px(r), px(b), px(l));
    }
    out
}

// One arm per node kind.
#[allow(clippy::too_many_lines)]
fn node_line(
    node: &Node,
    rect: Option<crate::deck::Rect>,
    depth: usize,
    parent_flows: bool,
) -> String {
    let at = rect.map_or(String::new(), |r| {
        format!(" [{},{} {}x{}]", px(r.x), px(r.y), px(r.w), px(r.h))
    });
    let mut sizing = String::new();
    if node.sizing.w != Dim::Hug || node.sizing.h != Dim::Hug {
        let _ = write!(
            sizing,
            " size {}x{}",
            dim(node.sizing.w),
            dim(node.sizing.h)
        );
    }
    for (key, value) in [
        ("min_w", node.sizing.min_w),
        ("max_w", node.sizing.max_w),
        ("min_h", node.sizing.min_h),
        ("max_h", node.sizing.max_h),
    ] {
        if let Some(v) = value {
            let _ = write!(sizing, " {key} {}", px(v));
        }
    }
    let position = match node.position {
        Position::Absolute { x, y } if parent_flows => format!(" absolute({},{})", px(x), px(y)),
        _ => String::new(),
    };
    let mut extra = String::new();
    if node.opacity < 1.0 {
        let _ = write!(extra, " opacity {}", node.opacity);
    }
    if let Some(r) = node.rotation {
        let _ = write!(extra, " rotation {r}");
    }
    if node.hidden {
        extra.push_str(" hidden");
    }
    if node.locked {
        extra.push_str(" locked");
    }
    let label = node
        .name
        .as_ref()
        .map_or(String::new(), |n| format!(" {n:?}"));
    let detail = match &node.kind {
        Kind::Frame {
            layout,
            fill,
            stroke,
            radius,
            clip,
            ..
        } => {
            let mut d = format!("layout {}", layout_line(layout));
            if let Some(f) = fill {
                let _ = write!(d, " fill {f}");
            }
            if let Some(s) = stroke {
                let _ = write!(d, " stroke {} {}", s.color, px(s.width));
            }
            if *radius > 0.0 {
                let _ = write!(d, " radius {}", px(*radius));
            }
            if *clip {
                d.push_str(" clip");
            }
            d
        }
        Kind::Text { text, style, font } => {
            format!(
                "style {style} {} : {}",
                font_line(font),
                clip(text, READ_TEXT_CHARS)
            )
        }
        Kind::Code {
            language,
            source,
            style,
            font,
        } => format!(
            "{language} style {style} {} : {}",
            font_line(font),
            clip(source, READ_TEXT_CHARS)
        ),
        Kind::Table { rows, style, font } => {
            let cells = rows
                .iter()
                .map(|row| row.join(" | "))
                .collect::<Vec<_>>()
                .join(" ; ");
            format!(
                "{}x{} style {style} {} : {}",
                rows.len(),
                rows.iter().map(Vec::len).max().unwrap_or(0),
                font_line(font),
                clip(&cells, READ_TEXT_CHARS)
            )
        }
        Kind::Image { src, fit } => format!("{src} {}", name(fit)),
        Kind::Shape {
            shape,
            fill,
            stroke,
            radius,
        } => {
            let mut d = name(shape);
            if let Some(f) = fill {
                let _ = write!(d, " fill {f}");
            }
            if let Some(s) = stroke {
                let _ = write!(d, " stroke {} {}", s.color, px(s.width));
            }
            if *radius > 0.0 {
                let _ = write!(d, " radius {}", px(*radius));
            }
            d
        }
        Kind::Chart {
            chart,
            labels,
            series,
        } => {
            let series = series
                .iter()
                .map(|s| {
                    let values = s.values.iter().map(f64::to_string).collect::<Vec<_>>();
                    format!("{}: {}", s.name, values.join(" "))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "{} labels=[{}] series=[{series}]",
                name(chart),
                labels.join(", ")
            )
        }
    };
    format!(
        "{}{} {}{label}{at}{sizing}{position}{extra} {detail}",
        "  ".repeat(depth + 1),
        node.id,
        node.kind.name(),
    )
}

fn write_tree(
    out: &mut String,
    nodes: &[Node],
    rects: &std::collections::HashMap<String, crate::deck::Rect>,
    depth: usize,
    parent_flows: bool,
) {
    for node in nodes {
        let _ = writeln!(
            out,
            "{}",
            node_line(node, rects.get(&node.id).copied(), depth, parent_flows)
        );
        if let Kind::Frame {
            layout, children, ..
        } = &node.kind
        {
            write_tree(out, children, rects, depth + 1, layout.mode != Mode::None);
        }
    }
}

/// The compact field reference, sent with the first `read` or `create` of a
/// session so the tool description can stay short.
const SCHEMA: &str = "\nschema (sent once per session)\n\
- container (slide, frame): layout{mode:none|row|column, wrap, gap, gap_cross, padding:[t,r,b,l], main:start|center|end|space_between, cross:start|center|end|stretch}. mode none = children are absolute. Slides also have fill (color) and notes.\n\
- node: id name? sizing{w,h: px|\"hug\"|\"fill\", min_w,max_w,min_h,max_h} position:\"auto\"|{x,y} (absolute = ignore auto-layout; every child of a mode:none parent is absolute) opacity rotation hidden locked. hug = fit content, fill = share the leftover main-axis space (or stretch across).\n\
- frame: fill stroke{color,width} radius clip children[]. A hug frame needs mode row|column.\n\
- text: text (Markdown subset: # headings, - lists, **bold**, *italic*, `code`), style (title|heading|subheading|body|caption|code or a name you add), font{family,size px,weight 100-900,italic,line_height x,letter_spacing em,color,align left|center|right,valign top|middle|bottom,transform none|uppercase|lowercase|capitalize}. Text hugs its width unless sized; give it w fill (or a fixed w) so it wraps, h hug so it grows.\n\
- shape: shape rect|ellipse|line|arrow, fill, stroke{color,width}, radius. image: src, fit contain|cover. table: rows[[..]], style, font. code: language, source, style, font. chart: chart bar|line|pie, labels[], series[{name,values[]}].\n\
- colors: theme roles (bg fg accent muted ...) or hex. font.family: a role (heading body mono), a deck font or an installed family (see fonts).\n\
- layouts that work: column slide, padding 120-160, gap 32-56, cross stretch; text w fill; two columns = a row frame (gap 64-96, cross stretch) with w fill children; centered title = main center.\n\
- set_layout converts children like Figma (none to row/column makes them auto; back makes them absolute where they are). wrap{ids,layout} puts siblings in a new frame; unwrap{id} dissolves it. update_node merges: {\"font\":{\"size\":64}} keeps other font fields, null removes one. set_text_style{name,font} changes every node using the style.\n";

fn schema_once(shown: &mut bool) -> &'static str {
    if std::mem::replace(shown, true) {
        ""
    } else {
        SCHEMA
    }
}

fn author(role: &Role) -> &'static str {
    if matches!(role, Role::Model) {
        "nexus"
    } else {
        "user"
    }
}

fn read_text(relative: &str, path: &Path, deck: &Deck) -> Result<String> {
    let mut out = format!(
        "{relative} · {:?} · {} · {}x{} px · revision {}\n",
        deck.title,
        slides(deck.slides.len()),
        px(deck.theme.size.w),
        px(deck.theme.size.h),
        short(&deck.revision())
    );
    let tokens = |map: &std::collections::BTreeMap<String, String>| {
        map.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let _ = writeln!(
        out,
        "theme colors {} · fonts {}",
        tokens(&deck.theme.colors),
        tokens(&deck.theme.fonts)
    );
    for style in deck.theme.text_styles.keys() {
        let font = deck.theme.resolve_font(style, &Font::default());
        let _ = writeln!(
            out,
            "text style {style}: {} {}px/{} lh {} ls {} {} {}",
            deck.theme.family(&font.family),
            px(font.size),
            font.weight,
            font.line_height,
            font.letter_spacing,
            font.color,
            name(&font.align)
        );
    }
    for file in &deck.theme.font_files {
        let _ = writeln!(
            out,
            "deck font {}: {} weight {}{}{}",
            file.family,
            file.file,
            file.weight,
            if file.italic { " italic" } else { "" },
            if file.fallback.is_empty() {
                String::new()
            } else {
                format!(" fallback {}", file.fallback.join(", "))
            }
        );
    }
    let assets = path.parent().map(|dir| dir.join("assets"));
    for (index, slide) in deck.slides.iter().enumerate() {
        let notes = if slide.notes.is_empty() {
            String::new()
        } else {
            format!(" · notes: {}", clip(&slide.notes, 160))
        };
        let label = slide
            .name
            .as_ref()
            .map_or(String::new(), |n| format!(" {n:?}"));
        let fill = slide
            .fill
            .as_ref()
            .map_or(String::new(), |f| format!(" fill {f}"));
        let _ = writeln!(
            out,
            "slide {} (id {}){label}{fill} layout {}{notes}",
            index + 1,
            slide.id,
            layout_line(&slide.layout)
        );
        let rects = crate::deck::layout_map(deck, slide, assets.as_deref());
        write_tree(
            &mut out,
            &slide.children,
            &rects,
            0,
            slide.layout.mode != Mode::None,
        );
    }
    thread_lines(&mut out, deck, &feedback::load(path)?);
    Ok(out)
}

fn thread_lines(out: &mut String, deck: &Deck, threads: &[Thread]) {
    let open = threads.iter().filter(|t| t.resolved.is_none()).count();
    let _ = writeln!(out, "comment threads: {} ({open} open)", threads.len());
    for thread in threads {
        let slide =
            slide_number(deck, &thread.anchor.slide).map_or_else(|_| "?".into(), |n| n.to_string());
        let block = thread
            .anchor
            .block
            .as_ref()
            .map_or(String::new(), |b| format!(" node {b}"));
        let state = if thread.resolved.is_some() {
            "resolved"
        } else {
            "open"
        };
        let first = thread.messages.first();
        let _ = writeln!(
            out,
            "  {} · slide {slide}{block} · {} · {state} · {} messages · {}: {}",
            thread.id,
            name(&thread.severity),
            thread.messages.len(),
            first.map_or("-", |m| author(&m.role)),
            first.map_or(String::new(), |m| clip(&m.text, 160)),
        );
        if thread.messages.len() > 1
            && let Some(last) = thread.messages.last()
        {
            let _ = writeln!(
                out,
                "    latest ({}): {}",
                author(&last.role),
                clip(&last.text, 160)
            );
        }
    }
}

/// The `fonts` action: the deck's own families, then installed ones.
fn fonts_text(deck: &Deck, query: Option<&str>) -> String {
    let mut out = String::new();
    let roles = deck
        .theme
        .fonts
        .iter()
        .map(|(role, family)| format!("{role}={family}"))
        .collect::<Vec<_>>()
        .join(" ");
    let _ = writeln!(out, "deck font roles: {roles}");
    if deck.theme.font_files.is_empty() {
        out.push_str("deck font files: none imported\n");
    }
    for file in &deck.theme.font_files {
        let _ = writeln!(
            out,
            "deck font: {} ({}, weight {}{})",
            file.family,
            file.file,
            file.weight,
            if crate::deck::text::loadable_font(&file.file) {
                ""
            } else {
                ", woff2: shown in studio only, renders with the fallback here"
            }
        );
    }
    let query = query
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let installed = crate::deck::text::installed_families();
    let matching: Vec<&String> = installed
        .iter()
        .filter(|name| {
            query
                .as_ref()
                .is_none_or(|q| name.to_lowercase().contains(q))
        })
        .collect();
    let shown: Vec<&str> = matching.iter().take(30).map(|s| s.as_str()).collect();
    let _ = writeln!(
        out,
        "installed families{}: {} of {} shown{}",
        query
            .as_ref()
            .map_or(String::new(), |q| format!(" matching {q:?}")),
        shown.len(),
        matching.len(),
        if matching.len() > shown.len() {
            " (narrow with query)"
        } else {
            ""
        }
    );
    for family in shown {
        let _ = writeln!(out, "- {family}");
    }
    out
}

/// A new deck: the title slide only, or the given slides.
fn new_deck(title: &str, slides: Option<&Value>) -> Result<Deck> {
    if let Some(slides) = slides {
        return Deck::check(&json!({"version": 2, "title": title, "slides": slides}).to_string())
            .map_err(|errors| anyhow::anyhow!("slides are invalid: {}", located(&errors)));
    }
    Ok(Deck::titled(title))
}

/// Image references the model sees: individual slides, or one contact sheet.
struct Shown {
    ids: Vec<String>,
    sheet: bool,
}

impl Shown {
    fn some(deck: &Deck, ids: Vec<String>) -> Self {
        if ids.is_empty() {
            Self::overview(deck)
        } else if ids.len() > MAX_SEPARATE_IMAGES {
            Self {
                ids: ids.into_iter().take(MAX_SHEET_SLIDES).collect(),
                sheet: true,
            }
        } else {
            Self { ids, sheet: false }
        }
    }

    /// The whole deck: a lone slide on its own, otherwise a contact sheet.
    fn overview(deck: &Deck) -> Self {
        Self {
            ids: deck
                .slides
                .iter()
                .take(MAX_SHEET_SLIDES)
                .map(|s| s.id.clone())
                .collect(),
            sheet: deck.slides.len() > 1,
        }
    }

    fn lines(&self, deck: &Deck, path: &Path, files: &Path) -> Result<String> {
        let mut out = String::new();
        if self.ids.is_empty() {
            return Ok(out);
        }
        if self.sheet {
            let file = render::render_cached(deck, path, files, &self.ids, true)?;
            let _ = writeln!(out, "![{} slides]({file})", self.ids.len());
        } else {
            for id in &self.ids {
                let file =
                    render::render_cached(deck, path, files, std::slice::from_ref(id), false)?;
                let _ = writeln!(out, "![slide {}]({file})", slide_number(deck, id)?);
            }
        }
        Ok(out)
    }

    /// `slide 3` for one slide, `4 slides` otherwise.
    fn label(&self, deck: &Deck) -> Result<String> {
        Ok(match self.ids.as_slice() {
            [id] if !self.sheet => format!("slide {}", slide_number(deck, id)?),
            ids => slides(ids.len()),
        })
    }
}

impl ToolBox {
    /// The slide studio is showing for `path`, if it is linked to this session.
    fn studio_focus(&self, path: &Path) -> Option<String> {
        let context = self.deck_link.lock().expect("deck link");
        let (focused, slide, _) = context.focus.as_ref()?;
        (focused.canonicalize().ok()? == path.canonicalize().ok()?).then(|| slide.clone())
    }

    /// Tell the terminal which slide studio should jump to.
    fn deck_changed(&self, path: &Path, slide: Option<String>) {
        if let Some(slide) = slide {
            let mut context = self.deck_link.lock().expect("deck link");
            context.changed.push((path.to_path_buf(), slide));
            context.notify.notify_one();
        }
    }

    /// Run one `deck` action (`deck_create` … `deck_resolve`).
    pub(super) fn deck_action(&self, action: &str, v: &Value) -> Result<String> {
        let relative = match (v["path"].as_str(), v["title"].as_str()) {
            (Some(path), _) => path.to_string(),
            (None, Some(title)) if action == "deck_create" => {
                format!("decks/{}.deck.json", slug(title))
            }
            _ => bail!("path is required, for example decks/talk.deck.json"),
        };
        let path = confined(&self.space_files_dir, &relative)?;
        match action {
            "deck_create" => self.deck_create(&relative, &path, v),
            "deck_read" => {
                let mut out = read_text(&relative, &path, &load(&path)?)?;
                out.push_str(schema_once(
                    &mut self.deck_link.lock().expect("deck link").schema_shown,
                ));
                Ok(out)
            }
            "deck_fonts" => Ok(fonts_text(
                &load(&path)?,
                v["text"].as_str().or_else(|| v["query"].as_str()),
            )),
            "deck_view" => self.deck_view(&relative, &path, v),
            "deck_edit" => self.deck_edit(&relative, &path, v),
            "deck_comment" => deck_comment(&relative, &path, v),
            "deck_resolve" => deck_resolve(&relative, &path, v),
            _ => bail!("unknown deck action"),
        }
    }

    fn deck_create(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        ensure!(
            !path.exists(),
            "{relative} already exists; read it, then edit"
        );
        let title = v["title"].as_str().context("title is required")?;
        let deck = new_deck(title, v.get("slides"))?;
        save(path, &deck)?;
        self.deck_changed(path, deck.slides.first().map(|s| s.id.clone()));
        let schema = schema_once(&mut self.deck_link.lock().expect("deck link").schema_shown);
        Ok(format!(
            "created deck · {relative} · {} · revision {}\n{}{schema}",
            slides(deck.slides.len()),
            short(&deck.revision()),
            Shown::overview(&deck).lines(&deck, path, &self.space_files_dir)?
        ))
    }

    fn deck_view(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        let deck = load(path)?;
        let shown = if let Some(ids) = v.get("slides") {
            let ids: Vec<String> = serde_json::from_value(ids.clone())
                .context("slides must be a list of slide ids, for example [\"s1\",\"s3\"]")?;
            ensure!(
                ids.len() <= MAX_SHEET_SLIDES,
                "view at most {MAX_SHEET_SLIDES} slides at once"
            );
            for id in &ids {
                slide_number(&deck, id)?;
            }
            Shown::some(&deck, ids)
        } else if let Some(id) = self
            .studio_focus(path)
            .filter(|id| slide_number(&deck, id).is_ok())
        {
            Shown::some(&deck, vec![id])
        } else {
            Shown::overview(&deck)
        };
        Ok(format!(
            "viewed {} · {relative} · revision {}\n{}",
            shown.label(&deck)?,
            short(&deck.revision()),
            shown.lines(&deck, path, &self.space_files_dir)?
        ))
    }

    fn deck_edit(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        let deck = load(path)?;
        if let Some(revision) = v["revision"].as_str() {
            ensure!(
                revision.len() >= 8 && deck.revision().starts_with(revision),
                "stale revision: the deck is now at revision {}. Nothing was written; read it again before editing.",
                short(&deck.revision())
            );
        }
        let edit = prepare(&deck, &v["ops"])?;
        let next = crate::deck::apply(&deck, &edit.ops)?;
        let changed = changed_slides(&deck, &next);
        save(path, &next)?;
        let focus = changed
            .first()
            .or_else(|| next.slides.first().map(|s| &s.id))
            .cloned();
        self.deck_changed(path, focus);
        let shown = Shown::some(&next, changed);
        let mut out = format!(
            "edited {} · {relative} · revision {}\n",
            shown.label(&next)?,
            short(&next.revision())
        );
        for op in &edit.ops {
            let _ = writeln!(out, "- {}", describe(op, &deck));
        }
        out.push_str(&shown.lines(&next, path, &self.space_files_dir)?);
        out.push_str(schema_once(
            &mut self.deck_link.lock().expect("deck link").schema_shown,
        ));
        Ok(out)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn deck_comment(relative: &str, path: &Path, v: &Value) -> Result<String> {
    let deck = load(path)?;
    let text = v["text"].as_str().context("text is required")?.trim();
    ensure!(
        !text.is_empty() && text.chars().count() <= MAX_COMMENT_CHARS,
        "a comment is one to two sentences, at most {MAX_COMMENT_CHARS} characters"
    );
    let mut threads = feedback::load(path)?;
    let edits = |anchor: &Anchor| -> Result<Vec<Edit>> {
        if v.get("ops").is_none() {
            return Ok(vec![]);
        }
        Ok(vec![Edit::new(&deck, prepare(&deck, &v["ops"])?, anchor)])
    };
    if let Some(id) = v["thread"].as_str() {
        let thread = threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("unknown thread; read the deck for current thread ids")?;
        ensure!(thread.messages.len() < 100, "that thread is full");
        thread.anchor.check(&deck).map_err(anyhow::Error::msg)?;
        let edits = edits(&thread.anchor)?;
        thread
            .messages
            .push(Message::new(Role::Model, text.into(), edits));
        feedback::save(path, &threads)?;
        return Ok(format!("replied in thread {id} · {relative}\n"));
    }
    let slide = v["slide"]
        .as_str()
        .context("slide is required for a new comment (or pass thread to reply)")?;
    let severity: Severity = serde_json::from_value(v["severity"].clone())
        .context("severity is required for a new comment: must, should or consider")?;
    let block = v["block"].as_str().map(str::to_string);
    let size = deck.theme.size;
    let rect = deck.slide(slide).zip(block.as_ref()).and_then(|(s, id)| {
        let assets = path.parent().map(|dir| dir.join("assets"));
        crate::deck::layout_map(&deck, s, assets.as_deref()).remove(id)
    });
    let clamp = |v: f64, bound: f64| v.clamp(0.0, bound - 1.0).round() as i32;
    let anchor = Anchor {
        slide: slide.into(),
        block,
        x: rect.map_or(120, |r| clamp(r.x, size.w)),
        y: rect.map_or(120, |r| clamp(r.y, size.h)),
        px: true,
    };
    anchor.check(&deck).map_err(anyhow::Error::msg)?;
    ensure!(
        threads.len() < MAX_THREADS,
        "this deck has {MAX_THREADS} threads; resolve some first"
    );
    let edits = edits(&anchor)?;
    let id = uuid::Uuid::new_v4().to_string();
    threads.push(Thread {
        id: id.clone(),
        anchor,
        severity,
        created: feedback::now(),
        resolved: None,
        messages: vec![Message::new(Role::Model, text.into(), edits)],
    });
    feedback::save(path, &threads)?;
    Ok(format!(
        "commented on slide {} · {relative}\nthread {id}\n",
        slide_number(&deck, slide)?
    ))
}

fn deck_resolve(relative: &str, path: &Path, v: &Value) -> Result<String> {
    let id = v["thread"].as_str().context("thread is required")?;
    let mut threads = feedback::load(path)?;
    threads
        .iter_mut()
        .find(|t| t.id == id)
        .context("unknown thread; read the deck for current thread ids")?
        .resolved = Some(feedback::now());
    feedback::save(path, &threads)?;
    Ok(format!("resolved thread {id} · {relative}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        tools: ToolBox,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nexus-deck-{}", uuid::Uuid::new_v4()));
            let mut tools = ToolBox::new(
                root.join("skills"),
                None,
                None,
                "auto".into(),
                vec![],
                None,
                None,
                None,
            );
            tools.space_files_dir = root.join("files");
            Self { root, tools }
        }

        async fn run(&self, action: &str, mut fields: Value) -> String {
            fields["action"] = json!(action);
            self.tools.run("deck", &fields.to_string()).await.0
        }

        fn file(&self, relative: &str) -> PathBuf {
            self.tools.space_files_dir.join(relative)
        }

        /// The files named by `![alt](path)` lines in a result.
        fn images(&self, result: &str) -> Vec<PathBuf> {
            result
                .lines()
                .filter_map(|line| line.strip_prefix("!["))
                .filter_map(|line| line.split_once("](")?.1.strip_suffix(')'))
                .map(|relative| self.file(relative))
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const PATH: &str = "decks/test-deck.deck.json";

    #[tokio::test]
    async fn create_read_view_return_text_and_real_images() {
        let f = Fixture::new();
        let created = f.run("create", json!({"title": "Test deck"})).await;
        assert!(
            created.starts_with("created deck · decks/test-deck.deck.json"),
            "{created}"
        );
        let images = f.images(&created);
        assert_eq!(images.len(), 1, "{created}");
        assert!(image::open(&images[0]).is_ok());
        assert!(images[0].starts_with(f.file("decks/.renders")));
        let again = f.run("create", json!({"title": "Test deck"})).await;
        assert!(again.contains("already exists"), "{again}");

        let read = f.run("read", json!({"path": PATH})).await;
        assert!(
            read.contains("slide 1 (id s1)") && read.contains("b1 text [160,"),
            "{read}"
        );
        assert!(
            read.contains("1920x1080 px")
                && read.contains("text style title:")
                && read.contains("comment threads: 0"),
            "{read}"
        );
        // The field reference comes once per session: with the create result.
        assert!(
            created.contains("schema (sent once per session)"),
            "{created}"
        );
        assert!(!read.contains("schema (sent once per session)"), "{read}");

        let view = f.run("view", json!({"path": PATH})).await;
        assert!(view.starts_with("viewed slide 1 ·"), "{view}");
        assert_eq!(f.images(&view).len(), 1);
        let missing = f
            .run("view", json!({"path": PATH, "slides": ["nope"]}))
            .await;
        assert!(
            missing.contains("unknown slide") && missing.contains("s1"),
            "{missing}"
        );
    }

    #[tokio::test]
    async fn create_accepts_initial_slides_and_rejects_invalid_ones() {
        let f = Fixture::new();
        let slides = json!([
            {"id": "a", "layout": {"mode": "column", "padding": 80}, "children": [{"kind": "text", "text": "Hello"}]},
            {"id": "b", "children": []}
        ]);
        let created = f
            .run("create", json!({"title": "Talk", "slides": slides}))
            .await;
        assert!(created.contains("2 slides"), "{created}");
        let bad = f
            .run(
                "create",
                json!({"title": "Bad", "path": "decks/bad.deck.json", "slides": [{"layout": "nonsense"}]}),
            )
            .await;
        assert!(
            bad.contains("deck failed") && !f.file("decks/bad.deck.json").exists(),
            "{bad}"
        );
        let view = f
            .run(
                "view",
                json!({"path": "decks/talk.deck.json", "slides": ["a", "b", "a", "b", "a"]}),
            )
            .await;
        assert!(
            view.contains("viewed 5 slides") && f.images(&view).len() == 1,
            "{view}"
        );
    }

    #[tokio::test]
    async fn edit_is_atomic_stale_aware_located_and_shows_the_change() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        let before = std::fs::read(f.file(PATH)).unwrap();
        let rejected = [
            json!({"path": PATH, "ops": [{"op": "update_node", "id": "missing", "fields": {"text": "x"}}]}),
            json!({"path": PATH, "ops": [{"op": "update_block", "slide": "s1", "id": "b1", "fields": {"text": "x"}}]}),
            json!({"path": PATH, "ops": [{"op": "launch_missiles"}]}),
            json!({"path": PATH, "ops": []}),
            json!({"path": PATH, "revision": "deadbeefdeadbeef", "ops": [{"op": "delete_slide", "id": "s1"}]}),
        ];
        for fields in rejected {
            let error = f.run("edit", fields).await;
            assert!(error.starts_with("deck failed"), "{error}");
            assert_eq!(std::fs::read(f.file(PATH)).unwrap(), before);
        }
        let stale = f
            .run(
                "edit",
                json!({"path": PATH, "revision": "deadbeefdeadbeef", "ops": [{"op": "delete_slide", "id": "s1"}]}),
            )
            .await;
        assert!(
            stale.contains("stale revision") && stale.contains("read it again"),
            "{stale}"
        );
        let located = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "update_node", "id": "missing", "fields": {"text": "x"}}]}),
            )
            .await;
        assert!(
            located.contains("ops[0]") && located.contains("Nothing was written"),
            "{located}"
        );

        let read = f.run("read", json!({"path": PATH})).await;
        let revision = read
            .split("revision ")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .unwrap()
            .to_string();
        let edited = f
            .run(
                "edit",
                json!({"path": PATH, "revision": revision, "ops": [
                    {"op": "update_node", "id": "b1", "fields": {"text": "New title"}},
                    {"op": "update_node", "id": "b1", "fields": {"font": {"size": 100}}},
                    {"op": "add_slide", "slide": {"children": [{"kind": "text", "text": "Added", "position": {"x": 80, "y": 80}}]}}
                ]}),
            )
            .await;
        assert!(edited.starts_with("edited 2 slides ·"), "{edited}");
        assert!(
            edited.contains("- update_node b1 on slide 1: text") && edited.contains("- add_slide"),
            "{edited}"
        );
        assert_eq!(f.images(&edited).len(), 2, "{edited}");
        let deck = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
        assert_eq!(deck.slides.len(), 2);
        assert!(!deck.slides[1].id.is_empty() && !deck.slides[1].children[0].id.is_empty());

        let one = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "set_notes", "slide": "s1", "notes": "n"}]}),
            )
            .await;
        assert!(one.starts_with("edited slide 1 ·"), "{one}");
        let deleted = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "delete_slide", "id": "s1"}]}),
            )
            .await;
        assert!(deleted.starts_with("edited slide 1 ·"), "{deleted}");
    }

    #[tokio::test]
    async fn edits_and_creates_notify_the_linked_studio() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        f.run(
            "edit",
            json!({"path": PATH, "ops": [{"op": "set_notes", "slide": "s1", "notes": "n"}]}),
        )
        .await;
        let changed = std::mem::take(&mut f.tools.deck_link.lock().unwrap().changed);
        assert_eq!(changed.len(), 2);
        assert!(
            changed
                .iter()
                .all(|(path, slide)| path.ends_with("test-deck.deck.json") && slide == "s1")
        );
    }

    #[tokio::test]
    async fn view_defaults_to_the_slide_studio_is_showing() {
        let f = Fixture::new();
        f.run(
            "create",
            json!({"title": "Test deck", "slides": [
                {"id": "a", "children": [{"kind": "text", "text": "One"}]},
                {"id": "b", "children": [{"kind": "text", "text": "Two"}]}
            ]}),
        )
        .await;
        let path = "decks/test-deck.deck.json";
        let overview = f.run("view", json!({"path": path})).await;
        assert!(overview.starts_with("viewed 2 slides"), "{overview}");
        f.tools.deck_link.lock().unwrap().focus = Some((f.file(path), "b".into(), vec![]));
        let focused = f.run("view", json!({"path": path})).await;
        assert!(focused.starts_with("viewed slide 2"), "{focused}");
    }

    #[tokio::test]
    async fn comments_resolve_and_replies_share_studios_sidecar() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        let comment = f
            .run(
                "comment",
                json!({"path": PATH, "slide": "s1", "block": "b1", "severity": "should",
                    "text": "Clarify the title so the audience knows the point.",
                    "ops": [{"op": "update_node", "id": "b1", "fields": {"text": "Clear title"}}]}),
            )
            .await;
        assert!(comment.starts_with("commented on slide 1"), "{comment}");
        let threads = feedback::load(&f.file(PATH)).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].messages[0].edits.len(), 1);
        let id = threads[0].id.clone();

        let reply = f
            .run(
                "comment",
                json!({"path": PATH, "thread": id, "text": "Updated."}),
            )
            .await;
        assert!(reply.starts_with("replied in thread"), "{reply}");
        assert_eq!(feedback::load(&f.file(PATH)).unwrap()[0].messages.len(), 2);
        let read = f.run("read", json!({"path": PATH})).await;
        assert!(
            read.contains("1 open") && read.contains("latest (nexus): Updated."),
            "{read}"
        );

        for bad in [
            json!({"path": PATH, "slide": "s1", "block": "nope", "severity": "must", "text": "x"}),
            json!({"path": PATH, "slide": "nope", "severity": "must", "text": "x"}),
            json!({"path": PATH, "slide": "s1", "severity": "loud", "text": "x"}),
            json!({"path": PATH, "slide": "s1", "severity": "must", "text": "x".repeat(501)}),
            json!({"path": PATH, "slide": "s1", "severity": "must", "text": "x", "ops": [{"op": "delete_slide", "id": "zz"}]}),
            json!({"path": PATH, "text": "no anchor"}),
        ] {
            let result = f.run("comment", bad).await;
            assert!(result.starts_with("deck failed"), "{result}");
        }
        assert_eq!(feedback::load(&f.file(PATH)).unwrap().len(), 1);

        let resolved = f.run("resolve", json!({"path": PATH, "thread": id})).await;
        assert!(resolved.contains("resolved thread"));
        assert!(feedback::load(&f.file(PATH)).unwrap()[0].resolved.is_some());
        let unknown = f
            .run("resolve", json!({"path": PATH, "thread": "nope"}))
            .await;
        assert!(unknown.contains("unknown thread"), "{unknown}");
    }

    #[tokio::test]
    async fn read_returns_resolved_rects_for_every_node_and_the_tree_shape() {
        let f = Fixture::new();
        let slides = json!([{"id": "a", "layout": {"mode": "column", "gap": 20, "padding": 100, "cross": "stretch"}, "children": [
            {"id": "t", "kind": "text", "text": "Title", "style": "heading"},
            {"id": "row", "kind": "frame", "layout": {"mode": "row", "gap": 40}, "sizing": {"h": 300}, "fill": "muted", "children": [
                {"id": "x", "kind": "frame", "sizing": {"w": 200, "h": "fill"}},
                {"id": "y", "kind": "text", "text": "Side", "sizing": {"w": "fill"}, "font": {"size": 30, "color": "accent"}}
            ]}
        ]}]);
        f.run("create", json!({"title": "Tree", "slides": slides}))
            .await;
        let read = f.run("read", json!({"path": "decks/tree.deck.json"})).await;
        // Resolved rects, with children nested one level deeper.
        assert!(read.contains("\n  t text [100,100 1720x"), "{read}");
        assert!(read.contains("\n  row frame [100,"), "{read}");
        assert!(
            read.contains("\n    x frame [100,") && read.contains("size 200xfill"),
            "{read}"
        );
        assert!(
            read.contains("layout row gap 40") && read.contains("fill muted"),
            "{read}"
        );
        assert!(read.contains("size=30 color=accent"), "{read}");
        assert!(
            read.contains("slide 1 (id a) layout column gap 20"),
            "{read}"
        );
    }

    #[tokio::test]
    async fn v2_structure_ops_work_through_edit() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        let edited = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [
                    {"op": "insert_node", "parent": "s1", "node": {"id": "sub", "kind": "text", "text": "More", "style": "caption"}},
                    {"op": "wrap", "ids": ["b1", "sub"], "layout": {"mode": "column", "gap": 12}, "id": "stack"},
                    {"op": "set_text_style", "name": "caption", "font": {"size": 30}},
                    {"op": "set_layout", "target": "s1", "layout": {"mode": "column", "padding": 120, "main": "center"}}
                ]}),
            )
            .await;
        assert!(edited.starts_with("edited slide 1"), "{edited}");
        for line in [
            "- insert_node text sub into s1",
            "- wrap b1, sub in a column frame",
            "- set_text_style caption",
            "- set_layout s1 to column",
        ] {
            assert!(edited.contains(line), "{line}: {edited}");
        }
        let deck = Deck::check(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
        assert_eq!(deck.version, 2);
        assert_eq!(deck.node("stack").unwrap().children().len(), 2);
        assert_eq!(deck.theme.text_styles["caption"].size, Some(30.0));
        let two = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [
                    {"op": "update_node", "id": "sub", "fields": {"text": "A"}},
                    {"op": "update_node", "id": "sub", "fields": {"font": {"weight": 700}}}
                ]}),
            )
            .await;
        assert!(
            two.starts_with("edited slide 1"),
            "two updates of one node in a call are fine: {two}"
        );
    }

    #[tokio::test]
    async fn a_version_one_deck_on_disk_reads_and_edits_as_version_two() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.file("decks")).unwrap();
        std::fs::write(
            f.file(PATH),
            include_str!("../../tests/fixtures/deck/classic.deck.json"),
        )
        .unwrap();
        let read = f.run("read", json!({"path": PATH})).await;
        assert!(read.contains("b1 text [120,360 1680x300]"), "{read}");
        assert!(read.contains("size=119"), "{read}");
        let edited = f
            .run(
                "edit",
                json!({"path": PATH, "ops": [{"op": "update_node", "id": "b1", "fields": {"font": {"size": 140}}}]}),
            )
            .await;
        assert!(edited.starts_with("edited slide 1"), "{edited}");
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(f.file(PATH)).unwrap()).unwrap();
        assert_eq!(on_disk["version"], 2);
        assert!(on_disk["slides"][0].get("blocks").is_none());
    }

    #[tokio::test]
    async fn fonts_lists_deck_fonts_and_at_most_thirty_installed_families() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        f.run(
            "edit",
            json!({"path": PATH, "ops": [{"op": "set_theme", "theme": {
                "fonts": {"heading": "Brand Sans"},
                "font_files": [{"family": "Brand Sans", "file": "assets/fonts/brand.ttf", "weight": 700},
                               {"family": "Web Only", "file": "assets/fonts/web.woff2"}]}}]}),
        )
        .await;
        let all = f.run("fonts", json!({"path": PATH})).await;
        assert!(
            all.contains("deck font roles: body=Inter heading=Brand Sans mono=JetBrains Mono"),
            "{all}"
        );
        assert!(
            all.contains("deck font: Brand Sans (assets/fonts/brand.ttf, weight 700)"),
            "{all}"
        );
        assert!(all.contains("Web Only") && all.contains("woff2"), "{all}");
        let listed = all.lines().filter(|l| l.starts_with("- ")).count();
        assert!(listed <= 30, "{all}");
        let none = f
            .run(
                "fonts",
                json!({"path": PATH, "text": "zzzz-no-such-family"}),
            )
            .await;
        assert!(none.contains("0 of 0 shown"), "{none}");
        assert_eq!(none.lines().filter(|l| l.starts_with("- ")).count(), 0);
        // Fonts never write.
        assert!(!all.contains("deck failed"));
    }

    #[tokio::test]
    async fn paths_cannot_leave_decks_or_pass_symlinks() {
        let f = Fixture::new();
        let files = &f.tools.space_files_dir;
        for bad in [
            "../bad.deck.json",
            "decks/../../bad.deck.json",
            "/tmp/bad.deck.json",
            "other/bad.deck.json",
            "decks/bad.json",
            "decks/.renders/x.deck.json",
            "decks/a b.deck.json",
            "decks/a)(b.deck.json",
        ] {
            assert!(confined(files, bad).is_err(), "{bad}");
            let result = f.run("create", json!({"title": "x", "path": bad})).await;
            assert!(result.starts_with("deck failed"), "{bad}: {result}");
        }
        assert!(confined(files, "decks/sub/ok-1.deck.json").is_ok());
        #[cfg(unix)]
        {
            std::fs::create_dir_all(files.join("decks")).unwrap();
            std::os::unix::fs::symlink(&f.root, files.join("decks/escape")).unwrap();
            assert!(confined(files, "decks/escape/bad.deck.json").is_err());
            let result = f
                .run(
                    "create",
                    json!({"title": "x", "path": "decks/escape/bad.deck.json"}),
                )
                .await;
            assert!(result.contains("symlink"), "{result}");
        }
    }

    #[test]
    fn read_only_actions_are_classified_for_batch_and_agent_mode() {
        for (args, read_only) in [
            (r#"{"action":"read","path":"decks/a.deck.json"}"#, true),
            (r#"{"action":"view","path":"decks/a.deck.json"}"#, true),
            (r#"{"action":"fonts","path":"decks/a.deck.json"}"#, true),
            (
                r#"{"action":"edit","path":"decks/a.deck.json","ops":[]}"#,
                false,
            ),
            (r#"{"action":"create","title":"x"}"#, false),
            (
                r#"{"action":"comment","path":"decks/a.deck.json","text":"x"}"#,
                false,
            ),
            (
                r#"{"action":"resolve","path":"decks/a.deck.json","thread":"t"}"#,
                false,
            ),
        ] {
            assert_eq!(
                crate::tools::is_read_only_tool("deck", args),
                read_only,
                "{args}"
            );
        }
    }
}
