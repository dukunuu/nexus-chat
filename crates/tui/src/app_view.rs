//! The TUI's view layer (Phase 2e): `AppView` wraps the domain `App` plus
//! every piece of UI state extracted from core — composer, popup chrome and
//! caches, render state, theme, status line. `Deref<Target = App>` keeps the
//! existing `app.*` call sites compiling unchanged: fields the view owns
//! resolve to `AppView`, everything else falls through to the domain `App`,
//! and method calls on the domain (`send_message`, `push_status`, …) resolve via
//! deref. Events carry UI feedback the other way (`AppEvent::Status`,
//! `ComposerSet`/`ComposerClear`, `ViewportReset`, `HistoryInvalidated`,
//! `OpenLoginPopup`) — `apply_event` applies them to this layer.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

use anyhow::{Result, bail};
use ratatui::layout::Rect;
use tui_textarea::TextArea;

use nexus_core::app::{
    App, AppEvent, AppsMode, CopyOption, FilesMode, FilesTab, ImagesMode, KeyTarget, ModelPanel,
    MouseTarget, PendingEditor, Popup, ScriptsMode, SessionMode, SettingsField, SkillsMode,
    SpaceMode, SwarmPopupMode, WatchMode,
};
use nexus_core::db::{FileRow, Space as SpaceRow};
use nexus_core::provider::BackendTag;

use crate::filter_input::FilterInput;
use crate::flows::files::PickerEntry;
use crate::history_cache::HistoryCache;
use crate::selection::HistorySel;
use crate::theme::{BackgroundMode, Theme};

/// Built-in start-screen banner (override with `~/.config/nexus-chat/banner.txt`).
const BANNER: &str = r"
███╗   ██╗███████╗██╗  ██╗██╗   ██╗███████╗
████╗  ██║██╔════╝╚██╗██╔╝██║   ██║██╔════╝
██╔██╗ ██║█████╗   ╚███╔╝ ██║   ██║███████╗
██║╚██╗██║██╔══╝   ██╔██╗ ██║   ██║╚════██║
██║ ╚████║███████╗██╔╝ ██╗╚██████╔╝███████║
╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝";

// App state is inherently many booleans (modes, toggles, dirty flags).
#[allow(clippy::struct_excessive_bools)]
pub struct AppView {
    /// The domain half — sessions, research pipeline, provider clients,
    /// tools, db. Everything this view layer doesn't own.
    pub core: App,

    // ── Composer ──────────────────────────────────────────────────────────
    /// Message composer. A real editor: cursor movement, word-jump, selection,
    /// cut/copy/paste, undo — all from tui-textarea's default keymap.
    pub input: TextArea<'static>,
    /// Long-lived OS clipboard handle. Kept alive so X11 keeps serving the
    /// contents to clipboard managers (recreating per-op drops ownership in ~1ms).
    pub clipboard: Option<arboard::Clipboard>,
    /// The composer's inner (inside-border) rect from the last render, so mouse
    /// clicks can be mapped to cursor positions.
    pub input_inner: Rect,
    /// Composer double/triple-click tracking: (time, screen pos) of the last
    /// press, and its click count (2 = word select, 3+ = line select), so a
    /// drag that follows can extend by word/line instead of by char.
    pub composer_click: Option<(std::time::Instant, (u16, u16))>,
    pub composer_click_count: u8,
    /// Data-space cursor position at the start of a word-mode composer drag,
    /// so each drag step can re-select from that word outward.
    pub composer_word_anchor: Option<(usize, usize)>,
    /// Highlighted row in the slash-command autocomplete popup.
    pub cmd_selected: usize,
    /// `@` file autocomplete: (matches, selected, cursor byte offset of `@`).
    pub at_state: Option<(Vec<FileRow>, usize, usize)>,

