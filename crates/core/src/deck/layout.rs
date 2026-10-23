//! Auto-layout with `taffy`, measuring text with the renderer's own glyph
//! metrics. The editor lays the same trees out with CSS flexbox; this is the
//! mapping both sides follow:
//!
//! | Deck | Flexbox |
//! |---|---|
//! | `mode: row` / `column` | `display: flex`, `flex-direction`, `gap`, `padding` |
//! | `mode: none` | children are absolute; the parent's flow is empty |
//! | `wrap` | `flex-wrap: wrap`, `align-content: flex-start`, `gap_cross` as the line gap |
//! | `main` | `justify-content` (`space_between` is `space-between`) |
//! | `cross` | `align-items` (`stretch` also stretches `hug` children) |
//! | size `fixed(n)` | `width`/`height: n`; on the main axis `flex: 0 0 auto` |
//! | size `hug` | `auto`; on the main axis `flex: 0 1 auto` |
//! | size `fill` | main axis `flex: 1 1 0`; cross axis `align-self: stretch` |
//! | `min_*` / `max_*` | `min-width` ... `max-height` |
//! | `position: absolute{x,y}` | `position: absolute; left; top` (origin: parent's box, ignoring padding) |
//! | `fill` on an absolute node | the other edge is pinned at `0` |
//! | `clip` | `overflow: hidden` |
//! | `stroke` | drawn inside the box; takes no layout space |
//! | `hidden` | `display: none` |
//! | everything is `box-sizing: border-box` | |
//!
//! Text nodes measure with the typesetter: max-content is the widest unwrapped
//! line, min-content the longest word.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use taffy::geometry::Rect as TRect;
use taffy::prelude::*;
use taffy::{Overflow, Point, compute_leaf_layout};

use super::model::{
    AutoLayout, Cross, Deck, Dim, Kind, Main, Mode, Node, Position as Pos, Shape, Slide, round2,
};
use super::text::{TextStyle, Typesetter, code_stack};

/// A resolved box in slide pixels (relative to the slide's top left).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where an image `src` lives on disk, if it does. Never fetches.
pub(super) fn image_path(src: &str, assets: Option<&Path>) -> Option<PathBuf> {
    let root = assets?;
    if src.starts_with("https://") {
        return crate::remote_image::Fetcher::new(root).cached(src);
    }
    let path = root
        .join(src.strip_prefix("assets/").unwrap_or(src))
        .canonicalize()
        .ok()?;
    path.starts_with(root.canonicalize().ok()?).then_some(path)
}

/// Whether an image `src` can be drawn right now: a file in the deck's
/// `assets/` or an https image the fetcher already holds. Never fetches.
#[must_use]
pub fn image_resolves(src: &str, assets: Option<&Path>) -> bool {
    image_path(src, assets).is_some()
}

fn image_size(src: &str, assets: Option<&Path>) -> (f64, f64) {
    image_path(src, assets)
        .and_then(|path| {
            image::ImageReader::open(path)
                .ok()?
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()
        })
        .map_or((640.0, 360.0), |(w, h)| (f64::from(w), f64::from(h)))
}

/// What a leaf is measured as.
enum Leaf {
    Text {
        style: TextStyle,
        source: String,
    },
    Code {
        style: TextStyle,
        source: String,
    },
    Table {
        style: TextStyle,
        rows: Vec<Vec<String>>,
    },
    /// A natural size. With `ratio` (images) the other dimension follows the
    /// aspect ratio when one is known; charts and shapes size each axis alone.
    Natural(f64, f64, bool),
}

fn text_min_content(ts: &mut Typesetter<'_>, leaf: &Leaf) -> f64 {
    match leaf {
        Leaf::Text { style, source } => ts.markdown_min_width(style, source),
        Leaf::Code { style, source } => {
            ts.plain_min_width(style, source)
                + 2.0 * style.font.size * super::text::LAYOUT.code_padding
        }
        Leaf::Table { style, rows } => ts.table_min_width(style, rows),
        Leaf::Natural(..) => 0.0,
    }
}

