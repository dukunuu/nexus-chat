//! What the image viewer does besides show a picture: copy it or its path to
//! the clipboard, save a copy into the space or `~/Downloads`, reveal it in the
//! file manager, attach it to the draft, or take a file dropped on it.
//!
//! The pointer's state (drag, clicks, hover, the action menu) lives here too,
//! on the [`Viewer`](crate::image_viewer::Viewer) inside `AppView`. Everything
//! that touches the outside world goes through [`Hooks`], so tests inject a
//! fake clipboard, a Downloads folder and a recording file-manager launcher.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use ratatui::layout::Rect;

use crate::app_view::AppView;
use crate::image_viewer::{Decoded, Kind, Source, display_name, is_remote, kind_of, source_of};

/// Originals up to this many pixels are decoded again at full size for "copy
/// image"; bigger ones copy the downscaled copy the viewer already holds.
pub const MAX_COPY_PIXELS: u64 = 32 * 1000 * 1000;
/// Two clicks this close together are a double-click.
pub const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

// ── Pointer state ─────────────────────────────────────────────────────────

/// A left-button press that may turn into a pan or a click.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub from: (u16, u16),
    pub last: (u16, u16),
    pub moved: bool,
}

/// The open action menu.
#[derive(Clone, Copy, Debug)]
pub struct Menu {
    pub selected: usize,
    /// Where it was asked for (the pointer, or the window's centre).
    pub at: (u16, u16),
}

#[derive(Default)]
pub struct Pointer {
    /// The window the picture is drawn in, and the whole screen, as of the
    /// last draw (cells): what clicks and the zoom anchor are measured in.
    pub body: Rect,
    pub screen: Rect,
    pub drag: Option<Drag>,
    pub last_click: Option<(Instant, (u16, u16))>,
    pub hover: Option<(u16, u16)>,
    pub menu: Option<Menu>,
}

// ── Seams ─────────────────────────────────────────────────────────────────

/// The slice of the OS clipboard the viewer uses.
pub trait Clipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
    /// Straight (not premultiplied) RGBA, row by row.
    fn set_image(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> Result<(), String>;
}

impl Clipboard for arboard::Clipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        arboard::Clipboard::set_text(self, text.to_string()).map_err(|e| e.to_string())
    }

    fn set_image(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> Result<(), String> {
        arboard::Clipboard::set_image(
            self,
            arboard::ImageData {
                width,
                height,
                bytes: rgba.into(),
            },
        )
        .map_err(|e| e.to_string())
    }
}

/// Launches the file manager with an argv (never a shell).
#[derive(Clone)]
pub struct Launcher(Arc<RunFn>);

type RunFn = dyn Fn(&[OsString]) -> Result<(), String> + Send + Sync;

impl Launcher {
    pub fn new(run: impl Fn(&[OsString]) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(run))
    }

    #[must_use]
    pub fn system() -> Self {
        Self::new(run_detached)
    }

    fn run(&self, argv: &[OsString]) -> Result<(), String> {
        (self.0)(argv)
    }
}

fn run_detached(argv: &[OsString]) -> Result<(), String> {
    let (command, rest) = argv.split_first().ok_or("nothing to run")?;
    let mut child = std::process::Command::new(command)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", command.to_string_lossy()))?;
    // Reap it so a long session leaves no zombies behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub struct Hooks {
    /// `None` means the app's own clipboard handle.
    pub clipboard: Option<Box<dyn Clipboard>>,
    pub launcher: Launcher,
    /// `None` means `~/Downloads`.
    pub downloads: Option<PathBuf>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            clipboard: None,
            // Tests must never launch a file manager.
            #[cfg(test)]
            launcher: Launcher::new(|_| Ok(())),
            #[cfg(not(test))]
            launcher: Launcher::system(),
            downloads: None,
        }
    }
}

// ── The actions ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CopyImage,
    CopyPath,
    SaveToFiles,
    SaveToDownloads,
    Reveal,
    Open,
    Attach,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Self::CopyImage,
        Self::CopyPath,
        Self::SaveToFiles,
        Self::SaveToDownloads,
        Self::Reveal,
        Self::Open,
        Self::Attach,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::CopyImage => "Copy image",
            Self::CopyPath => "Copy path",
            Self::SaveToFiles => "Save to files",
            Self::SaveToDownloads => "Save to Downloads",
            Self::Reveal => "Reveal",
            Self::Open => "Open in default app",
            Self::Attach => "Attach to message",
        }
    }

    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::CopyImage => "y",
            Self::CopyPath => "c",
            Self::SaveToFiles => "s",
            Self::SaveToDownloads => "S",
            Self::Reveal => "r",
            Self::Open => "o",
            Self::Attach => "a",
        }
    }
}

