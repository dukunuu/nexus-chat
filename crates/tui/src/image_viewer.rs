//! The in-terminal image viewer: what opens where (terminal first, the OS
//! viewer as the fallback), the conversation's image order, and the
//! decode/encode work behind the full-screen popup.
//!
//! Decoding and graphics encoding run on bounded background jobs, like the
//! diagram previews: a draw only places an already-encoded image. Each image is
//! decoded once and downscaled to [`MAX_EDGE`], so a huge photo costs bounded
//! memory; resizing the terminal, toggling fit / 1:1 or panning re-encodes
//! from that decoded copy.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};
use nexus_core::app::Popup;
use ratatui::layout::{Rect, Size};
use ratatui_image::Resize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use tokio::sync::mpsc;

use crate::app_view::AppView;
use crate::interactive::PendingImage;

/// Longest edge kept after decoding; larger images are downscaled once.
pub const MAX_EDGE: u32 = 3072;
/// Files bigger than this are not read into memory for the viewer.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Vector images rasterize to this longest edge.
const SVG_EDGE: u32 = 2048;
/// Decode and encode jobs allowed in flight at once.
const MAX_JOBS: usize = 2;

// ── Where an image opens ──────────────────────────────────────────────────

/// The `image viewer` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preference {
    /// In the terminal when it draws real graphics, else the OS viewer.
    Auto,
    /// Always in the terminal, even with half-block rendering.
    Terminal,
    /// Always the OS viewer.
    System,
}

impl Preference {
    /// Unknown strings are `auto`, the setting's default.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "terminal" => Self::Terminal,
            "system" => Self::System,
            _ => Self::Auto,
        }
    }
}

/// What a target is, as far as the viewer can tell without decoding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    /// A video thumbnail whose sibling `.mp4` exists: plays in the OS player.
    Video,
    /// A format the `image` crate build here cannot decode.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Terminal,
    System,
}

/// The fallback decision table: the OS viewer for videos and for formats we
/// cannot decode; otherwise the setting decides, with `auto` choosing the
/// terminal only when it has a real graphics protocol.
#[must_use]
pub fn route(preference: Preference, protocol: ProtocolType, kind: Kind) -> Route {
    match (kind, preference) {
        (Kind::Video | Kind::Unsupported, _) | (_, Preference::System) => Route::System,
        (_, Preference::Auto) if protocol == ProtocolType::Halfblocks => Route::System,
        (_, Preference::Terminal | Preference::Auto) => Route::Terminal,
    }
}

#[must_use]
pub fn is_remote(target: &str) -> bool {
    target.starts_with("https://") || target.starts_with("http://")
}

/// `abc_first.png` / `abc_last.png` -> the sibling `abc.mp4` or
/// `_stitch_abc.mp4`, when one exists.
#[must_use]
pub fn video_for(target: &str) -> Option<PathBuf> {
    if is_remote(target) {
        return None;
    }
    let path = Path::new(target);
    let stem = path.file_stem()?.to_str()?;
    let base = stem
        .strip_suffix("_first")
        .or_else(|| stem.strip_suffix("_last"))?;
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    [
        dir.join(format!("{base}.mp4")),
        dir.join(format!("_stitch_{base}.mp4")),
    ]
    .into_iter()
    .find(|candidate| candidate.exists())
}

fn extension(target: &str) -> Option<String> {
    let path = if is_remote(target) {
        reqwest::Url::parse(target).ok()?.path().to_string()
    } else {
        target.to_string()
    };
    Path::new(&path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether `ext` names a raster or vector image (decodable or not).
#[must_use]
pub fn is_image_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("svg") || ImageFormat::from_extension(ext).is_some()
}

#[must_use]
pub fn kind_of(target: &str) -> Kind {
    if video_for(target).is_some() {
        return Kind::Video;
    }
    match extension(target) {
        Some(ext) if ext == "svg" => Kind::Image,
        Some(ext) => match ImageFormat::from_extension(&ext) {
            Some(format) if !format.reading_enabled() => Kind::Unsupported,
            _ => Kind::Image,
        },
        None => Kind::Image,
    }
}

/// The title: a file's name, or a URL's host plus the tail of its path.
#[must_use]
pub fn display_name(target: &str) -> String {
    if is_remote(target)
        && let Ok(url) = reqwest::Url::parse(target)
    {
        let host = url.host_str().unwrap_or_default();
        let segments: Vec<_> = url
            .path_segments()
            .map(|parts| parts.filter(|part| !part.is_empty()).collect())
            .unwrap_or_default();
        return match segments.as_slice() {
            [] => host.to_string(),
            [only] => format!("{host}/{only}"),
            [.., last] => format!("{host}/\u{2026}/{last}"),
        };
    }
    Path::new(target)
        .file_name()
        .map_or_else(|| target.to_string(), |name| name.to_string_lossy().into())
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)] // display only
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", value / 1024.0)
    } else {
        format!("{:.1} MB", value / (1024.0 * 1024.0))
    }
}

// ── The conversation's images, in history order ───────────────────────────

/// Where an item sits in the transcript, so a click lands on the right copy
/// of an image that appears more than once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The first rendered line of an inline image.
    Line(usize),
    /// The `index`th image of an interactive block.
    Widget {
        owner: usize,
        ordinal: usize,
        index: usize,
    },
    /// Not from the transcript: the first item with this target.
    ByTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// A file path or an `https` URL.
    pub target: String,
    pub anchor: Anchor,
}

/// Where the viewer returns to when it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Return {
    #[default]
    Nothing,
    Files,
    /// The focused interactive block it was opened from.
    Interactive,
}

// ── Decoding and encoding ─────────────────────────────────────────────────

/// What the footer says about the loaded image.
#[derive(Clone, Debug)]
pub struct Meta {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub bytes: u64,
    /// Set when the stored copy is smaller than the file.
    pub shown: Option<(u32, u32)>,
}

#[derive(Clone)]
pub struct Decoded {
    path: PathBuf,
    image: Arc<DynamicImage>,
    meta: Meta,
}

enum Load {
    Idle,
    Pending(PathBuf),
    Ready(Decoded),
    Failed(PathBuf),
}

