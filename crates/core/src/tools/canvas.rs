//! The `canvas` tool: confined, atomic deck and doc actions. Studio and the model share
//! the same `*.deck.json` and `*.deck.json.comments.json` files, so every
//! write here shows up in an open studio window within half a second.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};

use crate::deck::brief::{font_line, px};
use crate::deck::feedback::{self, Anchor, Edit, Message, Role, Severity, Thread};
use crate::deck::history::{self, Who};
use crate::deck::lenient::{self, Context as Lenient};
use crate::deck::suggest::{self, RawSuggestion, Suggestion};
use crate::deck::{
    AnimationSlot, BuildBy, CheckError, Deck, Dim, Font, Kind, Mode, Node, Op, Position, Slide,
    Transition, Trigger, assets, direct, render,
};
use crate::remote_image::Fetcher;

use super::ToolBox;

#[cfg(test)]
#[path = "canvas/tests.rs"]
mod canvas_tests;
#[path = "canvas/design.rs"]
mod design;
#[path = "canvas/docs.rs"]
mod docs;
#[path = "canvas/read.rs"]
mod read;

/// Slides shown as individual images; more become one contact sheet.
const MAX_SEPARATE_IMAGES: usize = 4;
/// Slides on one contact sheet.
const MAX_SHEET_SLIDES: usize = 12;
const MAX_COMMENT_CHARS: usize = 500;
const MAX_THREADS: usize = 200;
/// How much of a block's text `read` returns.
const READ_TEXT_CHARS: usize = 400;
const SHORT_REVISION: usize = 12;
/// Located errors listed in one failed call.
const MAX_ERRORS_SHOWN: usize = 12;
/// Operation lines listed before the edit result switches to counts.
const MAX_OP_LINES: usize = 10;
/// Versions listed by `history`.
const HISTORY_LINES: usize = 20;
/// Remote images fetched in one call; the rest are reported.
const MAX_FETCHES: usize = 12;
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// Approximations listed when a `.pptx` is imported; the rest are counted.
const MAX_IMPORT_WARNINGS: usize = 12;

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
    load_noted(path).map(|(deck, _)| deck)
}

/// Load a deck with the notes about animation data dropped while parsing it.
fn load_noted(path: &Path) -> Result<(Deck, Vec<String>)> {
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
    Deck::check_noted(&source)
        .map_err(|errors| anyhow::anyhow!("deck file is invalid: {}", located(&errors)))
}

fn save(path: &Path, deck: &Deck) -> Result<()> {
    crate::deck::write_atomic(path, &serde_json::to_vec_pretty(deck)?)
}

/// Located errors, one per line, each with a one-line fix when the mistake
/// is a known one.
fn format_errors(errors: &[CheckError]) -> String {
    let mut out = String::new();
    for e in errors.iter().take(MAX_ERRORS_SHOWN) {
        let _ = write!(out, "\n- {}: {}", e.path, e.message);
        if let Some(hint) = lenient::hint(&e.path, &e.message) {
            let _ = write!(out, " — fix: {hint}");
        }
    }
    if errors.len() > MAX_ERRORS_SHOWN {
        let _ = write!(out, "\n- … {} more", errors.len() - MAX_ERRORS_SHOWN);
    }
    out
}

/// Fill hashes and insertion ids, check every operation against `deck`, and
/// return the prepared suggestion; no write happens here. Used for comments:
/// a suggestion is reviewed by the author before it applies, so it is held to
/// the suggestion rules (at most 40 operations, no new images).
fn prepare(deck: &Deck, ops: &Value, lenient: &mut Lenient) -> Result<Suggestion> {
    let mut ops = ops.as_array().context("ops must be an array")?.clone();
    ensure!(!ops.is_empty(), "ops must contain at least one operation");
    lenient.ops(&mut ops);
    let (mut valid, errors) = suggest::prepare(
        deck,
        vec![RawSuggestion {
            why: "Suggested edit".into(),
            ops,
        }],
    );
    valid.pop().ok_or_else(|| {
        let limit = errors
            .iter()
            .any(|e| e.message.contains("at most 40") || e.message.contains("image src must"));
        anyhow::anyhow!(
            "Nothing was written. Problems to fix:{}{}",
            format_errors(&errors),
            if limit {
                "\nA comment's suggested edit is limited to 40 operations and cannot add images; use edit to change the deck directly."
            } else {
                ""
            }
        )
    })
}

/// Write a deck after keeping the version it replaces.
fn write_deck(path: &Path, deck: &Deck, who: Who) -> Result<()> {
    // History is a safety net: failing to keep a version must not block the write.
    let _ = history::record_before_write(path, &deck.revision(), who);
    save(path, deck)
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
        Op::ApplyDesign { .. } => "apply_design".into(),
        Op::SetTransition { slide, .. } => format!("set_transition slide {}", number(slide)),
        Op::SetAnimation { id, slot, .. } => format!("set_animation {id} {slot:?}"),
        Op::ReorderBuilds { slide, .. } => format!("reorder_builds slide {}", number(slide)),
    }
}

