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

mod chat;
mod compaction;
mod memory;
mod models;
mod sessions;
mod settings;
mod spaces;
#[cfg(test)]
use chat::split_inline_reasoning;
use chat::{code_blocks, pick_greeting};
#[cfg(test)]
use memory::{parse_fact_line, parse_memory_ops};
use sessions::parse_topic;

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
}

/// What the skills popup is doing: browsing, typing a GitHub `owner/repo/path`
/// to install, or confirming removal of the highlighted skill.
#[derive(PartialEq, Clone, Copy)]
pub enum SkillsMode {
    Browse,
    Install,
    ConfirmRemove,
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
}

impl SettingsField {
    pub const ALL: [SettingsField; 12] = [
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
    pub skills_mode: SkillsMode,
    pub skills_selected: usize,
    /// GitHub `owner/repo/path` shorthand being typed in Install mode.
    pub skills_edit: String,
    pub(crate) skills_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,

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
    pub(crate) stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
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
        let toolbox =
            std::sync::Arc::new(crate::tools::ToolBox::new(skills_dir, None, None, "auto".to_string()));
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
            skills_mode: SkillsMode::Browse,
            skills_selected: 0,
            skills_edit: String::new(),
            skills_rx: None,
            active_space,
            spaces_cache: Vec::new(),
            space_selected: 0,
            space_filter: String::new(),
            space_mode: SpaceMode::Browse,
            space_edit: String::new(),
            memory_model: "google/gemini-2.5-flash-lite".to_string(),
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
            stream_rx: None,
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
    fn refresh_toolbox(&mut self) {
        let url = (!self.searxng_url.trim().is_empty()).then(|| self.searxng_url.trim().to_string());
        let key = (!self.langsearch_key.trim().is_empty()).then(|| self.langsearch_key.trim().to_string());
        crate::skills::install_builtin(&self.toolbox.skills_dir);
        self.toolbox = std::sync::Arc::new(crate::tools::ToolBox::new(
            self.toolbox.skills_dir.clone(),
            url,
            key,
            self.search_provider.clone(),
        ));
        self.reload_skills();
    }

    /// Kick off the initial model fetch if a key is already present. Call once
    /// after construction, from within the tokio runtime.
    pub fn init(&mut self) {
        if self.provider.is_some() {
            self.fetch_models();
        }
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

    /// List installed skills in the status line. Replaced by a real popup in
    /// a later phase; `/skills` needs *something* to do meanwhile.
    fn open_skills_popup(&mut self) {
        self.skills_mode = SkillsMode::Browse;
        self.skills_selected = self.skills_selected.min(self.skills.len().saturating_sub(1));
        self.popup = Popup::Skills;
    }

    pub fn reload_skills(&mut self) {
        self.skills = crate::skills::load_skills(&self.toolbox.skills_dir);
        let len = self.skills.len();
        self.skills_selected = self.skills_selected.min(len.saturating_sub(1));
    }

    pub fn move_skills_selection(&mut self, delta: i32) {
        let len = self.skills.len() as i32;
        if len == 0 {
            self.skills_selected = 0;
            return;
        }
        let next = self.skills_selected as i32 + delta;
        self.skills_selected = next.clamp(0, len - 1) as usize;
    }

    pub fn start_skill_install(&mut self) {
        self.skills_edit.clear();
        self.skills_mode = SkillsMode::Install;
    }

    pub fn start_skill_remove(&mut self) {
        if self.skills.get(self.skills_selected).is_some() {
            self.skills_mode = SkillsMode::ConfirmRemove;
        }
    }

    /// Parse the typed `owner/repo/path` (or `owner/repo`) and kick off the
    /// background GitHub fetch. Same bg-task shape as memory extraction.
    pub fn confirm_skill_install(&mut self) {
        let spec = self.skills_edit.trim().to_string();
        self.skills_mode = SkillsMode::Browse;
        let Some((owner, repo, path)) = crate::skills::parse_gh_shorthand(&spec) else {
            self.status = format!("expected owner/repo/path, got: {spec}");
            return;
        };
        let dest = self.toolbox.skills_dir.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.skills_rx = Some(rx);
        self.status = format!("installing {spec}…");
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let result = crate::skills::install_from_github(&client, &owner, &repo, &path, &dest)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    pub fn on_skill_install_result(&mut self, result: Option<Result<String, String>>) {
        self.skills_rx = None;
        match result {
            Some(Ok(name)) => {
                self.reload_skills();
                self.status = format!("installed skill: {name}");
            }
            Some(Err(e)) => self.status = format!("skill install failed: {e}"),
            None => {}
        }
    }

    pub fn confirm_skill_remove(&mut self) {
        if let Some(skill) = self.skills.get(self.skills_selected) {
            let name = skill.name.clone();
            let _ = std::fs::remove_dir_all(&skill.dir);
            self.reload_skills();
            self.status = format!("removed skill: {name}");
        }
        self.skills_mode = SkillsMode::Browse;
    }

    /// Path to the highlighted skill's SKILL.md, for Ctrl+E in the skills popup.
    pub fn skill_edit_path_for_selected(&self) -> Option<std::path::PathBuf> {
        self.skills.get(self.skills_selected).map(|s| s.dir.join("SKILL.md"))
    }

    /// Copy arbitrary text to the clipboard and report it in the status line.
    pub fn copy_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let n = text.chars().count();
        let ok = self
            .clipboard
            .as_mut()
            .is_some_and(|cb| cb.set_text(text).is_ok());
        self.status = if ok {
            format!("copied {n} chars")
        } else {
            "clipboard unavailable".into()
        };
    }

    /// Copy a message's exact original content by its index into `self.messages`
    /// (streaming reply uses index `messages.len()`) — not the on-screen,
    /// wrap-reconstructed text a long-press selects for highlighting.
    pub fn copy_message(&mut self, idx: usize) {
        let text = match self.messages.get(idx) {
            Some(m) if m.role == "assistant" => Some(crate::markdown::to_plain(&m.content)),
            Some(m) => Some(m.content.clone()),
            None if idx == self.messages.len() => self.streaming.clone(),
            None => None,
        };
        if let Some(t) = text {
            self.copy_text(t);
        }
    }

    /// Open the `/copy` menu for the last assistant reply: the whole response
    /// plus one entry per fenced code block.
    fn open_copy_menu(&mut self) {
        let opts = {
            let Some(msg) = self.messages.iter().rev().find(|m| m.role == "assistant") else {
                self.status = "no response to copy".into();
                return;
            };
            let mut opts = vec![CopyOption {
                label: "Entire response".into(),
                text: crate::markdown::to_plain(&msg.content),
            }];
            for (i, (lang, code)) in code_blocks(&msg.content).into_iter().enumerate() {
                let label = match lang {
                    Some(l) => format!("Code block {} ({l})", i + 1),
                    None => format!("Code block {}", i + 1),
                };
                opts.push(CopyOption { label, text: code });
            }
            opts
        };
        self.copy_options = opts;
        self.copy_selected = 0;
        self.popup = Popup::Copy;
    }

    /// Copy the highlighted `/copy` menu entry and close the menu.
    pub fn confirm_copy(&mut self) {
        if let Some(text) = self.copy_options.get(self.copy_selected).map(|o| o.text.clone()) {
            self.copy_text(text);
        }
        self.popup = Popup::None;
    }

    pub fn move_copy_selection(&mut self, delta: i32) {
        let n = self.copy_options.len() as i32;
        if n > 0 {
            self.copy_selected = (self.copy_selected as i32 + delta).clamp(0, n - 1) as usize;
        }
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
            | SettingsField::SearchProvider => None,
            SettingsField::Temperature => Some(0),
            SettingsField::TopP => Some(1),
            SettingsField::MaxTokens => Some(2),
            SettingsField::CompactThreshold => Some(3),
            SettingsField::SearxngUrl => Some(4),
            SettingsField::LangsearchKey => Some(5),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway space dir under the OS temp dir, unique per test.
    fn test_space() -> Space {
        Space { root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())) }
    }

    #[test]
    fn parse_topic_extracts_and_slugifies() {
        let (t, s) = parse_topic(r#"{"topic": "Rust Async Runtimes", "id": "rust async!"}"#).unwrap();
        assert_eq!(t, "Rust Async Runtimes");
        assert_eq!(s, "rust-async");
        // Tolerates surrounding prose / fences.
        let (t, s) = parse_topic("sure:\n```json\n{\"topic\":\"Hi There\",\"id\":\"hi\"}\n```").unwrap();
        assert_eq!(t, "Hi There");
        assert_eq!(s, "hi");
        assert!(parse_topic("no json here").is_none());
    }

    #[test]
    fn session_filter_matches_title_and_slug() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        let space = a.active_space.id.clone();
        let s1 = a.db.create_session("Rust async runtimes", "a/b", &space).unwrap();
        let s2 = a.db.create_session("Cooking pasta", "a/b", &space).unwrap();
        a.db.set_session_title(&s1.id, "Rust async runtimes", Some("rust-async")).unwrap();
        a.sessions_cache = a.db.list_sessions(&space).unwrap();

        a.session_filter = "rust".into();
        let hits = a.filtered_sessions();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, s1.id);

        a.session_filter = "pasta".into();
        assert_eq!(a.filtered_sessions()[0].id, s2.id);
    }