fn content_at(ts: &mut Typesetter<'_>, leaf: &Leaf, width: f64) -> f64 {
    let mut scratch = String::new();
    match leaf {
        Leaf::Text { style, source } => {
            ts.markdown(&mut scratch, style, source, (0.0, 0.0, width), "")
                .0
        }
        Leaf::Code { style, source } => {
            ts.code(&mut scratch, style, source, (0.0, 0.0, width), 0.0, "")
                .0
        }
        Leaf::Table { style, rows } => ts.table(&mut scratch, style, rows, (0.0, 0.0, width), ""),
        Leaf::Natural(w, h, _) => width * h / w.max(1.0),
    }
}

fn max_content(ts: &mut Typesetter<'_>, leaf: &Leaf) -> (f64, f64) {
    let mut scratch = String::new();
    match leaf {
        Leaf::Text { style, source } => {
            let (h, w) = ts.markdown(&mut scratch, style, source, (0.0, 0.0, 1e7), "");
            (w, h)
        }
        Leaf::Code { style, source } => {
            let (h, w) = ts.code(&mut scratch, style, source, (0.0, 0.0, 1e7), 0.0, "");
            (w, h)
        }
        Leaf::Table { style, rows } => {
            let w = ts.table_width(style, rows);
            (w, content_at(ts, leaf, w))
        }
        Leaf::Natural(w, h, _) => (*w, *h),
    }
}

fn measure(
    ts: &mut Typesetter<'_>,
    leaf: &Leaf,
    known: Size<Option<f32>>,
    available: Size<AvailableSpace>,
) -> Size<f32> {
    if let (Some(width), Some(height)) = (known.width, known.height) {
        return Size { width, height };
    }
    if let Leaf::Natural(w, h, ratio) = leaf {
        let (w, h) = (*w as f32, *h as f32);
        // No automatic minimum: a chart or image may shrink as far as its parent says.
        let natural_w = if matches!(available.width, AvailableSpace::MinContent) {
            0.0
        } else {
            w
        };
        return match (known.width, known.height) {
            (Some(width), None) => Size {
                width,
                height: if *ratio { width * h / w.max(1.0) } else { h },
            },
            (None, Some(height)) => Size {
                width: if *ratio && natural_w > 0.0 {
                    height * w / h.max(1.0)
                } else {
                    natural_w
                },
                height,
            },
            _ => Size {
                width: natural_w,
                height: h,
            },
        };
    }
    let width = known.width.unwrap_or_else(|| {
        let max = max_content(ts, leaf).0 as f32;
        match available.width {
            AvailableSpace::MaxContent => max,
            AvailableSpace::MinContent => text_min_content(ts, leaf) as f32,
            AvailableSpace::Definite(space) => {
                max.min(space).max(text_min_content(ts, leaf) as f32)
            }
        }
    });
    let height = known
        .height
        .unwrap_or_else(|| content_at(ts, leaf, f64::from(width)) as f32);
    Size { width, height }
}

// -- style mapping ---------------------------------------------------------------

fn len(v: f64) -> LengthPercentage {
    length(v as f32)
}

fn dimension(d: Dim) -> Dimension {
    match d {
        Dim::Fixed(n) => length(n as f32),
        Dim::Hug | Dim::Fill => auto(),
    }
}

fn limit(v: Option<f64>) -> LengthPercentageAuto {
    v.map_or_else(auto, |n| length(n as f32))
}

fn container_style(style: &mut Style, layout: &AutoLayout, clip: bool) {
    style.flex_direction = match layout.mode {
        Mode::Column => FlexDirection::Column,
        Mode::Row | Mode::None => FlexDirection::Row,
    };
    style.flex_wrap = if layout.wrap && layout.mode != Mode::None {
        FlexWrap::Wrap
    } else {
        FlexWrap::NoWrap
    };
    let cross_gap = layout.gap_cross.unwrap_or(layout.gap);
    style.gap = if layout.mode == Mode::Column {
        Size {
            width: len(cross_gap),
            height: len(layout.gap),
        }
    } else {
        Size {
            width: len(layout.gap),
            height: len(cross_gap),
        }
    };
    let [top, right, bottom, left] = layout.padding;
    style.padding = TRect {
        left: len(left),
        right: len(right),
        top: len(top),
        bottom: len(bottom),
    };
    style.justify_content = match layout.main {
        Main::Start => JustifyContent::FLEX_START,
        Main::Center => JustifyContent::CENTER,
        Main::End => JustifyContent::FLEX_END,
        Main::SpaceBetween => JustifyContent::SPACE_BETWEEN,
    };
    style.align_items = match layout.cross {
        Cross::Start => AlignItems::FLEX_START,
        Cross::Center => AlignItems::CENTER,
        Cross::End => AlignItems::FLEX_END,
        Cross::Stretch => AlignItems::STRETCH,
    };
    if layout.wrap {
        style.align_content = AlignContent::FLEX_START;
    }
    if clip {
        style.overflow = Point {
            x: Overflow::Hidden,
            y: Overflow::Hidden,
        };
    }
}

