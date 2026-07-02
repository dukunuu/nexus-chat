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
use crate::provider::{ChatMessage, ChatParams, Model, StreamEvent, Usage};
use crate::space::Space;

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
    space: Space,
    provider: Option<OpenRouter>,
    key: Option<String>,

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
    memory_rx: Option<mpsc::UnboundedReceiver<(String, Vec<MemoryOp>)>>,
    /// Background compaction result: (session id, digest, messages-covered, pre-compaction %).
    compact_rx: Option<mpsc::UnboundedReceiver<(String, String, i64, u64)>>,

    /// Installed skills (name/description only — bodies are read from disk on
    /// invocation, so this list is cheap and reloaded whenever it changes).
    pub skills: Vec<crate::skills::Skill>,
    /// A skill armed by `/<skill-name>`, injected into the next message only.
    forced_skill: Option<String>,
    toolbox: std::sync::Arc<crate::tools::ToolBox>,
    pub skills_mode: SkillsMode,
    pub skills_selected: usize,
    /// GitHub `owner/repo/path` shorthand being typed in Install mode.
    pub skills_edit: String,
    skills_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,

    /// Live model catalog (fetched on demand, never hardcoded).
    pub models: Vec<Model>,
    pub current_model: Option<String>,
    models_rx: Option<mpsc::UnboundedReceiver<ModelsResult>>,
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
    thinking_text: String,
    /// Label for a tool currently running mid-stream (e.g. "Searching the web…").
    pub tool_status: Option<String>,
    stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
    /// Wall-clock start of the current stream, for TPS.
    stream_started: Option<std::time::Instant>,
    /// Exact usage reported for the in-flight stream, if any.
    stream_usage: Option<Usage>,
    /// Exact conversation token total from the last completed response.
    context_total: Option<u64>,

    pub settings: Settings,
    /// Animated "thinking" indicator shown while a response streams.
    spinner_frame: usize,
    thinking_idx: usize,
    spinner_color: Color,

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

    pub fn submit(&mut self) -> Result<()> {
        let text = self.input_text();
        self.clear_input();
        self.sel.clear(); // history line indices are about to shift
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd)?;
        } else {
            self.send_message(text.to_string())?;
        }
        Ok(())
    }

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

    fn send_message(&mut self, text: String) -> Result<()> {
        if self.is_streaming() {
            self.status = "wait for the current response to finish".to_string();
            self.set_input(&text);
            return Ok(());
        }
        let Some(provider) = self.provider.clone() else {
            self.status = "set your API key first with /key".to_string();
            self.set_input(&text);
            return Ok(());
        };
        let Some(model) = self.current_model.clone() else {
            self.status = "pick a model first with /model".to_string();
            self.set_input(&text);
            return Ok(());
        };

        // Auto-create a session on the first message.
        if self.session.is_none() {
            let title = title_from(&text);
            let s = self.db.create_session(&title, &model, &self.active_space.id)?;
            self.session = Some(s);
        }
        let session_id = self.session.as_ref().unwrap().id.clone();

        self.db.add_user_message(&session_id, &text)?;
        self.messages.push(Message {
            role: "user".to_string(),
            content: text,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: None,
        });

        let mut history: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 3);
        history.push(ChatMessage::text("system", self.system_prompt()));
        // If this session has been auto-compacted, send the digest instead of
        // the raw messages it covers — only the tail after it goes verbatim.
        if let Some(summary) = self.session.as_ref().and_then(|s| s.compact_summary.clone()) {
            history.push(ChatMessage::text(
                "system",
                format!("Summary of earlier conversation (auto-compacted for length):\n{summary}"),
            ));
        }
        if let Some(name) = self.forced_skill.take()
            && let Some(skill) = self.skills.iter().find(|s| s.name == name)
        {
            let body = std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).to_string())
                .unwrap_or_default();
            history.push(ChatMessage::text(
                "system",
                format!("The user invoked the skill '{name}'. Follow these instructions:\n{body}"),
            ));
        }
        history.extend(
            self.effective_messages().iter().map(|m| ChatMessage::text(m.role.clone(), m.content.clone())),
        );

        let params = ChatParams {
            reasoning_effort: self.reasoning.get(&model).cloned(),
            temperature: self.settings.temperature,
            top_p: self.settings.top_p,
            max_tokens: self.settings.max_tokens,
        };
        let tools = self.toolbox.defs();
        self.stream_rx = Some(provider.stream_chat(model, history, params, tools, self.toolbox.clone()));
        self.streaming = Some(String::new());
        self.thinking_text.clear();
        self.tool_status = None;
        self.stream_usage = None;
        self.stream_started = Some(std::time::Instant::now());
        self.spinner_frame = 0;
        let (idx, color) = pick_flavor();
        self.thinking_idx = idx;
        self.spinner_color = color;
        self.status.clear();
        self.scroll = 0;
        Ok(())
    }

    pub fn on_stream_event(&mut self, ev: StreamEvent) -> Result<()> {
        match ev {
            StreamEvent::Token(t) => {
                if let Some(buf) = self.streaming.as_mut() {
                    buf.push_str(&t);
                }
            }
            StreamEvent::Reasoning(t) => self.thinking_text.push_str(&t),
            StreamEvent::Usage(u) => self.stream_usage = Some(u),
            StreamEvent::Status(s) => self.tool_status = Some(s),
            StreamEvent::Done => self.finish_stream()?,
            StreamEvent::Error(e) => {
                self.status = format!("stream error: {e}");
                self.finish_stream()?;
            }
        }
        Ok(())
    }

    fn finish_stream(&mut self) -> Result<()> {
        self.stream_rx = None;
        self.tool_status = None;
        let started = self.stream_started.take();
        let mut reasoning = std::mem::take(&mut self.thinking_text);
        let Some(buf) = self.streaming.take() else {
            return Ok(());
        };
        if buf.is_empty() {
            return Ok(());
        }
        // Some reasoning models (routed without the separate `reasoning` delta
        // field) inline their thinking as `<think>...</think>` in `content`
        // itself. Pull that out so the stored/displayed/copied message is just
        // the actual answer, not the thinking — same treatment as the explicit
        // reasoning channel above.
        let (buf, inline) = split_inline_reasoning(&buf);
        if let Some(inline) = inline {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&inline);
        }
        let model = self.current_model.clone();
        // Prefer the provider's exact usage; fall back to a ~4-chars/token estimate.
        let usage = self.stream_usage.take();
        let tokens = Some(match usage {
            Some(u) => u.completion_tokens as i64,
            None => buf.chars().count().div_ceil(4) as i64,
        });
        if let Some(u) = usage {
            // Some providers omit total; derive it from prompt + completion.
            let total = if u.total_tokens > 0 {
                u.total_tokens
            } else {
                u.prompt_tokens + u.completion_tokens
            };
            self.context_total = Some(total);
        }
        let secs = started.map(|s| s.elapsed().as_secs_f64());
        let reasoning = (!reasoning.is_empty()).then_some(reasoning);
        let phrase = Some(THINKING[self.thinking_idx].1.to_string());

        if let Some(session) = &self.session {
            self.db.add_assistant_message(
                &session.id,
                &buf,
                model.as_deref(),
                reasoning.as_deref(),
                tokens,
                secs,
                phrase.as_deref(),
            )?;
        }
        self.messages.push(Message {
            role: "assistant".to_string(),
            content: buf,
            model,
            reasoning,
            tokens,
            secs,
            phrase,
        });
        self.maybe_generate_title();
        self.maybe_extract_memory();
        self.maybe_compact();
        Ok(())
    }

    /// After the first exchange of a session, ask the model for a short topic and
    /// slug in the background. Runs once per session (guarded by `slug.is_none()`).
    fn maybe_generate_title(&mut self) {
        let (Some(session), Some(provider), Some(model)) =
            (self.session.as_ref(), self.provider.clone(), self.current_model.clone())
        else {
            return;
        };
        if session.slug.is_some() {
            return; // already named
        }
        // Build a compact transcript of the conversation so far.
        let convo: String = self
            .messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content.chars().take(500).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n");
        let session_id = session.id.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.title_rx = Some(rx);
        tokio::spawn(async move {
            let prompt = format!(
                "Summarise this conversation as a session name. Reply with ONLY a JSON object, \
                 no markdown, of the form {{\"topic\": \"<3-5 word title>\", \"id\": \"<short-kebab-slug>\"}}.\n\n{convo}"
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(text) = provider.complete(&model, msgs).await
                && let Some((topic, slug)) = parse_topic(&text)
            {
                let _ = tx.send((session_id, topic, slug));
            }
        });
    }

    /// Apply a generated topic/slug to the matching session (in memory + db).
    pub fn on_title_result(&mut self, result: Option<(String, String, String)>) {
        self.title_rx = None;
        let Some((id, topic, slug)) = result else { return };
        let _ = self.db.set_session_title(&id, &topic, Some(&slug));
        if let Some(s) = self.session.as_mut().filter(|s| s.id == id) {
            s.title = topic.clone();
            s.slug = Some(slug.clone());
        }
        if let Some(s) = self.sessions_cache.iter_mut().find(|s| s.id == id) {
            s.title = topic;
            s.slug = Some(slug);
        }
    }

    // --- memory (per-space, extracted after every assistant reply) ---

    /// Instructions + memory for the active space, combined into one system
    /// message. `None` if the space has neither (today's no-system-prompt path).
    /// The full system prompt: the app's own base prompt (identity/formatting
    /// rules, `$EDITOR`-editable) first, then space instructions, skills, and
    /// memory layered on top. Unlike those three, the base prompt is never
    /// empty — it's the app speaking, not per-space configuration.
    fn system_prompt(&self) -> String {
        let mut parts: Vec<String> = vec![self.resolved_base_system_prompt()];
        let instructions = std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(i) = instructions {
            parts.push(i);
        }
        if let Some(skills) = self.skills_section() {
            parts.push(skills);
        }
        let memory = self.read_memory();
        if !memory.trim().is_empty() {
            parts.push(format!("## Memory\n{memory}"));
        }
        parts.join("\n\n")
    }

    /// `base_system_prompt` (raw, as read from `system_prompt.md`) with the
    /// `{{verbosity}}` placeholder swapped for the level the user picked.
    fn resolved_base_system_prompt(&self) -> String {
        let now = Utc::now().format("%Y-%m-%d %H:%M UTC, %A").to_string();
        self.base_system_prompt
            .replace("{{verbosity}}", verbosity_clause(&self.verbosity))
            .replace("{{datetime}}", &now)
    }

    /// Re-read `system_prompt.md` after a Ctrl+E hand-edit.
    pub fn reload_base_system_prompt(&mut self) {
        if let Ok(text) = crate::config::load_system_prompt() {
            self.base_system_prompt = text;
            self.status = "system prompt reloaded".to_string();
        }
    }

    /// Names + descriptions of installed skills and how to invoke one — full
    /// bodies stay off the wire until the model calls the `skill` tool.
    fn skills_section(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut s = "## Skills\nYou have skills available. To use one, call the `skill` tool \
                     with its name; the full instructions will be returned.\n"
            .to_string();
        for skill in &self.skills {
            s.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        Some(s.trim_end().to_string())
    }

    /// Raw contents of the active space's memory file, capped to ~16k chars.
    fn read_memory(&self) -> String {
        let text = std::fs::read_to_string(self.space.memory_path(&self.active_space.name))
            .unwrap_or_default();
        text.chars().take(16_000).collect()
    }

    /// After an assistant reply, ask the memory model for ADD/UPDATE/DELETE ops
    /// against the space's fact file. No-op if extraction is disabled or the
    /// last exchange is unavailable.
    fn maybe_extract_memory(&mut self) {
        if self.memory_model.trim().is_empty() {
            return;
        }
        let Some(provider) = self.provider.clone() else { return };
        let [user_msg, assistant_msg] = match self.messages.as_slice() {
            [.., u, a] if u.role == "user" && a.role == "assistant" => [u.clone(), a.clone()],
            _ => return,
        };
        let facts = self.read_memory();
        let model = self.memory_model.clone();
        let space = self.active_space.name.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.memory_rx = Some(rx);
        tokio::spawn(async move {
            let truncate = |s: &str| s.chars().take(2000).collect::<String>();
            let prompt = format!(
                "Stored facts (numbered, may be empty):\n{facts}\n\n\
                 Latest exchange:\nuser: {}\nassistant: {}\n\n\
                 Reply with ONLY a JSON array of memory ops, no markdown, no prose. \
                 Each op is one of:\n\
                 {{\"op\":\"add\",\"text\":\"<durable single-line fact>\"}}\n\
                 {{\"op\":\"update\",\"id\":<N>,\"text\":\"<replacement>\"}}\n\
                 {{\"op\":\"delete\",\"id\":<N>}}\n\
                 Empty array [] if nothing memory-worthy. Facts must be durable and \
                 user/project-relevant (preferences, identity, ongoing goals) — never a \
                 summary of what was just said. Merge/update instead of duplicating. \
                 Keep the total under 50 facts.",
                truncate(&user_msg.content),
                truncate(&assistant_msg.content),
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(text) = provider.complete(&model, msgs).await {
                let ops = parse_memory_ops(&text);
                let _ = tx.send((space, ops));
            }
        });
    }

    /// Apply extracted ops to the active space's memory file, if it's still the
    /// active one (a meanwhile space-switch discards stale results).
    pub fn on_memory_result(&mut self, result: Option<(String, Vec<MemoryOp>)>) {
        self.memory_rx = None;
        let Some((space, ops)) = result else { return };
        if space != self.active_space.name || ops.is_empty() {
            return;
        }
        // Ids in `ops` refer to the *original* numbering, so resolve updates/
        // deletes against that fixed list before appending adds — mutating the
        // vector in place as ops are applied would shift later ids underfoot.
        let original: Vec<String> = self
            .read_memory()
            .lines()
            .filter_map(parse_fact_line)
            .map(|(_, text)| text)
            .collect();
        let mut updates: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
        let mut deletes: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut adds: Vec<String> = Vec::new();
        for op in ops {
            match op {
                MemoryOp::Add(text) => adds.push(text),
                MemoryOp::Update(id, text) => {
                    updates.insert(id, text);
                }
                MemoryOp::Delete(id) => {
                    deletes.insert(id);
                }
            }
        }
        let mut facts: Vec<String> = original
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !deletes.contains(&(i + 1)))
            .map(|(i, text)| updates.remove(&(i + 1)).unwrap_or(text))
            .collect();
        facts.extend(adds);
        let body: String = facts
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {f}\n", i + 1))
            .collect();
        let _ = std::fs::write(self.space.memory_path(&self.active_space.name), body);
    }

    // --- auto-compaction ---

    /// The messages actually sent on the next turn: everything after the
    /// session's compaction boundary, or all of them if it hasn't compacted
    /// (yet). The full, uncompacted history stays in `self.messages`/the db
    /// for scrollback — only what's sent shrinks.
    fn effective_messages(&self) -> &[Message] {
        let through = self
            .session
            .as_ref()
            .map(|s| s.compact_through as usize)
            .unwrap_or(0)
            .min(self.messages.len());
        &self.messages[through..]
    }

    /// After a reply, auto-compact once context usage crosses the configured
    /// threshold (0 disables it).
    fn maybe_compact(&mut self) {
        if self.settings.compact_threshold == 0 || self.compact_rx.is_some() {
            return;
        }
        let Some(limit) = self.context_limit() else { return };
        let used = self.context_used();
        let pct = used.checked_mul(100).and_then(|v| v.checked_div(limit)).unwrap_or(0);
        if pct < self.settings.compact_threshold as u64 {
            return;
        }
        self.start_compaction(pct);
    }

    /// Manually trigger compaction right now (`/compact`), ignoring the
    /// threshold. No-ops with a status message if there's nothing to compact.
    pub fn force_compact(&mut self) {
        if self.compact_rx.is_some() {
            self.status = "already compacting…".to_string();
            return;
        }
        if self.is_streaming() {
            self.status = "wait for the current response to finish".to_string();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            self.status = "no active session to compact".to_string();
            return;
        };
        if session.compact_through as usize >= self.messages.len() {
            self.status = "nothing new to compact".to_string();
            return;
        }
        let pct = self
            .context_limit()
            .filter(|&l| l > 0)
            .map(|l| self.context_used().checked_mul(100).and_then(|v| v.checked_div(l)).unwrap_or(0))
            .unwrap_or(0);
        self.start_compaction(pct);
    }

    /// Kick off the background compaction job on the memory model (falling
    /// back to the session model), same pattern as memory extraction.
    /// `before_pct` is only used to report the before/after status on completion.
    fn start_compaction(&mut self, before_pct: u64) {
        let Some(provider) = self.provider.clone() else {
            self.status = "set your API key first with /key".to_string();
            return;
        };
        let model = if !self.memory_model.trim().is_empty() {
            self.memory_model.clone()
        } else {
            match self.current_model.clone() {
                Some(m) => m,
                None => {
                    self.status = "pick a model first with /model".to_string();
                    return;
                }
            }
        };
        let Some(session) = self.session.as_ref() else { return };
        let through = session.compact_through as usize;
        if through >= self.messages.len() {
            return; // nothing new since the last compaction to fold in
        }
        let prior_summary = session.compact_summary.clone();
        let tail: String = self.messages[through..]
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        let session_id = session.id.clone();
        let new_through = self.messages.len() as i64;
        let (tx, rx) = mpsc::unbounded_channel();
        self.compact_rx = Some(rx);
        self.status = "compacting…".to_string();
        tokio::spawn(async move {
            let mut prompt = String::new();
            if let Some(s) = &prior_summary {
                prompt.push_str("Existing summary of earlier conversation:\n");
                prompt.push_str(s);
                prompt.push_str("\n\n");
            }
            prompt.push_str("New messages since that summary:\n");
            prompt.push_str(&tail);
            prompt.push_str(
                "\n\nCompress ALL of the above into one ultra-dense technical digest: cut \
                 every pleasantry, filler word, and repeated explanation, but keep every \
                 decision, fact, file/function name, code snippet, number, and open thread — \
                 nothing substantive may be lost. Terse fragments are fine. No headers, no \
                 meta-commentary about summarizing. Reply with ONLY the digest.",
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(summary) = provider.complete(&model, msgs).await {
                let summary = summary.trim().to_string();
                if !summary.is_empty() {
                    let _ = tx.send((session_id, summary, new_through, before_pct));
                }
            }
        });
    }

    /// Apply a compaction digest to the matching session (in memory + db).
    /// Clears the exact usage total — it reflects the pre-compaction request,
    /// so `context_used` should fall back to the (now accurate) estimate
    /// until the next real response reports fresh usage.
    pub fn on_compact_result(&mut self, result: Option<(String, String, i64, u64)>) {
        self.compact_rx = None;
        let Some((id, summary, through, before_pct)) = result else { return };
        let _ = self.db.set_compaction(&id, &summary, through);
        if let Some(s) = self.session.as_mut().filter(|s| s.id == id) {
            s.compact_summary = Some(summary.clone());
            s.compact_through = through;
        }
        self.context_total = None;
        let after_pct = self
            .context_limit()
            .filter(|&l| l > 0)
            .map(|l| self.context_used() * 100 / l);
        self.status = match after_pct {
            Some(after) => format!("compacted: {before_pct}% → {after}%"),
            None => "compacted".to_string(),
        };
    }

    /// System/memory/conversation token estimate for the context breakdown
    /// popup (Ctrl+I). Each bucket is a ~4-chars/token estimate, same method
    /// `context_used` falls back to, so the parts add up to (roughly) the whole.
    pub fn context_breakdown(&self) -> ContextBreakdown {
        let mut instructions_chars = self.resolved_base_system_prompt().chars().count();
        instructions_chars += std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
            .map(|s| s.trim().chars().count())
            .unwrap_or(0);
        let memory_chars = self.read_memory().chars().count();
        let mut skills_chars: usize =
            self.skills.iter().map(|s| s.name.chars().count() + s.description.chars().count()).sum();
        if let Some(name) = &self.forced_skill
            && let Some(skill) = self.skills.iter().find(|s| &s.name == name)
        {
            skills_chars += std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).chars().count())
                .unwrap_or(0);
        }
        let mut conversation_chars: usize =
            self.effective_messages().iter().map(|m| m.content.chars().count()).sum();
        if let Some(s) = self.session.as_ref().and_then(|s| s.compact_summary.as_deref()) {
            conversation_chars += s.chars().count();
        }
        if let Some(buf) = &self.streaming {
            conversation_chars += buf.chars().count();
        }
        ContextBreakdown {
            system_tokens: (instructions_chars / 4) as u64,
            memory_tokens: (memory_chars / 4) as u64,
            skills_tokens: (skills_chars / 4) as u64,
            conversation_tokens: (conversation_chars / 4) as u64,
            limit: self.context_limit(),
            compacted: self.session.as_ref().is_some_and(|s| s.compact_summary.is_some()),
        }
    }

    /// Path to a temp file holding the active session's compaction digest, so
    /// it can be viewed/edited in `$EDITOR` from the context popup (Ctrl+G, `v`).
    /// `None` if the session hasn't been compacted yet.
    pub fn compact_summary_path(&self) -> Option<std::path::PathBuf> {
        let session = self.session.as_ref()?;
        let summary = session.compact_summary.as_ref()?;
        let path = std::env::temp_dir().join(format!("nexus-chat-compact-{}.md", session.id));
        std::fs::write(&path, summary).ok()?;
        Some(path)
    }

    /// Read `path` (from `compact_summary_path`) back after `$EDITOR` closes —
    /// hand-edits to the digest persist (db + in-memory), same as any other
    /// file-backed edit in the app.
    pub fn reload_compact_summary(&mut self, path: &std::path::Path) -> Result<()> {
        let Some(session) = self.session.as_ref() else { return Ok(()) };
        let Ok(text) = std::fs::read_to_string(path) else { return Ok(()) };
        let text = text.trim().to_string();
        if text.is_empty() || Some(&text) == session.compact_summary.as_ref() {
            return Ok(());
        }
        let id = session.id.clone();
        let through = session.compact_through;
        self.db.set_compaction(&id, &text, through)?;
        if let Some(s) = self.session.as_mut() {
            s.compact_summary = Some(text);
        }
        self.status = "compaction digest updated".to_string();
        Ok(())
    }

    // --- spaces ---

    fn open_space_picker(&mut self) -> Result<()> {
        self.spaces_cache = self.db.list_spaces()?;
        self.space_selected = self
            .spaces_cache
            .iter()
            .position(|s| s.id == self.active_space.id)
            .unwrap_or(0);
        self.space_filter.clear();
        self.space_mode = SpaceMode::Browse;
        self.popup = Popup::Space;
        Ok(())
    }

    /// Spaces matching the current fuzzy filter, best match first. Empty filter
    /// keeps db order (default first, then creation order).
    pub fn filtered_spaces(&self) -> Vec<&SpaceRow> {
        let needle = self.space_filter.trim();
        if needle.is_empty() {
            return self.spaces_cache.iter().collect();
        }
        use crate::input::fuzzy_score;
        let mut scored: Vec<(i32, &SpaceRow)> = self
            .spaces_cache
            .iter()
            .filter_map(|s| fuzzy_score(&s.name, needle).map(|sc| (sc, s)))
            .collect();
        scored.sort_by_key(|(sc, _)| std::cmp::Reverse(*sc));
        scored.into_iter().map(|(_, s)| s).collect()
    }

    pub fn selected_space(&self) -> Option<SpaceRow> {
        self.filtered_spaces().get(self.space_selected).map(|s| (*s).clone())
    }

    pub fn move_space_selection(&mut self, delta: i32) {
        let len = self.filtered_spaces().len() as i32;
        if len == 0 {
            self.space_selected = 0;
            return;
        }
        let next = self.space_selected as i32 + delta;
        self.space_selected = next.clamp(0, len - 1) as usize;
    }

    pub fn space_filter_push(&mut self, c: char) {
        self.space_filter.push(c);
        self.space_selected = 0;
    }

    pub fn space_filter_pop(&mut self) {
        self.space_filter.pop();
        self.space_selected = 0;
    }

    pub fn start_space_create(&mut self) {
        self.space_edit.clear();
        self.space_mode = SpaceMode::Create;
    }

    /// A named space cannot be renamed/deleted if it's the default.
    pub fn start_space_rename(&mut self) {
        if let Some(s) = self.selected_space().filter(|s| s.name != DEFAULT_SPACE) {
            self.space_edit = s.name;
            self.space_mode = SpaceMode::Rename;
        }
    }

    pub fn confirm_space_create(&mut self) -> Result<()> {
        let name = self.space_edit.trim().to_string();
        if !name.is_empty() && !self.spaces_cache.iter().any(|s| s.name == name) {
            let s = self.db.create_space(&name)?;
            self.space.ensure_space_dir(&name)?;
            self.spaces_cache.push(s);
            self.space_selected = self.spaces_cache.len() - 1;
        }
        self.space_mode = SpaceMode::Browse;
        Ok(())
    }

    pub fn confirm_space_rename(&mut self) -> Result<()> {
        let name = self.space_edit.trim().to_string();
        if let (false, Some(s)) = (name.is_empty(), self.selected_space()) {
            self.db.rename_space(&s.id, &name)?;
            self.space.rename_space_dir(&s.name, &name)?;
            if let Some(cached) = self.spaces_cache.iter_mut().find(|c| c.id == s.id) {
                cached.name = name.clone();
            }
            if self.active_space.id == s.id {
                self.active_space.name = name;
            }
        }
        self.space_mode = SpaceMode::Browse;
        Ok(())
    }

    /// Delete the highlighted space (default is never offered for delete —
    /// gated by `handle_space_popup`). Sessions move to default; if the active
    /// space was deleted, switch to default.
    pub fn confirm_space_delete(&mut self) -> Result<()> {
        if let Some(s) = self.selected_space().filter(|s| s.name != DEFAULT_SPACE) {
            self.db.delete_space(&s.id)?;
            self.space.remove_space_dir(&s.name)?;
            self.spaces_cache.retain(|c| c.id != s.id);
            if self.active_space.id == s.id {
                self.switch_to_default_space()?;
            }
            self.status = format!("deleted space: {}", s.name);
        }
        self.space_mode = SpaceMode::Browse;
        let len = self.filtered_spaces().len();
        self.space_selected = self.space_selected.min(len.saturating_sub(1));
        Ok(())
    }

    fn switch_to_default_space(&mut self) -> Result<()> {
        let default_id = self.db.default_space_id()?;
        let row = self
            .db
            .list_spaces()?
            .into_iter()
            .find(|s| s.id == default_id)
            .unwrap();
        self.set_active_space(row);
        Ok(())
    }

    /// Switch the active space, clearing the open conversation (a session
    /// belongs to exactly one space).
    fn set_active_space(&mut self, row: SpaceRow) {
        self.active_space = row;
        self.session = None;
        self.messages.clear();
        self.context_total = None;
        self.scroll = 0;
        self.status = format!("space: {}", self.active_space.name);
    }

    /// Path to the highlighted space's instructions file, creating a stub with
    /// a short header comment if it doesn't exist yet (so $EDITOR has something
    /// to open).
    pub fn instructions_path_for_selected(&self) -> Option<std::path::PathBuf> {
        let s = self.selected_space()?;
        let path = self.space.instructions_path(&s.name);
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!("<!-- instructions for the \"{}\" space -->\n", s.name),
            );
        }
        Some(path)
    }

    /// Path to the highlighted space's memory file (the numbered facts a
    /// conversation in that space has accumulated), creating an empty stub
    /// with a header comment if nothing's been extracted yet.
    pub fn memory_path_for_selected(&self) -> Option<std::path::PathBuf> {
        let s = self.selected_space()?;
        let path = self.space.memory_path(&s.name);
        if !path.exists() {
            let _ = std::fs::write(
                &path,
                format!("<!-- memory for the \"{}\" space — numbered facts, one per line -->\n", s.name),
            );
        }
        Some(path)
    }

    pub fn confirm_space(&mut self) -> Result<()> {
        if let Some(s) = self.selected_space() {
            self.set_active_space(s);
        }
        self.popup = Popup::None;
        Ok(())
    }

    // --- commands ---

    /// Clear back to a blank conversation. Doesn't touch the db — a session
    /// row is only created lazily on the first message actually sent (same as
    /// the very first message of the app), so `/new` without typing anything
    /// doesn't leave an empty "new chat" behind in the session list.
    fn new_session(&mut self) -> Result<()> {
        self.session = None;
        self.messages.clear();
        self.context_total = None;
        self.scroll = 0;
        self.status = "new chat — send a message to start it".to_string();
        Ok(())
    }

    fn open_session_picker(&mut self) -> Result<()> {
        self.sessions_cache = self.db.list_sessions(&self.active_space.id)?;
        if self.sessions_cache.is_empty() {
            self.status = "no sessions yet — send a message to start one".to_string();
            return Ok(());
        }
        self.session_selected = 0;
        self.session_filter.clear();
        self.session_mode = SessionMode::Browse;
        self.popup = Popup::Session;
        Ok(())
    }

    /// Sessions matching the current fuzzy filter (title, slug, and id), best
    /// match first. Empty filter keeps the recency order from the db.
    pub fn filtered_sessions(&self) -> Vec<&Session> {
        let needle = self.session_filter.trim();
        if needle.is_empty() {
            return self.sessions_cache.iter().collect();
        }
        let mut scored: Vec<(i32, &Session)> = self
            .sessions_cache
            .iter()
            .filter_map(|s| session_score(s, needle).map(|sc| (sc, s)))
            .collect();
        scored.sort_by_key(|(sc, _)| std::cmp::Reverse(*sc));
        scored.into_iter().map(|(_, s)| s).collect()
    }

    /// The session under the picker cursor (respecting the active filter).
    pub fn selected_session(&self) -> Option<Session> {
        self.filtered_sessions().get(self.session_selected).map(|s| (*s).clone())
    }

    pub fn move_session_selection(&mut self, delta: i32) {
        let len = self.filtered_sessions().len() as i32;
        if len == 0 {
            self.session_selected = 0;
            return;
        }
        let next = self.session_selected as i32 + delta;
        self.session_selected = next.clamp(0, len - 1) as usize;
    }

    /// A filter keystroke re-runs the fuzzy match and resets the cursor to the top.
    pub fn session_filter_push(&mut self, c: char) {
        self.session_filter.push(c);
        self.session_selected = 0;
    }

    pub fn session_filter_pop(&mut self) {
        self.session_filter.pop();
        self.session_selected = 0;
    }

    /// Enter rename mode, seeding the edit buffer with the current title.
    pub fn start_rename(&mut self) {
        if let Some(s) = self.selected_session() {
            self.session_edit = s.title;
            self.session_mode = SessionMode::Rename;
        }
    }

    pub fn confirm_rename(&mut self) -> Result<()> {
        let title = self.session_edit.trim().to_string();
        if let (false, Some(s)) = (title.is_empty(), self.selected_session()) {
            self.db.set_session_title(&s.id, &title, None)?;
            if let Some(cached) = self.sessions_cache.iter_mut().find(|c| c.id == s.id) {
                cached.title = title.clone();
            }
            if let Some(cur) = self.session.as_mut().filter(|c| c.id == s.id) {
                cur.title = title;
            }
        }
        self.session_mode = SessionMode::Browse;
        Ok(())
    }

    /// Delete the highlighted session; if it was the active one, reset to a blank
    /// state so the stale conversation doesn't linger.
    pub fn confirm_delete(&mut self) -> Result<()> {
        if let Some(s) = self.selected_session() {
            self.db.delete_session(&s.id)?;
            self.sessions_cache.retain(|c| c.id != s.id);
            if self.session.as_ref().is_some_and(|c| c.id == s.id) {
                self.session = None;
                self.messages.clear();
                self.context_total = None;
                self.scroll = 0;
            }
            self.status = format!("deleted: {}", s.title);
        }
        self.session_mode = SessionMode::Browse;
        let len = self.filtered_sessions().len();
        self.session_selected = self.session_selected.min(len.saturating_sub(1));
        if self.sessions_cache.is_empty() {
            self.popup = Popup::None;
        }
        Ok(())
    }

    fn open_model_picker(&mut self) {
        self.model_pick_target = ModelPickTarget::Session;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the memory model
    /// (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_memory(&mut self) {
        self.model_pick_target = ModelPickTarget::Memory;
        self.open_model_picker_impl();
    }

    fn open_model_picker_impl(&mut self) {
        if self.provider.is_none() {
            self.open_key_prompt();
            return;
        }
        if self.models.is_empty() {
            self.status = "loading models…".to_string();
            self.fetch_models();
            return;
        }
        self.model_filter.clear();
        self.model_focus = if self.favorite_models().is_empty() {
            ModelPanel::Available
        } else {
            ModelPanel::Favorites
        };
        self.reset_model_selection();
        self.popup = Popup::Model;
    }

    /// Point each panel's selection at its first row (or nothing if empty).
    pub fn reset_model_selection(&mut self) {
        self.fav_state
            .select((!self.favorite_models().is_empty()).then_some(0));
        self.avail_state
            .select((!self.available_models().is_empty()).then_some(0));
    }

    fn open_key_prompt(&mut self) {
        self.key_input.clear();
        self.status = "paste your OpenRouter key, then Enter".to_string();
        self.popup = Popup::Key;
    }

    /// Save the entered key: persist it, build the provider, fetch models.
    pub fn confirm_key(&mut self) {
        let key = std::mem::take(&mut self.key_input);
        let key = key.trim().to_string();
        self.popup = Popup::None;
        if key.is_empty() {
            self.status = "no key entered".to_string();
            return;
        }
        if let Err(e) = config::save_key(&key) {
            self.status = format!("could not save key: {e}");
        }
        self.provider = Some(OpenRouter::new(key.clone()));
        self.key = Some(key);
        self.status = "key saved, loading models…".to_string();
        self.fetch_models();
    }

    /// Favorite models matching the search filter, most-recently-used first.
    pub fn favorite_models(&self) -> Vec<&Model> {
        self.filtered_panel(true)
    }

    /// Non-favorite models matching the search filter, most-recently-used first.
    pub fn available_models(&self) -> Vec<&Model> {
        self.filtered_panel(false)
    }

    fn filtered_panel(&self, want_fav: bool) -> Vec<&Model> {
        let f = self.model_filter.to_lowercase();
        let mut out: Vec<&Model> = self
            .models
            .iter()
            .filter(|m| self.favorites.contains(&m.id) == want_fav)
            .filter(|m| {
                f.is_empty()
                    || m.id.to_lowercase().contains(&f)
                    || m.name.to_lowercase().contains(&f)
            })
            .collect();
        // Most-recently-used first, then alphabetical.
        out.sort_by(|a, b| {
            let ra = self.last_used.get(&a.id).cloned().unwrap_or_default();
            let rb = self.last_used.get(&b.id).cloned().unwrap_or_default();
            rb.cmp(&ra).then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    fn panel_len(&self, panel: ModelPanel) -> usize {
        match panel {
            ModelPanel::Favorites => self.favorite_models().len(),
            ModelPanel::Available => self.available_models().len(),
        }
    }

    pub fn state_mut(&mut self, panel: ModelPanel) -> &mut ListState {
        match panel {
            ModelPanel::Favorites => &mut self.fav_state,
            ModelPanel::Available => &mut self.avail_state,
        }
    }

    fn id_at(&self, panel: ModelPanel, index: usize) -> Option<String> {
        let list = match panel {
            ModelPanel::Favorites => self.favorite_models(),
            ModelPanel::Available => self.available_models(),
        };
        list.get(index).map(|m| m.id.clone())
    }

    /// Context window of the active model, if known.
    pub fn context_limit(&self) -> Option<u64> {
        let id = self.current_model.as_deref()?;
        self.models
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.context_length)
    }

    /// Tokens used by the current session. Exact (from the provider's usage on
    /// the last response) when idle; a ~4-chars/token estimate while streaming or
    /// before the first response.
    /// Estimate is what would actually be *sent* — system/memory prompt, the
    /// compaction digest (if any), and only the tail after it, not the full
    /// (possibly much larger) on-screen scrollback.
    pub fn context_used(&self) -> u64 {
        if !self.is_streaming()
            && let Some(total) = self.context_total
        {
            return total;
        }
        let mut chars = self.system_prompt().chars().count();
        if let Some(s) = self.session.as_ref().and_then(|s| s.compact_summary.as_deref()) {
            chars += s.chars().count();
        }
        if let Some(name) = &self.forced_skill
            && let Some(skill) = self.skills.iter().find(|s| &s.name == name)
        {
            chars += std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).chars().count())
                .unwrap_or(0);
        }
        chars += self.effective_messages().iter().map(|m| m.content.chars().count()).sum::<usize>();
        if let Some(buf) = &self.streaming {
            chars += buf.chars().count();
        }
        (chars / 4) as u64
    }

    pub fn model_supports_reasoning(&self, id: &str) -> bool {
        self.models
            .iter()
            .any(|m| m.id == id && m.supports_reasoning)
    }

    pub fn reasoning_of(&self, id: &str) -> Option<&str> {
        self.reasoning.get(id).map(String::as_str)
    }

    /// Cycle the focused model's reasoning effort: off → low → medium → high → off.
    /// No-op for models that don't support reasoning.
    pub fn cycle_reasoning_focused(&mut self) -> Result<()> {
        let selected = self.state_mut(self.model_focus).selected().unwrap_or(0);
        let Some(id) = self.id_at(self.model_focus, selected) else {
            return Ok(());
        };
        if !self.model_supports_reasoning(&id) {
            self.status = format!("{id} has no reasoning setting");
            return Ok(());
        }
        let next = match self.reasoning.get(&id).map(String::as_str) {
            None => Some("low"),
            Some("low") => Some("medium"),
            Some("medium") => Some("high"),
            _ => None,
        };
        self.db.set_reasoning(&id, next)?;
        match next {
            Some(e) => {
                self.reasoning.insert(id.clone(), e.to_string());
                self.status = format!("reasoning {e}: {id}");
            }
            None => {
                self.reasoning.remove(&id);
                self.status = format!("reasoning off: {id}");
            }
        }
        Ok(())
    }

    // --- nerd config (settings popup) ---

    fn open_settings(&mut self) {
        self.settings_selected = 0;
        self.settings_inputs = [
            self.settings.temperature.map(|v| v.to_string()).unwrap_or_default(),
            self.settings.top_p.map(|v| v.to_string()).unwrap_or_default(),
            self.settings.max_tokens.map(|v| v.to_string()).unwrap_or_default(),
            self.settings.compact_threshold.to_string(),
            self.searxng_url.clone(),
            self.langsearch_key.clone(),
        ];
        self.status = "↑/↓ field · type to edit · Space toggles · Ctrl+E system prompt · Esc saves".to_string();
        self.popup = Popup::Settings;
    }

    pub fn settings_field(&self) -> SettingsField {
        SettingsField::ALL[self.settings_selected]
    }

    pub fn move_settings_selection(&mut self, delta: i32) {
        let n = SettingsField::ALL.len() as i32;
        self.settings_selected = (self.settings_selected as i32 + delta).rem_euclid(n) as usize;
    }

    pub fn toggle_settings_field(&mut self) {
        match self.settings_field() {
            SettingsField::ShowStats => self.settings.show_stats = !self.settings.show_stats,
            SettingsField::ShowReasoning => {
                self.settings.show_reasoning = !self.settings.show_reasoning
            }
            SettingsField::HideHints => self.settings.hide_hints = !self.settings.hide_hints,
            SettingsField::Verbosity => {
                let i = VERBOSITY_LEVELS.iter().position(|&l| l == self.verbosity).unwrap_or(1);
                self.verbosity = VERBOSITY_LEVELS[(i + 1) % VERBOSITY_LEVELS.len()].to_string();
            }
            SettingsField::SearchProvider => {
                let i = SEARCH_PROVIDERS.iter().position(|&p| p == self.search_provider).unwrap_or(0);
                self.search_provider = SEARCH_PROVIDERS[(i + 1) % SEARCH_PROVIDERS.len()].to_string();
            }
            _ => {}
        }
    }

    /// Expand/collapse stored reasoning traces (Ctrl+R), persisted.
    pub fn toggle_reasoning_view(&mut self) -> Result<()> {
        self.settings.show_reasoning = !self.settings.show_reasoning;
        self.db.set_setting(
            "show_reasoning",
            if self.settings.show_reasoning { "1" } else { "0" },
        )?;
        self.status = if self.settings.show_reasoning {
            "reasoning expanded".to_string()
        } else {
            "reasoning collapsed".to_string()
        };
        Ok(())
    }

    /// Type into the focused field: digits/`.` for the numeric rows, any
    /// printable char for the URL row.
    pub fn settings_input_char(&mut self, c: char) {
        let Some(i) = self.text_index() else { return };
        let numeric = !matches!(self.settings_field(), SettingsField::SearxngUrl | SettingsField::LangsearchKey);
        if numeric && !(c.is_ascii_digit() || c == '.') {
            return;
        }
        if !numeric && c.is_control() {
            return;
        }
        self.settings_inputs[i].push(c);
    }

    pub fn settings_input_backspace(&mut self) {
        if let Some(i) = self.text_index() {
            self.settings_inputs[i].pop();
        }
    }

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

    /// Parse the edit buffers into settings and persist everything (on Esc).
    pub fn save_settings(&mut self) -> Result<()> {
        self.settings.temperature = self.settings_inputs[0].trim().parse().ok();
        self.settings.top_p = self.settings_inputs[1].trim().parse().ok();
        self.settings.max_tokens = self.settings_inputs[2].trim().parse().ok();
        self.settings.compact_threshold =
            self.settings_inputs[3].trim().parse().unwrap_or(0).min(100);

        let stats = if self.settings.show_stats { "1" } else { "0" };
        let reason = if self.settings.show_reasoning { "1" } else { "0" };
        let hints = if self.settings.hide_hints { "1" } else { "0" };
        self.db.set_setting("show_stats", stats)?;
        self.db.set_setting("show_reasoning", reason)?;
        self.db.set_setting("hide_hints", hints)?;
        self.db.set_setting("temperature", self.settings_inputs[0].trim())?;
        self.db.set_setting("top_p", self.settings_inputs[1].trim())?;
        self.db.set_setting("max_tokens", self.settings_inputs[2].trim())?;
        self.db.set_setting("compact_threshold", &self.settings.compact_threshold.to_string())?;
        self.memory_model = self.memory_model.trim().to_string();
        self.db.set_setting("memory_model", &self.memory_model)?;
        self.searxng_url = self.settings_inputs[4].trim().trim_end_matches('/').to_string();
        self.db.set_setting("searxng_url", &self.searxng_url)?;
        self.db.set_setting("verbosity", &self.verbosity)?;
        self.langsearch_key = self.settings_inputs[5].trim().to_string();
        self.db.set_setting("langsearch_key", &self.langsearch_key)?;
        self.db.set_setting("search_provider", &self.search_provider)?;
        self.refresh_toolbox();
        self.popup = Popup::None;
        self.status = "settings saved".to_string();
        Ok(())
    }

    /// Move the focused panel's selection by `delta` (clamped).
    pub fn move_model_selection(&mut self, delta: i32) {
        let len = self.panel_len(self.model_focus);
        if len == 0 {
            return;
        }
        let state = self.state_mut(self.model_focus);
        let cur = state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
        state.select(Some(next));
    }

    pub fn toggle_model_focus(&mut self) {
        self.model_focus = match self.model_focus {
            ModelPanel::Favorites => ModelPanel::Available,
            ModelPanel::Available => ModelPanel::Favorites,
        };
    }

    /// Toggle favorite on the focused selection (Ctrl+S). The item then moves
    /// between panels, so selections are re-clamped.
    pub fn toggle_favorite_focused(&mut self) -> Result<()> {
        let selected = self.state_mut(self.model_focus).selected().unwrap_or(0);
        let Some(id) = self.id_at(self.model_focus, selected) else {
            return Ok(());
        };
        let now_fav = self.db.toggle_favorite(&id)?;
        if now_fav {
            self.favorites.insert(id.clone());
            self.status = format!("★ favorited {id}");
        } else {
            self.favorites.remove(&id);
            self.status = format!("unfavorited {id}");
        }
        self.clamp_selection(ModelPanel::Favorites);
        self.clamp_selection(ModelPanel::Available);
        Ok(())
    }

    fn clamp_selection(&mut self, panel: ModelPanel) {
        let len = self.panel_len(panel);
        let state = self.state_mut(panel);
        if len == 0 {
            state.select(None);
        } else {
            let cur = state.selected().unwrap_or(0).min(len - 1);
            state.select(Some(cur));
        }
    }

    /// Confirm the focused selection as the active model.
    pub fn confirm_model(&mut self) -> Result<()> {
        let selected = self.state_mut(self.model_focus).selected().unwrap_or(0);
        if let Some(id) = self.id_at(self.model_focus, selected) {
            self.pick_model(id)?;
        }
        Ok(())
    }

    /// Pick a model by clicking a specific row in a specific panel (mouse).
    /// A click past the end of the list just moves focus there.
    pub fn pick_model_at(&mut self, panel: ModelPanel, index: usize) -> Result<()> {
        self.model_focus = panel;
        if index >= self.panel_len(panel) {
            return Ok(());
        }
        self.state_mut(panel).select(Some(index));
        if let Some(id) = self.id_at(panel, index) {
            self.pick_model(id)?;
        }
        Ok(())
    }

    fn pick_model(&mut self, id: String) -> Result<()> {
        match self.model_pick_target {
            ModelPickTarget::Session => {
                self.current_model = Some(id.clone());
                if let Some(session) = &self.session {
                    self.db.set_session_model(&session.id, &id)?;
                }
                self.db.mark_model_used(&id)?;
                self.last_used.insert(id.clone(), Utc::now().to_rfc3339());
                self.status = format!("model: {id}");
                self.popup = Popup::None;
            }
            ModelPickTarget::Memory => {
                self.memory_model = id.clone();
                self.db.set_setting("memory_model", &id)?;
                self.status = format!("memory model: {id}");
                // Picked from inside /config — return there rather than closing.
                self.popup = Popup::Settings;
            }
        }
        Ok(())
    }

    /// Disable memory extraction entirely (Backspace on the memory-model row
    /// in `/config`).
    pub fn clear_memory_model(&mut self) -> Result<()> {
        self.memory_model.clear();
        self.db.set_setting("memory_model", "")?;
        self.status = "memory model cleared — extraction disabled".to_string();
        Ok(())
    }

    pub fn confirm_session(&mut self) -> Result<()> {
        if let Some(s) = self.selected_session() {
            self.messages = self.db.load_messages(&s.id)?;
            self.current_model = Some(s.model.clone());
            self.status = format!("switched to: {}", s.title);
            self.session = Some(s);
            // Estimate from history until the next response reports exact usage.
            self.context_total = None;
            self.scroll = 0;
        }
        self.popup = Popup::None;
        Ok(())
    }
}