/// What an encoded protocol was made for. Equal keys never re-encode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewKey {
    path: PathBuf,
    cols: u16,
    rows: u16,
    fit: bool,
    pan: (i32, i32),
}

struct Shown {
    key: ViewKey,
    protocol: Protocol,
}

/// Pixel sizes of the last view, for clamping and for deciding whether the
/// arrows pan or move between images.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    view: (u32, u32),
    image: (u32, u32),
}

pub enum Completion {
    Decoded {
        ticket: u64,
        path: PathBuf,
        result: Result<Decoded, String>,
    },
    Encoded {
        ticket: u64,
        key: ViewKey,
        result: Result<Protocol, String>,
    },
}

fn svg_like(path: &Path, data: &[u8]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
        || data[..data.len().min(512)]
            .windows(4)
            .any(|window| window == b"<svg")
}

fn rasterize_svg(data: &[u8]) -> Result<(DynamicImage, (u32, u32)), String> {
    let options = resvg::usvg::Options {
        // An image is untrusted input: never resolve paths, data or URLs in it.
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: nexus_core::deck::text::system_fonts(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(data, &options).map_err(|e| format!("svg: {e}"))?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !longest.is_finite() || longest <= 0.0 {
        return Err("svg has no size".into());
    }
    #[allow(clippy::cast_precision_loss)] // 2048 is exact
    let scale = SVG_EDGE as f32 / longest;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // bounded by SVG_EDGE
    let (width, height) = (
        ((size.width() * scale).round() as u32).max(1),
        ((size.height() * scale).round() as u32).max(1),
    );
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(width, height).ok_or("svg is too large to draw")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let bytes = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    let image = image::RgbaImage::from_raw(width, height, bytes).ok_or("svg raster failed")?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let natural = (
        size.width().round().max(1.0) as u32,
        size.height().round().max(1.0) as u32,
    );
    Ok((DynamicImage::ImageRgba8(image), natural))
}

fn format_name(format: ImageFormat) -> String {
    match format {
        ImageFormat::Jpeg => "JPEG".into(),
        ImageFormat::WebP => "WEBP".into(),
        other => format!("{other:?}").to_ascii_uppercase(),
    }
}

/// Read, decode and (once) downscale a file. Runs off the draw path.
fn decode(path: &Path) -> Result<Decoded, String> {
    let bytes = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if bytes > MAX_FILE_BYTES {
        return Err(format!(
            "file is {} (viewer limit 64.0 MB)",
            human_bytes(bytes)
        ));
    }
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let (image, format, natural) = if svg_like(path, &data) {
        let (image, natural) = rasterize_svg(&data)?;
        (image, "SVG".to_string(), natural)
    } else {
        let reader = image::ImageReader::new(std::io::Cursor::new(&data))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        let format = reader.format().ok_or("unrecognized image format")?;
        if !format.reading_enabled() {
            return Err(format!("{format:?} images can't be decoded here"));
        }
        let image = reader.decode().map_err(|e| e.to_string())?;
        let natural = image.dimensions();
        (image, format_name(format), natural)
    };
    let (width, height) = image.dimensions();
    let image = if width.max(height) > MAX_EDGE {
        image.resize(MAX_EDGE, MAX_EDGE, FilterType::Triangle)
    } else {
        image
    };
    let stored = image.dimensions();
    Ok(Decoded {
        path: path.to_path_buf(),
        meta: Meta {
            width: natural.0,
            height: natural.1,
            format,
            bytes,
            shown: (stored != natural).then_some(stored),
        },
        image: Arc::new(image),
    })
}

/// Fit: scale down to the view. 1:1: cut the view-sized window at `pan`
/// (pixels from centered) out of the pixels, untouched.
fn encode(
    picker: &Picker,
    image: &DynamicImage,
    cols: u16,
    rows: u16,
    fit: bool,
    pan: (i32, i32),
) -> Result<Protocol, String> {
    let font = picker.font_size();
    let view = (
        u32::from(cols) * u32::from(font.width),
        u32::from(rows) * u32::from(font.height),
    );
    let (width, height) = image.dimensions();
    let source = if fit {
        if width > view.0 || height > view.1 {
            image.resize(view.0.max(1), view.1.max(1), FilterType::Triangle)
        } else {
            image.clone()
        }
    } else {
        let origin = |size: u32, window: u32, pan: i32| -> u32 {
            if size <= window {
                0
            } else {
                let centered = i64::from((size - window) / 2) + i64::from(pan);
                u32::try_from(centered.clamp(0, i64::from(size - window))).unwrap_or(0)
            }
        };
        image.crop_imm(
            origin(width, view.0, pan.0),
            origin(height, view.1, pan.1),
            width.min(view.0),
            height.min(view.1),
        )
    };
    picker
        .new_protocol(source, Size::new(cols, rows), Resize::Fit(None))
        .map_err(|e| e.to_string())
}

fn clamp_pan(pan: i32, image: u32, view: u32) -> i32 {
    let half = i32::try_from(image.saturating_sub(view) / 2).unwrap_or(i32::MAX);
    pan.clamp(-half, half)
}

// ── The opener (injectable, so tests never launch a viewer) ───────────────

#[derive(Clone)]
pub struct Opener(Arc<dyn Fn(&str) + Send + Sync>);

impl Opener {
    pub fn new(open: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(open))
    }

    #[must_use]
    pub fn system() -> Self {
        Self::new(|target| {
            let _ = open::that_detached(target);
        })
    }

    pub fn call(&self, target: &str) {
        (self.0)(target);
    }
}

// ── The viewer ────────────────────────────────────────────────────────────

/// What a target resolves to right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Ready(PathBuf),
    Loading,
    Failed,
    /// Remote images are switched off.
    Off,
    Missing,
}

#[must_use]
pub fn source_of(target: &str) -> Source {
    if is_remote(target) {
        match crate::app_view::remote_image(target) {
            nexus_core::remote_image::Status::Ready(path) => Source::Ready(path),
            nexus_core::remote_image::Status::Loading => Source::Loading,
            nexus_core::remote_image::Status::Failed => Source::Failed,
            nexus_core::remote_image::Status::Off => Source::Off,
        }
    } else {
        let path = PathBuf::from(target);
        if path.is_file() {
            Source::Ready(path)
        } else {
            Source::Missing
        }
    }
}

