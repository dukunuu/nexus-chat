//! Deep research: a background multi-agent pipeline triggered by `/research`.
//! Every stage but the Searcher fan-out is a single `Provider::complete`
//! call; parsing/prompt-building here is pure and unit tested. The async
//! orchestration (Task 9) calls real network endpoints and is exercised
//! manually, like every other network-calling background job in this
//! codebase (`maybe_generate_title`, image description, embedding).

use crate::provider::ChatMessage;

/// A background research pipeline update: a phase label (+ progress detail),
/// the Planner's sub-questions awaiting approval, or the final report/error.
pub(crate) enum ResearchUpdate {
    /// Successive updates within one stage share a `label` so the UI/db
    /// replace one row in place instead of appending per tick.
    Stage { label: String, detail: String },
    /// The Planner finished; the pipeline is paused awaiting approve/edit
    /// (or its 60s auto-continue timeout).
    PlanReady { questions: Vec<String> },
    Done(std::result::Result<String, String>),
}

/// Hard cap on Planner-generated sub-questions per outer round.
const MAX_SUBQUESTIONS: usize = 6;
/// Tool-call budget for a single Searcher agent — a few search→fetch hops,
/// not a whole interactive conversation's worth.
pub(crate) const RESEARCH_SEARCHER_MAX_ITERS: usize = 6;

const PLANNER_PROMPT: &str = "You are the planning stage of an automated research pipeline. Given a research topic, decompose it into 3 to 6 focused sub-questions that together cover the topic thoroughly (different angles: definitions, current state, evidence/data, controversies, practical implications — whichever apply). Respond with ONLY a JSON array of strings, no prose, no markdown fences. Example: [\"question one\", \"question two\"]. Note: searcher agents handling scholarly sub-questions can call academic_search (Semantic Scholar) in addition to web_search, so peer-reviewed angles are fair game.";

pub(crate) const SEARCHER_PROMPT: &str = "You are a research searcher agent. You will be given one focused sub-question. Use the web_search and fetch_url tools to investigate it thoroughly: search, then fetch and read the most promising pages, and search again with new terms you learn from them if needed. When you have enough to answer well, write a concise findings summary (a few paragraphs, prose, no headers) that directly answers the sub-question, citing sources inline as [n]. End your answer with a line starting exactly with 'Sources:' followed by the numbered list of URLs you used, one per line, matching your [n] citations. Prefer sources from domains you have not already cited — diverse sources make a stronger report.";

const SYNTHESIZER_PROMPT: &str = "You are the synthesis stage of a research pipeline. You'll be given the original topic and findings from several searcher agents, each already citing their own sources. Combine them into a single coherent draft report on the topic: organize by theme (not by sub-question), resolve obvious overlaps, keep every citation but you may renumber them consistently as you merge. Do not invent facts not present in the findings. Output the draft report in markdown, no preamble.";

const CRITIC_PROMPT: &str = "You are the critic stage of a research pipeline. Given the original topic and a draft report, decide if it's ready. Respond in exactly one of these forms:\n- the single word SATISFIED, if the draft thoroughly covers the topic with no notable gaps or contradictions.\n- GAPS: followed by a newline-separated bullet list (each line starting with '- ') of specific missing sub-topics or unanswered angles, each phrased as a searchable question.\n- CONTRADICTION: followed by one line describing a specific factual contradiction between sources in the draft that isn't resolved.\nUse CONTRADICTION only for an actual conflict between sources, not a missing angle — missing angles are always GAPS. Respond with nothing else.";

const ESCALATION_PROMPT: &str = "You are resolving a contradiction found in a research draft. You are given the topic, the draft, the full set of source findings gathered so far, and a description of the contradiction. Determine which claim the evidence better supports (or that both apply in different contexts) and write one paragraph resolving it, citing the [n] sources involved. Output only that paragraph.";

const VERIFIER_PROMPT: &str = "You are the verifier stage. Given the topic, the gathered source findings (with their citations), and a draft report, check every factual claim in the draft against the source findings. Rewrite the draft unchanged except: (1) remove or mark with '⚠ unverifiable:' any claim not actually supported by the gathered findings; (2) immediately after a claim's citations, judge its confidence from citation count and cross-source agreement and, only for low or medium confidence, append the tag ‹low› or ‹med› right after the citation (high confidence is the default and stays untagged — do not tag it). Output the corrected draft in markdown, nothing else.";

const WRITER_PROMPT: &str = "You are the final writer stage. Given the topic and a verified draft report (with inline [n] citations and prose from earlier stages, possibly including a contradiction-resolution paragraph to fold in), produce the final report: clean markdown, a short introductory paragraph, organized sections with headers, inline [n] citations preserved/renumbered consistently, and a trailing '## Sources' section listing every cited URL as 'n. url'. Output only the final report markdown, nothing else — it will be saved and shown to the user as-is.";

/// The Critic stage's structured decision.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Critique {
    Satisfied,
    Gaps(Vec<String>),
    Contradiction(String),
}

