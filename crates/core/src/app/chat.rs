// Casts here are on bounded values: token counts, byte sizes, and
// selection indices — never on unbounded input. JSON-derived indices in
// provider/tools go through try_from instead.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
use anyhow::Result;
use chrono::Utc;
use std::fmt::Write as _;
use tokio::sync::mpsc;

use crate::db::Message;
use crate::prompts;
use crate::provider::profile::{ModelProfile, profile_for};
use crate::provider::{ChatMessage, ChatParams, StreamEvent, ToolCall, Usage};

use super::{App, SPINNER_COLORS, SpinnerColor, THINKING, WorkMode, parse_topic, verbosity_clause};

/// The Mermaid kinds the prompt advertises; the TUI tests that each one renders
/// so the prompt and renderer cannot drift.
pub const MERMAID_PREVIEW_EXAMPLES: &[&str] = &[
    "flowchart LR\nA-->B",
    "sequenceDiagram\nAlice->>Bob: Hello",
    "stateDiagram-v2\n[*] --> Idle",
];

impl App {
    /// Send one chat message: validation, session auto-creation, persistence.
    #[allow(clippy::too_many_lines)] // validation, session creation, ordered skill/user persistence
    pub fn send_message(&mut self, text: String) -> Result<()> {
        if let Some(session) = self.session.as_ref()
            && let Some(task) = self.chat_task_for_session(&session.id)
        {
            self.push_status(format!(
                "wait — response still streaming in: {}",
                task.session_title
            ));
            self.push_composer_set(&text);
            return Ok(());
        }
        if self.chat_task_count() >= super::MAX_CHAT_TASKS {
            self.push_status(format!(
                "chat task limit reached ({})",
                super::MAX_CHAT_TASKS
            ));
            self.push_composer_set(&text);
            return Ok(());
        }
        if !self.backends.any() {
            self.push_status("set your API key first with /login".to_string());
            self.push_composer_set(&text);
            return Ok(());
        }
        // A new turn starts a fresh tally: the rate beside the context window
        // describes this answer, not the previous one.
        self.turn_cache = super::CacheTally::default();
        let Some(model) = self.current_model.clone() else {
            self.push_status("pick a model first with /model".to_string());
            self.push_composer_set(&text);
            return Ok(());
        };

        let session_id = self.ensure_session(&text, model)?;

        if let Some(name) = self.forced_skill.take()
            && let Some(skill) = self.skills.iter().find(|s| s.name == name)
        {
            let body = std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).to_string())
                .unwrap_or_default();
            let content = format!("{name}\n{body}");
            if !self.incognito {
                self.db.add_skill_message(&session_id, &content)?;
            }
            self.messages.push(Message {
                role: "skill".to_string(),
                content,
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            });
        }
        if !self.incognito {
            self.db.add_user_message(&session_id, &text)?;
        }
        self.messages.push(Message {
            role: "user".to_string(),
            content: text,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            cost: None,
            phrase: None,
            persona: None,
            created_at: None,
        });

        if self.session.as_ref().is_some_and(|s| s.swarm_mode) {
            self.start_swarm_turn();
            return Ok(());
        }
        self.start_stream()
    }

    /// The active session's id, creating it (titled from `text`) when none is
    /// open. Refresh the memory snapshot here as well as in `/new` so edits
    /// made while no session was open are picked up.
    fn ensure_session(&mut self, text: &str, model: String) -> Result<String> {
        if let Some(session) = &self.session {
            return Ok(session.id.clone());
        }
        self.mask_through = 0;
        self.refresh_memory_snapshot();
        let title = title_from(text);
        let s = if self.incognito {
            crate::db::Session {
                id: uuid::Uuid::new_v4().to_string(),
                title,
                model,
                slug: None,
                created_at: Utc::now().to_rfc3339(),
                compact_summary: None,
                compact_through: 0,
                web_mode: self.web_mode,
                swarm_mode: false,
                kind: "chat".to_string(),
                research_parent_id: None,
            }
        } else {
            let mut s = self
                .db
                .create_session(&title, &model, &self.active_space.id, "chat")?;
            // Carry a pre-session `/web` toggle onto the session it creates.
            if self.web_mode {
                s.web_mode = true;
                let _ = self.db.set_session_web_mode(&s.id, true);
            }
            s
        };
        if !self.incognito && self.work_mode == WorkMode::Agent {
            self.db.set_session_workspace(
                &s.id,
                self.workspace.as_ref().map(|workspace| workspace.root()),
            )?;
        }
        let id = s.id.clone();
        self.session = Some(s);
        Ok(id)
    }

    /// Regenerate the last reply, retaining its user message and rejecting its trace steps.
    pub fn retry_last_reply(&mut self) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            self.push_status("nothing to retry — no active session");
            return Ok(());
        };
        if self.is_streaming() {
            self.push_status("wait — response still streaming");
            return Ok(());
        }
        if session.swarm_mode || session.kind == "research" {
            self.push_status("/retry is not available in swarm or research sessions");
            return Ok(());
        }
        let Some(user_index) = self
            .messages
            .iter()
            .rposition(|message| matches!(message.role.as_str(), "user" | "user_command"))
        else {
            self.push_status("nothing to retry — no user message");
            return Ok(());
        };
        if user_index + 1 == self.messages.len() {
            self.push_status("nothing to retry — no reply after the last user message");
            return Ok(());
        }
        let session_id = session.id.clone();
        let changed_files = self.work_mode == WorkMode::Agent
            && self
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.turn_changed_files(&session_id));
        if !self.incognito {
            for id in self.db.reply_ids_after_last_user(&session_id)? {
                self.db.delete_message(&id)?;
            }
        }
        self.messages.truncate(user_index + 1);
        if self.mask_through > self.messages.len() {
            self.mask_through = self.messages.len();
            if !self.incognito {
                let _ = self.db.set_session_mask(&session_id, self.mask_through);
            }
        }
        self.push_history_invalidated();
        if let Some((turn_session, turn_id)) = &self.last_turn
            && turn_session == &session_id
            && let Some(store) = &self.trace_store
            && let Ok(steps) = store.steps_for_turn(turn_id)
        {
            for step in steps {
                let _ = store.add_label(&step, "rejected", "retry");
            }
        }
        self.start_stream()?;
        if changed_files {
            self.push_status(
                "retrying · the replaced turn's file changes are kept · /undo walks back one agent turn at a time",
            );
        }
        Ok(())
    }

    /// Restore the files the last agent turn changed, unless changed since.
    pub fn undo_last_turn(&mut self) -> Result<()> {
        if self.is_streaming() {
            self.push_status("wait — response still streaming");
            return Ok(());
        }
        let Some(workspace) = self
            .workspace
            .clone()
            .filter(|_| self.work_mode == WorkMode::Agent)
        else {
            self.push_status("/undo works in agent mode · switch with /agent <path>");
            return Ok(());
        };
        let report = match &self.session {
            Some(session) => workspace.undo_last(&session.id),
            None => Ok(None),
        };
        match report {
            Ok(Some(report)) => self.push_status(report.summary()),
            Ok(None) => self.push_status("nothing to undo — no agent file changes in this session"),
            Err(error) => self.push_status(format!("undo failed: {error}")),
        }
        Ok(())
    }

    /// Where `!` commands run: the workspace root in agent mode, else the
    /// active space's directory (created if missing).
    pub fn user_command_cwd(&self) -> std::path::PathBuf {
        if let Some(workspace) = self
            .workspace
            .as_ref()
            .filter(|_| self.work_mode == WorkMode::Agent)
        {
            return workspace.root().to_path_buf();
        }
        let _ = self.space.ensure_space_dir(&self.active_space.name);
        self.space.space_dir(&self.active_space.name)
    }

    /// `!cmd`: run `sh -c <cmd>` as the user — full environment, no gate, no
    /// sandbox — on a background task. Its scrubbed, capped output becomes a
    /// `user_command` row the model sees next turn (`finish_user_command`).
    pub fn run_user_command(&mut self, cmd: String) -> Result<()> {
        if !self.interactive_ui {
            self.push_status("!commands run only in the terminal UI");
            return Ok(());
        }
        if self.is_streaming() || self.user_command_rx.is_some() {
            self.push_status(if self.is_streaming() {
                "wait — response still streaming"
            } else {
                "wait — a command is still running"
            });
            self.push_composer_set(format!("!{cmd}"));
            return Ok(());
        }
        let cwd = self.user_command_cwd();
        let session_id =
            self.ensure_session(&cmd, self.current_model.clone().unwrap_or_default())?;
        let scrubber = crate::trace::Scrubber::new(self.saved_secrets());
        let incognito = self.incognito;
        let (tx, rx) = mpsc::unbounded_channel();
        self.user_command_rx = Some(rx);
        self.push_status(format!("running: {cmd}"));
        tokio::spawn(async move {
            let (exit, started, output) =
                capture_user_command(&cmd, &cwd, USER_COMMAND_TIMEOUT).await;
            let _ = tx.send(UserCommandRun {
                session_id,
                incognito,
                cmd,
                cwd: cwd.display().to_string(),
                exit,
                started,
                output: crate::tools::cap_head_tail(&scrubber.scrub(&output), USER_COMMAND_CHARS),
            });
        });
        Ok(())
    }

    /// Store a finished captured `!` command in the session it started in.
    pub(super) fn finish_user_command(&mut self, run: &UserCommandRun) -> Result<()> {
        self.push_status(format!(
            "ran: {} · {}",
            run.cmd,
            user_command_outcome_label(run.exit, run.started)
        ));
        let content = user_command_row(
            &run.cmd,
            &run.cwd,
            run.exit,
            run.started,
            &run.output,
            false,
        );
        self.store_user_command(&run.session_id, run.incognito, content)
    }

    /// Record an interactive `!` command the terminal UI ran on the real
    /// terminal. Its output never passed through nexus.
    pub fn record_user_command(
        &mut self,
        cmd: &str,
        exit: Option<i32>,
        started: bool,
    ) -> Result<()> {
        if !self.interactive_ui {
            self.push_status("!commands run only in the terminal UI");
            return Ok(());
        }
        let cwd = self.user_command_cwd().display().to_string();
        let session_id =
            self.ensure_session(cmd, self.current_model.clone().unwrap_or_default())?;
        self.push_status(format!(
            "ran: {cmd} · {}",
            user_command_outcome_label(exit, started)
        ));
        let content = user_command_row(
            cmd,
            &cwd,
            exit,
            started,
            USER_COMMAND_INTERACTIVE_NOTE,
            true,
        );
        self.store_user_command(&session_id, self.incognito, content)?;
        self.push_history_invalidated();
        Ok(())
    }

    fn store_user_command(
        &mut self,
        session_id: &str,
        incognito: bool,
        content: String,
    ) -> Result<()> {
        if !incognito {
            self.db.add_user_command_message(session_id, &content)?;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == session_id)
        {
            self.messages.push(Message {
                role: "user_command".to_string(),
                content,
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            });
        }
        Ok(())
    }

    fn model_profile(&self) -> ModelProfile {
        profile_for(
            self.models.iter().find(|model| {
                self.current_model.as_deref() == Some(super::composite_id(model).as_str())
            }),
            self.saved.local.as_ref(),
        )
    }

    /// After a reply, clear old observations in bulk once the token trigger is met.
    pub fn maybe_mask(&mut self) {
        if !self.experiments.mask_tool_results || self.is_streaming() {
            return;
        }
        let trigger = if self.experiments.mask_trigger_tokens > 0 {
            self.experiments.mask_trigger_tokens
        } else if let Some(limit) = self.context_limit() {
            limit / 2
        } else {
            return;
        };
        if self.context_used() < trigger {
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let keep = self.experiments.mask_keep_rounds as usize;
        let through = if keep == 0 {
            self.messages.len()
        } else {
            let Some((index, _)) = self
                .messages
                .iter()
                .enumerate()
                .filter(|(i, m)| {
                    m.role == "tool_call" && (*i == 0 || self.messages[*i - 1].role != "tool_call")
                })
                .rev()
                .nth(keep - 1)
            else {
                return;
            };
            index
        };
        if through <= self.mask_through {
            return;
        }
        // Already compacted or masked observations cannot save any more context.
        let start = self
            .mask_through
            .max(self.messages.len() - self.effective_messages().len());
        let mut count = 0;
        let mut chars = 0;
        for message in &self.messages[start.min(through)..through] {
            if message.role == "tool_call"
                && !Self::excluded_from_model_history(message)
                && let Some(row) = parse_tool_call_row(&message.content)
                && crate::tools::classify_tool_result(&row.result, "").is_none()
            {
                count += 1;
                chars += row.result.chars().count();
            }
        }
        let tokens = chars / 4;
        if tokens < 2_000 {
            return;
        }
        if !self.incognito {
            let _ = self.db.set_session_mask(&session.id, through);
        }
        self.mask_through = through;
        self.bump_cache_epoch();
        self.push_status(format!(
            "masked {count} old tool results (~{tokens} tokens) · experiment"
        ));
    }

    /// Conversation estimate with cleared observations counted at their masked size.
    pub(super) fn effective_conversation_chars(&self) -> usize {
        let effective = self.effective_messages();
        let offset = self.messages.len() - effective.len();
        effective
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != "compaction")
            .map(|(index, m)| {
                let chars = m.content.chars().count();
                if self.experiments.mask_tool_results
                    && offset + index < self.mask_through
                    && m.role == "tool_call"
                    && let Some(row) = parse_tool_call_row(&m.content)
                    && crate::tools::classify_tool_result(&row.result, "").is_none()
                {
                    let note =
                        crate::tools::tool_result_masked_note(&row.call.name, &row.call.arguments);
                    return chars.saturating_sub(
                        serde_json::Value::String(row.result)
                            .to_string()
                            .chars()
                            .count(),
                    ) + serde_json::Value::String(note).to_string().chars().count();
                }
                chars
            })
            .sum()
    }

    /// The exact message list a completion request will carry: system prompt,
    /// compaction digest, then the effective conversation tail.
    /// User messages with images become multimodal parts for vision models, or
    /// get their stored descriptions appended as text for everything else.
    pub fn build_history(&mut self) -> Vec<ChatMessage> {
        let mut history: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 3);
        history.push(ChatMessage::text("system", self.system_prompt()));
        // Include space-file images inline for vision models.
        if self.current_model_supports_images()
            && let Some(img_msg) = self.space_images_message()
        {
            history.push(img_msg);
        }
        // If this session has been auto-compacted, send the digest instead of
        // the raw messages it covers — only the tail after it goes verbatim.
        if let Some(summary) = self
            .session
            .as_ref()
            .and_then(|s| s.compact_summary.clone())
        {
            history.push(ChatMessage::text(
                "system",
                format!("Summary of earlier conversation (auto-compacted for length):\n{summary}"),
            ));
        }
        let vision = self.current_model_supports_images();
        let result_cap = self.model_profile().result_cap;
        // Dedup state for replayed tool results: (tool, arguments) → latest
        // full result. A row whose result is byte-identical to the latest
        // one for the same tool+arguments (e.g. a re-read of an unchanged
        // file) is replayed as a one-line note instead of the full copy, so
        // duplicates don't re-enter the context on every request. The live
        // tool loop applies the same rule with the same state (seeded from
        // this history), keeping the prompt-cache prefix continuous.
        let mut seen_results: std::collections::HashMap<(String, String), String> =
            std::collections::HashMap::new();
        let effective = self.effective_messages();
        let mask_through = if self.experiments.mask_tool_results {
            self.mask_through
                .saturating_sub(self.messages.len() - effective.len())
        } else {
            0
        };
        let tool_images_dir = self
            .toolbox
            .supports_images()
            .then(|| self.toolbox.space_files_dir())
            .flatten();
        let mut index = 0;
        while index < effective.len() {
            let m = &effective[index];
            // Replay past tool calls as real assistant/tool message pairs so
            // the model remembers what it already tried (and got back) in
            // prior turns — dropping these caused it to repeat the same
            // mistakes on file-writing tools with no memory of the failure.
            // Skip every row that must never reach the model (shared with
            // compaction, so a digest can't leak the same rows later):
            // background-job scratch, UI-only prompts, transport failures,
            // per-persona swarm round replies, and gate replies (whose
            // survey/plan sections are excluded, so a bare "the second
            // option" or "drop Q2" must not reach the model without context).
            if Self::excluded_from_model_history(m) {
                index += 1;
                continue;
            }
            if m.role == "tool_call" {
                append_tool_call_history(
                    &mut history,
                    effective,
                    &mut index,
                    &mut seen_results,
                    tool_images_dir.as_deref(),
                    result_cap,
                    mask_through,
                );
                continue;
            }
            let mut cm = if m.role == "user_command" {
                ChatMessage::text("user", user_command_replay(&m.content))
            } else if m.role == "skill" {
                let (name, body) = m
                    .content
                    .split_once('\n')
                    .unwrap_or((m.content.as_str(), ""));
                ChatMessage::text(
                    "user",
                    format!(
                        "The user invoked the skill '{name}'. Follow these instructions for the rest of this conversation:\n{body}"
                    ),
                )
            } else {
                ChatMessage::text(m.role.clone(), m.content.clone())
            };
            if m.role == "user" && vision {
                let images_dir = self.space.files_dir(&self.active_space.name);
                let mut images = Vec::new();
                let mut rest = m.content.as_str();
                while let Some(start) = rest.find("![") {
                    if let Some(end) = rest[start..].find(')') {
                        let inner = &rest[start + 2..start + end];
                        if let Some((_alt, file)) = inner.split_once("](") {
                            let path = images_dir.join(file);
                            if let Ok(bytes) = std::fs::read(&path) {
                                images.push(crate::app::transcribe::png_bytes_data_url(&bytes));
                            }
                        }
                        rest = &rest[start + end + 1..];
                    } else {
                        break;
                    }
                }
                cm.images = images;
            }
            history.push(cm);
            index += 1;
        }
        self.mark_cache_breakpoints(&mut history);
        history
    }

    /// Mark where the cacheable prefix ends, for models that cache only when
    /// asked. No-op for backends that cache automatically.
    ///
    /// Two breakpoints, on the boundaries [`App::bump_cache_epoch`] already
    /// keeps byte-identical for the life of a cache epoch:
    ///
    /// 1. the end of the leading system block (base prompt, space
    ///    instructions, file/app/script sections, memory snapshot, skills);
    /// 2. the last message before the live tail, so a tool loop replays over
    ///    a cached prefix instead of resending the whole conversation.
    ///
    /// Placing them anywhere later would invalidate the cache on every turn,
    /// which is the failure this exists to avoid.
    fn mark_cache_breakpoints(&self, history: &mut [ChatMessage]) {
        if !self.wants_explicit_cache_breakpoints() {
            return;
        }
        let last_system = history
            .iter()
            .position(|message| message.role != "system")
            .unwrap_or(history.len())
            .saturating_sub(1);
        if let Some(message) = history.get_mut(last_system) {
            message.cache_breakpoint = true;
        }
        // The tail message is the live one; the prefix worth caching ends
        // just before it. With fewer than two messages after the system
        // block there is no separate prefix to mark.
        if history.len() >= last_system + 3
            && let Some(message) = history.get_mut(history.len() - 2)
        {
            message.cache_breakpoint = true;
        }
    }

    /// Whether the active model caches only when the request marks a
    /// breakpoint. `OpenAI`, Gemini and `DeepSeek`-family models cache
    /// automatically and reject or ignore the marker; Anthropic-family models
    /// cache nothing without it.
    #[must_use]
    pub fn wants_explicit_cache_breakpoints(&self) -> bool {
        self.current_model
            .as_deref()
            .is_some_and(super::model_needs_cache_breakpoints)
    }

    /// Build history and fire one independently-routed streaming request.
    #[allow(clippy::too_many_lines)] // Request/profile setup and independent task registration.
    pub fn start_stream(&mut self) -> Result<()> {
        let Some(model) = self.current_model.clone() else {
            return Ok(());
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.push_status(format!(
                "model backend unavailable: {model} — pick another with /model"
            ));
            return Ok(());
        };
        let history = self.build_history();
        // Catalog capabilities can change underneath a persisted preference.
        // Keep the request, in-memory badge, and database in sync rather than
        // silently omitting a stale value while the picker still shows it.
        let stored_effort = self.reasoning.get(&model).cloned();
        let (reasoning_effort, reasoning_warning) = match stored_effort {
            Some(effort) if self.effort_accepted(&model, &effort) => (Some(effort), None),
            Some(effort) => {
                self.db.set_reasoning(&model, None)?;
                self.reasoning.remove(&model);
                (
                    None,
                    Some(format!("cleared unsupported reasoning {effort}: {model}")),
                )
            }
            None => (None, None),
        };
        self.push_status(reasoning_warning.unwrap_or_default());
        let profile = self.model_profile();
        let mut params = self.chat_params(reasoning_effort, provider.backend_tag());
        params.parallel_tool_calls = (!profile.parallel_calls).then_some(false);
        params.tool_result_cap = profile.result_cap;
        let tools = if profile.compact_tools {
            self.toolbox.defs_compact()
        } else {
            self.toolbox.defs()
        };
        if provider.backend_tag() == crate::provider::BackendTag::Local
            && !self.context_warned
            && let Some(limit) = self.context_limit()
        {
            let defs = serde_json::to_string(&tools)?;
            let tokens = (self.system_prompt().chars().count() + defs.chars().count()) as u64 / 4;
            if tokens > limit / 2 {
                self.push_status(format!(
                    "prompt + tool definitions use ~{tokens} of {limit} tokens — raise [local] context_length"
                ));
                self.context_warned = true;
            }
        }
        if self.work_mode == WorkMode::Agent
            && let (Some(workspace), Some(session)) = (&self.workspace, &self.session)
        {
            workspace.begin_turn(&session.id);
        }
        let (rx, abort) = provider.stream_chat(
            raw_model.clone(),
            history,
            params,
            tools,
            self.toolbox.clone(),
            crate::provider::openrouter::MAX_TOOL_ITERS,
        );
        let Some(session) = self.session.clone() else {
            return Ok(());
        };
        let backend = provider.backend_tag();
        let (thinking_idx, spinner_color) = pick_flavor();
        let task_id = self.next_chat_task_id;
        self.next_chat_task_id = self.next_chat_task_id.wrapping_add(1);
        let tx = self.chat_event_tx.clone();
        tokio::spawn(async move {
            let mut rx = rx;
            while let Some(event) = rx.recv().await {
                if tx.send(super::ChatEvent { task_id, event }).is_err() {
                    break;
                }
            }
            let _ = tx.send(super::ChatEvent {
                task_id,
                event: StreamEvent::Done,
            });
        });
        self.chat_tasks.insert(
            task_id,
            super::ChatTask {
                id: task_id,
                session_id: session.id,
                session_title: session.title,
                space_id: self.active_space.id.clone(),
                model,
                model_id: raw_model,
                backend,
                incognito: self.incognito,
                buffer: String::new(),
                thinking: String::new(),
                tool_status: None,
                usage: None,
                usage_row_id: None,
                started: std::time::Instant::now(),
                thinking_idx,
                spinner_color,
                abort,
            },
        );
        self.spinner_frame = 0;
        self.thinking_idx = thinking_idx;
        self.spinner_color = spinner_color;
        // New content is about to arrive below the conversation: the view
        // resets its scroll/pinning baseline (AppEvent::ViewportReset).
        self.push_viewport_reset();
        Ok(())
    }

    /// Build the main chat's parameters without starting a provider request.
    pub(super) fn chat_params(
        &mut self,
        reasoning_effort: Option<String>,
        backend: crate::provider::BackendTag,
    ) -> ChatParams {
        let mut params = ChatParams {
            reasoning_effort,
            prompt_cache_key: self
                .session
                .as_ref()
                .map(|session| self.prompt_cache_key_for(&session.id)),
            temperature: self.settings.temperature,
            top_p: self.settings.top_p,
            max_tokens: self.settings.max_tokens,
            ..ChatParams::default()
        };
        if self.training_enabled()
            && let Some(session_id) = self.session.as_ref().map(|session| session.id.clone())
            && let Some(store) = self.open_trace_store()
        {
            let turn_id = uuid::Uuid::new_v4().to_string();
            self.last_turn = Some((session_id.clone(), turn_id.clone()));
            params.trace = Some(crate::trace::TraceContext {
                store,
                session_id,
                space_id: self.active_space.id.clone(),
                turn_id,
                backend: backend.name().to_string(),
            });
        }
        params
    }

    /// Compatibility entry point for tests and synchronous callers. Runtime
    /// events use `on_chat_event`, which always includes the task id.
    #[cfg(test)]
    pub fn on_stream_event(&mut self, ev: StreamEvent) -> Result<()> {
        let task_id = self
            .active_chat_task()
            .map(|task| task.id)
            .or_else(|| self.chat_tasks.keys().next().copied());
        if let Some(task_id) = task_id {
            return self.on_chat_event(task_id, ev);
        }
        if let StreamEvent::ToolCall {
            id,
            reasoning,
            assistant_content,
            name,
            arguments,
            result,
        } = ev
        {
            let content = serde_json::json!({
                "id": id,
                "reasoning_content": reasoning,
                "assistant_content": assistant_content,
                "name": name,
                "arguments": arguments,
                "result": result,
            })
            .to_string();
            if let Some(session) = self.session.as_ref()
                && !self.incognito
            {
                let _ = self.db.add_tool_call_message(&session.id, &content);
            }
            self.messages.push(Message {
                role: "tool_call".to_string(),
                content,
                model: None,
                reasoning: None,
                tokens: None,
                secs: None,
                cost: None,
                phrase: None,
                persona: None,
                created_at: None,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one match per stream event; arms are short
    pub fn on_chat_event(&mut self, task_id: super::ChatTaskId, ev: StreamEvent) -> Result<()> {
        match ev {
            StreamEvent::Token(t) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.buffer.push_str(&t);
                }
            }
            StreamEvent::Reasoning(t) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.thinking.push_str(&t);
                }
            }
            StreamEvent::Usage(u) => {
                // The provider emits one Usage event per underlying API
                // request, including each tool-loop round. Keep every real
                // accounting event as its own row; overwriting one row for
                // the whole top-level chat turn made a long tool loop look as
                // if prompt tokens had gone down or vanished.
                //
                // OpenCode Zen may follow a real usage event with a
                // cost-only event. That event belongs to the most recent row
                // and is merged in place rather than logged separately.
                let has_tokens = u.prompt_tokens + u.completion_tokens > 0;
                if has_tokens {
                    self.last_cache_rate = u.cache_hit_rate();
                    self.turn_cache.observe(&u);
                }
                let Some(task) = self.chat_tasks.get_mut(&task_id) else {
                    return Ok(());
                };
                let merged = if has_tokens {
                    // A real event starts a new API-request record. Do not
                    // carry a previous request's provider cost into it.
                    u
                } else {
                    task.usage.map_or(u, |prev| Usage {
                        cost: u.cost.or(prev.cost),
                        ..prev
                    })
                };
                task.usage = Some(merged);
                let session_id = task.session_id.clone();
                let space_id = task.space_id.clone();
                let model_id = task.model_id.clone();
                let backend = task.backend;
                let incognito = task.incognito;
                let row_id = task.usage_row_id;
                if !incognito
                    && (merged.prompt_tokens > 0
                        || merged.completion_tokens > 0
                        || merged.cost.is_some())
                {
                    // Provider-reported cost wins; otherwise estimate from
                    // cache-aware catalog rates (None when unknown).
                    // Non-OpenRouter backends price via their OpenRouter
                    // catalog twin (see `Db::model_price`).
                    let cost_is_provider = merged.cost.is_some();
                    let cost = merged.cost.or_else(|| {
                        self.db.request_cost(
                            &model_id,
                            merged.prompt_tokens,
                            merged.completion_tokens,
                            merged.cache_read_tokens,
                            merged.cache_creation_tokens,
                        )
                    });
                    if has_tokens || row_id.is_none() {
                        let id = self.db.log_usage(
                            backend.name(),
                            &model_id,
                            merged.prompt_tokens,
                            merged.completion_tokens,
                            merged.cache_read_tokens,
                            merged.cache_creation_tokens,
                            merged.prompt_convention,
                            cost,
                            cost_is_provider,
                            Some(&session_id),
                            Some(&space_id),
                        );
                        if let Ok(id) = id
                            && let Some(task) = self.chat_tasks.get_mut(&task_id)
                        {
                            task.usage_row_id = Some(id);
                        }
                    } else if let Some(row_id) = row_id {
                        let _ = self.db.update_usage(
                            row_id,
                            merged.prompt_tokens,
                            merged.completion_tokens,
                            merged.cache_read_tokens,
                            merged.cache_creation_tokens,
                            cost,
                            cost_is_provider,
                        );
                    }
                }
            }
            StreamEvent::Status(s) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.tool_status = Some(s);
                }
            }
            StreamEvent::ToolCall {
                id,
                reasoning,
                assistant_content,
                name,
                arguments,
                result,
            } => {
                if ((name == "skills")
                    || (name == "skill_admin")
                    || (name == "install_skill" && result.starts_with("installed"))
                    || (name == "create_skill"
                        && (result.starts_with("created") || result.starts_with("updated"))))
                    && (result.starts_with("installed")
                        || result.starts_with("created")
                        || result.starts_with("updated"))
                {
                    self.reload_skills();
                }
                let Some(task) = self.chat_tasks.get(&task_id) else {
                    return Ok(());
                };
                let content = serde_json::json!({
                    "id": id,
                    "reasoning_content": reasoning,
                    "assistant_content": assistant_content,
                    "name": name,
                    "arguments": arguments,
                    "result": result,
                })
                .to_string();
                let target = task.session_id.clone();
                let incognito = task.incognito;
                let in_active_space = task.space_id == self.active_space.id;
                let viewing = self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.id == target);
                if !incognito {
                    let _ = self.db.add_tool_call_message(&target, &content);
                }
                if viewing {
                    self.messages.push(Message {
                        role: "tool_call".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        cost: None,
                        phrase: None,
                        persona: None,
                        created_at: None,
                    });
                }
                if viewing
                    && ((name == "propose_command" && result.starts_with("proposed to the user"))
                        || (name == "request_workspace" && result.starts_with("approved:")))
                {
                    let args =
                        serde_json::from_str::<serde_json::Value>(&arguments).unwrap_or_default();
                    let arg = |key: &str| args[key].as_str().unwrap_or_default().trim().to_string();
                    if name == "propose_command" {
                        // The status line is the view's wake-up to take it.
                        self.proposed_command = Some(format!("!{}", arg("cmd")));
                        self.push_status(arg("reason"));
                    } else {
                        // Applied when the reply finishes: never swap the toolbox mid-turn.
                        self.queued_workspace = Some((task_id, arg("root")));
                    }
                }
                // Generated images land on disk but aren't indexed until
                // rescan_files picks them up. Without this, they'd be missing
                // from files_cache and never OCR'd for descriptive naming.
                let media_generated = matches!(
                    name.as_str(),
                    "generate_image" | "generate_video" | "video_transform"
                ) || matches!(
                    name.as_str(),
                    "edit_video" | "extract_frame" | "stitch_videos"
                ) || (name == "media"
                    && serde_json::from_str::<serde_json::Value>(&arguments)
                        .ok()
                        .and_then(|v| v.get("action").and_then(|a| a.as_str()).map(str::to_string))
                        .is_some_and(|a| {
                            matches!(
                                a.as_str(),
                                "generate_image"
                                    | "generate_video"
                                    | "edit"
                                    | "extract_frame"
                                    | "stitch"
                            )
                        }));
                if in_active_space && media_generated {
                    self.rescan_files();
                }
            }
            StreamEvent::Done => self.finish_chat_task(task_id, None)?,
            StreamEvent::Error(e) => {
                self.finish_chat_task(task_id, Some(e))?;
            }
        }
        Ok(())
    }

    /// Cancel one in-flight chat task by id (kills any in-flight request and
    /// tool loop) and keep whatever text already arrived.
    pub fn cancel_chat_task(&mut self, task_id: super::ChatTaskId) -> Result<()> {
        if let Some(task) = self.chat_tasks.remove(&task_id) {
            task.abort.abort();
            self.finish_chat_task_state(task, None, true)?;
        }
        Ok(())
    }

    /// Esc while a response streams: abort the background chat task (kills any
    /// in-flight request and tool loop) and keep whatever text already arrived.
    pub fn stop_stream(&mut self) -> Result<()> {
        let Some(task_id) = self.active_chat_task().map(|task| task.id) else {
            return Ok(());
        };
        self.cancel_chat_task(task_id)
    }

    pub fn discard_chat_task(&mut self, session_id: &str) {
        if let Some(task) = self
            .chat_tasks
            .iter()
            .find(|(_, task)| task.session_id == session_id)
            .map(|(id, _)| *id)
            .and_then(|id| self.chat_tasks.remove(&id))
        {
            task.abort.abort();
            if self
                .session
                .as_ref()
                .is_some_and(|session| session.id == session_id)
            {
                self.pending_approval = None;
                self.pending_approval_request = None;
            }
        }
    }

    fn finish_chat_task(
        &mut self,
        task_id: super::ChatTaskId,
        error: Option<String>,
    ) -> Result<()> {
        let Some(task) = self.chat_tasks.remove(&task_id) else {
            return Ok(());
        };
        self.finish_chat_task_state(task, error, false)
    }

    // Long by design (long state-transition fn).
    #[allow(clippy::too_many_lines)]
    fn finish_chat_task_state(
        &mut self,
        task: super::ChatTask,
        error: Option<String>,
        stopped: bool,
    ) -> Result<()> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.id == task.session_id)
        {
            self.pending_approval = None;
            self.pending_approval_request = None;
        }
        if let Some((_, root)) = self.queued_workspace.take_if(|(id, _)| *id == task.id)
            && self
                .session
                .as_ref()
                .is_some_and(|session| session.id == task.session_id)
        {
            self.execute(super::AppCommand::SetWorkMode {
                mode: WorkMode::Agent,
                root: Some(root),
            })?;
        }
        let mut reasoning = task.thinking;
        let (buf, inline) = split_inline_reasoning(&task.buffer);
        if let Some(inline) = inline {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&inline);
        }
        if buf.is_empty() && error.is_none() {
            self.push_status(if stopped {
                "response stopped".to_string()
            } else {
                "response finished without text".to_string()
            });
            return Ok(());
        }
        // Some reasoning models (routed without the separate `reasoning` delta
        // field) inline their thinking as `<think>...</think>` in `content`
        // itself. Pull that out so the stored/displayed/copied message is just
        // the actual answer, not the thinking — same treatment as the explicit
        // reasoning channel above.
        let viewing = self
            .session
            .as_ref()
            .is_some_and(|session| session.id == task.session_id);
        let model = Some(task.model.clone());
        // Prefer the provider's exact usage; fall back to a ~4-chars/token estimate.
        let usage = task.usage;
        let tokens = Some(match usage {
            Some(u) => u.completion_tokens as i64,
            None => buf.chars().count().div_ceil(4) as i64,
        });
        if viewing && let Some(u) = usage {
            // Some providers omit total; derive it from prompt + completion.
            let total = if u.total_tokens > 0 {
                u.total_tokens
            } else {
                u.prompt_tokens + u.completion_tokens
            };
            self.context_total = Some(total);
        }
        let secs = Some(task.started.elapsed().as_secs_f64());
        // Prefer the provider's exact charge; otherwise estimate from the
        // cache-aware catalog. The same value was logged by the Usage event.
        let cost = usage.and_then(|u| {
            u.cost.or_else(|| {
                self.db.request_cost(
                    &task.model_id,
                    u.prompt_tokens,
                    u.completion_tokens,
                    u.cache_read_tokens,
                    u.cache_creation_tokens,
                )
            })
        });
        let reasoning = (!reasoning.is_empty()).then_some(reasoning);
        let phrase = Some(THINKING[task.thinking_idx].1.to_string());
        let created_at = Some(chrono::Utc::now().to_rfc3339());

        if !task.incognito && !buf.is_empty() {
            self.db.add_assistant_message(
                &task.session_id,
                &buf,
                model.as_deref(),
                reasoning.as_deref(),
                tokens,
                secs,
                cost,
                phrase.as_deref(),
            )?;
        }
        if viewing && !buf.is_empty() {
            self.messages.push(Message {
                role: "assistant".to_string(),
                content: buf,
                model,
                reasoning,
                tokens,
                secs,
                cost,
                phrase,
                persona: None,
                created_at,
            });
            // These jobs still use active-session state, so only launch them
            // when the task's origin is the session currently being viewed.
            if !task.incognito {
                self.maybe_generate_title();
                self.maybe_extract_memory();
            }
            self.maybe_mask();
            if !task.incognito {
                self.maybe_compact();
            }
        }
        if let Some(error) = error {
            if !task.incognito {
                self.db.add_error_message(&task.session_id, &error)?;
            }
            let msg = format!("stream error: {error}");
            if viewing {
                self.messages.push(Message {
                    role: "error".to_string(),
                    content: error.clone(),
                    model: None,
                    reasoning: None,
                    tokens: None,
                    secs: None,
                    cost: None,
                    phrase: None,
                    persona: None,
                    created_at: None,
                });
            } else if !task.incognito {
                self.unread.insert(task.session_id.clone());
            }
            self.push_status(if viewing {
                msg.clone()
            } else {
                format!("stream error in {}: {error}", task.session_title)
            });
            if !viewing && !task.incognito {
                super::send_system_notification(
                    &format!("Chat failed: {}", task.session_title),
                    &msg,
                );
            }
            if !viewing && !task.incognito {
                self.notifications.push_back(super::ChatNotification {
                    session_id: task.session_id,
                    title: task.session_title,
                    text: msg,
                    success: false,
                });
            }
        } else if stopped {
            self.push_status("response stopped".to_string());
        } else {
            if !viewing && !task.incognito {
                self.unread.insert(task.session_id.clone());
                self.push_status(format!("✓ response ready in: {}", task.session_title));
                super::send_system_notification(
                    &format!("Chat ready: {}", task.session_title),
                    "response complete",
                );
            } else if viewing {
                self.push_status("response complete".to_string());
            }
            if !viewing && !task.incognito {
                self.notifications.push_back(super::ChatNotification {
                    session_id: task.session_id,
                    title: task.session_title,
                    text: "response complete".to_string(),
                    success: true,
                });
            }
        }
        Ok(())
    }

    /// After the first exchange of a session, ask the model for a short topic and
    /// slug in the background. Runs once per session (guarded by `slug.is_none()`).
    pub fn maybe_generate_title(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some((provider, raw_model)) = self
            .current_model
            .as_deref()
            .and_then(|model| self.resolve_model_backend(model))
            .or_else(|| self.resolve_utility_model_backend(&self.memory_model))
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
            .filter(|m| m.role != "tool_call")
            .map(|m| {
                format!(
                    "{}: {}",
                    m.role,
                    m.content.chars().take(500).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let session_id = session.id.clone();
        let (tx, rx) = mpsc::unbounded_channel();
        self.title_rx = Some(rx);
        tokio::spawn(async move {
            let prompt = prompts::render(prompts::TITLE, &[("conversation", &convo)]);
            let msgs = vec![ChatMessage::text("user", prompt)];
            if let Ok(text) = provider.complete(&raw_model, msgs).await
                && let Some((topic, slug)) = parse_topic(&text)
            {
                let _ = tx.send((session_id, topic, slug));
            }
        });
    }

    /// Apply a generated topic/slug to the matching session (in memory + db).
    pub fn on_title_result(&mut self, result: Option<(String, String, String)>) {
        self.title_rx = None;
        let Some((id, topic, slug)) = result else {
            return;
        };
        let _ = self.db.set_session_title(&id, &topic, Some(&slug));
        if let Some(s) = self.session.as_mut().filter(|s| s.id == id) {
            s.title.clone_from(&topic);
            s.slug = Some(slug.clone());
        }
        if let Some(s) = self.sessions_cache.iter_mut().find(|s| s.id == id) {
            s.title = topic;
            s.slug = Some(slug);
        }
    }

    /// Instructions + the active session's memory snapshot, combined into one
    /// system message. Stable base, rendering and environment instructions
    /// precede space context, user additions, skills and the frozen memory
    /// snapshot. Volatile date and session-mode instructions come last.
    pub fn system_prompt(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let base = self.resolved_base_system_prompt();
        if !base.is_empty() {
            parts.push(base);
        }
        if self.interactive_ui {
            parts.push(Self::rendering_section());
            parts.push(crate::interactive::INSTRUCTIONS.to_string());
        }
        parts.push(self.environment_section());
        if self.work_mode == WorkMode::Agent {
            parts.push(prompts::render(prompts::AGENT, &[]));
        }
        if !self.incognito {
            if let Some(files) = self.files_section() {
                parts.push(files);
            }
            if let Some(apps) = self.apps_section() {
                parts.push(apps);
            }
            if let Some(scripts) = self.scripts_section() {
                parts.push(scripts);
            }
            let instructions =
                std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            if let Some(i) = instructions {
                parts.push(i);
            }
        }
        let overlay = self.resolved_system_prompt_overlay();
        if !overlay.is_empty() {
            parts.push(overlay);
        }
        if let Some(skills) = self.skills_section() {
            parts.push(skills);
        }
        if !self.incognito {
            let memory = self.memory_snapshot();
            if !memory.trim().is_empty() {
                parts.push(format!("## Memory\n{memory}"));
            }
        }
        parts.push(self.now_section());
        if self.web_mode {
            let today = self
                .prompt_datetime
                .split_whitespace()
                .next()
                .unwrap_or_default();
            parts.push(web_mode_clause(today));
        }
        if self.is_research_session() {
            parts.push(
                "This session came from /research — prefer research_lookup with scope=session_sources over search with mode=web for \
                 follow-ups; only use web search on a miss."
                    .to_string(),
            );
        }
        parts.join("\n\n")
    }

    pub(super) fn environment_section(&self) -> String {
        let mut section = "## Environment\n".to_string();
        if let Some((provider, raw_model)) = self
            .current_model
            .as_deref()
            .and_then(|model| self.resolve_model_backend(model))
        {
            let _ = writeln!(
                section,
                "- Backend: {} · model {raw_model}",
                provider.backend_tag().name()
            );
        }
        let _ = write!(
            section,
            "- OS: {} ({})",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        if self.work_mode == WorkMode::Agent
            && let Some(workspace) = &self.workspace
        {
            let _ = write!(
                section,
                "\n- Mode: agent · workspace {}",
                workspace.root().display()
            );
        }
        section
    }

    pub(super) fn rendering_section() -> String {
        "## Rendering\nFenced ```mermaid blocks render as inline diagrams for flowchart, sequenceDiagram and stateDiagram-v2 sources. Other Mermaid diagram types are shown as source text, so prefer the supported kinds or a plain list."
            .to_string()
    }

    pub(super) fn now_section(&self) -> String {
        let mut section = format!(
            "## Now\nCurrent date/time: {}. Your training data has a cutoff well before this — trust this timestamp over any date assumption from training",
            self.prompt_datetime
        );
        if self.toolbox.defs().iter().any(|tool| tool.name == "search") {
            section.push_str(
                ", and use `search` with mode=web for anything that may have changed since training (news, releases, prices, \"latest\" anything).",
            );
        } else {
            section.push('.');
        }
        section
    }

    /// Ctrl+O: navigate to the session linked in a `session_link` message
    /// under the text selection. Expects the message content's first line to
    /// be the target session id. `owner` is the message index at the
    /// selection start.
    pub fn open_session_link(&mut self, owner: Option<usize>) {
        let Some(msg) = owner.and_then(|i| self.messages.get(i)) else {
            self.push_status(
                "select text on a session link message, then press Ctrl+O".to_string(),
            );
            return;
        };
        if msg.role != "session_link" {
            self.push_status(
                "select text on a session link message, then press Ctrl+O".to_string(),
            );
            return;
        }
        let Some(target) = msg
            .content
            .split_once('\n')
            .map(|(s, _)| s.trim().to_string())
        else {
            self.push_status("malformed session link".to_string());
            return;
        };
        if let Err(e) = self.switch_to_session_by_id(&target) {
            self.push_status(format!("session switch failed: {e}"));
        }
    }

    /// Pin or discard the `[n]` source under the current history selection
    /// (same selection→citation resolution as `open_citation_under_selection`).
    /// Flags are keyed by the message's normalized URL, session-scoped.
    /// `selected`/`owner` come from the view's `HistorySel` state.
    pub fn flag_source_under_selection(
        &mut self,
        flag: Option<&str>,
        selected: Option<String>,
        owner: Option<usize>,
    ) {
        let Some(selected) = selected else {
            self.push_status("select a [n] citation first".to_string());
            return;
        };
        let Some(n) = crate::citations::citation_number_in(&selected) else {
            self.push_status("no [n] citation in the current selection".to_string());
            return;
        };
        let Some(msg) = owner.and_then(|i| self.messages.get(i)) else {
            self.push_status("no [n] citation in the current selection".to_string());
            return;
        };
        let citations = crate::citations::parse_citations(&msg.content);
        let Some((_, url)) = citations.iter().find(|(num, _)| *num == n) else {
            self.push_status(format!("no source [{n}] in this message"));
            return;
        };
        let Some(session) = &self.session else {
            self.push_status("no active session".to_string());
            return;
        };
        let url_norm = crate::tools::normalize_url(url);
        let verb = match flag {
            Some("discarded") => "discarded",
            Some(_) => "pinned",
            None => "cleared",
        };
        match self.db.set_source_flag(&session.id, &url_norm, flag) {
            Ok(()) => {
                self.push_status(format!("{verb} [{n}]: {url}"));
                self.refresh_toolbox();
            }
            Err(e) => self.push_status(format!("flag failed: {e}")),
        }
    }

    /// `/web`: flip web answer mode for the active (or about-to-be-created)
    /// session. Persisted immediately if a session already exists; otherwise
    /// applied to the session created by the next message.
    pub fn toggle_web_mode(&mut self) {
        self.web_mode = !self.web_mode;
        self.bump_cache_epoch();
        if let Some(session) = self.session.as_mut() {
            session.web_mode = self.web_mode;
            let _ = self.db.set_session_web_mode(&session.id, self.web_mode);
        }
        self.push_status(if self.web_mode {
            "🌐 web mode on".to_string()
        } else {
            "web mode off".to_string()
        });
    }

    pub fn toggle_incognito(&mut self) -> Result<()> {
        if self.is_streaming() {
            self.stop_stream()?;
        }
        self.session = None;
        self.messages.clear();
        self.mask_through = 0;
        self.refresh_memory_snapshot();
        self.context_total = None;
        self.push_composer_clear();
        self.push_viewport_reset();
        self.cleanup_incognito_images();
        self.incognito = !self.incognito;
        self.work_mode = WorkMode::Chat;
        self.workspace = None;
        self.refresh_toolbox();
        self.push_status(if self.incognito {
            "incognito mode — nothing persists, no apps".to_string()
        } else {
            "incognito mode off".to_string()
        });
        Ok(())
    }

    /// Bundled base prompt with `{{verbosity}}` and `{{datetime}}` resolved.
    pub fn resolved_base_system_prompt(&self) -> String {
        self.base_system_prompt
            .replace("{{verbosity}}", verbosity_clause(&self.verbosity))
            .replace("{{datetime}}", &self.prompt_datetime)
    }

    /// User additions with the same runtime placeholders as the bundled base.
    pub(super) fn resolved_system_prompt_overlay(&self) -> String {
        self.system_prompt_overlay
            .trim()
            .replace("{{verbosity}}", verbosity_clause(&self.verbosity))
            .replace("{{datetime}}", &self.prompt_datetime)
    }

    /// Re-read the user additions in `system_prompt.md` after a Ctrl+E hand-edit.
    pub fn reload_base_system_prompt(&mut self) {
        let text = crate::config::load_system_prompt_overlay();
        if text != self.system_prompt_overlay {
            self.system_prompt_overlay = text;
            self.bump_cache_epoch();
        }
        self.push_status("system prompt reloaded".to_string());
    }

    /// Names + descriptions of installed skills and how to invoke one — full
    /// bodies stay off the wire until the model calls the `skill` tool.
    fn skills_section(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut s = "## Skills\nYou have skills available. Their descriptions are metadata only: when a skill \
                     matches the user's task, call the `skills` tool with action=load and its name before \
                     acting; the full instructions will be returned. Load bundled resources by their relative \
                     path only, and do not load every skill spec up front.\n"
            .to_string();
        for skill in &self.skills {
            let _ = writeln!(s, "- {}: {}", skill.name, skill.description);
        }
        Some(s.trim_end().to_string())
    }

    /// Scripts the model has written (or the user has placed) in the space,
    /// listed so the model reuses them instead of rewriting from scratch.
    fn scripts_section(&self) -> Option<String> {
        if self.scripts_cache.is_empty() {
            return None;
        }
        let mut s = "## Scripts\nThe user has reusable scripts in this space. \
                      Call `scripts(action=read, path=...)` to see one, \
                      `scripts(action=edit, path=..., edits=...)` to modify, or \
                      `scripts(action=run, space=true, path=...)` to execute. \
                      The `path` parameter is relative to the scripts dir — do NOT prefix `scripts/`.\n"
            .to_string();
        for script in &self.scripts_cache {
            let _ = writeln!(s, "- {} ({})", script.name, human_size(script.size));
        }
        Some(s.trim_end().to_string())
    }

    /// Names/types/sizes of the space's imported files — content stays off the
    /// wire until the model calls `files` with the appropriate action.
    fn files_section(&self) -> Option<String> {
        if self.files_cache.is_empty() {
            return None;
        }
        let mut s = "## Files\nThe user has imported these files into this space. Do not guess \
                     their contents: call `files(action=search, query=...)` to find relevant passages, or \
                     `files(action=read, name=...)` to read one (200 lines per call, use offset to page). \
                     Need several at once? Wrap them in one `batch` call.\n"
            .to_string();
        for f in &self.files_cache {
            let kind = std::path::Path::new(&f.name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("file")
                .to_lowercase();
            let _ = writeln!(
                s,
                "- {} ({kind}, {}, {})",
                f.name,
                human_size(f.size.unsigned_abs()),
                f.status
            );
        }
        Some(s.trim_end().to_string())
    }

    /// How to build/edit locally served web apps, plus the space's existing
    /// apps. Present whenever the app server is running. Hidden in incognito.
    fn apps_section(&self) -> Option<String> {
        if self.incognito {
            return None;
        }
        self.app_server.as_ref()?;
        let mut s = "## Apps\nYou can build apps served locally. \
                     ALWAYS use the KV store for persistence (not LocalStorage), the upload endpoint for file \
                     uploads, and the app tool to bring user data into the app.\n\n\
                     **CRITICAL: URLs are UUID-based.** The UUID comes from `app(action=write)`'s result \
                     (\"live at http://...\"). Copy it from there — never invent or guess a UUID. \
                     If you don't have the result, call `app(action=read)` or `app(action=search)` on the app to \
                     rediscover it.\n\n"
            .to_string();

        s.push_str("### Tools\n");
        s.push_str("- `batch` — wrap several app operations into one round-trip (results come back labeled).\n");
        s.push_str("- `app(action=write, app, path, content)` — create/replace a file. App can be a name or UUID; a new app gets a UUID.\n");
        s.push_str("- `app(action=read, app, path)` / `app(action=patch, app, path, edits)` — read and edit by hashline.\n");
        s.push_str("- `app(action=diff, app, path, content)` — preview a complete-file change without writing it.\n");
        s.push_str("- `app(action=search, app, pattern)` — search non-ignored app files; respects `.gitignore`.\n");
        s.push_str(
            "- `scripts(action=install, app=..., packages=[...])` — npm-install into an app.\n",
        );
        s.push_str("- `app(action=list)` — list pasted conversation images.\n");
        s.push_str("- `app(action=copy_images, image_ids, app)` — copy images into `_images/` for `<img src=\"...\">`.\n");
        s.push_str("- `app(action=copy_file, file_name, app)` — copy a space file's text into the app's KV store.\n\n");

        s.push_str("### KV Store (persistent key-value per app)\n");
        s.push_str("Each app has a SQLite-backed KV store. Call these from frontend JS:\n");
        s.push_str("- `PUT <app_url>/_api/kv/<key>` — upsert a value (body = raw text)\n");
        s.push_str("- `GET <app_url>/_api/kv/<key>` — read a value\n");
        s.push_str("- `DELETE <app_url>/_api/kv/<key>` — delete a value\n");
        s.push_str("- `GET <app_url>/_api/kv` — list all keys (returns JSON array)\n\n");

        s.push_str("### File Upload\n");
        s.push_str("- `POST <app_url>/_api/upload` with `multipart/form-data` — upload a file. Returns `{\"name\", \"url\"}`. Files persist and are served via GET.\n\n");

        s.push_str("### Using User Images\n");
        s.push_str("1. `app(action=list)` to see conversation images.\n");
        s.push_str("2. `app(action=copy_images, image_ids, app)` to copy them into `_images/`.\n");
        s.push_str("3. Use returned URLs in `<img src=\"...\">` tags.\n\n");

        s.push_str("### Using Space Files\n");
        s.push_str("- `app(action=copy_file, file_name, app)` copies file text into KV under `_file:<name>`. Read it via `GET <app_url>/_api/kv/_file:<name>`.\n\n");

        let apps = self.list_apps();
        if apps.is_empty() {
            s.push_str("No apps exist in this space yet.");
        } else {
            s.push_str("Existing apps:\n");
            for a in &apps {
                if let Some(uuid) = self
                    .app_server
                    .as_ref()
                    .and_then(|s| s.registry().resolve(&self.active_space.name, a))
                {
                    let _ = writeln!(s, "- {a} (uuid {uuid})");
                } else {
                    let _ = writeln!(s, "- {a}");
                }
            }
        }
        Some(s.trim_end().to_string())
    }

    /// A system message with space-file images inline, for vision models.
    /// Returns None when there are no image-type space files.
    fn space_images_message(&self) -> Option<ChatMessage> {
        let files_dir = self.space.files_dir(&self.active_space.name);
        let mut urls: Vec<String> = Vec::new();
        let mut names: Vec<&str> = Vec::new();
        for f in &self.files_cache {
            let ext = std::path::Path::new(&f.name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !crate::extract::is_image_ext(ext) {
                continue;
            }
            let path = files_dir.join(&f.name);
            if let Ok(bytes) = std::fs::read(&path) {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let mime = match ext {
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "webp" => "image/webp",
                    "bmp" => "image/bmp",
                    _ => "image/png",
                };
                urls.push(format!("data:{mime};base64,{b64}"));
            }
            names.push(f.name.as_str());
        }
        if urls.is_empty() {
            return None;
        }
        let text = format!(
            "The user has these image files in this space which you can see below: {}",
            names.join(", ")
        );
        Some(ChatMessage {
            role: "system".to_string(),
            content: text,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            images: urls,
            cache_breakpoint: false,
        })
    }

    /// Names of the active space's existing apps (directory listing).
    pub fn list_apps(&self) -> Vec<String> {
        let dir = self.space.apps_dir(&self.active_space.name);
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut apps: Vec<String> = rd
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        apps.sort();
        apps
    }
}

/// Each fenced code block in `md` as `(language, code)`.
pub fn code_blocks(md: &str) -> Vec<(Option<String>, String)> {
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

pub fn pick_greeting() -> &'static str {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    super::GREETINGS[n % super::GREETINGS.len()]
}

pub fn pick_flavor() -> (usize, SpinnerColor) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    (n % THINKING.len(), SPINNER_COLORS[n % SPINNER_COLORS.len()])
}

/// The instruction block appended to the system prompt when web mode is on:
/// forces search-first, inline `[n]` citations, and a trailing Sources list.
/// `today` keeps the model from hedging with stale training-data dates.
pub fn web_mode_clause(today: &str) -> String {
    format!(
        "Web answer mode is ON for this session. Today's date is {today}. Before answering, you \
         MUST call search with mode=web and a focused query (and fetch_url on the most promising results) — \
         never answer from memory alone. You may search more than once with refined queries if the \
         first results are insufficient. Each search result is numbered [1], [2], ... with title, \
         URL, and snippet. Cite every claim inline immediately as [n]; do not bunch citations at \
         the end of a paragraph. End your reply with a line starting exactly with 'Sources:' \
         followed by every citation you used, one per line, as [n] title — url. Do not fabricate \
         sources; if a claim is not backed by a search/fetched source, do not cite it."
    )
}

pub fn title_from(text: &str) -> String {
    let t: String = text.chars().take(40).collect();
    if t.trim().is_empty() {
        "new chat".to_string()
    } else {
        t
    }
}

/// Append one persisted assistant tool-call batch and its tool results to a
/// reconstructed request history.
fn append_tool_call_history(
    history: &mut Vec<ChatMessage>,
    effective: &[Message],
    index: &mut usize,
    seen_results: &mut std::collections::HashMap<(String, String), String>,
    tool_images_dir: Option<&std::path::Path>,
    result_cap: Option<usize>,
    mask_through: usize,
) {
    let start = *index;
    let mut rows = Vec::new();
    while *index < effective.len() && effective[*index].role == "tool_call" {
        if let Some(row) = parse_tool_call_row(&effective[*index].content) {
            let masked = *index < mask_through
                && crate::tools::classify_tool_result(&row.result, "").is_none();
            rows.push((row, masked));
        }
        *index += 1;
    }
    if rows.is_empty() {
        // Keep malformed legacy rows out of the model.
        debug_assert!(*index > start);
        return;
    }
    let calls = rows
        .iter()
        .map(|(row, _)| row.call.clone())
        .collect::<Vec<_>>();
    let reasoning_content = rows
        .iter()
        .find_map(|(row, _)| row.reasoning_content.clone());
    let assistant_content = rows
        .iter()
        .find_map(|(row, _)| row.assistant_content.clone());
    history.push(ChatMessage {
        role: "assistant".to_string(),
        content: assistant_content.unwrap_or_default(),
        reasoning_content,
        tool_calls: Some(calls),
        tool_call_id: None,
        images: Vec::new(),
        cache_breakpoint: false,
    });
    for (row, masked) in rows {
        let call = row.call;
        let images = tool_images_dir
            .filter(|_| !masked)
            .and_then(|dir| crate::provider::openrouter::extract_tool_images(&row.result, dir));
        let key = (call.name.clone(), call.arguments.clone());
        // Dedup the model-visible result so replay and live-loop seeding agree.
        let visible = result_cap.map_or_else(
            || row.result.clone(),
            |cap| crate::tools::cap_head_tail(&row.result, cap),
        );
        let content = if masked {
            crate::tools::tool_result_masked_note(&call.name, &call.arguments)
        } else {
            match seen_results.get(&key) {
                Some(prev) if *prev == visible => {
                    crate::tools::tool_result_unchanged_note(&call.name, &call.arguments)
                }
                _ => {
                    seen_results.insert(key, visible.clone());
                    visible
                }
            }
        };
        history.push(ChatMessage {
            role: "tool".to_string(),
            content,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call.id),
            images: Vec::new(),
            cache_breakpoint: false,
        });
        if let Some(images) = images {
            history.push(ChatMessage {
                role: "user".to_string(),
                content: images.description,
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                images: images.urls,
                cache_breakpoint: false,
            });
        }
    }
}

/// A captured `!` command can run this long before its process group is killed.
const USER_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);
/// Characters of a captured command's output kept in its row (head and tail).
const USER_COMMAND_CHARS: usize = 8_000;
/// Bytes of raw output buffered from each end while a command runs.
const USER_COMMAND_KEEP_BYTES: usize = 64 * 1024;
const USER_COMMAND_INTERACTIVE_NOTE: &str =
    "(interactive — output not captured; paste anything relevant)";

/// How long the output reader may stay open after the command's process
/// group is gone (a descendant that left the group can hold the pipe).
const USER_COMMAND_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);
const USER_COMMAND_STREAM_OPEN_NOTE: &str =
    "(output stream still open — a background process kept it; not waited for)";

/// A finished captured `!` command, sent from its task to `next_event`.
pub(crate) struct UserCommandRun {
    session_id: String,
    incognito: bool,
    cmd: String,
    cwd: String,
    exit: Option<i32>,
    started: bool,
    output: String,
}

/// How a `!` command ended: `exit <code>`, `killed` (by a signal or the
/// timeout), or `failed to start`.
pub fn user_command_outcome_label(exit: Option<i32>, started: bool) -> String {
    match exit {
        Some(code) => format!("exit {code}"),
        None if started => "killed".to_string(),
        None => "failed to start".to_string(),
    }
}

/// A `user_command` row's outcome, shared by the replay and the TUI card.
pub fn user_command_outcome(row: &serde_json::Value) -> String {
    user_command_outcome_label(
        row["exit"]
            .as_i64()
            .and_then(|code| i32::try_from(code).ok()),
        row["started"].as_bool().unwrap_or(true),
    )
}

fn user_command_row(
    cmd: &str,
    cwd: &str,
    exit: Option<i32>,
    started: bool,
    output: &str,
    interactive: bool,
) -> String {
    serde_json::json!({
        "cmd": cmd,
        "cwd": cwd,
        "exit": exit,
        "started": started,
        "output": output,
        "interactive": interactive,
    })
    .to_string()
}

/// A `user_command` row as the user message the model reads.
fn user_command_replay(content: &str) -> String {
    let Ok(row) = serde_json::from_str::<serde_json::Value>(content) else {
        return content.to_string();
    };
    let field = |key: &str| row[key].as_str().unwrap_or_default();
    let mut text = format!(
        "I ran `{}` in {} ({}):\n",
        field("cmd"),
        field("cwd"),
        user_command_outcome(&row)
    );
    let output = field("output");
    if row["interactive"].as_bool() == Some(true) {
        text.push_str(output);
    } else {
        // A fence longer than any backtick run in the output keeps it intact.
        let longest = output
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or_default();
        let fence = "`".repeat(longest.max(2) + 1);
        let _ = write!(text, "{fence}\n{}\n{fence}", output.trim_end());
    }
    text
}

/// Run `sh -c <cmd>` with the user's environment and stdin closed, returning
/// its exit code, whether it started, and combined stdout+stderr. The process
/// group is killed when the shell exits (no stray background jobs) or at
/// `timeout`.
async fn capture_user_command(
    cmd: &str,
    cwd: &std::path::Path,
    timeout: std::time::Duration,
) -> (Option<i32>, bool, String) {
    let (child, reader) = match spawn_captured(cmd, cwd) {
        Ok(spawned) => spawned,
        Err(error) => return (None, false, format!("could not run: {error}")),
    };
    match collect_captured(child, reader, timeout).await {
        Ok((exit, output)) => (exit, true, output),
        Err(error) => (None, true, format!("could not read its output: {error}")),
    }
}

fn spawn_captured(
    cmd: &str,
    cwd: &std::path::Path,
) -> std::io::Result<(tokio::process::Child, std::io::PipeReader)> {
    // One pipe behind both descriptors keeps stdout and stderr in write order.
    let (reader, writer) = std::io::pipe()?;
    let mut command = tokio::process::Command::new("sh");
    command
        .args(["-c", cmd])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn()?;
    // Dropping `command` closes this side's write ends, so the reader sees
    // EOF once every process holding the pipe has exited.
    Ok((child, reader))
}

async fn collect_captured(
    mut child: tokio::process::Child,
    reader: std::io::PipeReader,
    timeout: std::time::Duration,
) -> std::io::Result<(Option<i32>, String)> {
    let group = child.id();
    let sink = std::sync::Arc::new(std::sync::Mutex::new(HeadTail::default()));
    let reader_sink = std::sync::Arc::clone(&sink);
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    // A plain thread, not `spawn_blocking`: a reader stuck on an escaped
    // descendant must never hold up runtime shutdown.
    std::thread::spawn(move || {
        let _ = done_tx.send(read_into(reader, &reader_sink));
    });
    let (exit, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => (None, true),
    };
    kill_group(group);
    if timed_out {
        child.wait().await?;
    }
    let stream_open = match tokio::time::timeout(USER_COMMAND_DRAIN, done_rx).await {
        Ok(Ok(read)) => {
            read?;
            false
        }
        Ok(Err(closed)) => return Err(std::io::Error::other(closed)),
        Err(_) => true,
    };
    let mut output = sink.lock().expect("output buffer").text();
    if timed_out {
        let _ = write!(output, "\n[timed out after {timeout:?} · killed]");
    }
    if stream_open {
        let _ = write!(output, "\n{USER_COMMAND_STREAM_OPEN_NOTE}");
    }
    Ok((exit, output))
}

#[cfg(unix)]
fn kill_group(group: Option<u32>) {
    if let Some(pid) = group.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: the pid came from our child, spawned as its own process
        // group leader; negating it signals only that group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

#[cfg(not(unix))]
fn kill_group(_group: Option<u32>) {}

/// A stream's first and last `USER_COMMAND_KEEP_BYTES`, so a runaway command
/// cannot exhaust memory.
#[derive(Default)]
struct HeadTail {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    total: usize,
}

impl HeadTail {
    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len();
        let split = (USER_COMMAND_KEEP_BYTES - self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..split]);
        self.tail.extend(&bytes[split..]);
        if self.tail.len() > USER_COMMAND_KEEP_BYTES {
            self.tail.drain(..self.tail.len() - USER_COMMAND_KEEP_BYTES);
        }
    }

    fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        let omitted = self.total - self.head.len() - self.tail.len();
        if omitted > 0 {
            let _ = write!(text, "\n… [{omitted} bytes omitted] …\n");
        }
        let (front, back) = self.tail.as_slices();
        text.push_str(&String::from_utf8_lossy(&[front, back].concat()));
        text
    }
}

