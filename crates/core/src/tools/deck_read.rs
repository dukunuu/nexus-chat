//! What the `deck` tool tells the model about a deck: `read` (always within
//! the tool-result cap, falling back to an outline), edit summaries with the
//! rectangles of changed nodes, and the comparison behind stale-revision
//! handling.

use super::{
    BTreeMap, Deck, Kind, Mode, Node, Path, Result, Slide, Thread, Value, author, builds_line,
    clip, feedback, layout_line, name, px, short, slide_number, slides, transition_line,
    write_tree,
};
use anyhow::{Context as _, bail};
use std::fmt::Write as _;

/// Characters of header and slides in one `read`; a bigger deck is outlined.
pub(super) const READ_BUDGET: usize = 7000;
/// Everything a result may hold before the generic tool-result cap (8000)
/// would cut it.
pub(super) const RESULT_BUDGET: usize = 7800;
/// Most slide ids named in a "more slides" line.
const MORE_IDS: usize = 8;
/// Text shown per node in an outline.
const OUTLINE_TEXT: usize = 40;

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// Slide ids or 1-based numbers (as numbers or numeric strings) to indexes.
/// `None` when the call names no slides.
pub(super) fn selection(deck: &Deck, value: &Value, dedup: bool) -> Result<Option<Vec<usize>>> {
    let Some(list) = value.get("slides").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let items = list.as_array().context(
        "slides must be a list of slide ids or 1-based slide numbers, for example [\"s2\", 3]",
    )?;
    let mut out = Vec::new();
    for item in items {
        let text = match item {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            _ => bail!("slides must be slide ids or 1-based slide numbers"),
        };
        let index = deck
            .slides
            .iter()
            .position(|s| s.id == text)
            .or_else(|| {
                text.parse::<usize>()
                    .ok()
                    .filter(|n| (1..=deck.slides.len()).contains(n))
                    .map(|n| n - 1)
            })
            .with_context(|| {
                format!(
                    "unknown slide {text:?}; slides are: {}",
                    deck.slides
                        .iter()
                        .map(|s| s.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        if !dedup || !out.contains(&index) {
            out.push(index);
        }
    }
    Ok(Some(out))
}

fn header_text(relative: &str, deck: &Deck) -> String {
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
        let font = deck.theme.resolve_font(style, &super::Font::default());
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
    out
}

fn slide_heading(index: usize, slide: &Slide) -> String {
    let label = slide
        .name
        .as_ref()
        .map_or(String::new(), |n| format!(" {n:?}"));
    let fill = slide
        .fill
        .as_ref()
        .map_or(String::new(), |f| format!(" fill {f}"));
    let notes = if slide.notes.is_empty() {
        String::new()
    } else {
        format!(" · notes: {}", clip(&slide.notes, 160))
    };
    format!(
        "slide {} (id {}){label}{fill} layout {}{notes}\n",
        index + 1,
        slide.id,
        layout_line(&slide.layout)
    )
}

fn rects_of(
    deck: &Deck,
    slide: &Slide,
    path: &Path,
) -> std::collections::HashMap<String, crate::deck::Rect> {
    let assets = path.parent().map(|dir| dir.join("assets"));
    crate::deck::layout_map(deck, slide, assets.as_deref())
}

/// A slide with every node's sizing, style and overrides.
fn slide_full(deck: &Deck, path: &Path, index: usize) -> String {
    let slide = &deck.slides[index];
    let mut out = slide_heading(index, slide);
    if let Some(transition) = &slide.transition {
        let _ = writeln!(out, "  transition: {}", transition_line(transition));
    }
    if let Some(builds) = builds_line(slide) {
        let _ = writeln!(out, "  builds: {builds}");
    }
    write_tree(
        &mut out,
        &slide.children,
        &rects_of(deck, slide, path),
        0,
        slide.layout.mode != Mode::None,
    );
    out
}

/// `id kind x,y wxh "text"`: what a node is and where it sits.
pub(super) fn node_summary(node: &Node, rect: Option<crate::deck::Rect>, text: usize) -> String {
    let at = rect.map_or(String::new(), |r| {
        format!(" {},{} {}x{}", px(r.x), px(r.y), px(r.w), px(r.h))
    });
    let detail = match &node.kind {
        Kind::Text { text: t, .. } => format!(" {:?}", clip(t, text)),
        Kind::Code { source, .. } => format!(" {:?}", clip(source, text)),
        Kind::Image { src, .. } => format!(" {}", clip(src, 60)),
        Kind::Shape { shape, .. } => format!(" {}", name(shape)),
        Kind::Table { rows, .. } => format!(
            " {}x{}",
            rows.len(),
            rows.iter().map(Vec::len).max().unwrap_or(0)
        ),
        Kind::Chart { chart, .. } => format!(" {}", name(chart)),
        Kind::Frame { layout, .. } => format!(" {}", name(&layout.mode)),
    };
    format!("{} {}{at}{detail}", node.id, node.kind.name())
}

fn outline_nodes(
    out: &mut String,
    nodes: &[Node],
    rects: &std::collections::HashMap<String, crate::deck::Rect>,
    depth: usize,
    budget: usize,
    skipped: &mut usize,
) {
    for node in nodes {
        let line = format!(
            "{}{}\n",
            "  ".repeat(depth + 1),
            node_summary(node, rects.get(&node.id).copied(), OUTLINE_TEXT)
        );
        if chars(out) + chars(&line) > budget {
            let mut all = Vec::new();
            node.walk(&mut all);
            *skipped += all.len();
            continue;
        }
        out.push_str(&line);
        outline_nodes(out, node.children(), rects, depth + 1, budget, skipped);
    }
}

/// One slide as an outline, within `budget` characters in all.
fn slide_outline(deck: &Deck, path: &Path, index: usize, used: usize, budget: usize) -> String {
    let slide = &deck.slides[index];
    let mut out = format!(
        "slide {} (id {}) layout {}\n",
        index + 1,
        slide.id,
        name(&slide.layout.mode)
    );
    let mut skipped = 0;
    let mut body = String::new();
    let rects = rects_of(deck, slide, path);
    outline_nodes(
        &mut body,
        &slide.children,
        &rects,
        0,
        budget.saturating_sub(used + chars(&out) + 100),
        &mut skipped,
    );
    out.push_str(&body);
    if skipped > 0 {
        let _ = writeln!(
            out,
            "  … {skipped} more nodes on this slide: read it alone with slides:[{:?}]",
            slide.id
        );
    }
    out
}

/// Threads, open ones first, within `budget` characters.
fn thread_block(deck: &Deck, threads: &[Thread], budget: usize) -> String {
    let open = threads.iter().filter(|t| t.resolved.is_none()).count();
    let mut out = format!("comment threads: {} ({open} open)\n", threads.len());
    let mut ordered: Vec<&Thread> = threads.iter().collect();
    ordered.sort_by_key(|t| t.resolved.is_some());
    for (shown, thread) in ordered.iter().enumerate() {
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
        let mut entry = format!(
            "  {} · slide {slide}{block} · {} · {state} · {} messages · {}: {}\n",
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
                entry,
                "    latest ({}): {}",
                author(&last.role),
                clip(&last.text, 160)
            );
        }
        if chars(&out) + chars(&entry) > budget {
            let _ = writeln!(out, "  … {} more threads not shown", ordered.len() - shown);
            break;
        }
        out.push_str(&entry);
    }
    out
}

/// The `read` result. `selected` limits it to those slide indexes. A deck whose
/// full text would pass [`READ_BUDGET`] comes back as an outline with an
/// explicit "more slides" line, so the generic cap never decides what is cut.
pub(super) fn read_text(
    relative: &str,
    path: &Path,
    deck: &Deck,
    selected: Option<&[usize]>,
) -> Result<String> {
    let picked = selected.filter(|s| !s.is_empty());
    let indexes: Vec<usize> =
        picked.map_or_else(|| (0..deck.slides.len()).collect(), <[usize]>::to_vec);
    let mut head = header_text(relative, deck);
    if picked.is_some() {
        let numbers = indexes
            .iter()
            .map(|i| (i + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(head, "showing slides {numbers} of {}", deck.slides.len());
    }
    let full: Vec<String> = indexes.iter().map(|&i| slide_full(deck, path, i)).collect();
    let threads = feedback::load(path)?;
    let mut out = head.clone();
    let total = chars(&head) + full.iter().map(|s| chars(s)).sum::<usize>();
    if total <= READ_BUDGET {
        for block in &full {
            out.push_str(block);
        }
    } else {
        let _ = writeln!(
            out,
            "outline (the full tree is over {READ_BUDGET} characters): `id kind x,y wxh \"text\"`. Read slides:[ids or numbers] for styles, sizing and fonts."
        );
        let mut shown = 0;
        for &index in &indexes {
            let block = slide_outline(deck, path, index, chars(&out), READ_BUDGET);
            if shown > 0 && chars(&out) + chars(&block) > READ_BUDGET {
                break;
            }
            out.push_str(&block);
            shown += 1;
        }
        let rest = &indexes[shown..];
        if !rest.is_empty() {
            let ids = rest
                .iter()
                .take(MORE_IDS)
                .map(|i| format!("{:?}", deck.slides[*i].id))
                .collect::<Vec<_>>()
                .join(",");
            let more = if rest.len() > MORE_IDS { ",…" } else { "" };
            let _ = writeln!(
                out,
                "… {} more slides (slides {}-{}): read with slides:[{ids}{more}]",
                rest.len(),
                rest[0] + 1,
                rest[rest.len() - 1] + 1
            );
        }
    }
    let leftover = (RESULT_BUDGET + 100).saturating_sub(chars(&out)).max(500);
    out.push_str(&thread_block(deck, &threads, leftover));
    Ok(out)
}

/// A node without its children, to tell what changed on the node itself.
fn shallow(node: &Node) -> Node {
    let mut copy = node.clone();
    if let Some(children) = copy.children_mut() {
        children.clear();
    }
    copy
}

fn shell(slide: &Slide) -> Slide {
    let mut copy = slide.clone();
    copy.children.clear();
    copy
}

/// The nodes that are new or different after an edit, one line each with their
/// rectangles, grouped by slide, so the next edit needs no `read`.
pub(super) fn changed_summary(before: &Deck, after: &Deck, assets: &Path, budget: usize) -> String {
    let mut out = String::new();
    let mut hidden = 0;
    for (index, slide) in after.slides.iter().enumerate() {
        let old = before.slide(&slide.id);
        let rects = crate::deck::layout_map(after, slide, Some(assets));
        let mut lines = Vec::new();
        for node in slide.nodes() {
            let prior = before.node(&node.id);
            if prior.is_none_or(|p| shallow(p) != shallow(node)) {
                lines.push(format!(
                    "  {}\n",
                    node_summary(node, rects.get(&node.id).copied(), 60)
                ));
            }
        }
        let slide_changed = old.is_none_or(|o| shell(o) != shell(slide));
        if lines.is_empty() && !slide_changed {
            continue;
        }
        let mut block = format!(
            "slide {} (id {}){}\n",
            index + 1,
            slide.id,
            if old.is_none() { " added" } else { "" }
        );
        for line in lines {
            if chars(&out) + chars(&block) + chars(&line) > budget {
                hidden += 1;
            } else {
                block.push_str(&line);
            }
        }
        out.push_str(&block);
    }
    if hidden > 0 {
        let _ = writeln!(out, "… {hidden} more changed nodes: read with slides:[…]");
    }
    let removed: Vec<String> = before
        .slides
        .iter()
        .filter(|s| after.slide(&s.id).is_some())
        .flat_map(Slide::nodes)
        .filter(|n| after.node(&n.id).is_none())
        .map(|n| n.id.clone())
        .collect();
    if !removed.is_empty() {
        let shown: Vec<&str> = removed.iter().take(30).map(String::as_str).collect();
        let _ = writeln!(
            out,
            "removed: {}{}",
            shown.join(", "),
            if removed.len() > 30 {
                format!(", … {} more", removed.len() - 30)
            } else {
                String::new()
            }
        );
    }
    let gone: Vec<&str> = before
        .slides
        .iter()
        .filter(|s| after.slide(&s.id).is_none())
        .map(|s| s.id.as_str())
        .collect();
    if !gone.is_empty() {
        let _ = writeln!(out, "removed slides: {}", gone.join(", "));
    }
    out
}

/// What differs between the deck a caller last saw (`old`) and now
/// (`current`): node or slide id to a description of its present state, and
/// whether the theme changed.
pub(super) fn differences(
    old: &Deck,
    current: &Deck,
    assets: &Path,
) -> (BTreeMap<String, String>, bool) {
    let mut changed = BTreeMap::new();
    for slide in &current.slides {
        match old.slide(&slide.id) {
            None => {
                changed.insert(slide.id.clone(), "slide added".to_string());
            }
            Some(prior) if shell(prior) != shell(slide) => {
                changed.insert(slide.id.clone(), "slide settings changed".into());
            }
            Some(_) => {}
        }
        let rects = crate::deck::layout_map(current, slide, Some(assets));
        for node in slide.nodes() {
            let line = |tag: &str| {
                format!(
                    "{tag}{}",
                    node_summary(node, rects.get(&node.id).copied(), 80)
                )
            };
            match old.node(&node.id) {
                None => {
                    changed.insert(node.id.clone(), line("added: "));
                }
                Some(prior) if shallow(prior) != shallow(node) => {
                    changed.insert(node.id.clone(), line("now: "));
                }
                Some(_) => {}
            }
        }
    }
    for slide in &old.slides {
        if current.slide(&slide.id).is_none() {
            changed.insert(slide.id.clone(), "slide deleted".into());
        }
        for node in slide.nodes() {
            if current.node(&node.id).is_none() {
                changed.insert(node.id.clone(), "deleted".into());
            }
        }
    }
    (changed, old.theme != current.theme)
}
