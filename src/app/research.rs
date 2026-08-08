//! Deep research: a background multi-agent pipeline triggered by `/research`.
//! Every stage but the Searcher fan-out is a single `Provider::complete`
//! call; parsing/prompt-building here is pure and unit tested. The async
//! orchestration (Task 9) calls real network endpoints and is exercised
//! manually, like every other network-calling background job in this
//! codebase (`maybe_generate_title`, image description, embedding).

use crate::provider::ChatMessage;

/// A background research pipeline update: a phase label (+ progress detail),
/// the survey's clarifying questions awaiting a chat reply, the Planner's
/// sub-questions awaiting approval, or the final report/error.
pub(crate) enum ResearchUpdate {
    /// Successive updates within one stage share a `label` so the UI/db
    /// replace one row in place instead of appending per tick.
    Stage {
        label: String,
        detail: String,
    },
    /// The scoping agent's clarifying questions; the pipeline is parked
    /// awaiting a chat reply (`reply_to_survey_gate`). `round` is 1-based
    /// (max `MAX_SURVEY_ROUNDS`).
    SurveyReady {
        questions: Vec<String>,
        round: u8,
    },
    /// The Planner finished; the pipeline is parked awaiting a chat reply:
    /// "approve" runs the questions, edits get folded in by the approval
    /// agent and re-presented (`rework = true`) once, capped.
    PlanReady {
        questions: Vec<PlanQuestion>,
        rework: bool,
    },
    Done(std::result::Result<String, String>),
}

/// Hard cap on Planner-generated sub-questions per outer round.
const MAX_SUBQUESTIONS: usize = 6;
/// Hard bound on queued `/steer` instructions (and thus on retained steer
/// text and the unbounded channel) — beyond this, new steers are refused
/// with a status message until the next round boundary drains the queue.
const MAX_QUEUED_STEERS: usize = 64;
/// Cap on the scoping agent's clarifying questions per round.
const MAX_SURVEY_QUESTIONS: usize = 4;
/// Max survey rounds (initial + follow-ups) before the survey force-completes.
pub(crate) const MAX_SURVEY_ROUNDS: u8 = 3;
/// Tool-call budget for a single Searcher agent — a few search→fetch hops,
/// not a whole interactive conversation's worth.
pub(crate) const RESEARCH_SEARCHER_MAX_ITERS: usize = 6;

/// One Planner sub-question with its supporting brief: why it matters, the
/// angles to cover, and promising source types/leads. The whole block is
/// handed to its Searcher agent as the prompt — detail is functional.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct PlanQuestion {
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub angles: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
}

impl PlanQuestion {
    /// A question with no brief — the fallback shape when the Planner didn't
    /// follow the JSON-object instructions.
    pub(crate) fn bare(question: String) -> Self {
        PlanQuestion {
            question,
            why: String::new(),
            angles: Vec::new(),
            sources: Vec::new(),
        }
    }

    /// The Searcher's prompt: the topic plus this question's full block
    /// (why/angles/sources), so one focused agent answers one focused brief.
    pub(crate) fn prompt(&self, topic: &str) -> String {
        let mut p = format!(
            "Research topic: {topic}\n\nSub-question: {}\n",
            self.question
        );
        if !self.why.is_empty() {
            p.push_str(&format!("\nWhy this angle matters: {}\n", self.why));
        }
        if !self.angles.is_empty() {
            p.push_str(&format!("\nAngles to cover: {}\n", self.angles.join("; ")));
        }
        if !self.sources.is_empty() {
            p.push_str(&format!("\nSource leads: {}\n", self.sources.join("; ")));
        }
        p
    }
}

/// A plan rendered for the transcript / plan file: numbered questions, each
/// with its Why/Angles/Sources brief indented under it.
pub(crate) fn plan_text(questions: &[PlanQuestion]) -> String {
    questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let mut s = format!("{}. {}", i + 1, q.question);
            if !q.why.is_empty() {
                s.push_str(&format!("\n   Why: {}", q.why));
            }
            if !q.angles.is_empty() {
                s.push_str(&format!("\n   Angles: {}", q.angles.join("; ")));
            }
            if !q.sources.is_empty() {
                s.push_str(&format!("\n   Sources: {}", q.sources.join("; ")));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The scoping agent's reply: the single-word COMPLETE marker, a numbered
/// list of clarifying questions, or output that violates the contract
/// (`Malformed` — empty, explanatory prose, error text) which fails the
/// survey visibly instead of silently dropping it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SurveyReply {
    Complete,
    Questions(Vec<String>),
    Malformed,
}

/// The approval agent's verdict on a user reply to the plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Approval {
    Approved,
    Revised(Vec<PlanQuestion>),
    /// The agent produced output that is neither an approval nor a usable
    /// revision (bare prose, empty reply). Fails the phase visibly —
    /// malformed output must never be mistaken for approval.
    Malformed,
}

const PLANNER_PROMPT: &str = "You are the planning stage of an automated research pipeline. Given a research topic, decompose it into 3 to 6 focused sub-questions that together cover the topic thoroughly (different angles: definitions, current state, evidence/data, controversies, practical implications — whichever apply). For each sub-question include: 'question' (the sub-question itself), 'why' (one short sentence on the angle it covers), 'angles' (2-5 specific facets to investigate), and 'sources' (1-4 source types or leads likely to answer it). Respond with ONLY a JSON array of objects, no prose, no markdown fences. Example: [{\"question\": \"...\", \"why\": \"...\", \"angles\": [\"...\", \"...\"], \"sources\": [\"...\", \"...\"]}]. Note: searcher agents handling scholarly sub-questions can call search(mode=academic) in addition to search(mode=web), so peer-reviewed angles are fair game.";

const SURVEY_AGENT_PROMPT: &str = "You are the scoping stage of a research pipeline. You'll be given a research topic and, on later rounds, the user's answers so far. Ask 1 to 4 focused clarifying questions that would meaningfully change the research plan — scope, depth, angles, constraints — and skip anything you can infer. When you have enough to plan, reply with exactly the single word COMPLETE. Otherwise reply with your numbered questions only, one per line, no preamble, no markdown.";

const PLAN_APPROVAL_PROMPT: &str = "You are the approval stage of a research pipeline. The user was shown a plan of sub-questions (each with why/angles/sources). If the user's reply approves it — phrases like 'approve', 'looks good', 'go', 'ok', 'yes', or a bare affirmation — reply with exactly the single word APPROVED. Otherwise fold their feedback into the plan: apply the requested changes (drop questions, add angles, reword, add new questions up to 6 total) and reply with ONLY the revised JSON array of plan objects, no prose, no markdown fences. Example: [{\"question\": \"...\", \"why\": \"...\", \"angles\": [\"...\", \"...\"], \"sources\": [\"...\", \"...\"]}]";

pub(crate) const SEARCHER_PROMPT: &str = "You are a research searcher agent. You will be given one focused sub-question. Use search(mode=web) and fetch_url to investigate it thoroughly: search, then fetch and read the most promising pages, and search again with new terms you learn from them if needed. When you have enough to answer well, write a concise findings summary (a few paragraphs, prose, no headers) that directly answers the sub-question, citing sources inline as [n]. End your answer with a line starting exactly with 'Sources:' followed by the numbered list of URLs you used, one per line, matching your [n] citations. Prefer sources from domains you have not already cited — diverse sources make a stronger report.";

const SYNTHESIZER_PROMPT: &str = "You are the synthesis stage of a research pipeline. You'll be given the original topic and findings from several searcher agents, each already citing their own sources. Combine them into a single coherent draft report on the topic: organize by theme (not by sub-question), resolve obvious overlaps, keep every citation but you may renumber them consistently as you merge. Do not invent facts not present in the findings. Output the draft report in markdown, no preamble.";

const CRITIC_PROMPT: &str = "You are the critic stage of a research pipeline. Given the original topic and a draft report, decide if it's ready. Respond in exactly one of these forms:\n- the single word SATISFIED, if the draft thoroughly covers the topic with no notable gaps or contradictions.\n- GAPS: followed by a newline-separated bullet list (each line starting with '- ') of specific missing sub-topics or unanswered angles, each phrased as a searchable question.\n- CONTRADICTION: followed by one line describing a specific factual contradiction between sources in the draft that isn't resolved.\nUse CONTRADICTION only for an actual conflict between sources, not a missing angle — missing angles are always GAPS. Respond with nothing else.";

const ESCALATION_PROMPT: &str = "You are resolving a contradiction found in a research draft. You are given the topic, the draft, the full set of source findings gathered so far, and a description of the contradiction. Determine which claim the evidence better supports (or that both apply in different contexts) and write one paragraph resolving it, citing the [n] sources involved. Output only that paragraph.";

const VERIFIER_PROMPT: &str = "You are the verifier stage. Given the topic, the gathered source findings (with their citations), and a draft report, check every factual claim in the draft against the source findings. Rewrite the draft unchanged except: (1) remove or mark with '⚠ unverifiable:' any claim not actually supported by the gathered findings; (2) immediately after a claim's citations, judge its confidence from citation count and cross-source agreement and, only for low or medium confidence, append the tag ‹low› or ‹med› right after the citation (high confidence is the default and stays untagged — do not tag it). Output the corrected draft in markdown, nothing else. You have a fetch_url tool restricted to already-cached pages: use it to check any direct quote in the draft against the cached source text, and mark a quote that doesn't actually match with '‹unverified quote›' immediately after it.";

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
    if digits_end > 0
        && let Some(rest) = s[digits_end..].strip_prefix(['.', ')'])
    {
        return rest.trim().to_string();
    }
    s.to_string()
}

/// Parse the scoping agent's reply: `COMPLETE` (case-insensitive, optionally
/// with trailing punctuation or prose) ends the survey. Otherwise only lines
/// that look like questions — numbered/bulleted, or ending in `?` — are read
/// as clarifying questions. Anything else (empty output, explanatory or
/// error prose) is `Malformed`: the agent's output contract says COMPLETE or
/// questions, so a violation must fail the survey visibly — never be
/// mistaken for completion, and never park the pipeline awaiting an answer
/// for a non-question.
pub(crate) fn parse_survey_reply(text: &str) -> SurveyReply {
    let t = text.trim();
    // First word COMPLETE ends the survey, tolerating trailing punctuation
    // and prose: "COMPLETE", "COMPLETE.", "COMPLETE: proceed", "COMPLETE —".
    let head = t
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if head.eq_ignore_ascii_case("COMPLETE") {
        return SurveyReply::Complete;
    }
    let mut qs: Vec<String> = Vec::new();
    for line in t.lines() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        // Only list-marked or question-shaped lines count as questions;
        // bare prose (explanations, error messages) is a contract violation.
        let marked =
            s.starts_with(['-', '*']) || s.chars().next().is_some_and(|c| c.is_ascii_digit());
        let q = strip_list_prefix(s);
        if q.is_empty() {
            continue;
        }
        if !marked && !q.ends_with('?') {
            continue;
        }
        qs.push(q);
        if qs.len() >= MAX_SURVEY_QUESTIONS {
            break;
        }
    }
    if qs.is_empty() {
        SurveyReply::Malformed
    } else {
        SurveyReply::Questions(qs)
    }
}

/// Byte offset of the first `[` or `{` in `s`, if any. Structured JSON the
/// model wrapped in prose ("Here is the plan:\n[{\"question\":…}]") is
/// still JSON and must be parsed as such — never re-read as bare lines.
fn json_start(s: &str) -> Option<usize> {
    s.find(['[', '{'])
}

/// Parse the Planner's reply into plan blocks: a JSON array of objects with
/// `question`/`why`/`angles`/`sources` (missing fields default to empty).
/// Structured JSON is unambiguous — malformed JSON (parse failure, wrong
/// field types) or JSON with no usable questions yields an empty result and
/// fails planning; the raw JSON lines are never reinterpreted as bare
/// questions (`[{}]` must not become a plan whose question is literally
/// `[{}]`, and prose-prefixed JSON like "Here is the plan:\n[…]" is still
/// parsed as JSON, not as two raw lines). Only output with no JSON shape at
/// all falls back to one bare question per line (the legacy line format),
/// which also still accepts a legacy JSON array of strings. Always capped at
/// `MAX_SUBQUESTIONS`.
pub(crate) fn parse_plan_blocks(text: &str) -> Vec<PlanQuestion> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Some(start) = json_start(trimmed) {
        let candidate = &trimmed[start..];
        if let Ok(v) = serde_json::from_str::<Vec<PlanQuestion>>(candidate) {
            let qs: Vec<PlanQuestion> = v
                .into_iter()
                .map(|mut q| {
                    q.question = q.question.trim().to_string();
                    q
                })
                .filter(|q| !q.question.is_empty())
                .take(MAX_SUBQUESTIONS)
                .collect();
            if !qs.is_empty() {
                return qs;
            }
        }
        // Legacy structured format: a JSON array of plain strings.
        if let Ok(v) = serde_json::from_str::<Vec<String>>(candidate) {
            let qs: Vec<PlanQuestion> = v
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PlanQuestion::bare)
                .take(MAX_SUBQUESTIONS)
                .collect();
            if !qs.is_empty() {
                return qs;
            }
        }
        // Malformed or unusable structured output: fail, no line fallback.
        return Vec::new();
    }
    parse_subquestions(text)
        .into_iter()
        .map(PlanQuestion::bare)
        .collect()
}