/// Which file manager command reveals a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Windows,
}

impl Os {
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The argv that shows `path` in the file manager: macOS selects the file in
/// Finder, Windows selects it in Explorer, Linux (no common "select" verb)
/// opens the folder that holds it.
#[must_use]
pub fn reveal_argv(os: Os, path: &Path) -> Vec<OsString> {
    match os {
        Os::Mac => vec!["open".into(), "-R".into(), path.into()],
        Os::Linux => vec![
            "xdg-open".into(),
            path.parent().unwrap_or(path).as_os_str().to_owned(),
        ],
        Os::Windows => {
            let mut select = OsString::from("/select,");
            select.push(path);
            vec!["explorer".into(), select]
        }
    }
}

/// A file name that is safe in a markdown image reference and on every
/// filesystem: letters, digits, `-`, `_` and `.`; anything else becomes `_`.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    let cleaned: String = cleaned.chars().take(100).collect();
    if cleaned.is_empty() {
        "image".into()
    } else {
        cleaned
    }
}

/// What to call the saved copy of `target`, whose bytes are at `source`: a
/// file keeps its name; a URL's last path segment (else its host) is used,
/// with the cached copy's extension, since the cache stores everything as PNG.
fn saved_name(target: &str, source: &Path) -> String {
    let extension = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    if is_remote(target) {
        let stem = reqwest::Url::parse(target).ok().and_then(|url| {
            let segment = url
                .path_segments()?
                .rfind(|part| !part.is_empty())
                .map(|part| {
                    Path::new(part)
                        .file_stem()
                        .map_or_else(|| part.to_string(), |s| s.to_string_lossy().into())
                });
            segment.or_else(|| url.host_str().map(str::to_string))
        });
        return safe_name(&format!("{}.{extension}", stem.unwrap_or_default()));
    }
    safe_name(&display_name(target))
}

/// Copy `source` into `dir` under a name no other file has: `name.png`, then
/// `name-2.png`, `name-3.png`, ... The name is claimed atomically, so two
/// saves never overwrite each other.
///
/// # Errors
/// When the folder can't be created or the copy fails.
pub fn save_unique(source: &Path, dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for attempt in 1..=10_000u32 {
        let candidate = if attempt == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{stem}-{attempt}{extension}"))
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut out) => {
                let copied = std::fs::File::open(source)
                    .and_then(|mut input| std::io::copy(&mut input, &mut out));
                return match copied {
                    Ok(_) => Ok(candidate),
                    Err(e) => {
                        drop(out);
                        let _ = std::fs::remove_file(&candidate);
                        Err(e)
                    }
                };
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "too many files with that name",
    ))
}

/// `~/Pictures/x.png` for a path under the home folder.
fn tilde(path: &Path) -> String {
    if let Some(home) = std::env::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

/// The pixels "copy image" puts on the clipboard, and whether they are the
/// image at its real size. Small images and SVGs use the copy the viewer
/// already decoded; a photo the viewer downscaled is decoded again from the
/// file at full size when it is at most [`MAX_COPY_PIXELS`].
fn clipboard_pixels(decoded: &Decoded) -> (u32, u32, Vec<u8>, bool) {
    let meta = &decoded.meta;
    let pixels = u64::from(meta.width) * u64::from(meta.height);
    if meta.shown.is_some()
        && meta.format != "SVG"
        && pixels <= MAX_COPY_PIXELS
        && let Ok(full) = crate::image_viewer::decode_original(&decoded.path)
    {
        let (width, height) = image::GenericImageView::dimensions(&full);
        return (width, height, full.into_rgba8().into_raw(), true);
    }
    let (width, height) = image::GenericImageView::dimensions(&*decoded.image);
    let real = meta.shown.is_none() || meta.format == "SVG";
    (width, height, decoded.image.to_rgba8().into_raw(), real)
}

/// A pasted or dropped file path, however the terminal wrote it: plain,
/// quoted, `file://`, or with backslash-escaped spaces.
fn dropped_path(text: &str) -> Option<PathBuf> {
    if let Some(path) = crate::composer::pasted_file_path(text) {
        return Some(path);
    }
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.trim().chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            unescaped.extend(chars.next());
        } else {
            unescaped.push(c);
        }
    }
    crate::composer::pasted_file_path(&unescaped)
}

