//! Reconnectable, session-scoped research projections for thin clients.
//!
//! The HTTP router owns authentication and command dispatch.  This module
//! keeps the read projection in one place so a reconnecting browser can
//! reconstruct the live stages and report without relying on missed SSE
//! frames.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::{App, SurveyPhase};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchStageView {
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitationView {
    pub report_file: String,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResearchView {
    pub session_id: String,
    pub topic: String,
    pub stages: Vec<ResearchStageView>,
    pub gate: Option<Value>,
    pub steers: Vec<String>,
    pub report: Option<String>,
    pub citations: Vec<CitationView>,
    pub running: bool,
}

/// Build the durable view for exactly `session_id`.  In particular, a job in
/// another tab/session cannot leak its gate, report, or stage rows here.
pub fn view(app: &App, session_id: &str) -> Result<ResearchView> {
    ensure_session(app, session_id)?;
    let messages = if app
        .session
        .as_ref()
        .is_some_and(|session| session.id == session_id)
        && app.research_incognito
    {
        app.messages.clone()
    } else {
        app.db.load_messages(session_id)?
    };
    let stages = messages
        .iter()
        .filter(|message| message.role == "research_stage")
        .map(|message| {
            let (label, detail) = message
                .content
                .split_once(':')
                .map_or((message.content.as_str(), ""), |(label, detail)| {
                    (label, detail.trim())
                });
            ResearchStageView {
                label: label.to_string(),
                detail: detail.to_string(),
            }
        })
        .collect();
    let report = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
        .map(|message| message.content.clone());
    let citations = report
        .as_deref()
        .map(|text| {
            let urls = crate::citations::parse_citations(text)
                .into_iter()
                .map(|(_, url)| url)
                .collect::<std::collections::HashSet<_>>();
            app.db
                .search_citations(&app.active_space.id, None)
                .unwrap_or_default()
                .into_iter()
                .filter(|(_, url, _)| urls.contains(url))
                .map(|(report_file, url, title)| CitationView {
                    report_file,
                    url,
                    title,
                })
                .collect()
        })
        .unwrap_or_default();
    let gate = app
        .survey_gate
        .as_ref()
        .filter(|gate| gate.session_id == session_id)
        .map(|gate| {
            let questions = gate_questions(&gate.prompt_content);
            match &gate.phase {
                SurveyPhase::Clarify { round } => {
                    json!({ "session_id": session_id, "phase": { "Clarify": { "round": round } }, "questions": questions })
                }
                SurveyPhase::Approve { rework } => {
                    json!({ "session_id": session_id, "phase": { "Approve": { "rework": rework } }, "questions": questions })
                }
            }
        });
    let running = app
        .research_running
        .as_ref()
        .is_some_and(|(id, _)| id == session_id);
    let topic = app
        .research_running
        .as_ref()
        .filter(|(id, _)| id == session_id)
        .map_or_else(String::new, |(_, topic)| topic.clone());
    let steers = if running {
        app.research_steer_log
            .iter()
            .map(|(_, text)| text.clone())
            .collect()
    } else {
        Vec::new()
    };
    Ok(ResearchView {
        session_id: session_id.to_string(),
        topic,
        stages,
        gate,
        steers,
        report,
        citations,
        running,
    })
}

/// Extract the numbered questions from the durable survey/plan transcript.
/// The prompt also contains a heading, plan briefs, and reply instructions;
/// exposing those lines as questions makes a reconnecting web client render a
/// different gate from the live event.
fn gate_questions(prompt: &str) -> Vec<String> {
    let numbered = prompt
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (number, question) = line.split_once(". ")?;
            if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let question = question.trim();
            (!question.is_empty()).then(|| question.to_string())
        })
        .collect::<Vec<_>>();
    if !numbered.is_empty() {
        return numbered;
    }
    // Keep legacy/unusual prompts visible when they do not use the normal
    // numbered format; an empty questions array would hide the actionable
    // text behind an otherwise usable gate.
    prompt
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn ensure_session(app: &App, session_id: &str) -> Result<()> {
    if app
        .db
        .list_sessions(&app.active_space.id)?
        .iter()
        .any(|session| session.id == session_id)
    {
        Ok(())
    } else {
        bail!("unknown session in active space")
    }
}

