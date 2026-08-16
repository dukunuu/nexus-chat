//! Wire types for the Phase 4 `nexus host` `HTTP`/`SSE` API.
//!
//! The daemon never leaks domain internals onto the wire: [`AppEvent`]
//! nests provider ([`StreamEvent`]), research, swarm, and file types that
//! carry no serde and are free to change shape as the domain evolves.
//! Everything the host emits is instead this module's [`WireEvent`] — a
//! serde mirror of the domain event with a stable `JSON` shape. `From`
//! impls convert in one direction only (domain → wire); the host never
//! parses events back into domain types.
//!
//! [`AppCommand`](crate::app::AppCommand) is the one exception: it is a
//! plain enum of strings, bools, and optionals with no internals, so the
//! command seam itself carries the serde derives and `POST /v1/command`
//! ships it directly.

use serde::{Deserialize, Serialize};

use crate::app::{
    AppEvent, GateState, LoginMsg, MemoryOp, OcrUpdate, PlanQuestion, ResearchUpdate, SurveyPhase,
    SwarmUpdate,
};
use crate::config::CodexCredentials;
use crate::db::Persona;
use crate::provider::{BackendTag, Model, ModelPricing, ReasoningEffort, StreamEvent, Usage};

/// One event on the host's `/v1/events` `SSE` feed — the wire mirror of
/// [`AppEvent`], with every variant mapping 1:1 onto the domain event.
/// Frames serialize as `{"type": "<snake_case variant>", "payload": …}`;
/// unit variants carry no `payload` key. A `None` payload means that
/// source's channel closed (its background task ended).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum WireEvent {
    /// A one-line status update from a domain path.
    Status(String),
    /// The composer should be replaced with this text (e.g. a send-failure
    /// path restoring the user's message).
    ComposerSet(String),
    /// The composer should be cleared.
    ComposerClear,
    /// The view should reset its viewport state (scroll, selection,
    /// pinning baseline) — pushed wherever domain code switches sessions,
    /// starts a stream, or otherwise invalidates the rendered conversation.
    ViewportReset,
    /// The wrapped-history render cache must be rebuilt (in-place message
    /// edits would otherwise leave stale wrapped content).
    HistoryInvalidated,
    /// A domain path fell back to "no backend configured" and wants the
    /// login selector shown.
    OpenLoginPopup,
    /// A survey/plan gate armed (`Some`) or cleared (`None`).
    Gate(Option<WireGateState>),
    /// One chat-frame delta: (task id, stream event) — or `None` when the
    /// task's channel closed. Carries every frame the stream view renders,
    /// so the `SSE` bridge needs no separate chat pump.
    Stream(Option<(u64, WireStreamEvent)>),
    /// The model-catalog fetch outcome: the merged per-backend list, or an
    /// error string.
    Models(Option<WireModelsResult>),
    /// A generated session topic: (session id, title, slug).
    Title(Option<(String, String, String)>),
    /// Extracted memory ops for a space, tagged with the space name so a
    /// meanwhile space-switch can discard stale results.
    Memory(Option<(String, Vec<WireMemoryOp>)>),
    /// A compaction digest: (session id, digest, messages covered,
    /// pre-compaction %).
    Compact(Option<(String, String, i64, u64)>),
    /// `/skills` install outcome: skill name on success, error message on
    /// failure.
    SkillInstall(Option<Result<String, String>>),
    /// A per-page progress or final `OCR` result for one scanned `PDF`, or
    /// `None` when the batch's channel closed.
    Ocr(Option<(String, String, WireOcrUpdate)>),
    /// One file's chunk-embedding job finished (or the channel closed).
    Embed(Option<WireEmbedMsg>),
    /// A local-`OCR`-model pull finished: model name or error.
    OcrPull(Option<Result<String, String>>),
    /// A deep-research pipeline update, or `None` when its channel closed.
    Research(Option<WireResearchMsg>),
    /// `/research` with no topic: a distilled topic from recent chat, or an
    /// error.
    ResearchTopic(Option<Result<String, String>>),
    /// Startup update check: newest published version, or `None` when the
    /// check failed (offline, index hiccup) — silently ignored by clients.
    UpdateCheck(Option<String>),
    /// Codex subscription login status or final result.
    Login(Option<WireLoginMsg>),
    /// A `/swarm` turn update, or `None` when its channel closed.
    Swarm(Option<WireSwarmMsg>),
}