pub struct Viewer {
    picker: Picker,
    items: Vec<Item>,
    index: usize,
    fit: bool,
    pan: (i32, i32),
    return_to: Return,
    load: Load,
    shown: Option<Shown>,
    requested: Option<ViewKey>,
    ticket: u64,
    in_flight: usize,
    failures: Vec<String>,
    geometry: Option<Geometry>,
    tx: mpsc::UnboundedSender<Completion>,
    rx: mpsc::UnboundedReceiver<Completion>,
    opener: Opener,
}

impl Default for Viewer {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            picker: Picker::halfblocks(),
            items: Vec::new(),
            index: 0,
            fit: true,
            pan: (0, 0),
            return_to: Return::Nothing,
            load: Load::Idle,
            shown: None,
            requested: None,
            ticket: 0,
            in_flight: 0,
            failures: Vec::new(),
            geometry: None,
            tx,
            rx,
            // Tests must never launch a real viewer.
            #[cfg(test)]
            opener: Opener::new(|_| {}),
            #[cfg(not(test))]
            opener: Opener::system(),
        }
    }
}

impl Viewer {
    /// Called once after the terminal is initialised, with the probed picker.
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = picker;
        self.shown = None;
        self.requested = None;
    }

    #[must_use]
    pub fn protocol(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    #[cfg(test)]
    pub fn set_opener(&mut self, opener: Opener) {
        self.opener = opener;
    }

    /// Open `target` (or its sibling video) in the OS viewer.
    pub fn open_external(&self, target: &str) {
        let resolved = video_for(target);
        self.opener.call(
            &resolved
                .as_deref()
                .map_or_else(|| target.to_string(), |p| p.to_string_lossy().into_owned()),
        );
    }

    /// Begin viewing `items[index]`; fit mode, centered.
    pub fn start(&mut self, items: Vec<Item>, index: usize, return_to: Return) {
        self.index = index.min(items.len().saturating_sub(1));
        self.items = items;
        self.return_to = return_to;
        self.fit = true;
        self.reset_item();
    }

    fn reset_item(&mut self) {
        self.ticket = self.ticket.wrapping_add(1);
        self.load = Load::Idle;
        self.shown = None;
        self.requested = None;
        self.geometry = None;
        self.pan = (0, 0);
    }

    /// Drop the decoded image and anything in flight.
    pub fn close(&mut self) -> Return {
        self.ticket = self.ticket.wrapping_add(1);
        self.load = Load::Idle;
        self.shown = None;
        self.requested = None;
        self.items.clear();
        std::mem::take(&mut self.return_to)
    }

    #[must_use]
    pub fn return_to(&self) -> Return {
        self.return_to
    }

    #[must_use]
    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.index)
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    #[must_use]
    pub fn is_fit(&self) -> bool {
        self.fit
    }

    #[cfg(test)]
    #[must_use]
    pub fn pan(&self) -> (i32, i32) {
        self.pan
    }

    pub fn toggle_fit(&mut self) {
        self.fit = !self.fit;
        self.pan = (0, 0);
        self.requested = None;
    }

    /// Move to a neighbouring image; stays put at either end.
    pub fn step(&mut self, delta: isize) -> bool {
        let last = self.items.len().saturating_sub(1);
        let next = self.index.saturating_add_signed(delta).min(last);
        if next == self.index {
            return false;
        }
        self.index = next;
        self.reset_item();
        true
    }

    pub fn go(&mut self, index: usize) {
        let next = index.min(self.items.len().saturating_sub(1));
        if next != self.index {
            self.index = next;
            self.reset_item();
        }
    }

    /// Whether the 1:1 view is larger than the window on (x, y).
    #[must_use]
    pub fn can_pan(&self) -> (bool, bool) {
        match (self.fit, self.geometry) {
            (false, Some(g)) => (g.image.0 > g.view.0, g.image.1 > g.view.1),
            _ => (false, false),
        }
    }

    /// Pan by whole cells (the window shows more of the image in that direction).
    pub fn pan_by(&mut self, dx: i32, dy: i32) {
        let font = self.picker.font_size();
        self.pan.0 = self
            .pan
            .0
            .saturating_add(dx.saturating_mul(i32::from(font.width)));
        self.pan.1 = self
            .pan
            .1
            .saturating_add(dy.saturating_mul(i32::from(font.height)));
    }

    // ── Background work ──────────────────────────────────────────────────

    /// The next finished job, for the event loop.
    pub async fn next_completion(&mut self) -> Option<Completion> {
        self.rx.recv().await
    }

    fn spawn(&mut self, job: impl FnOnce() -> Completion + Send + 'static) {
        self.in_flight += 1;
        // Synchronous callers (the snapshot tests) have no runtime: run the
        // job inline. The real TUI always has one.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let tx = self.tx.clone();
            runtime.spawn_blocking(move || {
                let _ = tx.send(job());
            });
        } else {
            let _ = self.tx.send(job());
        }
    }

    /// Apply one finished job. A decode or encode error is queued as a failure
    /// for the app to turn into the OS-viewer fallback.
    pub fn apply(&mut self, completion: Completion) {
        self.in_flight = self.in_flight.saturating_sub(1);
        match completion {
            Completion::Decoded {
                ticket,
                path,
                result,
            } if ticket == self.ticket => match result {
                Ok(decoded) => self.load = Load::Ready(decoded),
                Err(error) => {
                    self.load = Load::Failed(path);
                    self.failures.push(error);
                }
            },
            Completion::Encoded {
                ticket,
                key,
                result,
            } if ticket == self.ticket => {
                if self.requested.as_ref() == Some(&key) {
                    self.requested = None;
                }
                match result {
                    Ok(protocol) => self.shown = Some(Shown { key, protocol }),
                    Err(error) => self.failures.push(error),
                }
            }
            _ => {}
        }
    }

    fn absorb(&mut self) {
        while let Ok(completion) = self.rx.try_recv() {
            self.apply(completion);
        }
    }

    /// Absorb finished jobs and hand back any failures since the last call.
    pub fn drain(&mut self) -> Vec<String> {
        self.absorb();
        std::mem::take(&mut self.failures)
    }

    /// Start whatever the current view still needs: decode the file, then
    /// encode it for `body`. Idempotent; bounded by [`MAX_JOBS`].
    pub fn drive(&mut self, source: &Source, body: Rect) {
        let Source::Ready(path) = source else {
            return;
        };
        if body.width == 0 || body.height == 0 {
            return;
        }
        // Inline (runtime-less) jobs finish inside the call, so a second pass
        // can start the encode that follows the decode.
        for _ in 0..2 {
            let current = match &self.load {
                Load::Idle => None,
                Load::Pending(p) | Load::Failed(p) => Some(p.as_path()),
                Load::Ready(d) => Some(d.path.as_path()),
            };
            if current != Some(path.as_path()) {
                if self.in_flight >= MAX_JOBS {
                    return;
                }
                self.load = Load::Pending(path.clone());
                self.shown = None;
                self.requested = None;
                let (ticket, target) = (self.ticket, path.clone());
                self.spawn(move || Completion::Decoded {
                    ticket,
                    result: decode(&target),
                    path: target,
                });
            } else if let Load::Ready(decoded) = &self.load {
                let image = Arc::clone(&decoded.image);
                let key = self.view_key(path, &image, body);
                let done = self.shown.as_ref().is_some_and(|shown| shown.key == key)
                    || self.requested.as_ref() == Some(&key);
                if !done && self.in_flight < MAX_JOBS {
                    self.requested = Some(key.clone());
                    let (ticket, picker) = (self.ticket, self.picker.clone());
                    self.spawn(move || Completion::Encoded {
                        ticket,
                        result: encode(&picker, &image, key.cols, key.rows, key.fit, key.pan),
                        key,
                    });
                }
            }
            self.absorb();
            if matches!(self.load, Load::Pending(_)) {
                break;
            }
        }
    }

    fn view_key(&mut self, path: &Path, image: &DynamicImage, body: Rect) -> ViewKey {
        let font = self.picker.font_size();
        let view = (
            u32::from(body.width) * u32::from(font.width),
            u32::from(body.height) * u32::from(font.height),
        );
        let size = image.dimensions();
        self.geometry = Some(Geometry { view, image: size });
        if self.fit {
            self.pan = (0, 0);
        } else {
            self.pan = (
                clamp_pan(self.pan.0, size.0, view.0),
                clamp_pan(self.pan.1, size.1, view.1),
            );
        }
        ViewKey {
            path: path.to_path_buf(),
            cols: body.width,
            rows: body.height,
            fit: self.fit,
            pan: self.pan,
        }
    }

    // ── What the popup draws ─────────────────────────────────────────────

    /// The encoded image for the current view, if one is ready.
    #[must_use]
    pub fn protocol_ready(&self) -> Option<&Protocol> {
        self.shown.as_ref().map(|shown| &shown.protocol)
    }

    #[must_use]
    pub fn meta(&self) -> Option<&Meta> {
        match &self.load {
            Load::Ready(decoded) => Some(&decoded.meta),
            _ => None,
        }
    }

    /// The footer fields after the separators: size, format, bytes, mode.
    #[must_use]
    pub fn info_fields(&self) -> Vec<String> {
        let mut fields = Vec::new();
        if let Some(meta) = self.meta() {
            fields.push(format!("{}x{}", meta.width, meta.height));
            fields.push(meta.format.clone());
            fields.push(human_bytes(meta.bytes));
            if let Some((w, h)) = meta.shown {
                fields.push(format!("shown at {w}x{h}"));
            }
        }
        fields.push(if self.fit { "fit".into() } else { "1:1".into() });
        fields
    }
}

