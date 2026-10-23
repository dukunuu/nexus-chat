//! Per-slide PNG archives and contact sheets, using the core renderer.

use super::super::{Deck, render};
use anyhow::{Result, ensure};
use std::path::Path;

/// A deterministic zip containing `slide-001.png`, … at the requested width.
pub fn export_png(deck: &Deck, assets: Option<&Path>, width: u32) -> Result<Vec<u8>> {
    ensure!((32..=6400).contains(&width), "PNG width must be 32 to 6400");
    let parts = deck
        .slides
        .iter()
        .enumerate()
        .map(|(i, slide)| {
            Ok((
                format!("slide-{:03}.png", i + 1),
                render::export_slide_at(deck, &slide.id, width, assets)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    super::zip(parts)
}

/// A single numbered contact sheet PNG, including local deck assets.
pub fn contact_sheet(deck: &Deck, assets: Option<&Path>) -> Result<Vec<u8>> {
    let ids = deck.slides.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
    render::sheet_at(deck, &ids, 3, assets)
}