/// Read a stream to its end into `sink`, which stays readable meanwhile.
fn read_into(
    mut reader: impl std::io::Read,
    sink: &std::sync::Mutex<HeadTail>,
) -> std::io::Result<()> {
    let mut chunk = [0; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => sink.lock().expect("output buffer").push(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// One persisted tool-call transcript row, including the provider call id,
/// assistant content, and optional reasoning needed to reconstruct the batch.
struct StoredToolCall {
    call: ToolCall,
    result: String,
    reasoning_content: Option<String>,
    assistant_content: Option<String>,
}

/// Recover a stored tool-call row from its JSON content. Older rows did not
/// persist an id, so they retain the legacy synthetic id as a fallback.
fn parse_tool_call_row(content: &str) -> Option<StoredToolCall> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let arguments = v.get("arguments")?.as_str()?.to_string();
    let result = v.get("result")?.as_str()?.to_string();
    Some(StoredToolCall {
        call: ToolCall {
            id: v
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .unwrap_or("call_0")
                .to_string(),
            name,
            arguments,
        },
        result,
        reasoning_content: v
            .get("reasoning_content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        assistant_content: v
            .get("assistant_content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

/// Compact byte counts: 940 B, 1.2 KB, 3.4 MB.
pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// Strip `<think>...</think>` blocks out of `text`, returning the cleaned
/// content and the extracted reasoning (blocks joined by newlines), if any. An
/// unterminated tag (e.g. a truncated stream) treats the remainder as
/// reasoning rather than leaking a dangling tag into the answer.
pub fn split_inline_reasoning(text: &str) -> (String, Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nexus-usercmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn captured_command_timeout_kills_its_process_group() {
        let dir = temp_dir();
        let started = std::time::Instant::now();
        let (exit, ran, output) = capture_user_command(
            "echo started; (sleep 1; touch late) & sleep 5",
            &dir,
            std::time::Duration::from_millis(300),
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert_eq!((exit, ran), (None, true));
        assert_eq!(user_command_outcome_label(exit, ran), "killed");
        assert!(output.starts_with("started\n"), "{output}");
        assert!(
            output.ends_with("[timed out after 300ms · killed]"),
            "{output}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(
            !dir.join("late").exists(),
            "background job outlived the kill"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn an_escaped_descendant_holding_the_pipe_does_not_stall_the_command() {
        let dir = temp_dir();
        let started = std::time::Instant::now();
        // The perl child leaves the process group (the shell waits until it
        // has), so the group kill misses it and it keeps the output pipe open
        // for ten seconds.
        let (exit, ran, output) = capture_user_command(
            "perl -MPOSIX -e 'POSIX::setsid(); open my $f, \">\", \"escaped\"; close $f; sleep 10' & \
             while [ ! -e escaped ]; do sleep 0.05; done; echo hi",
            &dir,
            USER_COMMAND_TIMEOUT,
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!((exit, ran), (Some(0), true));
        assert_eq!(output, format!("hi\n\n{USER_COMMAND_STREAM_OPEN_NOTE}"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_command_that_cannot_start_says_so() {
        let missing = std::env::temp_dir().join(format!("nexus-missing-{}", uuid::Uuid::new_v4()));
        let (exit, ran, output) =
            capture_user_command("true", &missing, USER_COMMAND_TIMEOUT).await;
        assert_eq!((exit, ran), (None, false));
        assert_eq!(user_command_outcome_label(exit, ran), "failed to start");
        assert!(output.starts_with("could not run: "), "{output}");
    }

    #[test]
    fn runaway_output_keeps_only_its_head_and_tail() {
        let mut buffer = HeadTail::default();
        buffer.push("H".repeat(USER_COMMAND_KEEP_BYTES - 1).as_bytes());
        buffer.push(b"HMMMMMMMMMM");
        buffer.push("T".repeat(USER_COMMAND_KEEP_BYTES).as_bytes());
        let text = buffer.text();
        assert!(!text.contains('M'));
        assert!(text.contains("\n… [10 bytes omitted] …\n"));
        assert_eq!(text.matches('H').count(), USER_COMMAND_KEEP_BYTES);
        assert_eq!(text.matches('T').count(), USER_COMMAND_KEEP_BYTES);
        let mut short = HeadTail::default();
        short.push(b"short");
        assert_eq!(short.text(), "short");
    }

    #[test]
    fn replay_fences_output_longer_than_its_own_backticks() {
        let row = user_command_row(
            "cat README.md",
            "/w",
            Some(0),
            true,
            "```rust\nfn x() {}\n```\n",
            false,
        );
        assert_eq!(
            user_command_replay(&row),
            "I ran `cat README.md` in /w (exit 0):\n````\n```rust\nfn x() {}\n```\n````"
        );
        for (exit, started, outcome) in [(None, true, "killed"), (None, false, "failed to start")] {
            let row = user_command_row(
                "sudo true",
                "/w",
                exit,
                started,
                USER_COMMAND_INTERACTIVE_NOTE,
                true,
            );
            assert_eq!(
                user_command_replay(&row),
                format!("I ran `sudo true` in /w ({outcome}):\n{USER_COMMAND_INTERACTIVE_NOTE}")
            );
        }
    }
}