// ── AppView: opening, closing, falling back ───────────────────────────────

impl AppView {
    /// Every image in the conversation in history order: inline and tool-card
    /// images by their line, interactive-block images by their block's row.
    #[must_use]
    pub fn image_gallery(&self) -> Vec<Item> {
        let mut entries: Vec<(usize, usize, Item)> = Vec::new();
        let mut previous: Option<&str> = None;
        for (line, target) in self.history_cache.image_at_line.iter().enumerate() {
            match target {
                Some(target) if previous != Some(target.as_str()) => {
                    previous = Some(target);
                    entries.push((
                        line,
                        0,
                        Item {
                            target: target.clone(),
                            anchor: Anchor::Line(line),
                        },
                    ));
                }
                Some(_) => {}
                None => previous = None,
            }
        }
        for placement in self.history_cache.widgets.iter().filter(|p| p.complete) {
            let Ok(document) = nexus_core::interactive::Document::parse(&placement.source) else {
                continue;
            };
            for (index, image) in crate::interactive::document_images(&document)
                .into_iter()
                .enumerate()
            {
                entries.push((
                    placement.row,
                    index + 1,
                    Item {
                        target: image.url,
                        anchor: Anchor::Widget {
                            owner: placement.owner,
                            ordinal: placement.ordinal,
                            index,
                        },
                    },
                ));
            }
        }
        entries.sort_by_key(|(row, sub, _)| (*row, *sub));
        entries.into_iter().map(|(_, _, item)| item).collect()
    }

    fn viewer_preference(&self) -> Preference {
        Preference::parse(&self.core.settings.image_viewer)
    }

