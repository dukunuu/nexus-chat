use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::Utc;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::ListState;
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::config;
use crate::input::{COMMANDS, new_textarea};
use crate::db::{Db, Message, Session, Space as SpaceRow, DEFAULT_SPACE};
use crate::provider::openrouter::OpenRouter;
use crate::provider::{Model, StreamEvent, Usage};
use crate::space::Space;

mod apps;
mod chat;
mod compaction;
mod copy;
mod files;
mod memory;
mod models;
mod sessions;
mod settings;
mod skills_popup;
mod spaces;
#[cfg(test)]
mod tests;
mod transcribe;
#[cfg(test)]
use chat::split_inline_reasoning;
use chat::{code_blocks, pick_greeting};
pub(crate) use chat::human_size;
#[cfg(test)]
use memory::{parse_fact_line, parse_memory_ops};
use sessions::parse_topic;

/// Nudge a bounded selection index by `delta`, hard-clamping to `[0, len-1]`
/// (or `0` if `len` is `0`). Shared by the picker/list selection-movement
/// methods that clamp at the ends rather than wrapping around.
pub(super) fn clamp_cursor(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    (current as i32 + delta).clamp(0, len as i32 - 1) as usize
}

/// Filter `items` down to those `score_fn` returns `Some` for, sorted
/// descending by score (best match first, stable on ties). Shared by the
/// space and session pickers' fuzzy filters.
pub(super) fn fuzzy_filter_sorted<'a, T>(
    items: &'a [T],
    score_fn: impl Fn(&T) -> Option<i32>,
) -> Vec<&'a T> {
    let mut scored: Vec<(i32, &T)> = items
        .iter()
        .filter_map(|item| score_fn(item).map(|sc| (sc, item)))
        .collect();
    scored.sort_by_key(|(sc, _)| std::cmp::Reverse(*sc));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// Which modal popover, if any, is open.
#[derive(PartialEq)]
pub enum Popup {
    None,
    Model,
    Session,
    Key,
    Settings,
    Copy,
    Space,
    Context,
    Skills,
    Files,
    Apps,
}

/// What the apps popup is doing: browsing the space's apps or confirming
/// removal of the highlighted one.
#[derive(PartialEq, Clone, Copy)]
pub enum AppsMode {
    Browse,
    ConfirmDelete,
}

/// What the skills popup is doing: browsing, typing a GitHub `owner/repo/path`
/// to install, or confirming removal of the highlighted skill.
#[derive(PartialEq, Clone, Copy)]
pub enum SkillsMode {
    Browse,
    Install,
    ConfirmRemove,
}

/// What the files popup is doing: browsing the fileset, typing a path to
/// import, renaming the highlighted file, or confirming its removal.
#[derive(PartialEq, Clone, Copy)]
pub enum FilesMode {
    Browse,
    Add,
    Rename,
    ConfirmDelete,
    Pick,
}

/// What the space picker is doing: browsing, naming a new space, renaming the
/// highlighted one, or confirming a delete.
#[derive(PartialEq, Clone, Copy)]
pub enum SpaceMode {
    Browse,
    Create,
    Rename,
    ConfirmDelete,
}

/// One entry in the `/copy` menu: what to show and the text it puts on the clipboard.
pub struct CopyOption {
    pub label: String,
    pub text: String,
}

/// Token estimate breakdown shown in the context popup (Ctrl+I).
pub struct ContextBreakdown {
    pub system_tokens: u64,
    pub memory_tokens: u64,
    pub skills_tokens: u64,
    pub conversation_tokens: u64,
    pub limit: Option<u64>,
    /// Whether the session has ever been auto-compacted.
    pub compacted: bool,
}

/// What the session picker is doing: browsing, renaming the highlighted row, or
/// confirming a delete.
#[derive(PartialEq, Clone, Copy)]
pub enum SessionMode {
    Browse,
    Rename,
    ConfirmDelete,
}

/// Which pane an in-progress mouse press is driving.
#[derive(PartialEq, Clone, Copy)]
pub enum MouseTarget {
    None,
    Input,
    History,
}

/// The two columns of the model picker.
#[derive(PartialEq, Clone, Copy)]
pub enum ModelPanel {
    Favorites,
    Available,
}

/// What a confirmed model picker selection is for: the active session's model,
/// or the background memory-extraction model.
#[derive(PartialEq, Clone, Copy, Default)]
pub enum ModelPickTarget {
    #[default]
    Session,
    Memory,
    Transcriber,
}