/// The wire mirror of provider [`StreamEvent`] — one chat-frame delta.
/// Token/reasoning/status are incremental chunks; `ToolCall` is a completed
/// tool run with its result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireStreamEvent {
    /// A chunk of the visible answer.
    Token(String),
    /// A chunk of the model's reasoning/thinking.
    Reasoning(String),
    /// Exact token counts (arrives near end of stream when usage accounting
    /// is on).
    Usage(WireUsage),
    /// A tool is about to run (e.g. "Searching the web…").
    Status(String),
    /// A tool finished: shown (and persisted) as its own transcript block.
    ToolCall {
        name: String,
        arguments: String,
        result: String,
    },
    /// The stream finished cleanly.
    Done,
    /// The stream failed.
    Error(String),
}

/// The wire mirror of provider [`Usage`] — exact token accounting reported
/// at end of stream.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WireUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Prompt tokens served from the provider's prompt cache (cache reads).
    pub cache_read_tokens: u64,
    /// Prompt tokens written into the cache on this request (cache writes).
    pub cache_creation_tokens: u64,
    /// Provider-reported request cost in `USD`; `None` when the provider
    /// omits cost.
    pub cost: Option<f64>,
}

/// The wire mirror of [`GateState`] — which session a parked survey/plan
/// gate is waiting on, and which phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireGateState {
    /// The session the reply must come from.
    pub session_id: String,
    /// Which phase is waiting (drives the prompt shown).
    pub phase: WireSurveyPhase,
}

/// What a parked survey gate is waiting for — drives which phase's reply is
/// routed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireSurveyPhase {
    /// A clarifying-question round (1-based).
    Clarify { round: u8 },
    /// Approval of a presented artifact; `rework` is true on a
    /// re-presentation after the user's edits were folded in.
    Approve { rework: bool },
}

/// One memory-extraction op, as emitted by the memory model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireMemoryOp {
    Add(String),
    Update(usize, String),
    Delete(usize),
}

/// A message from the background `OCR` batch about one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireOcrUpdate {
    /// A human-readable phase ("rendering pages…") shown while nothing is
    /// countable yet.
    Stage(String),
    /// (pages done, total pages, pages failed so far).
    Progress(usize, usize, usize),
    /// Final outcome: (extracted text, per-page errors as (index, reason)),
    /// or a whole-document error message.
    Done(Result<(String, Vec<(usize, String)>), String>),
}

/// A deep-research pipeline update: one stage tick, a parked gate, or the
/// final result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireResearchUpdate {
    /// Successive updates within one stage share a `label` so the client
    /// replaces one row in place instead of appending per tick.
    Stage { label: String, detail: String },
    /// The scoping agent's clarifying questions; the pipeline is parked
    /// awaiting a chat reply. `round` is 1-based.
    SurveyReady { questions: Vec<String>, round: u8 },
    /// The planner finished; the pipeline is parked awaiting a chat reply.
    /// `rework` is true on a re-presentation after the user's edits were
    /// folded in.
    PlanReady {
        questions: Vec<WirePlanQuestion>,
        rework: bool,
    },
    /// Final outcome: the report, or a whole-pipeline error.
    Done(Result<String, String>),
}

/// One planner sub-question handed to a searcher agent as its prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WirePlanQuestion {
    pub question: String,
    pub why: String,
    pub angles: Vec<String>,
    pub sources: Vec<String>,
}

/// Codex subscription login status or final result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireLoginMsg {
    Status(String),
    Done(Result<WireCodexCredentials, String>),
}

/// The wire mirror of [`CodexCredentials`] — a finished Codex login.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCodexCredentials {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
    pub account_id: String,
}

/// A `/swarm` turn update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireSwarmUpdate {
    /// The roster was empty, so one was suggested — persist it before the
    /// conversation's first turn.
    RosterSuggested(Vec<WirePersona>),
    /// Status-line progress text.
    Progress(String),
    /// One persona's reply for the turn just run.
    Reply {
        persona: String,
        model: String,
        content: String,
    },
    /// The moderator decided the conversation needed a new voice — persist
    /// it to the roster so it shows up in the popup too.
    PersonaJoined(WirePersona),
    /// The turn's final synthesis reply — the canonical assistant message.
    Synthesis(String),
    Error(String),
}