/// Parse the approval agent's reply: `APPROVED` (case-insensitive, optionally
/// with trailing prose) accepts the plan; a JSON plan array is a revision;
/// line-formatted revisions are only accepted when they look like a list
/// (bullets or `N.`/`N)` prefixes). Anything else is `Malformed` — a garbled
/// verdict must fail the phase visibly, never silently count as approval.
pub(crate) fn parse_approval(text: &str) -> Approval {
    let upper = text.trim().to_ascii_uppercase();
    if upper == "APPROVED"
        || upper.starts_with("APPROVED:")
        || upper.starts_with("APPROVED —")
        || upper.starts_with("APPROVED\n")
    {
        return Approval::Approved;
    }
    // Structured JSON (possibly wrapped in prose) is unambiguous: parse it
    // strictly through `parse_plan_blocks`, and treat malformed or unusable
    // output as `Malformed` — never fall back to reading the raw JSON lines
    // as bare plan questions.
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if json_start(trimmed).is_some() {
        let qs = parse_plan_blocks(text);
        return if qs.is_empty() {
            Approval::Malformed
        } else {
            Approval::Revised(qs)
        };
    }
    // Line fallback: only when the output is recognizably a list.
    let has_markers = trimmed.lines().any(|l| {
        let s = l.trim();
        s.starts_with(['-', '*']) || s.chars().next().is_some_and(|c| c.is_ascii_digit())
    });
    if !has_markers {
        return Approval::Malformed;
    }
    let qs = parse_plan_blocks(text);
    if qs.is_empty() {
        Approval::Malformed
    } else {
        Approval::Revised(qs)
    }
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

/// The Planner's request: the topic, the user's survey answers ("what they
/// said they want"), and any locally-known context (chunks from the space's
/// own files plus a preliminary web survey, semantically matched to the
/// topic) framed as "already known — plan sub-questions for the gaps".
fn planner_messages_with_context(
    topic: &str,
    answers: &[(String, String)],
    known: &[String],
) -> Vec<ChatMessage> {
    let mut user = String::new();
    if !answers.is_empty() {
        user.push_str("The user answered clarifying questions before planning:\n");
        for (i, (qs, reply)) in answers.iter().enumerate() {
            user.push_str(&format!(
                "Round {} — asked: {}\nAnswered: {reply}\n",
                i + 1,
                qs
            ));
        }
        user.push('\n');
    }
    if known.is_empty() {
        user.push_str(topic);
    } else {
        user.push_str(&format!(
            "Topic: {topic}\n\nAlready known (from local files and/or a preliminary web survey) — \
             plan sub-questions for the gaps, not what's already covered:\n{}",
            known.join("\n\n")
        ));
    }
    vec![
        ChatMessage::text("system", PLANNER_PROMPT),
        ChatMessage::text("user", user),
    ]
}

/// The scoping agent's request: the topic, and on later rounds the questions
/// asked + answers given so far. One prompt serves both the initial questions
/// (empty rounds) and each follow-up round — the agent replies COMPLETE when
/// it has enough.
fn survey_messages(topic: &str, rounds: &[(String, String)]) -> Vec<ChatMessage> {
    let mut user = format!("Research topic: {topic}\n");
    if !rounds.is_empty() {
        user.push_str("\nSo far:\n");
        for (i, (qs, reply)) in rounds.iter().enumerate() {
            user.push_str(&format!(
                "Round {} — I asked:\n{qs}\nThe user answered: {reply}\n",
                i + 1
            ));
        }
    }
    vec![
        ChatMessage::text("system", SURVEY_AGENT_PROMPT),
        ChatMessage::text("user", user),
    ]
}

/// Fold the user's reply to the presented plan back into the pipeline: the
/// approval agent either recognizes an approval (APPROVED) or returns a
/// revised plan.
fn plan_approval_messages(
    topic: &str,
    questions: &[PlanQuestion],
    user_reply: &str,
) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", PLAN_APPROVAL_PROMPT),
        ChatMessage::text(
            "user",
            format!(
                "Topic: {topic}\n\nPlan:\n{}\n\nUser reply: {user_reply}",
                plan_text(questions)
            ),
        ),
    ]
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
    vec![
        ChatMessage::text("system", SYNTHESIZER_PROMPT),
        ChatMessage::text("user", user),
    ]
}

fn critic_messages(topic: &str, draft: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", CRITIC_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nDraft:\n{draft}")),
    ]
}

fn escalation_messages(
    topic: &str,
    draft: &str,
    findings: &[String],
    contradiction: &str,
) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text("system", ESCALATION_PROMPT),
        ChatMessage::text(
            "user",
            format!(
                "Topic: {topic}\n\nContradiction: {contradiction}\n\nDraft:\n{draft}\n\nSource findings:\n{body}"
            ),
        ),
    ]
}

fn verifier_messages(topic: &str, draft: &str, findings: &[String]) -> Vec<ChatMessage> {
    let body = findings.join("\n\n");
    vec![
        ChatMessage::text("system", VERIFIER_PROMPT),
        ChatMessage::text(
            "user",
            format!("Topic: {topic}\n\nSource findings:\n{body}\n\nDraft:\n{draft}"),
        ),
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
    vec![
        ChatMessage::text("system", WRITER_PROMPT),
        ChatMessage::text("user", user),
    ]
}

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatParams, StreamEvent};
use crate::tools::ToolBox;

use super::ResearchMsg;
use super::{SurveyGate, SurveyPhase};

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
        ResearchUpdate::Stage {
            label: label.into(),
            detail: detail.into(),
        },
    ));
}

/// Every steer instruction queued since the last drain, without blocking —
/// `try_recv` until the channel is empty. Called at each round boundary so
/// a user's mid-flight `/steer` gets picked up as an extra searcher round.
pub(crate) async fn drain_steers(rx: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(s) = rx.try_recv() {
        out.push(s);
    }
    out
}

async fn complete_text(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
) -> Result<String, String> {
    provider
        .complete(model, messages)
        .await
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string())
}

/// Run a non-streaming pipeline agent and terminalize its activity row on
/// failure. The caller owns the stage-specific success detail.
async fn complete_agent(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    label: &str,
) -> Result<String, String> {
    match complete_text(provider, model, messages).await {
        Ok(text) => Ok(text),
        Err(e) => {
            send_stage(tx, ids, label, format!("error — {e}"));
            Err(e)
        }
    }
}

async fn plan(
    provider: &OpenRouter,
    model: &str,
    topic: &str,
    answers: &[(String, String)],
    known: &[String],
) -> Result<Vec<PlanQuestion>, String> {
    let text = complete_text(
        provider,
        model,
        planner_messages_with_context(topic, answers, known),
    )
    .await?;
    let qs = parse_plan_blocks(&text);
    if qs.is_empty() {
        return Err(format!(
            "planner returned no usable sub-questions (raw reply: {text:.200})"
        ));
    }
    Ok(qs)
}

/// One Searcher agent: given one focused sub-question prompt, runs the normal
/// tool-loop (restricted to search/fetch_url) and returns its final prose
/// findings (including its own "Sources:" citation list). Never returns an
/// `Err` — a dead search/fetch/model call becomes a placeholder finding
/// string so one bad sub-question can't sink the whole pipeline.
///
/// `prompt` is the full block handed to the model (topic + why/angles/sources
/// brief — detail is functional); `display` is the short label used in the
/// live activity rows, so a searcher's status never leaks the whole brief.
///
/// Every `Status`/`ToolCall` event along the way is forwarded as a live
/// stage update under this searcher's own label (`searcher N/total`), so the
/// UI shows what it's actually doing (searching, fetching a URL, etc.) in
/// real time instead of going silent until it finishes.
#[allow(clippy::too_many_arguments)]
async fn run_searcher(
    provider: &OpenRouter,
    model: &str,
    prompt: &str,
    display: &str,
    toolbox: Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    batch: &str,
    idx: usize,
    total: usize,
) -> String {
    // Include the batch in the identity so follow-up and steered agents never
    // overwrite earlier activity rows that happen to have the same index.
    let label = format!("searcher {batch} {}/{total}", idx + 1);
    send_stage(
        tx,
        ids,
        &label,
        format!("working — investigating \"{display}\""),
    );
    let messages = vec![
        ChatMessage::text("system", SEARCHER_PROMPT),
        ChatMessage::text("user", prompt),
    ];
    let tools = toolbox.defs();
    let (mut rx, abort) = provider.stream_chat(
        model.to_string(),
        messages,
        ChatParams::default(),
        tools,
        toolbox,
        RESEARCH_SEARCHER_MAX_ITERS,
    );
    let _abort = super::AbortOnDrop(abort);
    let mut buf = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Status(s) => {
                send_stage(tx, ids, &label, format!("working — {s}"));
            }
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
            } => {
                let summary = crate::app::tool_call_summary(&name, &arguments, &result);
                send_stage(tx, ids, &label, format!("working — {summary}"));
            }
            StreamEvent::Error(e) => {
                send_stage(tx, ids, &label, format!("error — {e}"));
                return format!("[search agent error on \"{display}\": {e}]");
            }
            StreamEvent::Done => break,
            _ => {}
        }
    }
    let text = buf.trim();
    if text.is_empty() {
        send_stage(tx, ids, &label, "error — no findings returned");
        format!("[no findings for \"{display}\"]")
    } else {
        send_stage(tx, ids, &label, format!("done — answered \"{display}\""));
        text.to_string()
    }
}

/// Run the Verifier stage with a cache-only toolbox so it can check direct
/// quotes against exactly the pages searchers already gathered (never a
/// fresh fetch). Never returns an `Err` — returns whatever text accumulated
/// before the stream ended, or an empty string if it errored before
/// producing any. The caller falls back to the unverified draft when this
/// comes back empty, so verification failing must never blank out an
/// otherwise-good report.
async fn verify_with_quote_check(
    provider: &OpenRouter,
    model: &str,
    messages: Vec<ChatMessage>,
    cache_only_toolbox: Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> String {
    let tools = cache_only_toolbox.defs();
    let (mut rx, abort) = provider.stream_chat(
        model.to_string(),
        messages,
        ChatParams::default(),
        tools,
        cache_only_toolbox,
        RESEARCH_SEARCHER_MAX_ITERS,
    );
    let _abort = super::AbortOnDrop(abort);
    let mut buf = String::new();
    let mut failed = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Token(t) => buf.push_str(&t),
            StreamEvent::Status(s) => send_stage(tx, ids, "verifier", format!("working — {s}")),
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
            } => {
                let summary = crate::app::tool_call_summary(&name, &arguments, &result);
                send_stage(tx, ids, "verifier", format!("working — {summary}"));
            }
            StreamEvent::Error(e) => {
                failed = true;
                send_stage(tx, ids, "verifier", format!("error — {e}"));
                break;
            }
            StreamEvent::Done => break,
            _ => {}
        }
    }
    if !failed {
        if buf.trim().is_empty() {
            send_stage(
                tx,
                ids,
                "verifier",
                "error — no verification output returned",
            );
        } else {
            send_stage(tx, ids, "verifier", "done — source checks complete");
        }
    }
    buf
}

/// Fan out one Searcher per question in parallel, sending a running
/// `{done}/{total}` stage update as each finishes (in addition to each
/// searcher's own live per-tool-call feed). Order of the returned findings
/// doesn't matter (synthesis treats them as an unordered set). Each item is
/// `(prompt, display)`: the full prompt goes to the agent, the short display
/// label goes into the activity rows.
async fn run_searchers(
    provider: &OpenRouter,
    model: &str,
    toolbox: &Arc<ToolBox>,
    items: &[(String, String)],
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    batch: &str,
) -> Vec<String> {
    let total = items.len();
    send_stage(
        tx,
        ids,
        format!("search {batch}"),
        format!("working — 0/{total} agents complete"),
    );
    let mut set = tokio::task::JoinSet::new();
    for (idx, (prompt, display)) in items.iter().cloned().enumerate() {
        let provider = provider.clone();
        let model = model.to_string();
        let toolbox = toolbox.clone();
        let tx = tx.clone();
        let ids = ids.clone();
        let batch = batch.to_string();
        set.spawn(async move {
            run_searcher(
                &provider, &model, &prompt, &display, toolbox, &tx, &ids, &batch, idx, total,
            )
            .await
        });
    }
    let mut done = 0usize;
    let mut findings = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        done += 1;
        send_stage(
            tx,
            ids,
            format!("search {batch}"),
            format!("working — {done}/{total} agents complete"),
        );
        findings.push(res.unwrap_or_else(|e| format!("[search agent panicked: {e}]")));
    }
    send_stage(
        tx,
        ids,
        format!("search {batch}"),
        format!("done — {done}/{total} agents complete"),
    );
    findings
}

