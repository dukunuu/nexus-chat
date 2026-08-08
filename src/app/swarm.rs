//! `/swarm`: a per-session roster of persona+model rows that, when swarm
//! mode is on, turns a normal chat message into a live conversation between
//! personas. Conversation proceeds in rounds: every persona gets a chance to
//! respond before the moderator can decide whether the panel has converged.
//! A synthesis reply is then written as the discussion's canonical assistant
//! message.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use super::{App, Popup, SwarmPopupMode};
use crate::app::backends::Backends;
use crate::db::Persona;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{ChatMessage, ChatParams, StreamEvent};
use crate::tools::ToolBox;

/// Maximum complete panel rounds before synthesis. Every persona present at
/// the start of a round gets one response opportunity in that round.
const MAX_ROUNDS: usize = 4;
/// Tool round-trips available to each persona response.
const SWARM_PERSONA_MAX_TOOL_ITERS: usize = 8;

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

    /// Queue the selected persona (or a new row) as a small structured file
    /// for `$EDITOR`. Format: `name`, `model`, `---`, then free-form blurb.
    pub(crate) fn queue_swarm_persona_editor(&mut self, new: bool) -> Result<()> {
        if new {
            self.swarm_cache.push(Persona {
                name: String::new(),
                model: self.current_model.clone().unwrap_or_default(),
                blurb: String::new(),
            });
            self.swarm_selected = self.swarm_cache.len() - 1;
        }
        let Some(persona) = self.swarm_cache.get(self.swarm_selected) else {
            return Ok(());
        };
        let path =
            std::env::temp_dir().join(format!("nexus-chat-persona-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            format!(
                "name: {}\nmodel: {}\n---\n{}\n",
                persona.name, persona.model, persona.blurb
            ),
        )?;
        self.swarm_popup_mode = SwarmPopupMode::Browse;
        self.pending_editor = Some(super::PendingEditor::Persona(path));
        self.status = "opening persona in $EDITOR…".to_string();
        Ok(())
    }

    /// Apply a persona file after `$EDITOR` exits. Leaving a newly-created row
    /// unnamed cancels it; malformed existing edits leave the roster intact.
    pub(crate) fn apply_swarm_persona_editor(&mut self, path: &std::path::Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let _ = std::fs::remove_file(path);
        match parse_persona_editor(&text) {
            Ok(persona) => {
                if let Some(row) = self.swarm_cache.get_mut(self.swarm_selected) {
                    *row = persona;
                }
                self.save_swarm_roster()?;
                self.status = "persona saved".to_string();
            }
            Err(e) => {
                if self
                    .swarm_cache
                    .get(self.swarm_selected)
                    .is_some_and(|p| p.name.trim().is_empty())
                {
                    self.swarm_cache.remove(self.swarm_selected);
                    self.save_swarm_roster()?;
                }
                self.status = format!("persona edit ignored: {e}");
            }
        }
        Ok(())
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
        let default_model = self.current_model.clone().unwrap_or_default();
        let Some((default_provider, raw_default_model)) =
            self.resolve_model_backend(&default_model)
        else {
            self.open_login_popup();
            return;
        };
        let (meta_provider, raw_meta_model) = self
            .resolve_feature_model_backend(&self.research_model, OpenRouter::default_research_model)
            .unwrap_or_else(|| (default_provider.clone(), raw_default_model.clone()));
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

        let swarm_session_id = session.id.clone();
        let task = tokio::spawn(run_swarm_turn(SwarmTurnOptions {
            backends: self.backends.clone(),
            meta_provider,
            raw_meta_model,
            personas,
            default_model,
            default_provider,
            raw_default_model,
            base_history,
            toolbox: self.toolbox.clone(),
            user_message,
            session_id: session.id,
            tx,
        }));
        self.swarm_abort = Some(task.abort_handle());
        self.swarm_session = Some(swarm_session_id);
    }

    /// Stop the running swarm immediately. Persona model/tool streams are
    /// children of the aborted orchestration task and are dropped with it.
    pub(crate) fn stop_swarm(&mut self) {
        if let Some(abort) = self.swarm_abort.take() {
            abort.abort();
        }
        if self.swarm_rx.take().is_some() {
            if let Some(id) = self.swarm_session.take() {
                let _ = self
                    .db
                    .upsert_research_stage_message(&id, "swarm", "stopped by user");
            }
            self.status = "swarm stopped".to_string();
            self.popup = Popup::None;
        } else {
            self.status = "no swarm is running".to_string();
        }
    }

    /// A swarm turn update: persist it, and mirror it into the live
    /// transcript if the session it belongs to is the one being viewed.
    /// `None` = the job's channel closed (fires once, right after the last update).
    // Long by design (event dispatch).
    #[allow(clippy::too_many_lines)]
    pub fn on_swarm_update(&mut self, r: Option<SwarmMsg>) {
        let Some((session_id, update)) = r else {
            self.swarm_rx = None;
            self.swarm_abort = None;
            self.swarm_session = None;
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
                let _ = self
                    .db
                    .upsert_research_stage_message(&session_id, "swarm", &s);
                if viewing {
                    let text = crate::db::stage_content("swarm", &s);
                    if let Some(row) = self.messages.iter_mut().rev().find(|m| {
                        m.role == "research_stage"
                            && (m.content == "swarm" || m.content.starts_with("swarm:"))
                    }) {
                        row.content = text;
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
                    self.status = format!("swarm: {s}");
                }
            }
            SwarmUpdate::Reply {
                persona,
                model,
                content,
            } => {
                if self
                    .db
                    .add_persona_message(&session_id, &content, &persona, &model)
                    .is_ok()
                    && viewing
                {
                    self.messages.push(crate::db::Message {
                        role: "assistant".to_string(),
                        content,
                        model: Some(model),
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
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
                        role: "assistant".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: Some("Discussed".to_string()),
                        persona: None,
                    });
                    self.status = "swarm turn complete".to_string();
                    self.maybe_generate_title();
                    self.maybe_extract_memory();
                    self.maybe_compact();
                }
            }
            SwarmUpdate::Error(e) => {
                // Keep failures separate from the upserted progress row so a
                // later round/tool update cannot overwrite and erase them.
                let _ = self
                    .db
                    .add_error_message(&session_id, &format!("swarm: {e}"));
                if viewing {
                    self.messages.push(crate::db::Message {
                        role: "error".to_string(),
                        content: format!("swarm: {e}"),
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        persona: None,
                    });
                    self.status = format!("swarm error: {e}");
                }
            }
        }
    }
}