/// One row of a session's `/swarm` roster: a model + a personality blurb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WirePersona {
    pub name: String,
    pub model: String,
    pub blurb: String,
}

/// The wire mirror of provider [`Model`] — one routable model from the
/// merged per-backend catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireModel {
    /// Composite id (backend prefix + wire id) — what `current_model`,
    /// favorites, and last-used store.
    pub id: String,
    pub name: String,
    /// The reasoning-effort values this model accepts, in cycle order.
    /// Empty = the model has no reasoning/thinking mode at all.
    pub reasoning_efforts: Vec<WireReasoningEffort>,
    /// Context window size in tokens, if the provider reports it.
    pub context_length: Option<u64>,
    /// Whether the model accepts image input.
    pub supports_images: bool,
    /// Whether the model generates image output.
    pub supports_image_generation: bool,
    /// Whether the model generates video output.
    pub supports_video_generation: bool,
    /// Which backend this model came from — the gateway's routing key.
    pub backend: WireBackendTag,
    /// `USD` per 1M tokens from the catalog; `None` = cost unknown.
    pub pricing: Option<WireModelPricing>,
}

/// The reasoning-effort values a model accepts, in `Ctrl`+`T` cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// Which backend a model routes through — the wire mirror of
/// [`BackendTag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireBackendTag {
    OpenRouter,
    OpenAi,
    OpencodeGo,
    Codex,
}

/// `USD` per 1M tokens from the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WireModelPricing {
    pub prompt: f64,
    pub completion: f64,
    /// Discounted cache-read price; `None` = cache reads use the regular
    /// prompt price.
    pub cache_read: Option<f64>,
    /// Cache-write price; `None` = writes use the regular prompt price.
    pub cache_write: Option<f64>,
}

/// The wire mirror of `app::ModelsResult` — the catalog fetch outcome.
pub type WireModelsResult = Result<Vec<WireModel>, String>;

/// The wire mirror of `app::EmbedMsg`: (space id, file id, (seq, vector)
/// pairs or error). Plain data, so it passes through unmapped.
pub type WireEmbedMsg = (String, String, Result<Vec<(i64, Vec<f32>)>, String>);

/// The wire mirror of `app::ResearchMsg`: (session id, space id, space
/// name, stage update or final result).
pub type WireResearchMsg = (String, String, String, WireResearchUpdate);

/// The wire mirror of `app::SwarmMsg`: (session id, update).
pub type WireSwarmMsg = (String, WireSwarmUpdate);

impl From<AppEvent> for WireEvent {
    fn from(ev: AppEvent) -> Self {
        match ev {
            AppEvent::Status(s) => Self::Status(s),
            AppEvent::ComposerSet(s) => Self::ComposerSet(s),
            AppEvent::ComposerClear => Self::ComposerClear,
            AppEvent::ViewportReset => Self::ViewportReset,
            AppEvent::HistoryInvalidated => Self::HistoryInvalidated,
            AppEvent::OpenLoginPopup => Self::OpenLoginPopup,
            AppEvent::Gate(g) => Self::Gate(g.map(WireGateState::from)),
            AppEvent::Stream(s) => {
                Self::Stream(s.map(|(task_id, event)| (task_id, WireStreamEvent::from(event))))
            }
            AppEvent::Models(m) => Self::Models(
                m.map(|r| r.map(|models| models.into_iter().map(WireModel::from).collect())),
            ),
            AppEvent::Title(t) => Self::Title(t),
            AppEvent::Memory(m) => Self::Memory(
                m.map(|(space, ops)| (space, ops.into_iter().map(WireMemoryOp::from).collect())),
            ),
            AppEvent::Compact(c) => Self::Compact(c),
            AppEvent::SkillInstall(s) => Self::SkillInstall(s),
            AppEvent::Ocr(o) => Self::Ocr(
                o.map(|(file, session, update)| (file, session, WireOcrUpdate::from(update))),
            ),
            AppEvent::Embed(e) => Self::Embed(e),
            AppEvent::OcrPull(p) => Self::OcrPull(p),
            AppEvent::Research(r) => {
                Self::Research(r.map(|(session_id, space_id, space_name, update)| {
                    (
                        session_id,
                        space_id,
                        space_name,
                        WireResearchUpdate::from(update),
                    )
                }))
            }
            AppEvent::ResearchTopic(t) => Self::ResearchTopic(t),
            AppEvent::UpdateCheck(u) => Self::UpdateCheck(u),
            AppEvent::Login(l) => Self::Login(l.map(WireLoginMsg::from)),
            AppEvent::Swarm(s) => Self::Swarm(
                s.map(|(session_id, update)| (session_id, WireSwarmUpdate::from(update))),
            ),
        }
    }
}

