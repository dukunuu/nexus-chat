//! Vector PDF assembly shared by decks and docs: each page is an SVG drawn
//! through `krilla-svg`, so text stays real, selectable and font-subsetted.
//! Nothing here reads the network, the clock or random state, so the same
//! input always produces the same bytes.
#![allow(clippy::cast_possible_truncation)]

use anyhow::{Context as _, Result, anyhow};
use krilla::Document;
use krilla::action::LinkAction;
use krilla::annotation::{Annotation, LinkAnnotation, Target};
use krilla::destination::XyzDestination;
use krilla::geom::{Point, Rect, Size};
use krilla::metadata::Metadata;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla_svg::{SurfaceExt as _, SvgSettings};
use resvg::usvg;

/// A clickable rectangle in page coordinates (top left origin, points).
pub(crate) struct Link {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub uri: String,
}

/// One PDF page: an SVG whose own size is scaled onto `size` points.
pub(crate) struct Sheet {
    pub svg: String,
    pub size: (f32, f32),
    pub links: Vec<Link>,
}

/// A bookmark; `level` starts at 1 and `page` is zero based.
pub(crate) struct Bookmark {
    pub level: u8,
    pub title: String,
    pub page: usize,
    pub y: f32,
}

fn tree(bookmarks: &[Bookmark]) -> Vec<OutlineNode> {
    let mut roots = Vec::new();
    let mut stack: Vec<(u8, OutlineNode)> = Vec::new();
    let close = |stack: &mut Vec<(u8, OutlineNode)>, roots: &mut Vec<OutlineNode>| {
        let (_, node) = stack.pop().expect("open outline node");
        if let Some((_, parent)) = stack.last_mut() {
            parent.push_child(node);
        } else {
            roots.push(node);
        }
    };
    for mark in bookmarks {
        while stack.last().is_some_and(|(level, _)| *level >= mark.level) {
            close(&mut stack, &mut roots);
        }
        let destination = XyzDestination::new(mark.page, Point::from_xy(0.0, mark.y));
        stack.push((
            mark.level,
            OutlineNode::new(mark.title.clone(), destination).with_open(true),
        ));
    }
    while !stack.is_empty() {
        close(&mut stack, &mut roots);
    }
    roots
}

/// Build the PDF. `options` carries the font database the SVGs were measured with.
pub(crate) fn assemble(
    sheets: &[Sheet],
    fonts: &std::sync::Arc<usvg::fontdb::Database>,
    title: &str,
    bookmarks: &[Bookmark],
) -> Result<Vec<u8>> {
    let options = usvg::Options {
        fontdb: fonts.clone(),
        ..usvg::Options::default()
    };
    let mut document = Document::new();
    for sheet in sheets {
        let (w, h) = sheet.size;
        let tree = usvg::Tree::from_str(&sheet.svg, &options).context("page SVG")?;
        let size = Size::from_wh(w, h).context("page size")?;
        let mut page = document.start_page_with(PageSettings::new(size));
        let mut surface = page.surface();
        surface
            .draw_svg(&tree, size, SvgSettings::default())
            .context("drawing page")?;
        surface.finish();
        for link in &sheet.links {
            if let Some(rect) = Rect::from_xywh(link.x, link.y, link.w, link.h) {
                page.add_annotation(Annotation::new_link(
                    LinkAnnotation::new(
                        rect,
                        Target::Action(LinkAction::new(link.uri.clone()).into()),
                    ),
                    None,
                ));
            }
        }
        page.finish();
    }
    let mut outline = Outline::new();
    for node in tree(bookmarks) {
        outline.push_child(node);
    }
    document.set_outline(outline);
    document.set_metadata(
        Metadata::new()
            .title(title.to_string())
            .creator("nexus studio".into())
            .producer("nexus".into()),
    );
    document
        .finish()
        .map_err(|error| anyhow!("PDF assembly failed: {error:?}"))
}

/// One vector page per slide at half the slide's pixel size, no speaker notes.
/// Animations appear in their final state, as in the other static exports.
pub fn export_pdf(deck: &super::super::Deck, assets: Option<&std::path::Path>) -> Result<Vec<u8>> {
    let size = deck.theme.size;
    let (svgs, fonts) = super::super::render::export_svgs(deck, assets);
    let sheets = svgs
        .into_iter()
        .map(|svg| Sheet {
            svg,
            size: (960.0, (size.h * 960.0 / size.w) as f32),
            links: Vec::new(),
        })
        .collect::<Vec<_>>();
    assemble(&sheets, &fonts, &deck.title, &[])
}
