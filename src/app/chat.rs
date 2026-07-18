use anyhow::Result;
use chrono::Utc;
use ratatui::style::Color;
use tokio::sync::mpsc;

use crate::db::Message;
use crate::provider::{ChatMessage, ChatParams, StreamEvent, ToolCall};

use super::{transcribe, App, SPINNER_COLORS, THINKING, parse_topic, verbosity_clause};

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
        if self.is_streaming() {
            self.status = match &self.stream_session {
                Some((_, title)) => format!("wait — response still streaming in: {title}"),
                None => "wait for the current response to finish".to_string(),
            };
            self.set_input(&text);
            return Ok(());
        }
        if self.deferred_send.is_some() || self.describe_rx.is_some() {
            self.status = "wait — still understanding the attached image".to_string();
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
                    title: title.clone(),
                    model: model.clone(),
                    slug: None,
                    created_at: Utc::now().to_rfc3339(),
                    compact_summary: None,
                    compact_through: 0,
                    web_mode: self.web_mode,
                    swarm_mode: false,
                }
            } else {
                let mut s = self
                    .db
                    .create_session(&title, &model, &self.active_space.id)?;
                // Carry a pre-session `/web` toggle onto the session it creates.
                if self.web_mode {
                    s.web_mode = true;
                    let _ = self.db.set_session_web_mode(&s.id, true);
                }
                s
            };
            self.session = Some(s);
        }
        let session_id = self.session.as_ref().unwrap().id.clone();

        let message_id = if self.incognito {
            uuid::Uuid::new_v4().to_string()
        } else {
            self.db.add_user_message(&session_id, &text)?
        };
        let images = if self.pending_images.is_empty() {
            Vec::new()
        } else {
            let paths: Vec<String> = self
                .pending_images
                .iter()
                .map(|p| p.path.to_string_lossy().to_string())
                .collect();
            let images = if self.incognito {
                self.pending_images.clear();
                paths
                    .iter()
                    .map(|p| crate::db::MessageImage {
                        id: String::new(),
                        path: p.clone(),
                        description: None,
                    })
                    .collect()
            } else {
                let images = self.db.add_message_images(&message_id, &paths)?;
                self.pending_images.clear();
                images
            };
            images
        };
        self.messages.push(Message {
            id: message_id,
            role: "user".to_string(),
            content: text,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: None,
            images,
            persona: None,
        });

        // Deferred-send gate: a non-vision model can't see the images just
        // attached, so send a description request first and resume once every
        // description lands (`resume_deferred_send`, driven by `on_described`).
        if !self.current_model_supports_images() {
            let missing = self.undescribed_images();
            if !missing.is_empty() {
                if self.transcriber_model.trim().is_empty() {
                    self.status =
                        "model can't see images — set an image model in /config or pick a vision model"
                            .to_string();
                    return Ok(());
                }
                self.deferred_send = Some(String::new()); // marker: resume streams the reply
                self.start_describing(missing);
                return Ok(());
            }
        }

        if self.session.as_ref().is_some_and(|s| s.swarm_mode) {
            self.start_swarm_turn();
            return Ok(());
        }
        self.start_stream()
    }

    /// `/gen <prompt>`: generate an image, save it, and show it as an assistant
    /// message with the image attached.
    pub(crate) fn cmd_generate_image(&mut self, prompt: &str) -> Result<()> {
        if prompt.is_empty() {
            self.status = "usage: /gen <description of image to create>".to_string();
            return Ok(());
        }
        let model = self.image_gen_model.trim().to_string();
        if model.is_empty() {
            self.status = "no image generation model configured — set one in /config".to_string();
            return Ok(());
        }
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.status = format!("image gen model backend unavailable: {model}");
            return Ok(());
        };
        if self.is_streaming() {
            self.status = "wait for the current response to finish first".to_string();
            return Ok(());
        }
        if self.session.is_none() {
            if self.incognito {
                self.session = Some(crate::db::Session {
                    id: uuid::Uuid::new_v4().to_string(),
                    title: "Generate Image".to_string(),
                    model: model.clone(),
                    slug: None,
                    created_at: Utc::now().to_rfc3339(),
                    compact_summary: None,
                    compact_through: 0,
                    web_mode: false,
                    swarm_mode: false,
                });
            } else {
                let s = self.db.create_session("Generate Image", &model, &self.active_space.id)?;
                self.session = Some(s);
            }
            return self.cmd_generate_image(prompt);
        };
        let session_id = self.session.as_ref().unwrap().id.clone();

        self.status = "Generating image…".to_string();
        self.scroll = 0;

        let png_bytes = match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                provider.generate_image(&raw_model, prompt, "1024x1024", None).await
            })
        }) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.status = format!("image generation failed: {e}");
                return Ok(());
            }
        };

        let id = uuid::Uuid::new_v4().to_string();
        let filename = format!("{id}.png");
        let img_path = if self.incognito {
            let d = self.incognito_img_dir.get_or_insert_with(|| {
                let p = std::env::temp_dir().join(format!("nexus-incognito-{}", uuid::Uuid::new_v4()));
                let _ = std::fs::create_dir_all(&p);
                p
            });
            d.join(&filename)
        } else {
            let dir = self.space.images_dir(&self.active_space.name);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.status = format!("cannot create images dir: {e}");
                return Ok(());
            }
            dir.join(&filename)
        };
        if let Err(e) = std::fs::write(&img_path, &png_bytes) {
            self.status = format!("cannot write image: {e}");
            return Ok(());
        }
        // Also copy to space files dir for searchability (skipped in incognito).
        if !self.incognito {
            let files_dir = self.space.files_dir(&self.active_space.name);
            let _ = std::fs::create_dir_all(&files_dir);
            let _ = std::fs::write(files_dir.join(&filename), &png_bytes);
        }

        let description = format!("generated image of {prompt}");

        // Push a user message with the prompt
        let user_msg = format!("/gen {prompt}");
        let user_msg_id = if self.incognito {
            uuid::Uuid::new_v4().to_string()
        } else {
            self.db.add_user_message(&session_id, &user_msg)?
        };
        self.messages.push(Message {
            id: user_msg_id,
            role: "user".to_string(),
            content: user_msg,
            model: None,
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: None,
            images: Vec::new(),
            persona: None,
        });

        // Push an assistant message showing the generated image
        let assistant_content = format!("Here's your generated image: **{prompt}**");
        let msg_id = if self.incognito {
            uuid::Uuid::new_v4().to_string()
        } else {
            self.db.insert_message(
                &session_id,
                "assistant",
                &assistant_content,
                Some(&model),
                None,
                None,
                None,
                Some("Generated"),
            )?
        };
        let images = if self.incognito {
            vec![crate::db::MessageImage {
                id: id.clone(),
                path: img_path.to_string_lossy().to_string(),
                description: Some(description.clone()),
            }]
        } else {
            let imgs = self.db.add_message_images(
                &msg_id,
                &[img_path.to_string_lossy().to_string()],
            )?;
            for img in &imgs {
                let _ = self.db.set_image_description(&img.id, &description);
            }
            imgs
        };
        self.messages.push(Message {
            id: msg_id,
            role: "assistant".to_string(),
            content: assistant_content,
            model: Some(model),
            reasoning: None,
            tokens: None,
            secs: None,
            phrase: Some("Generated".to_string()),
            images,
            persona: None,
        });

        self.status = "image generated ✓".to_string();
        self.refresh_toolbox();
        Ok(())
    }

    /// The exact message list a completion request will carry: system prompt,
    /// compaction digest, forced skill, then the effective conversation tail.
    /// User messages with images become multimodal parts for vision models, or
    /// get their stored descriptions appended as text for everything else.
    pub(crate) fn build_history(&mut self) -> Vec<ChatMessage> {
        let mut history: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 3);
        history.push(ChatMessage::text("system", self.system_prompt()));
        // Include space-file images inline for vision models.
        if self.current_model_supports_images() {
            if let Some(img_msg) = self.space_images_message() {
                history.push(img_msg);
            }
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
            // Skip research/progress/error rows — background job scratch,
            // UI-only prompts, and transport failures are never shown to the
            // model. Skip per-persona swarm round replies too — supporting
            // detail for that turn, not part of ongoing conversation context;
            // only the turn's final synthesis reply carries forward.
            if m.role == "research_stage"
                || m.role == "research_plan"
                || m.role == "error"
                || m.persona.is_some()
            {
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
            if m.role == "user" && !m.images.is_empty() {
                if vision {
                    // ponytail: PNGs re-read from disk every send; cache in RAM
                    // if large sessions ever make this noticeable.
                    cm.images = m
                        .images
                        .iter()
                        .filter_map(|img| std::fs::read(&img.path).ok())
                        .map(|bytes| crate::app::transcribe::png_bytes_data_url(&bytes))
                        .collect();
                } else {
                    for img in &m.images {
                        if let Some(d) = &img.description {
                            cm.content.push_str(&format!("\n\n[Image: {d}]"));
                        }
                    }
                }
            }
            history.push(cm);
        }
        history
    }

    /// (message_images row id, path) for every image in the effective range
    /// that still has no description.
    pub(crate) fn undescribed_images(&self) -> Vec<(String, String)> {
        self.effective_messages()
            .iter()
            .flat_map(|m| m.images.iter())
            .filter(|i| i.description.is_none())
            .map(|i| (i.id.clone(), i.path.clone()))
            .collect()
    }

    /// Build history and fire the streaming request — the shared tail of
    /// `send_message` and `resume_deferred_send`.
    pub(super) fn start_stream(&mut self) -> Result<()> {
        let Some(model) = self.current_model.clone() else {
            return Ok(());
        };
        let Some((provider, raw_model)) = self.resolve_model_backend(&model) else {
            self.status = format!("model backend unavailable: {model} — pick another with /model");
            return Ok(());
        };
        let history = self.build_history();
        let params = ChatParams {
            reasoning_effort: self.reasoning.get(&model).cloned(),
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
        self.stream_session = self
            .session
            .as_ref()
            .map(|s| (s.id.clone(), s.title.clone()));
        self.stream_rx = Some(rx);
        self.stream_abort = Some(abort);
        self.streaming = Some(String::new());
        self.thinking_text.clear();
        self.tool_status = None;
        self.stream_usage = None;
        self.stream_started = Some(std::time::Instant::now());
        self.spinner_frame = 0;
        let (idx, color) = pick_flavor();
        self.thinking_idx = idx;
        self.spinner_color = color;
        self.status.clear();
        self.scroll = 0;
        self.prev_total = 0;
        Ok(())
    }

    pub fn on_stream_event(&mut self, ev: StreamEvent) -> Result<()> {
        match ev {
            StreamEvent::Token(t) => {
                if let Some(buf) = self.streaming.as_mut() {
                    buf.push_str(&t);
                }
            }
            StreamEvent::Reasoning(t) => self.thinking_text.push_str(&t),
            StreamEvent::Usage(u) => self.stream_usage = Some(u),
            StreamEvent::Status(s) => self.tool_status = Some(s),
            StreamEvent::ToolCall {
                name,
                arguments,
                result,
            } => {
                if name == "install_skill" && result.starts_with("installed") {
                    self.reload_skills(); // new skill shows in the system prompt next turn
                }
                if name == "generate_image" {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&result) {
                        if let (Some(_id), Some(path)) = (v["id"].as_str(), v["path"].as_str()) {
                            let p = std::path::Path::new(path);
                            if p.exists() {
                                self.pending_gen_images.push(transcribe::PendingGenImage {
                                    path: p.to_path_buf(),
                                    description: v["description"].as_str().unwrap_or("generated image").to_string(),
                                });
                            }
                        }
                    }
                }
                let content =
                    serde_json::json!({ "name": name, "arguments": arguments, "result": result })
                        .to_string();
                // Persist to the stream's origin session (may not be active).
                let target = self
                    .stream_session
                    .as_ref()
                    .map(|(id, _)| id.clone())
                    .or_else(|| self.session.as_ref().map(|s| s.id.clone()));
                if let Some(id) = &target {
                    if !self.incognito {
                        let _ = self.db.add_tool_call_message(id, &content);
                    }
                }
                if self.viewing_stream() {
                    self.messages.push(Message {
                        id: String::new(),
                        role: "tool_call".to_string(),
                        content,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                        persona: None,
                    });
                }
            }
            StreamEvent::Done => self.finish_stream()?,
            StreamEvent::Error(e) => {
                // Capture the origin before `finish_stream` clears it. Save any
                // partial answer first, then append the failure so transcript
                // order matches what the user saw.
                let target = self
                    .stream_session
                    .as_ref()
                    .map(|(id, _)| id.clone())
                    .or_else(|| self.session.as_ref().map(|s| s.id.clone()));
                let viewing = self.viewing_stream();
                let status = match (&self.stream_session, viewing) {
                    (Some((_, title)), false) => format!("stream error in {title}: {e}"),
                    _ => format!("stream error: {e}"),
                };
                self.finish_stream()?;
                if let Some(id) = &target {
                    if !self.incognito {
                        self.db.add_error_message(id, &e)?;
                    }
                }
                if viewing {
                    self.messages.push(Message {
                        id: String::new(),
                        role: "error".to_string(),
                        content: e,
                        model: None,
                        reasoning: None,
                        tokens: None,
                        secs: None,
                        phrase: None,
                        images: Vec::new(),
                        persona: None,
                    });
                } else if let Some(id) = target {
                    self.unread.insert(id);
                }
                self.status = status;
            }
        }
        Ok(())
    }

    /// Esc while a response streams: abort the background chat task (kills any
    /// in-flight request and tool loop) and keep whatever text already arrived.
    pub fn stop_stream(&mut self) -> Result<()> {
        if !self.is_streaming() {
            return Ok(());
        }
        if let Some(h) = self.stream_abort.take() {
            h.abort();
        }
        self.finish_stream()?;
        self.status = "response stopped".to_string();
        Ok(())
    }

    /// Kill the in-flight stream and throw its partial text away — used when
    /// the origin session is deleted (nothing left to save into).
    pub(crate) fn discard_stream(&mut self) {
        if let Some(h) = self.stream_abort.take() {
            h.abort();
        }
        self.stream_rx = None;
        self.streaming = None;
        self.stream_session = None;
        self.thinking_text.clear();
        self.stream_usage = None;
        self.stream_started = None;
        self.tool_status = None;
    }

    fn finish_stream(&mut self) -> Result<()> {
        self.stream_rx = None;
        self.stream_abort = None;
        self.tool_status = None;
        let origin = self.stream_session.take();
        let started = self.stream_started.take();
        let mut reasoning = std::mem::take(&mut self.thinking_text);
        let has_pending = !self.pending_gen_images.is_empty();
        let mut buf = self.streaming.take().unwrap_or_default();
        if buf.is_empty() && !has_pending {
            return Ok(());
        }
        // Some reasoning models (routed without the separate `reasoning` delta
        // field) inline their thinking as `<think>...</think>` in `content`
        // itself. Pull that out so the stored/displayed/copied message is just
        // the actual answer, not the thinking — same treatment as the explicit
        // reasoning channel above.
        let (buf, inline) = split_inline_reasoning(&buf);
        if let Some(inline) = inline {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&inline);
        }
        // Did the stream finish in the session the user is looking at?
        let viewing = match (&origin, &self.session) {
            (Some((id, _)), Some(s)) => *id == s.id,
            (None, _) => true,
            (Some(_), None) => false,
        };
        let model = self.current_model.clone();
        // Prefer the provider's exact usage; fall back to a ~4-chars/token estimate.
        let usage = self.stream_usage.take();
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
        let secs = started.map(|s| s.elapsed().as_secs_f64());
        let reasoning = (!reasoning.is_empty()).then_some(reasoning);
        let phrase = Some(THINKING[self.thinking_idx].1.to_string());

        // The response always lands in its origin session.
        let target = origin
            .as_ref()
            .map(|(id, _)| id.clone())
            .or_else(|| self.session.as_ref().map(|s| s.id.clone()));
        let mut msg_id = String::new();
        if let Some(id) = &target {
            if !self.incognito {
                msg_id = self.db.add_assistant_message(
                    id,
                    &buf,
                    model.as_deref(),
                    reasoning.as_deref(),
                    tokens,
                    secs,
                    phrase.as_deref(),
                )?;
            }
        }
        let images = if !self.incognito && !msg_id.is_empty() && !self.pending_gen_images.is_empty() {
            let paths: Vec<String> = self.pending_gen_images.iter().map(|i| i.path.to_string_lossy().to_string()).collect();
            let imgs = self.db.add_message_images(&msg_id, &paths).unwrap_or_default();
            for (img, pending) in imgs.iter().zip(&self.pending_gen_images) {
                let _ = self.db.set_image_description(&img.id, &pending.description);
            }
            self.pending_gen_images.clear();
            imgs
        } else {
            Vec::new()
        };
        if viewing {
            self.messages.push(Message {
                id: msg_id,
                role: "assistant".to_string(),
                content: buf,
                model,
                reasoning,
                tokens,
                secs,
                phrase,
                images,
                persona: None,
            });
            // These read the *active* conversation, so they only make sense here.
            if !self.incognito {
                self.maybe_generate_title();
                self.maybe_extract_memory();
                self.maybe_compact();
            }
        } else if let Some((id, title)) = origin {
            self.unread.insert(id);
            self.status = format!("✓ response ready in: {title}");
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
            s.title = topic.clone();
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
                "This session came from /research — prefer search_sources over web_search for \
                 follow-ups; only use web_search on a miss."
                    .to_string(),
            );
        }
        parts.join("\n\n")
    }

    /// `o` in the history pane: open the `[n]` citation under the current
    /// text selection (via the `open` crate), resolved against the Sources
    /// list of the message the selection belongs to. Every miss surfaces as
    /// a status message rather than doing nothing silently.
    pub(crate) fn open_citation_under_selection(&mut self) {
        let Some(selected) = self.sel.selected_text() else {
            self.status = "select a [n] citation, then press o".to_string();
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
        match citations.iter().find(|(num, _)| *num == n) {
            Some((_, url)) => {
                // Don't actually launch a browser under `cargo test`.
                #[cfg(not(test))]
                let _ = open::that_detached(url);
                self.status = format!("opened [{n}]: {url}");
            }
            None => self.status = format!("no source [{n}] in this message"),
        }
    }

    /// If the given rendered line index is an image line, open the image in
    /// the default OS viewer. Returns true if it was an image line.
    pub(crate) fn open_image_at_line(&self, line: usize) -> bool {
        if let Some(Some(path)) = self.history_cache.image_at_line.get(line) {
            let _ = open::that_detached(path);
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
            self.status = "select a [n] citation, then press p/x".to_string();
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
        self.clear_image_state();
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
            s.push_str(&format!("- {}: {}\n", skill.name, skill.description));
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
                      Call `read_script(path)` to see one, `edit_script` to modify, or \
                      `run_script(space=true, path=...)` to execute. \
                      The `path` parameter is relative to the scripts dir — do NOT prefix `scripts/`.\n"
            .to_string();
        for script in &self.scripts_cache {
            s.push_str(&format!(
                "- {} ({})\n",
                script.name,
                human_size(script.size as i64),
            ));
        }
        Some(s.trim_end().to_string())
    }

    /// Names/types/sizes of the space's imported files — content stays off the
    /// wire until the model calls `search_files`/`read_file`.
    fn files_section(&self) -> Option<String> {
        if self.files_cache.is_empty() {
            return None;
        }
        let mut s = "## Files\nThe user has imported these files into this space. Do not guess \
                     their contents: call `search_files` to find relevant passages, or \
                     `read_file` to read one (200 lines per call, use offset to page).\n"
            .to_string();
        for f in &self.files_cache {
            let kind = std::path::Path::new(&f.name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("file")
                .to_lowercase();
            s.push_str(&format!(
                "- {} ({kind}, {}, {})\n",
                f.name,
                human_size(f.size),
                f.status
            ));
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
                     uploads, and copy_images_to_app/copy_file_to_app to bring user data into the app.\n\n\
                     **CRITICAL: URLs are UUID-based.** The UUID comes from `write_file`'s tool result \
                     (\"live at http://...\"). Copy it from there — never invent or guess a UUID. \
                     If you don't have the result, call `read_app_file` or `grep_app` on the app to \
                     rediscover it.\n\n"
            .to_string();

        s.push_str("### Tools\n");
        s.push_str("- `write_file(app, path, content)` — create/edit a file. App can be a name or its UUID. First write to a new app auto-generates a UUID.\n");
        s.push_str("- `read_app_file(app, path)` / `edit_file(app, path, edits)` — read and edit by hashline.\n");
        s.push_str("- `grep_app(app, pattern)` — search all files in an app.\n");
        s.push_str("- `install_packages(app=..., packages=[...])` — npm-install into an app.\n");
        s.push_str("- `list_images` — list pasted conversation images.\n");
        s.push_str("- `copy_images_to_app(image_ids, app)` — copy images into `_images/` for `<img src=\"...\">`.\n");
        s.push_str("- `copy_file_to_app(file_name, app)` — copy a space file's text into the app's KV store.\n\n");

        s.push_str("### KV Store (persistent key-value per app)\n");
        s.push_str("Each app has a SQLite-backed KV store. Call these from frontend JS:\n");
        s.push_str("- `PUT <app_url>/_api/kv/<key>` — upsert a value (body = raw text)\n");
        s.push_str("- `GET <app_url>/_api/kv/<key>` — read a value\n");
        s.push_str("- `DELETE <app_url>/_api/kv/<key>` — delete a value\n");
        s.push_str("- `GET <app_url>/_api/kv` — list all keys (returns JSON array)\n\n");

        s.push_str("### File Upload\n");
        s.push_str("- `POST <app_url>/_api/upload` with `multipart/form-data` — upload a file. Returns `{\"name\", \"url\"}`. Files persist and are served via GET.\n\n");

        s.push_str("### Using User Images\n");
        s.push_str("1. `list_images` to see conversation images.\n");
        s.push_str("2. `copy_images_to_app(image_ids, app)` to copy them into `_images/`.\n");
        s.push_str("3. Use returned URLs in `<img src=\"...\">` tags.\n\n");

        s.push_str("### Using Space Files\n");
        s.push_str("- `copy_file_to_app(file_name, app)` copies file text into KV under `_file:<name>`. Read it via `GET <app_url>/_api/kv/_file:<name>`.\n\n");

        let apps = self.list_apps();
        if apps.is_empty() {
            s.push_str("No apps exist in this space yet.");
        } else {
            s.push_str("Existing apps:\n");
            for a in &apps {
                if let Some(uuid) = self.app_server.as_ref().and_then(|s| s.registry().resolve(&self.active_space.name, a)) {
                    s.push_str(&format!("- {a} (uuid {uuid})\n"));
                } else {
                    s.push_str(&format!("- {a}\n"));
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
                    "png" => "image/png",
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
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        apps.sort();
        apps
    }
}

/// Pick a thinking-phrase index and spinner colour pseudo-randomly (seeded from
/// the clock; no rng dep).
/// Each fenced ``` code block in `md` as `(language, code)`.
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
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    super::GREETINGS[n % super::GREETINGS.len()]
}

pub(super) fn pick_flavor() -> (usize, Color) {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    (n % THINKING.len(), SPINNER_COLORS[n % SPINNER_COLORS.len()])
}

/// Short session title from the first user message.
/// The instruction block appended to the system prompt when web mode is on:
/// forces search-first, inline `[n]` citations, and a trailing Sources list.
/// `today` keeps the model from hedging with stale training-data dates.
pub(super) fn web_mode_clause(today: &str) -> String {
    format!(
        "Web answer mode is ON for this session. Today's date is {today}. Before answering, you \
         MUST call web_search with a focused query (and fetch_url on the most promising results) — \
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
pub(crate) fn human_size(bytes: i64) -> String {
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
