// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use chrono::Utc;
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::config;
use crate::db::{DEFAULT_SPACE, Db, Message, Session, Space as SpaceRow};
use crate::filter_input::FilterInput;
use crate::input::new_textarea;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model, StreamEvent, Usage};
use crate::space::Space;
use crate::theme::Theme;

mod apps;
mod backends;
mod chat;
mod commands;
mod compaction;
mod copy;
pub mod export;
mod files;
pub mod headless;
mod images;
mod memory;
mod models;
mod research;
mod scripts;
mod sessions;
mod settings;
mod skills_popup;
mod snapshot;
mod spaces;
mod swarm;
#[cfg(test)]
mod tests;
mod transcribe;
pub mod usage;
mod watches;
pub use backends::{Backends, composite_id};
pub use chat::human_size;
pub use commands::AppCommand;

#[cfg(test)]
use chat::split_inline_reasoning;
use chat::{code_blocks, pick_greeting};
#[cfg(test)]
use memory::parse_memory_ops;
use sessions::parse_topic;

/// Nudge a bounded selection index by `delta`, hard-clamping to `[0, len-1]`
/// (or `0` if `len` is `0`). Shared by the picker/list selection-movement
/// methods that clamp at the ends rather than wrapping around.
pub fn clamp_cursor(current: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    (current as i32 + delta).clamp(0, len as i32 - 1) as usize
}

/// Filter `items` down to those `score_fn` returns `Some` for, sorted
/// descending by score (best match first, stable on ties). Shared by the
/// space and session pickers' fuzzy filters.
pub fn fuzzy_filter_sorted<T>(items: &[T], score_fn: impl Fn(&T) -> Option<i32>) -> Vec<&T> {
    let mut scored: Vec<(i32, &T)> = items
        .iter()
        .filter_map(|item| score_fn(item).map(|sc| (sc, item)))
        .collect();
    scored.sort_by_key(|(sc, _)| std::cmp::Reverse(*sc));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// Which tab in the `/files` popup is active.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum FilesTab {
    Files,
    Images,
    Scripts,
}

/// Which modal popover, if any, is open.
#[derive(Debug, PartialEq, Eq)]
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
    Watch,
    ResearchLive,
    Swarm,
    /// `/usage`: aggregated per-backend/per-model token, cache, and cost stats.
    Usage,
    /// `/login`'s provider selector (`OpenRouter` / `OpenCode` Go / `OpenAI` / Codex).
    Login,
}

/// Which backend a pasted key in `Popup::Key` is for — set by whichever
/// `/login` row opened the prompt, since these keys aren't distinguishable
/// by shape (unlike `OpenRouter`'s `sk-or-` prefix).
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum KeyTarget {
    OpenRouter,
    OpenAi,
    OpencodeGo,
}

/// What the `/swarm` roster popup is doing.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SwarmPopupMode {
    Browse,
    ConfirmDelete,
}

/// What the apps popup is doing: browsing the space's apps or confirming
/// removal of the highlighted one.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum AppsMode {
    Browse,
    ConfirmDelete,
    EditFile,
}

/// What the watch picker is doing: browsing or confirming removal of the
/// highlighted watch.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WatchMode {
    Browse,
    ConfirmDelete,
}

/// What the skills popup is doing: browsing, typing a GitHub `owner/repo/path`
/// to install, or confirming removal of the highlighted skill.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SkillsMode {
    Browse,
    Install,
    ConfirmRemove,
}

/// What the files popup is doing: browsing the fileset, typing a path to
/// import, renaming the highlighted file, or confirming its removal.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum FilesMode {
    Browse,
    Add,
    Rename,
    ConfirmDelete,
    Pick,
}

/// What the images popup is doing: browsing or confirming removal.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ImagesMode {
    Browse,
    ConfirmDelete,
}

/// What the scripts popup is doing: browsing, creating, renaming, or
/// confirming removal of the highlighted script.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ScriptsMode {
    Browse,
    Create,
    Rename,
    ConfirmDelete,
}

/// What the space picker is doing: browsing, naming a new space, renaming the
/// highlighted one, or confirming a delete.
#[derive(PartialEq, Eq, Clone, Copy)]
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
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum SessionMode {
    Browse,
    Rename,
    ConfirmDelete,
}

/// Which pane an in-progress mouse press is driving.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum MouseTarget {
    None,
    Input,
    History,
}

/// The two columns of the model picker.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ModelPanel {
    Favorites,
    Available,
}

/// What a confirmed model picker selection is for: the active session's model,
/// or the background memory-extraction model.
#[derive(PartialEq, Eq, Clone, Copy, Default)]
pub enum ModelPickTarget {
    #[default]
    Session,
    Memory,
    Transcriber,
    Ocr,
    /// Picking the model for one row of the active session's `/swarm` roster.
    SwarmPersona(usize),
    /// Model used for AI image generation.
    ImageGen,
    /// Model used for AI video generation.
    VideoGen,
}

/// Editable rows in the nerd-config popup.
#[derive(PartialEq, Eq, Clone, Copy)]
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
    OcrModel,
    OcrEngine,
    EmbeddingModel,
    BlockedDomains,
    ImageGenModel,
    VideoGenModel,
}

impl SettingsField {
    pub const ALL: [Self; 19] = [
        Self::ShowStats,
        Self::ShowReasoning,
        Self::HideHints,
        Self::Temperature,
        Self::TopP,
        Self::MaxTokens,
        Self::MemoryModel,
        Self::CompactThreshold,
        Self::SearxngUrl,
        Self::Verbosity,
        Self::LangsearchKey,
        Self::SearchProvider,
        Self::TranscriberModel,
        Self::OcrModel,
        Self::OcrEngine,
        Self::EmbeddingModel,
        Self::BlockedDomains,
        Self::ImageGenModel,
        Self::VideoGenModel,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::ShowStats => "show stats (model · TPS footer)",
            Self::ShowReasoning => "expand reasoning (Ctrl+R)",
            Self::HideHints => "hide hints (keybind labels)",
            Self::Temperature => "temperature",
            Self::TopP => "top_p",
            Self::MaxTokens => "max_tokens",
            Self::MemoryModel => "memory model (Enter to pick, Backspace clears)",
            Self::CompactThreshold => "auto-compact at (% of context, 0 disables)",
            Self::SearxngUrl => "web search URL (SearXNG instance, blank disables)",
            Self::Verbosity => "answer length (Space cycles normal/concise/caveman)",
            Self::LangsearchKey => "LangSearch API key (langsearch.com/dashboard, free)",
            Self::SearchProvider => {
                "search provider (Space cycles auto/langsearch/searxng/duckduckgo)"
            }
            Self::TranscriberModel => "image model (Enter to pick, Backspace clears)",
            Self::OcrModel => "OCR model (Enter to pick, Backspace clears)",
            Self::OcrEngine => {
                "OCR engine (Space cycles auto/tesseract/vlm/local; local pulls via ollama)"
            }
            Self::EmbeddingModel => "embedding model (file search, blank disables)",
            Self::BlockedDomains => "blocked domains (comma-separated, always excluded; per-space)",
            Self::ImageGenModel => {
                "image gen model (Enter to pick, Backspace clears; blank = disabled)"
            }
            Self::VideoGenModel => {
                "video gen model (Enter to pick, Backspace clears; blank = disabled)"
            }
        }
    }
}

