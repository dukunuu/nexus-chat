//! The TUI's view layer: `AppView` wraps the domain `App` plus all UI state —
//! composer, popup chrome and caches, render state, theme, status line.
//! `Deref<Target = App>` resolves view-owned fields here and falls through to
//! the domain for everything else. UI feedback comes back as `AppEvent`s,
//! which `apply_event` applies to this layer.

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
use nexus_core::remote_image::{Fetcher, Status as ImageStatus};
use std::sync::{Arc, Weak};

thread_local! {
    // The terminal renders on one thread. A weak context lets all measured
    // layout helpers use this view's fetcher without retaining a closed view.
    static IMAGE_CONTEXT: std::cell::RefCell<Weak<Fetcher>> = const { std::cell::RefCell::new(Weak::new()) };
}

/// Test hook: render on this thread against `fetcher`.
#[cfg(test)]
pub(crate) fn use_fetcher_for_tests(fetcher: &Arc<Fetcher>) {
    IMAGE_CONTEXT.with_borrow_mut(|context| *context = Arc::downgrade(fetcher));
}

pub(crate) fn remote_image(url: &str) -> ImageStatus {
    IMAGE_CONTEXT.with_borrow(|context| {
        context
            .upgrade()
            .map_or(ImageStatus::Off, |fetcher| fetcher.request(url))
    })
}

/// One row standing in for an image that is not on screen: `▣ alt · loading`,
/// `▣ alt · host (couldn't load)`, or `▣ alt · host` when images are off. It
/// never exceeds `max_width`: the host goes first, then the alt text is cut.
pub(crate) fn image_placeholder(
    url: &str,
    alt: &str,
    status: &ImageStatus,
    theme: &Theme,
    max_width: usize,
) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};
    let host = reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();
    let alt = crate::ui::markdown::terminal_safe(alt).into_owned();
    let lead = format!("{} ", crate::ui::style::glyph::MEDIA);
    let full = match status {
        ImageStatus::Loading => " \u{b7} loading".to_string(),
        ImageStatus::Failed => format!(" \u{b7} {host} (couldn't load)"),
        _ => format!(" \u{b7} {host}"),
    };
    let short = match status {
        ImageStatus::Loading => full.clone(),
        ImageStatus::Failed => " \u{b7} couldn't load".to_string(),
        _ => String::new(),
    };
    let count = |text: &str| text.chars().count();
    let room = |suffix: &str| max_width.saturating_sub(count(&lead) + count(suffix));
    let suffix = if count(&alt) <= room(&full) || room(&full) >= 8 {
        full
    } else {
        short
    };
    let budget = room(&suffix);
    let alt = if count(&alt) > budget {
        let keep = budget.saturating_sub(1);
        format!("{}\u{2026}", alt.chars().take(keep).collect::<String>())
    } else {
        alt
    };
    let alt = if budget == 0 { String::new() } else { alt };
    Line::from(vec![
        Span::raw(format!("{lead}{alt}")),
        Span::styled(suffix, ratatui::style::Style::default().fg(theme.fg_dim)),
    ])
}

thread_local! {
    static PIXELS: std::cell::RefCell<HashMap<(std::path::PathBuf, u16, u16), Vec<ratatui::text::Line<'static>>>> =
        std::cell::RefCell::default();
}