/// The style of `node` as a child of a parent with `parent` layout.
fn node_style(node: &Node, parent: &AutoLayout) -> Style {
    let mut style = Style {
        display: if node.hidden {
            Display::None
        } else {
            Display::Flex
        },
        size: Size {
            width: dimension(node.sizing.w),
            height: dimension(node.sizing.h),
        },
        min_size: Size {
            width: limit(node.sizing.min_w),
            height: limit(node.sizing.min_h),
        },
        max_size: Size {
            width: limit(node.sizing.max_w),
            height: limit(node.sizing.max_h),
        },
        ..Style::default()
    };
    if let Kind::Frame { layout, clip, .. } = &node.kind {
        container_style(&mut style, layout, *clip);
    }
    let absolute = match (parent.mode, node.position) {
        (_, Pos::Absolute { x, y }) => Some((x, y)),
        (Mode::None, Pos::Auto) => Some((0.0, 0.0)),
        _ => None,
    };
    if let Some((x, y)) = absolute {
        style.position = Position::Absolute;
        style.inset = TRect {
            left: length(x as f32),
            top: length(y as f32),
            right: if node.sizing.w == Dim::Fill {
                length(0.0)
            } else {
                auto()
            },
            bottom: if node.sizing.h == Dim::Fill {
                length(0.0)
            } else {
                auto()
            },
        };
        return style;
    }
    let row = parent.mode == Mode::Row;
    let (main, cross) = if row {
        (node.sizing.w, node.sizing.h)
    } else {
        (node.sizing.h, node.sizing.w)
    };
    match main {
        Dim::Fixed(_) => {
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
        }
        Dim::Hug => {
            style.flex_grow = 0.0;
            style.flex_shrink = 1.0;
        }
        Dim::Fill => {
            style.flex_grow = 1.0;
            style.flex_shrink = 1.0;
            style.flex_basis = length(0.0);
        }
    }
    if cross == Dim::Fill {
        style.align_self = Some(AlignSelf::STRETCH);
    }
    style
}

struct Entry {
    id: String,
    taffy: NodeId,
    parent: Option<usize>,
}

struct Builder<'a, 'b> {
    tree: TaffyTree<Leaf>,
    ts: &'b mut Typesetter<'a>,
    assets: Option<&'b Path>,
    entries: Vec<Entry>,
}

impl Builder<'_, '_> {
    fn leaf_for(&mut self, node: &Node) -> Option<Leaf> {
        let theme = self.ts.theme;
        Some(match &node.kind {
            Kind::Frame { .. } => return None,
            Kind::Text { text, style, font } => Leaf::Text {
                style: self.ts.text_style(theme.resolve_font(style, font), false),
                source: text.clone(),
            },
            Kind::Code {
                source,
                style,
                font,
                ..
            } => {
                let font = theme.resolve_font(style, font);
                let mut text = self.ts.text_style(font.clone(), true);
                text.stack = code_stack(theme, &font.family);
                Leaf::Code {
                    style: text,
                    source: source.clone(),
                }
            }
            Kind::Table { rows, style, font } => Leaf::Table {
                style: self.ts.text_style(theme.resolve_font(style, font), false),
                rows: rows.clone(),
            },
            Kind::Image { src, .. } => {
                let (w, h) = image_size(src, self.assets);
                Leaf::Natural(w, h, true)
            }
            Kind::Chart { .. } => Leaf::Natural(640.0, 360.0, true),
            Kind::Shape { shape, .. } => match shape {
                Shape::Line | Shape::Arrow => Leaf::Natural(240.0, 40.0, false),
                _ => Leaf::Natural(160.0, 160.0, false),
            },
        })
    }