/// Parse the Planner's raw reply into sub-questions: a JSON string array, or
/// (if the model didn't follow instructions) a best-effort line-by-line
/// fallback stripping bullet/number prefixes. Always capped at
/// `MAX_SUBQUESTIONS`.
pub(crate) fn parse_subquestions(text: &str) -> Vec<String> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Vec<String>>(trimmed) {
        return v
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .take(MAX_SUBQUESTIONS)
            .collect();
    }
    trimmed
        .lines()
        .map(strip_list_prefix)
        .filter(|l| !l.is_empty())
        .take(MAX_SUBQUESTIONS)
        .collect()
}

/// Strip a leading `-`, `*`, or `N.`/`N)` list-item marker, if present.
fn strip_list_prefix(line: &str) -> String {
    let s = line.trim().trim_start_matches(['-', '*']).trim();
    let digits_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits_end > 0 {
        if let Some(rest) = s[digits_end..].strip_prefix(['.', ')']) {
            return rest.trim().to_string();
        }
    }
    s.to_string()
}

/// Parse the user-edited plan (one sub-question per line, same bullet/number
/// tolerance as the Planner's own fallback parser) back into a list.
pub(crate) fn parse_plan_edit(text: &str) -> Vec<String> {
    text.lines().map(strip_list_prefix).filter(|l| !l.is_empty()).take(MAX_SUBQUESTIONS).collect()
}

/// Parse the Critic's raw reply into a `Critique`. Anything that doesn't
/// match one of the three expected shapes is treated as `Satisfied` — an
/// unparseable critique shouldn't loop the pipeline forever on garbage.
pub(crate) fn parse_critique(text: &str) -> Critique {
    let t = text.trim();
    if t.eq_ignore_ascii_case("SATISFIED") {
        return Critique::Satisfied;
    }
    if let Some(rest) = t.strip_prefix("CONTRADICTION:") {
        let desc = rest.trim();
        if !desc.is_empty() {
            return Critique::Contradiction(desc.to_string());
        }
    }
    if let Some(rest) = t.strip_prefix("GAPS:") {
        let gaps: Vec<String> = rest
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix('-'))
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(MAX_SUBQUESTIONS)
            .collect();
        if !gaps.is_empty() {
            return Critique::Gaps(gaps);
        }
    }
    Critique::Satisfied
}

/// The Planner's request, with any locally-known context (chunks from the
/// space's own files, semantically matched to the topic) framed as "already
/// known — plan sub-questions for the gaps".
fn planner_messages_with_context(topic: &str, known: &[String]) -> Vec<ChatMessage> {
    let user = if known.is_empty() {
        topic.to_string()
    } else {
        format!(
            "Topic: {topic}\n\nAlready known (from the user's own files) — plan sub-questions \
             for the gaps, not what's already covered:\n{}",
            known.join("\n\n")
        )
    };
    vec![ChatMessage::text("system", PLANNER_PROMPT), ChatMessage::text("user", user)]
}

fn synthesizer_messages(topic: &str, findings: &[String], pinned: &[String]) -> Vec<ChatMessage> {
    let body = findings
        .iter()
        .enumerate()
        .map(|(i, f)| format!("--- Searcher {} findings ---\n{f}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut user = format!("Topic: {topic}\n\n");
    if !pinned.is_empty() {
        user.push_str(&format!(
            "Prioritize these pinned sources in the synthesis if their content is present in the findings below:\n{}\n\n",
            pinned.join("\n")
        ));
    }
    user.push_str(&body);
    vec![ChatMessage::text("system", SYNTHESIZER_PROMPT), ChatMessage::text("user", user)]
}

fn critic_messages(topic: &str, draft: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", CRITIC_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nDraft:\n{draft}")),
    ]
}

fn escalation_messages(topic: &str, draft: &str, findings: &[String], contradiction: &str) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text("system", ESCALATION_PROMPT),
        ChatMessage::text(
            "user",
            format!("Topic: {topic}\n\nContradiction: {contradiction}\n\nDraft:\n{draft}\n\nSource findings:\n{body}"),
        ),
    ]
}

fn verifier_messages(topic: &str, draft: &str, findings: &[String]) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text("system", VERIFIER_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nSource findings:\n{body}\n\nDraft:\n{draft}")),
    ]
}

fn writer_messages(topic: &str, verified_draft: &str, pinned: &[String]) -> Vec<ChatMessage> {
    let mut user = format!("Topic: {topic}\n\n");
    if !pinned.is_empty() {
        user.push_str(&format!(
            "Prioritize these pinned sources in the final report if their content is present in the verified draft below:\n{}\n\n",
            pinned.join("\n")
        ));
    }
    user.push_str(&format!("Verified draft:\n{verified_draft}"));
    vec![ChatMessage::text("system", WRITER_PROMPT), ChatMessage::text("user", user)]
}

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatParams, StreamEvent};
use crate::tools::ToolBox;

use super::ResearchMsg;