    // ── Popup chrome ──────────────────────────────────────────────────────
    /// Which modal popover, if any, is open.
    pub popup: Popup,
    pub spaces_cache: Vec<SpaceRow>,
    pub space_selected: usize,
    pub space_filter: FilterInput,
    pub space_mode: SpaceMode,
    pub space_edit: String,
    pub session_selected: usize,
    /// Fuzzy filter typed in the session picker (matches title, slug, and id).
    pub session_filter: FilterInput,
    /// Whether the picker is browsing, renaming, or confirming a delete.
    pub session_mode: SessionMode,
    /// Edit buffer while renaming a session.
    pub session_edit: String,
    /// (session id, preview text) of the last session the picker previewed
    /// from the db — recomputed only when the selection moves.
    pub session_preview: Option<(String, String)>,
    pub files_selected: usize,
    pub files_mode: FilesMode,
    pub files_tab: FilesTab,
    /// Path being typed/pasted in the files popup's Add mode.
    pub files_edit: String,
    /// Directory the file-picker browser is showing (remembered across opens).
    pub picker_dir: PathBuf,
    pub picker_entries: Vec<PickerEntry>,
    pub picker_filter: String,
    pub picker_selected: usize,
    pub images_selected: usize,
    pub images_mode: ImagesMode,
    pub scripts_selected: usize,
    pub scripts_mode: ScriptsMode,
    pub scripts_edit: String,
    pub apps_selected: usize,
    pub apps_mode: AppsMode,
    pub apps_edit: String,
    pub watch_selected: usize,
    pub watch_mode: WatchMode,
    pub swarm_selected: usize,
    pub swarm_popup_mode: SwarmPopupMode,
    pub skills_mode: SkillsMode,
    pub skills_selected: usize,
    /// GitHub `owner/repo/path` shorthand being typed in Install mode.
    pub skills_edit: String,
    /// The `/usage` popup: aggregates snapshot + recent-list cursor.
    pub usage_data: Option<nexus_core::app::usage::UsageData>,
    pub usage_scroll: usize,
    pub key_input: String,
    /// Which backend the current `Popup::Key` entry is for.
    pub key_target: KeyTarget,
    /// Highlighted row in the `/login` provider selector.
    pub login_selected: usize,
    pub settings_selected: usize,
    /// Text edit buffers for the numeric settings (temperature, `top_p`, `max_tokens`).
    pub settings_inputs: [String; 8],
    /// Indices into `SETTINGS_GROUPS` currently collapsed (hidden fields).
    pub settings_collapsed: HashSet<usize>,
    pub model_filter: FilterInput,
    /// Narrow the merged model list to one backend (Ctrl+P cycles it); `None` = all.
    pub model_backend_filter: Option<BackendTag>,
    pub model_focus: ModelPanel,
    pub fav_selected: usize,
    pub avail_selected: usize,
    /// Last-rendered scroll offset of each model-picker panel (render state
    /// stashed for click mapping).
    pub fav_offset: usize,
    pub avail_offset: usize,
    /// `/copy` menu entries and the highlighted row.
    pub copy_options: Vec<CopyOption>,
    pub copy_selected: usize,