    fn add(&mut self, node: &Node, parent: &AutoLayout, parent_entry: Option<usize>) -> NodeId {
        let style = node_style(node, parent);
        let index = self.entries.len();
        // Children are added after this entry so ids list in tree order.
        self.entries.push(Entry {
            id: node.id.clone(),
            taffy: NodeId::new(0),
            parent: parent_entry,
        });
        let taffy = if let Kind::Frame {
            layout, children, ..
        } = &node.kind
        {
            let kids: Vec<NodeId> = children
                .iter()
                .map(|child| self.add(child, layout, Some(index)))
                .collect();
            self.tree
                .new_with_children(style, &kids)
                .expect("layout node")
        } else {
            let leaf = self.leaf_for(node).expect("leaf");
            self.tree
                .new_leaf_with_context(style, leaf)
                .expect("layout leaf")
        };
        self.entries[index].taffy = taffy;
        taffy
    }
}

/// Resolved rectangles for every node on `slide`, in tree order (parents
/// before children). Imported deck fonts are not loaded; use [`layout_at`].
#[must_use]
pub fn layout(deck: &Deck, slide: &Slide) -> Vec<(String, Rect)> {
    layout_at(deck, slide, None)
}

/// Like [`layout`], measuring with the deck's own `assets` folder: its
/// imported fonts and image sizes.
#[must_use]
pub fn layout_at(deck: &Deck, slide: &Slide, assets: Option<&Path>) -> Vec<(String, Rect)> {
    let mut ts = Typesetter::new(&deck.theme, assets);
    layout_with(&mut ts, deck, slide, assets)
}

pub(super) fn layout_with(
    ts: &mut Typesetter<'_>,
    deck: &Deck,
    slide: &Slide,
    assets: Option<&Path>,
) -> Vec<(String, Rect)> {
    let size = deck.theme.size;
    let mut builder = Builder {
        tree: TaffyTree::new(),
        ts,
        assets,
        entries: Vec::new(),
    };
    builder.tree.disable_rounding();
    let kids: Vec<NodeId> = slide
        .children
        .iter()
        .map(|child| builder.add(child, &slide.layout, None))
        .collect();
    let mut root_style = Style {
        display: Display::Flex,
        size: Size {
            width: length(size.w as f32),
            height: length(size.h as f32),
        },
        ..Style::default()
    };
    container_style(&mut root_style, &slide.layout, false);
    let root = builder
        .tree
        .new_with_children(root_style, &kids)
        .expect("slide node");
    let Builder {
        mut tree,
        ts,
        entries,
        ..
    } = builder;
    tree.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(size.w as f32),
            height: AvailableSpace::Definite(size.h as f32),
        },
        |inputs, _id, leaf, style| {
            compute_leaf_layout(
                inputs,
                style,
                |_, _| 0.0,
                |known, available| match leaf {
                    Some(leaf) => measure(ts, leaf, known, available),
                    None => Size::ZERO,
                },
            )
        },
    )
    .expect("layout");
    let mut absolute: Vec<(f64, f64)> = Vec::with_capacity(entries.len());
    let mut out = Vec::with_capacity(entries.len());
    for entry in &entries {
        let layout = tree.layout(entry.taffy).expect("node layout");
        let origin = entry.parent.map_or((0.0, 0.0), |p| absolute[p]);
        let x = origin.0 + f64::from(layout.location.x);
        let y = origin.1 + f64::from(layout.location.y);
        absolute.push((x, y));
        out.push((
            entry.id.clone(),
            Rect {
                x: round2(x),
                y: round2(y),
                w: round2(f64::from(layout.size.width)),
                h: round2(f64::from(layout.size.height)),
            },
        ));
    }
    out
}

/// `layout` as a lookup.
#[must_use]
pub fn layout_map(deck: &Deck, slide: &Slide, assets: Option<&Path>) -> HashMap<String, Rect> {
    layout_at(deck, slide, assets).into_iter().collect()
}

#[cfg(test)]
mod tests;
