//! Deep research: a background multi-agent pipeline triggered by `/research`.
//! Every stage but the Searcher fan-out is a single `Provider::complete`
//! call; parsing/prompt-building here is pure and unit tested. The async
//! orchestration (Task 9) calls real network endpoints and is exercised
//! manually, like every other network-calling background job in this
//! codebase (`maybe_generate_title`, image description, embedding).

use crate::provider::ChatMessage;

/// A background research pipeline update: a phase label, or the final
/// report/error.
pub(crate) enum ResearchUpdate {
    Stage(String),
    Done(std::result::Result<String, String>),
}

/// Hard cap on Planner-generated sub-questions per outer round.
const MAX_SUBQUESTIONS: usize = 6;
/// Tool-call budget for a single Searcher agent — a few search→fetch hops,
/// not a whole interactive conversation's worth.
pub(crate) const RESEARCH_SEARCHER_MAX_ITERS: usize = 6;

const PLANNER_PROMPT: &str = "You are the planning stage of an automated research pipeline. Given a research topic, decompose it into 3 to 6 focused sub-questions that together cover the topic thoroughly (different angles: definitions, current state, evidence/data, controversies, practical implications — whichever apply). Respond with ONLY a JSON array of strings, no prose, no markdown fences. Example: [\"question one\", \"question two\"]";

pub(crate) const SEARCHER_PROMPT: &str = "You are a research searcher agent. You will be given one focused sub-question. Use the web_search and fetch_url tools to investigate it thoroughly: search, then fetch and read the most promising pages, and search again with new terms you learn from them if needed. When you have enough to answer well, write a concise findings summary (a few paragraphs, prose, no headers) that directly answers the sub-question, citing sources inline as [n]. End your answer with a line starting exactly with 'Sources:' followed by the numbered list of URLs you used, one per line, matching your [n] citations.";

const SYNTHESIZER_PROMPT: &str = "You are the synthesis stage of a research pipeline. You'll be given the original topic and findings from several searcher agents, each already citing their own sources. Combine them into a single coherent draft report on the topic: organize by theme (not by sub-question), resolve obvious overlaps, keep every citation but you may renumber them consistently as you merge. Do not invent facts not present in the findings. Output the draft report in markdown, no preamble.";

const CRITIC_PROMPT: &str = "You are the critic stage of a research pipeline. Given the original topic and a draft report, decide if it's ready. Respond in exactly one of these forms:\n- the single word SATISFIED, if the draft thoroughly covers the topic with no notable gaps or contradictions.\n- GAPS: followed by a newline-separated bullet list (each line starting with '- ') of specific missing sub-topics or unanswered angles, each phrased as a searchable question.\n- CONTRADICTION: followed by one line describing a specific factual contradiction between sources in the draft that isn't resolved.\nUse CONTRADICTION only for an actual conflict between sources, not a missing angle — missing angles are always GAPS. Respond with nothing else.";

const ESCALATION_PROMPT: &str = "You are resolving a contradiction found in a research draft. You are given the topic, the draft, the full set of source findings gathered so far, and a description of the contradiction. Determine which claim the evidence better supports (or that both apply in different contexts) and write one paragraph resolving it, citing the [n] sources involved. Output only that paragraph.";

const VERIFIER_PROMPT: &str = "You are the verifier stage. Given the topic, the gathered source findings (with their citations), and a draft report, check every factual claim in the draft against the source findings. Rewrite the draft unchanged except: remove or mark with '⚠ unverifiable:' any claim not actually supported by the gathered findings. Output the corrected draft in markdown, nothing else.";

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

fn planner_messages(topic: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::text("system", PLANNER_PROMPT), ChatMessage::text("user", topic)]
}