    // ── Render / view ─────────────────────────────────────────────────────
    /// Lines scrolled up from the bottom (0 = following the newest lines).
    pub scroll: usize,
    /// Max useful `scroll` (lines above the viewport), refreshed each render so
    /// scrolling can be clamped instead of running off into empty space.
    pub max_scroll: usize,
    /// Total rendered lines from the previous render frame, used during streaming
    /// to keep the viewport pinned when the user has scrolled up.
    pub prev_total: usize,
    /// Rendered text of the streaming tail from the last frame (empty when
    /// not streaming). The tail re-renders from scratch every frame, so line
    /// indices aren't stable — this lets the next frame re-find the line the
    /// viewport was pinned to by content instead of by index.
    pub prev_tail: Vec<String>,
    /// One-shot flag: the user just toggled a display flag (Ctrl+R reasoning,
    /// Ctrl+T tool detail), so the next frame's cache re-wrap must expand/
    /// collapse in place — pin the viewport top even when following the bottom
    /// of a live stream.
    pub pin_viewport_top: bool,
    /// Mouse text-selection over the history pane.
    pub sel: HistorySel,
    /// Which pane a mouse press is currently interacting with, so drag/release
    /// route to the right place even when the cursor leaves the pane.
    pub mouse_target: MouseTarget,
    /// Wrapped-line cache for the transcript, so redraws don't re-render
    /// markdown for the whole conversation every frame.
    pub history_cache: HistoryCache,
    /// Per-session history caches preserved across session switches so
    /// switching back doesn't re-wrap every message from scratch.
    pub session_caches: HashMap<String, HistoryCache>,
    /// Whether tool-call blocks show full arguments/results (Ctrl+T).
    pub show_tool_detail: bool,
    /// Screen rectangles for the currently rendered notification rows.
    pub notification_areas: Vec<(Rect, usize)>,
    /// Start-screen banner (custom or built-in) and a greeting picked at launch.
    pub banner: String,
    pub greeting: &'static str,
    /// Color palette — the active omarchy theme when present, else the
    /// built-in default. `theme_link` is the last-seen omarchy symlink
    /// target, polled by the event loop to detect a theme switch.
    pub theme: Theme,
    /// How the TUI surface background is painted; persisted per device and
    /// changed with `/theme opaque` or `/theme transparent`.
    pub background_mode: BackgroundMode,
    pub theme_link: Option<PathBuf>,
    /// Bumped every time `theme` changes, so the history render cache (which
    /// bakes colors into cached `Line`s) knows to re-wrap on a theme switch.
    pub theme_gen: usize,

    // ── Flow chrome ───────────────────────────────────────────────────────
    /// One-line status, fed by `AppEvent::Status` (domain code pushes status
    /// lines; it no longer owns this field).
    pub status: String,
    pub should_quit: bool,
    /// External edit queued for the event loop, which owns terminal
    /// suspension and knows which app callback should consume the saved file.
    pub pending_editor: Option<PendingEditor>,
}

impl Deref for AppView {
    type Target = App;
    fn deref(&self) -> &App {
        &self.core
    }
}

impl DerefMut for AppView {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.core
    }
}

fn load_background_mode(core: &App) -> BackgroundMode {
    core.db
        .load_settings()
        .ok()
        .and_then(|settings| {
            settings
                .into_iter()
                .find(|(key, _)| key == BackgroundMode::SETTING_KEY)
        })
        .and_then(|(_, value)| BackgroundMode::parse(&value))
        .unwrap_or_default()
}