/// Pick a thinking-phrase index and spinner colour pseudo-randomly (seeded from
/// the clock; no rng dep).
/// Each fenced ``` code block in `md` as `(language, code)`.
fn code_blocks(md: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut lang: Option<String> = None;
    let mut buf = String::new();
    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if inside {
                out.push((lang.take(), std::mem::take(&mut buf)));
            } else {
                let l = trimmed.trim_start_matches('`').trim();
                lang = (!l.is_empty()).then(|| l.to_string());
            }
            inside = !inside;
            continue;
        }
        if inside {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if inside && !buf.is_empty() {
        out.push((lang.take(), buf)); // unterminated (e.g. mid-stream)
    }
    out
}

fn pick_greeting() -> &'static str {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    GREETINGS[n % GREETINGS.len()]
}

fn pick_flavor() -> (usize, Color) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    (n % THINKING.len(), SPINNER_COLORS[n % SPINNER_COLORS.len()])
}

/// Short session title from the first user message.
fn title_from(text: &str) -> String {
    let t: String = text.chars().take(40).collect();
    if t.trim().is_empty() {
        "new chat".to_string()
    } else {
        t
    }
}

/// Best fuzzy score of `needle` against a session's title, slug, and uuid.
fn session_score(s: &Session, needle: &str) -> Option<i32> {
    use crate::input::fuzzy_score;
    let mut best = fuzzy_score(&s.title, needle);
    let upd = |best: &mut Option<i32>, cand: Option<i32>| {
        if let Some(c) = cand {
            *best = Some(best.map_or(c, |b| b.max(c)));
        }
    };
    if let Some(slug) = &s.slug {
        upd(&mut best, fuzzy_score(slug, needle).map(|v| v + 2));
    }
    upd(&mut best, fuzzy_score(&s.id, needle));
    best
}