/// Editable rows in the nerd-config popup.
#[derive(PartialEq, Clone, Copy)]
pub enum SettingsField {
    ShowStats,
    ShowReasoning,
    HideHints,
    Temperature,
    TopP,
    MaxTokens,
    MemoryModel,
    CompactThreshold,
    SearxngUrl,
    Verbosity,
    LangsearchKey,
    SearchProvider,
    TranscriberModel,
}

impl SettingsField {
    pub const ALL: [SettingsField; 13] = [
        SettingsField::ShowStats,
        SettingsField::ShowReasoning,
        SettingsField::HideHints,
        SettingsField::Temperature,
        SettingsField::TopP,
        SettingsField::MaxTokens,
        SettingsField::MemoryModel,
        SettingsField::CompactThreshold,
        SettingsField::SearxngUrl,
        SettingsField::Verbosity,
        SettingsField::LangsearchKey,
        SettingsField::SearchProvider,
        SettingsField::TranscriberModel,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsField::ShowStats => "show stats (model · TPS footer)",
            SettingsField::ShowReasoning => "expand reasoning (Ctrl+R)",
            SettingsField::HideHints => "hide hints (keybind labels)",
            SettingsField::Temperature => "temperature",
            SettingsField::TopP => "top_p",
            SettingsField::MaxTokens => "max_tokens",
            SettingsField::MemoryModel => "memory model (Enter to pick, Backspace clears)",
            SettingsField::CompactThreshold => "auto-compact at (% of context, 0 disables)",
            SettingsField::SearxngUrl => "web search URL (SearXNG instance, blank disables)",
            SettingsField::Verbosity => "answer length (Space cycles normal/concise/caveman)",
            SettingsField::LangsearchKey => "LangSearch API key (langsearch.com/dashboard, free)",
            SettingsField::SearchProvider => "search provider (Space cycles auto/langsearch/searxng/duckduckgo)",
            SettingsField::TranscriberModel => "image model (Enter to pick, Backspace clears)",
        }
    }
}

const VERBOSITY_LEVELS: [&str; 3] = ["normal", "concise", "caveman"];
const SEARCH_PROVIDERS: [&str; 4] = ["auto", "langsearch", "searxng", "duckduckgo"];

/// Nerd config: footer toggles + core sampling parameters.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Show the per-message model · TPS footer.
    pub show_stats: bool,
    /// Expand stored reasoning traces (vs. a collapsed one-liner).
    pub show_reasoning: bool,
    /// Hide keybind hints (input hint, popup titles, "Ctrl+R to expand", …).
    pub hide_hints: bool,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Auto-compact once context usage crosses this percent of the model's
    /// context window. 0 disables auto-compaction.
    pub compact_threshold: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            show_stats: false,
            show_reasoning: false,
            hide_hints: false,
            temperature: None,
            top_p: None,
            max_tokens: None,
            compact_threshold: 60,
        }
    }
}

