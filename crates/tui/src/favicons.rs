//! Site icons over the link badges the markdown renderer puts before each
//! `https://` URL (`ui::markdown::badge_links`). With Kitty graphics
//! (Ghostty, Kitty, `WezTerm`) a badge becomes the site's real icon, drawn in
//! the badge's two cells; elsewhere the badge takes the icon's color. Icons
//! come from the shared, vetted image fetcher: this module never touches the
//! network, and an icon that is missing or still loading leaves the badge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nexus_core::remote_image::Status;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Modifier};
use ratatui::widgets::Widget;
use ratatui_image::Resize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;

use crate::ui::markdown::BADGE_GAP;

/// Decoded icons kept at once; a long session cycles through more.
const ICONS_MAX: usize = 256;

struct Icon {
    protocol: Option<Protocol>,
    color: Option<Color>,
}

/// Icons decoded for drawing, keyed by their cache file.
#[derive(Default)]
pub struct Favicons {
    /// Only a Kitty picker: its images are sent once and then drawn with
    /// placeholder cells, cheap enough for a screenful of icons.
    picker: Option<Picker>,
    icons: HashMap<PathBuf, Icon>,
}

impl Favicons {
    /// Use real icons when the terminal speaks the Kitty graphics protocol.
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = (picker.protocol_type() == ProtocolType::Kitty).then_some(picker);
    }

    /// Draw icons over the badges in `area`. `lookup` asks the fetcher for an
    /// origin's icon (scheduling the download); `graphics` is false while a
    /// popup dims the view, since dimming would corrupt Kitty placeholders.
    pub fn paint(
        &mut self,
        buffer: &mut Buffer,
        area: Rect,
        lookup: impl Fn(&str) -> Status,
        graphics: bool,
    ) {
        for (x, y, origin) in badges(buffer, area) {
            let Status::Ready(path) = lookup(&origin) else {
                continue;
            };
            if self.icons.len() >= ICONS_MAX && !self.icons.contains_key(&path) {
                self.icons.clear();
            }
            let picker = self.picker.as_ref();
            let icon = self
                .icons
                .entry(path)
                .or_insert_with_key(|path| load(path, picker));
            if graphics && let Some(protocol) = &icon.protocol {
                ratatui_image::Image::new(protocol).render(Rect::new(x, y, 2, 1), buffer);
            } else if let Some(color) = icon.color {
                buffer[(x, y)].fg = color;
            }
        }
    }
}

fn load(path: &Path, picker: Option<&Picker>) -> Icon {
    let Ok(image) = image::open(path) else {
        return Icon {
            protocol: None,
            color: None,
        };
    };
    let color = average(&image);
    let protocol = picker.and_then(|picker| {
        picker
            .new_protocol(image, Size::new(2, 1), Resize::Fit(None))
            .ok()
    });
    Icon { protocol, color }
}

/// The mean color of an icon's visible pixels.
fn average(image: &image::DynamicImage) -> Option<Color> {
    let pixels = image.to_rgba8();
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for pixel in pixels.pixels().filter(|pixel| pixel[3] >= 128) {
        for (total, channel) in sum.iter_mut().zip(pixel.0) {
            *total += u64::from(channel);
        }
        count += 1;
    }
    let channel = |total: u64| u8::try_from(total / count).unwrap_or(u8::MAX);
    (count > 0).then(|| Color::Rgb(channel(sum[0]), channel(sum[1]), channel(sum[2])))
}