impl AppView {
    /// Run one of the viewer's actions on the current image.
    pub fn viewer_action(&mut self, action: Action) {
        match action {
            Action::CopyImage => self.viewer_copy_image(),
            Action::CopyPath => self.viewer_copy_path(),
            Action::SaveToFiles => self.viewer_save(false),
            Action::SaveToDownloads => self.viewer_save(true),
            Action::Reveal => self.viewer_reveal(),
            Action::Open => self.open_viewer_item_externally(),
            Action::Attach => self.viewer_attach(),
        }
    }

    /// The current image and the file its pixels are in (for a URL, the
    /// cached copy), or a status line saying why there is none.
    fn viewer_file(&mut self) -> Option<(String, PathBuf)> {
        let target = self.image_viewer.current()?.target.clone();
        match source_of(&target) {
            Source::Ready(path) => Some((target, path)),
            Source::Loading => {
                self.push_status("the image is still loading");
                None
            }
            Source::Off => {
                self.push_status("remote images are off \u{2014} nothing to save");
                None
            }
            Source::Failed | Source::Missing => {
                self.push_status("there is no file for this image");
                None
            }
        }
    }

    fn with_clipboard<T>(
        &mut self,
        use_it: impl FnOnce(&mut dyn Clipboard) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(sink) = self.image_viewer.hooks.clipboard.as_deref_mut() {
            return use_it(sink);
        }
        match self.clipboard.as_mut() {
            Some(clipboard) => use_it(clipboard),
            None => Err("no clipboard here".into()),
        }
    }

    fn viewer_copy_image(&mut self) {
        let Some(decoded) = self.image_viewer.decoded() else {
            self.push_status("the image isn't loaded yet");
            return;
        };
        let natural = (decoded.meta.width, decoded.meta.height);
        let (width, height, rgba, full) = clipboard_pixels(decoded);
        let result = self
            .with_clipboard(|clipboard| clipboard.set_image(width as usize, height as usize, rgba));
        self.push_status(match result {
            Ok(()) if full => format!("copied image {width}\u{d7}{height}"),
            Ok(()) => format!(
                "copied image {width}\u{d7}{height} (downscaled from {}\u{d7}{})",
                natural.0, natural.1
            ),
            Err(e) => format!("couldn't copy the image: {e}"),
        });
    }

    fn viewer_copy_path(&mut self) {
        let Some(target) = self.image_viewer.current().map(|item| item.target.clone()) else {
            return;
        };
        let result = self.with_clipboard(|clipboard| clipboard.set_text(&target));
        self.push_status(match result {
            Ok(()) if is_remote(&target) => "copied URL".to_string(),
            Ok(()) => "copied path".to_string(),
            Err(e) => format!("couldn't copy: {e}"),
        });
    }

    /// Where `s` and `a` keep a copy: the top level of the space's files, so
    /// it is indexed, searchable and listed in the Files and Images popups.
    fn space_files_dir(&self) -> PathBuf {
        self.core.space.files_dir(&self.core.active_space.name)
    }

    fn downloads_dir(&self) -> Option<PathBuf> {
        self.image_viewer
            .hooks
            .downloads
            .clone()
            .or_else(|| std::env::home_dir().map(|home| home.join("Downloads")))
    }

    /// `s`: a copy in the space's files; `S`: a copy in Downloads.
    fn viewer_save(&mut self, downloads: bool) {
        let Some((target, source)) = self.viewer_file() else {
            return;
        };
        let dir = if downloads {
            let Some(dir) = self.downloads_dir() else {
                self.push_status("no Downloads folder here");
                return;
            };
            dir
        } else {
            self.space_files_dir()
        };
        match save_unique(&source, &dir, &saved_name(&target, &source)) {
            Ok(saved) if downloads => self.push_status(format!("saved to {}", tilde(&saved))),
            Ok(saved) => self.push_status(format!(
                "saved to files/{}",
                saved.file_name().unwrap_or_default().to_string_lossy()
            )),
            Err(e) => self.push_status(format!("couldn't save: {e}")),
        }
    }