/// The three answer-length presets, cycled by the settings popup's
/// `verbosity` field. "concise" is the default — terse but not telegraphic;
/// "caveman" is deliberately more aggressive (dropped articles/grammar),
/// matching the community caveman-prompt technique, for users who want it.
fn verbosity_clause(level: &str) -> &'static str {
    match level {
        "normal" => "Answer at whatever length the question deserves — don't optimize for brevity over completeness.",
        "caveman" => "Talk caveman-terse. Drop articles (a/an/the) and filler words. Short fragments over full sentences. No politeness, no hedging, no restating the question. Symbols over words where clear (→, =, vs). Full explanations ONLY when explicitly asked — otherwise state the fact/answer and stop. Preserve numbers, code, names, and technical terms exactly.",
        _ => "Answer style: default to short. Say the answer, then stop — don't restate the question, don't add a summary, don't hedge (\"it's worth noting...\", \"generally speaking...\"). No preamble (\"Great question!\", \"I'd be happy to help with that\"), no postamble (\"Let me know if you have questions!\"). One clear sentence beats three vague ones.\nThis is a floor, not a ceiling: if the user asks to explain, teach, or go deep, or the topic genuinely needs multiple steps to be correct (debugging, multi-part instructions, tradeoffs), give it the room it needs. Brevity for its own sake that omits a needed step is wrong, not concise.\nKeep full grammar — this isn't telegraphic shorthand. Drop filler, not clarity.",
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Built-in start-screen banner (override with `~/.config/nexus-chat/banner.txt`).
const BANNER: &str = r"
███╗   ██╗███████╗██╗  ██╗██╗   ██╗███████╗
████╗  ██║██╔════╝╚██╗██╔╝██║   ██║██╔════╝
██╔██╗ ██║█████╗   ╚███╔╝ ██║   ██║███████╗
██║╚██╗██║██╔══╝   ██╔██╗ ██║   ██║╚════██║
██║ ╚████║███████╗██╔╝ ██╗╚██████╔╝███████║
╚═╝  ╚═══╝╚══════╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝";

/// Greeting lines for the start screen; one is picked at random per launch.
const GREETINGS: [&str; 8] = [
    "What are we building today?",
    "Ask me anything.",
    "Fresh session, fresh ideas.",
    "The terminal is your canvas.",
    "Ready when you are.",
    "Type / to see commands.",
    "Let's get to work.",
    "How can I help?",
];

/// Dumb little "thinking" verbs, Claude-Code style: (present, past). One pair is
/// picked per request — "⠹ Vibing" while streaming, "Vibed for 3s" once done.
const THINKING: [(&str, &str); 16] = [
    ("Thinking", "Thought"),
    ("Pondering", "Pondered"),
    ("Noodling", "Noodled"),
    ("Ruminating", "Ruminated"),
    ("Cogitating", "Cogitated"),
    ("Marinating", "Marinated"),
    ("Percolating", "Percolated"),
    ("Mulling", "Mulled"),
    ("Conjuring", "Conjured"),
    ("Vibing", "Vibed"),
    ("Scheming", "Schemed"),
    ("Wrangling tokens", "Wrangled tokens"),
    ("Brewing", "Brewed"),
    ("Musing", "Mused"),
    ("Galaxy-braining", "Galaxy-brained"),
    ("Doing the thing", "Did the thing"),
];

/// Palette the spinner colour is randomly drawn from each request.
const SPINNER_COLORS: [Color; 6] = [
    Color::Green,
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Blue,
    Color::LightRed,
];

type ModelsResult = std::result::Result<Vec<Model>, String>;

/// One memory-extraction op, as emitted by the memory model.
pub(crate) enum MemoryOp {
    Add(String),
    Update(usize, String),
    Delete(usize),
}

/// A background event surfaced to the event loop. `None` means that source's
/// channel closed (task ended).
pub enum AppEvent {
    Stream(Option<StreamEvent>),
    Models(Option<ModelsResult>),
    /// A generated session topic: (session id, title, slug).
    Title(Option<(String, String, String)>),
    /// Extracted memory ops for a space, tagged with the space name so a
    /// meanwhile space-switch can discard stale results.
    Memory(Option<(String, Vec<MemoryOp>)>),
    /// A compaction digest: (session id, digest, messages covered, pre-compaction %).
    Compact(Option<(String, String, i64, u64)>),
    /// Result of `/skills` install: skill name on success, error message on failure.
    SkillInstall(Option<Result<String, String>>),
    /// One image-description result (or the error), or `None` when the
    /// describe batch's channel closed (all images done).
    Described(Option<(String, std::result::Result<String, String>)>),
    /// A per-page progress or final OCR result for one scanned PDF, or `None`
    /// when the batch's channel closed.
    Ocr(Option<(String, String, files::OcrUpdate)>),
}

pub struct App {
    pub db: Db,
    pub(crate) space: Space,
    pub(crate) provider: Option<OpenRouter>,
    pub(crate) key: Option<String>,

    /// The space the current/next session belongs to.
    pub active_space: SpaceRow,
    pub spaces_cache: Vec<SpaceRow>,
    pub space_selected: usize,
    pub space_filter: String,
    pub space_mode: SpaceMode,
    pub space_edit: String,
    /// Model used for background memory extraction (empty = disabled).
    pub memory_model: String,
    /// Model used for image transcription (empty = disabled).
    pub transcriber_model: String,
    /// Base URL of a SearXNG instance for the web-search skill, or empty to
    /// disable it. Configured in-app (Ctrl+O settings), not a config file.
    pub searxng_url: String,
    /// LangSearch API key (free tier), or empty to disable it.
    pub langsearch_key: String,
    /// Which web-search backend to prefer: "auto"/"langsearch"/"searxng"/"duckduckgo".
    pub search_provider: String,
    /// Raw contents of `system_prompt.md` (with an unresolved `{{verbosity}}`
    /// placeholder) — the app's own base system prompt, `$EDITOR`-editable.
    pub base_system_prompt: String,
    /// Answer-length preference woven into the system prompt: "normal",
    /// "concise" (default), or "caveman".
    pub verbosity: String,
    pub(crate) memory_rx: Option<mpsc::UnboundedReceiver<(String, Vec<MemoryOp>)>>,
    /// Background compaction result: (session id, digest, messages-covered, pre-compaction %).
    pub(crate) compact_rx: Option<mpsc::UnboundedReceiver<(String, String, i64, u64)>>,

    /// Installed skills (name/description only — bodies are read from disk on
    /// invocation, so this list is cheap and reloaded whenever it changes).
    pub skills: Vec<crate::skills::Skill>,
    /// A skill armed by `/<skill-name>`, injected into the next message only.
    pub(crate) forced_skill: Option<String>,
    pub(crate) toolbox: std::sync::Arc<crate::tools::ToolBox>,
    /// Local static server for model-created apps (None if it failed to bind).
    pub app_server: Option<crate::appserver::AppServer>,
    pub skills_mode: SkillsMode,
    pub skills_selected: usize,
    /// GitHub `owner/repo/path` shorthand being typed in Install mode.
    pub skills_edit: String,
    pub(crate) skills_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
    /// Background image-description result channel: (message_images row id, description or error).
    pub(crate) describe_rx: Option<mpsc::UnboundedReceiver<(String, std::result::Result<String, String>)>>,
    /// Background OCR updates: (space_id, file name, progress or final result).
    pub(crate) ocr_rx: Option<mpsc::UnboundedReceiver<(String, String, files::OcrUpdate)>>,
    /// Images pasted from the clipboard, staged for the next message.
    pub pending_images: Vec<transcribe::PendingImage>,
    /// A message queued to send once its images finish being described.
    pub(crate) deferred_send: Option<String>,

    /// The active space's imported files (refreshed by `rescan_files`).
    pub files_cache: Vec<crate::db::FileRow>,
    pub files_selected: usize,
    pub files_mode: FilesMode,
    /// The space's apps (`/apps` popup): names, cursor, and mode.
    pub apps_cache: Vec<String>,
    pub apps_selected: usize,
    pub apps_mode: AppsMode,
    /// Path being typed/pasted in the files popup's Add mode.
    pub files_edit: String,
    /// Directory the file-picker browser is showing (remembered across opens).
    pub picker_dir: std::path::PathBuf,
    pub picker_entries: Vec<crate::app::files::PickerEntry>,
    pub picker_filter: String,
    pub picker_selected: usize,

    /// Live model catalog (fetched on demand, never hardcoded).
    pub models: Vec<Model>,
    pub current_model: Option<String>,
    pub(crate) models_rx: Option<mpsc::UnboundedReceiver<ModelsResult>>,
    /// Model ids marked favorite, and when each model was last used (rfc3339).
    pub favorites: HashSet<String>,
    pub last_used: HashMap<String, String>,
    /// Per-model reasoning effort ("low"/"medium"/"high").
    pub reasoning: HashMap<String, String>,

    pub session: Option<Session>,
    pub messages: Vec<Message>,

    /// Message composer. A real editor: cursor movement, word-jump, selection,
    /// cut/copy/paste, undo — all from tui-textarea's default keymap.
    pub input: TextArea<'static>,
    /// Long-lived OS clipboard handle. Kept alive so X11 keeps serving the
    /// contents to clipboard managers (recreating per-op drops ownership in ~1ms).
    pub clipboard: Option<arboard::Clipboard>,
    /// The composer's inner (inside-border) rect from the last render, so mouse
    /// clicks can be mapped to cursor positions.
    pub input_inner: Rect,
    /// In-progress assistant response while a completion streams.
    pub streaming: Option<String>,
    /// In-progress reasoning/thinking tokens for the current stream (transient).
    pub(crate) thinking_text: String,
    /// Label for a tool currently running mid-stream (e.g. "Searching the web…").
    pub tool_status: Option<String>,
    /// Whether tool-call blocks show full arguments/results (Ctrl+T).
    pub show_tool_detail: bool,
    /// Wrapped-line cache for the transcript, so redraws don't re-render
    /// markdown for the whole conversation every frame.
    pub(crate) history_cache: crate::ui::history::HistoryCache,
    /// File `/edit` wants opened in `$EDITOR`; the event loop (which owns the
    /// terminal) takes it and suspends the TUI.
    pub pending_editor: Option<std::path::PathBuf>,
    pub(crate) stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
    /// Abort handle for the in-flight chat task (Esc stops the response).
    pub(crate) stream_abort: Option<tokio::task::AbortHandle>,
    /// Wall-clock start of the current stream, for TPS.
    pub(crate) stream_started: Option<std::time::Instant>,
    /// Exact usage reported for the in-flight stream, if any.
    pub(crate) stream_usage: Option<Usage>,
    /// Exact conversation token total from the last completed response.
    pub(crate) context_total: Option<u64>,

    pub settings: Settings,
    /// Animated "thinking" indicator shown while a response streams.
    pub(crate) spinner_frame: usize,
    pub(crate) thinking_idx: usize,
    pub(crate) spinner_color: Color,

    pub popup: Popup,
    pub model_filter: String,
    pub model_focus: ModelPanel,
    /// What a confirmed selection in the model picker is currently for.
    pub model_pick_target: ModelPickTarget,
    pub fav_state: ListState,
    pub avail_state: ListState,
    pub sessions_cache: Vec<Session>,
    pub session_selected: usize,
    /// Fuzzy filter typed in the session picker (matches title, slug, and id).
    pub session_filter: String,
    /// Whether the picker is browsing, renaming, or confirming a delete.
    pub session_mode: SessionMode,
    /// Edit buffer while renaming a session.
    pub session_edit: String,
    /// Background topic-generation result channel.
    title_rx: Option<mpsc::UnboundedReceiver<(String, String, String)>>,
    /// `/copy` menu entries and the highlighted row.
    pub copy_options: Vec<CopyOption>,
    pub copy_selected: usize,
    pub key_input: String,
    pub settings_selected: usize,
    /// Text edit buffers for the numeric settings (temperature, top_p, max_tokens).
    pub settings_inputs: [String; 6],

    /// Highlighted row in the slash-command autocomplete popup.
    pub cmd_selected: usize,

    /// Start-screen banner (custom or built-in) and a greeting picked at launch.
    pub banner: String,
    pub greeting: &'static str,
    pub scroll: u16,
    /// Max useful `scroll` (lines above the viewport), refreshed each render so
    /// scrolling can be clamped instead of running off into empty space.
    pub max_scroll: u16,
    /// Mouse text-selection over the history pane.
    pub sel: crate::selection::HistorySel,
    /// Which pane a mouse press is currently interacting with, so drag/release
    /// route to the right place even when the cursor leaves the pane.
    pub mouse_target: MouseTarget,
    /// Composer double/triple-click tracking: (time, screen pos) of the last
    /// press, and its click count (2 = word select, 3+ = line select), so a
    /// drag that follows can extend by word/line instead of by char.
    pub composer_click: Option<(std::time::Instant, (u16, u16))>,
    pub composer_click_count: u8,
    /// Data-space cursor position at the start of a word-mode composer drag,
    /// so each drag step can re-select from that word outward.
    pub composer_word_anchor: Option<(usize, usize)>,
    pub status: String,
    pub should_quit: bool,
}

impl App {
    pub fn new(db: Db, key: Option<String>, space: Space) -> Self {
        let provider = key.clone().map(OpenRouter::new);
        let status = if key.is_some() {
            "loading models…  (/model to pick, /help for commands)".to_string()
        } else {
            "no API key — set it with /key (or $OPENROUTER_API_KEY)".to_string()
        };
        // Fall back to a fresh in-memory default row if the db lookup somehow
        // fails — the space name still resolves to real files on disk.
        let active_space = db.default_space_id().ok().and_then(|id| {
            db.list_spaces().ok().and_then(|s| s.into_iter().find(|s| s.id == id))
        }).unwrap_or_else(|| SpaceRow {
            id: String::new(),
            name: DEFAULT_SPACE.to_string(),
            created_at: Utc::now().to_rfc3339(),
        });
        let _ = space.ensure_space_dir(&active_space.name);
        let skills_dir = crate::skills::skills_dir(&space.root);
        let skills = crate::skills::load_skills(&skills_dir);
        // Built with search disabled; `load_settings()` below reads the
        // persisted config (if any) and rebuilds this via `refresh_toolbox`.
        let toolbox = std::sync::Arc::new(crate::tools::ToolBox::new(
            skills_dir,
            None,
            None,
            "auto".to_string(),
            Some(crate::tools::FilesCtx { db_path: space.db_path(), space_id: active_space.id.clone() }),
            // No apps ctx yet — the app server starts after construction;
            // main() calls refresh_toolbox() once it's up.
            None,
        ));
        let mut app = App {
            db,
            space,
            provider,
            key,
            skills,
            searxng_url: String::new(),
            langsearch_key: String::new(),
            search_provider: "auto".to_string(),
            forced_skill: None,
            toolbox,
            app_server: None,
            skills_mode: SkillsMode::Browse,
            skills_selected: 0,
            skills_edit: String::new(),
            skills_rx: None,
            describe_rx: None,
            ocr_rx: None,
            pending_images: Vec::new(),
            deferred_send: None,
            files_cache: Vec::new(),
            files_selected: 0,
            files_mode: FilesMode::Browse,
            apps_cache: Vec::new(),
            apps_selected: 0,
            apps_mode: AppsMode::Browse,
            files_edit: String::new(),
            picker_dir: std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
            picker_entries: Vec::new(),
            picker_filter: String::new(),
            picker_selected: 0,
            active_space,
            spaces_cache: Vec::new(),
            space_selected: 0,
            space_filter: String::new(),
            space_mode: SpaceMode::Browse,
            space_edit: String::new(),
            memory_model: "google/gemini-2.5-flash-lite".to_string(),
            transcriber_model: "google/gemini-2.5-flash-lite".to_string(),
            base_system_prompt: config::load_system_prompt().unwrap_or_default(),
            verbosity: "concise".to_string(),
            memory_rx: None,
            compact_rx: None,
            models: Vec::new(),
            current_model: None,
            models_rx: None,
            favorites: HashSet::new(),
            last_used: HashMap::new(),
            reasoning: HashMap::new(),
            session: None,
            messages: Vec::new(),
            input: new_textarea(),
            clipboard: arboard::Clipboard::new().ok(),
            input_inner: Rect::default(),
            streaming: None,
            thinking_text: String::new(),
            tool_status: None,
            show_tool_detail: false,
            history_cache: Default::default(),
            pending_editor: None,
            stream_rx: None,
            stream_abort: None,
            stream_started: None,
            stream_usage: None,
            context_total: None,
            settings: Settings::default(),
            spinner_frame: 0,
            thinking_idx: 0,
            spinner_color: Color::Green,
            popup: Popup::None,
            model_filter: String::new(),
            model_focus: ModelPanel::Available,
            model_pick_target: ModelPickTarget::Session,
            fav_state: ListState::default(),
            avail_state: ListState::default(),
            sessions_cache: Vec::new(),
            session_selected: 0,
            session_filter: String::new(),
            session_mode: SessionMode::Browse,
            session_edit: String::new(),
            title_rx: None,
            copy_options: Vec::new(),
            copy_selected: 0,
            key_input: String::new(),
            settings_selected: 0,
            settings_inputs: [
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
            cmd_selected: 0,
            banner: config::load_banner().unwrap_or_else(|| BANNER.trim_matches('\n').to_string()),
            greeting: pick_greeting(),
            scroll: 0,
            max_scroll: 0,
            sel: crate::selection::HistorySel::default(),
            mouse_target: MouseTarget::None,
            composer_click: None,
            composer_click_count: 0,
            composer_word_anchor: None,
            status,
            should_quit: false,
        };
        app.load_prefs();
        app.load_settings();
        app
    }

    /// Load favorites + last-used timestamps from the db (best effort), and
    /// default the active model to the most-recently-used one so a new session
    /// needs no re-selection.
    fn load_prefs(&mut self) {
        if let Ok(prefs) = self.db.load_model_prefs() {
            for p in prefs {
                if p.favorite {
                    self.favorites.insert(p.id.clone());
                }
                if let Some(t) = p.last_used {
                    self.last_used.insert(p.id.clone(), t);
                }
                if let Some(r) = p.reasoning {
                    self.reasoning.insert(p.id, r);
                }
            }
        }
        if let Some((id, _)) = self.last_used.iter().max_by(|a, b| a.1.cmp(b.1)) {
            let id = id.clone();
            if self.provider.is_some() {
                self.status = format!("model: {id} — type a message, /model to change");
            }
            self.current_model = Some(id);
        }
    }

    /// Load persisted nerd-config settings from the db (best effort).
    fn load_settings(&mut self) {
        let Ok(kv) = self.db.load_settings() else {
            return;
        };
        for (k, v) in kv {
            match k.as_str() {
                "show_stats" => self.settings.show_stats = v == "1",
                "show_reasoning" => self.settings.show_reasoning = v == "1",
                "hide_hints" => self.settings.hide_hints = v == "1",
                "temperature" => self.settings.temperature = v.parse().ok(),
                "top_p" => self.settings.top_p = v.parse().ok(),
                "max_tokens" => self.settings.max_tokens = v.parse().ok(),
                "memory_model" => self.memory_model = v,
                "transcriber_model" => self.transcriber_model = v,
                "compact_threshold" => {
                    if let Ok(t) = v.parse() {
                        self.settings.compact_threshold = t;
                    }
                }
                "searxng_url" => self.searxng_url = v,
                "verbosity" if VERBOSITY_LEVELS.contains(&v.as_str()) => self.verbosity = v,
                "langsearch_key" => self.langsearch_key = v,
                "search_provider" if SEARCH_PROVIDERS.contains(&v.as_str()) => self.search_provider = v,
                _ => {}
            }
        }
        self.refresh_toolbox();
    }

    /// Rebuild the toolbox from the current `searxng_url`, so a settings
    /// change takes effect immediately (no restart). Web search works either
    /// way (SearXNG if configured, DuckDuckGo scraping otherwise), so the
    /// built-in skill is always installed.
    pub(crate) fn refresh_toolbox(&mut self) {
        let url = (!self.searxng_url.trim().is_empty()).then(|| self.searxng_url.trim().to_string());
        let key = (!self.langsearch_key.trim().is_empty()).then(|| self.langsearch_key.trim().to_string());
        crate::skills::install_builtin(&self.toolbox.skills_dir);
        self.toolbox = std::sync::Arc::new(crate::tools::ToolBox::new(
            self.toolbox.skills_dir.clone(),
            url,
            key,
            self.search_provider.clone(),
            Some(crate::tools::FilesCtx {
                db_path: self.space.db_path(),
                space_id: self.active_space.id.clone(),
            }),
            // App tools only exist while the server runs — a write_file whose
            // link can never load is worse than no tool.
            self.app_server.as_ref().map(|s| crate::tools::AppsCtx {
                dir: self.space.apps_dir(&self.active_space.name),
                space_url: s.space_url(&self.active_space.name),
            }),
        ));
        self.reload_skills();
    }

    /// Kick off the initial model fetch if a key is already present. Call once
    /// after construction, from within the tokio runtime.
    pub fn init(&mut self) {
        if self.provider.is_some() {
            self.fetch_models();
        }
        self.rescan_files();
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming.is_some()
    }

    /// The empty start screen (banner + greeting + clock) shows when there's no
    /// conversation yet.
    pub fn is_welcome(&self) -> bool {
        self.messages.is_empty() && self.streaming.is_none()
    }

    /// Advance the spinner one frame (called on the animation tick).
    pub fn tick_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    /// Current spinner glyph.
    pub fn spinner_char(&self) -> &'static str {
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    /// Randomly-chosen spinner colour for the current response.
    pub fn spinner_color(&self) -> Color {
        self.spinner_color
    }

    /// Present-tense phrase for the in-progress response ("Vibing").
    pub fn thinking_phrase(&self) -> &'static str {
        THINKING[self.thinking_idx].0
    }

    /// Reasoning tokens accumulated so far this stream, if any.
    pub fn thinking_text(&self) -> Option<&str> {
        (!self.thinking_text.is_empty()).then_some(self.thinking_text.as_str())
    }

    // --- async event sources (drained by the event loop) ---

    /// Next background event from either the streaming task or a model fetch.
    /// Pends on an idle source, so it only resolves when something happens.
    pub async fn next_event(&mut self) -> AppEvent {
        tokio::select! {
            ev = async {
                match self.stream_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Stream(ev),
            res = async {
                match self.models_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Models(res),
            t = async {
                match self.title_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Title(t),
            m = async {
                match self.memory_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Memory(m),
            c = async {
                match self.compact_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Compact(c),
            r = async {
                match self.skills_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::SkillInstall(r),
            r = async {
                match self.describe_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Described(r),
            r = async {
                match self.ocr_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Ocr(r),
        }
    }

    fn fetch_models(&mut self) {
        let Some(provider) = self.provider.clone() else {
            return;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        self.models_rx = Some(rx);
        tokio::spawn(async move {
            let result = provider.list_models().await.map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    pub fn on_models_result(&mut self, result: Option<ModelsResult>) {
        self.models_rx = None;
        let result = match result {
            Some(r) => r,
            None => Err("model fetch cancelled".to_string()),
        };
        match result {
            Ok(models) => {
                let n = models.len();
                self.models = models;
                self.status = format!("loaded {n} models");
                // First key just landed and nothing picked yet → jump into the picker.
                if !self.models.is_empty()
                    && self.current_model.is_none()
                    && self.popup == Popup::None
                {
                    self.open_model_picker();
                }
            }
            Err(e) => self.status = format!("model fetch failed: {e}"),
        }
    }

    // --- input handling ---

    pub(crate) fn run_command(&mut self, cmd: &str) -> Result<()> {
        let token = cmd.split_whitespace().next().unwrap_or("");
        // Resolve aliases (e.g. "history" -> "session") to a canonical name.
        let canonical = COMMANDS
            .iter()
            .find(|c| c.name == token || c.aliases.contains(&token))
            .map(|c| c.name)
            .unwrap_or(token);
        match canonical {
            "quit" => self.should_quit = true,
            "new" => self.new_session()?,
            "compact" => self.force_compact(),
            "session" => self.open_session_picker()?,
            "space" => self.open_space_picker()?,
            "model" => self.open_model_picker(),
            "key" => self.open_key_prompt(),
            "config" => self.open_settings(),
            "think" => self.toggle_reasoning_view()?,
            "copy" => self.open_copy_menu(),
            "help" => {
                let list = COMMANDS
                    .iter()
                    .map(|c| format!("/{}", c.name))
                    .collect::<Vec<_>>()
                    .join("  ");
                self.status = list;
            }
            "skills" => self.open_skills_popup(),
            "files" => self.open_files_popup(),
            "apps" => self.open_apps_popup(),
            "edit" => self.request_app_file_edit(cmd[token.len()..].trim()),
            other => {
                if self.skills.iter().any(|s| s.name == other) {
                    self.forced_skill = Some(other.to_string());
                    let rest = cmd[token.len()..].trim().to_string();
                    if rest.is_empty() {
                        self.status = format!("skill {other} armed for next message");
                    } else {
                        self.send_message(rest)?;
                    }
                } else {
                    self.status = format!("unknown command: /{other}");
                }
            }
        }
        Ok(())
    }

    /// `/edit <app>/<file>` — queue an app file for `$EDITOR` (the event loop
    /// owns the terminal and does the actual suspend/open).
    fn request_app_file_edit(&mut self, arg: &str) {
        if arg.is_empty() || !arg.contains('/') {
            self.status = "usage: /edit <app>/<file>  e.g. /edit deck/index.html".to_string();
            return;
        }
        if arg.starts_with('/') || arg.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
            self.status = format!("invalid path: {arg}");
            return;
        }
        let path = self.space.apps_dir(&self.active_space.name).join(arg);
        if !path.is_file() {
            self.status = format!("no such app file: {arg}");
            return;
        }
        self.pending_editor = Some(path);
    }

    // --- commands ---


    // --- nerd config (settings popup) ---

    /// Index into `settings_inputs` for any typed (non-toggle, non-picker) field.
    pub(crate) fn text_index(&self) -> Option<usize> {
        match self.settings_field() {
            SettingsField::ShowStats
            | SettingsField::ShowReasoning
            | SettingsField::HideHints
            | SettingsField::MemoryModel
            | SettingsField::Verbosity
            | SettingsField::SearchProvider
            | SettingsField::TranscriberModel => None,
            SettingsField::Temperature => Some(0),
            SettingsField::TopP => Some(1),
            SettingsField::MaxTokens => Some(2),
            SettingsField::CompactThreshold => Some(3),
            SettingsField::SearxngUrl => Some(4),
            SettingsField::LangsearchKey => Some(5),
        }
    }

}

/// The one-line transcript summary for a tool-call block: the tool's name
/// plus the argument (and result shape) a reader actually cares about.
pub(crate) fn tool_call_summary(name: &str, args: &str, result: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let f = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    match name {
        "skill" => format!("skill {}", f("name")),
        "install_skill" => format!("install_skill {} → {}", f("source"), first_line(result)),
        "run_script" => format!("run_script {}/{}", f("skill"), f("script")),
        "run_python" => format!("run_python ({} lines)", f("code").lines().count().max(1)),
        "grep_app" => {
            let hits = if result.starts_with("no matches") { "no hits".to_string() } else { format!("{} hits", result.lines().count()) };
            format!("grep_app {} \"{}\" → {hits}", f("app"), f("pattern"))
        }
        "install_packages" => {
            let pkgs = v
                .get("packages")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let target = [f("skill"), f("app")].into_iter().find(|t| !t.is_empty()).unwrap_or_default();
            format!("install_packages {pkgs} → {target}")
        }
        "web_search" | "search_files" => {
            let failed = result.starts_with("no results")
                || result.starts_with("no matches")
                || result.contains("failed");
            let hits =
                if failed { "no hits".to_string() } else { format!("{} hits", result.lines().count()) };
            format!("{name} \"{}\" → {hits}", f("query"))
        }
        "read_file" => format!("read_file {} → {}", f("name"), first_line(result)),
        "read_app_file" => format!("read_app_file {}/{}", f("app"), f("path")),
        "write_file" => {
            format!("write_file {}/{} ({} bytes)", f("app"), f("path"), f("content").len())
        }
        "edit_file" => format!("edit_file {}/{}", f("app"), f("path")),
        _ => {
            let mut a: String = args.chars().take(60).collect();
            if args.chars().count() > 60 {
                a.push('…');
            }
            format!("{name} {a}")
        }
    }
}

/// First line of a tool result, e.g. `report.pdf (lines 1-200 of 831):`.
fn first_line(result: &str) -> String {
    result.lines().next().unwrap_or("").trim_end_matches(':').to_string()
}