impl From<StreamEvent> for WireStreamEvent {
    fn from(ev: StreamEvent) -> Self {
        match ev {
            StreamEvent::Token(t) => Self::Token(t),
            StreamEvent::Reasoning(r) => Self::Reasoning(r),
            StreamEvent::Usage(u) => Self::Usage(u.into()),
            StreamEvent::Status(s) => Self::Status(s),
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
            } => Self::ToolCall {
                name,
                arguments,
                result,
            },
            StreamEvent::Done => Self::Done,
            StreamEvent::Error(e) => Self::Error(e),
        }
    }
}

impl From<Usage> for WireUsage {
    fn from(u: Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_creation_tokens: u.cache_creation_tokens,
            cost: u.cost,
        }
    }
}

impl From<GateState> for WireGateState {
    fn from(g: GateState) -> Self {
        Self {
            session_id: g.session_id,
            phase: g.phase.into(),
        }
    }
}

impl From<SurveyPhase> for WireSurveyPhase {
    fn from(p: SurveyPhase) -> Self {
        match p {
            SurveyPhase::Clarify { round } => Self::Clarify { round },
            SurveyPhase::Approve { rework } => Self::Approve { rework },
        }
    }
}

impl From<MemoryOp> for WireMemoryOp {
    fn from(op: MemoryOp) -> Self {
        match op {
            MemoryOp::Add(s) => Self::Add(s),
            MemoryOp::Update(i, s) => Self::Update(i, s),
            MemoryOp::Delete(i) => Self::Delete(i),
        }
    }
}

impl From<OcrUpdate> for WireOcrUpdate {
    fn from(u: OcrUpdate) -> Self {
        match u {
            OcrUpdate::Stage(s) => Self::Stage(s),
            OcrUpdate::Progress(done, total, failed) => Self::Progress(done, total, failed),
            OcrUpdate::Done(d) => Self::Done(d),
        }
    }
}

impl From<ResearchUpdate> for WireResearchUpdate {
    fn from(u: ResearchUpdate) -> Self {
        match u {
            ResearchUpdate::Stage { label, detail } => Self::Stage { label, detail },
            ResearchUpdate::SurveyReady { questions, round } => {
                Self::SurveyReady { questions, round }
            }
            ResearchUpdate::PlanReady { questions, rework } => Self::PlanReady {
                questions: questions.into_iter().map(WirePlanQuestion::from).collect(),
                rework,
            },
            ResearchUpdate::Done(d) => Self::Done(d),
        }
    }
}

impl From<PlanQuestion> for WirePlanQuestion {
    fn from(q: PlanQuestion) -> Self {
        Self {
            question: q.question,
            why: q.why,
            angles: q.angles,
            sources: q.sources,
        }
    }
}

impl From<LoginMsg> for WireLoginMsg {
    fn from(m: LoginMsg) -> Self {
        match m {
            LoginMsg::Status(s) => Self::Status(s),
            LoginMsg::Done(d) => Self::Done(d.map(WireCodexCredentials::from)),
        }
    }
}

impl From<CodexCredentials> for WireCodexCredentials {
    fn from(c: CodexCredentials) -> Self {
        Self {
            access: c.access,
            refresh: c.refresh,
            expires: c.expires,
            account_id: c.account_id,
        }
    }
}