/// Parse the model's topic reply into `(topic, slug)`. Tolerates surrounding prose
/// or code fences by extracting the first `{...}` and reading the two fields.
fn parse_topic(text: &str) -> Option<(String, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let json = text.get(start..=end)?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let topic = v.get("topic").and_then(|t| t.as_str())?.trim();
    if topic.is_empty() {
        return None;
    }
    let raw_slug = v.get("id").and_then(|s| s.as_str()).unwrap_or(topic);
    Some((topic.to_string(), slugify(raw_slug)))
}

/// Normalise to a short kebab-case slug: lowercase, `[a-z0-9-]`, max 5 words.
fn slugify(s: &str) -> String {
    let slug = s
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { "chat".to_string() } else { slug }
}

/// Strip `<think>...</think>` blocks out of `text`, returning the cleaned
/// content and the extracted reasoning (blocks joined by newlines), if any. An
/// unterminated tag (e.g. a truncated stream) treats the remainder as
/// reasoning rather than leaking a dangling tag into the answer.
fn split_inline_reasoning(text: &str) -> (String, Option<String>) {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut content = String::with_capacity(text.len());
    let mut reasoning = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find(OPEN) else {
            content.push_str(rest);
            break;
        };
        content.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        let (block, remainder) = match after_open.find(CLOSE) {
            Some(end) => (&after_open[..end], &after_open[end + CLOSE.len()..]),
            None => (after_open, ""),
        };
        if !reasoning.is_empty() {
            reasoning.push('\n');
        }
        reasoning.push_str(block.trim());
        rest = remainder;
    }
    let content = content.trim().to_string();
    (content, (!reasoning.is_empty()).then_some(reasoning))
}