/// A functional group of settings fields, shown as a collapsible section in
/// the nerd-config popup — grouped by what part of the pipeline the fields
/// configure (chat display, sampling, memory, research, web search, or
/// voice/vision input), not by widget type.
pub struct SettingsGroup {
    pub name: &'static str,
    pub fields: &'static [SettingsField],
}

pub const SETTINGS_GROUPS: &[SettingsGroup] = &[
    SettingsGroup {
        name: "Interface",
        fields: &[
            SettingsField::ShowStats,
            SettingsField::ShowReasoning,
            SettingsField::HideHints,
            SettingsField::Verbosity,
        ],
    },
    SettingsGroup {
        name: "Generation",
        fields: &[
            SettingsField::Temperature,
            SettingsField::TopP,
            SettingsField::MaxTokens,
        ],
    },
    SettingsGroup {
        name: "Memory & Context",
        fields: &[
            SettingsField::MemoryModel,
            SettingsField::CompactThreshold,
            SettingsField::EmbeddingModel,
        ],
    },
    SettingsGroup {
        name: "Web Search",
        fields: &[
            SettingsField::SearchProvider,
            SettingsField::SearxngUrl,
            SettingsField::LangsearchKey,
            SettingsField::BlockedDomains,
        ],
    },
    SettingsGroup {
        name: "Voice & Vision",
        fields: &[
            SettingsField::TranscriberModel,
            SettingsField::OcrModel,
            SettingsField::OcrEngine,
        ],
    },
    SettingsGroup {
        name: "Image Generation",
        fields: &[SettingsField::ImageGenModel],
    },
    SettingsGroup {
        name: "Video Generation",
        fields: &[SettingsField::VideoGenModel],
    },
];

/// One visible row in the settings popup: a collapsible group header, or a
/// field nested under one (only present when its group isn't collapsed).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    Group(usize),
    Field(SettingsField),
}

const VERBOSITY_LEVELS: [&str; 3] = ["normal", "concise", "caveman"];
pub const OCR_ENGINES: [&str; 4] = ["auto", "tesseract", "vlm", "local"];
const SEARCH_PROVIDERS: [&str; 4] = ["auto", "langsearch", "searxng", "duckduckgo"];

/// Nerd config: footer toggles + core sampling parameters.
#[derive(Debug, Clone)]
// A settings struct is inherently many booleans; grouped by feature in the popup.
#[allow(clippy::struct_excessive_bools)]
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
        Self {
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
        "normal" => {
            "Answer at whatever length the question deserves — don't optimize for brevity over completeness."
        }
        "caveman" => {
            "Talk caveman-terse. Drop articles (a/an/the) and filler words. Short fragments over full sentences. No politeness, no hedging, no restating the question. Symbols over words where clear (→, =, vs). Full explanations ONLY when explicitly asked — otherwise state the fact/answer and stop. Preserve numbers, code, names, and technical terms exactly."
        }
        _ => {
            "Answer style: default to short. Say the answer, then stop — don't restate the question, don't add a summary, don't hedge (\"it's worth noting...\", \"generally speaking...\"). No preamble (\"Great question!\", \"I'd be happy to help with that\"), no postamble (\"Let me know if you have questions!\"). One clear sentence beats three vague ones.\nThis is a floor, not a ceiling: if the user asks to explain, teach, or go deep, or the topic genuinely needs multiple steps to be correct (debugging, multi-part instructions, tradeoffs), give it the room it needs. Brevity for its own sake that omits a needed step is wrong, not concise.\nKeep full grammar — this isn't telegraphic shorthand. Drop filler, not clarity."
        }
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

/// Spinner colour for an in-flight response. Core names the palette
/// abstractly (the TUI maps it to terminal colors) so this type can cross
/// the Phase 4 API boundary unchanged.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SpinnerColor {
    Green,
    Cyan,
    Magenta,
}

/// Palette the spinner colour is randomly drawn from each request.
const SPINNER_COLORS: [SpinnerColor; 3] = [
    SpinnerColor::Green,
    SpinnerColor::Cyan,
    SpinnerColor::Magenta,
];

type ModelsResult = std::result::Result<Vec<Model>, String>;

/// One memory-extraction op, as emitted by the memory model.
pub enum MemoryOp {
    Add(String),
    Update(usize, String),
    Delete(usize),
}