    /// `r`: show the file (for a URL, its cached copy) in the file manager.
    fn viewer_reveal(&mut self) {
        let Some((_, source)) = self.viewer_file() else {
            return;
        };
        let path = std::path::absolute(&source).unwrap_or(source);
        let argv = reveal_argv(Os::current(), &path);
        let launcher = self.image_viewer.hooks.launcher.clone();
        self.push_status(match launcher.run(&argv) {
            Ok(()) => format!("revealed {}", display_name(&path.to_string_lossy())),
            Err(e) => format!("couldn't reveal: {e}"),
        });
    }

    /// `a`: put `![name](path)` in the draft the way a pasted image lands, and
    /// close the viewer. The reference is relative to the space's files, so a
    /// picture from anywhere else is first saved there.
    fn viewer_attach(&mut self) {
        let Some((target, source)) = self.viewer_file() else {
            return;
        };
        let files = self.core.space.files_dir(&self.core.active_space.name);
        let inside = source.strip_prefix(&files).ok().filter(|rel| {
            rel.components()
                .all(|c| matches!(c, std::path::Component::Normal(part) if safe_name(&part.to_string_lossy()) == part.to_string_lossy()))
        });
        let reference = if let Some(rel) = inside {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        } else {
            match save_unique(
                &source,
                &self.space_files_dir(),
                &saved_name(&target, &source),
            ) {
                Ok(saved) => saved
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                Err(e) => {
                    self.push_status(format!("couldn't attach: {e}"));
                    return;
                }
            }
        };
        let alt = Path::new(&reference)
            .file_stem()
            .map_or_else(|| "image".into(), |s| s.to_string_lossy().into_owned());
        let markdown = format!("![{alt}]({reference})");
        let (row, column) = self.input.cursor();
        let after_text = column > 0
            && self.input.lines()[row]
                .chars()
                .nth(column - 1)
                .is_some_and(|c| !c.is_whitespace());
        self.input.insert_str(if after_text {
            format!(" {markdown}")
        } else {
            markdown
        });
        self.close_image_viewer();
        self.popup = nexus_core::app::Popup::None;
        self.push_status(format!("attached {reference}"));
    }

    /// A file path pasted or dropped while the viewer is open: an image opens
    /// in the viewer, anything else is ignored with a note. Text that isn't a
    /// file path is ignored silently.
    pub fn viewer_drop(&mut self, text: &str) {
        let Some(path) = dropped_path(text) else {
            return;
        };
        let target = path.to_string_lossy().into_owned();
        let name = display_name(&target);
        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(crate::image_viewer::is_image_extension);
        if !is_image {
            self.push_status(format!("{name}: not an image \u{2014} ignored"));
            return;
        }
        if kind_of(&target) == Kind::Image {
            self.image_viewer.insert_after_current(target);
            self.push_status(format!("opened {name}"));
        } else {
            self.image_viewer.open_external(&target);
            self.push_status(format!(
                "{name}: can't be shown in the terminal \u{2014} opened in the system viewer"
            ));
        }
    }

    /// Open the action menu at a cell, or at the window's centre.
    pub fn open_viewer_menu(&mut self, at: Option<(u16, u16)>) {
        let body = self.image_viewer.pointer.body;
        let at = at.unwrap_or((
            body.x + body.width.saturating_sub(MENU_WIDTH) / 2,
            body.y + body.height.saturating_sub(MENU_HEIGHT) / 2,
        ));
        self.image_viewer.pointer.drag = None;
        self.image_viewer.pointer.menu = Some(Menu { selected: 0, at });
    }
}

pub const MENU_WIDTH: u16 = 38;
#[allow(clippy::cast_possible_truncation)] // seven actions
pub const MENU_HEIGHT: u16 = Action::ALL.len() as u16 + 2;

/// Where the action menu opened at `at` sits on `screen`: its corner at the
/// pointer, pushed back in when it would run off an edge.
#[must_use]
pub fn menu_rect(screen: Rect, at: (u16, u16)) -> Rect {
    let width = MENU_WIDTH.min(screen.width);
    let height = MENU_HEIGHT.min(screen.height);
    Rect::new(
        at.0.min(screen.right().saturating_sub(width)).max(screen.x),
        at.1.min(screen.bottom().saturating_sub(height))
            .max(screen.y),
        width,
        height,
    )
}

#[cfg(test)]
mod tests;