fn parse_persona_editor(text: &str) -> Result<Persona, String> {
    let mut lines = text.lines();
    let name = lines
        .next()
        .and_then(|line| line.strip_prefix("name:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "first line must be `name: <non-empty name>`".to_string())?;
    let model = lines
        .next()
        .and_then(|line| line.strip_prefix("model:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "second line must be `model: <model id>`".to_string())?;
    let remaining: Vec<&str> = lines.collect();
    let separator = remaining
        .iter()
        .position(|line| line.trim() == "---")
        .ok_or_else(|| "missing `---` before the blurb".to_string())?;
    let blurb = remaining[separator + 1..].join("\n").trim().to_string();
    Ok(Persona {
        name: name.to_string(),
        model: model.to_string(),
        blurb,
    })
}

#[allow(clippy::too_many_arguments)]
/// Everything needed to start one swarm conversation turn: the persona
/// roster, the model/provider config, the conversation history, and the
/// orchestration plumbing (toolbox, session identity, update channel).
pub struct SwarmTurnOptions {
    pub backends: Backends,
    pub meta_provider: OpenRouter,
    pub raw_meta_model: String,
    pub personas: Vec<Persona>,
    pub default_model: String,
    pub default_provider: OpenRouter,
    pub raw_default_model: String,
    pub base_history: Vec<ChatMessage>,
    pub toolbox: Arc<ToolBox>,
    pub user_message: String,
    pub session_id: String,
    pub tx: mpsc::UnboundedSender<SwarmMsg>,
}

// Long by design (roundtable orchestration).
#[allow(clippy::too_many_lines)]
async fn run_swarm_turn(opts: SwarmTurnOptions) {
    let SwarmTurnOptions {
        backends,
        meta_provider,
        raw_meta_model,
        mut personas,
        default_model,
        default_provider,
        raw_default_model,
        base_history,
        toolbox,
        user_message,
        session_id,
        tx,
    } = opts;
    let send = |u: SwarmUpdate| {
        let _ = tx.send((session_id.clone(), u));
    };
    if personas.is_empty() {
        send(SwarmUpdate::Progress("suggesting personas…".to_string()));
        match suggest_personas(
            &meta_provider,
            &raw_meta_model,
            &user_message,
            &default_model,
        )
        .await
        {
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

    // (persona name, reply content), in speaking order — a running transcript
    // every persona converses through. A successful reply marks that persona
    // as having answered; the moderator is gated on every current persona
    // having answered at least once and on the current round being complete.
    let mut discussion: Vec<(String, String)> = Vec::new();
    // Track roster positions, not names: users may configure duplicate names,
    // but every row still deserves its own response opportunity.
    let mut answered: HashSet<usize> = HashSet::new();
    'convo: for round in 1..=MAX_ROUNDS {
        // Snapshot the roster for this round. Personas can only be added by the
        // moderator after the round, so everyone in this snapshot gets exactly
        // one opportunity before moderation.
        let round_personas = personas.clone();
        let round_size = round_personas.len();
        for (idx, p) in round_personas.into_iter().enumerate() {
            send(SwarmUpdate::Progress(format!(
                "round {round}/{MAX_ROUNDS} · persona {}/{} — {} is responding",
                idx + 1,
                round_size,
                p.name
            )));

            let mut messages = base_history.clone();
            messages.push(ChatMessage::text(
                "system",
                format!(
                    "You are {} in a live group conversation with the other personas below. \
                     Your persona: {}. This discussion proceeds in rounds and every persona \
                     gets one response per round. Speak naturally and briefly (a few sentences), \
                     stay in character, and actually engage with what the prior speakers said — \
                     agree, push back, build on it, or ask a question. Use the available tools \
                     when they would improve your answer. Don't just restate your opening \
                     position every round.",
                    p.name, p.blurb
                ),
            ));
            match render_discussion(&discussion) {
                Some(transcript) => messages.push(ChatMessage::text(
                    "user",
                    format!(
                        "Conversation so far:\n\n{transcript}\n\nRound {round}: it's your chance \
                         to respond, {}. Engage with what was said.",
                        p.name
                    ),
                )),
                None => messages.push(ChatMessage::text(
                    "user",
                    format!(
                        "Round {round}: you're opening the discussion, {}. Give your first take.",
                        p.name
                    ),
                )),
            }

            let (persona_provider, raw_persona_model) =
                backends.resolve(&p.model).unwrap_or_else(|| {
                    send(SwarmUpdate::Error(format!(
                        "{} model unavailable ({}); using session model",
                        p.name, p.model
                    )));
                    (default_provider.clone(), raw_default_model.clone())
                });
            let persona_toolbox = toolbox.clone();
            let tools = persona_toolbox.defs();
            let (mut rx, abort) = persona_provider.stream_chat(
                raw_persona_model,
                messages,
                ChatParams::default(),
                tools,
                persona_toolbox,
                SWARM_PERSONA_MAX_TOOL_ITERS,
            );
            let abort = super::AbortOnDrop(abort);
            let response = timeout(Duration::from_mins(2), async {
                let mut content = String::new();
                while let Some(event) = rx.recv().await {
                    match event {
                        StreamEvent::Token(token) => content.push_str(&token),
                        StreamEvent::Status(status) => send(SwarmUpdate::Progress(format!(
                            "round {round}/{MAX_ROUNDS} — {}: {status}",
                            p.name
                        ))),
                        StreamEvent::ToolCall {
                            name,
                            arguments,
                            result,
                        } => {
                            let summary = crate::app::tool_call_summary(&name, &arguments, &result);
                            send(SwarmUpdate::Progress(format!(
                                "round {round}/{MAX_ROUNDS} — {}: {summary}",
                                p.name
                            )));
                        }
                        StreamEvent::Error(error) => return Err(anyhow::anyhow!(error)),
                        StreamEvent::Done => break,
                        _ => {}
                    }
                }
                if content.trim().is_empty() {
                    Err(anyhow::anyhow!("returned an empty response"))
                } else {
                    Ok(content)
                }
            })
            .await;
            let response = if let Ok(result) = response {
                result
            } else {
                abort.0.abort();
                Err(anyhow::anyhow!("timed out after 120s"))
            };
            match response {
                Ok(content) => {
                    send(SwarmUpdate::Reply {
                        persona: p.name.clone(),
                        model: p.model.clone(),
                        content: content.clone(),
                    });
                    answered.insert(idx);
                    discussion.push((p.name, content));
                }
                Err(e) => send(SwarmUpdate::Error(format!("{} failed: {e}", p.name))),
            }
        }

        // The hard cap synthesizes after the final complete round. Never ask
        // the moderator to add someone who would have no round left to speak.
        if round == MAX_ROUNDS {
            break 'convo;
        }
        if !all_personas_answered(personas.len(), &answered) {
            let missing = personas
                .iter()
                .enumerate()
                .filter(|(idx, _)| !answered.contains(idx))
                .map(|(_, p)| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            send(SwarmUpdate::Progress(format!(
                "round {round} complete — waiting for replies from: {missing}"
            )));
            continue;
        }

        send(SwarmUpdate::Progress(format!(
            "round {round} complete — moderator checking for a conclusion…"
        )));
        match moderator_check(&meta_provider, &raw_meta_model, &user_message, &discussion).await {
            Ok(ModeratorVerdict::Converged) | Err(_) => break 'convo,
            Ok(ModeratorVerdict::Continue) => {}
            Ok(ModeratorVerdict::AddPersona { name, blurb }) => {
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
        }
    }

    send(SwarmUpdate::Progress("writing synthesis…".to_string()));
    match synthesize(&meta_provider, &raw_meta_model, &user_message, &discussion).await {
        Ok(content) => send(SwarmUpdate::Synthesis(content)),
        Err(e) => send(SwarmUpdate::Error(format!("synthesis failed: {e}"))),
    }
}

fn all_personas_answered(persona_count: usize, answered: &HashSet<usize>) -> bool {
    persona_count > 0 && (0..persona_count).all(|idx| answered.contains(&idx))
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

#[derive(serde::Deserialize)]
struct Suggested {
    name: String,
    blurb: String,
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
    let raw = timeout(
        Duration::from_mins(1),
        provider.complete(meta_model, vec![ChatMessage::text("user", prompt)]),
    )
    .await
    .map_err(|_| "timed out after 60s".to_string())?
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
    let raw = timeout(
        Duration::from_secs(30),
        provider.complete(meta_model, vec![ChatMessage::text("user", prompt)]),
    )
    .await
    .map_err(|_| "timed out after 30s".to_string())?
    .map_err(|e| e.to_string())?;
    Ok(parse_moderator_verdict(&raw))
}

fn parse_moderator_verdict(raw: &str) -> ModeratorVerdict {
    // ASCII-only uppercasing so byte offsets stay valid for slicing `raw`
    // (full Unicode `to_uppercase` can change a string's byte length).
    let upper = raw.to_ascii_uppercase();
    if let Some(rest) = upper.find("ADD:").map(|i| &raw[i + 4..])
        && let Some((name, blurb)) = rest.split_once('|')
    {
        let name = name.trim().trim_matches('*').to_string();
        let blurb = blurb.lines().next().unwrap_or("").trim().to_string();
        if !name.is_empty() && !blurb.is_empty() {
            return ModeratorVerdict::AddPersona { name, blurb };
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
    timeout(
        Duration::from_secs(90),
        provider.complete(meta_model, vec![ChatMessage::text("user", prompt)]),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out after 90s"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persona_editor_round_trips_name_model_and_multiline_blurb() {
        let persona = parse_persona_editor(
            "name: Skeptic\nmodel: codex:gpt-5.4-mini\n---\npokes holes\nand checks evidence\n",
        )
        .unwrap();
        assert_eq!(persona.name, "Skeptic");
        assert_eq!(persona.model, "codex:gpt-5.4-mini");
        assert_eq!(persona.blurb, "pokes holes\nand checks evidence");
        assert!(parse_persona_editor("name: \nmodel: m\n---\n").is_err());
    }

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
    fn moderator_gate_requires_every_current_persona_row_to_have_answered() {
        let mut answered = HashSet::from([0usize]);
        assert!(!all_personas_answered(2, &answered));

        answered.insert(1);
        assert!(all_personas_answered(2, &answered));
        assert!(
            !all_personas_answered(3, &answered),
            "a newly joined persona must get a round before moderation"
        );
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
