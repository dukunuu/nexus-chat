//! Vector PDF: every page's SVG goes through `krilla-svg`, with embedded,
//! subsetted fonts, selectable text, link annotations and heading bookmarks.
#![allow(clippy::cast_possible_truncation)]
use super::super::{
    Doc, Kind, inline,
    layout::{self, Paint},
    render,
};
use crate::deck::export::pdf::{Bookmark, Link, Sheet, assemble};
use anyhow::Result;
use std::path::Path;

/// Render the checked document to PDF bytes.
pub fn export(doc: &Doc, assets: Option<&Path>) -> Result<Vec<u8>> {
    let pages = layout::paginate(doc, assets);
    let sheets = pages
        .iter()
        .map(|page| Sheet {
            svg: render::svg(doc, page, assets),
            size: (page.width as f32, page.height as f32),
            links: page
                .items
                .iter()
                .filter_map(|item| match &item.paint {
                    Paint::Run(run) => run.link.as_ref().map(|uri| Link {
                        x: item.x as f32,
                        y: item.y as f32,
                        w: item.w as f32,
                        h: item.h as f32,
                        uri: uri.clone(),
                    }),
                    _ => None,
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let bookmarks = doc
        .blocks
        .iter()
        .filter_map(|block| {
            let Kind::Heading { level, .. } = &block.kind else {
                return None;
            };
            let (page, y) = pages.iter().enumerate().find_map(|(index, page)| {
                page.items
                    .iter()
                    .filter(|item| item.block == block.id)
                    .map(|item| item.y)
                    .reduce(f64::min)
                    .map(|y| (index, y))
            })?;
            Some(Bookmark {
                level: *level,
                title: block.plain_text(),
                page,
                y: (y - 6.0).max(0.0) as f32,
            })
        })
        .collect::<Vec<_>>();
    let fonts = crate::deck::text::deck_fonts(&inline::theme(doc), assets);
    assemble(&sheets, &fonts, &doc.title, &bookmarks)
}