fn synthesizer_messages(topic: &str, findings: &[String]) -> Vec<ChatMessage> {
    let body = findings
        .iter()
        .enumerate()
        .map(|(i, f)| format!("--- Searcher {} findings ---\n{f}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    vec![
        ChatMessage::text("system", SYNTHESIZER_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\n{body}")),
    ]
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

fn writer_messages(topic: &str, verified_draft: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", WRITER_PROMPT),
        ChatMessage::text("user", format!("Topic: {topic}\n\nVerified draft:\n{verified_draft}")),
    ]
}

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatParams, StreamEvent};
use crate::tools::ToolBox;

use super::ResearchMsg;

/// Send the `(session_id, space_id, space_name)` triple's stage update.
fn send_stage(tx: &mpsc::UnboundedSender<ResearchMsg>, ids: &(String, String, String), s: impl Into<String>) {
    let _ = tx.send((ids.0.clone(), ids.1.clone(), ids.2.clone(), ResearchUpdate::Stage(s.into())));
}

async fn complete_text(provider: &OpenRouter, model: &str, messages: Vec<ChatMessage>) -> Result<String, String> {
    provider.complete(model, messages).await.map(|s| s.trim().to_string()).map_err(|e| e.to_string())
}

async fn plan(provider: &OpenRouter, model: &str, topic: &str) -> Result<Vec<String>, String> {
    let text = complete_text(provider, model, planner_messages(topic)).await?;
    let qs = parse_subquestions(&text);
    if qs.is_empty() {
        return Err(format!("planner returned no usable sub-questions (raw reply: {text:.200})"));
    }
    Ok(qs)
}

/// One Searcher agent: given a single sub-question, runs the normal
/// tool-loop (restricted to web_search/fetch_url) and returns its final
/// prose findings (including its own "Sources:" citation list). Never
/// returns an `Err` — a dead search/fetch/model call becomes a placeholder
/// finding string so one bad sub-question can't sink the whole pipeline.
async fn run_searcher(provider: &OpenRouter, model: &str, sub_question: &str, toolbox: Arc<ToolBox>) -> String {
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
/// `{done}/{total}` stage update as each finishes. Order of the returned
/// findings doesn't matter (synthesis treats them as an unordered set).
async fn run_searchers(
    provider: &OpenRouter,
    model: &str,
    toolbox: &Arc<ToolBox>,
    questions: &[String],
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
    round: usize,
) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    for q in questions.iter().cloned() {
        let provider = provider.clone();
        let model = model.to_string();
        let toolbox = toolbox.clone();
        set.spawn(async move { run_searcher(&provider, &model, &q, toolbox).await });
    }
    let total = questions.len();
    let mut done = 0usize;
    let mut findings = Vec::with_capacity(total);
    while let Some(res) = set.join_next().await {
        done += 1;
        send_stage(tx, ids, format!("searching (round {round}, {done}/{total})…"));
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
    topic: String,
    toolbox: Arc<ToolBox>,
    tx: mpsc::UnboundedSender<ResearchMsg>,
    session_id: String,
    space_id: String,
    space_name: String,
) {
    let ids = (session_id, space_id, space_name);
    let result = run_research_inner(&provider, &research_model, &escalation_model, &topic, &toolbox, &tx, &ids).await;
    let _ = tx.send((ids.0, ids.1, ids.2, ResearchUpdate::Done(result)));
}

async fn run_research_inner(
    provider: &OpenRouter,
    research_model: &str,
    escalation_model: &str,
    topic: &str,
    toolbox: &Arc<ToolBox>,
    tx: &mpsc::UnboundedSender<ResearchMsg>,
    ids: &(String, String, String),
) -> Result<String, String> {
    send_stage(tx, ids, "planning…");
    let questions = plan(provider, research_model, topic).await?;

    let mut findings = run_searchers(provider, research_model, toolbox, &questions, tx, ids, 1).await;

    send_stage(tx, ids, "synthesizing…");
    let mut draft = complete_text(provider, research_model, synthesizer_messages(topic, &findings)).await?;

    send_stage(tx, ids, "critiquing…");
    let mut critique = parse_critique(&complete_text(provider, research_model, critic_messages(topic, &draft)).await?);

    if let Critique::Gaps(gaps) = &critique {
        let more = run_searchers(provider, research_model, toolbox, gaps, tx, ids, 2).await;
        findings.extend(more);
        send_stage(tx, ids, "re-synthesizing…");
        draft = complete_text(provider, research_model, synthesizer_messages(topic, &findings)).await?;
        send_stage(tx, ids, "critiquing (round 2)…");
        critique = parse_critique(&complete_text(provider, research_model, critic_messages(topic, &draft)).await?);
    }

    if let Critique::Contradiction(desc) = &critique {
        send_stage(tx, ids, "resolving a contradiction…");
        let resolution =
            complete_text(provider, escalation_model, escalation_messages(topic, &draft, &findings, desc)).await?;
        draft.push_str("\n\n");
        draft.push_str(&resolution);
    }

    send_stage(tx, ids, "verifying…");
    let verified = complete_text(provider, research_model, verifier_messages(topic, &draft, &findings)).await?;

    send_stage(tx, ids, "writing final report…");
    complete_text(provider, research_model, writer_messages(topic, &verified)).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let msgs = synthesizer_messages("rust async runtimes", &["finding one".to_string(), "finding two".to_string()]);
        assert_eq!(msgs[0].role, "system");
        assert!(msgs[1].content.contains("rust async runtimes"));
        assert!(msgs[1].content.contains("finding one"));
        assert!(msgs[1].content.contains("finding two"));
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
        let msgs = writer_messages("t", "verified content");
        assert!(msgs[1].content.contains("verified content"));
    }
}
