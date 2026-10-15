//! TUI-only Mermaid previews. Layout, rasterization and graphics encoding run
//! on bounded background jobs; draw only places already-encoded images.
//! Sources and fixed-height preview slots remain in the transcript, so late
//! results cannot change its geometry or enter the provider's conversation.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use anyhow::{Context as _, Result, bail, ensure};
use image::DynamicImage;
use mermaid_rs_renderer::ir::DiagramKind;
use mermaid_rs_renderer::{RenderOptions, compute_layout, parse_mermaid_strict, render_svg};
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::{SlicedImage, SlicedProtocol};
use tokio::sync::mpsc;

/// Reserved even while a diagram streams or a renderer is still working.
pub const PREVIEW_ROWS: u16 = 12;
const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_SOURCE_LINES: usize = 128;
const MAX_JOBS: usize = 2;
const MAX_ENTRIES: usize = 8;
const MAX_PIXEL_WIDTH: u32 = 4096;
const MAX_PIXEL_HEIGHT: u32 = 1024;
static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
thread_local! {
    static IN_RENDER_JOB: Cell<bool> = const { Cell::new(false) };
}

/// Install after `ratatui::init`: its global panic hook otherwise restores
/// the terminal even for renderer panics that we catch in a worker. All other
/// panics retain the normal terminal restoration and reporting behavior.
pub fn protect_terminal_from_renderer_panics() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !IN_RENDER_JOB.get() {
            hook(info);
        }
    }));
}

fn render_safely<T>(render: impl FnOnce() -> Result<T>) -> Result<T, String> {
    let previous = IN_RENDER_JOB.replace(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(render));
    IN_RENDER_JOB.set(previous);
    result.map_or_else(
        |_| Err("diagram renderer failed; source is available below".into()),
        |result| result.map_err(|error| format!("{error:#}")),
    )
}

/// A preview region in the line-based transcript; its source is also retained
/// in the normal fenced-code copy metadata.
#[derive(Clone, Debug)]
pub struct Placement {
    pub row: usize,
    pub width: u16,
    pub source: String,
    pub complete: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    source: String,
    width: u16,
    dark: bool,
    generation: u64,
}

enum State {
    Pending,
    Ready(SlicedProtocol),
    Failed(String),
}

struct Entry {
    state: State,
    touched: u64,
}

/// Completion delivered only to the TUI event loop, never to core/host events.
pub struct Completion {
    key: Key,
    result: Result<SlicedProtocol, String>,
}

/// Small, disposable preview cache. No output files, network access, or Node.
pub struct DiagramCache {
    picker: Picker,
    entries: HashMap<Key, Entry>,
    sender: mpsc::UnboundedSender<Completion>,
    receiver: mpsc::UnboundedReceiver<Completion>,
    in_flight: usize,
    generation: u64,
    clock: u64,
}

impl Default for DiagramCache {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            picker: Picker::halfblocks(),
            entries: HashMap::new(),
            sender,
            receiver,
            in_flight: 0,
            generation: 0,
            clock: 0,
        }
    }
}

impl DiagramCache {
    /// Called after terminal initialization, before the terminal event reader.
    pub fn set_picker(&mut self, picker: Picker) {
        self.clear();
        self.picker = picker;
    }