    /// Open `target`: in the terminal viewer when the setting and the
    /// terminal allow it, else (or for videos and formats we cannot decode)
    /// in the OS viewer.
    pub fn open_image_target(&mut self, target: &str, anchor: &Anchor, from: Return) {
        let kind = kind_of(target);
        match route(self.viewer_preference(), self.image_viewer.protocol(), kind) {
            Route::System => {
                self.image_viewer.open_external(target);
                if kind == Kind::Unsupported {
                    self.push_status(format!(
                        "{}: can't be shown in the terminal \u{2014} opened in the system viewer",
                        display_name(target)
                    ));
                }
            }
            Route::Terminal => {
                let items = self.image_gallery();
                let found = match anchor {
                    Anchor::Line(line) => items.iter().rposition(|item| {
                        matches!(item.anchor, Anchor::Line(start) if start <= *line)
                            && item.target == target
                    }),
                    Anchor::Widget { .. } => items.iter().position(|item| item.anchor == *anchor),
                    Anchor::ByTarget => items.iter().position(|item| item.target == target),
                };
                let (items, index) = found.map_or_else(
                    || {
                        (
                            vec![Item {
                                target: target.to_string(),
                                anchor: Anchor::ByTarget,
                            }],
                            0,
                        )
                    },
                    |index| (items, index),
                );
                self.image_viewer.start(items, index, from);
                self.popup = Popup::ImageViewer;
            }
        }
    }

    /// Click or Enter on a rendered image line of the transcript.
    pub fn open_image_at_line(&mut self, line: usize) -> bool {
        let Some(Some(target)) = self.history_cache.image_at_line.get(line).cloned() else {
            return false;
        };
        self.open_image_target(&target, &Anchor::Line(line), Return::Nothing);
        true
    }

    /// An image inside an interactive block.
    pub fn open_widget_image(&mut self, pending: &PendingImage, from: Return) {
        let anchor = Anchor::Widget {
            owner: pending.owner,
            ordinal: pending.ordinal,
            index: pending.index,
        };
        self.open_image_target(&pending.url, &anchor, from);
    }

    /// `/view <path|url>`: a path as given, `~`-expanded, or relative to the
    /// space's files.
    pub fn open_image_command(&mut self, argument: &str) {
        let argument = argument.trim();
        let target = if is_remote(argument) {
            argument.to_string()
        } else {
            let expanded = match argument.strip_prefix("~/") {
                Some(rest) => std::env::home_dir().map(|home| home.join(rest)),
                None => Some(PathBuf::from(argument)),
            };
            let in_space = self
                .core
                .space
                .files_dir(&self.core.active_space.name)
                .join(argument);
            let Some(path) = expanded
                .into_iter()
                .chain([in_space])
                .find(|path| path.is_file())
            else {
                self.push_status(format!("no such image: {argument}"));
                return;
            };
            path.to_string_lossy().into_owned()
        };
        self.open_image_target(&target, &Anchor::ByTarget, Return::Nothing);
    }

    /// Close the popup and go back to where it was opened from.
    pub fn close_image_viewer(&mut self) {
        let back = self.image_viewer.close();
        self.popup = match back {
            Return::Files => Popup::Files,
            Return::Interactive if self.interactive.is_focused() => Popup::InteractiveUi,
            Return::Interactive | Return::Nothing => Popup::None,
        };
    }

    /// The explicit fallback: open the current item in the OS viewer/player.
    pub fn open_viewer_item_externally(&mut self) {
        let Some(item) = self.image_viewer.current().cloned() else {
            return;
        };
        self.image_viewer.open_external(&item.target);
        self.push_status(format!("opened {}", display_name(&item.target)));
    }

    /// Take finished decode/encode jobs; a failure closes the viewer and opens
    /// the file in the OS viewer with a status note.
    pub fn pump_image_viewer(&mut self) {
        if self.popup != Popup::ImageViewer {
            return;
        }
        if let Some(reason) = self.image_viewer.drain().into_iter().next() {
            let Some(item) = self.image_viewer.current().cloned() else {
                return;
            };
            self.close_image_viewer();
            self.image_viewer.open_external(&item.target);
            self.push_status(format!(
                "{}: {reason} \u{2014} opened in the system viewer",
                display_name(&item.target)
            ));
        }
    }

