//! `/swarm`: a per-session roster of persona+model rows that, when swarm
//! mode is on, turns a normal chat message into a multi-persona roundtable —
//! each persona replies each round, a moderator decides when the discussion
//! has converged (bounded by a hard cap), then a synthesis reply is written
//! as the turn's canonical assistant message.

use anyhow::Result;
use tokio::sync::mpsc;

use super::{App, Popup, SwarmPopupMode};
use crate::db::Persona;
use crate::provider::ChatMessage;
use crate::provider::openrouter::OpenRouter;

/// Bounds how many rounds a roundtable can run before the moderator's answer
/// stops being consulted and synthesis runs regardless.
const MAX_ROUNDS: usize = 4;

/// A `/swarm` turn update tagged with the session it belongs to (the turn
/// runs in the background, so the viewer may have navigated away by the
/// time an update lands).
pub type SwarmMsg = (String, SwarmUpdate);

pub enum SwarmUpdate {
    /// The roster was empty, so one was suggested — persist it before round 1.
    RosterSuggested(Vec<Persona>),
    /// Status-line progress text.
    Progress(String),
    /// One persona's reply for the round just run.
    Reply {
        persona: String,
        model: String,
        content: String,
    },
    /// The turn's final synthesis reply — the canonical assistant message.
    Synthesis(String),
    Error(String),
}

impl App {
    pub(crate) fn open_swarm_popup(&mut self) {
        let Some(session) = &self.session else {
            self.status = "start a chat first, then /swarm".to_string();
            return;
        };
        self.swarm_cache = self.db.list_swarm_personas(&session.id).unwrap_or_default();
        self.swarm_selected = 0;
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.popup = Popup::Swarm;
    }

    pub(crate) fn move_swarm_selection(&mut self, delta: i32) {
        self.swarm_selected =
            super::clamp_cursor(self.swarm_selected, self.swarm_cache.len(), delta);
    }