/// One row in the `/image` popup: name, size in bytes, modified rfc3339.
#[derive(Clone)]
pub struct ImageMeta {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

/// One row in the `/script` popup: name, size in bytes, modified rfc3339.
#[derive(Clone)]
pub struct ScriptMeta {
    pub name: String,
    pub size: u64,
    pub modified: String,
}

/// Maximum number of concurrent interactive chat responses.
pub const MAX_CHAT_TASKS: usize = 10;

pub type ChatTaskId = u64;

/// Identity of a parked survey/plan gate, as surfaced on the event stream.
pub struct GateState {
    /// The session the reply must come from.
    pub session_id: String,
    /// Which phase is waiting (drives the prompt shown).
    pub phase: SurveyPhase,
}

/// One event routed from a provider stream to its originating chat task.
pub struct ChatEvent {
    pub task_id: ChatTaskId,
    pub event: StreamEvent,
}

/// State owned by one in-flight chat response. Provider/toolbox values stay in
/// the spawned task; this state is the UI/database-facing projection.
pub struct ChatTask {
    pub id: ChatTaskId,
    pub session_id: String,
    pub session_title: String,
    pub space_id: String,
    pub model: String,
    /// Raw wire model id (backend prefix stripped) — the key for pricing.
    pub model_id: String,
    /// Which backend serves this task's requests.
    pub backend: BackendTag,
    pub incognito: bool,
    pub buffer: String,
    pub thinking: String,
    pub tool_status: Option<String>,
    pub usage: Option<Usage>,
    /// Row id of this task's `usage_log` row (one per request); the trailing
    /// `OpenCode` Zen cost event updates it rather than inserting a duplicate.
    pub usage_row_id: Option<i64>,
    pub started: std::time::Instant,
    pub thinking_idx: usize,
    pub spinner_color: SpinnerColor,
    pub abort: tokio::task::AbortHandle,
}

/// A completed chat task waiting for the user to open its session.
pub struct ChatNotification {
    pub session_id: String,
    pub title: String,
    pub text: String,
    pub success: bool,
}

/// A background event surfaced to the event loop. `None` means that source's
/// channel closed (task ended).
/// One file's embedding result: (space id, file id, (seq, vector) pairs or error).
pub type EmbedMsg = (
    String,
    String,
    std::result::Result<Vec<(i64, Vec<f32>)>, String>,
);

/// A background research pipeline update: (session id, space id, space name,
/// stage update or final result).
pub type ResearchMsg = (String, String, String, research::ResearchUpdate);

/// A parked conversation awaiting a chat reply: which session the reply
/// must come from, the channel to send it on, and which phase is waiting.
/// Generic — the research survey/plan-approval gates ride it, and any other
/// mode (swarm, watch setup, plain chat) can arm it the same way. Armed by
/// the owning job's update handler on each pending section, cleared when
/// the user replies or the job ends.
pub struct SurveyGate {
    pub session_id: String,
    pub reply_tx: mpsc::UnboundedSender<String>,
    pub phase: SurveyPhase,
    /// The actionable transcript row for this gate. Keeping it with the gate
    /// lets an incognito prompt be restored after the user switches away and
    /// back without writing private content to the database.
    pub prompt_role: String,
    pub prompt_content: String,
}

/// What a parked survey gate is waiting for — drives the status line and
/// which phase's reply is routed. Mode-agnostic: `Clarify` is any
/// clarifying-question round, `Approve` any presented-artifact approval.
#[derive(Clone)]
pub enum SurveyPhase {
    /// A clarifying-question round (1-based).
    Clarify { round: u8 },
    /// Approval of a presented artifact; `rework` is true on a
    /// re-presentation after the user's edits were folded in.
    Approve { rework: bool },
}

pub enum LoginMsg {
    Status(String),
    Done(Result<crate::config::CodexCredentials, String>),
}

/// External-editor request queued by app/input code. The event loop owns the
/// terminal suspend/resume and applies structured edits after `$EDITOR` exits.
pub enum PendingEditor {
    AppFile(std::path::PathBuf),
    Persona(std::path::PathBuf),
    ScriptFile(std::path::PathBuf),
}

/// Ensures a child stream spawned by `OpenRouter::stream_chat` is cancelled
/// when its parent research/swarm task is aborted or otherwise dropped.
pub struct AbortOnDrop(pub tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub enum AppEvent {
    /// A one-line status update from a domain path (the 2e step converts the
    /// `status` field writes into these; the field still exists until then).
    Status(String),
    /// A survey/plan gate armed (`Some`) or cleared (`None`). `GateState`
    /// carries the session id the reply must come from, so a gate in another
    /// session can never swallow typing — the consumer compares the payload
    /// against the viewed session.
    Gate(Option<GateState>),
    Stream(Option<(ChatTaskId, StreamEvent)>),
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

    /// A per-page progress or final OCR result for one scanned PDF, or `None`
    /// when the batch's channel closed.
    Ocr(Option<(String, String, files::OcrUpdate)>),
    /// One file's chunk-embedding job finished (or the channel closed).
    Embed(Option<EmbedMsg>),
    /// A local-OCR-model pull finished: model name or error.
    OcrPull(Option<Result<String, String>>),
    /// A deep-research pipeline update, or `None` when its channel closed.
    Research(Option<ResearchMsg>),
    /// `/research` with no topic: a distilled topic from recent chat, or an error.
    ResearchTopic(Option<Result<String, String>>),
    /// Startup update check: newest published version, or `None` when the
    /// check failed (offline, index hiccup) — silently ignored.
    UpdateCheck(Option<String>),
    /// `OpenAI` Codex subscription login status or final result.
    Login(Option<LoginMsg>),
    /// A `/swarm` turn update, or `None` when its channel closed.
    Swarm(Option<swarm::SwarmMsg>),
}

// Channel/state fields share *_rx/*_tx postfixes by design — the postfix is the meaning.
// App state is inherently many booleans (modes, toggles, dirty flags).
#[allow(clippy::struct_field_names, clippy::struct_excessive_bools)]
pub struct App {
    pub db: Db,
    pub space: Space,
    /// Every backend currently logged into. `/model` merges all of their
    /// catalogs into one list; picking a model resolves back to the right
    /// one here.
    pub backends: Backends,
    /// Every credential configured on disk, kept in sync with `backends`.
    pub saved: crate::config::SavedCreds,

    /// The space the current/next session belongs to.
    pub active_space: SpaceRow,
    pub spaces_cache: Vec<SpaceRow>,
    pub space_selected: usize,
    pub space_filter: FilterInput,
    pub space_mode: SpaceMode,
    pub space_edit: String,
    /// Model used for background memory extraction (empty = disabled).
    pub memory_model: String,
    /// Model used for image transcription (empty = disabled).
    pub transcriber_model: String,
    /// Vision model for scanned-PDF OCR (empty = tesseract only).
    pub ocr_model: String,
    /// OCR engine choice: "auto" (vlm when `ocr_model` set), "tesseract",
    /// "vlm", or "local" (Ollama on 127.0.0.1:11434, set up by cycling to it in /config).
    pub ocr_engine: String,
    /// Ollama model name for the "local" OCR engine.
    pub local_ocr_model: String,
    /// Embedding model for semantic file search (empty = keyword FTS only).
    pub embedding_model: String,
    /// Model used for AI image generation (empty = disabled).
    pub image_gen_model: String,
    /// Model used for AI video generation (empty = disabled).
    pub video_gen_model: String,
    /// Base URL of a `SearXNG` instance for the web-search tool, or empty to
    /// disable it. Configured in-app (Ctrl+O settings), not a config file.
    pub searxng_url: String,
    /// `LangSearch` API key (free tier), or empty to disable it.
    pub langsearch_key: String,
    /// Which web-search backend to prefer: "auto"/"langsearch"/"searxng"/"duckduckgo".
    pub search_provider: String,
    /// Raw contents of `system_prompt.md` (with an unresolved `{{verbosity}}`
    /// placeholder) — the app's own base system prompt, `$EDITOR`-editable.
    pub base_system_prompt: String,
    /// Answer-length preference woven into the system prompt: "normal",
    /// "concise" (default), or "caveman".
    pub verbosity: String,
    pub memory_rx: Option<mpsc::UnboundedReceiver<(String, Vec<MemoryOp>)>>,
    /// Background compaction result: (session id, digest, messages-covered, pre-compaction %).
    pub compact_rx: Option<mpsc::UnboundedReceiver<(String, String, i64, u64)>>,

    /// Installed skills (name/description only — bodies are read from disk on
    /// invocation, so this list is cheap and reloaded whenever it changes).
    pub skills: Vec<crate::skills::Skill>,
    /// A skill armed by `/<skill-name>`, injected into the next message only.
    pub forced_skill: Option<String>,
    /// `/web` answer mode for the active session (or the next one created).
    pub web_mode: bool,
    pub incognito: bool,
    /// Temp directory for incognito image files, cleaned up on toggle.
    pub incognito_img_dir: Option<std::path::PathBuf>,
    /// A parked conversation's chat-reply gate (clarifying questions or an
    /// approval) — armed only while a reply is actually pending, so a gate
    /// in another session can never swallow typing.
    pub survey_gate: Option<SurveyGate>,
    /// Sender half of the reply channel into a parked gate. Created at the
    /// owning job's start; the gate itself arms/disarms as pending-section
    /// updates arrive.
    pub survey_reply_tx: Option<mpsc::UnboundedSender<String>>,
    /// Every `/steer` queued during the current job, as `(queue position,
    /// text)` — position 1-based, assigned in queue order. Entries are
    /// dropped once the pipeline acknowledges them (`research_steer_acked`),
    /// and the whole log is cleared when the job stops or its channel
    /// closes, so retained steer text stays bounded per job.
    pub research_steer_log: Vec<(usize, String)>,
    /// Steer positions (`steer #N`) the pipeline has drained and persisted —
    /// parsed from `Stage` updates in `on_research_done`, so the live popup
    /// knows what's picked up even when opened from another session, and the
    /// retained log can drop acknowledged entries.
    pub research_steer_acked: std::collections::HashSet<usize>,
    /// The running job's stage rows (`label: detail` content strings), kept
    /// in sync by `mirror_stage` regardless of which session is viewed — the
    /// live popup renders from here instead of re-reading the db per frame.
    pub research_stage_rows: Vec<String>,
    /// Incognito mode captured when the job started: artifact persistence
    /// (plan files, and the plan message itself, which folds in survey
    /// replies) is decided by this, never by toggling `incognito` mid-job.
    pub research_incognito: bool,
    /// Queues `/steer` instructions into the currently running research job's
    /// round-boundary check. `None` when no research job is running.
    pub research_steer_tx: Option<mpsc::UnboundedSender<String>>,
    /// In-progress `/research` (no args) topic distillation from recent chat.
    pub research_topic_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
    /// Composer buffer for the live research-activity view's steer input.
    pub research_live_input: String,
    pub toolbox: std::sync::Arc<dyn crate::tools::ToolExecutor>,
    /// Local static server for model-created apps (None if it failed to bind).
    pub app_server: Option<crate::appserver::AppServer>,
    pub skills_mode: SkillsMode,
    pub skills_selected: usize,
    /// GitHub `owner/repo/path` shorthand being typed in Install mode.
    pub skills_edit: String,
    pub skills_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
    /// Background OCR updates: (`space_id`, file name, progress or final result).
    pub ocr_rx: Option<mpsc::UnboundedReceiver<(String, String, files::OcrUpdate)>>,
    /// One in-flight chunk-embedding job: (space id, file id, vectors or error).
    pub embed_rx: Option<mpsc::UnboundedReceiver<EmbedMsg>>,
    /// A running local-OCR-model pull: model name on success, error text on failure.
    pub ocr_pull_rx: Option<mpsc::UnboundedReceiver<Result<String, String>>>,
    /// A running `/research` job's channel and cancellation handle.
    pub research_rx: Option<mpsc::UnboundedReceiver<ResearchMsg>>,
    pub research_abort: Option<tokio::task::AbortHandle>,
    pub login_rx: Option<mpsc::UnboundedReceiver<LoginMsg>>,
    /// Startup update check result (newest published version, or `None` on failure).
    pub update_rx: Option<mpsc::UnboundedReceiver<Option<String>>>,
    /// A running `/swarm` discussion's channel, cancellation handle, and
    /// origin session id (used for targeting the correct progress row).
    pub swarm_rx: Option<mpsc::UnboundedReceiver<swarm::SwarmMsg>>,
    pub swarm_abort: Option<tokio::task::AbortHandle>,
    pub swarm_session: Option<String>,
    /// The active session's `/swarm` roster, cached for the popup.
    pub swarm_cache: Vec<crate::db::Persona>,
    pub swarm_selected: usize,
    pub swarm_popup_mode: SwarmPopupMode,
    /// (session id, topic) of the `/research` job currently running, if any —
    /// cleared when its channel closes.
    pub research_running: Option<(String, String)>,

    /// The active space's imported files (refreshed by `rescan_files`).
    pub files_cache: Vec<crate::db::FileRow>,
    pub files_selected: usize,
    pub files_mode: FilesMode,
    pub files_tab: FilesTab,
    /// The space's apps (`/apps` popup): names, cursor, and mode.
    pub apps_cache: Vec<String>,
    pub apps_selected: usize,
    pub apps_mode: AppsMode,
    pub apps_edit: String,

    /// The `/usage` popup: aggregates snapshot + recent-list cursor.
    pub usage_data: Option<usage::UsageData>,
    pub usage_scroll: usize,
    /// Time window the `/usage` dashboard aggregates (`24h/7d/30d/all`).
    pub usage_range: crate::db::UsageRange,

    /// The space's images (`/image` popup): cache and cursor.
    pub images_cache: Vec<ImageMeta>,
    pub images_selected: usize,
    pub images_mode: ImagesMode,

    /// The space's scripts (`/script` popup): cache, cursor, and edit buffer.
    pub scripts_cache: Vec<ScriptMeta>,
    pub scripts_selected: usize,
    pub scripts_mode: ScriptsMode,
    pub scripts_edit: String,
    /// The space's standing research watches (`/watch` picker): cache + cursor.
    pub watches_cache: Vec<crate::db::Watch>,
    pub watch_selected: usize,
    pub watch_mode: WatchMode,
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
    pub models_rx: Option<mpsc::UnboundedReceiver<ModelsResult>>,
    /// Model ids marked favorite, and when each model was last used (rfc3339).
    pub favorites: HashSet<String>,
    pub last_used: HashMap<String, String>,
    /// Per-model reasoning effort (wire string from `ReasoningEffort::as_str`,
    /// e.g. "minimal" / "low" / "high" / "xhigh" / "max" / "none").
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
    /// Whether tool-call blocks show full arguments/results (Ctrl+T).
    pub show_tool_detail: bool,
    /// Wrapped-line cache for the transcript, so redraws don't re-render
    /// markdown for the whole conversation every frame.
    pub history_cache: crate::history_cache::HistoryCache,
    /// Per-session history caches preserved across session switches so
    /// switching back doesn't re-wrap every message from scratch.
    pub session_caches: std::collections::HashMap<String, crate::history_cache::HistoryCache>,
    /// External edit queued for the event loop, which owns terminal
    /// suspension and knows which app callback should consume the saved file.
    pub pending_editor: Option<PendingEditor>,
    /// Central event channel for all in-flight chat tasks.
    pub chat_event_tx: mpsc::UnboundedSender<ChatEvent>,
    pub chat_event_rx: mpsc::UnboundedReceiver<ChatEvent>,
    pub chat_tasks: HashMap<ChatTaskId, ChatTask>,
    pub next_chat_task_id: ChatTaskId,
    /// Completed task notifications, kept independently of the one-line status.
    pub notifications: VecDeque<ChatNotification>,
    /// Queued `Status`/`Gate` events, drained by `next_event` before the
    /// channel sources. The TUI/CLI/host consume the same stream either way.
    pub(crate) pending_events: VecDeque<AppEvent>,
    /// Screen rectangles for the currently rendered notification rows.
    pub notification_areas: Vec<(Rect, usize)>,
    /// Sessions holding a response that finished while the user was elsewhere.
    pub unread: std::collections::HashSet<String>,
    /// Exact conversation token total from the last completed response.
    pub context_total: Option<u64>,
    /// Cache hit rate of the most recent completed request (0..=1), shown
    /// next to the context window. Transient — not persisted here; the
    /// per-request numbers live in `usage_log`.
    pub last_cache_rate: Option<f64>,

    pub settings: Settings,
    /// Animated "thinking" indicator shown while a response streams.
    pub spinner_frame: usize,
    pub thinking_idx: usize,
    pub spinner_color: SpinnerColor,

    /// Color palette — the active omarchy theme when present, else the
    /// built-in default. `theme_link` is the last-seen omarchy symlink
    /// target, polled by the event loop to detect a theme switch.
    pub theme: Theme,
    pub theme_link: Option<std::path::PathBuf>,
    /// Bumped every time `theme` changes, so the history render cache (which
    /// bakes colors into cached `Line`s) knows to re-wrap on a theme switch.
    pub theme_gen: usize,

    pub popup: Popup,
    pub model_filter: FilterInput,
    /// Narrow the merged model list to one backend (Ctrl+P cycles it); `None` = all.
    pub model_backend_filter: Option<BackendTag>,
    pub model_focus: ModelPanel,
    /// What a confirmed selection in the model picker is currently for.
    pub model_pick_target: ModelPickTarget,
    pub fav_selected: usize,
    pub avail_selected: usize,
    /// Last-rendered scroll offset of each model-picker panel (render state
    /// stashed for click mapping; the TUI owns the ratatui `ListState`).
    pub fav_offset: usize,
    pub avail_offset: usize,
    pub sessions_cache: Vec<Session>,
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
    /// Background topic-generation result channel.
    title_rx: Option<mpsc::UnboundedReceiver<(String, String, String)>>,
    /// `/copy` menu entries and the highlighted row.
    pub copy_options: Vec<CopyOption>,
    pub copy_selected: usize,
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

    /// Highlighted row in the slash-command autocomplete popup.
    pub cmd_selected: usize,
    /// `@` file autocomplete: (matches, selected, cursor byte offset of `@`).
    pub at_state: Option<(Vec<crate::db::FileRow>, usize, usize)>,

    /// Start-screen banner (custom or built-in) and a greeting picked at launch.
    pub banner: String,
    pub greeting: &'static str,
    pub scroll: u16,
    /// Max useful `scroll` (lines above the viewport), refreshed each render so
    /// scrolling can be clamped instead of running off into empty space.
    pub max_scroll: u16,
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
    // Long by design (app bootstrap).
    #[allow(clippy::too_many_lines)]
    pub fn new(db: Db, key: Option<&str>, space: Space) -> Self {
        let provider = key.map(|k| OpenRouter::from_key_auto(k.to_string()));
        // A single bootstrap key (test convenience / a fresh single-backend
        // config): guess its flavor and seed both `backends` and `saved`
        // with it. Real app startup (main.rs) overwrites `saved` with the
        // authoritative on-disk creds right after construction and rebuilds
        // `backends` from that instead.
        let mut backends = Backends::default();
        let mut saved = crate::config::SavedCreds::default();
        if let (Some(k), Some(p)) = (&key, &provider) {
            let tag = p.backend_tag();
            backends.set(tag, p.clone());
            match tag {
                crate::provider::BackendTag::OpenRouter => {
                    saved.openrouter_key = Some((*k).to_string());
                }
                crate::provider::BackendTag::OpenAi => saved.openai_key = Some((*k).to_string()),
                crate::provider::BackendTag::OpencodeGo => {
                    saved.opencode_key = Some((*k).to_string());
                }
                // No full CodexCredentials from a bare key — fine for the
                // bootstrap/test path, main.rs always has the real ones.
                crate::provider::BackendTag::Codex => {}
            }
        }
        let status = if key.is_some() {
            "loading models…  (/model to pick, /help for commands)".to_string()
        } else {
            "no API key — set it with /key (or $OPENROUTER_API_KEY/$OPENAI_API_KEY)".to_string()
        };
        // Fall back to a fresh in-memory default row if the db lookup somehow
        // fails — the space name still resolves to real files on disk.
        let active_space = db
            .default_space_id()
            .ok()
            .and_then(|id| {
                db.list_spaces()
                    .ok()
                    .and_then(|s| s.into_iter().find(|s| s.id == id))
            })
            .unwrap_or_else(|| SpaceRow {
                id: String::new(),
                name: DEFAULT_SPACE.to_string(),
                created_at: Utc::now().to_rfc3339(),
            });
        let _ = space.ensure_space_dir(&active_space.name);
        let default_model_id = |f: fn(&OpenRouter) -> &'static str, fallback: &'static str| {
            provider.as_ref().map_or_else(
                || fallback.to_string(),
                |p| {
                    let model = f(p);
                    if model.is_empty() {
                        String::new()
                    } else {
                        format!("{}{}", p.backend_tag().key_prefix(), model)
                    }
                },
            )
        };
        let utility_model = default_model_id(
            OpenRouter::default_utility_model,
            "google/gemini-2.5-flash-lite",
        );
        let embedding_model = default_model_id(
            OpenRouter::default_embedding_model,
            "openai/text-embedding-3-small",
        );
        let image_gen_model =
            default_model_id(OpenRouter::default_image_gen_model, "openai/gpt-image-2");
        let video_gen_model =
            default_model_id(OpenRouter::default_video_gen_model, "google/veo-3.1");
        let skills_dir = crate::skills::skills_dir(&space.root);
        let skills = crate::skills::load_skills(&skills_dir);
        let (chat_event_tx, chat_event_rx) = mpsc::unbounded_channel();
        // Built with search disabled; `load_settings()` below reads the
        // persisted config (if any) and rebuilds this via `refresh_toolbox`.
        let toolbox = std::sync::Arc::new(crate::tools::ToolBox::new(
            skills_dir,
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(space.db_path()),
            Some(crate::tools::FilesCtx {
                db_path: space.db_path(),
                space_id: active_space.id.clone(),
                embedder: (!embedding_model.is_empty())
                    .then(|| backends.resolve(&embedding_model))
                    .flatten(),
            }),
            // No apps ctx yet — the app server starts after construction;
            // main() calls refresh_toolbox() once it's up.
            None,
        ));
        let mut app = Self {
            db,
            space,
            backends,
            saved,
            skills,
            searxng_url: String::new(),
            langsearch_key: String::new(),
            search_provider: "auto".to_string(),
            forced_skill: None,
            web_mode: false,
            incognito: false,
            incognito_img_dir: None,
            survey_gate: None,
            survey_reply_tx: None,
            research_steer_log: Vec::new(),
            research_steer_acked: std::collections::HashSet::new(),
            research_stage_rows: Vec::new(),
            research_incognito: false,
            research_steer_tx: None,
            research_topic_rx: None,
            research_live_input: String::new(),
            toolbox,
            app_server: None,
            skills_mode: SkillsMode::Browse,
            skills_selected: 0,
            skills_edit: String::new(),
            skills_rx: None,
            ocr_rx: None,
            embed_rx: None,
            ocr_pull_rx: None,
            research_rx: None,
            research_abort: None,
            login_rx: None,
            update_rx: None,
            swarm_rx: None,
            swarm_abort: None,
            swarm_session: None,
            swarm_cache: Vec::new(),
            swarm_selected: 0,
            swarm_popup_mode: SwarmPopupMode::Browse,
            research_running: None,
            files_cache: Vec::new(),
            files_selected: 0,
            files_mode: FilesMode::Browse,
            files_tab: FilesTab::Files,
            apps_cache: Vec::new(),
            apps_selected: 0,
            apps_mode: AppsMode::Browse,
            apps_edit: String::new(),
            usage_data: None,
            usage_scroll: 0,
            usage_range: crate::db::UsageRange::default(),
            watches_cache: Vec::new(),
            watch_selected: 0,
            watch_mode: WatchMode::Browse,
            images_cache: Vec::new(),
            images_selected: 0,
            images_mode: ImagesMode::Browse,
            scripts_cache: Vec::new(),
            scripts_selected: 0,
            scripts_mode: ScriptsMode::Browse,
            scripts_edit: String::new(),
            files_edit: String::new(),
            picker_dir: std::env::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")),
            picker_entries: Vec::new(),
            picker_filter: String::new(),
            picker_selected: 0,
            active_space,
            spaces_cache: Vec::new(),
            space_selected: 0,
            space_filter: FilterInput::default(),
            space_mode: SpaceMode::Browse,
            space_edit: String::new(),
            memory_model: utility_model.clone(),
            transcriber_model: utility_model.clone(),
            ocr_model: utility_model,
            ocr_engine: "auto".to_string(),
            local_ocr_model: "glm-ocr".to_string(),
            embedding_model,
            image_gen_model,
            video_gen_model,
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
            show_tool_detail: false,
            history_cache: crate::history_cache::HistoryCache::default(),
            session_caches: std::collections::HashMap::new(),
            pending_editor: None,
            chat_event_tx,
            chat_event_rx,
            chat_tasks: HashMap::new(),
            next_chat_task_id: 0,
            notifications: VecDeque::new(),
            pending_events: VecDeque::new(),
            notification_areas: Vec::new(),
            unread: std::collections::HashSet::new(),
            context_total: None,
            last_cache_rate: None,
            settings: Settings::default(),
            spinner_frame: 0,
            thinking_idx: 0,
            spinner_color: SpinnerColor::Green,
            theme: crate::theme::load(),
            theme_link: crate::theme::current_link_target(),
            theme_gen: 0,
            popup: Popup::None,
            model_filter: FilterInput::default(),
            model_backend_filter: None,
            model_focus: ModelPanel::Available,
            model_pick_target: ModelPickTarget::Session,
            fav_selected: 0,
            avail_selected: 0,
            fav_offset: 0,
            avail_offset: 0,
            sessions_cache: Vec::new(),
            session_selected: 0,
            session_filter: FilterInput::default(),
            session_mode: SessionMode::Browse,
            session_edit: String::new(),
            session_preview: None,
            title_rx: None,
            copy_options: Vec::new(),
            copy_selected: 0,
            key_input: String::new(),
            key_target: KeyTarget::OpenRouter,
            login_selected: 0,
            settings_selected: 0,
            settings_inputs: Default::default(),
            settings_collapsed: HashSet::new(),
            cmd_selected: 0,
            at_state: None,
            banner: config::load_banner().unwrap_or_else(|| BANNER.trim_matches('\n').to_string()),
            greeting: pick_greeting(),
            scroll: 0,
            max_scroll: 0,
            prev_total: 0,
            prev_tail: Vec::new(),
            pin_viewport_top: false,
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
            if self.backends.any() {
                self.push_status(format!("model: {id} — type a message, /model to change"));
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
            self.apply_setting(&k, &v);
        }
        self.refresh_toolbox();
    }

    /// Apply one persisted setting key to live state. Shared by
    /// `load_settings` and the `SetSetting` command.
    fn apply_setting(&mut self, k: &str, v: &str) {
        match k {
            "show_stats" => self.settings.show_stats = v == "1",
            "show_reasoning" => self.settings.show_reasoning = v == "1",
            "hide_hints" => self.settings.hide_hints = v == "1",
            "usage_range" => self.usage_range = crate::db::UsageRange::from_key(v),
            "temperature" => self.settings.temperature = v.parse().ok(),
            "top_p" => self.settings.top_p = v.parse().ok(),
            "max_tokens" => self.settings.max_tokens = v.parse().ok(),
            "memory_model" => self.memory_model = v.to_string(),
            "transcriber_model" => self.transcriber_model = v.to_string(),
            "ocr_model" => self.ocr_model = v.to_string(),
            "ocr_engine" if OCR_ENGINES.contains(&v) => self.ocr_engine = v.to_string(),
            "local_ocr_model" => self.local_ocr_model = v.to_string(),
            "embedding_model" => self.embedding_model = v.to_string(),
            // Migrate the old defaults: flux-dev is no longer in
            // OpenRouter's image catalog, and Veo Lite is the lower
            // quality tier.
            "image_gen_model" => {
                self.image_gen_model = match v {
                    "black-forest-labs/flux-dev" => "openai/gpt-image-2".to_string(),
                    _ => v.to_string(),
                }
            }
            "video_gen_model" => {
                self.video_gen_model = match v {
                    "google/veo-3.1-lite" => "google/veo-3.1".to_string(),
                    _ => v.to_string(),
                }
            }
            "compact_threshold" => {
                if let Ok(t) = v.parse() {
                    self.settings.compact_threshold = t;
                }
            }
            "searxng_url" => self.searxng_url = v.to_string(),
            "verbosity" if VERBOSITY_LEVELS.contains(&v) => self.verbosity = v.to_string(),
            "langsearch_key" => self.langsearch_key = v.to_string(),
            "search_provider" if SEARCH_PROVIDERS.contains(&v) => {
                self.search_provider = v.to_string();
            }
            _ => {}
        }
    }

    /// Rebuild the toolbox from the current `searxng_url`, so a settings
    /// change takes effect immediately (no restart). Web search tries the
    /// configured backends first and has keyless HTML fallbacks.
    pub fn refresh_toolbox(&mut self) {
        let url =
            (!self.searxng_url.trim().is_empty()).then(|| self.searxng_url.trim().to_string());
        let key = (!self.langsearch_key.trim().is_empty())
            .then(|| self.langsearch_key.trim().to_string());
        // The toolbox sits behind the `ToolExecutor` seam now, so the skills
        // dir comes from the space layout instead of off the old toolbox.
        let skills_dir = crate::skills::skills_dir(&self.space.root);
        crate::skills::install_builtin(&skills_dir);
        let mut toolbox = crate::tools::ToolBox::new(
            skills_dir.clone(),
            url,
            key,
            self.search_provider.clone(),
            self.blocked_domains(),
            Some(self.space.db_path()),
            Some(crate::tools::FilesCtx {
                db_path: self.space.db_path(),
                space_id: self.active_space.id.clone(),
                embedder: (!self.embedding_model.trim().is_empty())
                    .then(|| self.backends.resolve(self.embedding_model.trim()))
                    .flatten(),
            }),
            // App tools only exist while the server runs — an app(action=write) whose
            // link can never load is worse than no tool. Disabled in incognito.
            self.app_server
                .as_ref()
                .filter(|_| !self.incognito)
                .map(|s| crate::tools::AppsCtx {
                    dir: self.space.apps_dir(&self.active_space.name),
                    server_port: s.port(),
                    registry: s.registry().clone(),
                    space_name: self.active_space.name.clone(),
                    space_id: self.active_space.id.clone(),
                    space_db_path: self.space.db_path(),
                    files_dir: self.space.files_dir(&self.active_space.name),
                    session_id: self
                        .session
                        .as_ref()
                        .map(|s| s.id.clone())
                        .unwrap_or_default(),
                }),
        );
        if self.is_research_session()
            && let Some(session_id) = self.session.as_ref().map(|s| s.id.clone())
        {
            toolbox = toolbox.with_research_session(session_id);
        }
        toolbox.image_gen_backend = (!self.image_gen_model.trim().is_empty())
            .then(|| self.backends.resolve(self.image_gen_model.trim()))
            .flatten();
        toolbox.video_gen_backend = (!self.video_gen_model.trim().is_empty())
            .then(|| self.backends.resolve(self.video_gen_model.trim()))
            .flatten();
        toolbox.space_files_dir = self.space.files_dir(&self.active_space.name);
        toolbox.space_apps_dir = self.space.apps_dir(&self.active_space.name);
        toolbox.space_scripts_dir = self.space.scripts_dir(&self.active_space.name);
        toolbox.supports_images = self.current_model_supports_images();
        toolbox.session_id = self
            .session
            .as_ref()
            .map(|s| s.id.clone())
            .unwrap_or_default();
        self.toolbox = std::sync::Arc::new(toolbox);
        self.reload_skills();
    }

    /// Whether the active session is a research session.
    pub fn is_research_session(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.kind == "research")
    }

    /// The active space's always-excluded search domains, from its
    /// `blocked_domains.txt` (comma-separated; missing file = none).
    pub fn blocked_domains(&self) -> Vec<String> {
        std::fs::read_to_string(self.space.blocked_domains_path(&self.active_space.name))
            .unwrap_or_default()
            .split(',')
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect()
    }

    /// Kick off the initial model fetch if a key is already present. Call once
    /// after construction, from within the tokio runtime.
    pub fn init(&mut self) {
        if self.backends.any() {
            self.fetch_models();
        }
        self.rescan_files();
    }

    /// Kick off the startup update check: once per day, compare the newest
    /// published version against this build and let the user know when a
    /// newer release exists. Best-effort — offline or a failed fetch is
    /// silent. Spawned before the event loop starts; the result arrives as
    /// `AppEvent::UpdateCheck`.
    pub fn spawn_update_check(&mut self) {
        if !self.db.update_check_due() {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.update_rx = Some(rx);
        tokio::spawn(async move {
            let latest = crate::update::latest_version().await;
            let _ = tx.send(latest);
        });
    }

    /// Handle the startup update check result: notify when a newer version
    /// is published. `None` (failed check) and same/older versions are
    /// silent — the check is a courtesy, not a nag.
    pub fn on_update_check(&mut self, latest: Option<String>) {
        let Some(latest) = latest else {
            return;
        };
        if !crate::update::version_gt(&latest, crate::update::CURRENT) {
            return;
        }
        self.push_status(format!(
            "update available: v{latest} (you have {}) — run `cargo install nexus-chat`",
            crate::update::CURRENT
        ));
        self.notifications.push_back(ChatNotification {
            session_id: String::new(),
            title: "update available".to_string(),
            text: format!("v{latest} is out — you have {}", crate::update::CURRENT),
            success: true,
        });
    }

    pub fn is_streaming(&self) -> bool {
        !self.chat_tasks.is_empty()
    }

    pub fn chat_task_count(&self) -> usize {
        self.chat_tasks.len()
    }

    pub fn chat_task_for_session(&self, session_id: &str) -> Option<&ChatTask> {
        self.chat_tasks
            .values()
            .find(|task| task.session_id == session_id)
    }

    pub fn active_chat_task(&self) -> Option<&ChatTask> {
        self.session
            .as_ref()
            .and_then(|session| self.chat_task_for_session(&session.id))
    }

    pub fn active_streaming_text(&self) -> Option<&str> {
        self.active_chat_task().map(|task| task.buffer.as_str())
    }

    /// True when the in-flight stream belongs to the active session (every
    /// stream carries its origin `session_id`, so this is exact).
    pub fn viewing_stream(&self) -> bool {
        self.active_chat_task().is_some()
    }

    /// The empty start screen (banner + greeting + clock) shows when there's no
    /// conversation yet — a stream running in another session doesn't hide it.
    pub fn is_welcome(&self) -> bool {
        self.messages.is_empty() && !self.viewing_stream()
    }

    /// Advance the spinner one frame (called on the animation tick).
    pub const fn tick_spinner(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
    }

    /// Current spinner glyph.
    pub const fn spinner_char(&self) -> &'static str {
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    /// Randomly-chosen spinner colour for the current response.
    pub fn spinner_color(&self) -> SpinnerColor {
        self.active_chat_task()
            .map_or(self.spinner_color, |task| task.spinner_color)
    }

    /// Present-tense phrase for the in-progress response ("Vibing").
    pub fn thinking_phrase(&self) -> &'static str {
        self.active_chat_task()
            .map_or(THINKING[self.thinking_idx].0, |task| {
                THINKING[task.thinking_idx].0
            })
    }

    /// Reasoning tokens accumulated so far this stream, if any.
    pub fn thinking_text(&self) -> Option<&str> {
        self.active_chat_task()
            .and_then(|task| (!task.thinking.is_empty()).then_some(task.thinking.as_str()))
    }

    // --- async event sources (drained by the event loop) ---

    /// Queue a one-line status update: keeps the `status` field in sync for
    /// the TUI's direct reads (until 2e) and emits `AppEvent::Status` for
    /// the seam consumers.
    pub fn push_status(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.status.clone_from(&s);
        self.pending_events.push_back(AppEvent::Status(s));
    }

    /// Next background event from either the streaming task or a model fetch.
    /// Pends on an idle source, so it only resolves when something happens.
    pub async fn next_event(&mut self) -> AppEvent {
        // Locally-queued events (status lines, gate arming) drain first.
        if let Some(ev) = self.pending_events.pop_front() {
            return ev;
        }
        tokio::select! {
            ev = self.chat_event_rx.recv() => {
                AppEvent::Stream(ev.map(|event| (event.task_id, event.event)))
            },
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
                match self.ocr_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Ocr(r),
            r = async {
                match self.embed_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Embed(r),
            r = async {
                match self.ocr_pull_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::OcrPull(r),
            r = async {
                match self.research_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Research(r),
            r = async {
                match self.research_topic_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::ResearchTopic(r),
            r = async {
                match self.login_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Login(r),
            r = async {
                match self.update_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::UpdateCheck(r.flatten()),
            r = async {
                match self.swarm_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => AppEvent::Swarm(r),
        }
    }

    /// Fetch every configured backend's catalog concurrently and merge them
    /// into one list. A backend that fails is dropped from the merge (its
    /// error is only surfaced if *every* backend failed) — one flaky login
    /// shouldn't blank out the models of the others.
    fn fetch_models(&mut self) {
        let providers: Vec<OpenRouter> = [
            self.backends.openrouter.clone(),
            self.backends.openai.clone(),
            self.backends.opencode.clone(),
            self.backends.codex.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if providers.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.models_rx = Some(rx);
        tokio::spawn(async move {
            let mut set = tokio::task::JoinSet::new();
            for p in providers {
                set.spawn(async move { p.list_models().await });
            }
            let mut merged = Vec::new();
            let mut errors = Vec::new();
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok(models)) => merged.extend(models),
                    Ok(Err(e)) => errors.push(e.to_string()),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            let result = if merged.is_empty() && !errors.is_empty() {
                Err(errors.join("; "))
            } else {
                merged.sort_by(|a, b| a.id.cmp(&b.id));
                Ok(merged)
            };
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
                // Keep per-model catalog pricing current (OpenRouter only;
                // other backends report none). Costs are computed against
                // this table when usage is logged. Batched into a single
                // transaction — hundreds of per-model writes on the UI task
                // would stall the interface after every catalog fetch.
                let prices: Vec<_> = models
                    .iter()
                    .filter_map(|m| {
                        m.pricing
                            .map(|price| (m.id.clone(), m.backend.name().to_string(), price))
                    })
                    .collect();
                let _ = self.db.upsert_model_prices(&prices);
                // Fresh catalog in hand — recompute every logged request's
                // cost against it (fills rows logged before pricing existed
                // and heals any legacy per-token-shaped catalog).
                let _ = self.db.backfill_usage_costs();
                self.models = models;
                self.push_status(format!("loaded {n} models"));
                // First key just landed and nothing picked yet → jump into the picker.
                if !self.models.is_empty()
                    && self.current_model.is_none()
                    && self.popup == Popup::None
                {
                    self.open_model_picker();
                }
            }
            Err(e) => self.push_status(format!("model fetch failed: {e}")),
        }
    }

    // --- input handling ---
    // --- nerd config (settings popup) ---

    /// Index into `settings_inputs` for any typed (non-toggle, non-picker) field.
    /// `None` both for non-text fields and when a group header is selected.
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

    /// Whether scanned PDFs should OCR through the `OpenRouter` vision model:
    /// explicit "vlm", or "auto" with an OCR model configured. ("local" and
    /// "tesseract" route elsewhere.)
    pub fn vlm_ocr_enabled(&self) -> bool {
        !self.ocr_model.trim().is_empty() && matches!(self.ocr_engine.as_str(), "vlm" | "auto")
    }
}

impl App {
    /// Abort every interactive chat task before the TUI exits. Chat streams are
    /// intentionally not persisted or resumed across process restarts.
    pub fn cancel_chat_tasks(&mut self) {
        for task in self.chat_tasks.values() {
            task.abort.abort();
        }
        self.chat_tasks.clear();
    }
}

/// Best-effort transient Linux desktop notification. The TUI notification
/// remains the actionable one because `notify-send` cannot route a click back
/// into this running process without a long-lived D-Bus action listener.
pub fn send_system_notification(title: &str, body: &str) {
    #[cfg(all(target_os = "linux", not(test)))]
    {
        let _ = std::process::Command::new("notify-send")
            .args([
                "--app-name=nexus-chat",
                "--urgency=normal",
                "--expire-time=5000",
                "--hint=int:transient:1",
                title,
                body,
            ])
            .spawn();
    }
    #[cfg(any(not(target_os = "linux"), test))]
    let _ = (title, body);
}

// Long by design (tool-summary table).
#[allow(clippy::too_many_lines)]
/// The one-line transcript summary for a tool-call block: the tool's name
/// plus the argument (and result shape) a reader actually cares about.
pub fn tool_call_summary(name: &str, args: &str, result: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let f = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let f_or = |first: &str, second: &str| {
        let value = f(first);
        if value.is_empty() { f(second) } else { value }
    };
    match name {
        "skills" => {
            let target = if f("action") == "install" {
                f("source")
            } else {
                f("name")
            };
            format!("skills/{} {}", f("action"), target)
        }
        "scripts" => {
            let target = match f("action").as_str() {
                "install" => {
                    let n = v
                        .get("packages")
                        .and_then(|a| a.as_array())
                        .map_or(0, Vec::len);
                    format!("{n} packages")
                }
                "python" => f("name"),
                _ => f("path"),
            };
            format!("scripts/{} {}", f("action"), target)
        }
        "app" => format!("app/{} {}", f("action"), f("app")),
        "media" => {
            let target = [f("video_id"), f("name"), f("image_id")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_else(|| f("prompt").chars().take(30).collect());
            format!("media/{} {}", f("action"), target)
        }
        "skill" => format!("skill {}", f("name")),
        "skill_admin" => format!("skill_admin {} → {}", f("action"), first_line(result)),
        "search" => {
            let failed = result.starts_with("no results") || result.contains("failed");
            let hits = if failed { "no hits" } else { "hits" };
            format!("search/{} \"{}\" → {hits}", f("mode"), f("query"))
        }
        "research_lookup" => format!("research_lookup/{} \"{}\"", f("scope"), f("query")),
        "batch" => {
            let calls = v.get("calls").and_then(|c| c.as_array());
            let n = calls.map_or(0, Vec::len);
            let tools: Vec<&str> = calls
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.get("tool").and_then(|t| t.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            format!("batch [{n} ops: {}]", tools.join(", "))
        }
        "fetch_url" => format!("fetch_url {} → {}", f("url"), first_line(result)),
        "files" => format!(
            "files/{} {} → {}",
            f("action"),
            f_or("name", "query"),
            first_line(result)
        ),
        "app_inspect" => format!(
            "app_inspect/{} {}/{}",
            f("action"),
            f("app"),
            f_or("path", "pattern")
        ),
        "app_modify" => format!("app_modify/{} {}/{}", f("action"), f("app"), f("path")),
        "app_assets" => format!("app_assets/{} {}", f("action"), f("app")),
        "script_files" => format!("script_files/{} {}", f("action"), f("path")),
        "video_transform" => format!("video_transform/{} {}", f("action"), f("video_id")),
        "video_references" => format!("video_references/{} {}", f("action"), f("name")),
        "install_skill" => format!("install_skill {} → {}", f("source"), first_line(result)),
        "create_skill" => format!("create_skill {} → {}", f("name"), first_line(result)),
        "run_script" => {
            let path = f_or("path", "script");
            if v.get("space")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                format!("run_script space/{path}")
            } else {
                format!("run_script {}/{}", f("skill"), path)
            }
        }
        "run_python" => format!("run_python ({} lines)", f("code").lines().count().max(1)),
        "grep_app" => {
            let hits = if result.starts_with("no matches") {
                "no hits".to_string()
            } else {
                format!(
                    "{} files",
                    result.lines().filter(|line| !line.starts_with('…')).count()
                )
            };
            format!("grep_app {} \"{}\" → {hits}", f("app"), f("pattern"))
        }
        "install_packages" => {
            let pkgs = v
                .get("packages")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let target = [f("skill"), f("app")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            format!("install_packages {pkgs} → {target}")
        }
        "web_search" | "search_files" => {
            let failed = result.starts_with("no results")
                || result.starts_with("no matches")
                || result.contains("failed");
            let hits = if failed {
                "no hits".to_string()
            } else {
                format!("{} hits", result.lines().count())
            };
            format!("{name} \"{}\" → {hits}", f("query"))
        }
        "read_file" => format!("read_file {} → {}", f("name"), first_line(result)),
        "read_app_file" => format!("read_app_file {}/{}", f("app"), f("path")),
        "diff_app" => format!("diff_app {}/{}", f("app"), f("path")),
        "write_file" => {
            format!(
                "write_file {}/{} ({} bytes)",
                f("app"),
                f("path"),
                f("content").len()
            )
        }
        "edit_file" => format!("edit_file {}/{}", f("app"), f("path")),
        "generate_video" => format!("generate_video \"{}\"", f("prompt")),
        "edit_video" => format!("edit_video {} {}", f("video_id"), f("lighting")),
        "extract_frame" => format!("extract_frame {} @ {:.1}s", f("video_id"), f("time_sec")),
        "stitch_videos" => format!("stitch_videos {}", f("video_ids")),
        "save_reference" => format!("save_reference {}", f("name")),
        "list_references" => "list_references".to_string(),
        "delete_reference" => format!("delete_reference {}", f("name")),
        "generate_image" => format!("generate_image \"{}\"", f("prompt")),
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
    result
        .lines()
        .next()
        .unwrap_or("")
        .trim_end_matches(':')
        .to_string()
}