impl AppView {
    /// Wrap a freshly-booted domain `App` with a fresh view layer.
    pub fn new(mut core: App) -> Self {
        // `App::new` queues the launch status line as an event; seed the
        // view's status field from it so the first frame reads correctly.
        let status = core
            .pop_pending_event()
            .map(|ev| match ev {
                AppEvent::Status(s) => s,
                _ => String::new(),
            })
            .unwrap_or_default();
        let background_mode = load_background_mode(&core);
        let mut theme = crate::theme::load();
        theme.set_background_mode(background_mode);
        Self {
            core,
            input: crate::composer::new_textarea(),
            clipboard: arboard::Clipboard::new().ok(),
            input_inner: Rect::default(),
            composer_click: None,
            composer_click_count: 0,
            composer_word_anchor: None,
            cmd_selected: 0,
            at_state: None,
            popup: Popup::None,
            spaces_cache: Vec::new(),
            space_selected: 0,
            space_filter: FilterInput::default(),
            space_mode: SpaceMode::Browse,
            space_edit: String::new(),
            session_selected: 0,
            session_filter: FilterInput::default(),
            session_mode: SessionMode::Browse,
            session_edit: String::new(),
            session_preview: None,
            files_selected: 0,
            files_mode: FilesMode::Browse,
            files_tab: FilesTab::Files,
            files_edit: String::new(),
            picker_dir: std::env::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            picker_entries: Vec::new(),
            picker_filter: String::new(),
            picker_selected: 0,
            images_selected: 0,
            images_mode: ImagesMode::Browse,
            scripts_selected: 0,
            scripts_mode: ScriptsMode::Browse,
            scripts_edit: String::new(),
            apps_selected: 0,
            apps_mode: AppsMode::Browse,
            apps_edit: String::new(),
            watch_selected: 0,
            watch_mode: WatchMode::Browse,
            swarm_selected: 0,
            swarm_popup_mode: SwarmPopupMode::Browse,
            skills_mode: SkillsMode::Browse,
            skills_selected: 0,
            skills_edit: String::new(),
            usage_data: None,
            usage_scroll: 0,
            key_input: String::new(),
            key_target: KeyTarget::OpenRouter,
            login_selected: 0,
            settings_selected: 0,
            settings_inputs: Default::default(),
            settings_collapsed: HashSet::new(),
            model_filter: FilterInput::default(),
            model_backend_filter: None,
            model_focus: ModelPanel::Available,
            fav_selected: 0,
            avail_selected: 0,
            fav_offset: 0,
            avail_offset: 0,
            copy_options: Vec::new(),
            copy_selected: 0,
            scroll: 0,
            max_scroll: 0,
            prev_total: 0,
            prev_tail: Vec::new(),
            pin_viewport_top: false,
            sel: HistorySel::default(),
            mouse_target: MouseTarget::None,
            history_cache: HistoryCache::default(),
            session_caches: HashMap::new(),
            show_tool_detail: false,
            notification_areas: Vec::new(),
            banner: nexus_core::config::load_banner()
                .unwrap_or_else(|| BANNER.trim_matches('\n').to_string()),
            greeting: nexus_core::app::pick_greeting(),
            theme,
            background_mode,
            theme_link: crate::theme::current_link_target(),
            theme_gen: 0,
            status,
            should_quit: false,
            pending_editor: None,
        }
    }

