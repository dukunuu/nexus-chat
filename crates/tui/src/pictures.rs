//! Sharp pictures in the transcript. Reply pictures are laid out as
//! half-block rows (two pixels per cell, `ui::cards::image_to_halfblock_lines`)
//! that any terminal can show and that fix the picture's height. With Kitty
//! graphics (Ghostty, Kitty, `WezTerm`) the same cells are cleared and the
//! real picture is drawn over them at the terminal's own resolution: sent
//! once, then placeholder cells, clipped when it scrolls part-way off.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::{SlicedImage, SlicedProtocol};
use ratatui_image::{FilterType, Resize};
use unicode_width::UnicodeWidthStr;

/// The half-block glyph each picture cell is drawn with.
const PIXELS: &str = "\u{2580}";
/// Encoded pictures kept at once.
const PICTURES_MAX: usize = 16;

/// One picture's half-block block in the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    /// First transcript line.
    pub row: usize,
    /// Columns before the pixels (the assistant rail).
    pub x: u16,
    pub width: u16,
    pub height: u16,
    /// What `image_at_line` names: a URL or a local path.
    pub target: String,
}

fn pixels(line: &Line) -> bool {
    line.spans.iter().any(|span| span.content == PIXELS)
}

/// Picture blocks overlapping `visible`: runs of half-block lines with the
/// same target, including a run that starts above the view.
pub fn regions(lines: &[Line], targets: &[Option<String>], visible: Range<usize>) -> Vec<Region> {
    let picture = |index: usize| {
        lines.get(index).is_some_and(pixels) && targets.get(index).is_some_and(Option::is_some)
    };
    let mut index = visible.start;
    while index > 0 && picture(index) && picture(index - 1) && targets[index - 1] == targets[index]
    {
        index -= 1;
    }
    let mut found = Vec::new();
    while index < visible.end.min(lines.len()) {
        if !picture(index) {
            index += 1;
            continue;
        }
        let begin = index;
        while picture(index) && targets[index] == targets[begin] {
            index += 1;
        }
        let first = &lines[begin];
        let lead = first
            .spans
            .iter()
            .take_while(|span| span.content != PIXELS)
            .map(|span| span.content.width())
            .sum::<usize>();
        let width = first
            .spans
            .iter()
            .filter(|span| span.content == PIXELS)
            .count();
        found.push(Region {
            row: begin,
            x: u16::try_from(lead).unwrap_or(u16::MAX),
            width: u16::try_from(width).unwrap_or(u16::MAX),
            height: u16::try_from(index - begin).unwrap_or(u16::MAX),
            target: targets[begin].clone().unwrap_or_default(),
        });
    }
    found
}

/// Encoded pictures, keyed by file and cell size.
#[derive(Default)]
pub struct Pictures {
    /// Only a Kitty picker; elsewhere the half-blocks stay.
    picker: Option<Picker>,
    encoded: HashMap<(PathBuf, u16, u16), Option<SlicedProtocol>>,
}

