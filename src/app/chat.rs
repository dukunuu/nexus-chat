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
use ratatui::style::Color;
use std::fmt::Write as _;
use tokio::sync::mpsc;

use crate::db::Message;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, ToolCall};

use super::{App, SPINNER_COLORS, THINKING, parse_topic, verbosity_clause};

impl App {
    pub fn submit(&mut self) -> Result<()> {
        let text = self.input_text();
        self.clear_input();
        self.sel.clear(); // history line indices are about to shift
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd)?;
        } else {
            self.send_message(text.to_string())?;
        }
        Ok(())
    }

    pub(super) fn send_message(&mut self, text: String) -> Result<()> {
        if let Some(session) = self.session.as_ref()
            && let Some(task) = self.chat_task_for_session(&session.id)
        {
            self.status = format!("wait — response still streaming in: {}", task.session_title);
            self.set_input(&text);
            return Ok(());
        }
        if self.chat_task_count() >= super::MAX_CHAT_TASKS {
            self.status = format!("chat task limit reached ({})", super::MAX_CHAT_TASKS);
            self.set_input(&text);
            return Ok(());
        }
        if !self.backends.any() {
            self.status = "set your API key first with /login".to_string();
            self.set_input(&text);
            return Ok(());
        }
        let Some(model) = self.current_model.clone() else {
            self.status = "pick a model first with /model".to_string();
            self.set_input(&text);
            return Ok(());
        };

        // Auto-create a session on the first message.
        if self.session.is_none() {
            let title = title_from(&text);
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
                let mut s =
                    self.db
                        .create_session(&title, &model, &self.active_space.id, "chat")?;
                // Carry a pre-session `/web` toggle onto the session it creates.
                if self.web_mode {
                    s.web_mode = true;
                    let _ = self.db.set_session_web_mode(&s.id, true);
                }
                s
            };
            self.session = Some(s);
        }
        let Some(session_id) = self.session.as_ref().map(|s| s.id.clone()) else {
            return Ok(());
        };

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

    /// The exact message list a completion request will carry: system prompt,
    /// compaction digest, forced skill, then the effective conversation tail.
    /// User messages with images become multimodal parts for vision models, or
    /// get their stored descriptions appended as text for everything else.
    pub(crate) fn build_history(&mut self) -> Vec<ChatMessage> {
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
        if let Some(name) = self.forced_skill.take()
            && let Some(skill) = self.skills.iter().find(|s| s.name == name)
        {
            let body = std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).to_string())
                .unwrap_or_default();
            history.push(ChatMessage::text(
                "system",
                format!("The user invoked the skill '{name}'. Follow these instructions:\n{body}"),
            ));
        }
        let vision = self.current_model_supports_images();
        for m in self.effective_messages() {
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
                continue;
            }
            if m.role == "tool_call" {
                if let Some((call, result)) = parse_tool_call_row(&m.content) {
                    history.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: String::new(),
                        tool_calls: Some(vec![call.clone()]),
                        tool_call_id: None,
                        images: Vec::new(),
                    });
                    history.push(ChatMessage {
                        role: "tool".to_string(),
                        content: result,
                        tool_calls: None,
                        tool_call_id: Some(call.id),
                        images: Vec::new(),
                    });
                }
                continue;
            }
            let mut cm = ChatMessage::text(m.role.clone(), m.content.clone());
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
        }
        history
    }

    /// Build history and fire one independently-routed streaming request.
    pub(super) fn start_stream(&mut self) -> Result<()> {
        let Some(model) = self.current_model.clone() else {
            return Ok(());
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.status = format!("model backend unavailable: {model} — pick another with /model");
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
        let params = ChatParams {
            reasoning_effort,
            temperature: self.settings.temperature,
            top_p: self.settings.top_p,
            max_tokens: self.settings.max_tokens,
        };
        let tools = self.toolbox.defs();
        let (rx, abort) = provider.stream_chat(
            raw_model,
            history,
            params,
            tools,
            self.toolbox.clone(),
            crate::provider::openrouter::MAX_TOOL_ITERS,
        );
        let Some(session) = self.session.clone() else {
            return Ok(());
        };
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
                incognito: self.incognito,
                buffer: String::new(),
                thinking: String::new(),
                tool_status: None,
                usage: None,
                started: std::time::Instant::now(),
                thinking_idx,
                spinner_color,
                abort,
            },
        );
        self.spinner_frame = 0;
        self.thinking_idx = thinking_idx;
        self.spinner_color = spinner_color;
        self.status = reasoning_warning.unwrap_or_default();
        self.scroll = 0;
        self.prev_total = 0;
        Ok(())
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
            name,
            arguments,
            result,
        } = ev
        {
            let content = serde_json::json!({
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
                phrase: None,
                persona: None,
                created_at: None,
            });
        }
        Ok(())
    }

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
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.usage = Some(u);
                }
            }
            StreamEvent::Status(s) => {
                if let Some(task) = self.chat_tasks.get_mut(&task_id) {
                    task.tool_status = Some(s);
                }
            }
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
            } => {
                if ((name == "skill_admin")
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
                let content =
                    serde_json::json!({ "name": name, "arguments": arguments, "result": result })
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
                        phrase: None,
                        persona: None,
                        created_at: None,
                    });
                }
                // Generated images land on disk but aren't indexed until
                // rescan_files picks them up. Without this, they'd be missing
                // from files_cache and never OCR'd for descriptive naming.
                if in_active_space
                    && (name == "generate_image"
                        || name == "generate_video"
                        || name == "video_transform"
                        || matches!(
                            name.as_str(),
                            "edit_video" | "extract_frame" | "stitch_videos"
                        ))
                {
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

    /// Esc while a response streams: abort the background chat task (kills any
    /// in-flight request and tool loop) and keep whatever text already arrived.
    pub fn stop_stream(&mut self) -> Result<()> {
        let Some(task_id) = self.active_chat_task().map(|task| task.id) else {
            return Ok(());
        };
        if let Some(task) = self.chat_tasks.remove(&task_id) {
            task.abort.abort();
            self.finish_chat_task_state(task, None, true)?;
        }
        Ok(())
    }

    pub(crate) fn discard_chat_task(&mut self, session_id: &str) {
        if let Some(task) = self
            .chat_tasks
            .iter()
            .find(|(_, task)| task.session_id == session_id)
            .map(|(id, _)| *id)
            .and_then(|id| self.chat_tasks.remove(&id))
        {
            task.abort.abort();
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
        let mut reasoning = task.thinking;
        let (buf, inline) = split_inline_reasoning(&task.buffer);
        if let Some(inline) = inline {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&inline);
        }
        if buf.is_empty() && error.is_none() {
            self.status = if stopped {
                "response stopped".to_string()
            } else {
                "response finished without text".to_string()
            };
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
                phrase,
                persona: None,
                created_at,
            });
            // These jobs still use active-session state, so only launch them
            // when the task's origin is the session currently being viewed.
            if !task.incognito {
                self.maybe_generate_title();
                self.maybe_extract_memory();
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
                    phrase: None,
                    persona: None,
                    created_at: None,
                });
            } else if !task.incognito {
                self.unread.insert(task.session_id.clone());
            }
            self.status = if viewing {
                msg.clone()
            } else {
                format!("stream error in {}: {error}", task.session_title)
            };
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
            self.status = "response stopped".to_string();
        } else {
            if !viewing && !task.incognito {
                self.unread.insert(task.session_id.clone());
                self.status = format!("✓ response ready in: {}", task.session_title);
                super::send_system_notification(
                    &format!("Chat ready: {}", task.session_title),
                    "response complete",
                );
            } else if viewing {
                self.status = "response complete".to_string();
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
    pub(super) fn maybe_generate_title(&mut self) {
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
            let prompt = format!(
                "Summarise this conversation as a session name. Reply with ONLY a JSON object, \
                 no markdown, of the form {{\"topic\": \"<3-5 word title>\", \"id\": \"<short-kebab-slug>\"}}.\n\n{convo}"
            );
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

    /// Instructions + memory for the active space, combined into one system
    /// message. `None` if the space has neither (today's no-system-prompt path).
    /// The full system prompt: the app's own base prompt (identity/formatting
    /// rules, `$EDITOR`-editable) first, then space instructions, skills, and
    /// memory layered on top. Unlike those three, the base prompt is never
    /// empty — it's the app speaking, not per-space configuration.
    pub(super) fn system_prompt(&self) -> String {
        let mut parts: Vec<String> = vec![self.resolved_base_system_prompt()];
        if !self.incognito {
            let instructions =
                std::fs::read_to_string(self.space.instructions_path(&self.active_space.name))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            if let Some(i) = instructions {
                parts.push(i);
            }
            if let Some(files) = self.files_section() {
                parts.push(files);
            }
            if let Some(apps) = self.apps_section() {
                parts.push(apps);
            }
            if let Some(scripts) = self.scripts_section() {
                parts.push(scripts);
            }
            let memory = self.read_memory();
            if !memory.trim().is_empty() {
                parts.push(format!("## Memory\n{memory}"));
            }
        }
        if let Some(skills) = self.skills_section() {
            parts.push(skills);
        }
        if self.web_mode {
            let today = Utc::now().format("%Y-%m-%d").to_string();
            parts.push(web_mode_clause(&today));
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

    /// `o` in the history pane: open the `[n]` citation under the current
    /// text selection (via the `open` crate), resolved against the Sources
    /// list of the message the selection belongs to. Every miss surfaces as
    /// a status message rather than doing nothing silently.
    /// Ctrl+O: navigate to the session linked in a `session_link` message
    /// under the text selection. Expects the message content's first line to
    /// be the target session id.
    pub(crate) fn open_session_link(&mut self) {
        let idx = self.sel.owner_at_selection_start();
        let Some(msg) = idx.and_then(|i| self.messages.get(i)) else {
            self.status = "select text on a session link message, then press Ctrl+O".to_string();
            return;
        };
        if msg.role != "session_link" {
            self.status = "select text on a session link message, then press Ctrl+O".to_string();
            return;
        }
        let Some(target) = msg
            .content
            .split_once('\n')
            .map(|(s, _)| s.trim().to_string())
        else {
            self.status = "malformed session link".to_string();
            return;
        };
        if let Err(e) = self.switch_to_session_by_id(&target) {
            self.status = format!("session switch failed: {e}");
        }
    }

    /// Force a full history cache rebuild on the next frame. Used when
    /// in-place message edits would otherwise leave stale wrapped content.
    pub(crate) fn invalidate_history_cache(&mut self) {
        if let Some(sid) = self.session.as_ref().map(|s| s.id.clone()) {
            self.session_caches.remove(&sid);
        }
        self.history_cache = crate::ui::history::HistoryCache::default();
    }

    /// If the given rendered line index is an image or video thumbnail line,
    /// open it in the default OS viewer. For video thumbnails (`_first.png`),
    /// opens the sibling `.mp4` instead.
    pub(crate) fn open_image_at_line(&self, line: usize) -> bool {
        if let Some(Some(path)) = self.history_cache.image_at_line.get(line) {
            let p = std::path::Path::new(path);
            let open_path = if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                // Check for sibling video: abc123_first.png → abc123.mp4 or _stitch_abc123.mp4
                if let Some(base) = stem
                    .strip_suffix("_first")
                    .or_else(|| stem.strip_suffix("_last"))
                {
                    let dir = p.parent().unwrap_or_else(|| std::path::Path::new(""));
                    let direct = dir.join(format!("{base}.mp4"));
                    let stitched = dir.join(format!("_stitch_{base}.mp4"));
                    if direct.exists() {
                        direct.to_string_lossy().to_string()
                    } else if stitched.exists() {
                        stitched.to_string_lossy().to_string()
                    } else {
                        path.clone()
                    }
                } else {
                    path.clone()
                }
            } else {
                path.clone()
            };
            let _ = open::that_detached(&open_path);
            true
        } else {
            false
        }
    }

    /// Pin or discard the `[n]` source under the current history selection
    /// (same selection→citation resolution as `open_citation_under_selection`).
    /// Flags are keyed by the message's normalized URL, session-scoped.
    pub(crate) fn flag_source_under_selection(&mut self, flag: Option<&str>) {
        let Some(selected) = self.sel.selected_text() else {
            self.status = "select a [n] citation, then press x".to_string();
            return;
        };
        let Some(n) = crate::citations::citation_number_in(&selected) else {
            self.status = "no [n] citation in the current selection".to_string();
            return;
        };
        let Some(msg) = self
            .sel
            .owner_at_selection_start()
            .and_then(|i| self.messages.get(i))
        else {
            self.status = "no [n] citation in the current selection".to_string();
            return;
        };
        let citations = crate::citations::parse_citations(&msg.content);
        let Some((_, url)) = citations.iter().find(|(num, _)| *num == n) else {
            self.status = format!("no source [{n}] in this message");
            return;
        };
        let Some(session) = &self.session else {
            self.status = "no active session".to_string();
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
                self.status = format!("{verb} [{n}]: {url}");
                self.refresh_toolbox();
            }
            Err(e) => self.status = format!("flag failed: {e}"),
        }
    }

    /// `/web`: flip web answer mode for the active (or about-to-be-created)
    /// session. Persisted immediately if a session already exists; otherwise
    /// applied to the session created by the next message.
    pub(crate) fn toggle_web_mode(&mut self) {
        self.web_mode = !self.web_mode;
        if let Some(session) = self.session.as_mut() {
            session.web_mode = self.web_mode;
            let _ = self.db.set_session_web_mode(&session.id, self.web_mode);
        }
        self.status = if self.web_mode {
            "🌐 web mode on".to_string()
        } else {
            "web mode off".to_string()
        };
    }

    pub(crate) fn toggle_incognito(&mut self) -> Result<()> {
        if self.is_streaming() {
            self.stop_stream()?;
        }
        self.session = None;
        self.messages.clear();
        self.context_total = None;
        self.scroll = 0;
        self.set_input("");
        self.cleanup_incognito_images();
        self.incognito = !self.incognito;
        self.status = if self.incognito {
            "incognito mode — nothing persists, no apps".to_string()
        } else {
            "incognito mode off".to_string()
        };
        Ok(())
    }

    /// `base_system_prompt` (raw, as read from `system_prompt.md`) with the
    /// `{{verbosity}}` placeholder swapped for the level the user picked.
    pub(super) fn resolved_base_system_prompt(&self) -> String {
        let now = Utc::now().format("%Y-%m-%d %H:%M UTC, %A").to_string();
        self.base_system_prompt
            .replace("{{verbosity}}", verbosity_clause(&self.verbosity))
            .replace("{{datetime}}", &now)
    }

    /// Re-read `system_prompt.md` after a Ctrl+E hand-edit.
    pub fn reload_base_system_prompt(&mut self) {
        if let Ok(text) = crate::config::load_system_prompt() {
            self.base_system_prompt = text;
            self.status = "system prompt reloaded".to_string();
        }
    }

    /// Names + descriptions of installed skills and how to invoke one — full
    /// bodies stay off the wire until the model calls the `skill` tool.
    fn skills_section(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut s = "## Skills\nYou have skills available. To use one, call the `skill` tool \
                     with its name; the full instructions will be returned.\n"
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
                      Call `script_files(action=read, path=...)` to see one, \
                      `script_files(action=edit, path=..., edits=...)` to modify, or \
                      `run_script(space=true, path=...)` to execute. \
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
                     `files(action=read, name=...)` to read one (200 lines per call, use offset to page).\n"
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
                     uploads, and app_assets to bring user data into the app.\n\n\
                     **CRITICAL: URLs are UUID-based.** The UUID comes from `app_modify(action=write)`'s result \
                     (\"live at http://...\"). Copy it from there — never invent or guess a UUID. \
                     If you don't have the result, call `app_inspect(action=read)` or `app_inspect(action=search)` on the app to \
                     rediscover it.\n\n"
            .to_string();

        s.push_str("### Tools\n");
        s.push_str("- `app_modify(action=write, app, path, content)` — create/replace a file. App can be a name or UUID; a new app gets a UUID.\n");
        s.push_str("- `app_inspect(action=read, app, path)` / `app_modify(action=patch, app, path, edits)` — read and edit by hashline.\n");
        s.push_str("- `app_modify(action=diff, app, path, content)` — preview a complete-file change without writing it.\n");
        s.push_str("- `app_inspect(action=search, app, pattern)` — search non-ignored app files; respects `.gitignore`.\n");
        s.push_str("- `install_packages(app=..., packages=[...])` — npm-install into an app.\n");
        s.push_str("- `app_assets(action=list)` — list pasted conversation images.\n");
        s.push_str("- `app_assets(action=copy_images, image_ids, app)` — copy images into `_images/` for `<img src=\"...\">`.\n");
        s.push_str("- `app_assets(action=copy_file, file_name, app)` — copy a space file's text into the app's KV store.\n\n");

        s.push_str("### KV Store (persistent key-value per app)\n");
        s.push_str("Each app has a SQLite-backed KV store. Call these from frontend JS:\n");
        s.push_str("- `PUT <app_url>/_api/kv/<key>` — upsert a value (body = raw text)\n");
        s.push_str("- `GET <app_url>/_api/kv/<key>` — read a value\n");
        s.push_str("- `DELETE <app_url>/_api/kv/<key>` — delete a value\n");
        s.push_str("- `GET <app_url>/_api/kv` — list all keys (returns JSON array)\n\n");

        s.push_str("### File Upload\n");
        s.push_str("- `POST <app_url>/_api/upload` with `multipart/form-data` — upload a file. Returns `{\"name\", \"url\"}`. Files persist and are served via GET.\n\n");

        s.push_str("### Using User Images\n");
        s.push_str("1. `app_assets(action=list)` to see conversation images.\n");
        s.push_str(
            "2. `app_assets(action=copy_images, image_ids, app)` to copy them into `_images/`.\n",
        );
        s.push_str("3. Use returned URLs in `<img src=\"...\">` tags.\n\n");

        s.push_str("### Using Space Files\n");
        s.push_str("- `app_assets(action=copy_file, file_name, app)` copies file text into KV under `_file:<name>`. Read it via `GET <app_url>/_api/kv/_file:<name>`.\n\n");

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
            tool_calls: None,
            tool_call_id: None,
            images: urls,
        })
    }

    /// Names of the active space's existing apps (directory listing).
    pub(crate) fn list_apps(&self) -> Vec<String> {
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

/// Pick a thinking-phrase index and spinner colour pseudo-randomly (seeded from
/// the clock; no rng dep).
/// Each fenced code block in `md` as `(language, code)`.
pub(super) fn code_blocks(md: &str) -> Vec<(Option<String>, String)> {
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

pub(super) fn pick_greeting() -> &'static str {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    super::GREETINGS[n % super::GREETINGS.len()]
}

pub(super) fn pick_flavor() -> (usize, Color) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as usize);
    (n % THINKING.len(), SPINNER_COLORS[n % SPINNER_COLORS.len()])
}

/// Short session title from the first user message.
/// The instruction block appended to the system prompt when web mode is on:
/// forces search-first, inline `[n]` citations, and a trailing Sources list.
/// `today` keeps the model from hedging with stale training-data dates.
pub(super) fn web_mode_clause(today: &str) -> String {
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

pub(super) fn title_from(text: &str) -> String {
    let t: String = text.chars().take(40).collect();
    if t.trim().is_empty() {
        "new chat".to_string()
    } else {
        t
    }
}

/// Recover a `(ToolCall, result)` pair from a stored `tool_call` row's JSON
/// content (`{"name","arguments","result"}`), for replaying past tool use
/// back into the request history. `id` is synthesized — it only needs to
/// match between the assistant/tool pair built at the same call site.
fn parse_tool_call_row(content: &str) -> Option<(ToolCall, String)> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let arguments = v.get("arguments")?.as_str()?.to_string();
    let result = v.get("result")?.as_str()?.to_string();
    Some((
        ToolCall {
            id: "call_0".to_string(),
            name,
            arguments,
        },
        result,
    ))
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
pub(super) fn split_inline_reasoning(text: &str) -> (String, Option<String>) {
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