/// Read-only host route helper.  The router should pass `/v1/research/{id}`
/// or `/v1/research/{id}/citations` after authentication.
// The explicit route table keeps method and session checks together.
#[allow(clippy::too_many_lines)]
pub fn dispatch(
    app: &mut App,
    method: &str,
    path: &str,
    _query: &str,
    body: &[u8],
) -> Result<Value> {
    if path == "/v1/research/start" {
        if method != "POST" {
            bail!("research start requires POST");
        }
        let payload: Value = serde_json::from_slice(body)?;
        let space_id = payload
            .get("space_id")
            .and_then(Value::as_str)
            .unwrap_or(&app.active_space.id);
        if space_id != app.active_space.id {
            bail!("space is not active");
        }
        let topic = payload
            .get("topic")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing topic"))?;
        let gated = payload
            .get("gated")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        start(app, topic, gated)?;
        return Ok(
            json!({ "ok": true, "session_id": app.research_running.as_ref().map(|(id, _)| id) }),
        );
    }
    let prefix = "/v1/research/";
    let id = path
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow::anyhow!("unknown research route"))?;
    let (session_id, action) = id
        .split_once('/')
        .map_or((id, "view"), |(id, action)| (id, action));
    if session_id.is_empty()
        || (action != "view"
            && action != "citations"
            && action != "context"
            && action != "steer"
            && action != "answer"
            && action != "start"
            && action != "stop")
    {
        bail!("invalid research route");
    }
    let decoded = super::api::percent_decode(session_id);
    let session_id = decoded.as_str();
    ensure_session(app, session_id)?;
    if method == "POST" {
        let payload: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(body)?
        };
        match action {
            "steer" => {
                let text = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("missing text"))?;
                if app
                    .research_running
                    .as_ref()
                    .is_none_or(|(id, _)| id != session_id)
                {
                    bail!("research session is not running");
                }
                app.steer_research(text);
                if text.trim().is_empty()
                    || !app
                        .research_steer_log
                        .iter()
                        .any(|(_, queued)| queued == text)
                {
                    bail!("steer could not be queued; the queue may be full");
                }
            }
            "answer" => {
                let text = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("missing text"))?;
                if app
                    .survey_gate
                    .as_ref()
                    .is_none_or(|gate| gate.session_id != session_id)
                {
                    bail!("research session is not waiting for an answer");
                }
                app.reply_to_survey_gate(text);
                if app.survey_gate.is_some() {
                    bail!("answer could not be delivered; try again");
                }
            }
            "start" => {
                if app
                    .session
                    .as_ref()
                    .is_none_or(|session| session.id != session_id)
                {
                    app.switch_to_session_by_id(session_id)?;
                }
                let topic = payload
                    .get("topic")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("missing topic"))?;
                let gated = payload
                    .get("gated")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                start(app, topic, gated)?;
                return Ok(
                    json!({ "ok": true, "session_id": app.research_running.as_ref().map(|(id, _)| id) }),
                );
            }
            "stop" => {
                if app
                    .research_running
                    .as_ref()
                    .is_some_and(|(id, _)| id == session_id)
                {
                    app.stop_research();
                }
            }
            _ => bail!("route requires GET"),
        }
        return Ok(serde_json::to_value(view(app, session_id)?)?);
    }
    if method != "GET" {
        bail!("research view only supports GET or POST");
    }
    if action == "citations" {
        return Ok(json!({ "citations": view(app, session_id)?.citations }));
    }
    if action == "context" {
        if app
            .session
            .as_ref()
            .is_none_or(|session| session.id != session_id)
        {
            bail!("context is only available for the active session");
        }
        let context = app.context_breakdown();
        return Ok(json!({
            "system_tokens": context.system_tokens,
            "memory_tokens": context.memory_tokens,
            "skills_tokens": context.skills_tokens,
            "conversation_tokens": context.conversation_tokens,
            "limit": context.limit,
            "compacted": context.compacted,
        }));
    }
    if action != "view" {
        bail!("route requires POST");
    }
    Ok(serde_json::to_value(view(app, session_id)?)?)
}

fn start(app: &mut App, topic: &str, gated: bool) -> Result<()> {
    if topic.trim().is_empty() || topic.len() > 32_000 {
        bail!("research topic must be 1–32000 bytes");
    }
    if app.research_running.is_some() || app.research_rx.is_some() {
        bail!("research is already running");
    }
    let model = app
        .current_model
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("select a model before starting research"))?;
    if app.resolve_model_backend(model).is_none() {
        bail!("model backend is unavailable; configure provider login");
    }
    app.start_research_with_gate(topic, gated);
    if app.research_running.is_none() {
        bail!("research could not start");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let root =
            std::env::temp_dir().join(format!("nexus-research-view-{}", uuid::Uuid::new_v4()));
        App::new(
            crate::db::Db::open_in_memory().expect("db"),
            Some("sk-test"),
            crate::space::Space { root },
        )
    }

    #[test]
    fn citation_route_is_session_explicit() {
        let id = "/v1/research/session/citations"
            .strip_prefix("/v1/research/")
            .expect("research route");
        assert_eq!(id.split_once('/'), Some(("session", "citations")));
    }

    #[test]
    fn stage_content_splits_only_the_first_separator() {
        let content = "search: source: detail";
        let (label, detail) = content.split_once(':').expect("separator");
        assert_eq!(label, "search");
        assert_eq!(detail.trim(), "source: detail");
    }

    #[test]
    fn gate_questions_skip_prompt_headings_and_plan_briefs() {
        let prompt = "Research plan — reply to approve:\n1. Compare costs\n   Why: prices vary\n   Sources: filings\n2. Check risks\n\nReply to approve";
        assert_eq!(gate_questions(prompt), ["Compare costs", "Check risks"]);
    }

    #[test]
    fn gate_questions_preserve_unnumbered_legacy_prompt() {
        assert_eq!(
            gate_questions("Please clarify the target region."),
            ["Please clarify the target region."]
        );
    }

    #[test]
    fn view_reads_only_the_requested_session() {
        let app = test_app();
        let first = app
            .db
            .create_session("first", "test", &app.active_space.id, "research")
            .expect("first session");
        let second = app
            .db
            .create_session("second", "test", &app.active_space.id, "research")
            .expect("second session");
        app.db
            .upsert_research_stage_message(&first.id, "survey", "first detail")
            .expect("stage");
        app.db
            .upsert_research_stage_message(&second.id, "survey", "second detail")
            .expect("stage");
        app.db
            .add_assistant_message(&first.id, "report [1]", None, None, None, None, None, None)
            .expect("report");
        let first_view = view(&app, &first.id).expect("view");
        assert_eq!(first_view.stages[0].detail, "first detail");
        assert_eq!(first_view.report.as_deref(), Some("report [1]"));
        assert!(view(&app, "missing").is_err());
    }
}