/// Half-block rows for a cached PNG, memoized so a layout does not decode the
/// file on every frame.
fn pixel_lines(
    path: &std::path::Path,
    width: u16,
    rows: u16,
) -> Option<Vec<ratatui::text::Line<'static>>> {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    let key = (path.to_path_buf(), width, rows);
    if let Some(hit) = PIXELS.with_borrow(|cache| cache.get(&key).cloned()) {
        return Some(hit);
    }
    let image = image::open(path).ok()?;
    let pixels = image
        .thumbnail(u32::from(width), u32::from(rows) * 2)
        .to_rgb8();
    let lines: Vec<_> = (0..pixels.height())
        .step_by(2)
        .map(|y| {
            Line::from(
                (0..pixels.width())
                    .map(|x| {
                        let top = pixels.get_pixel(x, y).0;
                        let bottom = pixels.get_pixel(x, (y + 1).min(pixels.height() - 1)).0;
                        Span::styled(
                            "\u{2580}",
                            Style::default()
                                .fg(Color::Rgb(top[0], top[1], top[2]))
                                .bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    PIXELS.with_borrow_mut(|cache| {
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, lines.clone());
    });
    Some(lines)
}

/// Fixed-height, cell-sized previews for tiles and thumbnails. Loading, failure
/// and disabled states occupy the exact same reservation as decoded pixels.
pub(crate) fn image_lines(
    url: &str,
    alt: &str,
    width: u16,
    rows: u16,
    theme: &Theme,
) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::text::Line;
    let rows = rows.min(20);
    let status = remote_image(url);
    let mut lines = Vec::new();
    if let ImageStatus::Ready(path) = &status
        && width > 0
        && rows > 0
        && let Some(pixels) = pixel_lines(path, width, rows)
    {
        lines = pixels;
    } else {
        lines.push(image_placeholder(
            url,
            alt,
            &status,
            theme,
            usize::from(width),
        ));
    }
    lines.resize(usize::from(rows), Line::default());
    lines
}

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
    /// Alt+↑/↓ recall: how far back (0 = newest) through this session's sent
    /// messages, and the draft stashed when recall started.
    pub recall: Option<usize>,
    pub recall_draft: String,
    /// `@` file autocomplete: (matches, selected, cursor byte offset of `@`).
    pub at_state: Option<(Vec<FileRow>, usize, usize)>,
    /// User shell completion uses the same floating menu as slash and @ lists.
    pub shell_state: Option<crate::composer::ShellCompletionState>,
    pub shell_completion_rx:
        Option<tokio::sync::oneshot::Receiver<crate::composer::ShellCompletionResult>>,
    /// Stable shell-history traversal and its original draft.
    pub shell_recall: Option<(Vec<String>, usize, String)>,

    // ── Popup chrome ──────────────────────────────────────────────────────
    /// Which modal popover, if any, is open.
    pub popup: Popup,
    /// The workspace action currently displayed for permission.
    pub approval_request: Option<nexus_core::tools::ApprovalRequest>,
    /// The core's `approval_shown` when `approval_request` was displayed, so
    /// the next of two identical queued requests still resets the guards.
    pub approval_displayed: u64,
    /// Wrapped-row offset and bounds from the last approval render.
    pub approval_scroll: std::cell::Cell<usize>,
    pub approval_max_scroll: std::cell::Cell<usize>,
    pub approval_page_height: std::cell::Cell<usize>,
    /// Where the docked approval panel was last drawn, so the wheel over it
    /// scrolls the request and over the conversation scrolls the transcript.
    pub approval_area: std::cell::Cell<Option<ratatui::layout::Rect>>,
    /// A shell command stays locked until its final wrapped row has been shown.
    pub approval_seen_end: std::cell::Cell<bool>,
    pub approval_opened_at: Option<std::time::Instant>,
    /// Brief input guard, injectable so approval tests need not sleep.
    pub approval_delay: std::time::Duration,
    /// Model-authored UI state, isolated by message and source revision.
    pub interactive: crate::interactive::Widgets,
    /// One disposable scripted terminal app instance.
    pub tui_app: Option<Box<crate::ui::popups::tui_app::TuiApp>>,
    pub spaces_cache: Vec<SpaceRow>,
    pub space_selected: usize,
    pub space_filter: FilterInput,
    pub space_mode: SpaceMode,
    pub space_edit: String,
    pub session_selected: usize,
    /// Fuzzy filter typed in the session picker (matches title, slug, and id).
    pub session_filter: FilterInput,
    pub session_mode: SessionMode,
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
    /// Selected failure mode in the `/harness` popup.
    pub harness_selected: usize,
    pub jobs_popup: crate::ui::popups::jobs::JobsPopup,
    /// The `/connectors` popup: mode, cursors, add form and import rows.
    pub connectors_popup: crate::ui::popups::connectors::ConnectorsPopup,
    /// Background-process popup selection, output view and kill confirmation.
    pub procs_selected: usize,
    pub procs_output: Option<String>,
    pub procs_scroll: u16,
    pub procs_kill_confirm: Option<String>,
    /// `/help` popup scroll offset (clamped at render time).
    pub help_scroll: u16,
    pub key_input: String,
    /// Which backend the current `Popup::Key` entry is for.
    pub key_target: KeyTarget,
    pub login_selected: usize,
    /// Cursor in the `/local` runtime selector, and whether it was reached
    /// through `/login` — Esc goes back there, but closes for a bare `/local`.
    pub local_selected: usize,
    pub local_from_login: bool,
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
    pub copy_options: Vec<CopyOption>,
    pub copy_selected: usize,

    // ── Render / view ─────────────────────────────────────────────────────
    /// Lines scrolled up from the bottom (0 = following the newest lines).
    pub scroll: usize,
    /// Max useful `scroll` (lines above the viewport), refreshed each render so
    /// scrolling can be clamped instead of running off into empty space.
    pub max_scroll: usize,
    /// Rows the history pane showed last frame — the PgUp/PgDn page size.
    pub history_height: usize,
    /// The popup list drawn last frame (set by `chrome::render_list`,
    /// cleared at the start of each frame) — maps mouse clicks to rows.
    pub list_hit: std::cell::Cell<Option<crate::ui::popups::chrome::ListHit>>,
    /// The centered popup drawn last frame (set by `chrome::popup_area`,
    /// cleared at the start of each frame) — where a border press resizes it.
    pub popup_hit: std::cell::Cell<Option<crate::ui::popups::chrome::PopupHit>>,
    /// A column or popup-border drag in progress, and the last border press
    /// (for double-click-to-reset).
    pub drag: Option<crate::resize::Drag>,
    pub edge_click: Option<(std::time::Instant, crate::resize::Grip)>,
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
    /// Shared across replies, layouts and search cards; never synced.
    pub remote_images: Arc<Fetcher>,
    /// Decoded half-block rows for images in the reply that is still streaming,
    /// so they are not re-decoded every frame.
    pub stream_image_cache:
        std::cell::RefCell<HashMap<(String, usize), Vec<ratatui::text::Line<'static>>>>,
    /// Disposable, background-rendered TUI diagram previews.
    pub diagrams: crate::diagrams::DiagramCache,
    /// The full-screen in-terminal image viewer's state and background jobs.
    pub image_viewer: crate::image_viewer::Viewer,
    /// Per-session history caches preserved across session switches so
    /// switching back doesn't re-wrap every message from scratch.
    pub session_caches: HashMap<String, HistoryCache>,
    /// Whether tool-call blocks show full arguments/results (Ctrl+T).
    pub show_tool_detail: bool,
    /// Per-message overrides of the global tool-detail flag.
    pub tool_open: HashMap<usize, bool>,
    /// Individually opened batch sub-cards, keyed by message and call index.
    pub batch_open: HashSet<(usize, usize)>,
    /// Last focused batch row; Enter toggles it while the composer is empty.
    pub batch_focus: Option<(usize, usize)>,
    /// Monotonic render generation for tool-card changes.
    pub tool_open_gen: usize,
    /// The linked studio window and the comments it sent (`crate::studio`).
    pub studio: crate::studio::Studio,
    /// Session whose message indices the overrides refer to.
    pub tool_session: Option<String>,
    /// Clicked message and its previous block start, for in-place folding.
    pub tool_click_anchor: Option<(usize, Option<usize>, usize)>,
    pub notification_areas: Vec<(Rect, usize)>,
    /// The welcome screen's recent-session rows from the last frame, as
    /// (row rect, session id) — click targets, in `Alt+1…` order.
    pub welcome_targets: Vec<(Rect, String)>,
    /// Start-screen banner (custom or built-in) and a greeting picked at launch.
    pub banner: String,
    pub greeting: &'static str,
    /// Color palette — omarchy's theme, else Ghostty's, else the built-in
    /// ANSI palette. `theme_link` is the last-seen `theme::source_stamp`,
    /// polled by the event loop to pick up a theme switch live.
    pub theme: Theme,
    /// How the TUI surface background is painted; persisted per device and
    /// changed with `/theme opaque` or `/theme transparent`.
    pub background_mode: BackgroundMode,
    pub theme_link: String,
    /// Bumped every time `theme` changes, so the history render cache (which
    /// bakes colors into cached `Line`s) knows to re-wrap on a theme switch.
    pub theme_gen: usize,

    // ── Flow chrome ───────────────────────────────────────────────────────
    /// One-line status, fed by `AppEvent::Status`.
    pub status: String,
    /// When `status` was set; it clears itself after [`STATUS_TTL`] (errors
    /// after [`ERROR_STATUS_TTL`]) so stale lines don't linger.
    pub status_at: Option<std::time::Instant>,
    pub should_quit: bool,
    /// Set by a Ctrl+C that had nothing to undo; a second press soon after
    /// quits. Any other key disarms it.
    pub quit_armed_at: Option<std::time::Instant>,
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
    // Long by design (one initializer per view-state field).
    #[allow(clippy::too_many_lines)]
    pub fn new(mut core: App) -> Self {
        core.interactive_ui = true;
        core.tui_app_renderer = Some(crate::ui::popups::tui_app::preview);
        core.interactive_approvals = true;
        // Rebuild with the approver and `propose_command` this frontend offers.
        core.refresh_toolbox();
        if let Err(error) = core.collect_job_notifications() {
            core.push_status(format!("jobs notifications: {error}"));
        }
        // `App::new` queues the launch status line as an event; seed the
        // view's status field from it so the first frame reads correctly.
        let status = core
            .pop_pending_event()
            .map(|ev| match ev {
                AppEvent::Status(s) => s,
                _ => String::new(),
            })
            .unwrap_or_default();
        let remote_images = Fetcher::new(core.space.db_path().with_file_name("cache"));
        // Tests stay hermetic: the default fetcher never downloads; image tests
        // install their own seeded fetcher with `use_fetcher_for_tests`.
        #[cfg(test)]
        remote_images.set_enabled(false);
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
            recall: None,
            recall_draft: String::new(),
            at_state: None,
            shell_state: None,
            shell_completion_rx: None,
            shell_recall: None,
            popup: Popup::None,
            approval_request: None,
            approval_displayed: 0,
            approval_scroll: std::cell::Cell::new(0),
            approval_area: std::cell::Cell::new(None),
            approval_max_scroll: std::cell::Cell::new(0),
            approval_page_height: std::cell::Cell::new(1),
            approval_seen_end: std::cell::Cell::new(false),
            approval_opened_at: None,
            approval_delay: if cfg!(test) {
                std::time::Duration::ZERO
            } else {
                std::time::Duration::from_millis(400)
            },
            interactive: crate::interactive::Widgets::default(),
            tui_app: None,
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
            harness_selected: 0,
            jobs_popup: crate::ui::popups::jobs::JobsPopup::default(),
            connectors_popup: crate::ui::popups::connectors::ConnectorsPopup::default(),
            procs_selected: 0,
            procs_output: None,
            procs_scroll: 0,
            procs_kill_confirm: None,
            help_scroll: 0,
            key_input: String::new(),
            key_target: KeyTarget::OpenRouter,
            login_selected: 0,
            local_selected: 0,
            local_from_login: false,
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
            history_height: 0,
            list_hit: std::cell::Cell::new(None),
            popup_hit: std::cell::Cell::new(None),
            drag: None,
            edge_click: None,
            prev_total: 0,
            prev_tail: Vec::new(),
            pin_viewport_top: false,
            sel: HistorySel::default(),
            mouse_target: MouseTarget::None,
            history_cache: HistoryCache::default(),
            remote_images,
            stream_image_cache: std::cell::RefCell::default(),
            diagrams: crate::diagrams::DiagramCache::default(),
            image_viewer: crate::image_viewer::Viewer::default(),
            session_caches: HashMap::new(),
            show_tool_detail: false,
            tool_open: HashMap::new(),
            batch_open: HashSet::new(),
            batch_focus: None,
            tool_open_gen: 0,
            studio: crate::studio::Studio::default(),
            tool_session: None,
            tool_click_anchor: None,
            notification_areas: Vec::new(),
            welcome_targets: Vec::new(),
            banner: nexus_core::config::load_banner()
                .unwrap_or_else(|| BANNER.trim_matches('\n').to_string()),
            greeting: nexus_core::app::pick_greeting(),
            theme,
            background_mode,
            theme_link: crate::theme::source_stamp(),
            theme_gen: 0,
            status,
            status_at: Some(std::time::Instant::now()),
            should_quit: false,
            quit_armed_at: None,
            pending_editor: None,
        }
    }

    /// Apply one view-side event to this layer's state. Domain handlers run
    /// separately in the event loop; these are the events that carry UI
    /// feedback from domain paths (status lines, composer restore, viewport
    /// and render-cache invalidation, the login fallback).
    pub fn apply_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::Status(s) => {
                self.status.clone_from(s);
                self.status_at = Some(std::time::Instant::now());
            }
            AppEvent::ComposerSet(s) => self.set_input(s),
            AppEvent::ComposerClear => self.clear_input(),
            AppEvent::ViewportReset => {
                self.shell_state = None;
                self.shell_completion_rx = None;
                self.shell_recall = None;
                self.scroll = 0;
                self.max_scroll = 0;
                self.prev_total = 0;
                self.prev_tail.clear();
                self.tool_open.clear();
                self.batch_open.clear();
                self.batch_focus = None;
                self.tool_open_gen = self.tool_open_gen.wrapping_add(1);
                self.tool_click_anchor = None;
                self.diagrams.clear();
                self.interactive.clear();
                if self.popup == Popup::InteractiveUi {
                    self.popup = Popup::None;
                }
                self.sel.clear();
            }
            AppEvent::HistoryInvalidated => self.invalidate_history_cache(),
            AppEvent::OpenLoginPopup if self.popup != Popup::Approval => self.open_login_popup(),
            _ => {}
        }
        self.sync_approval_popup();
    }

    pub(crate) fn sync_approval_popup(&mut self) {
        if self
            .pending_approval
            .as_ref()
            .is_some_and(tokio::sync::oneshot::Sender::is_closed)
        {
            self.pending_approval = None;
            self.pending_approval_request = None;
        }
        self.show_next_approval();
        if let Some(request) = &self.pending_approval_request {
            if self.popup != Popup::Approval
                || self.approval_request.as_ref() != Some(request)
                || self.approval_displayed != self.approval_shown
            {
                self.approval_request = Some(request.clone());
                self.approval_displayed = self.approval_shown;
                self.approval_scroll.set(0);
                self.approval_max_scroll.set(0);
                self.approval_page_height.set(1);
                self.approval_seen_end.set(false);
                self.approval_opened_at = Some(std::time::Instant::now());
                self.popup = Popup::Approval;
            }
        } else if self.popup == Popup::Approval && self.pending_approval.is_none() {
            self.approval_request = None;
            self.popup = Popup::None;
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
    pub(crate) fn activate_image_context(&self) -> usize {
        self.remote_images
            .set_enabled(self.core.settings.remote_images);
        IMAGE_CONTEXT.with_borrow_mut(|context| *context = Arc::downgrade(&self.remote_images));
        self.remote_images.generation()
    }

    pub fn invalidate_history_cache(&mut self) {
        if let Some(sid) = self.core.session.as_ref().map(|s| s.id.clone()) {
            self.session_caches.remove(&sid);
        }
        self.history_cache = HistoryCache::default();
    }

    /// Ctrl+O: navigate to the session linked in the message under the
    /// selection. Selection state is read here; the domain resolves the link.
    pub fn open_session_link(&mut self) {
        let owner = self.sel.owner_at_selection_start();
        self.core.open_session_link(owner);
    }

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

    /// Index into `settings_inputs` for any typed (non-toggle, non-picker)
    /// field. `None` both for non-text fields and when a group header is
    /// selected.
    pub fn text_index(&self) -> Option<usize> {
        match self.settings_field()? {
            SettingsField::RemoteImages
            | SettingsField::ShowStats
            | SettingsField::ShowReasoning
            | SettingsField::HideHints
            | SettingsField::MemoryModel
            | SettingsField::Verbosity
            | SettingsField::SearchProvider
            | SettingsField::TranscriberModel
            | SettingsField::OcrModel
            | SettingsField::OcrEngine
            | SettingsField::ImageGenModel
            | SettingsField::VideoGenModel
            | SettingsField::ReplyStyle
            | SettingsField::ImageViewer
            | SettingsField::ColumnWidth => None,
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

/// How long an ordinary status line stays up.
pub const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(10);
/// Errors stay long enough to read after looking back at the terminal.
pub const ERROR_STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

impl AppView {
    pub fn status_deadline(&self) -> Option<std::time::Instant> {
        let at = self.status_at?;
        let lower = self.status.to_lowercase();
        let ttl = if lower.contains("error") || lower.contains("failed") {
            ERROR_STATUS_TTL
        } else {
            STATUS_TTL
        };
        Some(at + ttl)
    }

    pub fn expire_status(&mut self) {
        if self
            .status_deadline()
            .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.status.clear();
            self.status_at = None;
        }
    }
}

impl AppView {
    /// One history page for PgUp/PgDn: the visible rows minus a two-line
    /// overlap so the reader keeps their place.
    pub fn history_page(&self) -> usize {
        self.history_height.saturating_sub(2).max(1)
    }
}

impl AppView {
    /// Open the `n`th (0-based) recent session listed on the welcome screen.
    pub fn open_welcome_session(&mut self, n: usize) -> anyhow::Result<()> {
        let Some((_, id)) = self.welcome_targets.get(n).cloned() else {
            return Ok(());
        };
        self.core.switch_to_session_by_id(&id)
    }
}