impl From<SwarmUpdate> for WireSwarmUpdate {
    fn from(u: SwarmUpdate) -> Self {
        match u {
            SwarmUpdate::RosterSuggested(p) => {
                Self::RosterSuggested(p.into_iter().map(WirePersona::from).collect())
            }
            SwarmUpdate::Progress(s) => Self::Progress(s),
            SwarmUpdate::Reply {
                persona,
                model,
                content,
            } => Self::Reply {
                persona,
                model,
                content,
            },
            SwarmUpdate::PersonaJoined(p) => Self::PersonaJoined(p.into()),
            SwarmUpdate::Synthesis(s) => Self::Synthesis(s),
            SwarmUpdate::Error(e) => Self::Error(e),
        }
    }
}

impl From<Persona> for WirePersona {
    fn from(p: Persona) -> Self {
        Self {
            name: p.name,
            model: p.model,
            blurb: p.blurb,
        }
    }
}

impl From<Model> for WireModel {
    fn from(m: Model) -> Self {
        Self {
            id: m.id,
            name: m.name,
            reasoning_efforts: m
                .reasoning_efforts
                .into_iter()
                .map(WireReasoningEffort::from)
                .collect(),
            context_length: m.context_length,
            supports_images: m.supports_images,
            supports_image_generation: m.supports_image_generation,
            supports_video_generation: m.supports_video_generation,
            backend: m.backend.into(),
            pricing: m.pricing.map(WireModelPricing::from),
        }
    }
}

impl From<ReasoningEffort> for WireReasoningEffort {
    fn from(e: ReasoningEffort) -> Self {
        match e {
            ReasoningEffort::None => Self::None,
            ReasoningEffort::Minimal => Self::Minimal,
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
            ReasoningEffort::XHigh => Self::XHigh,
            ReasoningEffort::Max => Self::Max,
        }
    }
}

impl From<BackendTag> for WireBackendTag {
    fn from(t: BackendTag) -> Self {
        match t {
            BackendTag::OpenRouter => Self::OpenRouter,
            BackendTag::OpenAi => Self::OpenAi,
            BackendTag::OpencodeGo => Self::OpencodeGo,
            BackendTag::Codex => Self::Codex,
        }
    }
}