    /// Flip swarm mode for the active session.
    pub(crate) fn toggle_swarm_mode(&mut self) -> Result<()> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        let on = !session.swarm_mode;
        session.swarm_mode = on;
        self.db.set_session_swarm_mode(&session.id, on)?;
        self.status = format!("swarm mode: {}", if on { "ON" } else { "OFF" });
        Ok(())
    }

    /// Add a blank row and go straight into naming it.
    pub(crate) fn swarm_add_row(&mut self) {
        let default_model = self.current_model.clone().unwrap_or_default();
        self.swarm_cache.push(Persona {
            name: String::new(),
            model: default_model,
            blurb: String::new(),
        });
        self.swarm_selected = self.swarm_cache.len() - 1;
        self.swarm_edit.clear();
        self.swarm_popup_mode = SwarmPopupMode::EditName;
    }

    pub(crate) fn swarm_start_edit_name(&mut self) {
        if let Some(p) = self.swarm_cache.get(self.swarm_selected) {
            self.swarm_edit = p.name.clone();
            self.swarm_popup_mode = SwarmPopupMode::EditName;
        }
    }

    pub(crate) fn swarm_start_edit_blurb(&mut self) {
        if let Some(p) = self.swarm_cache.get(self.swarm_selected) {
            self.swarm_edit = p.blurb.clone();
            self.swarm_popup_mode = SwarmPopupMode::EditBlurb;
        }
    }

    pub(crate) fn swarm_confirm_edit(&mut self) -> Result<()> {
        let text = std::mem::take(&mut self.swarm_edit).trim().to_string();
        let mode = self.swarm_popup_mode;
        if let Some(p) = self.swarm_cache.get_mut(self.swarm_selected) {
            match mode {
                SwarmPopupMode::EditName => p.name = text,
                SwarmPopupMode::EditBlurb => p.blurb = text,
                _ => {}
            }
        }
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.save_swarm_roster()
    }

    /// Esc out of an edit: a freshly-added, still-unnamed row gets dropped by
    /// `save_swarm_roster`'s empty-name filter, same as confirming a blank name.
    pub(crate) fn swarm_cancel_edit(&mut self) -> Result<()> {
        self.swarm_edit.clear();
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.save_swarm_roster()
    }

    pub(crate) fn swarm_remove_row(&mut self) -> Result<()> {
        if self.swarm_selected < self.swarm_cache.len() {
            self.swarm_cache.remove(self.swarm_selected);
        }
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.save_swarm_roster()
    }

    fn save_swarm_roster(&mut self) -> Result<()> {
        self.swarm_cache.retain(|p| !p.name.trim().is_empty());
        if let Some(session) = &self.session {
            self.db
                .save_swarm_personas(&session.id, &self.swarm_cache)?;
        }
        self.swarm_selected = self
            .swarm_selected
            .min(self.swarm_cache.len().saturating_sub(1));
        Ok(())
    }

    /// Start a swarm turn for the just-sent message (already pushed to
    /// `self.messages`/the db by `send_message`). No-op if one's running.
    pub(crate) fn start_swarm_turn(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.swarm_rx.is_some() {
            self.status = "a swarm turn is already running".to_string();
            return;
        }
        let Some(provider) = self.provider.clone() else {
            self.open_key_prompt();
            return;
        };
        let default_model = self.current_model.clone().unwrap_or_default();
        let meta_model = if !self.research_model.trim().is_empty() {
            self.research_model.trim().to_string()
        } else {
            default_model.clone()
        };
        let personas = self.db.list_swarm_personas(&session.id).unwrap_or_default();
        let user_message = self
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let base_history = self.build_history();

        let (tx, rx) = mpsc::unbounded_channel();
        self.swarm_rx = Some(rx);
        self.status = "swarm: starting…".to_string();

        tokio::spawn(run_swarm_turn(
            provider,
            meta_model,
            personas,
            default_model,
            base_history,
            user_message,
            session.id,
            tx,
        ));
    }

    /// A swarm turn update: persist it, and mirror it into the live
    /// transcript if the session it belongs to is the one being viewed.
    /// `None` = the job's channel closed (fires once, right after the last update).
    pub fn on_swarm_update(&mut self, r: Option<SwarmMsg>) {
        let Some((session_id, update)) = r else {
            self.swarm_rx = None;
            return;
        };
        let viewing = self.session.as_ref().is_some_and(|s| s.id == session_id);
        match update {
            SwarmUpdate::RosterSuggested(personas) => {
                let _ = self.db.save_swarm_personas(&session_id, &personas);
                if viewing {
                    self.swarm_cache = personas;
                    self.status = "swarm: roster suggested — round 1…".to_string();
                }
            }
            SwarmUpdate::Progress(s) => {
                if viewing {
                    self.status = format!("swarm: {s}");
                }
            }
            SwarmUpdate::Reply {
                persona,
                model,
                content,
            } => {
                if let Ok(id) = self
                    .db
                    .add_persona_message(&session_id, &content, &persona, &model)
                    && viewing
                {
                    self.messages.push(crate::db::Message {
                        id,
                        role: "assistant".to_string(),
                        content,
                        model: Some(model),
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                        persona: Some(persona),
                    });
                }
            }
            SwarmUpdate::Synthesis(content) => {
                let _ = self.db.add_assistant_message(
                    &session_id,
                    &content,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                if viewing {
                    self.messages.push(crate::db::Message {
                        id: String::new(),
                        role: "assistant".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: Some("Discussed".to_string()),
                        images: Vec::new(),
                        persona: None,
                    });
                    self.status = "swarm turn complete".to_string();
                }
            }
            SwarmUpdate::Error(e) => {
                if viewing {
                    self.status = format!("swarm error: {e}");
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_swarm_turn(
    provider: OpenRouter,
    meta_model: String,
    mut personas: Vec<Persona>,
    default_model: String,
    base_history: Vec<ChatMessage>,
    user_message: String,
    session_id: String,
    tx: mpsc::UnboundedSender<SwarmMsg>,
) {
    let send = |u: SwarmUpdate| {
        let _ = tx.send((session_id.clone(), u));
    };

    if personas.is_empty() {
        send(SwarmUpdate::Progress("suggesting personas…".to_string()));
        match suggest_personas(&provider, &meta_model, &user_message, &default_model).await {
            Ok(p) => {
                personas = p;
                send(SwarmUpdate::RosterSuggested(personas.clone()));
            }
            Err(e) => {
                send(SwarmUpdate::Error(format!(
                    "couldn't suggest personas: {e}"
                )));
                return;
            }
        }
    }
    if personas.is_empty() {
        send(SwarmUpdate::Error("no personas to run".to_string()));
        return;
    }

    // (persona name, reply content), in the order replies landed, across all rounds.
    let mut discussion: Vec<(String, String)> = Vec::new();
    let mut round = 0usize;
    loop {
        round += 1;
        send(SwarmUpdate::Progress(format!(
            "round {round}/{MAX_ROUNDS} — 0 of {} personas replied",
            personas.len()
        )));

        let transcript_so_far = render_discussion(&discussion);
        let mut set = tokio::task::JoinSet::new();
        for (i, p) in personas.iter().enumerate() {
            let provider = provider.clone();
            let model = p.model.clone();
            let name = p.name.clone();
            let mut messages = base_history.clone();
            messages.push(ChatMessage::text(
                "system",
                format!(
                    "You are participating in a multi-persona discussion. Your persona: {}. \
                     Respond briefly (a few sentences) and stay in character, from this \
                     perspective only.",
                    p.blurb
                ),
            ));
            if let Some(transcript) = &transcript_so_far {
                messages.push(ChatMessage::text(
                    "user",
                    format!(
                        "Discussion so far:\n\n{transcript}\n\nGive your next response, \
                         reacting to the above from your persona's perspective."
                    ),
                ));
            }
            set.spawn(async move {
                let res = provider
                    .complete(&model, messages)
                    .await
                    .map_err(|e| e.to_string());
                (i, name, model, res)
            });
        }

        let mut round_replies: Vec<(usize, String, String, String)> = Vec::new();
        let mut done = 0usize;
        while let Some(joined) = set.join_next().await {
            let Ok((i, name, model, res)) = joined else {
                continue;
            };
            match res {
                Ok(content) => {
                    done += 1;
                    send(SwarmUpdate::Progress(format!(
                        "round {round}/{MAX_ROUNDS} — {done} of {} personas replied",
                        personas.len()
                    )));
                    send(SwarmUpdate::Reply {
                        persona: name.clone(),
                        model: model.clone(),
                        content: content.clone(),
                    });
                    round_replies.push((i, name, model, content));
                }
                Err(e) => send(SwarmUpdate::Error(format!("{name} failed: {e}"))),
            }
        }
        round_replies.sort_by_key(|(i, ..)| *i);
        for (_, name, _, content) in round_replies {
            discussion.push((name, content));
        }

        if round >= MAX_ROUNDS {
            break;
        }
        send(SwarmUpdate::Progress(
            "moderator checking convergence…".to_string(),
        ));
        match moderator_converged(&provider, &meta_model, &user_message, &discussion).await {
            Ok(true) => break,
            Ok(false) => continue,
            Err(_) => break, // fail safe: stop and synthesize with what's gathered
        }
    }

    send(SwarmUpdate::Progress("writing synthesis…".to_string()));
    match synthesize(&provider, &meta_model, &user_message, &discussion).await {
        Ok(content) => send(SwarmUpdate::Synthesis(content)),
        Err(e) => send(SwarmUpdate::Error(format!("synthesis failed: {e}"))),
    }
}

fn render_discussion(discussion: &[(String, String)]) -> Option<String> {
    if discussion.is_empty() {
        return None;
    }
    Some(
        discussion
            .iter()
            .map(|(name, content)| format!("**{name}**: {content}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

async fn suggest_personas(
    provider: &OpenRouter,
    meta_model: &str,
    topic: &str,
    default_model: &str,
) -> Result<Vec<Persona>, String> {
    let prompt = format!(
        "Suggest exactly 3 distinct personas to discuss the following message from different \
         points of view. Reply with ONLY a JSON array, no prose, no markdown fences, shaped \
         like: [{{\"name\": \"short persona name\", \"blurb\": \"one-sentence personality and \
         perspective\"}}, ...]\n\nMessage: {topic}"
    );
    let raw = provider
        .complete(meta_model, vec![ChatMessage::text("user", prompt)])
        .await
        .map_err(|e| e.to_string())?;
    parse_suggested_personas(&raw, default_model)
}

/// Extract a `[{"name","blurb"}, ...]` JSON array from a (possibly
/// prose/fence-wrapped) model reply. Split out from `suggest_personas` so
/// the parsing logic is testable without a network call.
fn parse_suggested_personas(raw: &str, default_model: &str) -> Result<Vec<Persona>, String> {
    let start = raw.find('[').ok_or("no JSON array in response")?;
    let end = raw.rfind(']').ok_or("no JSON array in response")?;
    let json = raw.get(start..=end).ok_or("no JSON array in response")?;

    #[derive(serde::Deserialize)]
    struct Suggested {
        name: String,
        blurb: String,
    }
    let parsed: Vec<Suggested> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(parsed
        .into_iter()
        .filter(|s| !s.name.trim().is_empty())
        .map(|s| Persona {
            name: s.name,
            model: default_model.to_string(),
            blurb: s.blurb,
        })
        .collect())
}

async fn moderator_converged(
    provider: &OpenRouter,
    meta_model: &str,
    topic: &str,
    discussion: &[(String, String)],
) -> Result<bool, String> {
    let transcript = render_discussion(discussion).unwrap_or_default();
    let prompt = format!(
        "A panel of personas is discussing this message:\n\n{topic}\n\nDiscussion so far:\n\n\
         {transcript}\n\nHas the discussion converged (little new perspective left to add), or \
         should another round run? Reply with ONLY one word: CONVERGED or CONTINUE."
    );
    let raw = provider
        .complete(meta_model, vec![ChatMessage::text("user", prompt)])
        .await
        .map_err(|e| e.to_string())?;
    Ok(raw.to_uppercase().contains("CONVERGED"))
}

async fn synthesize(
    provider: &OpenRouter,
    meta_model: &str,
    topic: &str,
    discussion: &[(String, String)],
) -> Result<String> {
    let transcript = render_discussion(discussion).unwrap_or_default();
    let prompt = format!(
        "A panel of personas discussed this message:\n\n{topic}\n\nDiscussion:\n\n{transcript}\n\n\
         Write one final reply to the original message that reconciles the perspectives above \
         into a clear, useful answer. Markdown allowed. Don't mention that this came from a \
         panel — just answer well, informed by the discussion."
    );
    provider
        .complete(meta_model, vec![ChatMessage::text("user", prompt)])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_suggested_personas_tolerates_prose_and_fences() {
        let raw = "sure, here you go:\n```json\n[{\"name\":\"Skeptic\",\"blurb\":\"pokes holes\"},\
                   {\"name\":\"Advocate\",\"blurb\":\"user-first\"}]\n```";
        let personas = parse_suggested_personas(raw, "a/one").unwrap();
        assert_eq!(personas.len(), 2);
        assert_eq!(personas[0].name, "Skeptic");
        assert_eq!(personas[0].model, "a/one");
        assert_eq!(personas[1].blurb, "user-first");
    }

    #[test]
    fn parse_suggested_personas_drops_blank_names() {
        let raw = r#"[{"name":"","blurb":"nameless"},{"name":"Real","blurb":"ok"}]"#;
        let personas = parse_suggested_personas(raw, "a/one").unwrap();
        assert_eq!(personas.len(), 1);
        assert_eq!(personas[0].name, "Real");
    }

    #[test]
    fn parse_suggested_personas_errors_without_an_array() {
        assert!(parse_suggested_personas("no json here", "a/one").is_err());
    }

    #[test]
    fn render_discussion_none_when_empty_some_when_not() {
        assert!(render_discussion(&[]).is_none());
        let d = vec![("Skeptic".to_string(), "wait, really?".to_string())];
        let rendered = render_discussion(&d).unwrap();
        assert!(rendered.contains("**Skeptic**"));
        assert!(rendered.contains("wait, really?"));
    }
}