/// Send the `(session_id, space_id, space_name)` triple's stage update.
fn send_stage(
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    label: impl Into<String>,
    detail: impl Into<String>,
) {
    let _ = tx.send((
        ids.0.clone(),
        ids.1.clone(),
        ids.2.clone(),
        ResearchUpdate::Stage { label: label.into(), detail: detail.into() },
    ));
}

async fn complete_text(provider: &OpenRouter, model: &str, messages: Vec<ChatMessage>) -> Result<String, String> {
    provider.complete(model, messages).await.map(|s| s.trim().to_string()).map_err(|e| e.to_string())
}

async fn plan(provider: &OpenRouter, model: &str, topic: &str, known: &[String]) -> Result<Vec<String>, String> {
    let text = complete_text(provider, model, planner_messages_with_context(topic, known)).await?;
    let qs = parse_subquestions(&text);
    if qs.is_empty() {
        return Err(format!("planner returned no usable sub-questions (raw reply: {text:.200})"));
    }
    Ok(qs)
}

/// One Searcher agent: given a single sub-question, runs the normal
/// tool-loop (restricted to web_search/fetch_url/academic_search) and
/// returns its final prose findings (including its own "Sources:" citation
/// list). Never returns an `Err` — a dead search/fetch/model call becomes a
/// placeholder finding string so one bad sub-question can't sink the whole
/// pipeline.
///
/// Every `Status`/`ToolCall` event along the way is forwarded as a live
/// stage update under this searcher's own label (`searcher N/total`), so the
/// UI shows what it's actually doing (searching, fetching a URL, etc.) in
/// real time instead of going silent until it finishes.
#[allow(clippy::too_many_arguments)]
async fn run_searcher(
    provider: &OpenRouter,
    model: &str,
    sub_question: &str,
    toolbox: Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    round: usize,
    idx: usize,
    total: usize,
) -> String {
    let label = format!("searcher {}/{total}", idx + 1);
    send_stage(tx, ids, &label, format!("round {round}: \"{sub_question}\" — starting…"));
    let messages = vec![
        ChatMessage::text("system", SEARCHER_PROMPT),
        ChatMessage::text("user", sub_question),
    ];
    let tools = toolbox.defs();
    let (mut rx, _abort) = provider.stream_chat(
        model.to_string(),
        messages,
        ChatParams::default(),
        tools,
        toolbox,
        RESEARCH_SEARCHER_MAX_ITERS,
    );
    let mut buf = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Status(s) => {
                send_stage(tx, ids, &label, format!("round {round}: \"{sub_question}\" — {s}"));
            }
            StreamEvent::ToolCall { name, arguments, .. } => {
                let arg_summary: String = arguments.chars().take(80).collect();
                send_stage(
                    tx,
                    ids,
                    &label,
                    format!("round {round}: \"{sub_question}\" — used {name}({arg_summary})"),
                );
            }
            StreamEvent::Error(e) => return format!("[search agent error on \"{sub_question}\": {e}]"),
            StreamEvent::Done => break,
            _ => {}
        }
    }
    let text = buf.trim();
    if text.is_empty() {
        format!("[no findings for \"{sub_question}\"]")
    } else {
        text.to_string()
    }
}

/// Fan out one Searcher per question in parallel, sending a running
/// `{done}/{total}` stage update as each finishes (in addition to each
/// searcher's own live per-tool-call feed). Order of the returned findings
/// doesn't matter (synthesis treats them as an unordered set).
async fn run_searchers(
    provider: &OpenRouter,
    model: &str,
    toolbox: &Arc<ToolBox>,
    questions: &[String],
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    round: usize,
) -> Vec<String> {
    let total = questions.len();
    let mut set = tokio::task::JoinSet::new();
    for (idx, q) in questions.iter().cloned().enumerate() {
        let provider = provider.clone();
        let model = model.to_string();
        let toolbox = toolbox.clone();
        let tx = tx.clone();
        let ids = ids.clone();
        set.spawn(async move { run_searcher(&provider, &model, &q, toolbox, &tx, &ids, round, idx, total).await });
    }
    let mut done = 0usize;
    let mut findings = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        done += 1;
        send_stage(tx, ids, "searching", format!("round {round}, {done}/{total} done"));
        findings.push(res.unwrap_or_else(|e| format!("[search agent panicked: {e}]")));
    }
    findings
}

/// Run the full pipeline and send exactly one final `Done` on `tx` (the
/// caller's channel then closes naturally when this function returns and
/// `tx` is dropped).
pub(crate) async fn run_research(
    provider: OpenRouter,
    research_model: String,
    escalation_model: String,
    embedding_model: String,
    db_path: std::path::PathBuf,
    topic: String,
    gate_rx: Option<tokio::sync::oneshot::Receiver<Vec<String>>>,
    toolbox: Arc<ToolBox>,
    tx: mpsc::UnboundedSender<ResearchMsg>,
    session_id: String,
    space_id: String,
    space_name: String,
) {
    let known = local_known_chunks(&provider, &embedding_model, &db_path, &space_id, &topic).await;
    let ids = (session_id, space_id, space_name);
    let result = run_research_inner(
        &provider,
        &research_model,
        &escalation_model,
        &topic,
        &known,
        gate_rx,
        &db_path,
        &toolbox,
        &tx,
        &ids,
    )
    .await;
    let _ = tx.send((ids.0, ids.1, ids.2, ResearchUpdate::Done(result)));
}