impl From<ModelPricing> for WireModelPricing {
    fn from(p: ModelPricing) -> Self {
        Self {
            prompt: p.prompt,
            completion: p.completion,
            cache_read: p.cache_read,
            cache_write: p.cache_write,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire model used across the round-trip and `From` tests: one
    /// OpenRouter model with pricing and reasoning efforts.
    fn wire_model() -> WireModel {
        WireModel {
            id: "openrouter:anthropic/claude-sonnet-4".into(),
            name: "Claude Sonnet 4".into(),
            reasoning_efforts: vec![WireReasoningEffort::None, WireReasoningEffort::High],
            context_length: Some(200_000),
            supports_images: true,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: WireBackendTag::OpenRouter,
            pricing: Some(WireModelPricing {
                prompt: 3.0,
                completion: 15.0,
                cache_read: Some(0.3),
                cache_write: Some(3.0),
            }),
        }
    }

    /// One `WireEvent` per variant (plus `Some`/`None` for the optional
    /// payloads) must survive a `serde_json` round-trip unchanged.
    #[test]
    fn round_trips_every_wire_event_variant() {
        let events = vec![
            WireEvent::Status("ready".into()),
            WireEvent::ComposerSet("hi".into()),
            WireEvent::ComposerClear,
            WireEvent::ViewportReset,
            WireEvent::HistoryInvalidated,
            WireEvent::OpenLoginPopup,
            WireEvent::Gate(Some(WireGateState {
                session_id: "s1".into(),
                phase: WireSurveyPhase::Clarify { round: 1 },
            })),
            WireEvent::Gate(None),
            WireEvent::Stream(Some((
                7,
                WireStreamEvent::ToolCall {
                    name: "python".into(),
                    arguments: "print(1)".into(),
                    result: "1\n".into(),
                },
            ))),
            WireEvent::Stream(None),
            WireEvent::Models(Some(Ok(vec![wire_model()]))),
            WireEvent::Models(Some(Err("no backend configured".into()))),
            WireEvent::Models(None),
            WireEvent::Title(Some(("s1".into(), "Hello".into(), "hello".into()))),
            WireEvent::Memory(Some((
                "sp1".into(),
                vec![
                    WireMemoryOp::Add("alpha".into()),
                    WireMemoryOp::Update(2, "beta".into()),
                    WireMemoryOp::Delete(3),
                ],
            ))),
            WireEvent::Compact(Some(("s1".into(), "digest".into(), 12, 40))),
            WireEvent::SkillInstall(Some(Ok("python".into()))),
            WireEvent::SkillInstall(Some(Err("no model".into()))),
            WireEvent::Ocr(Some((
                "f1".into(),
                "s1".into(),
                WireOcrUpdate::Progress(1, 3, 0),
            ))),
            WireEvent::Embed(Some((
                "s1".into(),
                "f1".into(),
                Ok(vec![(0, vec![0.1, 0.2])]),
            ))),
            WireEvent::OcrPull(Some(Err("pull failed".into()))),
            WireEvent::Research(Some((
                "s1".into(),
                "sp1".into(),
                "Space".into(),
                WireResearchUpdate::PlanReady {
                    questions: vec![WirePlanQuestion {
                        question: "q1".into(),
                        why: "w1".into(),
                        angles: vec!["a1".into()],
                        sources: vec!["s1".into()],
                    }],
                    rework: true,
                },
            ))),
            WireEvent::ResearchTopic(Some(Ok("topic".into()))),
            WireEvent::UpdateCheck(Some("0.2.0".into())),
            WireEvent::Login(Some(WireLoginMsg::Done(Ok(WireCodexCredentials {
                access: "a".into(),
                refresh: "r".into(),
                expires: 123,
                account_id: "acc".into(),
            })))),
            WireEvent::Swarm(Some((
                "s1".into(),
                WireSwarmUpdate::RosterSuggested(vec![WirePersona {
                    name: "ada".into(),
                    model: "m1".into(),
                    blurb: "b".into(),
                }]),
            ))),
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).expect("serializes");
            let back: WireEvent = serde_json::from_str(&json).expect("parses");
            assert_eq!(back, ev, "round-trip failed for {json}");
        }
    }

    /// A golden frame locks the `SSE` wire shape for Phase 5 clients: the
    /// adjacently-tagged envelope, snake_case type names, and the exact
    /// payload nesting.
    #[test]
    fn golden_wire_event_json() {
        let ev = WireEvent::Stream(Some((
            7,
            WireStreamEvent::ToolCall {
                name: "python".into(),
                arguments: "print(1)".into(),
                result: "1\n".into(),
            },
        )));
        let json = serde_json::to_string(&ev).expect("serializes");
        assert_eq!(
            json,
            r#"{"type":"stream","payload":[7,{"ToolCall":{"name":"python","arguments":"print(1)","result":"1\n"}}]}"#
        );
        assert_eq!(
            serde_json::to_string(&WireEvent::ComposerClear).expect("serializes"),
            r#"{"type":"composer_clear"}"#
        );
        assert_eq!(
            serde_json::to_string(&WireEvent::Gate(None)).expect("serializes"),
            r#"{"type":"gate","payload":null}"#
        );
    }

    /// The plain variants map across unchanged; the optional ones carry
    /// `None` straight through.
    #[test]
    fn from_app_event_maps_plain_variants() {
        let pairs = [
            (AppEvent::Status("s".into()), WireEvent::Status("s".into())),
            (
                AppEvent::ComposerSet("c".into()),
                WireEvent::ComposerSet("c".into()),
            ),
            (AppEvent::ComposerClear, WireEvent::ComposerClear),
            (AppEvent::ViewportReset, WireEvent::ViewportReset),
            (AppEvent::HistoryInvalidated, WireEvent::HistoryInvalidated),
            (AppEvent::OpenLoginPopup, WireEvent::OpenLoginPopup),
            (
                AppEvent::Title(Some(("s1".into(), "Hello".into(), "hello".into()))),
                WireEvent::Title(Some(("s1".into(), "Hello".into(), "hello".into()))),
            ),
            (
                AppEvent::Compact(Some(("s1".into(), "digest".into(), 12, 40))),
                WireEvent::Compact(Some(("s1".into(), "digest".into(), 12, 40))),
            ),
            (
                AppEvent::SkillInstall(Some(Err("no model".into()))),
                WireEvent::SkillInstall(Some(Err("no model".into()))),
            ),
            (
                AppEvent::OcrPull(Some(Ok("glm-ocr".into()))),
                WireEvent::OcrPull(Some(Ok("glm-ocr".into()))),
            ),
            (
                AppEvent::ResearchTopic(Some(Err("offline".into()))),
                WireEvent::ResearchTopic(Some(Err("offline".into()))),
            ),
            (
                AppEvent::UpdateCheck(Some("0.2.0".into())),
                WireEvent::UpdateCheck(Some("0.2.0".into())),
            ),
        ];
        for (app, wire) in pairs {
            assert_eq!(WireEvent::from(app), wire);
        }
    }

    /// A closed source channel arrives as `AppEvent` with a `None` payload
    /// and must stay `None` on the wire.
    #[test]
    fn from_app_event_none_means_channel_closed() {
        for ev in [
            AppEvent::Gate(None),
            AppEvent::Stream(None),
            AppEvent::Models(None),
            AppEvent::Title(None),
            AppEvent::Memory(None),
            AppEvent::Compact(None),
            AppEvent::SkillInstall(None),
            AppEvent::Ocr(None),
            AppEvent::Embed(None),
            AppEvent::OcrPull(None),
            AppEvent::Research(None),
            AppEvent::ResearchTopic(None),
            AppEvent::UpdateCheck(None),
            AppEvent::Login(None),
            AppEvent::Swarm(None),
        ] {
            let wire = WireEvent::from(ev);
            let json = serde_json::to_string(&wire).expect("serializes");
            assert!(
                json.contains("\"payload\":null"),
                "expected null payload, got {json}"
            );
        }
    }

    /// The chat-frame carrier: `AppEvent::Stream` maps task id and event
    /// through every `StreamEvent` shape, including the tool-call delta the
    /// stream view renders as its own block.
    #[test]
    fn from_app_event_maps_stream_frames() {
        let frames = [
            (
                StreamEvent::Token("hi".into()),
                WireStreamEvent::Token("hi".into()),
            ),
            (
                StreamEvent::Reasoning("think".into()),
                WireStreamEvent::Reasoning("think".into()),
            ),
            (
                StreamEvent::Usage(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cache_read_tokens: 2,
                    cache_creation_tokens: 1,
                    cost: Some(0.0012),
                }),
                WireStreamEvent::Usage(WireUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    cache_read_tokens: 2,
                    cache_creation_tokens: 1,
                    cost: Some(0.0012),
                }),
            ),
            (
                StreamEvent::Status("running python…".into()),
                WireStreamEvent::Status("running python…".into()),
            ),
            (
                StreamEvent::ToolCall {
                    name: "python".into(),
                    arguments: "print(1)".into(),
                    result: "1".into(),
                },
                WireStreamEvent::ToolCall {
                    name: "python".into(),
                    arguments: "print(1)".into(),
                    result: "1".into(),
                },
            ),
            (StreamEvent::Done, WireStreamEvent::Done),
            (
                StreamEvent::Error("boom".into()),
                WireStreamEvent::Error("boom".into()),
            ),
        ];
        for (event, wire) in frames {
            let app = AppEvent::Stream(Some((3, event)));
            assert_eq!(WireEvent::from(app), WireEvent::Stream(Some((3, wire))));
        }
    }

    /// Gate and model-catalog payloads map their nested types (survey
    /// phase, reasoning efforts, pricing, backend tag) through.
    #[test]
    fn from_app_event_maps_gate_and_models() {
        let app = AppEvent::Gate(Some(GateState {
            session_id: "s1".into(),
            phase: SurveyPhase::Approve { rework: true },
        }));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Gate(Some(WireGateState {
                session_id: "s1".into(),
                phase: WireSurveyPhase::Approve { rework: true },
            }))
        );

        let model = Model {
            id: "openrouter:anthropic/claude-sonnet-4".into(),
            name: "Claude Sonnet 4".into(),
            reasoning_efforts: vec![ReasoningEffort::None, ReasoningEffort::High],
            context_length: Some(200_000),
            supports_images: true,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: Some(ModelPricing {
                prompt: 3.0,
                completion: 15.0,
                cache_read: Some(0.3),
                cache_write: Some(3.0),
            }),
        };
        let app = AppEvent::Models(Some(Ok(vec![model])));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Models(Some(Ok(vec![wire_model()])))
        );

        let app = AppEvent::Models(Some(Err("no backend configured".into())));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Models(Some(Err("no backend configured".into())))
        );
    }

    /// Memory and `OCR` payloads map every op/update shape.
    #[test]
    fn from_app_event_maps_memory_and_ocr() {
        let app = AppEvent::Memory(Some((
            "sp1".into(),
            vec![
                MemoryOp::Add("alpha".into()),
                MemoryOp::Update(2, "beta".into()),
                MemoryOp::Delete(3),
            ],
        )));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Memory(Some((
                "sp1".into(),
                vec![
                    WireMemoryOp::Add("alpha".into()),
                    WireMemoryOp::Update(2, "beta".into()),
                    WireMemoryOp::Delete(3),
                ],
            )))
        );

        let app = AppEvent::Ocr(Some((
            "f1".into(),
            "s1".into(),
            OcrUpdate::Done(Ok(("text".into(), vec![(2, "reason".into())]))),
        )));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Ocr(Some((
                "f1".into(),
                "s1".into(),
                WireOcrUpdate::Done(Ok(("text".into(), vec![(2, "reason".into())]))),
            )))
        );
    }

    /// Research payloads map stage ticks, both parked gates, and the final
    /// result — including the planner's question list.
    #[test]
    fn from_app_event_maps_research() {
        let updates = [
            (
                ResearchUpdate::Stage {
                    label: "survey".into(),
                    detail: "asking…".into(),
                },
                WireResearchUpdate::Stage {
                    label: "survey".into(),
                    detail: "asking…".into(),
                },
            ),
            (
                ResearchUpdate::SurveyReady {
                    questions: vec!["q1".into()],
                    round: 1,
                },
                WireResearchUpdate::SurveyReady {
                    questions: vec!["q1".into()],
                    round: 1,
                },
            ),
            (
                ResearchUpdate::PlanReady {
                    questions: vec![PlanQuestion {
                        question: "q1".into(),
                        why: "w1".into(),
                        angles: vec!["a1".into()],
                        sources: vec!["s1".into()],
                    }],
                    rework: false,
                },
                WireResearchUpdate::PlanReady {
                    questions: vec![WirePlanQuestion {
                        question: "q1".into(),
                        why: "w1".into(),
                        angles: vec!["a1".into()],
                        sources: vec!["s1".into()],
                    }],
                    rework: false,
                },
            ),
            (
                ResearchUpdate::Done(Ok("report".into())),
                WireResearchUpdate::Done(Ok("report".into())),
            ),
        ];
        for (update, wire) in updates {
            let app = AppEvent::Research(Some(("s1".into(), "sp1".into(), "Space".into(), update)));
            assert_eq!(
                WireEvent::from(app),
                WireEvent::Research(Some(("s1".into(), "sp1".into(), "Space".into(), wire)))
            );
        }
    }

    /// Swarm and login payloads map their nested types (roster personas,
    /// Codex credentials) through.
    #[test]
    fn from_app_event_maps_swarm_and_login() {
        let app = AppEvent::Swarm(Some((
            "s1".into(),
            SwarmUpdate::RosterSuggested(vec![Persona {
                name: "ada".into(),
                model: "m1".into(),
                blurb: "b".into(),
            }]),
        )));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Swarm(Some((
                "s1".into(),
                WireSwarmUpdate::RosterSuggested(vec![WirePersona {
                    name: "ada".into(),
                    model: "m1".into(),
                    blurb: "b".into(),
                }]),
            )))
        );

        let app = AppEvent::Login(Some(LoginMsg::Done(Ok(CodexCredentials {
            access: "a".into(),
            refresh: "r".into(),
            expires: 123,
            account_id: "acc".into(),
        }))));
        assert_eq!(
            WireEvent::from(app),
            WireEvent::Login(Some(WireLoginMsg::Done(Ok(WireCodexCredentials {
                access: "a".into(),
                refresh: "r".into(),
                expires: 123,
                account_id: "acc".into(),
            }))))
        );
    }
}