    /// Discard previews on session/space changes. Running jobs still count
    /// toward the concurrency bound; their stale completions are ignored.
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
    }

    pub async fn next_completion(&mut self) -> Option<Completion> {
        self.receiver.recv().await
    }

    pub fn complete(&mut self, completion: Completion) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(entry) = self.entries.get_mut(&completion.key) {
            entry.state = match completion.result {
                Ok(image) => State::Ready(image),
                Err(error) => State::Failed(error),
            };
        }
    }

    /// Request only visible, closed fences, then paint the ready image or a
    /// bounded error. This never decodes, lays out, rasterizes or encodes.
    pub fn paint(
        &mut self,
        frame: &mut Frame,
        viewport: Rect,
        top: usize,
        placements: &[Placement],
        background: Color,
    ) {
        if viewport.width < 8 || viewport.height == 0 {
            return;
        }
        let dark = match background {
            Color::Rgb(r, g, b) => u32::from(r) + u32::from(g) + u32::from(b) < 384,
            Color::White | Color::Gray => false,
            _ => true,
        };
        for placement in placements {
            if !placement.complete
                || placement.row >= top + usize::from(viewport.height)
                || placement.row + usize::from(PREVIEW_ROWS) <= top
            {
                continue;
            }
            let area = Rect::new(
                viewport.x + 4, // assistant rail + code-box border/padding
                viewport.y,
                placement.width.min(viewport.width.saturating_sub(4)),
                viewport.height.min(32_767),
            );
            let y = if placement.row >= top {
                i16::try_from(placement.row - top).unwrap_or(i16::MAX)
            } else {
                -i16::try_from(top - placement.row).unwrap_or(i16::MAX)
            };
            if let Err(error) = validate_source(&placement.source) {
                paint_error(frame, area, y, &error.to_string());
                continue;
            }
            let key = Key {
                source: placement.source.clone(),
                width: area.width,
                dark,
                generation: self.generation,
            };
            self.request(&key);
            if let Some(entry) = self.entries.get_mut(&key) {
                self.clock = self.clock.wrapping_add(1);
                entry.touched = self.clock;
                match &entry.state {
                    State::Ready(image) => {
                        // The generic iTerm2 sliced widget cannot safely clip
                        // both ends at once. Keep source visible on tiny views.
                        if self.picker.protocol_type() == ProtocolType::Iterm2
                            && y < 0
                            && i32::from(y) + i32::from(image.size().height)
                                > i32::from(area.height)
                        {
                            continue;
                        }
                        frame.render_widget(SlicedImage::new(image, (0, y).into()), area);
                    }
                    State::Failed(error) => paint_error(frame, area, y, error),
                    State::Pending => {}
                }
            }
        }
    }

    fn request(&mut self, key: &Key) {
        if self.entries.contains_key(key) || self.in_flight >= MAX_JOBS || key.width == 0 {
            return;
        }
        // Synchronous TestBackend callers need no runtime and never launch
        // workers. The real TUI always has one.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.entries.len() >= MAX_ENTRIES {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| !matches!(entry.state, State::Pending))
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            } else {
                return;
            }
        }
        self.entries.insert(
            key.clone(),
            Entry {
                state: State::Pending,
                touched: self.clock,
            },
        );
        self.in_flight += 1;
        let key = key.clone();
        let picker = self.picker.clone();
        let sender = self.sender.clone();
        runtime.spawn_blocking(move || {
            let result = render_safely(|| prepare(&key.source, key.width, key.dark, &picker));
            let _ = sender.send(Completion { key, result });
        });
    }
}

fn validate_source(source: &str) -> Result<()> {
    ensure!(
        source.len() <= MAX_SOURCE_BYTES,
        "diagram exceeds 16 KiB; showing source"
    );
    ensure!(
        source.lines().count() <= MAX_SOURCE_LINES,
        "diagram exceeds 128 lines; showing source"
    );
    ensure!(
        !source
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')),
        "diagram contains control characters"
    );
    Ok(())
}

fn prepare(source: &str, width: u16, dark: bool, picker: &Picker) -> Result<SlicedProtocol> {
    let image = rasterize(source, width, dark, picker)?;
    SlicedProtocol::new(picker, image, Some(Size::new(width, PREVIEW_ROWS)))
        .context("encoding terminal diagram")
}