/// Badges drawn in `area`: the badge's cell and the origin of the URL after
/// it. A badge is a reversed letter, a no-break space, then `https://`; a URL
/// whose host runs off the row is skipped, since its host may be cut.
fn badges(buffer: &Buffer, area: Rect) -> Vec<(u16, u16, String)> {
    let gap = BADGE_GAP.to_string();
    let mut found = Vec::new();
    for y in area.top()..area.bottom() {
        let cells: Vec<&str> = (area.left()..area.right())
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        for start in 0..cells.len().saturating_sub(10) {
            let x = area.x + u16::try_from(start).unwrap_or(0);
            if cells[start + 1] != gap
                || !buffer[(x, y)].modifier.contains(Modifier::REVERSED)
                || !cells[start].chars().all(|ch| ch.is_ascii_alphanumeric())
            {
                continue;
            }
            let mut url = String::new();
            let mut ended = false;
            for symbol in &cells[start + 2..] {
                if symbol
                    .chars()
                    .any(|ch| ch.is_whitespace() || "()[]<>\"',".contains(ch))
                {
                    ended = true;
                    break;
                }
                url.push_str(symbol);
            }
            let Some(rest) = url.strip_prefix("https://") else {
                continue;
            };
            let host_ends = rest.contains(['/', '?', '#']);
            if !(ended || host_ends) {
                continue;
            }
            if let Ok(parsed) = reqwest::Url::parse(&url)
                && parsed.host_str().is_some()
            {
                found.push((x, y, parsed.origin().ascii_serialization()));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    fn drawn(text: &str, width: u16) -> Buffer {
        let rendered = crate::ui::markdown::render(
            text,
            usize::from(width),
            crate::ui::markdown::MdColors {
                heading: Color::Blue,
                rule: Color::Gray,
                code: Color::White,
            },
        );
        let height = u16::try_from(rendered.lines.len()).unwrap();
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
        Paragraph::new(rendered.lines).render(buffer.area, &mut buffer);
        buffer
    }

    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .replace(BADGE_GAP, "_")
            .trim_end()
            .to_string()
    }

    #[test]
    fn links_get_a_letter_badge_that_wraps_with_the_url() {
        let buffer = drawn(
            "See [the docs](https://docs.rs/ratatui) and https://www.github.com/x, \
             not foohttps://bad.example or http://plain.example.",
            40,
        );
        let rows: Vec<_> = (0..buffer.area.height).map(|y| row(&buffer, y)).collect();
        let text = rows.join("\n");
        assert!(
            text.contains("the docs (D_https://docs.rs/ratatui)"),
            "{text}"
        );
        assert!(text.contains("G_https://www.github.com/x,"), "{text}");
        assert!(text.contains("foohttps://bad.example"), "{text}");
        assert!(!text.contains("_http://"), "{text}");
        assert_eq!(
            badges(&buffer, buffer.area)
                .into_iter()
                .map(|(_, _, origin)| origin)
                .collect::<Vec<_>>(),
            ["https://docs.rs", "https://www.github.com"]
        );
    }

    #[test]
    fn badges_need_the_reversed_style_and_a_whole_host() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 2));
        Paragraph::new(vec![
            Line::from(format!("A{BADGE_GAP}https://plain.example/x")),
            Line::styled(
                format!("B{BADGE_GAP}https://cut.examp"),
                Style::default().add_modifier(Modifier::REVERSED),
            ),
        ])
        .render(buffer.area, &mut buffer);
        assert!(
            badges(&buffer, Rect::new(0, 0, 40, 1)).is_empty(),
            "not reversed"
        );
        assert!(
            badges(&buffer, Rect::new(0, 1, 19, 1)).is_empty(),
            "the host may go on past the edge"
        );
        assert_eq!(badges(&buffer, Rect::new(0, 1, 40, 1)).len(), 1);
    }

    #[test]
    fn ready_icons_color_their_badge_without_graphics() {
        let dir = std::env::temp_dir().join(format!("nexus-favicon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("icon.png");
        image::RgbaImage::from_fn(4, 4, |x, _| {
            if x < 2 {
                image::Rgba([200, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        })
        .save(&path)
        .unwrap();
        let mut buffer = drawn("Read https://docs.rs/a and https://slow.example/b", 60);
        let mut favicons = Favicons::default();
        let area = buffer.area;
        favicons.paint(
            &mut buffer,
            area,
            |origin| {
                if origin == "https://docs.rs" {
                    Status::Ready(path.clone())
                } else {
                    Status::Loading
                }
            },
            true,
        );
        assert_eq!(buffer[(5, 0)].fg, Color::Rgb(200, 0, 0));
        let slow = row(&buffer, 0).find("S_").unwrap();
        assert_eq!(buffer[(u16::try_from(slow).unwrap(), 0)].fg, Color::Blue);
        let _ = std::fs::remove_dir_all(dir);
    }
}
