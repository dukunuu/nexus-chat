//! `/swarm`: a per-session roster of persona+model rows that, when swarm
//! mode is on, turns a normal chat message into a live conversation between
//! personas — each takes a turn in sequence, sees everything said before it,
//! and a moderator checks after every turn whether they've reached a
//! conclusion (bounded by a hard cap either way). A synthesis reply is then
//! written as the turn's canonical assistant message.

use anyhow::Result;
use tokio::sync::mpsc;

use super::{App, Popup, SwarmPopupMode};
use crate::db::Persona;
use crate::provider::ChatMessage;
use crate::provider::openrouter::OpenRouter;

/// Bounds how many individual persona turns a conversation can run before
/// stopping and synthesizing regardless of what the moderator says. Scales
/// with roster size so a bigger panel isn't cut off after everyone's barely
/// spoken.
const MAX_TURNS_PER_PERSONA: usize = 4;

/// Hard ceiling on how many personas a conversation can grow to via the
/// moderator adding new voices mid-run.
const MAX_PERSONAS: usize = 6;

/// A `/swarm` turn update tagged with the session it belongs to (the turn
/// runs in the background, so the viewer may have navigated away by the
/// time an update lands).
pub type SwarmMsg = (String, SwarmUpdate);

pub enum SwarmUpdate {
    /// The roster was empty, so one was suggested — persist it before the
    /// conversation's first turn.
    RosterSuggested(Vec<Persona>),
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
    PersonaJoined(Persona),
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
                    self.status =
                        "swarm: roster suggested — starting the conversation…".to_string();
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
            SwarmUpdate::PersonaJoined(persona) => {
                let mut roster = self.db.list_swarm_personas(&session_id).unwrap_or_default();
                if !roster.iter().any(|p| p.name == persona.name) {
                    roster.push(persona.clone());
                    let _ = self.db.save_swarm_personas(&session_id, &roster);
                }
                if viewing {
                    if !self.swarm_cache.iter().any(|p| p.name == persona.name) {
                        self.swarm_cache.push(persona.clone());
                    }
                    self.status = format!("swarm: {} joined the conversation", persona.name);
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

    // (persona name, reply content), in speaking order — a running
    // transcript every persona actually converses through, turn by turn.
    // `personas` can grow mid-conversation (the moderator can bring in a new
    // voice), so turns are driven by index, wrapping, rather than an
    // iterator over a fixed snapshot.
    let mut discussion: Vec<(String, String)> = Vec::new();
    let max_turns = personas.len().max(1) * MAX_TURNS_PER_PERSONA;
    let mut turn = 0usize;
    let mut idx = 0usize;
    'convo: loop {
        if idx >= personas.len() {
            idx = 0;
        }
        let p = personas[idx].clone();
        idx += 1;
        turn += 1;
        send(SwarmUpdate::Progress(format!(
            "turn {turn}/{max_turns} — {} is responding",
            p.name
        )));

        let mut messages = base_history.clone();
        messages.push(ChatMessage::text(
            "system",
            format!(
                "You are {} in a live group conversation with the other personas below. \
                 Your persona: {}. Speak naturally and briefly (a few sentences), stay in \
                 character, and actually engage with what the last speaker said — agree, \
                 push back, build on it, or ask a question, the way a real person would in \
                 a discussion. Don't just restate your own opening position every time.",
                p.name, p.blurb
            ),
        ));
        match render_discussion(&discussion) {
            Some(transcript) => messages.push(ChatMessage::text(
                "user",
                format!(
                    "Conversation so far:\n\n{transcript}\n\nIt's your turn, {}. Respond to \
                     what was just said.",
                    p.name
                ),
            )),
            None => messages.push(ChatMessage::text(
                "user",
                format!(
                    "You're opening the discussion, {}. Give your first take.",
                    p.name
                ),
            )),
        }

        match provider.complete(&p.model, messages).await {
            Ok(content) => {
                send(SwarmUpdate::Reply {
                    persona: p.name.clone(),
                    model: p.model.clone(),
                    content: content.clone(),
                });
                discussion.push((p.name.clone(), content));
            }
            Err(e) => send(SwarmUpdate::Error(format!("{} failed: {e}", p.name))),
        }

        if turn >= max_turns {
            break 'convo;
        }
        send(SwarmUpdate::Progress(
            "moderator checking for a conclusion…".to_string(),
        ));
        match moderator_check(&provider, &meta_model, &user_message, &discussion).await {
            Ok(ModeratorVerdict::Converged) => break 'convo,
            Ok(ModeratorVerdict::Continue) => {}
            Ok(ModeratorVerdict::AddPersona { name, blurb }) => {
                // ponytail: fixed cap on dynamic growth, not configurable — a
                // 3-persona chat that keeps spawning new voices would never
                // converge and never hit MAX_TURNS_PER_PERSONA-driven synthesis.
                if personas.len() >= MAX_PERSONAS
                    || personas.iter().any(|existing| existing.name == name)
                {
                    continue;
                }
                let new_persona = Persona {
                    name,
                    model: default_model.clone(),
                    blurb,
                };
                send(SwarmUpdate::PersonaJoined(new_persona.clone()));
                personas.push(new_persona);
            }
            Err(_) => break 'convo, // fail safe: stop and synthesize with what's gathered
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

enum ModeratorVerdict {
    Converged,
    Continue,
    /// The conversation would benefit from a new voice — its name and a
    /// one-sentence personality/perspective blurb.
    AddPersona {
        name: String,
        blurb: String,
    },
}

async fn moderator_check(
    provider: &OpenRouter,
    meta_model: &str,
    topic: &str,
    discussion: &[(String, String)],
) -> Result<ModeratorVerdict, String> {
    let transcript = render_discussion(discussion).unwrap_or_default();
    let prompt = format!(
        "A panel of personas is having a live conversation about this message:\n\n{topic}\n\n\
         Conversation so far:\n\n{transcript}\n\nDecide one of three things and reply with ONLY \
         one line, no other text:\n\
         - If they've reached a conclusion (agreement, a clear resolution, or a settled \
         trade-off), reply exactly: CONVERGED\n\
         - If the conversation is missing an important point of view that would meaningfully \
         change it, reply exactly: ADD: <short persona name> | <one-sentence personality and \
         perspective>\n\
         - Otherwise, reply exactly: CONTINUE"
    );
    let raw = provider
        .complete(meta_model, vec![ChatMessage::text("user", prompt)])
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_moderator_verdict(&raw))
}

fn parse_moderator_verdict(raw: &str) -> ModeratorVerdict {
    // ASCII-only uppercasing so byte offsets stay valid for slicing `raw`
    // (full Unicode `to_uppercase` can change a string's byte length).
    let upper = raw.to_ascii_uppercase();
    if let Some(rest) = upper.find("ADD:").map(|i| &raw[i + 4..]) {
        if let Some((name, blurb)) = rest.split_once('|') {
            let name = name.trim().trim_matches('*').to_string();
            let blurb = blurb.lines().next().unwrap_or("").trim().to_string();
            if !name.is_empty() && !blurb.is_empty() {
                return ModeratorVerdict::AddPersona { name, blurb };
            }
        }
    }
    if upper.contains("CONVERGED") {
        return ModeratorVerdict::Converged;
    }
    ModeratorVerdict::Continue
}

async fn synthesize(
    provider: &OpenRouter,
    meta_model: &str,
    topic: &str,
    discussion: &[(String, String)],
) -> Result<String> {
    let transcript = render_discussion(discussion).unwrap_or_default();
    let prompt = format!(
        "A panel of personas had this conversation about a message:\n\n{topic}\n\n\
         Conversation:\n\n{transcript}\n\nWrite one final reply to the original message that \
         states the conclusion this conversation actually reached — the agreement, resolution, \
         or trade-off they settled on — as a clear, useful answer. If they genuinely disagreed \
         to the end, say so and give the best-supported call. Markdown allowed. Don't mention \
         that this came from a panel — just answer well, informed by the conversation."
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

    #[test]
    fn parse_moderator_verdict_recognizes_converged() {
        assert!(matches!(
            parse_moderator_verdict("CONVERGED"),
            ModeratorVerdict::Converged
        ));
        assert!(matches!(
            parse_moderator_verdict("  converged.\n"),
            ModeratorVerdict::Converged
        ));
    }

    #[test]
    fn parse_moderator_verdict_defaults_to_continue() {
        assert!(matches!(
            parse_moderator_verdict("CONTINUE"),
            ModeratorVerdict::Continue
        ));
        assert!(matches!(
            parse_moderator_verdict("something unexpected"),
            ModeratorVerdict::Continue
        ));
    }

    #[test]
    fn parse_moderator_verdict_extracts_name_and_blurb_for_add() {
        match parse_moderator_verdict("ADD: Realist | grounds the discussion in constraints") {
            ModeratorVerdict::AddPersona { name, blurb } => {
                assert_eq!(name, "Realist");
                assert_eq!(blurb, "grounds the discussion in constraints");
            }
            _ => panic!("expected AddPersona"),
        }
    }

    #[test]
    fn parse_moderator_verdict_falls_back_to_continue_on_malformed_add() {
        assert!(matches!(
            parse_moderator_verdict("ADD: no separator here"),
            ModeratorVerdict::Continue
        ));
    }
}