/// Top-k chunks from the space's files already relevant to `topic`, for the
/// Planner's "already known" context — silently empty when embeddings are
/// unconfigured, embedding fails, or the space has no files (never blocks
/// `/research` on any of those).
async fn local_known_chunks(
    provider: &OpenRouter,
    embedding_model: &str,
    db_path: &std::path::Path,
    space_id: &str,
    topic: &str,
) -> Vec<String> {
    if embedding_model.trim().is_empty() {
        return Vec::new();
    }
    let Ok(mut vecs) = provider.embed(embedding_model, vec![topic.to_string()]).await else {
        return Vec::new();
    };
    if vecs.is_empty() {
        return Vec::new();
    }
    let query = vecs.remove(0);
    let Ok(conn) = rusqlite::Connection::open(db_path) else { return Vec::new() };
    crate::db::semantic_chunks(&conn, space_id, &query, 5)
        .map(|hits| hits.into_iter().map(|(name, loc, text, _)| format!("{name} ({loc}): {text}")).collect())
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn run_research_inner(
    provider: &OpenRouter,
    research_model: &str,
    escalation_model: &str,
    topic: &str,
    known: &[String],
    gate_rx: Option<tokio::sync::oneshot::Receiver<Vec<String>>>,
    db_path: &std::path::Path,
    toolbox: &Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> Result<String, String> {
    send_stage(tx, ids, "planning", "");
    let mut questions = plan(provider, research_model, topic, known).await?;

    // Plan-approval gate: show the sub-questions and wait for approve/edit,
    // auto-continuing after 60s so a backgrounded session never hangs.
    if let Some(gate_rx) = gate_rx {
        let _ = tx.send((
            ids.0.clone(),
            ids.1.clone(),
            ids.2.clone(),
            ResearchUpdate::PlanReady { questions: questions.clone() },
        ));
        questions = match tokio::time::timeout(std::time::Duration::from_secs(60), gate_rx).await {
            Ok(Ok(edited)) if !edited.is_empty() => edited,
            // Timeout, dropped sender, or an empty edit — continue as planned.
            _ => questions,
        };
    }

    let pinned = rusqlite::Connection::open(db_path)
        .ok()
        .and_then(|conn| crate::db::pinned_urls(&conn, &ids.0).ok())
        .unwrap_or_default();

    let mut findings = run_searchers(provider, research_model, toolbox, &questions, tx, ids, 1).await;
    persist_session_sources(db_path, &ids.0, &findings);

    send_stage(tx, ids, "synthesizing", "");
    let mut draft = complete_text(
        provider,
        research_model,
        synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings), &pinned),
    )
    .await?;

    send_stage(tx, ids, "critiquing", "");
    let mut critique = parse_critique(&complete_text(provider, research_model, critic_messages(topic, &draft)).await?);

    if let Critique::Gaps(gaps) = &critique {
        let more = run_searchers(provider, research_model, toolbox, gaps, tx, ids, 2).await;
        persist_session_sources(db_path, &ids.0, &more);
        findings.extend(more);
        send_stage(tx, ids, "re-synthesizing", "");
        draft = complete_text(
            provider,
            research_model,
            synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings), &pinned),
        )
        .await?;
        send_stage(tx, ids, "critiquing", "round 2");
        critique = parse_critique(&complete_text(provider, research_model, critic_messages(topic, &draft)).await?);
    }

    if let Critique::Contradiction(desc) = &critique {
        send_stage(tx, ids, "resolving a contradiction", "");
        let resolution =
            complete_text(provider, escalation_model, escalation_messages(topic, &draft, &findings, desc)).await?;
        draft.push_str("\n\n");
        draft.push_str(&resolution);
    }

    send_stage(tx, ids, "verifying", "");
    let verified = complete_text(provider, research_model, verifier_messages(topic, &draft, &findings)).await?;

    send_stage(tx, ids, "writing final report", "");
    complete_text(provider, research_model, writer_messages(topic, &verified, &pinned)).await
}

/// Link every URL cited in `findings` into the session's source bundle
/// (they're already in `web_cache` from `fetch_url`'s write-through). Best
/// effort — a failed write never disturbs the pipeline.
fn persist_session_sources(db_path: &std::path::Path, session_id: &str, findings: &[String]) {
    let url_norms = crate::tools::cited_url_norms(findings);
    if url_norms.is_empty() {
        return;
    }
    if let Ok(conn) = rusqlite::Connection::open(db_path) {
        let _ = crate::db::add_session_sources(&conn, session_id, &url_norms);
    }
}

impl super::App {
    /// `/research <topic>`: run the multi-agent research pipeline in a new
    /// background session. One job at a time. `/research! <topic>` skips the
    /// plan-approval gate.
    pub(crate) fn start_research(&mut self, topic: &str) {
        self.start_research_with_gate(topic, true)
    }