fn dim(d: Dim) -> String {
    match d {
        Dim::Fixed(n) => px(n),
        Dim::Hug => "hug".into(),
        Dim::Fill => "fill".into(),
    }
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
- slide transition{type:none|fade|push|slide|cover|zoom|smart, direction?:left|right|up|down, duration_ms:0-5000 (400), easing:linear|ease|ease_in|ease_out|ease_in_out|spring (ease_out)} plays on entering the slide in present mode. smart morphs nodes with the same id or name on the previous slide (position, size, opacity, rotation, fill, font size and color) and fades the rest. Renders and view show the final state of a slide, every build in.\n\
- node animation{build_in?:Build, emphasis?:Build, build_out?:Build}; Build{effect:appear|fade|rise|fly|scale|wipe|blur|typewriter|pulse|shake, direction?, duration_ms:0-5000 (400), delay_ms:0-10000 (0), easing, trigger:click|with_previous|after_previous (click), by:whole|word|line|item (whole), stagger_ms?:0-10000}. pulse and shake are emphasis only and emphasis takes only them; typewriter and by word|line|item need a text node (item = list line). Slide builds:[node_id,...] is the playback order of animated nodes (a node's slots run in, emphasis, out); animated nodes left out are appended in tree order and unknown ids dropped. A click build starts a step; with_previous and after_previous join the step before them; builds ahead of the first click play on entry. read lists them as `builds: 1 click fade b2 · 2 with-prev rise b3`.\n\
- animation ops: set_transition{slide,transition|null}, set_animation{id,slot:build_in|emphasis|build_out,build|null}, reorder_builds{slide,ids}. Each is atomic with the other ops; update_node and update_slide merge patches also reach animation and transition.\n\
- images: src is an https URL (fetched when you create or edit, so renders show it) or a space file such as us_1.jpg or files/us_1.jpg (checked by content and copied into decks/assets). view and edit list any image drawn as a placeholder and why. Use the user's own photos and generated images from the space files; do not replace them. For other pictures, find real photos with search mode images and use their https URLs rather than drawing an illustration out of shapes.\n\
- reading: read slides:[\"s2\",3] returns just those slides in full. A deck over about 7000 characters reads as an outline (`id kind x,y wxh \"text\"`, ids complete) ending in a \"more slides\" line. Edit results list the changed nodes with their rects and the new revision, so edit again without reading; revision is optional, and a stale one is applied when the nodes you touch did not change, else refused with what changed.\n\
- design: build slides with add_slide{layout,slots,after?}; design op=read lists layout slots; apply_design{design?} refreshes the copied theme. Use color roles and named styles; fix design: lint hints.\n\
- history lists earlier versions (kept on every write by you or the user, last 50); history with revision restores one as a new, undoable write. One deck per request: edit it instead of creating -redesign copies or probe decks.\n\
- create also takes text_styles{name:font} to define styles it uses; update_node/update_slide fields.children replaces the children; edit takes up to 500 ops. Small slips (font as a string, kind ellipse, numeric strings, letter_spacing in px, an undefined style, a short path) are repaired and listed as `normalized:` lines; other errors list every located problem with a fix.\n\
- import: create with from:\"file.pptx\" (a space file) rebuilds a PowerPoint deck: every shape absolute at its place, text as text nodes, pictures copied (crops baked in), tables as text, notes and transitions kept, glows as stacked ellipses. Charts become labelled placeholders and SmartArt becomes shapes or a list; the result lists each approximation. Rebuild what matters with edit, using the original's text and images only.\n\
- set_layout converts children like Figma (none to row/column makes them auto; back makes them absolute where they are). wrap{ids,layout} puts siblings in a new frame; unwrap{id} dissolves it. update_node merges: {\"font\":{\"size\":64}} keeps other font fields, null removes one. set_text_style{name,font} changes every node using the style.\n";

fn author(role: &Role) -> &'static str {
    if matches!(role, Role::Model) {
        "nexus"
    } else {
        "user"
    }
}

/// The numbered build sequence of a slide on one line, if it has builds.
fn builds_line(slide: &Slide) -> Option<String> {
    let sequence = slide.build_sequence();
    if sequence.is_empty() {
        return None;
    }
    let builds = sequence
        .iter()
        .enumerate()
        .map(|(i, (node, slot, build))| {
            let slot = match slot {
                AnimationSlot::BuildIn => "",
                AnimationSlot::Emphasis => "emphasis ",
                AnimationSlot::BuildOut => "out ",
            };
            let trigger = match build.trigger {
                Trigger::Click => "click",
                Trigger::WithPrevious => "with-prev",
                Trigger::AfterPrevious => "after-prev",
            };
            let by = if build.by == BuildBy::Whole {
                String::new()
            } else {
                format!(" by {}", name(&build.by))
            };
            format!(
                "{} {trigger} {slot}{}{by} {}",
                i + 1,
                name(&build.effect),
                node.id
            )
        })
        .collect::<Vec<_>>();
    Some(builds.join(" · "))
}

/// `push left 400ms ease_out`: a transition on one line.
fn transition_line(transition: &Transition) -> String {
    let snake = |value: &Value| value.as_str().unwrap_or_default().to_string();
    let mut parts = vec![snake(&json!(transition.kind))];
    if let Some(direction) = transition.direction {
        parts.push(snake(&json!(direction)));
    }
    parts.push(format!("{}ms", transition.duration_ms));
    parts.push(snake(&json!(transition.easing)));
    parts.join(" ")
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

/// A new deck: the title slide only, or the given slides, with any text
/// styles the call defines.
fn new_deck(
    title: &str,
    slides: Option<&Value>,
    styles: Option<&Value>,
    theme: &crate::deck::Theme,
) -> Result<Deck> {
    if slides.is_none() && styles.is_none() {
        return Ok(Deck::titled(title));
    }
    let slides = slides.cloned().unwrap_or_else(|| {
        json!([{"id": "s1", "children": [{"id": "b1", "kind": "text", "text": title, "style": "title"}]}])
    });
    let mut deck = json!({"version": 2, "title": title, "slides": slides, "theme": theme});
    if let Some(styles) = styles {
        crate::doc::ops::merge_patch(&mut deck["theme"]["text_styles"], styles);
    }
    Deck::check(&deck.to_string()).map_err(|errors| {
        anyhow::anyhow!(
            "Nothing was written. {} to fix:{}",
            if errors.len() == 1 {
                "1 problem".to_string()
            } else {
                format!("{} problems", errors.len())
            },
            format_errors(&errors)
        )
    })
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

    /// Run one canvas action (legacy `deck_*` routes cover both kinds). Repairs made to
    /// the input are reported at the top of the result as `normalized:` lines.
    pub(super) async fn canvas_action(&self, action: &str, v: &Value) -> Result<String> {
        if action == "canvas_design" {
            return self.canvas_design(v);
        }
        if Self::is_doc(v) {
            return self.doc_action(action, v);
        }
        let mut notes = Vec::new();
        match self.deck_dispatch(action, v, &mut notes).await {
            Ok(out) => Ok(with_notes(&out, &notes)),
            Err(error) if !notes.is_empty() => Err(anyhow::anyhow!(
                "{error:#}\n{}",
                note_lines(&notes).trim_end()
            )),
            Err(error) => Err(error),
        }
    }

    async fn deck_dispatch(
        &self,
        action: &str,
        v: &Value,
        notes: &mut Vec<String>,
    ) -> Result<String> {
        let relative = match (v["path"].as_str(), v["title"].as_str()) {
            (Some(path), _) => match lenient::normalize_path(path) {
                Some(fixed) => {
                    notes.push(format!("path {path:?} → {fixed}"));
                    fixed
                }
                None => path.to_string(),
            },
            (None, Some(title)) if action == "deck_create" => {
                format!("decks/{}.deck.json", slug(title))
            }
            _ => bail!("path is required, for example decks/talk.deck.json"),
        };
        let path = confined(&self.space_files_dir, &relative)?;
        match action {
            "deck_create" => self.deck_create(&relative, &path, v, notes).await,
            "deck_read" => {
                let (deck, load_notes) = load_noted(&path)?;
                let selected = read::selection(&deck, v, true)?;
                let mut out = read::read_text(&relative, &path, &deck, selected.as_deref())?;
                for note in load_notes {
                    let _ = writeln!(out, "load note: {note}");
                }
                let schema = self.schema_if_fits(out.chars().count());
                out.push_str(schema);
                Ok(out)
            }
            "deck_fonts" => Ok(fonts_text(
                &load(&path)?,
                v["text"].as_str().or_else(|| v["query"].as_str()),
            )),
            "deck_view" => self.deck_view(&relative, &path, v).await,
            "deck_edit" => self.deck_edit(&relative, &path, v, notes).await,
            "deck_history" => self.deck_history(&relative, &path, v).await,
            "deck_comment" => deck_comment(&relative, &path, v, notes),
            "deck_resolve" => deck_resolve(&relative, &path, v),
            "deck_export" => self.deck_export(&relative, &path, v),
            _ => bail!("unknown deck action"),
        }
    }

    /// The once-per-session field reference, unless this result has no room
    /// for it (it then goes with a later, shorter result).
    fn schema_if_fits(&self, used: usize) -> &'static str {
        let mut context = self.deck_link.lock().expect("deck link");
        if context.schema_shown || used + SCHEMA.chars().count() > read::RESULT_BUDGET {
            return "";
        }
        context.schema_shown = true;
        SCHEMA
    }

    /// [`Self::schema_if_fits`] for the error path in `ToolBox::run`.
    pub(super) fn deck_schema_if_fits(&self, used: usize) -> &'static str {
        self.schema_if_fits(used)
    }

    /// Copy space-file images named by `value` into the deck's `assets/`.
    fn import_images(&self, value: &mut Value, path: &Path, notes: &mut Vec<String>) -> Result<()> {
        let assets = path.parent().context("deck folder")?.join("assets");
        assets::visit_srcs(value, &mut |src| {
            if let Some(copy) = assets::import_space_image(src, &self.space_files_dir, &assets)? {
                notes.push(format!(
                    "image {src:?} → {copy} (copied from the space files)"
                ));
                *src = copy;
            }
            Ok(())
        })
    }

    // `self` is only used by the test seam.
    #[allow(clippy::unused_self)]
    fn deck_fetcher(&self, assets: &Path) -> Arc<Fetcher> {
        #[cfg(test)]
        if let Some(fetcher) = &self.deck_images {
            return fetcher.clone();
        }
        Fetcher::new(assets)
    }

    /// Download the https images of the given slides (all when empty) that are
    /// not on this device yet, through the vetted fetcher, so renders and
    /// studio show them. Returns a line per problem and whether any image
    /// arrived.
    async fn fetch_images(&self, path: &Path, deck: &Deck, ids: &[String]) -> (Vec<String>, bool) {
        let mut wanted: Vec<(String, String)> = Vec::new();
        for slide in deck
            .slides
            .iter()
            .filter(|s| ids.is_empty() || ids.contains(&s.id))
        {
            for node in slide.nodes() {
                if let Kind::Image { src, .. } = &node.kind
                    && src.starts_with("https://")
                {
                    wanted.push((node.id.clone(), src.clone()));
                }
            }
        }
        if wanted.is_empty() {
            return (Vec::new(), false);
        }
        let Some(assets) = path.parent().map(|dir| dir.join("assets")) else {
            return (Vec::new(), false);
        };
        if self.remote_images.as_ref().is_some_and(|f| !f.enabled()) {
            return (
                vec![format!(
                    "remote images are off (setting remote_images): {} https image(s) were not downloaded and will show as placeholders",
                    wanted.len()
                )],
                false,
            );
        }
        if assets.is_symlink() || std::fs::create_dir_all(&assets).is_err() {
            return (
                vec!["image cache unavailable: assets is not a usable folder".into()],
                false,
            );
        }
        let fetcher = self.deck_fetcher(&assets);
        let mut urls: Vec<String> = Vec::new();
        for (_, src) in &wanted {
            if !urls.contains(src) && fetcher.cached(src).is_none() {
                urls.push(src.clone());
            }
        }
        if urls.is_empty() {
            return (Vec::new(), false);
        }
        let skipped: Vec<String> = urls.split_off(urls.len().min(MAX_FETCHES));
        let results = futures_util::future::join_all(urls.iter().map(|url| {
            let fetcher = Arc::clone(&fetcher);
            async move {
                (
                    url.clone(),
                    tokio::time::timeout(FETCH_TIMEOUT, fetcher.fetch(url)).await,
                )
            }
        }))
        .await;
        let mut lines = Vec::new();
        let mut arrived = false;
        for (id, src) in &wanted {
            if skipped.contains(src) {
                lines.push(format!("image {id}: not fetched (at most {MAX_FETCHES} downloads per call); edit again to fetch the rest"));
            } else if let Some((_, result)) = results.iter().find(|(url, _)| url == src) {
                match result {
                    Ok(Ok(_)) => arrived = true,
                    Ok(Err(error)) => lines.push(format!("image {id}: {}", fetch_reason(error))),
                    Err(_) => lines.push(format!(
                        "image {id}: timed out after {}s",
                        FETCH_TIMEOUT.as_secs()
                    )),
                }
            }
        }
        lines.dedup();
        (lines, arrived)
    }

    async fn deck_create(
        &self,
        relative: &str,
        path: &Path,
        v: &Value,
        notes: &mut Vec<String>,
    ) -> Result<String> {
        ensure!(
            !path.exists(),
            "{relative} already exists; read it, then edit it (one deck per request: change the existing deck instead of creating variants)"
        );
        let title = v["title"].as_str().context("title is required")?;
        if let Some(from) = v["from"].as_str().filter(|f| !f.trim().is_empty()) {
            return self.deck_import(relative, path, title, from, v).await;
        }
        let mut designed = Deck::titled(title);
        self.copy_deck_design(&mut designed, v)?;
        let mut lenient = Lenient::for_deck(&designed);
        let mut styles = v.get("text_styles").cloned();
        if let Some(styles) = styles.as_mut() {
            lenient.text_styles(styles);
        }
        let mut slides_value = v.get("slides").cloned();
        if let Some(value) = slides_value.as_mut() {
            lenient.slides(value);
            notes.extend(lenient.notes());
            self.import_images(value, path, notes)?;
        }
        let fresh: Vec<String> = lenient
            .notes()
            .into_iter()
            .filter(|n| !notes.contains(n))
            .collect();
        notes.extend(fresh);
        let mut deck = new_deck(
            title,
            slides_value.as_ref(),
            styles.as_ref(),
            &designed.theme,
        )?;
        self.copy_deck_design(&mut deck, v)?;
        write_deck(path, &deck, Who::Model)?;
        self.deck_changed(path, deck.slides.first().map(|s| s.id.clone()));
        let (problems, _) = self.fetch_images(path, &deck, &[]).await;
        let shown = Shown::overview(&deck);
        let mut out = format!(
            "created deck · {relative} · {} · revision {}\n",
            slides(deck.slides.len()),
            short(&deck.revision()),
        );
        out.push_str(&Self::image_report(path, &deck, &shown.ids, &problems));
        out.push_str(&shown.lines(&deck, path, &self.space_files_dir)?);
        let schema = self.schema_if_fits(out.chars().count());
        out.push_str(schema);
        self.append_design_lint(&mut out, &deck)?;
        Ok(out)
    }

    /// `create` with `from`: import a `.pptx` from the space files as the new deck.
    async fn deck_import(
        &self,
        relative: &str,
        path: &Path,
        title: &str,
        from: &str,
        v: &Value,
    ) -> Result<String> {
        let Some(source) = assets::find_space_file(from, &self.space_files_dir) else {
            bail!("no file {from:?} in the space files; name a .pptx file from the files list");
        };
        let (target, name) = (path.to_path_buf(), title.to_string());
        let imported = tokio::task::spawn_blocking(move || {
            crate::deck::import::import_pptx_to(&source, &target, Some(&name))
        })
        .await
        .context("the import was interrupted")??;
        let mut deck = imported.deck;
        self.copy_deck_design(&mut deck, v)?;
        save(path, &deck)?;
        self.deck_changed(path, deck.slides.first().map(|s| s.id.clone()));
        let shown = Shown::overview(&deck);
        let mut out = format!(
            "created deck · {relative} · {} · revision {} · imported from {from}\n",
            slides(deck.slides.len()),
            short(&deck.revision()),
        );
        for warning in imported.warnings.iter().take(MAX_IMPORT_WARNINGS) {
            let _ = writeln!(out, "approximation: {warning}");
        }
        if imported.warnings.len() > MAX_IMPORT_WARNINGS {
            let _ = writeln!(
                out,
                "approximation: and {} more",
                imported.warnings.len() - MAX_IMPORT_WARNINGS
            );
        }
        out.push_str(&Self::image_report(path, &deck, &shown.ids, &[]));
        out.push_str(&shown.lines(&deck, path, &self.space_files_dir)?);
        let schema = self.schema_if_fits(out.chars().count());
        out.push_str(schema);
        self.append_design_lint(&mut out, &deck)?;
        Ok(out)
    }

    /// Fetch problems, then every image on `ids` that will render as a placeholder.
    fn image_report(path: &Path, deck: &Deck, ids: &[String], problems: &[String]) -> String {
        let mut out = String::new();
        for line in problems {
            let _ = writeln!(out, "{line}");
        }
        let assets = path.parent().map(|dir| dir.join("assets"));
        let mut listed = 0;
        for slide in deck.slides.iter().filter(|s| ids.contains(&s.id)) {
            for node in slide.nodes() {
                let Kind::Image { src, .. } = &node.kind else {
                    continue;
                };
                if crate::deck::image_resolves(src, assets.as_deref()) {
                    continue;
                }
                listed += 1;
                if listed > 10 {
                    break;
                }
                let reason = if src.starts_with("https://") {
                    "the download did not complete, so it is drawn as a placeholder"
                } else {
                    "no such file in the deck's assets; name a space file (us_1.jpg) or an https URL"
                };
                let _ = writeln!(
                    out,
                    "placeholder: image {} on slide {} ({}): {reason}",
                    node.id,
                    slide_number(deck, &slide.id).unwrap_or(0),
                    clip(src, 60)
                );
            }
        }
        out
    }

    async fn deck_view(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        let deck = load(path)?;
        let picked = read::selection(&deck, v, false)?.filter(|s| !s.is_empty());
        let shown = if let Some(indexes) = picked {
            ensure!(
                indexes.len() <= MAX_SHEET_SLIDES,
                "view at most {MAX_SHEET_SLIDES} slides at once"
            );
            Shown::some(
                &deck,
                indexes.iter().map(|i| deck.slides[*i].id.clone()).collect(),
            )
        } else if let Some(id) = self
            .studio_focus(path)
            .filter(|id| slide_number(&deck, id).is_ok())
        {
            Shown::some(&deck, vec![id])
        } else {
            Shown::overview(&deck)
        };
        let (problems, arrived) = self.fetch_images(path, &deck, &shown.ids).await;
        if arrived {
            // Renders drawn before the images arrived show placeholders.
            render::invalidate_cached(path, &self.space_files_dir, &deck.revision());
        }
        let mut out = format!(
            "viewed {} · {relative} · revision {}\n",
            shown.label(&deck)?,
            short(&deck.revision()),
        );
        out.push_str(&Self::image_report(path, &deck, &shown.ids, &problems));
        out.push_str(&shown.lines(&deck, path, &self.space_files_dir)?);
        Ok(out)
    }

    /// Check a caller's `revision` against the deck. A match is fine. A stale
    /// one is compared with the kept version it names: edits that touch
    /// nothing that changed since are applied anyway (the returned note says
    /// so); otherwise the edit is refused with what changed.
    fn check_revision(
        path: &Path,
        deck: &Deck,
        revision: Option<&str>,
        ops: &[Value],
    ) -> Result<Option<String>> {
        let Some(given) = revision
            .map(|r| r.trim().to_lowercase())
            .filter(|r| !r.is_empty())
        else {
            return Ok(None);
        };
        let current = deck.revision();
        if given.len() >= 8 && current.starts_with(&given) {
            return Ok(None);
        }
        let stale = format!(
            "stale revision {given}: the deck is now at revision {}.",
            short(&current)
        );
        let Ok(entry) = history::find(path, &given) else {
            bail!(
                "{stale} That revision is not among the kept versions, so what changed cannot be told. Nothing was written. Read the slides you are editing (read with slides:[…]) and send the edit again."
            );
        };
        let old = entry.load()?;
        let assets = path.parent().context("deck folder")?.join("assets");
        let (changed, theme_changed) = read::differences(&old, deck, &assets);
        let (touched, touches_theme) = touched_ids(ops);
        let clash: Vec<&String> = touched
            .iter()
            .filter(|id| changed.contains_key(*id))
            .collect();
        if !clash.is_empty() || (touches_theme && theme_changed) {
            let mut message =
                format!("{stale} These changed since then, and the edit touches them:");
            for id in clash.iter().take(20) {
                let _ = write!(message, "\n- {id}: {}", changed[*id]);
            }
            if touches_theme && theme_changed {
                message.push_str("\n- the theme changed");
            }
            message.push_str(
                "\nNothing was written. Send the edit again against these current values; no read is needed.",
            );
            bail!(message);
        }
        let what: Vec<&str> = changed.keys().take(8).map(String::as_str).collect();
        Ok(Some(format!(
            "rebased: the deck changed since revision {given} ({}{}), none of which these operations touch; applied to revision {}",
            what.join(", "),
            if changed.len() > 8 { ", …" } else { "" },
            short(&current)
        )))
    }

    async fn deck_edit(
        &self,
        relative: &str,
        path: &Path,
        v: &Value,
        notes: &mut Vec<String>,
    ) -> Result<String> {
        let deck = load(path)?;
        let mut ops: Vec<Value> = v["ops"].as_array().context("ops must be an array")?.clone();
        if let Some(Value::Object(styles)) = v.get("text_styles") {
            for (style, font) in styles.iter().rev() {
                ops.insert(
                    0,
                    json!({"op": "set_text_style", "name": style, "font": font}),
                );
            }
        }
        ensure!(!ops.is_empty(), "ops must contain at least one operation");
        let rebased = Self::check_revision(path, &deck, v["revision"].as_str(), &ops)?;
        let mut lenient = Lenient::for_deck(&deck);
        lenient.ops(&mut ops);
        let mut value = Value::Array(ops);
        let imported = self.import_images(&mut value, path, notes);
        notes.extend(lenient.notes());
        imported?;
        let Value::Array(ops) = value else {
            unreachable!("ops stay an array")
        };
        let resolver = |slug: &str| crate::design::load(&self.space_files_dir, slug);
        let prepared =
            direct::prepare_with_resolver(&deck, ops, Some(&resolver)).map_err(|errors| {
                anyhow::anyhow!(
                    "Nothing was written. {} to fix:{}",
                    if errors.len() == 1 {
                        "1 problem".to_string()
                    } else {
                        format!("{} problems", errors.len())
                    },
                    format_errors(&errors)
                )
            })?;
        notes.extend(prepared.notes.iter().cloned());
        let next = prepared.next;
        let changed = changed_slides(&deck, &next);
        write_deck(path, &next, Who::Model)?;
        let focus = changed
            .first()
            .or_else(|| next.slides.first().map(|s| &s.id))
            .cloned();
        self.deck_changed(path, focus);
        let (problems, _) = self.fetch_images(path, &next, &changed).await;
        let shown = Shown::some(&next, changed.clone());
        let mut out = format!(
            "edited {} · {relative} · revision {} (was {})\n",
            shown.label(&next)?,
            short(&next.revision()),
            short(&deck.revision())
        );
        if let Some(rebased) = rebased {
            let _ = writeln!(out, "{rebased}");
        }
        out.push_str(&op_lines(&prepared.ops, &deck));
        let assets = path.parent().context("deck folder")?.join("assets");
        out.push_str(&read::changed_summary(&deck, &next, &assets, 3000));
        out.push_str(&Self::image_report(path, &next, &changed, &problems));
        out.push_str(&shown.lines(&next, path, &self.space_files_dir)?);
        let schema = self.schema_if_fits(out.chars().count());
        out.push_str(schema);
        self.append_design_lint(&mut out, &next)?;
        Ok(out)
    }

    /// `history`: the kept versions, or restore one as a new write.
    async fn deck_history(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        let deck = load(path)?;
        if let Some(revision) = v["revision"].as_str().filter(|r| !r.trim().is_empty()) {
            let entry = history::find(path, revision)?;
            let snapshot = entry.load()?;
            if snapshot.revision() == deck.revision() {
                return Ok(format!(
                    "{relative} is already at that version (revision {}); nothing to restore\n",
                    short(&deck.revision())
                ));
            }
            write_deck(path, &snapshot, Who::Model)?;
            let changed = changed_slides(&deck, &snapshot);
            self.deck_changed(
                path,
                changed
                    .first()
                    .or_else(|| snapshot.slides.first().map(|s| &s.id))
                    .cloned(),
            );
            let (problems, _) = self.fetch_images(path, &snapshot, &changed).await;
            let shown = Shown::some(&snapshot, changed.clone());
            let mut out = format!(
                "restored {relative} to version {} · revision {} (was {})\n{}\nThis is a new write: studio's undo goes back, and the version it replaced is in history.\n",
                entry.rev8,
                short(&snapshot.revision()),
                short(&deck.revision()),
                history::describe_change(&deck, &snapshot)
            );
            out.push_str(&Self::image_report(path, &snapshot, &changed, &problems));
            out.push_str(&shown.lines(&snapshot, path, &self.space_files_dir)?);
            return Ok(out);
        }
        let entries = history::list(path)?;
        let mut out = format!(
            "history · {relative} · current revision {} · {} · {} image(s)\n",
            short(&deck.revision()),
            slides(deck.slides.len()),
            history::image_count(&deck)
        );
        if entries.is_empty() {
            out.push_str(
                "no earlier versions are kept yet; every write by nexus or studio from now on keeps the version it replaces\n",
            );
            return Ok(out);
        }
        let _ = writeln!(
            out,
            "{} earlier version(s), newest first. Each line is a version that was replaced; `then` is what the replacing write changed.",
            entries.len()
        );
        let shown: Vec<&history::Entry> = entries.iter().rev().take(HISTORY_LINES).collect();
        let mut later = deck.clone();
        for entry in shown {
            let Ok(version) = entry.load() else {
                let _ = writeln!(out, "{} · {} · unreadable", entry.rev8, entry.time());
                continue;
            };
            let _ = writeln!(
                out,
                "{} · {} · replaced by {} · {} · {} image(s) · then: {}",
                entry.rev8,
                entry.time(),
                entry.who,
                slides(version.slides.len()),
                history::image_count(&version),
                history::describe_change(&version, &later)
            );
            later = version;
        }
        if entries.len() > HISTORY_LINES {
            let _ = writeln!(
                out,
                "… {} older versions not listed",
                entries.len() - HISTORY_LINES
            );
        }
        out.push_str("restore one with history and revision set to its first column\n");
        Ok(out)
    }
}

/// Ids an operation list refers to, and whether it changes the theme.
fn touched_ids(ops: &[Value]) -> (BTreeSet<String>, bool) {
    let mut ids = BTreeSet::new();
    let mut theme = false;
    for op in ops {
        for key in ["id", "slide", "parent", "target", "after"] {
            if let Some(id) = op[key].as_str() {
                ids.insert(id.to_string());
            }
        }
        if let Some(list) = op["ids"].as_array() {
            ids.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        if matches!(
            op["op"].as_str(),
            Some("set_text_style" | "set_theme" | "apply_design")
        ) {
            theme = true;
        }
    }
    (ids, theme)
}

/// One line per operation, or counts by kind when there are many.
fn op_lines(ops: &[Op], before: &Deck) -> String {
    let mut out = String::new();
    if ops.len() <= MAX_OP_LINES {
        for op in ops {
            let _ = writeln!(out, "- {}", describe(op, before));
        }
        return out;
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for op in ops {
        let line = describe(op, before);
        let kind = line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        *counts.entry(kind).or_default() += 1;
    }
    let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
    let _ = writeln!(out, "- {} operations: {}", ops.len(), parts.join(", "));
    out
}

fn fetch_reason(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let text = text.split(" for url").next().unwrap_or(&text);
    clip(text, 120)
}

fn note_lines(notes: &[String]) -> String {
    notes.iter().fold(String::new(), |mut out, note| {
        let _ = writeln!(out, "normalized: {note}");
        out
    })
}

/// Put the `normalized:` lines right under the first line of a result.
fn with_notes(out: &str, notes: &[String]) -> String {
    if notes.is_empty() {
        return out.to_string();
    }
    match out.split_once('\n') {
        Some((first, rest)) => format!("{first}\n{}{rest}", note_lines(notes)),
        None => format!("{out}\n{}", note_lines(notes)),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn deck_comment(relative: &str, path: &Path, v: &Value, notes: &mut Vec<String>) -> Result<String> {
    let deck = load(path)?;
    let text = v["text"].as_str().context("text is required")?.trim();
    ensure!(
        !text.is_empty() && text.chars().count() <= MAX_COMMENT_CHARS,
        "a comment is one to two sentences, at most {MAX_COMMENT_CHARS} characters"
    );
    let mut threads = feedback::load(path)?;
    let mut lenient = Lenient::for_deck(&deck);
    let suggestion = if v.get("ops").is_some() {
        let prepared = prepare(&deck, &v["ops"], &mut lenient);
        notes.extend(lenient.notes());
        Some(prepared?)
    } else {
        None
    };
    let edits = |anchor: &Anchor| -> Vec<Edit> {
        suggestion
            .iter()
            .map(|s| Edit::new(&deck, s.clone(), anchor))
            .collect()
    };
    if let Some(id) = v["thread"].as_str() {
        let thread = threads
            .iter_mut()
            .find(|t| t.id == id)
            .context("unknown thread; read the deck for current thread ids")?;
        ensure!(thread.messages.len() < 100, "that thread is full");
        thread.anchor.check(&deck).map_err(anyhow::Error::msg)?;
        let edits = edits(&thread.anchor);
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
    let edits = edits(&anchor);
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

impl ToolBox {
    /// Write the deck as a file under `<space files>/exports/`, never
    /// overwriting, and list what had to be approximated.
    fn deck_export(&self, relative: &str, path: &Path, v: &Value) -> Result<String> {
        use crate::deck::export::{self, Format, Options};
        let format: Format = v["text"]
            .as_str()
            .or_else(|| v["format"].as_str())
            .context("text is the format: pptx, pdf, png, html or json")?
            .trim()
            .to_ascii_lowercase()
            .parse()?;
        let deck = load(path)?;
        let assets = path.parent().map(|dir| dir.join("assets"));
        let output = export::export(&deck, assets.as_deref(), format, Options::default())?;
        let written = export::write_export(&output, &self.space_files_dir, None)?;
        let shown = written
            .strip_prefix(self.space_files_dir.canonicalize().unwrap_or_default())
            .unwrap_or(&written)
            .display()
            .to_string();
        let mut out = format!(
            "exported {relative} → {shown} ({} KB)\n",
            output.bytes.len().div_ceil(1024)
        );
        for warning in &output.warnings {
            let _ = writeln!(out, "approximation: {warning}");
        }
        Ok(out)
    }
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
#[path = "deck_animation_tests.rs"]
mod animation_tests;

#[cfg(test)]
#[path = "deck_v3_tests.rs"]
mod v3_tests;

#[cfg(test)]
#[path = "deck_import_tests.rs"]
mod import_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct Fixture {
        pub(super) root: PathBuf,
        pub(super) tools: ToolBox,
    }

    impl Fixture {
        pub(super) fn new() -> Self {
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

        pub(super) async fn run(&self, action: &str, mut fields: Value) -> String {
            fields["action"] = json!(action);
            self.tools.run("deck", &fields.to_string()).await.0
        }

        pub(super) fn file(&self, relative: &str) -> PathBuf {
            self.tools.space_files_dir.join(relative)
        }

        /// The files named by `![alt](path)` lines in a result.
        pub(super) fn images(&self, result: &str) -> Vec<PathBuf> {
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
            stale.contains("stale revision") && stale.contains("Nothing was written"),
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
    async fn a_first_failed_create_carries_the_field_reference_once() {
        let f = Fixture::new();
        let bad = json!({"title": "Test deck", "slides": [{"children": [{"kind": "hologram"}]}]});
        let first = f.run("create", bad.clone()).await;
        assert!(first.starts_with("deck failed"), "{first}");
        assert!(first.contains("schema (sent once per session)"), "{first}");
        let second = f.run("create", bad).await;
        assert!(
            !second.contains("schema (sent once per session)"),
            "{second}"
        );
    }

    #[tokio::test]
    async fn export_writes_under_exports_without_overwriting() {
        let f = Fixture::new();
        f.run("create", json!({"title": "Test deck"})).await;
        let first = f.run("export", json!({"path": PATH, "text": "pptx"})).await;
        assert!(first.contains("exports/test-deck.pptx"), "{first}");
        let second = f.run("export", json!({"path": PATH, "text": "pptx"})).await;
        assert!(second.contains("exports/test-deck (2).pptx"), "{second}");
        let bytes = std::fs::read(f.file("exports/test-deck.pptx")).unwrap();
        assert!(bytes.starts_with(b"PK"), "a pptx is a zip package");
        let bad = f.run("export", json!({"path": PATH, "text": "key"})).await;
        assert!(!bad.contains("exported"), "{bad}");
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