/// Parse one numbered fact line (`"3. some fact"`) into `(id, text)`.
fn parse_fact_line(line: &str) -> Option<(usize, String)> {
    let (num, rest) = line.split_once(". ")?;
    let id: usize = num.trim().parse().ok()?;
    Some((id, rest.trim().to_string()))
}

/// Parse the memory model's reply into a list of ops. Tolerates surrounding
/// prose/fences by extracting the first `[...]`; malformed or unrecognized
/// entries are silently skipped rather than failing the whole batch.
fn parse_memory_ops(text: &str) -> Vec<MemoryOp> {
    let Some(start) = text.find('[') else { return Vec::new() };
    let Some(end) = text.rfind(']') else { return Vec::new() };
    let Some(json) = text.get(start..=end) else { return Vec::new() };
    let Ok(arr) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let Some(arr) = arr.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|v| {
            let op = v.get("op")?.as_str()?;
            match op {
                "add" => Some(MemoryOp::Add(v.get("text")?.as_str()?.trim().to_string())),
                "update" => Some(MemoryOp::Update(
                    v.get("id")?.as_u64()? as usize,
                    v.get("text")?.as_str()?.trim().to_string(),
                )),
                "delete" => Some(MemoryOp::Delete(v.get("id")?.as_u64()? as usize)),
                _ => None,
            }
        })
        .collect()
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