/// Run the full pipeline and send exactly one final `Done` on `tx` (the
/// caller's channel then closes naturally when this function returns and
/// `tx` is dropped).
/// Everything `run_research` needs to start a gated or ungated research job.
pub(crate) struct ResearchOptions {
    pub research_provider: OpenRouter,
    pub research_model: String,
    pub escalation_provider: OpenRouter,
    pub escalation_model: String,
    pub embedding_provider: OpenRouter,
    pub embedding_model: String,
    pub db_path: std::path::PathBuf,
    pub topic: String,
    pub reply_rx: Option<mpsc::UnboundedReceiver<String>>,
    pub steer_rx: mpsc::UnboundedReceiver<String>,
    pub toolbox: Arc<ToolBox>,
    pub tx: mpsc::UnboundedSender<ResearchMsg>,
    pub session_id: String,
    pub space_id: String,
    pub space_name: String,
}

pub(crate) async fn run_research(opts: ResearchOptions) {
    let ResearchOptions {
        research_provider,
        research_model,
        escalation_provider,
        escalation_model,
        embedding_provider,
        embedding_model,
        db_path,
        topic,
        reply_rx,
        steer_rx,
        toolbox,
        tx,
        session_id,
        space_id,
        space_name,
    } = opts;
    let ids = (session_id, space_id, space_name);
    let result = run_research_inner(
        &research_provider,
        &research_model,
        &escalation_provider,
        &escalation_model,
        &embedding_provider,
        &embedding_model,
        &topic,
        reply_rx,
        steer_rx,
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
    let Ok(mut vecs) = provider
        .embed(embedding_model, vec![topic.to_string()])
        .await
    else {
        return Vec::new();
    };
    if vecs.is_empty() {
        return Vec::new();
    }
    let query = vecs.remove(0);
    let Ok(conn) = rusqlite::Connection::open(db_path) else {
        return Vec::new();
    };
    crate::db::semantic_chunks(&conn, space_id, &query, 5)
        .map(|hits| {
            hits.into_iter()
                .map(|(name, loc, text, _)| format!("{name} ({loc}): {text}"))
                .collect()
        })
        .unwrap_or_default()
}

/// One survey round's questions, sent to the UI and awaited: park on
/// `reply_rx` until the user answers (or the job is stopped and the channel
/// drops). Returns the user's trimmed reply, or `None` when the channel
/// closed.
async fn await_survey_reply(
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    reply_rx: &mut mpsc::UnboundedReceiver<String>,
    questions: &[String],
    round: u8,
) -> Option<String> {
    let _ = tx.send((
        ids.0.clone(),
        ids.1.clone(),
        ids.2.clone(),
        ResearchUpdate::SurveyReady {
            questions: questions.to_vec(),
            round,
        },
    ));
    reply_rx.recv().await.map(|r| r.trim().to_string())
}

/// The conversational survey: the scoping agent asks what the user wants
/// (1–3 rounds), the user answers in chat, and the agent declares the survey
/// complete once it has enough — no phrase-matching in app code. An empty
/// reply (Enter on an empty input) skips ahead. Request failures (auth,
/// rate limits, network), malformed agent output, and a closed reply channel
/// all propagate as `Err` — the survey is a promised phase of the
/// conversational flow, not an optional garnish, so it must not silently
/// skip and still report success. Returns the (questions, answer) rounds
/// for the planner's context.
#[allow(clippy::too_many_arguments)]
async fn run_user_survey(
    provider: &OpenRouter,
    model: &str,
    topic: &str,
    reply_rx: &mut mpsc::UnboundedReceiver<String>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> Result<Vec<(String, String)>, String> {
    let mut rounds: Vec<(String, String)> = Vec::new();
    let initial = complete_text(provider, model, survey_messages(topic, &[]))
        .await
        .map_err(|e| format!("survey agent failed: {e}"))?;
    let mut questions = parse_survey_reply(&initial);
    let mut raw = initial;
    let mut round: u8 = 1;
    loop {
        match questions {
            SurveyReply::Complete => return Ok(rounds),
            SurveyReply::Malformed => {
                return Err(format!(
                    "survey agent returned unusable output (raw reply: {raw:.200})"
                ));
            }
            SurveyReply::Questions(qs) if qs.is_empty() || round > MAX_SURVEY_ROUNDS => {
                return Ok(rounds);
            }
            SurveyReply::Questions(qs) => {
                let Some(reply) = await_survey_reply(tx, ids, reply_rx, &qs, round).await else {
                    // The reply channel closed — the job is being torn down
                    // (or a stop raced the parked gate). Don't keep planning
                    // as if the scoping happened.
                    return Err("survey cancelled — the reply channel closed".to_string());
                };
                if reply.is_empty() {
                    return Ok(rounds); // Empty Enter = skip the rest.
                }
                rounds.push((qs.join("\n"), reply));
                round += 1;
                if round > MAX_SURVEY_ROUNDS {
                    return Ok(rounds);
                }
                raw = complete_text(provider, model, survey_messages(topic, &rounds))
                    .await
                    .map_err(|e| format!("survey follow-up failed: {e}"))?;
                questions = parse_survey_reply(&raw);
            }
        }
    }
}

/// The plan-approval phase: present the plan, park for a chat reply, and fold
/// edits back in via the approval agent. An empty reply (Enter) or an
/// agent-recognized "approve" runs the questions as-is; edits are re-presented
/// once (`rework` cap) for a final approval. A second edit, a failed approval
/// call, malformed agent output, or a closed reply channel (job teardown
/// racing the parked gate) fails visibly (`Err`) — searchers never run on a
/// plan the user hasn't approved, and approval never fails open.
#[allow(clippy::too_many_arguments)]
async fn await_plan_approval(
    provider: &OpenRouter,
    model: &str,
    topic: &str,
    questions: &mut Vec<PlanQuestion>,
    reply_rx: &mut mpsc::UnboundedReceiver<String>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> Result<(), String> {
    let mut rework = false;
    loop {
        let _ = tx.send((
            ids.0.clone(),
            ids.1.clone(),
            ids.2.clone(),
            ResearchUpdate::PlanReady {
                questions: questions.clone(),
                rework,
            },
        ));
        let Some(reply) = reply_rx.recv().await else {
            // The reply channel closed without an approval (job teardown, or
            // a stop racing the parked gate). Fail visibly rather than
            // running searchers on a plan the user hasn't approved.
            return Err(
                "plan approval cancelled — the reply channel closed before the plan was approved"
                    .to_string(),
            );
        };
        if reply.trim().is_empty() {
            return Ok(()); // Enter on an empty input = approve.
        }
        let text = complete_text(
            provider,
            model,
            plan_approval_messages(topic, questions, &reply),
        )
        .await
        .map_err(|e| format!("plan approval agent failed: {e}"))?;
        match parse_approval(&text) {
            Approval::Approved => return Ok(()),
            Approval::Revised(revised) if !revised.is_empty() => {
                if rework {
                    // Second edit: never silently folded in and run. The
                    // rework cap is one re-presentation — fail visibly
                    // rather than execute an unapproved revision.
                    return Err(
                        "plan was revised twice — rework cap reached; re-run /research \
                         with the final plan"
                            .to_string(),
                    );
                }
                *questions = revised;
                rework = true;
            }
            Approval::Revised(_) | Approval::Malformed => {
                return Err(format!(
                    "plan approval agent returned an unusable verdict (raw reply: {text:.200})"
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_research_inner(
    research_provider: &OpenRouter,
    research_model: &str,
    escalation_provider: &OpenRouter,
    escalation_model: &str,
    embedding_provider: &OpenRouter,
    embedding_model: &str,
    topic: &str,
    reply_rx: Option<mpsc::UnboundedReceiver<String>>,
    mut steer_rx: mpsc::UnboundedReceiver<String>,
    db_path: &std::path::Path,
    toolbox: &Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> Result<String, String> {
    // Gather the planning context (local chunks + a web landscape survey)
    // concurrently with the conversational survey — the user answers while
    // the ground truth arrives, so the plan targets real gaps. The two
    // gatherers also run concurrently with each other.
    let gather_task = {
        let provider = embedding_provider.clone();
        let model = embedding_model.to_string();
        let db_path = db_path.to_path_buf();
        let space_id = ids.1.clone();
        let topic = topic.to_string();
        let research_provider = research_provider.clone();
        let research_model = research_model.to_string();
        let toolbox = toolbox.clone();
        let tx = tx.clone();
        let ids = ids.clone();
        tokio::spawn(async move {
            let known =
                async { local_known_chunks(&provider, &model, &db_path, &space_id, &topic).await };
            let survey = async {
                send_stage(
                    &tx,
                    &ids,
                    "web survey",
                    "working — mapping the topic, debates, evidence, and source landscape",
                );
                let survey_question = format!(
                    "Conduct a broad preliminary survey of this research topic before planning: {topic}. \
                     Identify the major concepts, current debates, useful source types, and important \
                     evidence gaps."
                );
                let survey = run_searcher(
                    &research_provider,
                    &research_model,
                    &survey_question,
                    &survey_question,
                    toolbox,
                    &tx,
                    &ids,
                    "web survey",
                    0,
                    1,
                )
                .await;
                persist_session_sources(&db_path, &ids.0, std::slice::from_ref(&survey));
                survey
            };
            let (known, survey) = tokio::join!(known, survey);
            (known, survey)
        })
    };
    // Abort-on-drop guard: dropping the JoinHandle alone would detach the
    // gather task instead of cancelling it, so web calls and DB writes could
    // keep running after the user stops research. Aborting the outer task
    // drops this handle, which cancels the gather task.
    let gather_guard = super::AbortOnDrop(gather_task.abort_handle());

    // Phase 1: the conversational survey (skipped entirely for `/research!`).
    // Failures propagate visibly — the survey is a promised phase, and a
    // silent skip would report success without the user's scoping input.
    let mut answers: Vec<(String, String)> = Vec::new();
    let mut reply_rx = reply_rx;
    if let Some(rx) = reply_rx.as_mut() {
        answers = run_user_survey(research_provider, research_model, topic, rx, tx, ids)
            .await
            .map_err(|e| {
                send_stage(tx, ids, "survey", format!("error — {e}"));
                e
            })?;
    }

    // Join the concurrent gathering and fold it into planning context. A
    // panic (or cancellation) inside the gather task terminates the job with
    // context — defaulting to empty context would mask a programming failure
    // and let the pipeline report success on invented ground truth.
    let (known, web_survey) = match gather_task.await {
        Ok(v) => v,
        Err(e) if e.is_panic() => return Err(format!("context gathering panicked: {e}")),
        Err(e) => return Err(format!("context gathering was cancelled: {e}")),
    };
    drop(gather_guard);
    let mut planning_context = known;
    if !web_survey.is_empty() && !web_survey.starts_with('[') {
        planning_context.push(format!("Preliminary web survey:\n{web_survey}"));
        send_stage(
            tx,
            ids,
            "web survey",
            "done — landscape mapped for planning",
        );
    } else {
        send_stage(
            tx,
            ids,
            "web survey",
            "error — survey failed; planning from local context only",
        );
    }

    send_stage(
        tx,
        ids,
        "planner",
        "working — decomposing the surveyed landscape into focused questions",
    );
    let mut questions = match plan(
        research_provider,
        research_model,
        topic,
        &answers,
        &planning_context,
    )
    .await
    {
        Ok(questions) => questions,
        Err(e) => {
            send_stage(tx, ids, "planner", format!("error — {e}"));
            return Err(e);
        }
    };
    send_stage(
        tx,
        ids,
        "planner",
        format!("done — proposed {} questions", questions.len()),
    );

    // Phase 2: plan approval — reply in chat to approve or change it. The
    // gate parks with no timeout; Ctrl+↑ then Ctrl+X (the live view's stop)
    // is the escape hatch. Skipped entirely for `/research!`. Approval is
    // fail-closed: any failure here returns `Err` and research stops rather
    // than running searchers on an unapproved plan.
    if let Some(rx) = reply_rx.as_mut() {
        await_plan_approval(
            research_provider,
            research_model,
            topic,
            &mut questions,
            rx,
            tx,
            ids,
        )
        .await?;
    }

    let pinned = rusqlite::Connection::open(db_path)
        .ok()
        .and_then(|conn| crate::db::pinned_urls(&conn, &ids.0).ok())
        .unwrap_or_default();

    let mut findings: Vec<String> = if !web_survey.is_empty() && !web_survey.starts_with('[') {
        // Successful web survey: seed it into the findings so synthesis can
        // cite the landscape overview alongside each answer's own citations.
        vec![format!("--- Survey overview ---\n{web_survey}")]
    } else {
        Vec::new()
    };
    // Each searcher gets the full question block as its prompt (detail is
    // functional) but only the bare question as its live display label, so
    // the activity rows stay short.
    let searcher_items: Vec<(String, String)> = questions
        .iter()
        .map(|q| (q.prompt(topic), q.question.clone()))
        .collect();
    findings.extend(
        run_searchers(
            research_provider,
            research_model,
            toolbox,
            &searcher_items,
            tx,
            ids,
            "round 1",
        )
        .await,
    );
    persist_session_sources(db_path, &ids.0, &findings);

    // One stage row per drained steer, keyed by a job-global sequence number
    // (`steer #N` — N = the steer's 1-based queue position, which equals its
    // drain order). The stage upsert matches rows by label, so the key must
    // never be user text: duplicate, prefix-of-each-other, or LIKE-wildcard
    // (`%`/`_`) steer text would collapse or hijack rows and leave picked-up
    // steers looking queued (the live popup derives picked-up from the same
    // numbered keys).
    let mut steer_seq: usize = 0;
    let steers = drain_steers(&mut steer_rx).await;
    if !steers.is_empty() {
        let steer_items: Vec<(String, String)> =
            steers.iter().map(|s| (s.clone(), s.clone())).collect();
        for s in &steers {
            steer_seq += 1;
            send_stage(tx, ids, format!("steer #{steer_seq}"), s.clone());
        }
        let steered = run_searchers(
            research_provider,
            research_model,
            toolbox,
            &steer_items,
            tx,
            ids,
            "round 1 steer",
        )
        .await;
        persist_session_sources(db_path, &ids.0, &steered);
        findings.extend(steered);
    }

    send_stage(
        tx,
        ids,
        "synthesizer",
        format!("working — combining {} agent findings", findings.len()),
    );
    let mut draft = complete_agent(
        research_provider,
        research_model,
        synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings), &pinned),
        tx,
        ids,
        "synthesizer",
    )
    .await?;
    send_stage(tx, ids, "synthesizer", "done — draft assembled");

    send_stage(
        tx,
        ids,
        "critic",
        "working — checking coverage and contradictions",
    );
    let mut critique = parse_critique(
        &complete_agent(
            research_provider,
            research_model,
            critic_messages(topic, &draft),
            tx,
            ids,
            "critic",
        )
        .await?,
    );
    let critic_detail = match &critique {
        Critique::Satisfied => "done — draft is sufficiently complete".to_string(),
        // Quick win: surface the actual gap questions, not just a count — the
        // follow-up searchers are about to investigate exactly these.
        Critique::Gaps(gaps) => {
            let list = gaps
                .iter()
                .enumerate()
                .map(|(i, g)| format!("{}. {g}", i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            format!("done — found {} coverage gaps:\n{list}", gaps.len())
        }
        Critique::Contradiction(_) => "done — found a source contradiction".to_string(),
    };
    send_stage(tx, ids, "critic", critic_detail);

    if let Critique::Gaps(gaps) = &critique {
        let more = run_searchers(
            research_provider,
            research_model,
            toolbox,
            &gaps
                .iter()
                .map(|g| (g.clone(), g.clone()))
                .collect::<Vec<_>>(),
            tx,
            ids,
            "round 2",
        )
        .await;
        persist_session_sources(db_path, &ids.0, &more);
        findings.extend(more);

        let steers = drain_steers(&mut steer_rx).await;
        if !steers.is_empty() {
            let steer_items: Vec<(String, String)> =
                steers.iter().map(|s| (s.clone(), s.clone())).collect();
            for s in &steers {
                steer_seq += 1;
                send_stage(tx, ids, format!("steer #{steer_seq}"), s.clone());
            }
            let steered = run_searchers(
                research_provider,
                research_model,
                toolbox,
                &steer_items,
                tx,
                ids,
                "round 2 steer",
            )
            .await;
            persist_session_sources(db_path, &ids.0, &steered);
            findings.extend(steered);
        }

        send_stage(
            tx,
            ids,
            "synthesizer r2",
            "working — merging follow-up findings",
        );
        draft = complete_agent(
            research_provider,
            research_model,
            synthesizer_messages(topic, &crate::tools::dedup_source_lines(&findings), &pinned),
            tx,
            ids,
            "synthesizer r2",
        )
        .await?;
        send_stage(tx, ids, "synthesizer r2", "done — revised draft assembled");
        send_stage(
            tx,
            ids,
            "critic r2",
            "working — reviewing the revised draft",
        );
        critique = parse_critique(
            &complete_agent(
                research_provider,
                research_model,
                critic_messages(topic, &draft),
                tx,
                ids,
                "critic r2",
            )
            .await?,
        );
        let detail = match &critique {
            Critique::Satisfied => "done — revised draft is complete".to_string(),
            // Same shape as round 1: the remaining gap questions, not a count.
            Critique::Gaps(gaps) => {
                let list = gaps
                    .iter()
                    .enumerate()
                    .map(|(i, g)| format!("{}. {g}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("done — {} gaps remain:\n{list}", gaps.len())
            }
            Critique::Contradiction(_) => "done — contradiction remains".to_string(),
        };
        send_stage(tx, ids, "critic r2", detail);
    }

    if let Critique::Contradiction(desc) = &critique {
        send_stage(
            tx,
            ids,
            "resolver",
            "working — reconciling conflicting source claims",
        );
        let resolution = complete_agent(
            escalation_provider,
            escalation_model,
            escalation_messages(topic, &draft, &findings, desc),
            tx,
            ids,
            "resolver",
        )
        .await?;
        draft.push_str("\n\n");
        draft.push_str(&resolution);
        send_stage(tx, ids, "resolver", "done — contradiction reconciled");
    }

    send_stage(
        tx,
        ids,
        "verifier",
        "working — checking claims, citations, and direct quotes",
    );
    let verify_toolbox = Arc::new(
        ToolBox::research(
            None,
            None,
            "auto".to_string(),
            Vec::new(),
            Some(db_path.to_path_buf()),
        )
        .cache_only(),
    );
    let verified_raw = verify_with_quote_check(
        research_provider,
        research_model,
        verifier_messages(topic, &draft, &findings),
        verify_toolbox,
        tx,
        ids,
    )
    .await;
    let verified = if verified_raw.trim().is_empty() {
        draft.clone()
    } else {
        verified_raw
    };

    send_stage(
        tx,
        ids,
        "writer",
        "working — polishing structure and citations",
    );
    match complete_text(
        research_provider,
        research_model,
        writer_messages(topic, &verified, &pinned),
    )
    .await
    {
        Ok(report) => {
            send_stage(tx, ids, "writer", "done — final report ready");
            Ok(report)
        }
        Err(e) => {
            send_stage(tx, ids, "writer", format!("error — {e}"));
            Err(e)
        }
    }
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

    /// `/steer <text>`: queue an extra instruction for the running research
    /// job, picked up at the next round boundary. No-op with a status message
    /// if no research job is running.
    pub(crate) fn steer_research(&mut self, text: &str) {
        if text.is_empty() {
            self.status = "usage: /steer <what to also look into>".to_string();
            return;
        }
        match &self.research_steer_tx {
            // Hard bound: refuse once the queue is full — this also bounds
            // the unbounded channel and the retained log.
            Some(_) if self.research_steer_log.len() >= MAX_QUEUED_STEERS => {
                self.status = format!(
                    "steer queue full ({MAX_QUEUED_STEERS} pending) — wait for the next round"
                );
            }
            Some(tx) if tx.send(text.to_string()).is_ok() => {
                // Keep a log so the live popup can show what's queued vs.
                // already picked up by the pipeline. Entries carry their
                // 1-based queue position (positions are never renumbered, so
                // acknowledged entries can be dropped without shifting the
                // popup's view), and entries the pipeline has drained are
                // removed here — a long job can't retain unbounded steer
                // text without backpressure.
                let pos = self
                    .research_steer_acked
                    .iter()
                    .chain(self.research_steer_log.iter().map(|(p, _)| p))
                    .max()
                    .map_or(0, |&p| p)
                    + 1;
                self.research_steer_log.push((pos, text.to_string()));
                self.research_steer_log
                    .retain(|(p, _)| !self.research_steer_acked.contains(p));
                self.status = format!("queued steer: {text}");
            }
            _ => self.status = "no research job is running".to_string(),
        }
    }

    /// `/research` with no topic: distill one from the last ~20 chat turns
    /// (one cheap completion, same background-channel shape as
    /// `maybe_generate_title`) then hand it to `start_research_with_gate`
    /// exactly as if it had been typed — the existing plan-approval gate
    /// still lets you bail or edit before searchers run.
    pub(crate) fn start_research_from_chat(&mut self) {
        if self.research_topic_rx.is_some() {
            self.status = "already scoping a topic from this chat…".to_string();
            return;
        }
        if self.research_rx.is_some() {
            self.status = "a research job is already running".to_string();
            return;
        }
        let Some(model) = self.current_model.clone() else {
            self.status = "no model configured — set one in /login or /model".to_string();
            return;
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.status = format!("model backend unavailable: {model} — pick another with /model");
            return;
        };
        let convo: String = self
            .messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|m| {
                format!(
                    "{}: {}",
                    m.role,
                    m.content.chars().take(500).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if convo.trim().is_empty() {
            self.status = "nothing to scope yet — chat first, or use /research <topic>".to_string();
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.research_topic_rx = Some(rx);
        self.status = "scoping a research topic from this chat…".to_string();
        tokio::spawn(async move {
            let prompt = format!(
                "Based on this conversation, reply with ONLY a single-line research topic \
                 or question suitable for a multi-source research task. No preamble, no \
                 quotes, no markdown.\n\n{convo}"
            );
            let msgs = vec![ChatMessage::text("user", prompt)];
            let result = provider
                .complete(&raw_model, msgs)
                .await
                .map(|s| s.trim().to_string())
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// The topic-distillation job finished: start research with it, or
    /// report the failure. `None` = channel closed without a result.
    pub fn on_research_topic_derived(&mut self, r: Option<Result<String, String>>) {
        self.research_topic_rx = None;
        let Some(result) = r else { return };
        match result {
            Ok(topic) if !topic.is_empty() => self.start_research_with_gate(&topic, true),
            Ok(_) => self.status = "couldn't derive a topic — try /research <topic>".to_string(),
            Err(e) => self.status = format!("topic scoping failed: {e}"),
        }
    }

    /// Ctrl+↑: open the live per-searcher activity view. Caller already
    /// gates this on a research job running.
    pub(crate) fn open_research_live(&mut self) {
        self.research_live_input.clear();
        self.popup = super::Popup::ResearchLive;
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
        let research_model = self.research_model.trim().to_string();
        let Some((provider, raw_research_model)) = self.resolve_model_backend(&research_model)
        else {
            self.open_login_popup();
            return;
        };
        let escalation_model = if self.escalation_model.trim().is_empty() {
            research_model.clone()
        } else {
            self.escalation_model.trim().to_string()
        };
        let title = super::chat::title_from(&topic);
        // Hygiene: no gate or reply channel from a previous job may linger.
        self.survey_gate = None;
        self.survey_reply_tx = None;

        // Check if there's a conversation to migrate to the research session.
        let parent_id = self.session.as_ref().and_then(|s| {
            let has_content = self
                .messages
                .iter()
                .any(|m| m.role == "user" || m.role == "assistant");
            if has_content {
                Some(s.id.clone())
            } else {
                None
            }
        });
        let parent_title = parent_id
            .as_ref()
            .and_then(|pid| self.db.get_session(pid).ok()?.map(|s| s.title));

        let session =
            match self
                .db
                .create_session(&title, &research_model, &self.active_space.id, "research")
            {
                Ok(s) => s,
                Err(e) => {
                    self.status = format!("could not start research session: {e}");
                    return;
                }
            };

        if let Some(ref pid) = parent_id {
            let _ = self.db.set_research_parent(&session.id, pid);

            // Build compacted context from the original conversation
            let compact_summary = self
                .session
                .as_ref()
                .and_then(|s| s.compact_summary.clone());
            let compact_through = self
                .session
                .as_ref()
                .map(|s| s.compact_through as usize)
                .unwrap_or(0);
            let mut ctx = String::new();
            if let Some(ref summary) = compact_summary {
                ctx.push_str("Previous conversation summary:\n");
                ctx.push_str(summary);
            }
            let tail: Vec<&crate::db::Message> = self.messages[compact_through..]
                .iter()
                .filter(|m| m.role == "user" || m.role == "assistant")
                .collect();
            if !tail.is_empty() {
                if !ctx.is_empty() {
                    ctx.push_str("\n\n");
                }
                ctx.push_str("Recent messages:\n");
                for m in tail {
                    let t = m.content.chars().take(300).collect::<String>();
                    ctx.push_str(&format!("{}: {t}\n", m.role));
                }
            }
            let msg = if ctx.is_empty() {
                format!("/research {topic}")
            } else {
                format!("/research {topic}\n\n{ctx}")
            };
            let _ = self.db.add_user_message(&session.id, &msg);

            // Link message in the original session
            let _ = self.db.insert_message(
                pid,
                "session_link",
                &format!("{}\n🔗 Research session started for: {topic}", session.id),
                None,
                None,
                None,
                None,
                None,
            );

            // Link message in the research session back to the original
            let back_title = parent_title.as_deref().unwrap_or("previous chat");
            let _ = self.db.insert_message(
                &session.id,
                "session_link",
                &format!("{pid}\n↩ Originally from: {back_title}"),
                None,
                None,
                None,
                None,
                None,
            );

            self.messages = self.db.load_messages(&session.id).unwrap_or_default();
        } else {
            let _ = self
                .db
                .add_user_message(&session.id, &format!("/research {topic}"));
            self.messages = self.db.load_messages(&session.id).unwrap_or_default();
        }

        let searxng_url =
            (!self.searxng_url.trim().is_empty()).then(|| self.searxng_url.trim().to_string());
        let langsearch_key = (!self.langsearch_key.trim().is_empty())
            .then(|| self.langsearch_key.trim().to_string());
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
        self.status = format!("researching: {topic} · Ctrl+↑ agents");

        let (steer_tx, steer_rx) = mpsc::unbounded_channel();
        self.research_steer_tx = Some(steer_tx);
        self.research_steer_log.clear();
        self.research_steer_acked.clear();
        self.research_stage_rows.clear();
        // Capture the mode at job start: plan-message and artifact
        // persistence follow this, never a mid-job incognito toggle.
        self.research_incognito = self.incognito;

        // The conversational gates (survey + plan approval) ride one reply
        // channel: the pipeline parks on `reply_rx`, the App arms a gate on
        // each SurveyReady/PlanReady. Ungated (`/research!`, watches) skips
        // both phases entirely.
        let reply_rx = if gated {
            let (reply_tx, reply_rx) = mpsc::unbounded_channel();
            self.survey_reply_tx = Some(reply_tx);
            Some(reply_rx)
        } else {
            None
        };

        let space_id = self.active_space.id.clone();
        let space_name = self.active_space.name.clone();
        self.session = Some(session.clone());
        self.context_total = None;
        self.scroll = 0;
        self.refresh_toolbox();

        let (escalation_provider, raw_escalation_model) = self
            .resolve_model_backend(&escalation_model)
            .unwrap_or_else(|| (provider.clone(), escalation_model.clone()));
        let embedding_model = self.embedding_model.trim().to_string();
        let (embedding_provider, raw_embedding_model) = self
            .resolve_model_backend(&embedding_model)
            .unwrap_or_else(|| (provider.clone(), embedding_model.clone()));

        let task = tokio::spawn(run_research(crate::app::research::ResearchOptions {
            research_provider: provider,
            research_model: raw_research_model,
            escalation_provider,
            escalation_model: raw_escalation_model,
            embedding_provider,
            embedding_model: raw_embedding_model,
            db_path: self.space.db_path(),
            topic,
            reply_rx,
            steer_rx,
            toolbox,
            tx,
            session_id: session.id,
            space_id,
            space_name,
        }));
        self.research_abort = Some(task.abort_handle());
    }

    /// Abort the active research pipeline, including survey/searcher/tool
    /// streams spawned under its orchestration task.
    pub(crate) fn stop_research(&mut self) {
        self.survey_gate = None;
        if let Some(abort) = self.research_abort.take() {
            abort.abort();
        }
        if self.research_rx.take().is_some() {
            if let Some((session_id, _)) = self.research_running.take() {
                let _ = self.db.upsert_research_stage_message(
                    &session_id,
                    "research",
                    "stopped by user",
                );
            }
            self.survey_gate = None;
            self.survey_reply_tx = None;
            self.research_steer_tx = None;
            // Retained steer state belongs to the job: drop it so a long
            // session can't keep unbounded text after research stops.
            self.research_steer_log.clear();
            self.research_steer_acked.clear();
            self.status = "research stopped".to_string();
            self.popup = super::Popup::None;
        } else {
            self.status = "no research job is running".to_string();
        }
    }

    /// Whether the survey gate (clarifying questions or plan approval) is
    /// armed for the currently viewed session — the only case where Enter is
    /// intercepted and routed to the pipeline instead of a normal chat send.
    /// A gate in another session must never swallow typing (the old
    /// cross-session hijack).
    pub(crate) fn survey_gate_targets_current_session(&self) -> bool {
        self.survey_gate
            .as_ref()
            .is_some_and(|g| self.session.as_ref().is_some_and(|s| s.id == g.session_id))
    }

    /// Restore an actionable gate row after loading its session. Normal jobs
    /// already load the persisted row, while incognito jobs recover it from
    /// `SurveyGate` without writing private content to the database.
    pub(crate) fn restore_survey_gate_prompt(&mut self) {
        let pending = self.survey_gate.as_ref().and_then(|gate| {
            self.session
                .as_ref()
                .filter(|session| session.id == gate.session_id)
                .map(|_| (gate.prompt_role.clone(), gate.prompt_content.clone()))
        });
        let Some((role, content)) = pending else {
            return;
        };
        if self
            .messages
            .iter()
            .any(|message| message.role == role && message.content == content)
        {
            return;
        }
        self.messages.push(crate::db::Message {
            role,
            content,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: None,
            persona: None,
        });
    }

    /// Route a chat reply into the parked survey gate (survey answer or plan
    /// approval/edit). Records the reply as a `gate_reply` in the session —
    /// it renders in the transcript like a user message but is never replayed
    /// to the model, since the survey/plan rows it answers are excluded too:
    /// a bare "the second option" or "drop Q2" must not leak into model
    /// history without its context.
    pub(crate) fn reply_to_survey_gate(&mut self, text: &str) {
        let Some(gate) = self.survey_gate.take() else {
            return;
        };
        // Persist before acknowledging: the pipeline and the transcript must
        // never incorporate a reply the database didn't record (a locked or
        // full db would otherwise silently lose the answer on reload). On a
        // persistence failure the gate stays armed and the composer is
        // restored so the user can retry.
        let saved_id = if !text.trim().is_empty() && !self.research_incognito {
            match self.db.add_gate_reply_message(&gate.session_id, text) {
                Ok(id) => Some(id),
                Err(e) => {
                    self.survey_gate = Some(gate);
                    self.set_input(text);
                    self.status = format!("couldn't save your reply — {e}");
                    return;
                }
            }
        } else {
            None
        };
        if gate.reply_tx.send(text.to_string()).is_err() {
            // Delivery failed: roll back the persisted reply so a retry
            // can't duplicate it in the transcript, then put the text back
            // in the composer rather than eating the user's typing.
            let rollback_error = saved_id.and_then(|id| self.db.delete_message(&id).err());
            self.set_input(text);
            self.status = match rollback_error {
                Some(e) => format!(
                    "the job stopped waiting and the saved reply could not be rolled back: {e} — text restored to the composer"
                ),
                None => "the job is no longer waiting for a reply — text restored to the composer"
                    .to_string(),
            };
            return;
        }
        if !text.trim().is_empty()
            && self
                .session
                .as_ref()
                .is_some_and(|s| s.id == gate.session_id)
        {
            self.messages.push(crate::db::Message {
                role: "gate_reply".to_string(),
                content: text.to_string(),
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                phrase: None,
                persona: None,
            });
        }
        match gate.phase {
            SurveyPhase::Clarify { round } => {
                self.status = format!(
                    "answer noted (round {round}) — checking for follow-ups… · Ctrl+↑ agents"
                )
            }
            SurveyPhase::Approve { rework } => {
                self.status = if rework {
                    "revision folded in — continuing… · Ctrl+↑ agents".to_string()
                } else {
                    "plan reply sent — continuing… · Ctrl+↑ agents".to_string()
                }
            }
        }
    }

    /// Persist a stage row and keep both in-memory views in sync: the
    /// viewed transcript (`self.messages`, only when the job's session is
    /// viewed) and the job's stage-row mirror (`research_stage_rows`, always
    /// — the live popup renders from it without a db read per frame). One
    /// row per label, updated in place. Also used for error rows (plan
    /// record / report file) so a save failure is visible immediately,
    /// not only after a reload.
    fn mirror_stage(&mut self, session_id: &str, label: &str, detail: &str) {
        let _ = self
            .db
            .upsert_research_stage_message(session_id, label, detail);
        let text = crate::db::stage_content(label, detail);
        let prefix = format!("{label}:");
        // Job-level mirror: the live popup's single source of truth.
        if let Some(row) = self
            .research_stage_rows
            .iter_mut()
            .rev()
            .find(|c| c.as_str() == label || c.starts_with(prefix.as_str()))
        {
            *row = text.clone();
        } else {
            self.research_stage_rows.push(text.clone());
        }
        if self.session.as_ref().is_some_and(|s| s.id == session_id) {
            if let Some(row) = self.messages.iter_mut().rev().find(|m| {
                m.role == "research_stage" && (m.content == label || m.content.starts_with(&prefix))
            }) {
                row.content = text.clone();
                // Stage rows update in place, so message count does not
                // change and the wrapped transcript cache would otherwise
                // keep rendering stale progress.
                self.invalidate_history_cache();
            } else {
                self.messages.push(crate::db::Message {
                    role: "research_stage".to_string(),
                    content: text,
                    model: None,
                    reasoning: None,
                    tokens: None,
                    secs: None,
                    phrase: None,
                    persona: None,
                });
            }
        }
    }

    /// A research pipeline update: a stage label, or the final report/error.
    /// `None` = the job's channel closed (fires once, right after `Done`).
    pub fn on_research_done(&mut self, r: Option<ResearchMsg>) {
        let Some((session_id, space_id, space_name, update)) = r else {
            self.research_rx = None;
            self.research_abort = None;
            self.research_running = None;
            self.survey_gate = None;
            self.survey_reply_tx = None;
            self.research_steer_tx = None;
            // Retained steer state belongs to the job: drop it when the job
            // ends so the next job starts from an empty queue view.
            self.research_steer_log.clear();
            self.research_steer_acked.clear();
            self.research_stage_rows.clear();
            self.research_incognito = false;
            self.research_live_input.clear();
            if self.popup == super::Popup::ResearchLive {
                self.popup = super::Popup::None;
            }
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            ResearchUpdate::Stage { label, detail } => {
                // A `steer #N` stage row means the pipeline drained that
                // steer: record the acknowledgment and prune the retained
                // log immediately — acknowledged text must not linger in
                // memory while the job is parked.
                if let Some(n) = label.strip_prefix("steer #").and_then(|n| n.parse().ok()) {
                    self.research_steer_acked.insert(n);
                    self.research_steer_log
                        .retain(|(p, _)| !self.research_steer_acked.contains(p));
                }
                self.mirror_stage(&session_id, &label, &detail);
                if viewing {
                    self.status = format!(
                        "research: {} · Ctrl+↑ agents",
                        crate::db::stage_content(&label, &detail)
                    );
                }
            }
            ResearchUpdate::SurveyReady { questions, round } => {
                let topic = self
                    .research_running
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                let header = if round <= 1 {
                    format!("For \"{topic}\":")
                } else {
                    format!("Follow-up (round {round} of {MAX_SURVEY_ROUNDS}) for \"{topic}\":")
                };
                let qs = questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| format!(" {}. {q}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                let content = format!(
                    "{header}\n{qs}\n\nAnswer in chat — I may ask follow-ups (up to {MAX_SURVEY_ROUNDS} rounds), \
                     then say \"I approve\". (Enter on an empty input skips ahead.)"
                );

                // A normal job must make the prompt durable before it can
                // intercept Enter. Incognito deliberately keeps it only in
                // SurveyGate, where session loading can restore it in memory.
                if !self.research_incognito
                    && let Err(e) = self.db.add_survey_message(&session_id, &content)
                {
                    self.stop_research();
                    self.status = format!("couldn't persist the survey — research stopped: {e}");
                    return;
                }
                let Some(tx) = self.survey_reply_tx.clone() else {
                    self.stop_research();
                    self.status = "survey reply channel unavailable — research stopped".to_string();
                    return;
                };
                self.survey_gate = Some(SurveyGate {
                    session_id: session_id.clone(),
                    reply_tx: tx,
                    phase: SurveyPhase::Clarify { round },
                    prompt_role: "survey".to_string(),
                    prompt_content: content.clone(),
                });

                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "survey".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        persona: None,
                    });
                    self.status = format!("survey round {round} — answer in chat · Ctrl+↑ agents");
                } else {
                    // The gate is parked off-screen: mark the job's session
                    // unread and say where input is needed. In incognito the
                    // prompt will be restored from SurveyGate when opened.
                    self.unread.insert(session_id.clone());
                    self.status = format!(
                        "research is waiting on you — survey round {round} for \"{topic}\": \
                         open that session and answer in chat"
                    );
                }
            }
            ResearchUpdate::PlanReady { questions, rework } => {
                let topic = self
                    .research_running
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                let plan = plan_text(&questions);
                let heading = if rework {
                    "Research plan (revised with your feedback) — reply \"approve\" to continue:"
                } else {
                    "Research plan — reply to approve, or tell me what to change (\"drop Q2\", \"also look into X\"):"
                };
                let content = format!("{heading}\n{plan}");

                // As with survey prompts, normal jobs persist before arming;
                // incognito jobs retain the actionable row only in the gate.
                if !self.research_incognito
                    && let Err(e) = self.db.add_research_plan_message(&session_id, &content)
                {
                    self.stop_research();
                    self.status = format!("couldn't persist the plan — research stopped: {e}");
                    return;
                }
                let Some(tx) = self.survey_reply_tx.clone() else {
                    self.stop_research();
                    self.status =
                        "plan approval channel unavailable — research stopped".to_string();
                    return;
                };
                self.survey_gate = Some(SurveyGate {
                    session_id: session_id.clone(),
                    reply_tx: tx,
                    phase: SurveyPhase::Approve { rework },
                    prompt_role: "research_plan".to_string(),
                    prompt_content: content.clone(),
                });

                // A byproduct record in the space's files, like the report:
                // the conversation is the edit surface, the file is history.
                // Skipped entirely in incognito — the plan folds in the user's
                // survey replies, so "nothing persists" must not leave it on
                // disk even if the job stops before any report. Failures
                // surface as a transcript stage row instead of vanishing.
                if let Err(e) = self.save_space_artifact(
                    &space_id,
                    &space_name,
                    &topic,
                    "plan",
                    &format!("# Research plan: {topic}\n\n{plan}\n"),
                ) {
                    self.mirror_stage(
                        &session_id,
                        "plan record",
                        &format!("error — could not save plan record: {e}"),
                    );
                }
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "research_plan".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        persona: None,
                    });
                    self.status = if rework {
                        "revised plan ready — reply to approve".to_string()
                    } else {
                        "research plan ready — reply to approve or change · Ctrl+↑ agents"
                            .to_string()
                    };
                } else {
                    // The gate is parked off-screen: mark the job's session
                    // unread and say where input is needed. An incognito plan
                    // is restored from SurveyGate rather than the database.
                    self.unread.insert(session_id.clone());
                    self.status = format!(
                        "research is waiting on you — plan approval for \"{topic}\": \
                         open that session and reply \"approve\""
                    );
                }
            }
            ResearchUpdate::Done(Ok(report)) => {
                // A watch session with a prior run gets a "what changed"
                // section prepended, listing sources not cited last time.
                let report = if let Ok(Some(prev_citations)) =
                    self.previous_citations_for_watch_session(&session_id, &space_id)
                {
                    let new_sources =
                        crate::app::watches::new_sources_since(&report, &prev_citations);
                    format!(
                        "{}\n\n{}",
                        crate::app::watches::diff_section("", &report, &new_sources),
                        report
                    )
                } else {
                    report
                };
                let _ = self.db.add_assistant_message(
                    &session_id,
                    &report,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                let topic = self
                    .research_running
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                // Failures surface as a transcript stage row (mirrored into
                // the in-memory transcript and the live popup) instead of
                // vanishing (incognito skips the write by design).
                if let Err(e) = self.save_research_report(&space_id, &space_name, &topic, &report) {
                    self.mirror_stage(
                        &session_id,
                        "report file",
                        &format!("error — could not save report file: {e}"),
                    );
                }
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "assistant".to_string(),
                        content: report,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: Some("Researched".to_string()),
                        persona: None,
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
                let _ =
                    self.db
                        .add_assistant_message(&session_id, &msg, None, None, None, None, None);
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "assistant".to_string(),
                        content: msg.clone(),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        persona: None,
                    });
                }
                self.status = msg;
            }
        }
    }

    /// Write a space artifact (finished report or presented plan) into the
    /// job's own space — not necessarily the currently active one, since the
    /// user may have switched spaces while the job ran — named
    /// `{prefix}-<slug>-<timestamp>.md`. Only refreshes the files cache /
    /// triggers a rescan if that space is still active; otherwise the file
    /// sits on disk and gets picked up next time that space's /files is
    /// opened, same as any externally-dropped file. Returns the written path
    /// so callers can surface failures at the update boundary instead of
    /// dropping them. Never writes in incognito mode: the plan folds in the
    /// user's survey replies, so "nothing persists" must not leave it on
    /// disk even if the job stops before any report.
    fn save_space_artifact(
        &mut self,
        space_id: &str,
        space_name: &str,
        topic: &str,
        prefix: &str,
        body: &str,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        // "Nothing persists" mode: no plan/report records on disk at all —
        // plan files incorporate survey replies, so even a job stopped
        // before its report must not leak user details. The mode is the one
        // captured when the job started, not a mid-job toggle.
        if self.research_incognito {
            return Ok(None);
        }
        let dir = self.space.files_dir(space_name);
        std::fs::create_dir_all(&dir)?;
        let slug = super::sessions::slugify(topic);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = format!("{prefix}-{slug}-{stamp}.md");
        let path = dir.join(&name);
        std::fs::write(&path, body)?;
        if space_id == self.active_space.id {
            self.rescan_files();
        }
        Ok(Some(path))
    }

    /// Save the finished report into the job's own space, named
    /// `research-<slug>-<timestamp>.md`, via the shared artifact writer,
    /// then index the report's cited sources for
    /// `research_lookup(scope=citations)`. Failures propagate to the caller
    /// (the `Done` handler surfaces them as a transcript stage row).
    fn save_research_report(
        &mut self,
        space_id: &str,
        space_name: &str,
        topic: &str,
        report: &str,
    ) -> std::io::Result<Option<std::path::PathBuf>> {
        let saved = self.save_space_artifact(space_id, space_name, topic, "research", report)?;
        if let Some(path) = &saved {
            // Index the report's cited sources for research_lookup(scope=citations).
            let citations = crate::citations::parse_citations(report);
            if !citations.is_empty() {
                // Titles aren't in the Sources-list format; index url only.
                let rows: Vec<(String, Option<String>)> =
                    citations.into_iter().map(|(_, url)| (url, None)).collect();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = self.db.add_citations(space_id, &name, &rows);
            }
        }
        Ok(saved)
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
        let root =
            std::env::temp_dir().join(format!("nexus-research-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("spaces")).unwrap();
        let space = Space { root };
        App::new(db, Some("k".into()), space)
    }

    #[tokio::test]
    async fn drain_steers_collects_all_queued_without_blocking() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("look into X".to_string()).unwrap();
        tx.send("also Y".to_string()).unwrap();
        let drained = drain_steers(&mut rx).await;
        assert_eq!(
            drained,
            vec!["look into X".to_string(), "also Y".to_string()]
        );
        // Second call with nothing queued returns empty immediately (no hang).
        let empty = drain_steers(&mut rx).await;
        assert!(empty.is_empty());
    }

    #[test]
    fn planner_messages_with_context_includes_known_chunks_as_gap_guidance() {
        let msgs = planner_messages_with_context(
            "rust async runtimes",
            &[],
            &["Rust's async model uses a Future trait.".to_string()],
        );
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[1].content.contains("rust async runtimes"));
        assert!(msgs[1].content.contains("Already known"));
        assert!(msgs[1].content.contains("Future trait"));
    }

    #[test]
    fn planner_messages_with_context_falls_back_to_plain_prompt_when_empty() {
        let msgs = planner_messages_with_context("topic", &[], &[]);
        assert!(!msgs[1].content.contains("Already known"));
        assert_eq!(msgs[1].content, "topic");
    }

    #[test]
    fn planner_messages_with_context_folds_user_answers_into_the_prompt() {
        let msgs = planner_messages_with_context(
            "topic",
            &[
                ("q1".to_string(), "depth first".to_string()),
                ("q2".to_string(), "current state only".to_string()),
            ],
            &[],
        );
        let user = &msgs[1].content;
        assert!(user.contains("answered clarifying questions"));
        assert!(user.contains("depth first"));
        assert!(user.contains("current state only"));
        assert!(user.contains("topic"));
    }

    #[test]
    fn verifier_prompt_mentions_quote_checking() {
        assert!(VERIFIER_PROMPT.to_lowercase().contains("quote"));
    }

    #[test]
    fn parse_plan_blocks_reads_json_objects_with_all_fields() {
        let qs = parse_plan_blocks(
            r#"[{"question":"what is X","why":"definitions matter","angles":["a1","a2"],"sources":["s1","s2"]}]"#,
        );
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].question, "what is X");
        assert_eq!(qs[0].why, "definitions matter");
        assert_eq!(qs[0].angles, vec!["a1".to_string(), "a2".to_string()]);
        assert_eq!(qs[0].sources, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[test]
    fn parse_plan_blocks_defaults_missing_fields_and_strips_fences() {
        let qs = parse_plan_blocks("```json\n[{\"question\": \"what is X\"}]\n```");
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].question, "what is X");
        assert!(qs[0].why.is_empty());
        assert!(qs[0].angles.is_empty());
        assert!(qs[0].sources.is_empty());
    }

    #[test]
    fn parse_plan_blocks_falls_back_to_bare_questions_on_non_json() {
        let qs = parse_plan_blocks("- what is X\n2. how does Y work");
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].question, "what is X");
        assert!(qs[0].why.is_empty());
        assert_eq!(qs[1].question, "how does Y work");
    }

    #[test]
    fn parse_plan_blocks_filters_empty_questions_and_caps_at_max() {
        let qs = parse_plan_blocks(
            r#"[{"question":""},{"question":"q1"},{"question":"q2"},{"question":"q3"}]"#,
        );
        assert_eq!(qs.len(), 3);
        assert_eq!(qs[0].question, "q1");
        let lines: Vec<String> = (0..10).map(|i| format!("q{i}")).collect();
        assert_eq!(parse_plan_blocks(&lines.join("\n")).len(), MAX_SUBQUESTIONS);
    }

    #[test]
    fn parse_plan_blocks_rejects_malformed_json_without_line_fallback() {
        // Structured JSON is unambiguous: malformed or unusable output must
        // fail planning — the raw JSON lines are never reinterpreted as bare
        // questions (`[{}]` must not become a plan whose question is
        // literally `[{}]`).
        assert!(parse_plan_blocks("[{}]").is_empty(), "[{{}}] must fail");
        assert!(
            parse_plan_blocks(r#"[{"question":""}]"#).is_empty(),
            "empty questions must fail"
        );
        assert!(
            parse_plan_blocks(r#"[{"question": 5}]"#).is_empty(),
            "wrong field types must fail"
        );
        assert!(
            parse_plan_blocks(r#"{"question":"q1"}"#).is_empty(),
            "a bare object is not the required array and must fail"
        );
        // Non-JSON legacy line output still falls back to bare questions.
        assert_eq!(parse_plan_blocks("- what is X").len(), 1);
        // JSON wrapped in model prose is still JSON — never re-read as lines.
        let prose = parse_plan_blocks("Here is the plan:\n[{\"question\":\"q1\"}]");
        assert_eq!(prose.len(), 1, "prose-prefixed JSON must parse as JSON");
        assert_eq!(prose[0].question, "q1");
        // A legacy JSON array of strings still works.
        let legacy = parse_plan_blocks(r#"["what is X", "how does Y work"]"#);
        assert_eq!(legacy.len(), 2);
        assert_eq!(legacy[0].question, "what is X");
    }

    #[test]
    fn parse_survey_reply_recognizes_complete_markers() {
        assert_eq!(parse_survey_reply("COMPLETE"), SurveyReply::Complete);
        assert_eq!(parse_survey_reply("  complete  "), SurveyReply::Complete);
        assert_eq!(
            parse_survey_reply("COMPLETE: I have enough"),
            SurveyReply::Complete
        );
        assert_eq!(
            parse_survey_reply("COMPLETE — proceed"),
            SurveyReply::Complete
        );
        // Trailing punctuation counts too — the docs promise prose tolerance.
        assert_eq!(parse_survey_reply("COMPLETE."), SurveyReply::Complete);
        assert_eq!(parse_survey_reply("COMPLETE!"), SurveyReply::Complete);
    }

    #[test]
    fn parse_survey_reply_reads_numbered_questions() {
        assert_eq!(
            parse_survey_reply("1. Depth or breadth?\n2. History too?"),
            SurveyReply::Questions(vec![
                "Depth or breadth?".to_string(),
                "History too?".to_string()
            ])
        );
        assert_eq!(
            parse_survey_reply("- just one angle"),
            SurveyReply::Questions(vec!["just one angle".to_string()])
        );
    }

    #[test]
    fn parse_survey_reply_marks_output_contract_violations_and_caps_questions() {
        // Empty output and arbitrary prose violate the agent's contract
        // (COMPLETE or numbered questions) — they must be `Malformed`, not
        // silently indistinguishable from the required COMPLETE marker.
        assert_eq!(parse_survey_reply(""), SurveyReply::Malformed);
        assert_eq!(parse_survey_reply("\n\n"), SurveyReply::Malformed);
        assert_eq!(
            parse_survey_reply("I couldn't understand your last answer, please retry"),
            SurveyReply::Malformed
        );
        assert_eq!(
            parse_survey_reply("The model encountered an error processing the request."),
            SurveyReply::Malformed
        );
        assert_eq!(
            parse_survey_reply("No further questions are needed"),
            SurveyReply::Malformed
        );
        // An unmarked line is only a question when it looks like one.
        assert_eq!(
            parse_survey_reply("Depth or breadth?"),
            SurveyReply::Questions(vec!["Depth or breadth?".to_string()])
        );
        // Numbered lines are questions; prose mixed in is skipped.
        let mixed = "1. Depth or breadth?\nPlease be specific.\n2. History too?";
        assert_eq!(
            parse_survey_reply(mixed),
            SurveyReply::Questions(vec![
                "Depth or breadth?".to_string(),
                "History too?".to_string()
            ])
        );
        let lines: Vec<String> = (0..8).map(|i| format!("{}. q{i}?", i + 1)).collect();
        match parse_survey_reply(&lines.join("\n")) {
            SurveyReply::Questions(qs) => assert_eq!(qs.len(), MAX_SURVEY_QUESTIONS),
            _ => panic!("expected questions"),
        }
    }

    #[test]
    fn parse_approval_recognizes_approved_and_revised_plans() {
        assert_eq!(parse_approval("APPROVED"), Approval::Approved);
        assert_eq!(parse_approval("  approved  "), Approval::Approved);
        assert_eq!(parse_approval("APPROVED: run it"), Approval::Approved);
        let revised = parse_approval("[{\"question\": \"revised q\"}]");
        assert_eq!(
            revised,
            Approval::Revised(vec![PlanQuestion::bare("revised q".to_string())])
        );
        // Malformed output is never treated as approval — the phase fails
        // visibly instead of running an unapproved plan.
        assert_eq!(parse_approval("huh?"), Approval::Malformed);
        assert_eq!(parse_approval(""), Approval::Malformed);
        assert_eq!(
            parse_approval("Here is the revised plan I prepared for you"),
            Approval::Malformed
        );
        // Structured JSON that parses but holds no usable questions, or that
        // has wrong field types, is Malformed — never re-read as bare lines.
        assert_eq!(parse_approval("[{}]"), Approval::Malformed);
        assert_eq!(parse_approval("[{\"question\": 5}]"), Approval::Malformed);
        // JSON wrapped in model prose is still JSON.
        assert_eq!(
            parse_approval("Here is my revised plan:\n[{\"question\":\"q\"}]\n"),
            Approval::Revised(vec![PlanQuestion::bare("q".to_string())])
        );
        // A recognizably list-formatted revision still counts.
        assert_eq!(
            parse_approval("- drop q2"),
            Approval::Revised(vec![PlanQuestion::bare("drop q2".to_string())])
        );
    }

    #[test]
    fn plan_question_prompt_includes_topic_and_full_brief() {
        let q = PlanQuestion {
            question: "how does X work".to_string(),
            why: "mechanism matters".to_string(),
            angles: vec!["internals".to_string(), "benchmarks".to_string()],
            sources: vec!["papers".to_string()],
        };
        let p = q.prompt("rust async");
        assert!(p.contains("rust async"));
        assert!(p.contains("how does X work"));
        assert!(p.contains("mechanism matters"));
        assert!(p.contains("internals; benchmarks"));
        assert!(p.contains("papers"));
        // A bare question stays prompt-safe too.
        assert!(PlanQuestion::bare("q".into()).prompt("t").contains("q"));
    }

    #[test]
    fn plan_text_renders_numbered_questions_with_indented_briefs() {
        let qs = vec![
            PlanQuestion::bare("q1".to_string()),
            PlanQuestion {
                question: "q2".to_string(),
                why: "why2".to_string(),
                angles: vec!["a".to_string()],
                sources: vec!["s".to_string()],
            },
        ];
        let t = plan_text(&qs);
        assert!(t.contains("1. q1"));
        assert!(t.contains("2. q2"));
        assert!(t.contains("\n   Why: why2"));
        assert!(t.contains("\n   Angles: a"));
        assert!(t.contains("\n   Sources: s"));
    }

    #[test]
    fn survey_messages_include_topic_and_rounds() {
        let msgs = survey_messages("t", &[]);
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[1].content.contains("t"));
        let msgs = survey_messages("t", &[("q".to_string(), "a".to_string())]);
        assert!(msgs[1].content.contains("q"));
        assert!(msgs[1].content.contains("a"));
    }

    #[test]
    fn plan_approval_messages_include_plan_and_user_reply() {
        let msgs =
            plan_approval_messages("topic", &[PlanQuestion::bare("q1".to_string())], "drop q2");
        assert!(msgs[1].content.contains("topic"));
        assert!(msgs[1].content.contains("1. q1"));
        assert!(msgs[1].content.contains("drop q2"));
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
                "# Report\n\nBody [1].\n\n## Sources\n1. https://example.com/a\n".to_string(),
            )),
        )));

        let hits =
            a.db.search_citations(&space_id, Some("example.com"))
                .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "https://example.com/a");
        // And a miss filter returns nothing.
        assert!(
            a.db.search_citations(&space_id, Some("nope.example"))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn plan_ready_arms_the_gate_and_reply_routes_into_the_pipeline() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        // The pipeline's reply sender must be reachable for the gate to arm.
        let (tx, mut rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);
        let q1 = PlanQuestion::bare("q1".to_string());
        let q2 = PlanQuestion::bare("q2".to_string());

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::PlanReady {
                questions: vec![q1.clone(), q2.clone()],
                rework: false,
            },
        )));
        assert!(a.survey_gate.is_some());
        assert!(a.survey_gate_targets_current_session());
        assert!(a.messages.iter().any(|m| m.role == "research_plan"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "research_plan"));
        // The presented plan includes the block briefs.
        let plan_msg = a
            .messages
            .iter()
            .find(|m| m.role == "research_plan")
            .unwrap();
        assert!(plan_msg.content.contains("1. q1"));

        // A chat reply (with an edit) routes into the pipeline and is
        // recorded as a gate reply in the session transcript — rendered like
        // a user message but never replayed to the model.
        a.reply_to_survey_gate("drop q2");
        assert!(a.survey_gate.is_none());
        assert!(!a.survey_gate_targets_current_session());
        assert_eq!(rx.recv().await.unwrap(), "drop q2");
        assert!(
            a.messages
                .iter()
                .any(|m| m.role == "gate_reply" && m.content == "drop q2")
        );
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(
            stored
                .iter()
                .any(|m| m.role == "gate_reply" && m.content == "drop q2")
        );
        // The gate reply must not leak into model history without context.
        let history = a.build_history();
        assert!(
            !history.iter().any(|m| m.content == "drop q2"),
            "gate replies must be excluded from model history"
        );
    }

    #[tokio::test]
    async fn plan_ready_saves_a_plan_file_record_in_the_space() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();
        let (tx, _rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);

        a.on_research_done(Some((
            session_id,
            space_id,
            space_name.clone(),
            ResearchUpdate::PlanReady {
                questions: vec![PlanQuestion::bare("q1".to_string())],
                rework: false,
            },
        )));

        let dir = a.space.files_dir(&space_name);
        let saved: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("plan-") && n.ends_with(".md"))
            .collect();
        assert_eq!(saved.len(), 1, "expected one plan file in {dir:?}");
        let body = std::fs::read_to_string(dir.join(&saved[0])).unwrap();
        assert!(body.contains("Research plan: rust async runtimes"));
        assert!(body.contains("1. q1"));
    }

    #[tokio::test]
    async fn survey_ready_arms_the_gate_and_renders_a_survey_section() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("fine-tuning LLMs");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::SurveyReady {
                questions: vec!["Depth or breadth?".to_string()],
                round: 1,
            },
        )));
        assert!(a.survey_gate_targets_current_session());
        let survey = a.messages.iter().find(|m| m.role == "survey").unwrap();
        assert!(survey.content.contains("For \"fine-tuning LLMs\":"));
        assert!(survey.content.contains("1. Depth or breadth?"));
        assert!(survey.content.contains("I approve"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().any(|m| m.role == "survey"));

        a.reply_to_survey_gate("depth first");
        assert!(a.survey_gate.is_none());
        assert_eq!(rx.recv().await.unwrap(), "depth first");
        assert!(a.status.contains("follow-ups"));
    }

    #[tokio::test]
    async fn gate_only_targets_the_viewed_gated_session() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("topic one");
        let gated_session = a.session.as_ref().unwrap().id.clone();
        let (tx, _rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);
        a.on_research_done(Some((
            gated_session.clone(),
            a.active_space.id.clone(),
            a.active_space.name.clone(),
            ResearchUpdate::SurveyReady {
                questions: vec!["q?".to_string()],
                round: 1,
            },
        )));
        assert!(a.survey_gate_targets_current_session());

        // Switch to a different session: the gate must not intercept typing.
        let other =
            a.db.create_session("other", "m", &a.active_space.id, "chat")
                .unwrap();
        a.session = Some(other);
        a.messages.clear();
        assert!(!a.survey_gate_targets_current_session());
        assert!(a.survey_gate.is_some(), "gate stays armed for its session");
    }

    #[tokio::test]
    async fn closed_reply_channel_fails_plan_approval_closed() {
        // A gate whose reply channel closed (job teardown racing the parked
        // approval) must fail closed — never run searchers on an unapproved
        // plan. The closed channel surfaces immediately, before any provider
        // call, so a bare test client is fine.
        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();
        drop(reply_tx);
        let (tx, _rx) = mpsc::unbounded_channel::<ResearchMsg>();
        let ids = ("s".to_string(), "sp".to_string(), "sn".to_string());
        let mut questions = vec![PlanQuestion::bare("q1".to_string())];
        let provider = OpenRouter::openrouter_flavor("test-key".to_string());
        let result = await_plan_approval(
            &provider,
            "a/b",
            "topic",
            &mut questions,
            &mut reply_rx,
            &tx,
            &ids,
        )
        .await;
        let err = result.expect_err("closed channel must fail closed, not approve");
        assert!(err.contains("cancelled"), "{err}");
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
        let session = a
            .session
            .as_ref()
            .expect("switched into the research session");
        assert!(session.title.contains("rust async runtimes"));
        assert!(
            a.messages
                .iter()
                .any(|m| m.content.contains("/research rust async runtimes"))
        );
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
            ResearchUpdate::Stage {
                label: "planning".to_string(),
                detail: String::new(),
            },
        )));

        assert!(
            a.messages
                .iter()
                .any(|m| m.role == "research_stage" && m.content == "planning")
        );
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(
            stored
                .iter()
                .any(|m| m.role == "research_stage" && m.content == "planning")
        );
        assert!(a.status.contains("planning"));

        // A second tick with the same label replaces the row, not appends.
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();
        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name,
            ResearchUpdate::Stage {
                label: "planning".to_string(),
                detail: "revised".to_string(),
            },
        )));
        let stored = a.db.load_messages(&session_id).unwrap();
        let rows: Vec<_> = stored
            .iter()
            .filter(|m| m.role == "research_stage")
            .collect();
        assert_eq!(rows.len(), 1, "one row per label, updated in place");
        assert_eq!(rows[0].content, "planning: revised");
        let visible_rows: Vec<_> = a
            .messages
            .iter()
            .filter(|m| m.role == "research_stage")
            .collect();
        assert_eq!(visible_rows.len(), 1);
        assert_eq!(visible_rows[0].content, "planning: revised");
        assert!(a.status.contains("Ctrl+↑ agents"));
    }

    #[tokio::test]
    async fn multiple_drained_steers_each_keep_their_own_stage_row() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        // Two drained steers arrive as `steer #N` rows (N = queue position =
        // drain order). A text-keyed label would collapse rows when one
        // steer's text is a prefix of another's — the sequence key keeps
        // every drained steer its own persisted + visible row.
        for (i, steer) in ["look into X", "also Y"].iter().enumerate() {
            a.on_research_done(Some((
                session_id.clone(),
                space_id.clone(),
                space_name.clone(),
                ResearchUpdate::Stage {
                    label: format!("steer #{}", i + 1),
                    detail: steer.to_string(),
                },
            )));
        }
        // The pipeline's acknowledgements are job-global: both steers must
        // no longer count as queued, and their rows persist for display.
        assert_eq!(
            a.research_steer_acked,
            std::collections::HashSet::from([1, 2])
        );
        let stored = a.db.load_messages(&session_id).unwrap();
        let steer_rows: Vec<_> = stored
            .iter()
            .filter(|m| m.role == "research_stage" && m.content.starts_with("steer #"))
            .collect();
        assert_eq!(steer_rows.len(), 2, "one persisted row per drained steer");
        let visible: Vec<_> = a
            .messages
            .iter()
            .filter(|m| m.role == "research_stage" && m.content.starts_with("steer #"))
            .collect();
        assert_eq!(visible.len(), 2, "both steers visible in the transcript");
    }

    #[tokio::test]
    async fn steer_rows_do_not_collide_on_duplicate_prefix_or_wildcard_text() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();

        // The bug this guards against: steers were keyed by their text, so
        // "a: b" then "a" collapsed into one row (the upsert matches labels
        // by prefix), identical duplicates collapsed too, and `%`/`_` acted
        // as SQL LIKE wildcards in the match. Sequence keys make the text
        // irrelevant to row identity.
        let steers = ["a: b", "a", "same", "same", "100% done"];
        for (i, steer) in steers.iter().enumerate() {
            a.on_research_done(Some((
                session_id.clone(),
                space_id.clone(),
                space_name.clone(),
                ResearchUpdate::Stage {
                    label: format!("steer #{}", i + 1),
                    detail: steer.to_string(),
                },
            )));
        }
        let stored = a.db.load_messages(&session_id).unwrap();
        let rows: Vec<_> = stored
            .iter()
            .filter(|m| m.role == "research_stage" && m.content.starts_with("steer #"))
            .collect();
        assert_eq!(
            rows.len(),
            steers.len(),
            "one row per drained steer — no collapse on duplicate/prefix/wildcard text"
        );
        for (i, steer) in steers.iter().enumerate() {
            let want = format!("steer #{}: {steer}", i + 1);
            assert!(
                rows.iter().any(|m| m.content == want),
                "missing row for steer #{}: {want}",
                i + 1
            );
        }
        // The pipeline's acknowledgements are job-global and position-keyed.
        assert_eq!(
            a.research_steer_acked,
            (1..=steers.len()).collect::<std::collections::HashSet<usize>>(),
            "every steer position picked up"
        );
    }

    #[test]
    fn steer_log_drops_acknowledged_entries_and_clears_on_stop() {
        let mut a = test_app();
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        a.research_steer_tx = Some(tx);

        a.steer_research("first");
        a.steer_research("second");
        a.steer_research("third");
        assert_eq!(
            a.research_steer_log,
            vec![
                (1, "first".into()),
                (2, "second".into()),
                (3, "third".into())
            ]
        );

        // The pipeline drains 1 and 2: acknowledged entries are dropped on
        // the next queue (positions are never renumbered).
        a.research_steer_acked = std::collections::HashSet::from([1, 2]);
        a.steer_research("fourth");
        assert_eq!(
            a.research_steer_log,
            vec![(3, "third".into()), (4, "fourth".into())]
        );

        // An ack that arrives while the job is parked prunes the log
        // immediately — no need to wait for the next `/steer`.
        a.research_steer_acked.insert(3);
        a.on_research_done(Some((
            "s".to_string(),
            "sp".to_string(),
            "sn".to_string(),
            ResearchUpdate::Stage {
                label: "steer #3".to_string(),
                detail: "third".to_string(),
            },
        )));
        assert_eq!(a.research_steer_log, vec![(4, "fourth".into())]);

        // Stopping the job drops the whole retained log.
        let (_tx, rx) = mpsc::unbounded_channel::<ResearchMsg>();
        a.research_rx = Some(rx);
        a.research_running = Some(("s".to_string(), "t".to_string()));
        a.stop_research();
        assert!(a.research_steer_log.is_empty());
        assert!(a.research_steer_acked.is_empty());
    }

    #[test]
    fn steer_queue_is_hard_bound() {
        let mut a = test_app();
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        a.research_steer_tx = Some(tx);

        // Fill the queue to the bound; further steers are refused with a
        // status message (which also bounds the unbounded channel and the
        // retained log).
        for i in 0..MAX_QUEUED_STEERS {
            a.steer_research(&format!("steer {i}"));
        }
        assert_eq!(a.research_steer_log.len(), MAX_QUEUED_STEERS);
        a.steer_research("overflow");
        assert_eq!(a.research_steer_log.len(), MAX_QUEUED_STEERS);
        assert!(a.status.contains("steer queue full"), "{}", a.status);
    }

    #[tokio::test]
    async fn plan_ready_in_incognito_mode_writes_no_plan_file() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.incognito = true;
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let space_id = a.active_space.id.clone();
        let space_name = a.active_space.name.clone();
        let (tx, _rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);

        a.on_research_done(Some((
            session_id.clone(),
            space_id,
            space_name.clone(),
            ResearchUpdate::PlanReady {
                questions: vec![PlanQuestion::bare("q1".to_string())],
                rework: false,
            },
        )));

        // "Nothing persists": the plan (which folds in the user's survey
        // replies) must not land on disk, and must not be written to the
        // message db either — the in-memory transcript still shows it while
        // the session is viewed.
        let dir = a.space.files_dir(&space_name);
        let _ = std::fs::create_dir_all(&dir);
        let saved: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("plan-"))
            .collect();
        assert!(saved.is_empty(), "no plan files in incognito: {saved:?}");
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(
            stored.iter().all(|m| m.role != "research_plan"),
            "the plan must not be persisted to the message db in incognito"
        );
        assert!(
            a.messages.iter().any(|m| m.role == "research_plan"),
            "the in-memory transcript still shows the plan while viewed"
        );
    }

    #[tokio::test]
    async fn incognito_gate_rows_follow_the_mode_captured_at_job_start() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.incognito = true;
        a.start_research("private topic");
        let session_id = a.session.as_ref().unwrap().id.clone();
        assert!(a.research_incognito);

        // A later UI-mode change must not make this already-private job start
        // persisting its survey or the user's answer.
        a.incognito = false;
        let (tx, mut rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);
        a.on_research_done(Some((
            session_id.clone(),
            a.active_space.id.clone(),
            a.active_space.name.clone(),
            ResearchUpdate::SurveyReady {
                questions: vec!["Which confidential product?".to_string()],
                round: 1,
            },
        )));
        a.reply_to_survey_gate("Project Juniper");
        assert_eq!(rx.recv().await.unwrap(), "Project Juniper");

        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(stored.iter().all(|m| m.role != "survey"));
        assert!(stored.iter().all(|m| m.role != "gate_reply"));
    }

    #[tokio::test]
    async fn off_screen_incognito_plan_is_restored_when_its_session_opens() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.incognito = true;
        a.start_research("private topic");
        let session_id = a.session.as_ref().unwrap().id.clone();
        let other =
            a.db.create_session("other", "m", &a.active_space.id, "chat")
                .unwrap();
        a.session = Some(other);
        a.messages.clear();
        let (tx, _rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);

        a.on_research_done(Some((
            session_id.clone(),
            a.active_space.id.clone(),
            a.active_space.name.clone(),
            ResearchUpdate::PlanReady {
                questions: vec![PlanQuestion::bare("private question".to_string())],
                rework: true,
            },
        )));
        assert!(!a.survey_gate_targets_current_session());
        assert!(
            a.db.load_messages(&session_id)
                .unwrap()
                .iter()
                .all(|m| m.role != "research_plan")
        );

        a.switch_to_session_by_id(&session_id).unwrap();

        assert!(a.survey_gate_targets_current_session());
        let plan = a
            .messages
            .iter()
            .find(|m| m.role == "research_plan")
            .expect("pending incognito plan restored in memory");
        assert!(plan.content.contains("private question"));
        assert!(plan.content.contains("reply \"approve\""));
        assert!(!plan.content.contains("tell me what to change"));
    }

    #[tokio::test]
    async fn undelivered_gate_reply_is_rolled_back_and_restored_to_composer() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        // The gate's receiver is already gone: persisting succeeds, but
        // channel delivery fails — the persisted reply must be rolled back
        // so a retry can't duplicate it.
        let (reply_tx, rx) = mpsc::unbounded_channel::<String>();
        drop(rx);
        a.survey_reply_tx = Some(reply_tx.clone());
        a.on_research_done(Some((
            session_id.clone(),
            a.active_space.id.clone(),
            a.active_space.name.clone(),
            ResearchUpdate::PlanReady {
                questions: vec![PlanQuestion::bare("q1".to_string())],
                rework: false,
            },
        )));
        assert!(a.survey_gate.is_some());

        a.reply_to_survey_gate("drop q2");

        assert!(a.survey_gate.is_none());
        // Composer restored, nothing persisted, nothing mirrored in memory.
        assert!(a.input_text().contains("drop q2"), "{}", a.input_text());
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(
            stored.iter().all(|m| m.role != "gate_reply"),
            "undelivered reply must not remain persisted"
        );
        assert!(!a.messages.iter().any(|m| m.role == "gate_reply"));
    }

    #[tokio::test]
    async fn off_screen_gate_marks_the_session_unread_and_notifies() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("rust async runtimes");
        let session_id = a.session.as_ref().unwrap().id.clone();
        // Navigate away before the gate arrives.
        let other =
            a.db.create_session("other", "m", &a.active_space.id, "chat")
                .unwrap();
        a.session = Some(other);
        a.messages.clear();
        let (tx, _rx) = mpsc::unbounded_channel();
        a.survey_reply_tx = Some(tx);

        a.on_research_done(Some((
            session_id.clone(),
            a.active_space.id.clone(),
            a.active_space.name.clone(),
            ResearchUpdate::SurveyReady {
                questions: vec!["Depth or breadth?".to_string()],
                round: 1,
            },
        )));

        // The gate is armed for the job's session and the user is told where
        // input is needed — a silently parked pipeline can't block later
        // research unnoticed.
        assert!(a.survey_gate.is_some());
        assert!(!a.survey_gate_targets_current_session());
        assert!(
            a.unread.contains(&session_id),
            "session must be marked unread"
        );
        assert!(a.status.contains("waiting on you"), "{}", a.status);
        assert!(a.status.contains("survey round 1"), "{}", a.status);
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
            ResearchUpdate::Done(Ok(
                "# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string(),
            )),
        )));

        assert!(a.unread.contains(&session_id));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(
            stored
                .iter()
                .any(|m| m.role == "assistant" && m.content.contains("Rust Async Runtimes"))
        );

        // Saved into the space's files dir and picked up by a rescan.
        let dir = a.space.files_dir(&space_name);
        let saved = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .count();
        assert_eq!(
            saved, 1,
            "expected exactly one saved report file in {dir:?}"
        );
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
            ResearchUpdate::Done(Ok(
                "# Rust Async Runtimes\n\nBody text. [1]\n\n## Sources\n1. https://a".to_string(),
            )),
        )));

        // Assert: the report file lands in the ORIGINAL space's files_dir
        let original_dir = a.space.files_dir(&original_space_name);
        let original_files = std::fs::read_dir(&original_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .count();
        assert_eq!(
            original_files, 1,
            "expected exactly one report file in original space {original_dir:?}"
        );

        // Assert: the report file did NOT land in the second (now-active) space's files_dir
        let second_dir = a.space.files_dir(&second_space.name);
        let second_files = std::fs::read_dir(&second_dir)
            .map(|d| d.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        assert_eq!(
            second_files, 0,
            "expected no files in second (active) space {second_dir:?}"
        );

        // Assert: files_cache is still empty (rescan_files was NOT called for space B,
        // because the report was saved to space A, not space B)
        assert_eq!(
            a.files_cache.len(),
            0,
            "files_cache should be empty since rescan was not triggered"
        );
    }

    #[tokio::test]
    async fn on_research_done_failure_posts_error_message() {
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
            ResearchUpdate::Done(Err("planner: network down".to_string())),
        )));

        assert!(a.status.contains("network down"));
        let stored = a.db.load_messages(&session_id).unwrap();
        assert!(
            stored
                .iter()
                .any(|m| m.role == "assistant" && m.content.contains("network down"))
        );
    }

    #[tokio::test]
    async fn on_research_done_none_clears_state_and_closes_the_live_popup() {
        let mut a = test_app();
        a.research_model = "openai/gpt-5-mini".to_string();
        a.start_research("t");
        assert!(a.research_rx.is_some());
        a.popup = super::super::Popup::ResearchLive;
        a.research_live_input = "late steer".to_string();
        a.research_stage_rows = vec!["writer: done".to_string()];

        a.on_research_done(None);

        assert!(a.research_rx.is_none());
        assert!(a.research_running.is_none());
        assert_eq!(a.popup, super::super::Popup::None);
        assert!(a.research_live_input.is_empty());
        assert!(a.research_stage_rows.is_empty());
    }

    #[test]
    fn parse_subquestions_reads_a_clean_json_array() {
        let qs = parse_subquestions(r#"["what is X", "how does Y work"]"#);
        assert_eq!(
            qs,
            vec!["what is X".to_string(), "how does Y work".to_string()]
        );
    }

    #[test]
    fn parse_subquestions_strips_markdown_fences() {
        let qs = parse_subquestions("```json\n[\"a\", \"b\"]\n```");
        assert_eq!(qs, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_subquestions_falls_back_to_bullet_lines() {
        let qs = parse_subquestions("- what is X\n- how does Y work\n* a third one");
        assert_eq!(
            qs,
            vec![
                "what is X".to_string(),
                "how does Y work".to_string(),
                "a third one".to_string()
            ]
        );
    }

    #[test]
    fn parse_subquestions_falls_back_to_numbered_lines() {
        let qs = parse_subquestions("1. what is X\n2) how does Y work");
        assert_eq!(
            qs,
            vec!["what is X".to_string(), "how does Y work".to_string()]
        );
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
            Critique::Gaps(vec![
                "what about pricing?".to_string(),
                "any recent incidents?".to_string()
            ])
        );
    }

    #[test]
    fn parse_critique_recognizes_contradiction() {
        let c = parse_critique("CONTRADICTION: source A says X, source B says not-X");
        assert_eq!(
            c,
            Critique::Contradiction("source A says X, source B says not-X".to_string())
        );
    }

    #[test]
    fn parse_critique_falls_back_to_satisfied_on_garbage() {
        assert_eq!(
            parse_critique("uh, looks fine I guess?"),
            Critique::Satisfied
        );
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
        let msgs = synthesizer_messages(
            "topic",
            &["finding one".to_string()],
            &["https://a.example".to_string()],
        );
        let user = msgs.iter().find(|m| m.role == "user").unwrap();
        assert!(
            user.content.contains("https://a.example"),
            "{}",
            user.content
        );
        assert!(
            user.content.to_lowercase().contains("prioritize"),
            "{}",
            user.content
        );
    }

    #[test]
    fn synthesizer_messages_omits_pinned_section_when_empty() {
        let msgs = synthesizer_messages("topic", &["finding one".to_string()], &[]);
        let user = msgs.iter().find(|m| m.role == "user").unwrap();
        assert!(
            !user.content.to_lowercase().contains("prioritize"),
            "{}",
            user.content
        );
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
