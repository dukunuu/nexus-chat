//! Native Open and Save dialogs behind a trait, so every caller can be tested
//! with [`Scripted`] and nothing but a frontend ever opens a real window.
//!
//! The real dialogs ([`Native`], feature `native-dialogs`) are `rfd`'s blocking
//! ones. macOS requires them on the main thread: studio asks from inside its
//! `tao` event loop and the TUI from its main-thread event loop, which is where
//! both call these.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A named group of file extensions a dialog offers (`["pptx"]`, no dot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

/// What a dialog was asked, for tests to inspect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub save: bool,
    pub title: String,
    pub start: Option<PathBuf>,
    /// The file name a Save dialog is pre-filled with.
    pub name: String,
    pub filters: Vec<&'static str>,
}

/// One Open and one Save dialog. `None` is a cancel (or a dialog that could
/// not be shown): callers then do nothing.
pub trait Dialogs: Send + Sync {
    /// Whether a dialog can be shown at all: false over SSH or without a
    /// display, where callers fall back to a default location.
    fn available(&self) -> bool {
        true
    }
    fn open_file(&self, title: &str, start: Option<&Path>, filters: &[Filter]) -> Option<PathBuf>;
    fn save_file(
        &self,
        title: &str,
        start: Option<&Path>,
        name: &str,
        filters: &[Filter],
    ) -> Option<PathBuf>;
}

/// Answers from a queue (an empty queue cancels), and remembers each request.
/// Clones share the queue, so a test keeps a handle after boxing one.
#[derive(Clone, Debug)]
pub struct Scripted {
    answers: Arc<Mutex<VecDeque<Option<PathBuf>>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    available: bool,
}

impl Scripted {
    /// Dialogs that answer `answers` in order, then cancel.
    #[must_use]
    pub fn new(answers: impl IntoIterator<Item = Option<PathBuf>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into_iter().collect())),
            requests: Arc::default(),
            available: true,
        }
    }

    /// No dialog at all, as over SSH.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::new([])
        }
    }

    /// Everything asked so far.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests").clone()
    }

    fn ask(&self, request: Request) -> Option<PathBuf> {
        self.requests.lock().expect("requests").push(request);
        self.answers.lock().expect("answers").pop_front().flatten()
    }
}

impl Dialogs for Scripted {
    fn available(&self) -> bool {
        self.available
    }

    fn open_file(&self, title: &str, start: Option<&Path>, filters: &[Filter]) -> Option<PathBuf> {
        self.ask(Request {
            save: false,
            title: title.into(),
            start: start.map(Path::to_path_buf),
            name: String::new(),
            filters: filters.iter().map(|f| f.name).collect(),
        })
    }

    fn save_file(
        &self,
        title: &str,
        start: Option<&Path>,
        name: &str,
        filters: &[Filter],
    ) -> Option<PathBuf> {
        self.ask(Request {
            save: true,
            title: title.into(),
            start: start.map(Path::to_path_buf),
            name: name.into(),
            filters: filters.iter().map(|f| f.name).collect(),
        })
    }
}

/// The operating system's own dialogs.
#[cfg(feature = "native-dialogs")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

#[cfg(feature = "native-dialogs")]
impl Native {
    fn dialog(title: &str, start: Option<&Path>, filters: &[Filter]) -> rfd::FileDialog {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some(dir) = start.filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        for filter in filters {
            dialog = dialog.add_filter(filter.name, filter.extensions);
        }
        dialog
    }
}

#[cfg(feature = "native-dialogs")]
impl Dialogs for Native {
    fn available(&self) -> bool {
        gui_session()
    }

    fn open_file(&self, title: &str, start: Option<&Path>, filters: &[Filter]) -> Option<PathBuf> {
        guarded(|| Self::dialog(title, start, filters).pick_file())
    }

    fn save_file(
        &self,
        title: &str,
        start: Option<&Path>,
        name: &str,
        filters: &[Filter],
    ) -> Option<PathBuf> {
        guarded(|| {
            Self::dialog(title, start, filters)
                .set_file_name(name)
                .save_file()
        })
    }
}

/// A dialog that cannot be built (rfd panics when macOS has no main thread to
/// run it on) counts as a cancel rather than taking the program down.
#[cfg(feature = "native-dialogs")]
fn guarded(show: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(show))
        .ok()
        .flatten()
}

/// Whether the session can show a window: always on macOS and Windows unless
/// the shell is remote; on Linux a display or a session bus is needed.
#[cfg(feature = "native-dialogs")]
#[must_use]
pub fn gui_session() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return false;
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return true;
    }
    set("DISPLAY") || set("WAYLAND_DISPLAY") || set("DBUS_SESSION_BUS_ADDRESS")
}

/// Open filter shared by studio's Open and the library: nexus files and the
/// Office and Markdown files it imports.
pub const OPEN: &[Filter] = &[
    Filter {
        name: "Decks, docs and imports",
        extensions: &["json", "pptx", "docx", "dotx", "md"],
    },
    Filter {
        name: "Decks and docs",
        extensions: &["json"],
    },
    Filter {
        name: "PowerPoint",
        extensions: &["pptx"],
    },
    Filter {
        name: "Word",
        extensions: &["docx", "dotx"],
    },
    Filter {
        name: "Markdown",
        extensions: &["md"],
    },
];

/// Images studio stores with a deck or doc.
pub const IMAGES: &[Filter] = &[Filter {
    name: "Images",
    extensions: &["png", "jpg", "jpeg", "webp"],
}];

/// Templates studio can turn into a design.
pub const TEMPLATES: &[Filter] = &[Filter {
    name: "Office templates",
    extensions: &["pptx", "potx", "docx", "dotx"],
}];

/// The Save filter for a file extension (`pptx`, `deck.json`, `zip`, …).
#[must_use]
pub fn save_filter(extension: &str) -> Vec<Filter> {
    let (name, extensions): (&'static str, &'static [&'static str]) = match extension {
        "pptx" => ("PowerPoint", &["pptx"]),
        "docx" => ("Word", &["docx"]),
        "pdf" => ("PDF", &["pdf"]),
        "zip" => ("PNG images (zip)", &["zip"]),
        "html" => ("Web page", &["html"]),
        "md" => ("Markdown", &["md"]),
        "png" => ("PNG image", &["png"]),
        _ => ("JSON", &["json"]),
    };
    vec![Filter { name, extensions }]
}