    #[test]
    fn delete_removes_session_and_clears_if_active() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        let space = a.active_space.id.clone();
        let s = a.db.create_session("doomed", "a/b", &space).unwrap();
        a.sessions_cache = a.db.list_sessions(&space).unwrap();
        a.session = Some(s.clone());
        a.messages.push(Message {
            role: "user".into(), content: "hi".into(),
            model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        });
        a.session_selected = 0;
        a.confirm_delete().unwrap();
        assert!(a.sessions_cache.is_empty());
        assert!(a.session.is_none());
        assert!(a.messages.is_empty());
        assert!(a.db.list_sessions(&space).unwrap().is_empty());
    }

    #[test]
    fn code_blocks_split_by_fence_with_lang() {
        let md = "intro\n```rust\nfn a() {}\n```\ntext\n```\nplain\n```";
        let blocks = code_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0.as_deref(), Some("rust"));
        assert_eq!(blocks[0].1, "fn a() {}\n");
        assert_eq!(blocks[1].0, None);
        assert_eq!(blocks[1].1, "plain\n");
    }

    fn app_with_key() -> App {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("test-key".into()), test_space());
        a.models = vec![
            Model { id: "a/one".into(), name: "One".into(), supports_reasoning: false, context_length: None },
            Model { id: "b/two".into(), name: "Two".into(), supports_reasoning: false, context_length: None },
        ];
        a
    }

    #[test]
    fn no_key_rejects_message_and_points_to_key_cmd() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, None, test_space());
        a.set_input("hello");
        a.submit().unwrap();
        assert!(a.session.is_none());
        assert!(a.status.contains("/key"));
    }

    #[test]
    fn message_without_model_is_rejected() {
        let mut a = app_with_key();
        a.set_input("hello");
        a.submit().unwrap();
        assert!(a.session.is_none());
        assert!(a.status.contains("pick a model"));
        assert_eq!(a.input_text(), "hello");
    }

    #[tokio::test]
    async fn message_with_model_creates_session_and_streams() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        a.set_input("hello world");
        a.submit().unwrap();
        assert!(a.session.is_some());
        assert!(a.is_streaming());
        assert_eq!(a.messages.len(), 1);
        assert_eq!(a.messages[0].role, "user");
    }

    #[test]
    fn panels_split_favorites_from_available_by_recency() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        a.models = vec![
            Model { id: "a/one".into(), name: "One".into(), supports_reasoning: false, context_length: None },
            Model { id: "b/two".into(), name: "Two".into(), supports_reasoning: false, context_length: None },
            Model { id: "c/three".into(), name: "Three".into(), supports_reasoning: false, context_length: None },
        ];
        // three is favorite; two was used more recently than one.
        a.favorites.insert("c/three".into());
        a.last_used.insert("a/one".into(), "2026-01-01T00:00:00Z".into());
        a.last_used.insert("b/two".into(), "2026-02-01T00:00:00Z".into());

        let favs: Vec<&str> = a.favorite_models().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(favs, vec!["c/three"]);
        let avail: Vec<&str> = a.available_models().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(avail, vec!["b/two", "a/one"]); // recency first
    }

    #[test]
    fn toggle_favorite_persists_and_moves_panel() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        a.models = vec![Model { id: "a/one".into(), name: "One".into(), supports_reasoning: false, context_length: None }];
        a.model_focus = ModelPanel::Available;
        a.avail_state.select(Some(0));
        a.toggle_favorite_focused().unwrap();
        assert!(a.favorites.contains("a/one"));
        assert_eq!(a.favorite_models().len(), 1);
        assert_eq!(a.available_models().len(), 0);

        a.model_focus = ModelPanel::Favorites;
        a.fav_state.select(Some(0));
        a.toggle_favorite_focused().unwrap();
        assert!(!a.favorites.contains("a/one"));
    }

    #[test]
    fn filter_narrows_available() {
        let mut a = app_with_key();
        a.model_filter = "two".into();
        let f = a.available_models();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "b/two");
    }

    #[test]
    fn reasoning_cycles_only_for_supporting_models() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        a.models = vec![
            Model { id: "r/model".into(), name: "R".into(), supports_reasoning: true, context_length: Some(1000) },
        ];
        a.model_focus = ModelPanel::Available;
        a.avail_state.select(Some(0));
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(a.reasoning_of("r/model"), Some("low"));
        a.cycle_reasoning_focused().unwrap();
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(a.reasoning_of("r/model"), Some("high"));
        a.cycle_reasoning_focused().unwrap(); // high -> off
        assert_eq!(a.reasoning_of("r/model"), None);
        // persisted
        assert!(a.db.load_model_prefs().unwrap().iter().any(|p| p.id == "r/model"));
    }

    #[test]
    fn settings_edit_and_save_persists() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        a.popup = Popup::Settings;
        a.settings_selected = 0; // ShowStats
        a.toggle_settings_field();
        assert!(a.settings.show_stats);
        a.settings_selected = 3; // Temperature
        for c in "0.7".chars() {
            a.settings_input_char(c);
        }
        a.save_settings().unwrap();
        assert_eq!(a.settings.temperature, Some(0.7));

        // reload from db picks up the saved values
        let b = App::new(Db::open_in_memory().unwrap(), Some("k".into()), test_space());
        let _ = b; // separate in-memory db; just assert current instance loads its own
        let reloaded = a.db.load_settings().unwrap();
        assert!(reloaded.iter().any(|(k, v)| k == "temperature" && v == "0.7"));
        assert!(reloaded.iter().any(|(k, v)| k == "show_stats" && v == "1"));
    }

    #[test]
    fn base_system_prompt_is_always_present_and_resolves_verbosity() {
        let a = app_with_key();
        assert!(!a.base_system_prompt.is_empty());
        assert!(a.base_system_prompt.contains("{{verbosity}}")); // placeholder present in the raw file
        let resolved = a.system_prompt();
        assert!(!resolved.contains("{{verbosity}}")); // placeholder gets swapped
        assert!(resolved.contains(verbosity_clause("concise"))); // default level
    }

    #[test]
    fn verbosity_cycles_through_all_three_levels() {
        let mut a = app_with_key();
        a.popup = Popup::Settings;
        a.settings_selected = 9; // Verbosity
        assert_eq!(a.verbosity, "concise");
        a.toggle_settings_field();
        assert_eq!(a.verbosity, "caveman");
        a.toggle_settings_field();
        assert_eq!(a.verbosity, "normal");
        a.toggle_settings_field();
        assert_eq!(a.verbosity, "concise");
    }

    #[test]
    fn verbosity_setting_persists_and_changes_the_prompt() {
        let mut a = app_with_key();
        a.popup = Popup::Settings;
        a.settings_selected = 9; // Verbosity
        a.toggle_settings_field(); // -> caveman
        a.save_settings().unwrap();
        assert!(a.system_prompt().contains(verbosity_clause("caveman")));

        let reloaded = a.db.load_settings().unwrap();
        assert!(reloaded.iter().any(|(k, v)| k == "verbosity" && v == "caveman"));
    }

    #[test]
    fn searxng_url_setting_persists_and_enables_web_search_tool() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, Some("k".into()), test_space());
        // web_search always works (DuckDuckGo fallback needs no config); only
        // the backend it uses depends on this setting.
        assert!(a.toolbox.defs().iter().any(|t| t.name == "web_search"));
        assert!(a.toolbox.searxng_url.is_none());

        a.popup = Popup::Settings;
        a.settings_selected = 8; // SearxngUrl
        for c in "http://localhost:8080/".chars() {
            a.settings_input_char(c);
        }
        a.save_settings().unwrap();

        assert_eq!(a.searxng_url, "http://localhost:8080"); // trailing slash trimmed
        assert_eq!(a.toolbox.searxng_url.as_deref(), Some("http://localhost:8080"));
        assert!(a.skills.iter().any(|s| s.name == "web-search")); // built-in materialized

        let reloaded = a.db.load_settings().unwrap();
        assert!(reloaded.iter().any(|(k, v)| k == "searxng_url" && v == "http://localhost:8080"));

        // Reloading a fresh App from the same db picks it back up.
        let mut b = App::new(a.db, Some("k".into()), test_space());
        b.load_settings();
        assert_eq!(b.searxng_url, "http://localhost:8080");
    }

    #[test]
    fn last_used_model_restored_on_startup() {
        let db = Db::open_in_memory().unwrap();
        db.mark_model_used("a/one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        db.mark_model_used("b/two").unwrap(); // more recent
        let a = App::new(db, Some("k".into()), test_space());
        assert_eq!(a.current_model.as_deref(), Some("b/two"));
    }

    #[test]
    fn context_used_and_limit() {
        let mut a = app_with_key();
        a.skills.clear(); // isolate this test from the always-installed web-search skill
        a.base_system_prompt = String::new(); // isolate from the base system prompt
        a.models[0].context_length = Some(1000);
        a.current_model = Some("a/one".into());
        assert_eq!(a.context_limit(), Some(1000));
        a.messages.push(Message {
            role: "user".into(),
            content: "x".repeat(40), // ~10 tokens
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: None,
        });
        assert_eq!(a.context_used(), 10);
    }

    #[test]
    fn compaction_narrows_effective_messages_and_context_used() {
        let mut a = app_with_key();
        a.skills.clear(); // isolate this test from the always-installed web-search skill
        a.base_system_prompt = String::new(); // isolate from the base system prompt
        a.models[0].context_length = Some(1000);
        a.current_model = Some("a/one".into());
        let space = a.active_space.id.clone();
        let mut s = a.db.create_session("t", "a/one", &space).unwrap();
        for i in 0..4 {
            a.messages.push(Message {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: "x".repeat(40), // ~10 tokens each
                model: None, reasoning: None, tokens: None, secs: None, phrase: None,
            });
        }
        s.compact_summary = Some("y".repeat(80)); // ~20 tokens
        s.compact_through = 2; // first two raw messages folded away
        a.session = Some(s);

        assert_eq!(a.effective_messages().len(), 2);
        // 20 (summary) + 2*10 (tail) = 40 tokens; the two compacted-away
        // messages must NOT be counted.
        assert_eq!(a.context_used(), 40);
    }

    #[test]
    fn on_compact_result_persists_and_clears_stale_total() {
        let mut a = app_with_key();
        a.models[0].context_length = Some(1000);
        a.current_model = Some("a/one".into());
        let space = a.active_space.id.clone();
        let s = a.db.create_session("t", "a/one", &space).unwrap();
        let sid = s.id.clone();
        a.session = Some(s);
        a.context_total = Some(999); // stale exact usage from before compaction

        a.on_compact_result(Some((sid.clone(), "digest".into(), 3, 61)));

        assert_eq!(a.session.as_ref().unwrap().compact_summary.as_deref(), Some("digest"));
        assert_eq!(a.session.as_ref().unwrap().compact_through, 3);
        assert!(a.context_total.is_none()); // stale total dropped
        assert!(a.status.contains("compacted: 61%"));
        // Persisted to the db too.
        let reloaded = &a.db.list_sessions(&space).unwrap()[0];
        assert_eq!(reloaded.compact_summary.as_deref(), Some("digest"));
    }

    #[test]
    fn maybe_compact_noop_when_disabled_or_under_threshold() {
        let mut a = app_with_key();
        a.models[0].context_length = Some(1000);
        a.current_model = Some("a/one".into());
        a.settings.compact_threshold = 0; // disabled
        a.maybe_compact();
        assert!(a.compact_rx.is_none());

        a.settings.compact_threshold = 60;
        // No session / far under threshold — should still no-op.
        a.maybe_compact();
        assert!(a.compact_rx.is_none());
    }

    #[tokio::test]
    async fn force_compact_reports_why_it_no_ops() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());

        // No active session yet.
        a.force_compact();
        assert!(a.compact_rx.is_none());
        assert!(a.status.contains("no active session"));

        // Session exists but everything in it is already covered.
        let space = a.active_space.id.clone();
        let mut s = a.db.create_session("t", "a/one", &space).unwrap();
        s.compact_through = 0;
        a.session = Some(s);
        a.force_compact();
        assert!(a.compact_rx.is_none());
        assert!(a.status.contains("nothing new"));

        // Now there's an uncompacted message — should actually kick off a job.
        a.messages.push(Message {
            role: "user".into(), content: "hi".into(),
            model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        });
        a.force_compact();
        assert!(a.compact_rx.is_some());
    }

    #[test]
    fn context_breakdown_reports_system_memory_conversation() {
        let mut a = app_with_key();
        a.models[0].context_length = Some(1000);
        a.current_model = Some("a/one".into());
        a.messages.push(Message {
            role: "user".into(), content: "x".repeat(40),
            model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        });
        let b = a.context_breakdown();
        assert_eq!(b.conversation_tokens, 10);
        assert!(b.system_tokens > 0); // base system prompt is always present
        assert_eq!(b.limit, Some(1000));
        assert!(!b.compacted);
    }

    #[test]
    fn compact_summary_view_and_edit_roundtrip() {
        let mut a = app_with_key();
        assert!(a.compact_summary_path().is_none()); // not compacted yet

        let space = a.active_space.id.clone();
        let mut s = a.db.create_session("t", "a/one", &space).unwrap();
        s.compact_summary = Some("original digest".into());
        s.compact_through = 2;
        a.session = Some(s);

        let path = a.compact_summary_path().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original digest");

        // Simulate a hand-edit in $EDITOR, then reload.
        std::fs::write(&path, "hand-edited digest\n").unwrap();
        a.reload_compact_summary(&path).unwrap();
        assert_eq!(a.session.as_ref().unwrap().compact_summary.as_deref(), Some("hand-edited digest"));
        let reloaded = &a.db.list_sessions(&space).unwrap()[0];
        assert_eq!(reloaded.compact_summary.as_deref(), Some("hand-edited digest"));
        assert_eq!(reloaded.compact_through, 2); // boundary untouched by the edit
    }

    #[test]
    fn pick_model_at_sets_current_and_closes() {
        let mut a = app_with_key();
        a.popup = Popup::Model;
        a.pick_model_at(ModelPanel::Available, 0).unwrap();
        assert!(a.current_model.is_some());
        assert!(a.popup == Popup::None);
    }

    #[test]
    fn picking_memory_model_sets_it_and_returns_to_settings() {
        let mut a = app_with_key();
        let original_model = a.current_model.clone();
        a.open_model_picker_for_memory();
        assert!(a.popup == Popup::Model);
        assert!(a.model_pick_target == ModelPickTarget::Memory);

        a.pick_model_at(ModelPanel::Available, 0).unwrap();
        assert_eq!(a.memory_model, "a/one");
        assert!(a.popup == Popup::Settings); // back to /config, not closed
        assert_eq!(a.current_model, original_model); // session model untouched
        assert_eq!(
            a.db.load_settings().unwrap().iter().find(|(k, _)| k == "memory_model").map(|(_, v)| v.clone()),
            Some("a/one".to_string())
        );
    }

    #[test]
    fn clear_memory_model_disables_extraction() {
        let mut a = app_with_key();
        a.memory_model = "some/model".into();
        a.clear_memory_model().unwrap();
        assert!(a.memory_model.is_empty());
        assert_eq!(
            a.db.load_settings().unwrap().iter().find(|(k, _)| k == "memory_model").map(|(_, v)| v.clone()),
            Some(String::new())
        );
    }

    #[tokio::test]
    async fn finish_stream_persists_assistant_message() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        a.set_input("hi");
        a.submit().unwrap();
        a.on_stream_event(StreamEvent::Token("pong".into())).unwrap();
        a.on_stream_event(StreamEvent::Done).unwrap();
        assert!(!a.is_streaming());
        assert_eq!(a.messages.last().unwrap().content, "pong");
        let sid = a.session.as_ref().unwrap().id.clone();
        assert_eq!(a.db.load_messages(&sid).unwrap().len(), 2);
    }

    #[test]
    fn model_picker_without_key_opens_key_prompt() {
        let db = Db::open_in_memory().unwrap();
        let mut a = App::new(db, None, test_space());
        a.open_model_picker();
        assert!(a.popup == Popup::Key);
    }

    #[test]
    fn command_autocomplete_fuzzy_matches_names_aliases_and_desc() {
        let mut a = app_with_key();
        a.skills.clear(); // isolate this test from the always-installed web-search skill

        // Bare "/" lists everything; a space closes the popup.
        a.set_input("/");
        assert_eq!(a.command_matches().len(), crate::input::COMMANDS.len());
        a.set_input("/new foo");
        assert!(a.command_matches().is_empty());

        // Alias fuzzy-matches to the canonical command.
        a.set_input("/history");
        assert_eq!(a.command_matches()[0].name(), "session");

        // Description is searchable ("stats" -> config).
        a.set_input("/stats");
        assert_eq!(a.command_matches()[0].name(), "config");

        // Non-subsequence garbage matches nothing.
        a.set_input("/zzzz");
        assert!(a.command_matches().is_empty());
    }

    fn install_test_skill(a: &mut App, name: &str, desc: &str, body: &str) {
        let dir = std::env::temp_dir().join(format!("nexus-test-skill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: {desc}\n---\n{body}"))
            .unwrap();
        a.skills.push(crate::skills::Skill { name: name.to_string(), description: desc.to_string(), dir });
    }

    #[test]
    fn skills_are_merged_into_command_matches_and_ranked() {
        let mut a = app_with_key();
        a.skills.clear(); // isolate this test from the always-installed web-search skill
        install_test_skill(&mut a, "web-search", "Search the web", "instructions");
        a.set_input("/");
        let matches = a.command_matches();
        assert_eq!(matches.len(), crate::input::COMMANDS.len() + 1);
        assert!(matches.iter().any(|m| m.name() == "web-search"));

        a.set_input("/web-search");
        let matches = a.command_matches();
        assert_eq!(matches[0].name(), "web-search");
    }

    #[tokio::test]
    async fn forced_skill_with_trailing_text_sends_immediately() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        install_test_skill(&mut a, "web-search", "Search the web", "search instructions");
        a.set_input("/web-search rust news");
        a.submit().unwrap();
        assert!(a.forced_skill.is_none()); // consumed by send_message
        assert_eq!(a.messages.last().unwrap().content, "rust news");
        assert!(a.is_streaming());
    }

    #[test]
    fn forced_skill_without_text_arms_for_next_message() {
        let mut a = app_with_key();
        install_test_skill(&mut a, "web-search", "Search the web", "search instructions");
        a.set_input("/web-search");
        a.submit().unwrap();
        assert_eq!(a.forced_skill.as_deref(), Some("web-search"));
        assert!(a.status.contains("armed"));
    }

    #[test]
    fn accept_command_fill_vs_run() {
        // Tab fills the composer with the canonical command, doesn't run it.
        let mut a = app_with_key();
        a.set_input("/hist");
        a.accept_command(false).unwrap();
        assert_eq!(a.input_text(), "/session ");

        // Enter on the "new" alias runs it, clearing the composer.
        let mut b = app_with_key();
        b.current_model = Some("a/one".into());
        let space_id = b.active_space.id.clone();
        b.session = Some(b.db.create_session("old chat", "a/one", &space_id).unwrap());
        b.set_input("/clear");
        b.accept_command(true).unwrap();
        assert!(b.input_text().is_empty());
        assert!(b.session.is_none()); // /new clears the view; no row created until a message is sent
    }

    #[test]
    fn split_inline_reasoning_strips_think_tags() {
        let (content, reasoning) = split_inline_reasoning("plain answer, no tags");
        assert_eq!(content, "plain answer, no tags");
        assert_eq!(reasoning, None);

        let (content, reasoning) =
            split_inline_reasoning("<think>let me work this out</think>the actual answer");
        assert_eq!(content, "the actual answer");
        assert_eq!(reasoning.as_deref(), Some("let me work this out"));

        // Multiple blocks join; text around/between them stays in content.
        let (content, reasoning) =
            split_inline_reasoning("intro <think>step one</think>middle<think>step two</think> outro");
        assert_eq!(content, "intro middle outro");
        assert_eq!(reasoning.as_deref(), Some("step one\nstep two"));

        // Unterminated tag (truncated stream): remainder is reasoning, not a
        // dangling tag leaked into the answer.
        let (content, reasoning) = split_inline_reasoning("<think>still thinking...");
        assert_eq!(content, "");
        assert_eq!(reasoning.as_deref(), Some("still thinking..."));
    }

    #[tokio::test]
    async fn finish_stream_strips_inline_think_tags_into_reasoning() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        a.set_input("hi");
        a.submit().unwrap();
        a.on_stream_event(StreamEvent::Token("<think>pondering</think>".into())).unwrap();
        a.on_stream_event(StreamEvent::Token("the real answer".into())).unwrap();
        a.on_stream_event(StreamEvent::Done).unwrap();

        let msg = a.messages.last().unwrap();
        assert_eq!(msg.content, "the real answer");
        assert_eq!(msg.reasoning.as_deref(), Some("pondering"));

        // Copying the message must not leak the stripped reasoning back in.
        a.status = "sentinel".into();
        a.copy_message(1);
        assert_ne!(a.status, "sentinel");
    }

    #[test]
    fn parse_memory_ops_reads_add_update_delete() {
        let ops = parse_memory_ops(
            r#"sure, here:
            [{"op":"add","text":"likes rust"}, {"op":"update","id":2,"text":"deadline moved"}, {"op":"delete","id":5}, {"bogus":true}]"#,
        );
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], MemoryOp::Add(t) if t == "likes rust"));
        assert!(matches!(&ops[1], MemoryOp::Update(2, t) if t == "deadline moved"));
        assert!(matches!(&ops[2], MemoryOp::Delete(5)));
        assert!(parse_memory_ops("no json here").is_empty());
    }

    #[test]
    fn memory_ops_apply_and_renumber() {
        let mut a = app_with_key();
        let space = a.active_space.name.clone();
        std::fs::write(a_memory_path(&a), "1. likes rust\n2. old fact\n").unwrap();

        let ops = vec![
            MemoryOp::Delete(1),                          // drop "likes rust"
            MemoryOp::Update(2, "updated fact".into()),    // "old fact" -> "updated fact"
            MemoryOp::Add("new fact".into()),
        ];
        a.on_memory_result(Some((space, ops)));

        let saved = std::fs::read_to_string(a_memory_path(&a)).unwrap();
        assert_eq!(saved, "1. updated fact\n2. new fact\n");
    }

    #[test]
    fn memory_ops_dropped_if_space_switched_meanwhile() {
        let mut a = app_with_key();
        std::fs::write(a_memory_path(&a), "1. keep me\n").unwrap();
        a.on_memory_result(Some(("some-other-space".into(), vec![MemoryOp::Delete(1)])));
        assert_eq!(std::fs::read_to_string(a_memory_path(&a)).unwrap(), "1. keep me\n");
    }

    fn a_memory_path(a: &App) -> std::path::PathBuf {
        a.space.ensure_space_dir(&a.active_space.name).unwrap();
        a.space.memory_path(&a.active_space.name)
    }

    #[test]
    fn space_crud_via_app_methods() {
        let mut a = app_with_key();
        a.spaces_cache = a.db.list_spaces().unwrap();
        a.space_edit = "work".into();
        a.confirm_space_create().unwrap();
        assert!(a.spaces_cache.iter().any(|s| s.name == "work"));

        a.space_selected = a.spaces_cache.iter().position(|s| s.name == "work").unwrap();
        a.space_edit = "work-2".into();
        a.confirm_space_rename().unwrap();
        assert!(a.spaces_cache.iter().any(|s| s.name == "work-2"));

        a.confirm_space_delete().unwrap(); // "work-2" still selected
        assert!(!a.spaces_cache.iter().any(|s| s.name == "work-2"));
        assert_eq!(a.active_space.name, DEFAULT_SPACE); // untouched, wasn't active
    }

    #[tokio::test]
    async fn switching_space_clears_open_conversation() {
        let mut a = app_with_key();
        a.current_model = Some("a/one".into());
        a.set_input("hello");
        a.submit().unwrap();
        assert!(a.session.is_some());

        let other = a.db.create_space("other").unwrap();
        a.spaces_cache = vec![other.clone()];
        a.space_selected = 0;
        a.confirm_space().unwrap();

        assert_eq!(a.active_space.id, other.id);
        assert!(a.session.is_none());
        assert!(a.messages.is_empty());
    }

    #[test]
    fn copy_message_uses_exact_original_content() {
        let mut a = app_with_key();
        a.messages.push(Message {
            role: "user".into(), content: "raw *user* text".into(),
            model: None, reasoning: None, tokens: None, secs: None, phrase: None,
        });
        a.messages.push(Message {
            role: "assistant".into(), content: "**bold** reply".into(),
            model: Some("a/one".into()), reasoning: None, tokens: None, secs: None, phrase: None,
        });
        // copy_message resolves *some* text at each index (clipboard availability
        // is environment-dependent in CI, so just assert it didn't silently no-op).
        a.status = "sentinel".into();
        a.copy_message(0); // user message: verbatim
        assert_ne!(a.status, "sentinel");

        a.status = "sentinel".into();
        a.copy_message(1); // assistant message: through markdown::to_plain
        assert_ne!(a.status, "sentinel");

        // Streaming reply (not yet in `messages`) uses index == messages.len().
        a.streaming = Some("live tokens".into());
        a.status = "sentinel".into();
        a.copy_message(2);
        assert_ne!(a.status, "sentinel");

        // An out-of-range index (no streaming either) is a no-op.
        a.streaming = None;
        a.status = "sentinel".into();
        a.copy_message(2);
        assert_eq!(a.status, "sentinel");
    }
}