    /// Apply one view-side event to this layer's state. Domain handlers run
    /// separately in the event loop; these are the events that carry UI
    /// feedback from domain paths (status lines, composer restore, viewport
    /// and render-cache invalidation, the login fallback).
    pub fn apply_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::Status(s) => self.status.clone_from(s),
            AppEvent::ComposerSet(s) => self.set_input(s),
            AppEvent::ComposerClear => self.clear_input(),
            AppEvent::ViewportReset => {
                self.scroll = 0;
                self.max_scroll = 0;
                self.prev_total = 0;
                self.prev_tail.clear();
                self.sel.clear();
            }
            AppEvent::HistoryInvalidated => self.invalidate_history_cache(),
            AppEvent::OpenLoginPopup => self.open_login_popup(),
            _ => {}
        }
    }

    /// Change and persist the TUI background mode. With no argument, cycles
    /// between opaque and transparent; explicit values are useful for scripts
    /// and command history.
    pub fn set_background_mode(&mut self, requested: &str) -> Result<()> {
        let requested = requested.trim();
        let mode = if requested.is_empty() {
            self.background_mode.next()
        } else {
            let Some(mode) = BackgroundMode::parse(requested) else {
                bail!(
                    "unknown theme background {requested:?} — use /theme opaque or /theme transparent"
                );
            };
            mode
        };
        self.core
            .db
            .set_setting(BackgroundMode::SETTING_KEY, mode.key())?;
        self.background_mode = mode;
        self.theme.set_background_mode(mode);
        self.theme_gen = self.theme_gen.wrapping_add(1);
        self.invalidate_history_cache();
        self.push_status(format!("theme background: {}", mode.label()));
        Ok(())
    }

    /// Force a full history cache rebuild on the next frame. Used when
    /// in-place message edits would otherwise leave stale wrapped content.
    pub fn invalidate_history_cache(&mut self) {
        if let Some(sid) = self.core.session.as_ref().map(|s| s.id.clone()) {
            self.session_caches.remove(&sid);
        }
        self.history_cache = HistoryCache::default();
    }

    /// If the given rendered line index is an image or video thumbnail line,
    /// open it in the default OS viewer. For video thumbnails (`_first.png`),
    /// opens the sibling `.mp4` instead.
    pub fn open_image_at_line(&self, line: usize) -> bool {
        if let Some(Some(path)) = self.history_cache.image_at_line.get(line) {
            let p = std::path::Path::new(path);
            let open_path = if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                // Check for sibling video: abc123_first.png → abc123.mp4 or _stitch_abc123.mp4
                if let Some(base) = stem
                    .strip_suffix("_first")
                    .or_else(|| stem.strip_suffix("_last"))
                {
                    let dir = p.parent().unwrap_or_else(|| std::path::Path::new(""));
                    let direct = dir.join(format!("{base}.mp4"));
                    let stitched = dir.join(format!("_stitch_{base}.mp4"));
                    if direct.exists() {
                        direct.to_string_lossy().to_string()
                    } else if stitched.exists() {
                        stitched.to_string_lossy().to_string()
                    } else {
                        path.clone()
                    }
                } else {
                    path.clone()
                }
            } else {
                path.clone()
            };
            let _ = open::that_detached(&open_path);
            true
        } else {
            false
        }
    }

    /// `o` in the history pane: open the `[n]` citation under the current
    /// text selection, resolved against the Sources list of the message the
    /// selection belongs to (Ctrl+O navigates session links instead).
    /// Selection state is read here; the domain resolves the link.
    pub fn open_session_link(&mut self) {
        let owner = self.sel.owner_at_selection_start();
        self.core.open_session_link(owner);
    }

    /// Copy arbitrary text to the clipboard and report it in the status line.
    pub fn copy_text(&mut self, text: &str) {
        let msg = crate::composer::copy_to_clipboard(&mut self.clipboard, text);
        if !msg.is_empty() {
            self.push_status(msg);
        }
    }

    /// Copy a message's exact original content by its index into
    /// `core.messages` (streaming reply uses index `messages.len()`) — not
    /// the on-screen, wrap-reconstructed text a long-press selects for
    /// highlighting.
    pub fn copy_message(&mut self, idx: usize) {
        let text = match self.core.messages.get(idx) {
            Some(m) if m.role == "assistant" => Some(nexus_core::markdown::to_plain(&m.content)),
            Some(m) => Some(m.content.clone()),
            None if idx == self.core.messages.len() => {
                self.core.active_streaming_text().map(str::to_string)
            }
            None => None,
        };
        if let Some(t) = text {
            self.copy_text(&t);
        }
    }

    /// The settings popup's text-index helper (moved with the popup state).
    /// Index into `settings_inputs` for any typed (non-toggle, non-picker)
    /// field. `None` both for non-text fields and when a group header is
    /// selected.
    pub fn text_index(&self) -> Option<usize> {
        match self.settings_field()? {
            SettingsField::ShowStats
            | SettingsField::ShowReasoning
            | SettingsField::HideHints
            | SettingsField::MemoryModel
            | SettingsField::Verbosity
            | SettingsField::SearchProvider
            | SettingsField::TranscriberModel
            | SettingsField::OcrModel
            | SettingsField::OcrEngine
            | SettingsField::ImageGenModel
            | SettingsField::VideoGenModel => None,
            SettingsField::Temperature => Some(0),
            SettingsField::TopP => Some(1),
            SettingsField::MaxTokens => Some(2),
            SettingsField::CompactThreshold => Some(3),
            SettingsField::SearxngUrl => Some(4),
            SettingsField::LangsearchKey => Some(5),
            SettingsField::BlockedDomains => Some(7),
            SettingsField::EmbeddingModel => Some(6),
        }
    }

    /// Ctrl+↑: open the live per-searcher activity view. Caller already
    /// gates this on a research job running.
    pub fn open_research_live(&mut self) {
        self.core.research_live_input.clear();
        self.popup = Popup::ResearchLive;
    }
}