impl Pictures {
    /// Draw real pictures when the terminal speaks the Kitty graphics protocol.
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = (picker.protocol_type() == ProtocolType::Kitty).then_some(picker);
        self.encoded.clear();
    }

    /// Cover each region's half-blocks in `viewport` with its picture. `top`
    /// is the first visible transcript line; `resolve` maps a target to a
    /// file on this device, or `None` while it is not there yet.
    pub fn paint(
        &mut self,
        buffer: &mut Buffer,
        viewport: Rect,
        top: usize,
        regions: &[Region],
        resolve: impl Fn(&str) -> Option<PathBuf>,
    ) {
        let Some(picker) = &self.picker else {
            return;
        };
        for region in regions {
            let Some(path) = resolve(&region.target) else {
                continue;
            };
            let x = viewport.x.saturating_add(region.x);
            let width = region.width.min(viewport.right().saturating_sub(x));
            if width == 0 {
                continue;
            }
            let key = (path, width, region.height);
            if self.encoded.len() >= PICTURES_MAX && !self.encoded.contains_key(&key) {
                self.encoded.clear();
            }
            let protocol = self.encoded.entry(key).or_insert_with_key(|(path, w, h)| {
                let image = image::open(path).ok()?;
                SlicedProtocol::new_with_resize(
                    picker,
                    image,
                    Size::new(*w, *h),
                    Resize::Fit(Some(FilterType::Lanczos3)),
                )
                .ok()
            });
            let Some(protocol) = protocol else {
                continue;
            };
            let offset = i64::try_from(region.row).unwrap_or(i64::MAX)
                - i64::try_from(top).unwrap_or(i64::MAX);
            let Ok(y) = i16::try_from(offset) else {
                continue;
            };
            let start = offset.max(0);
            let end = (offset + i64::from(region.height)).min(i64::from(viewport.height));
            for row in start..end {
                let row = viewport.y + u16::try_from(row).unwrap_or(0);
                for column in x..x + width {
                    buffer[(column, row)].reset();
                }
            }
            SlicedImage::new(protocol, (0, y).into())
                .render(Rect::new(x, viewport.y, width, viewport.height), buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn picture_line(lead: &str, width: usize) -> Line<'static> {
        let mut spans = vec![Span::raw(lead.to_string())];
        spans.extend((0..width).map(|_| Span::raw(PIXELS)));
        Line::from(spans)
    }

    #[test]
    fn regions_are_runs_of_half_blocks_with_one_target() {
        let a = Some("https://img.example/a.png".to_string());
        let b = Some("/tmp/b.png".to_string());
        let lines = vec![
            Line::from("text"),
            picture_line("▎ ", 6),
            picture_line("▎ ", 6),
            picture_line("▎ ", 6),
            Line::from("▣ click to open in viewer"),
            Line::from(""),
            picture_line("", 4),
        ];
        let targets = vec![None, a.clone(), a.clone(), a.clone(), a.clone(), None, b];
        let region = |row, x, width, height, target: &str| Region {
            row,
            x,
            width,
            height,
            target: target.into(),
        };
        assert_eq!(
            regions(&lines, &targets, 2..7),
            [
                region(1, 2, 6, 3, "https://img.example/a.png"),
                region(6, 0, 4, 1, "/tmp/b.png"),
            ],
            "a run above the view still starts at its first line"
        );
        assert!(regions(&lines, &targets, 4..6).is_empty());
    }

    #[test]
    fn kitty_pictures_replace_the_half_blocks_and_clip_at_the_top() {
        let dir = std::env::temp_dir().join(format!("nexus-picture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.png");
        image::RgbImage::from_pixel(40, 80, image::Rgb([20, 200, 90]))
            .save(&path)
            .unwrap();
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Kitty);
        let mut pictures = Pictures::default();
        pictures.set_picker(picker);
        let viewport = Rect::new(0, 0, 20, 4);
        let mut buffer = Buffer::empty(viewport);
        for y in 0..4 {
            buffer.set_string(0, y, "▎ ▀▀▀▀▀▀▀▀", ratatui::style::Style::default());
        }
        let region = Region {
            row: 8,
            x: 2,
            width: 8,
            height: 6,
            target: "p".into(),
        };
        // Lines 8..14, viewed from line 10: the top two rows are scrolled off.
        pictures.paint(&mut buffer, viewport, 10, &[region], |_| Some(path.clone()));
        let row: String = (2..10).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(!row.contains(PIXELS), "{row:?}");
        assert!(row.contains('\u{10EEEE}'), "kitty placeholders: {row:?}");
        assert_eq!(buffer[(0, 0)].symbol(), "▎", "the rail is untouched");

        let mut plain = Pictures::default();
        plain.set_picker(Picker::halfblocks());
        let mut untouched = Buffer::empty(viewport);
        untouched.set_string(0, 0, "▀▀", ratatui::style::Style::default());
        plain.paint(
            &mut untouched,
            viewport,
            0,
            &[Region {
                row: 0,
                x: 0,
                width: 2,
                height: 1,
                target: "p".into(),
            }],
            |_| Some(path.clone()),
        );
        assert_eq!(untouched[(0, 0)].symbol(), PIXELS, "no graphics, no change");
        let _ = std::fs::remove_dir_all(dir);
    }
}