    /// A job finished while the event loop was idle.
    pub fn apply_image_completion(&mut self, completion: Completion) {
        self.image_viewer.apply(completion);
        self.pump_image_viewer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactive::Placement;
    use nexus_core::app::App;
    use nexus_core::db::Db;
    use nexus_core::space::Space;
    use serde_json::json;
    use std::sync::Mutex;

    const PIC: &str = "https://img.example.invalid/pic.png";
    const PIC2: &str = "https://img.example.invalid/second.png";

    fn app() -> AppView {
        let root = std::env::temp_dir().join(format!("nexus-viewer-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        AppView::new(App::new(
            Db::open_in_memory().unwrap(),
            Some("k"),
            Space { root },
        ))
    }

    /// An app whose opener records what it was asked to open.
    fn recording_app() -> (AppView, Arc<Mutex<Vec<String>>>) {
        let mut app = app();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        app.image_viewer.set_opener(Opener::new(move |target| {
            sink.lock().unwrap().push(target.into())
        }));
        (app, log)
    }

    fn picker(protocol: ProtocolType) -> Picker {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(protocol);
        picker
    }

    fn png_file(dir: &Path, name: &str, width: u32, height: u32) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x * 4 % 256) as u8, (y * 8 % 256) as u8, 120])
        })
        .save(&path)
        .unwrap();
        path.to_string_lossy().into_owned()
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("nexus-viewer-files-{}", uuid::Uuid::new_v4()))
    }

    fn gallery_doc(images: &[&str]) -> String {
        let view: Vec<_> = images
            .iter()
            .map(|src| json!({"type": "image", "src": src}))
            .collect();
        json!({"version": 1, "title": "Gallery", "view": view}).to_string()
    }

    /// The status line after the queued events have landed, as the event
    /// loop applies them before each draw.
    fn status(app: &mut AppView) -> String {
        while let Some(event) = app.pop_pending_event() {
            app.apply_event(&event);
        }
        app.status.clone()
    }

    fn key(app: &mut AppView, code: crossterm::event::KeyCode) {
        crate::ui::popups::image_viewer::handle_key(app, crossterm::event::KeyEvent::from(code));
    }

    // ── fallback decision table ──────────────────────────────────────────

    #[test]
    fn fallback_table_protocol_by_setting_by_kind() {
        use Preference::{Auto, System, Terminal};
        use ProtocolType::{Halfblocks, Iterm2, Kitty, Sixel};
        use Route::{System as Os, Terminal as Here};
        // (preference, protocol, kind, route)
        let table = [
            (Auto, Halfblocks, Kind::Image, Os),
            (Auto, Kitty, Kind::Image, Here),
            (Auto, Sixel, Kind::Image, Here),
            (Auto, Iterm2, Kind::Image, Here),
            (Terminal, Halfblocks, Kind::Image, Here),
            (Terminal, Kitty, Kind::Image, Here),
            (System, Halfblocks, Kind::Image, Os),
            (System, Kitty, Kind::Image, Os),
        ];
        for (preference, protocol, kind, expected) in table {
            assert_eq!(
                route(preference, protocol, kind),
                expected,
                "{preference:?} on {protocol:?} for {kind:?}"
            );
        }
        // Videos and formats we cannot decode always go to the OS, whatever
        // the setting or the terminal.
        for preference in [Auto, Terminal, System] {
            for protocol in [Halfblocks, Kitty, Sixel, Iterm2] {
                for kind in [Kind::Video, Kind::Unsupported] {
                    assert_eq!(route(preference, protocol, kind), Os);
                }
            }
        }
    }

    #[test]
    fn preference_parses_the_setting_and_defaults_to_auto() {
        assert_eq!(Preference::parse("auto"), Preference::Auto);
        assert_eq!(Preference::parse("terminal"), Preference::Terminal);
        assert_eq!(Preference::parse("system"), Preference::System);
        assert_eq!(Preference::parse("something else"), Preference::Auto);
        for value in nexus_core::app::IMAGE_VIEWERS {
            assert_eq!(
                Preference::parse(value) == Preference::Auto,
                value == "auto"
            );
        }
    }

    #[test]
    fn kinds_come_from_the_extension_and_a_sibling_video() {
        for good in [
            "a.png",
            "a.PNG",
            "a.jpg",
            "a.jpeg",
            "a.webp",
            "a.svg",
            "a",
            "https://x.test/p.png",
        ] {
            assert_eq!(kind_of(good), Kind::Image, "{good}");
        }
        // Whatever this build of `image` cannot read goes to the OS viewer.
        for (name, format) in [("a.gif", ImageFormat::Gif), ("a.bmp", ImageFormat::Bmp)] {
            let expected = if format.reading_enabled() {
                Kind::Image
            } else {
                Kind::Unsupported
            };
            assert_eq!(kind_of(name), expected, "{name}");
        }
        let dir = temp_dir();
        let thumb = png_file(&dir, "clip_first.png", 8, 8);
        assert_eq!(kind_of(&thumb), Kind::Image, "no mp4 yet");
        std::fs::write(dir.join("clip.mp4"), b"x").unwrap();
        assert_eq!(kind_of(&thumb), Kind::Video);
        assert_eq!(video_for(&thumb), Some(dir.join("clip.mp4")));
        let last = png_file(&dir, "clip_last.png", 8, 8);
        assert_eq!(video_for(&last), Some(dir.join("clip.mp4")));
        let stitched = png_file(&dir, "other_first.png", 8, 8);
        std::fs::write(dir.join("_stitch_other.mp4"), b"x").unwrap();
        assert_eq!(video_for(&stitched), Some(dir.join("_stitch_other.mp4")));
    }

    #[test]
    fn titles_are_a_file_name_or_a_host_and_path_tail() {
        assert_eq!(display_name("/tmp/x/sunset.png"), "sunset.png");
        assert_eq!(
            display_name("https://img.example.com/a/b/cat.png?x=1"),
            "img.example.com/\u{2026}/cat.png"
        );
        assert_eq!(
            display_name("https://img.example.com/cat.png"),
            "img.example.com/cat.png"
        );
        assert_eq!(display_name("https://img.example.com/"), "img.example.com");
    }

    // ── opening: terminal first, OS viewer as the fallback ───────────────

    #[test]
    fn auto_opens_in_the_terminal_with_graphics_and_in_the_os_with_halfblocks() {
        let dir = temp_dir();
        let pic = png_file(&dir, "a.png", 16, 16);
        let (mut app, log) = recording_app();

        app.image_viewer
            .set_picker(picker(ProtocolType::Halfblocks));
        app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
        assert!(app.popup == Popup::None);
        assert_eq!(*log.lock().unwrap(), [pic.clone()], "halfblocks -> OS");

        log.lock().unwrap().clear();
        app.image_viewer.set_picker(picker(ProtocolType::Kitty));
        app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
        assert!(app.popup == Popup::ImageViewer, "kitty -> terminal viewer");
        assert!(log.lock().unwrap().is_empty());
        app.close_image_viewer();

        // The setting overrides the protocol in both directions.
        app.core.settings.image_viewer = "system".into();
        app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
        assert!(app.popup == Popup::None);
        assert_eq!(log.lock().unwrap().len(), 1);
        app.core.settings.image_viewer = "terminal".into();
        app.image_viewer
            .set_picker(picker(ProtocolType::Halfblocks));
        app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
        assert!(app.popup == Popup::ImageViewer);
    }

    #[test]
    fn video_thumbnails_still_open_their_sibling_mp4_in_the_os_player() {
        let dir = temp_dir();
        let thumb = png_file(&dir, "abc_first.png", 16, 16);
        std::fs::write(dir.join("abc.mp4"), b"x").unwrap();
        let (mut app, log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        app.image_viewer.set_picker(picker(ProtocolType::Kitty));
        app.history_cache.image_at_line = vec![None, Some(thumb.clone()), Some(thumb), None];
        assert!(app.open_image_at_line(2));
        assert!(
            app.popup == Popup::None,
            "videos never open in the terminal"
        );
        assert_eq!(
            *log.lock().unwrap(),
            [dir.join("abc.mp4").to_string_lossy().into_owned()]
        );
        assert!(!app.open_image_at_line(0), "a text line opens nothing");
        assert!(!app.open_image_at_line(99));
    }

    #[test]
    fn formats_the_build_cannot_decode_fall_back_with_a_status_note() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let format = ImageFormat::Bmp;
        if format.reading_enabled() {
            return;
        }
        let path = dir.join("old.bmp");
        std::fs::write(&path, b"BM").unwrap();
        let (mut app, log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        app.open_image_target(&path.to_string_lossy(), &Anchor::ByTarget, Return::Nothing);
        assert!(app.popup == Popup::None);
        assert_eq!(log.lock().unwrap().len(), 1);
        let note = status(&mut app);
        assert!(note.contains("opened in the system viewer"), "{note}");
    }

    #[test]
    fn a_failed_decode_closes_the_viewer_and_opens_the_os_viewer() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.png");
        std::fs::write(&broken, b"definitely not a png").unwrap();
        let broken = broken.to_string_lossy().into_owned();
        let (mut app, log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        app.open_image_target(&broken, &Anchor::ByTarget, Return::Nothing);
        assert!(app.popup == Popup::ImageViewer);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| crate::ui::popups::image_viewer::render(f, &mut app))
            .unwrap();
        assert!(app.popup == Popup::None, "fell back");
        assert_eq!(*log.lock().unwrap(), [broken]);
        let note = status(&mut app);
        assert!(
            note.contains("broken.png") && note.contains("opened in the system viewer"),
            "{note}"
        );
    }

    #[test]
    fn view_command_resolves_paths_and_reports_missing_files() {
        let (mut app, _log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        let pic = png_file(&temp_dir(), "here.png", 8, 8);
        app.run_command(&format!("view {pic}")).unwrap();
        assert!(app.popup == Popup::ImageViewer);
        assert_eq!(app.image_viewer.current().unwrap().target, pic);
        app.close_image_viewer();
        // Relative names resolve against the space's files.
        let files = app.core.space.files_dir(&app.core.active_space.name);
        png_file(&files, "shared.png", 8, 8);
        app.run_command("view shared.png").unwrap();
        assert!(app.popup == Popup::ImageViewer);
        assert!(
            app.image_viewer
                .current()
                .unwrap()
                .target
                .ends_with("shared.png")
        );
        app.close_image_viewer();
        app.run_command("view /no/such/file.png").unwrap();
        assert!(app.popup == Popup::None);
        let note = status(&mut app);
        assert!(note.contains("no such image"), "{note}");
    }

    // ── the conversation's images in history order ───────────────────────

    fn app_with_history() -> AppView {
        let mut app = app();
        app.core.settings.image_viewer = "terminal".into();
        // line:   0     1        2        3     4        5        6     7   8 ..
        app.history_cache.image_at_line = vec![
            None,
            Some("/img/a.png".into()),
            Some("/img/a.png".into()),
            None,
            Some("/img/b.png".into()),
            Some("/img/b.png".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("/img/c.png".into()),
            None,
        ];
        app.history_cache.widgets = vec![Placement {
            row: 8,
            width: 70,
            owner: 3,
            ordinal: 0,
            height: 4,
            source: gallery_doc(&[PIC, PIC2]),
            complete: true,
        }];
        app
    }

    #[test]
    fn gallery_is_history_order_with_interactive_images_in_place() {
        let app = app_with_history();
        let targets: Vec<_> = app.image_gallery().into_iter().map(|i| i.target).collect();
        assert_eq!(
            targets,
            ["/img/a.png", "/img/b.png", PIC, PIC2, "/img/c.png"],
            "multi-line images count once; the block's images sit between b and c"
        );
    }

    #[test]
    fn the_same_image_twice_keeps_each_position() {
        let mut app = app_with_history();
        app.history_cache.image_at_line = vec![
            Some("/img/a.png".into()),
            None,
            Some("/img/a.png".into()),
            None,
        ];
        app.history_cache.widgets.clear();
        assert_eq!(app.image_gallery().len(), 2);
        app.open_image_target("/img/a.png", &Anchor::Line(2), Return::Nothing);
        assert_eq!(
            app.image_viewer.index(),
            1,
            "the clicked copy, not the first"
        );
        app.close_image_viewer();
        app.open_image_target("/img/a.png", &Anchor::Line(0), Return::Nothing);
        assert_eq!(app.image_viewer.index(), 0);
    }

    #[test]
    fn arrows_and_letters_walk_the_gallery_and_stop_at_the_ends() {
        use crossterm::event::KeyCode::{Char, End, Home, Left, Right};
        let mut app = app_with_history();
        app.open_image_target("/img/b.png", &Anchor::Line(5), Return::Nothing);
        assert!(app.popup == Popup::ImageViewer);
        assert_eq!(app.image_viewer.index(), 1);
        let at = |app: &AppView| app.image_viewer.current().unwrap().target.clone();
        key(&mut app, Right);
        assert_eq!(at(&app), PIC, "next: the first interactive image");
        key(&mut app, Char('l'));
        assert_eq!(at(&app), PIC2);
        key(&mut app, Char('l'));
        assert_eq!(at(&app), "/img/c.png");
        key(&mut app, Right);
        assert_eq!(at(&app), "/img/c.png", "stops at the last");
        key(&mut app, Left);
        key(&mut app, Char('h'));
        assert_eq!(at(&app), PIC);
        key(&mut app, Home);
        assert_eq!(at(&app), "/img/a.png");
        key(&mut app, Left);
        assert_eq!(at(&app), "/img/a.png", "stops at the first");
        key(&mut app, End);
        assert_eq!(app.image_viewer.index(), 4);
        key(&mut app, Char('p'));
        key(&mut app, Char('n'));
        assert_eq!(app.image_viewer.index(), 4);
        key(&mut app, Char('q'));
        assert!(app.popup == Popup::None);
        assert!(
            app.image_viewer.items().is_empty(),
            "closing drops the gallery"
        );
    }

    #[test]
    fn opening_from_an_interactive_image_lands_on_that_image() {
        let mut app = app_with_history();
        app.open_widget_image(
            &PendingImage {
                owner: 3,
                ordinal: 0,
                index: 1,
                url: PIC2.into(),
            },
            Return::Nothing,
        );
        assert!(app.popup == Popup::ImageViewer);
        assert_eq!(app.image_viewer.index(), 3);
        assert_eq!(app.image_viewer.current().unwrap().target, PIC2);
    }

    #[test]
    fn f_toggles_between_fit_and_one_to_one() {
        let dir = temp_dir();
        let pic = png_file(&dir, "big.png", 400, 300);
        let mut app = app();
        app.core.settings.image_viewer = "terminal".into();
        app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
        assert!(app.image_viewer.is_fit());
        key(&mut app, crossterm::event::KeyCode::Char('f'));
        assert!(!app.image_viewer.is_fit());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|f| crate::ui::popups::image_viewer::render(f, &mut app))
            .unwrap();
        // 400x300 px is wider than 38 cells of the halfblock picker's 10px font.
        assert_eq!(app.image_viewer.can_pan(), (true, true));
        // Arrows pan instead of changing image; Shift pans further.
        let at = app.image_viewer.index();
        key(&mut app, crossterm::event::KeyCode::Right);
        let first = app.image_viewer.pan();
        assert!(first.0 > 0 && first.1 == 0, "{first:?}");
        crate::ui::popups::image_viewer::handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Right,
                crossterm::event::KeyModifiers::SHIFT,
            ),
        );
        assert!(
            app.image_viewer.pan().0 > first.0 * 3,
            "{:?}",
            app.image_viewer.pan()
        );
        key(&mut app, crossterm::event::KeyCode::Char('j'));
        assert!(app.image_viewer.pan().1 > 0);
        assert_eq!(app.image_viewer.index(), at);
        key(&mut app, crossterm::event::KeyCode::Char('f'));
        assert!(app.image_viewer.is_fit());
        assert_eq!(app.image_viewer.pan(), (0, 0), "fit recenters");
    }

    #[test]
    fn large_images_are_downscaled_once_for_display() {
        let dir = temp_dir();
        let pic = png_file(&dir, "huge.png", 4000, 200);
        let decoded = decode(Path::new(&pic)).unwrap();
        assert_eq!((decoded.meta.width, decoded.meta.height), (4000, 200));
        assert_eq!(decoded.image.dimensions().0, MAX_EDGE);
        assert_eq!(decoded.meta.shown, Some(decoded.image.dimensions()));
        assert_eq!(decoded.meta.format, "PNG");
        let small = decode(Path::new(&png_file(&dir, "small.png", 30, 20))).unwrap();
        assert_eq!(small.meta.shown, None);
    }

    #[test]
    fn svg_files_are_rasterized_through_resvg() {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dot.svg");
        std::fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#f00"/></svg>"##,
        )
        .unwrap();
        let decoded = decode(&path).unwrap();
        assert_eq!(decoded.meta.format, "SVG");
        assert_eq!((decoded.meta.width, decoded.meta.height), (40, 20));
        assert_eq!(decoded.image.dimensions(), (SVG_EDGE, SVG_EDGE / 2));
    }

    #[test]
    fn remote_images_show_from_the_cache_and_say_when_they_are_off() {
        let (mut app, _log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        // No fetcher context on this thread: images are off.
        app.open_image_target(PIC, &Anchor::ByTarget, Return::Nothing);
        assert!(
            app.popup == Popup::ImageViewer,
            "opening never goes external by itself"
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| crate::ui::popups::image_viewer::render(f, &mut app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("remote images are off"), "{text}");
        assert!(text.contains("img.example.invalid/pic.png"), "{text}");
        app.close_image_viewer();

        let fetcher = nexus_core::remote_image::Fetcher::new(temp_dir());
        crate::app_view::use_fetcher_for_tests(&fetcher);
        fetcher.seed(PIC, &png_bytes(48, 24)).unwrap();
        app.open_image_target(PIC, &Anchor::ByTarget, Return::Nothing);
        terminal
            .draw(|f| crate::ui::popups::image_viewer::render(f, &mut app))
            .unwrap();
        let text = screen(&terminal);
        assert!(text.contains("48x24") && text.contains("PNG"), "{text}");
        assert!(text.contains('\u{2580}'), "halfblock pixels drawn:\n{text}");
    }

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([x as u8 * 5, y as u8 * 9, 77])
        })
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
        out
    }

    fn screen(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn the_files_popup_opens_images_in_the_viewer_and_returns_to_itself() {
        let (mut app, _log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        let files = app.core.space.files_dir(&app.core.active_space.name);
        png_file(&files, "from_files.png", 12, 12);
        app.open_files_popup(nexus_core::app::FilesTab::Images);
        assert!(app.popup == Popup::Files);
        app.open_selected_image();
        assert!(app.popup == Popup::ImageViewer);
        assert!(
            app.image_viewer
                .current()
                .unwrap()
                .target
                .ends_with("from_files.png")
        );
        key(&mut app, crossterm::event::KeyCode::Esc);
        assert!(
            app.popup == Popup::Files,
            "Esc goes back to the files popup"
        );
    }

    #[test]
    fn o_opens_the_os_viewer_explicitly_and_c_copies_the_path() {
        let dir = temp_dir();
        let pic = png_file(&dir, "p.png", 8, 8);
        let (mut app, log) = recording_app();
        app.core.settings.image_viewer = "terminal".into();
        app.open_image_target(&pic, &Anchor::ByTarget, Return::Nothing);
        key(&mut app, crossterm::event::KeyCode::Char('o'));
        assert_eq!(*log.lock().unwrap(), [pic.clone()]);
        assert!(app.popup == Popup::ImageViewer, "the viewer stays open");
        // A video thumbnail's Enter plays its mp4; Enter on a still does nothing.
        key(&mut app, crossterm::event::KeyCode::Enter);
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    #[test]
    fn enter_on_a_video_thumbnail_plays_the_video_externally() {
        let dir = temp_dir();
        let thumb = png_file(&dir, "v_first.png", 8, 8);
        std::fs::write(dir.join("v.mp4"), b"x").unwrap();
        let (mut app, log) = recording_app();
        // Reached by walking the gallery (a click goes straight to the OS).
        app.image_viewer.start(
            vec![Item {
                target: thumb,
                anchor: Anchor::ByTarget,
            }],
            0,
            Return::Nothing,
        );
        app.popup = Popup::ImageViewer;
        key(&mut app, crossterm::event::KeyCode::Enter);
        assert_eq!(
            *log.lock().unwrap(),
            [dir.join("v.mp4").to_string_lossy().into_owned()]
        );
    }
}