    pub(crate) fn start_research_with_gate(&mut self, topic: &str, gated: bool) {
        let topic = topic.trim().to_string();
        if topic.is_empty() {
            self.status = "usage: /research <topic>".to_string();
            return;
        }
        if self.research_model.trim().is_empty() {
            self.status = "no research model configured — set one in /config".to_string();
            return;
        }
        if self.research_rx.is_some() {
            self.status = "a research job is already running".to_string();
            return;
        }
        let Some(provider) = self.provider.clone() else {
            self.open_key_prompt();
            return;
        };
        let research_model = self.research_model.trim().to_string();
        let escalation_model = if self.escalation_model.trim().is_empty() {
            research_model.clone()
        } else {
            self.escalation_model.trim().to_string()
        };
        let title = super::chat::title_from(&topic);
        let session = match self.db.create_session(&title, &research_model, &self.active_space.id) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("could not start research session: {e}");
                return;
            }
        };
        let _ = self.db.add_user_message(&session.id, &format!("/research {topic}"));

        let searxng_url = (!self.searxng_url.trim().is_empty()).then(|| self.searxng_url.trim().to_string());
        let langsearch_key = (!self.langsearch_key.trim().is_empty()).then(|| self.langsearch_key.trim().to_string());
        let toolbox = Arc::new(ToolBox::research(
            searxng_url,
            langsearch_key,
            self.search_provider.clone(),
            self.blocked_domains(),
            Some(self.space.db_path()),
        ));

        let (tx, rx) = mpsc::unbounded_channel();
        self.research_rx = Some(rx);
        self.research_running = Some((session.id.clone(), topic.clone()));
        self.status = format!("researching: {topic}");

        let gate_rx = if gated {
            let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
            self.research_plan_gate = Some((session.id.clone(), gate_tx, Vec::new()));
            Some(gate_rx)
        } else {
            None
        };

        let space_id = self.active_space.id.clone();
        let space_name = self.active_space.name.clone();
        self.messages = self.db.load_messages(&session.id).unwrap_or_default();
        self.session = Some(session.clone());
        self.context_total = None;
        self.scroll = 0;
        self.refresh_toolbox();

        tokio::spawn(run_research(
            provider,
            research_model,
            escalation_model,
            self.embedding_model.trim().to_string(),
            self.space.db_path(),
            topic,
            gate_rx,
            toolbox,
            tx,
            session.id,
            space_id,
            space_name,
        ));
    }

    /// Enter on a pending plan gate: continue with the (possibly edited)
    /// cached questions as-is.
    pub(crate) fn approve_research_plan(&mut self) {
        if let Some((_, tx, cached)) = self.research_plan_gate.take() {
            let _ = tx.send(cached);
            self.status = "continuing research…".to_string();
        }
    }

    /// `e` on a pending plan gate: prefill the composer with one question
    /// per line so the user can edit it like any other message.
    pub(crate) fn edit_research_plan(&mut self) {
        if let Some((_, _, cached)) = &self.research_plan_gate {
            let text = cached.join("\n");
            self.set_input(&text);
            self.status = "edit the plan, one question per line — Enter to submit".to_string();
        }
    }

    /// Submit an edited plan (composer contents). A no-op with a status
    /// message if the gate already timed out and auto-continued.
    pub(crate) fn submit_research_plan_edit(&mut self, text: &str) {
        let Some((_, tx, _)) = self.research_plan_gate.take() else {
            self.status = "plan gate already closed (timed out) — edit ignored".to_string();
            return;
        };
        let _ = tx.send(parse_plan_edit(text));
        self.status = "plan updated — continuing research…".to_string();
    }

    /// A research pipeline update: a stage label, or the final report/error.
    /// `None` = the job's channel closed (fires once, right after `Done`).
    pub fn on_research_done(&mut self, r: Option<ResearchMsg>) {
        let Some((session_id, space_id, space_name, update)) = r else {
            self.research_rx = None;
            self.research_running = None;
            self.research_plan_gate = None;
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            ResearchUpdate::Stage { label, detail } => {
                let _ = self.db.upsert_research_stage_message(&session_id, &label, &detail);
                if viewing {
                    let text = crate::db::stage_content(&label, &detail);
                    let prefix = format!("{label}:");
                    // Mirror the db upsert in the in-memory transcript: one
                    // row per label, updated in place.
                    if let Some(row) = self.messages.iter_mut().rev().find(|m| {
                        m.role == "research_stage" && (m.content == label || m.content.starts_with(&prefix))
                    }) {
                        row.content = text.clone();
                    } else {
                        self.messages.push(crate::db::Message {
                            id: String::new(),
                            role: "research_stage".to_string(),
                            content: text.clone(),
                            model: None,
                            reasoning: None,
                            tokens: None,
                            secs: None,
                            phrase: None,
                            images: Vec::new(),
                        });
                    }
                    self.status = format!("research: {text}");
                }
            }
            ResearchUpdate::PlanReady { questions } => {
                if let Some((gate_session, _, cached)) = self.research_plan_gate.as_mut()
                    && *gate_session == session_id
                {
                    *cached = questions.clone();
                }
                let plan_text = questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| format!("{}. {q}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                let content = format!(
                    "Research plan ready — [e]dit / Enter to continue (auto-continues in 60s):\n{plan_text}"
                );
                let _ = self.db.add_research_plan_message(&session_id, &content);
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "research_plan".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                    });
                    self.status = "research plan ready — [e]dit / Enter to continue".to_string();
                }
            }
            ResearchUpdate::Done(Ok(report)) => {
                let _ = self.db.add_assistant_message(&session_id, &report, None, None, None, None, None);
                let topic = self.research_running.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
                self.save_research_report(&space_id, &space_name, &topic, &report);
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "assistant".to_string(),
                        content: report,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: Some("Researched".to_string()),
                        images: Vec::new(),
                    });
                    self.status = "research complete".to_string();
                } else {
                    self.unread.insert(session_id);
                    if let Some((_, topic)) = &self.research_running {
                        self.status = format!("✓ research ready: {topic}");
                    }
                }
            }
            ResearchUpdate::Done(Err(e)) => {
                let msg = format!("research failed: {e}");
                let _ = self.db.add_assistant_message(&session_id, &msg, None, None, None, None, None);
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "assistant".to_string(),
                        content: msg.clone(),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                    });
                }
                self.status = msg;
            }
        }
    }

    /// Save the finished report into the job's own space (not necessarily
    /// the currently active one — the user may have switched spaces while
    /// the job ran), named `research-<slug>-<timestamp>.md`. Only refreshes
    /// the files cache / triggers a rescan if that space is still active;
    /// otherwise the file sits on disk and gets picked up next time that
    /// space's /files is opened, same as any externally-dropped file.
    fn save_research_report(&mut self, space_id: &str, space_name: &str, topic: &str, report: &str) {
        let dir = self.space.files_dir(space_name);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let slug = super::sessions::slugify(topic);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = format!("research-{slug}-{stamp}.md");
        if std::fs::write(dir.join(&name), report).is_err() {
            return;
        }
        // Index the report's cited sources for list_citations.
        let citations = crate::citations::parse_citations(report);
        if !citations.is_empty() {
            // Titles aren't in the Sources-list format; index url only.
            let rows: Vec<(String, Option<String>)> = citations.into_iter().map(|(_, url)| (url, None)).collect();
            let _ = self.db.add_citations(space_id, &name, &rows);
        }
        if space_id == self.active_space.id {
            self.rescan_files();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::db::Db;
    use crate::space::Space;

    fn test_app() -> App {
        let db = Db::open_in_memory().unwrap();
        let root = std::env::temp_dir().join(format!("nexus-research-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k".into()), space)
    }

    #[test]
    fn planner_messages_with_context_includes_known_chunks_as_gap_guidance() {
        let msgs = planner_messages_with_context(
            "rust async runtimes",
            &["Rust's async model uses a Future trait.".to_string()],
        );
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[1].content.contains("rust async runtimes"));
        assert!(msgs[1].content.contains("Already known"));
        assert!(msgs[1].content.contains("Future trait"));
    }

    #[test]
    fn planner_messages_with_context_falls_back_to_plain_prompt_when_empty() {
        let msgs = planner_messages_with_context("topic", &[]);
        assert!(!msgs[1].content.contains("Already known"));
        assert_eq!(msgs[1].content, "topic");
    }

    #[tokio::test]
    async fn on_research_done_final_report_populates_citation_index() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        a.on_research_done(Some((
            session_id,
            space_id.clone(),
            space_name,
            ResearchUpdate::Done(Ok(
                "# Report\n\nBody [1].\n\n## Sources\n1. https://example.com/a\n".to_string()
            )),
        )));

        let hits = a.db.search_citations(&space_id, Some("example.com")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "https://example.com/a");
        // And a miss filter returns nothing.
        assert!(a.db.search_citations(&space_id, Some("nope.example")).unwrap().is_empty());
    }

    #[test]
    fn parse_plan_edit_reads_one_question_per_line() {
        let qs = parse_plan_edit("what is X\nhow does Y work\n\nis Z true");
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string(), "is Z true".to_string()]);
    }

    #[test]
    fn parse_plan_edit_strips_bullet_and_number_prefixes_like_the_planner_parser() {
        let qs = parse_plan_edit("- what is X\n2. how does Y work");
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string()]);
    }

    #[test]
    fn parse_plan_edit_caps_at_max_subquestions() {
        let lines: Vec<String> = (0..10).map(|i| format!("q{i}")).collect();
        assert_eq!(parse_plan_edit(&lines.join("\n")).len(), MAX_SUBQUESTIONS);
    }

    #[tokio::test]
    async fn plan_gate_pauses_then_approve_lets_the_cached_questions_through() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::PlanReady { questions: vec!["q1".to_string(), "q2".to_string()] },
        )));
        assert!(a.research_plan_gate.is_some());
        assert!(a.messages.iter().any(|m| m.role == "research_plan"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "research_plan"));

        a.approve_research_plan();
        assert!(a.research_plan_gate.is_none());
    }

    #[tokio::test]
    async fn plan_gate_edit_prefills_composer_and_submit_sends_parsed_questions() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();
        a.on_research_done(Some((
            session_id,
            space_id,
            space_name,
            ResearchUpdate::PlanReady { questions: vec!["q1".to_string()] },
        )));

        a.edit_research_plan();
        assert_eq!(a.input_text(), "q1");
        let text = "edited one\nedited two".to_string();
        a.submit_research_plan_edit(&text);
        assert!(a.research_plan_gate.is_none());
        assert!(a.status.contains("plan updated"));
    }

    #[tokio::test]
    async fn plan_gate_edit_after_timeout_is_a_noop_with_status() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        // Simulate the gate already having closed (approved, or the
        // pipeline's own 60s timeout fired) — either way, `research_plan_gate`
        // is `None` by the time a stray edit submission arrives.
        a.research_plan_gate = None;
        a.submit_research_plan_edit("whatever");
        assert!(a.status.contains("timed out") || a.status.contains("ignored"));
    }

    #[test]
    fn start_research_rejects_blank_topic_and_missing_model() {
        let mut a = test_app();
        a.start_research("  ");
        assert!(a.status.contains("usage:"));
        assert!(a.research_rx.is_none());

        a.research_model.clear();
        a.start_research("rust async runtimes");
        assert!(a.status.contains("no research model configured"));
        assert!(a.research_rx.is_none());
    }

    #[tokio::test]
    async fn start_research_creates_and_switches_into_a_new_session() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        assert!(a.research_rx.is_some());
        assert!(a.research_running.is_some());
        let session = a.session.as_ref().expect("switched into the research session");
        assert!(session.title.contains("rust async runtimes"));
        assert!(a.messages.iter().any(|m| m.content.contains("/research rust async runtimes")));
    }

    #[tokio::test]
    async fn start_research_refuses_a_second_concurrent_job() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("topic one");
        assert!(a.research_rx.is_some());
        a.start_research("topic two");
        assert!(a.status.contains("already running"));
        // Still the first job's session.
        assert!(a.session.as_ref().unwrap().title.contains("topic one"));
    }

    #[tokio::test]
    async fn on_research_done_stage_update_persists_and_shows_when_viewing() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::Stage { label: "planning".to_string(), detail: String::new() },
        )));

        assert!(a.messages.iter().any(|m| m.role == "research_stage" && m.content == "planning"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "research_stage" && m.content == "planning"));
        assert!(a.status.contains("planning"));

        // A second tick with the same label replaces the row, not appends.
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();
        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::Stage { label: "planning".to_string(), detail: "revised".to_string() },
        )));
        let stored = a.db.load_messages(&session_id).unwrap();
        let rows: Vec<_> = stored.iter().filter(|m| m.role == "research_stage").collect();
        assert_eq!(rows.len(), 1, "one row per label, updated in place");
        assert_eq!(rows[0].content, "planning: revised");
    }

    #[tokio::test]
    async fn on_research_done_final_report_posts_message_saves_file_and_notifies_when_away() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        // Simulate the user navigating away before the job finishes.
        a.session = None;
        a.messages.clear();

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name.clone(),
            ResearchUpdate::Done(Ok("# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string())),
        )));

        assert!(a.unread.contains(&session_id));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "assistant" && m.content.contains("Rust Async Runtimes")));

        // Saved into the space's files dir and picked up by a rescan.
        let dir = a.space.files_dir(&space_name);
        let saved = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).count();
        assert_eq!(saved, 1, "expected exactly one saved report file in {dir:?}");
    }

    #[tokio::test]
    async fn on_research_done_saves_report_to_original_space_even_if_user_switched() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();

        // Start research in the default space (space A)
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let original_space_id = a.active_space.id.clone();
        let original_space_name = a.active_space.name.clone();

        // Create a second space (space B) and switch to it
        let second_space = a.db.create_space("research-test-space-2").unwrap();
        a.space.ensure_space_dir(&second_space.name).unwrap();
        a.active_space = second_space.clone();
        a.session = None;
        a.messages.clear();
        a.files_cache.clear();

        // Verify we're now in space B
        assert_eq!(a.active_space.id, second_space.id);
        assert_ne!(a.active_space.id, original_space_id);

        // Simulate the research job completing while we're in space B
        a.on_research_done(Some((
            session_id.clone(),
            original_space_id.clone(),
            original_space_name.clone(),
            ResearchUpdate::Done(Ok("# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string())),
        )));

        // Assert: the report file lands in the ORIGINAL space's files_dir
        let original_dir = a.space.files_dir(&original_space_name);
        let original_files = std::fs::read_dir(&original_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .count();
        assert_eq!(original_files, 1, "expected exactly one report file in original space {original_dir:?}");

        // Assert: the report file did NOT land in the second (now-active) space's files_dir
        let second_dir = a.space.files_dir(&second_space.name);
        let second_files = std::fs::read_dir(&second_dir)
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        assert_eq!(second_files, 0, "expected no files in second (active) space {second_dir:?}");

        // Assert: files_cache is still empty (rescan_files was NOT called for space B,
        // because the report was saved to space A, not space B)
        assert_eq!(a.files_cache.len(), 0, "files_cache should be empty since rescan was not triggered");
    }

    #[tokio::test]
    async fn on_research_done_failure_posts_error_message() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        a.on_research_done(Some((session_id.clone(), space_id, space_name, ResearchUpdate::Done(Err("planner: network down".to_string())))));

        assert!(a.status.contains("network down"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "assistant" && m.content.contains("network down")));
    }

    #[tokio::test]
    async fn on_research_done_none_clears_channel_and_running_state() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("t");
        assert!(a.research_rx.is_some());
        a.on_research_done(None);
        assert!(a.research_rx.is_none());
        assert!(a.research_running.is_none());
    }

    #[test]
    fn parse_subquestions_reads_a_clean_json_array() {
        let qs = parse_subquestions(r#"["what is X", "how does Y work"]"#);
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string()]);
    }

    #[test]
    fn parse_subquestions_strips_markdown_fences() {
        let qs = parse_subquestions("```json\n[\"a\", \"b\"]\n```");
        assert_eq!(qs, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_subquestions_falls_back_to_bullet_lines() {
        let qs = parse_subquestions("- what is X\n- how does Y work\n* a third one");
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string(), "a third one".to_string()]);
    }

    #[test]
    fn parse_subquestions_falls_back_to_numbered_lines() {
        let qs = parse_subquestions("1. what is X\n2) how does Y work");
        assert_eq!(qs, vec!["what is X".to_string(), "how does Y work".to_string()]);
    }

    #[test]
    fn parse_subquestions_caps_at_max() {
        let lines: Vec<String> = (0..10).map(|i| format!("- q{i}")).collect();
        let qs = parse_subquestions(&lines.join("\n"));
        assert_eq!(qs.len(), MAX_SUBQUESTIONS);
    }

    #[test]
    fn parse_critique_recognizes_satisfied() {
        assert_eq!(parse_critique("SATISFIED"), Critique::Satisfied);
        assert_eq!(parse_critique("  satisfied  "), Critique::Satisfied);
    }

    #[test]
    fn parse_critique_recognizes_gaps() {
        let c = parse_critique("GAPS:\n- what about pricing?\n- any recent incidents?");
        assert_eq!(
            c,
            Critique::Gaps(vec!["what about pricing?".to_string(), "any recent incidents?".to_string()])
        );
    }

    #[test]
    fn parse_critique_recognizes_contradiction() {
        let c = parse_critique("CONTRADICTION: source A says X, source B says not-X");
        assert_eq!(c, Critique::Contradiction("source A says X, source B says not-X".to_string()));
    }

    #[test]
    fn parse_critique_falls_back_to_satisfied_on_garbage() {
        assert_eq!(parse_critique("uh, looks fine I guess?"), Critique::Satisfied);
        assert_eq!(parse_critique("GAPS:\n"), Critique::Satisfied);
    }

    #[test]
    fn synthesizer_messages_includes_topic_and_all_findings() {
        let msgs = synthesizer_messages(
            "rust async runtimes",
            &["finding one".to_string(), "finding two".to_string()],
            &[],
        );
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[1].content.contains("rust async runtimes"));
        assert!(msgs[1].content.contains("finding one"));
        assert!(msgs[1].content.contains("finding two"));
    }

    #[test]
    fn synthesizer_messages_lists_pinned_sources_when_present() {
        let msgs = synthesizer_messages("topic", &["finding one".to_string()], &["https://a.example".to_string()]);
        let user = msgs.iter().find(|m| m.role == "user").unwrap();
        assert!(user.content.contains("https://a.example"), "{}", user.content);
        assert!(user.content.to_lowercase().contains("prioritize"), "{}", user.content);
    }

    #[test]
    fn synthesizer_messages_omits_pinned_section_when_empty() {
        let msgs = synthesizer_messages("topic", &["finding one".to_string()], &[]);
        let user = msgs.iter().find(|m| m.role == "user").unwrap();
        assert!(!user.content.to_lowercase().contains("prioritize"), "{}", user.content);
    }

    #[test]
    fn critic_messages_includes_topic_and_draft() {
        let msgs = critic_messages("topic X", "draft text");
        assert!(msgs[1].content.contains("topic X"));
        assert!(msgs[1].content.contains("draft text"));
    }

    #[test]
    fn escalation_messages_includes_contradiction_description() {
        let msgs = escalation_messages("t", "draft", &["f1".to_string()], "A vs B");
        assert!(msgs[1].content.contains("A vs B"));
        assert!(msgs[1].content.contains("f1"));
    }

    #[test]
    fn writer_messages_includes_verified_draft() {
        let msgs = writer_messages("t", "verified content", &[]);
        assert!(msgs[1].content.contains("verified content"));
    }
}