fn rasterize(source: &str, width: u16, dark: bool, picker: &Picker) -> Result<DynamicImage> {
    validate_source(source)?;
    let parsed = parse_mermaid_strict(source).context("invalid Mermaid")?;
    ensure!(
        matches!(
            parsed.graph.kind,
            DiagramKind::Flowchart | DiagramKind::Sequence | DiagramKind::State
        ),
        "preview supports flowcharts, sequence and state diagrams; showing source"
    );
    ensure!(
        parsed.graph.nodes.len() <= 64
            && parsed.graph.edges.len() <= 128
            && parsed.graph.subgraphs.len() <= 16,
        "diagram is too complex; showing source"
    );
    let mut options = RenderOptions::modern();
    if dark {
        options.theme = mermaid_rs_renderer::Theme::dark();
    }
    // Avoid this library's persistent font cache. The supported kinds use
    // this setting throughout layout; other kinds have unchecked font I/O.
    options.layout.fast_text_metrics = true;
    let layout = compute_layout(&parsed.graph, &options.theme, &options.layout);
    let svg = render_svg(&layout, &options.theme, &options.layout);
    ensure!(svg.len() <= 1024 * 1024, "diagram SVG exceeds 1 MiB");

    let svg_options = resvg::usvg::Options {
        // Never resolve model-authored local paths, data or remote images.
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: FONTS
            .get_or_init(|| {
                let mut fonts = resvg::usvg::fontdb::Database::new();
                fonts.load_system_fonts();
                Arc::new(fonts)
            })
            .clone(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(&svg, &svg_options).context("reading diagram SVG")?;
    let font = picker.font_size();
    let pixel_width = u32::from(width) * u32::from(font.width);
    let pixel_height = u32::from(PREVIEW_ROWS) * u32::from(font.height);
    ensure!(
        (1..=MAX_PIXEL_WIDTH).contains(&pixel_width)
            && (1..=MAX_PIXEL_HEIGHT).contains(&pixel_height),
        "terminal diagram dimensions exceed the pixel budget"
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel_width, pixel_height)
        .context("allocating diagram image")?;
    let shade = if dark { 51 } else { 255 };
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(
        shade, shade, shade, 255,
    ));
    let size = tree.size();
    #[allow(clippy::cast_precision_loss)] // bounded raster dimensions
    let scale = (pixel_width as f32 / size.width()).min(pixel_height as f32 / size.height());
    if !scale.is_finite() || scale <= 0.0 {
        bail!("invalid diagram dimensions");
    }
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia uses premultiplied RGBA; the image crate expects straight RGBA.
    let bytes = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    let image = image::RgbaImage::from_raw(pixel_width, pixel_height, bytes)
        .context("reading rasterized diagram")?;
    Ok(DynamicImage::ImageRgba8(image))
}

fn paint_error(frame: &mut Frame, area: Rect, y: i16, message: &str) {
    let skip = u16::try_from(-i32::from(y).min(0)).unwrap_or(PREVIEW_ROWS);
    let offset = u16::try_from(y.max(0)).unwrap_or(0);
    let height = PREVIEW_ROWS
        .saturating_sub(skip)
        .min(area.height.saturating_sub(offset));
    let visible = Rect::new(area.x, area.y + offset, area.width, height);
    frame.render_widget(
        Paragraph::new(format!("Mermaid preview unavailable: {message}"))
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: false })
            .scroll((skip, 0)),
        visible,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tested_diagram_kinds_rasterize_without_browser_or_output_files() {
        let picker = Picker::halfblocks();
        for source in [
            "flowchart LR\nA[Start] -->|next| B[End]",
            "flowchart TD\nsubgraph Client\nA[Input] --> B[Parse]\nend\nB --> C[Render]",
            "sequenceDiagram\nAlice->>Bob: Hello",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Running",
            "flowchart LR\nA[研究] --> B[結果]",
        ] {
            let image = rasterize(source, 60, true, &picker).expect(source);
            assert_eq!(image.width(), 600);
            assert_eq!(image.height(), 240);
            assert!(
                image
                    .to_rgba8()
                    .pixels()
                    .any(|pixel| pixel.0 != [51, 51, 51, 255])
            );
        }
    }

    #[test]
    fn prompt_advertised_mermaid_examples_render() {
        let picker = Picker::halfblocks();
        for source in nexus_core::app::MERMAID_PREVIEW_EXAMPLES {
            let image = rasterize(source, 60, true, &picker).expect(source);
            assert_eq!(image.width(), 600);
            assert_eq!(image.height(), 240);
        }
    }

    #[test]
    fn renderer_panics_are_marked_for_the_terminal_hook_and_become_errors() {
        assert!(!IN_RENDER_JOB.get());
        let result: Result<(), String> = render_safely(|| {
            assert!(IN_RENDER_JOB.get());
            panic!("simulated renderer panic");
        });
        assert!(result.unwrap_err().contains("source is available"));
        assert!(!IN_RENDER_JOB.get());
        assert!(render_safely(|| Ok(())).is_ok());
        assert!(!IN_RENDER_JOB.get());
    }

    #[test]
    fn budgets_and_unsupported_kinds_fail_with_source_fallback() {
        let picker = Picker::halfblocks();
        assert!(rasterize("not a diagram", 60, false, &picker).is_err());
        assert!(rasterize("flowchart LR\nA-->B", u16::MAX, false, &picker).is_err());
        assert!(rasterize("pie\n\"one\" : 1", 60, false, &picker).is_err());
        assert!(rasterize(&"x".repeat(MAX_SOURCE_BYTES + 1), 60, false, &picker).is_err());
        assert!(rasterize(&"x\n".repeat(MAX_SOURCE_LINES + 1), 60, false, &picker).is_err());
        assert!(rasterize("flowchart LR\nA[\x1b]", 60, false, &picker).is_err());
        let large = format!(
            "flowchart LR\n{}",
            (0..65).map(|i| format!("N{i}\n")).collect::<String>()
        );
        assert!(rasterize(&large, 60, false, &picker).is_err());
    }

    #[test]
    fn clipped_halfblock_previews_stay_inside_the_history_viewport() {
        let mut cache = DiagramCache::default();
        let source = "flowchart LR\nA-->B";
        let key = Key {
            source: source.into(),
            width: 20,
            dark: true,
            generation: 0,
        };
        let image = prepare(source, 20, true, &cache.picker).expect("preview");
        cache.entries.insert(
            key,
            Entry {
                state: State::Ready(image),
                touched: 0,
            },
        );
        let placement = Placement {
            source: source.into(),
            width: 20,
            row: 3,
            complete: true,
        };
        let viewport = Rect::new(2, 3, 30, 5);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 15)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Block::default().style(Style::default().bg(Color::Blue)),
                    frame.area(),
                );
                cache.paint(frame, viewport, 5, &[placement.clone()], Color::Black);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut pixels = 0;
        for y in 0..15 {
            for x in 0..40 {
                if buffer[(x, y)].symbol() == "▀" {
                    pixels += 1;
                    assert!(viewport.contains(ratatui::layout::Position::new(x, y)));
                } else if !viewport.contains(ratatui::layout::Position::new(x, y)) {
                    assert_eq!(buffer[(x, y)].bg, Color::Blue);
                }
            }
        }
        assert!(pixels > 0);
    }

    #[tokio::test]
    async fn cache_eviction_preserves_the_most_recent_previews() {
        let mut cache = DiagramCache::default();
        let keys: Vec<_> = (0..MAX_ENTRIES)
            .map(|i| Key {
                source: format!("flowchart LR\nA-->N{i}"),
                width: 60,
                dark: true,
                generation: 0,
            })
            .collect();
        for (i, key) in keys.iter().enumerate() {
            cache.entries.insert(
                key.clone(),
                Entry {
                    state: State::Failed("old".into()),
                    touched: u64::try_from(i).unwrap(),
                },
            );
        }
        let key = Key {
            source: "flowchart LR\nA-->New".into(),
            width: 60,
            dark: true,
            generation: 0,
        };
        cache.request(&key);
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(!cache.entries.contains_key(&keys[0]));
        assert!(cache.entries.contains_key(&keys[MAX_ENTRIES - 1]));
        let completion = cache.next_completion().await.expect("completion");
        cache.complete(completion);
    }

    #[tokio::test]
    async fn requests_deduplicate_and_stale_completions_do_not_return() {
        let mut cache = DiagramCache::default();
        let key = Key {
            source: "flowchart LR\nA-->B".into(),
            width: 60,
            dark: true,
            generation: 0,
        };
        cache.request(&key);
        cache.request(&key);
        assert_eq!(cache.in_flight, 1);
        cache.clear();
        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(10), cache.next_completion())
                .await
                .expect("renderer completed")
                .expect("completion");
        cache.complete(completion);
        assert_eq!(cache.in_flight, 0);
        assert!(cache.entries.is_empty());
    }

    #[tokio::test]
    async fn worker_concurrency_is_bounded() {
        let mut cache = DiagramCache::default();
        for i in 0..MAX_JOBS + 2 {
            cache.request(&Key {
                source: format!("flowchart LR\nA-->N{i}"),
                width: 60,
                dark: true,
                generation: 0,
            });
        }
        assert_eq!(cache.in_flight, MAX_JOBS);
        assert_eq!(cache.entries.len(), MAX_JOBS);
        for _ in 0..MAX_JOBS {
            let completion = cache.next_completion().await.expect("completion");
            cache.complete(completion);
        }
        assert_eq!(cache.in_flight, 0);
    }
}
