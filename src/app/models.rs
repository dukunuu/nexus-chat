use anyhow::Result;
use ratatui::widgets::ListState;

use super::{App, KeyTarget, LoginMsg, ModelPanel, ModelPickTarget, Popup};
use crate::config;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model};
use chrono::Utc;

impl App {
    /// Rebuild `self.backends` from `self.saved` — every credential on file
    /// becomes a usable backend at once. Called after `self.saved` changes
    /// wholesale (app startup); `/login` updates one slot directly instead.
    pub(crate) fn rebuild_all_backends(&mut self) {
        self.backends = super::Backends::default();
        if let Some(k) = self.saved.openrouter_key.clone() {
            self.backends
                .set(BackendTag::OpenRouter, OpenRouter::openrouter(k));
        }
        if let Some(k) = self.saved.openai_key.clone() {
            self.backends.set(BackendTag::OpenAi, OpenRouter::openai(k));
        }
        if let Some(k) = self.saved.opencode_key.clone() {
            self.backends
                .set(BackendTag::OpencodeGo, OpenRouter::opencode_go(k));
        }
        if let Some(c) = self.saved.codex.clone() {
            self.backends
                .set(BackendTag::Codex, OpenRouter::openai_codex(c.access));
        }
    }

    /// Resolve a persisted/composite model id to the backend and raw model id
    /// to send on the wire.
    pub(crate) fn resolve_model_backend(&self, id: &str) -> Option<(OpenRouter, String)> {
        self.backends.resolve(id)
    }

    /// Resolve a background utility model setting. If an old bare OpenRouter
    /// default would be misrouted to a non-OpenRouter backend, fall back to
    /// that backend's own utility model instead of silently sending an invalid
    /// model id (which breaks memory/title jobs after OpenAI/Codex/Go login).
    pub(crate) fn resolve_utility_model_backend(
        &self,
        configured_id: &str,
    ) -> Option<(OpenRouter, String)> {
        self.resolve_feature_model_backend(configured_id, OpenRouter::default_utility_model)
    }

    pub(crate) fn resolve_feature_model_backend(
        &self,
        configured_id: &str,
        default: fn(&OpenRouter) -> &'static str,
    ) -> Option<(OpenRouter, String)> {
        let configured_id = configured_id.trim();
        if !configured_id.is_empty()
            && let Some((provider, raw)) = self.resolve_model_backend(configured_id)
            && self.resolved_model_looks_valid(configured_id, provider.backend_tag(), &raw)
        {
            return Some((provider, raw));
        }

        let provider = self
            .current_model
            .as_deref()
            .and_then(|id| self.resolve_model_backend(id).map(|(provider, _)| provider))
            .or_else(|| {
                self.backends
                    .configured_tags()
                    .first()
                    .and_then(|tag| self.backends.get(*tag).cloned())
            })?;
        Some((provider.clone(), default(&provider).to_string()))
    }

    fn resolved_model_looks_valid(
        &self,
        original_id: &str,
        backend: BackendTag,
        raw: &str,
    ) -> bool {
        if self.models.is_empty() {
            // The classic bad state is a legacy OpenRouter id like
            // `google/gemini-*` being resolved against OpenAI/Codex/Go because
            // OpenRouter is not configured. Those backends' built-in defaults
            // do not contain `/`, so treat slashy bare ids as OpenRouter-only.
            return backend == BackendTag::OpenRouter || !original_id.contains('/');
        }
        self.models
            .iter()
            .any(|m| m.backend == backend && m.id == raw)
    }

    /// Label for the model picker's current backend filter.
    pub(crate) fn model_backend_filter_label(&self) -> &'static str {
        self.model_backend_filter
            .map(BackendTag::display_name)
            .unwrap_or("all backends")
    }

    /// Cycle the model picker's backend filter among whichever backends are
    /// actually configured (`None` = show everything).
    pub(crate) fn cycle_model_backend_filter(&mut self) {
        let tags = self.backends.configured_tags();
        if tags.is_empty() {
            self.model_backend_filter = None;
            return;
        }
        self.model_backend_filter = match self.model_backend_filter {
            None => Some(tags[0]),
            Some(cur) => {
                let next = tags.iter().position(|t| *t == cur).map(|i| i + 1);
                next.and_then(|i| tags.get(i).copied())
            }
        };
        self.reset_model_selection();
    }

    pub(super) fn open_model_picker(&mut self) {
        self.model_pick_target = ModelPickTarget::Session;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the memory model
    /// (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_memory(&mut self) {
        self.model_pick_target = ModelPickTarget::Memory;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the transcriber
    /// model (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_transcriber(&mut self) {
        self.model_pick_target = ModelPickTarget::Transcriber;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the image
    /// generation model (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_image_gen(&mut self) {
        self.model_pick_target = ModelPickTarget::ImageGen;
        self.open_model_picker_impl();
    }

    pub(crate) fn open_model_picker_for_video_gen(&mut self) {
        self.model_pick_target = ModelPickTarget::VideoGen;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the OCR model
    /// (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_ocr(&mut self) {
        self.model_pick_target = ModelPickTarget::Ocr;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the research
    /// model (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_research(&mut self) {
        self.model_pick_target = ModelPickTarget::Research;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the escalation
    /// model (in `/config`) instead of the active session's model.
    pub(crate) fn open_model_picker_for_escalation(&mut self) {
        self.model_pick_target = ModelPickTarget::Escalation;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the model for
    /// one row of the active session's `/swarm` roster.
    pub(crate) fn open_model_picker_for_swarm_persona(&mut self, row: usize) {
        self.model_pick_target = ModelPickTarget::SwarmPersona(row);
        self.open_model_picker_impl();
    }

    fn open_model_picker_impl(&mut self) {
        if !self.backends.any() {
            self.open_login_popup();
            return;
        }
        if self.models.is_empty() {
            self.status = "loading models…".to_string();
            self.fetch_models();
            self.popup = Popup::Model;
            return;
        }
        self.model_filter.clear();
        self.model_focus = if self.favorite_models().is_empty() {
            ModelPanel::Available
        } else {
            ModelPanel::Favorites
        };
        self.reset_model_selection();
        self.popup = Popup::Model;
    }

    /// Point each panel's selection at its first row (or nothing if empty).
    pub(crate) fn reset_model_selection(&mut self) {
        self.fav_state
            .select((!self.favorite_models().is_empty()).then_some(0));
        self.avail_state
            .select((!self.available_models().is_empty()).then_some(0));
    }

    /// `/login`: open the provider selector. Also what `open_model_picker`
    /// falls back to when nothing's configured yet.
    pub(super) fn open_login_popup(&mut self) {
        self.login_selected = 0;
        self.popup = Popup::Login;
    }

    pub(crate) fn move_login_selection(&mut self, delta: i32) {
        self.login_selected = super::clamp_cursor(self.login_selected, 4, delta);
    }

    /// Enter on a `/login` row: OpenRouter/OpenCode Go/OpenAI activate
    /// immediately from their env var if set, else drop into the key-paste
    /// popup; Codex has no key, so it starts the device-code login instead.
    pub(crate) fn confirm_login_selection(&mut self) {
        match self.login_selected {
            0 => self.start_key_login(
                KeyTarget::OpenRouter,
                config::OPENROUTER_ENV_KEY,
                "openrouter",
                "OpenRouter",
            ),
            1 => self.start_key_login(
                KeyTarget::OpencodeGo,
                config::OPENCODE_ENV_KEY,
                "opencode",
                "OpenCode Go",
            ),
            2 => self.start_key_login(
                KeyTarget::OpenAi,
                config::OPENAI_ENV_KEY,
                "openai",
                "OpenAI",
            ),
            _ => self.start_codex_login(),
        }
    }

    fn start_key_login(&mut self, target: KeyTarget, env_var: &str, tag: &str, label: &str) {
        if let Ok(v) = std::env::var(env_var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                self.activate_key(target, tag, v);
                return;
            }
        }
        self.key_target = target;
        self.key_input.clear();
        self.status = format!("paste your {label} API key, then Enter");
        self.popup = Popup::Key;
    }

    /// Label for the key-entry popup's hint, matching whichever `/login`
    /// row opened it.
    pub(crate) fn key_target_label(&self) -> &'static str {
        match self.key_target {
            KeyTarget::OpenRouter => "OpenRouter",
            KeyTarget::OpenAi => "OpenAI",
            KeyTarget::OpencodeGo => "OpenCode Go",
        }
    }

    fn activate_key(&mut self, target: KeyTarget, tag: &str, key: String) {
        if let Err(e) = config::save_provider_key(tag, &key) {
            self.status = format!("could not save key: {e}");
        }
        let (backend_tag, provider) = match target {
            KeyTarget::OpenRouter => (BackendTag::OpenRouter, OpenRouter::openrouter(key.clone())),
            KeyTarget::OpenAi => (BackendTag::OpenAi, OpenRouter::openai(key.clone())),
            KeyTarget::OpencodeGo => (BackendTag::OpencodeGo, OpenRouter::opencode_go(key.clone())),
        };
        match target {
            KeyTarget::OpenRouter => self.saved.openrouter_key = Some(key),
            KeyTarget::OpenAi => self.saved.openai_key = Some(key),
            KeyTarget::OpencodeGo => self.saved.opencode_key = Some(key),
        }
        self.backends.set(backend_tag, provider);
        self.popup = Popup::None;
        self.status = "key saved, loading models…".to_string();
        self.fetch_models();
    }

    pub(crate) fn start_codex_login(&mut self) {
        // A previous login task can be left around after cancellation/timeout while
        // the UI has no useful way to resume it. Starting again should replace the
        // receiver instead of trapping the user behind a stale "already running" gate.
        self.login_rx = None;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.login_rx = Some(rx);
        self.status = "starting OpenAI Codex login…".to_string();
        tokio::spawn(async move {
            let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let forward = tx.clone();
            tokio::spawn(async move {
                while let Some(s) = status_rx.recv().await {
                    let _ = forward.send(LoginMsg::Status(s));
                }
            });
            let result = crate::config::login_openai_codex_device(status_tx)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(LoginMsg::Done(result));
        });
    }

    pub(crate) fn on_login_result(&mut self, msg: Option<LoginMsg>) {
        match msg {
            Some(LoginMsg::Status(s)) => self.status = s,
            Some(LoginMsg::Done(Ok(creds))) => {
                self.login_rx = None;
                self.backends.set(
                    BackendTag::Codex,
                    OpenRouter::openai_codex(creds.access.clone()),
                );
                self.saved.codex = Some(creds);
                self.status = "OpenAI Codex login saved, loading models…".to_string();
                self.fetch_models();
                self.refresh_toolbox();
            }
            Some(LoginMsg::Done(Err(e))) => {
                self.login_rx = None;
                self.status = format!("OpenAI Codex login failed: {e}");
            }
            None => self.login_rx = None,
        }
    }

    /// Save the key pasted into `Popup::Key`, for whichever backend
    /// `self.key_target` says it's for.
    pub(crate) fn confirm_key(&mut self) {
        let key = std::mem::take(&mut self.key_input);
        let key = key.trim().to_string();
        if key.is_empty() {
            self.status = "no key entered".to_string();
            self.popup = Popup::None;
            return;
        }
        let (target, tag) = match self.key_target {
            KeyTarget::OpenRouter => (KeyTarget::OpenRouter, "openrouter"),
            KeyTarget::OpenAi => (KeyTarget::OpenAi, "openai"),
            KeyTarget::OpencodeGo => (KeyTarget::OpencodeGo, "opencode"),
        };
        self.activate_key(target, tag, key);
    }

    /// Favorite models matching the search filter, most-recently-used first.
    /// When `image_gen_only` is true, only includes models that support image generation.
    pub(crate) fn favorite_models(&self) -> Vec<&Model> {
        self.filtered_panel(true)
    }

    /// Non-favorite models matching the search filter, most-recently-used first.
    pub(crate) fn available_models(&self) -> Vec<&Model> {
        self.filtered_panel(false)
    }

    fn filtered_panel(&self, want_fav: bool) -> Vec<&Model> {
        let f = self.model_filter.to_lowercase();
        let mut out: Vec<&Model> = self
            .models
            .iter()
            .filter(|m| self.model_backend_filter.is_none_or(|t| m.backend == t))
            .filter(|m| {
                if self.model_pick_target == ModelPickTarget::ImageGen {
                    m.supports_image_generation
                } else {
                    true
                }
            })
            .filter(|m| {
                if self.model_pick_target == ModelPickTarget::VideoGen {
                    m.supports_video_generation
                } else {
                    true
                }
            })
            .filter(|m| self.favorites.contains(&super::composite_id(m)) == want_fav)
            .filter(|m| {
                f.is_empty()
                    || m.id.to_lowercase().contains(&f)
                    || m.name.to_lowercase().contains(&f)
            })
            .collect();
        // Most-recently-used first, then alphabetical.
        out.sort_by(|a, b| {
            let (ca, cb) = (super::composite_id(a), super::composite_id(b));
            let ra = self.last_used.get(&ca).cloned().unwrap_or_default();
            let rb = self.last_used.get(&cb).cloned().unwrap_or_default();
            rb.cmp(&ra).then_with(|| ca.cmp(&cb))
        });
        out
    }

    fn panel_len(&self, panel: ModelPanel) -> usize {
        match panel {
            ModelPanel::Favorites => self.favorite_models().len(),
            ModelPanel::Available => self.available_models().len(),
        }
    }

    fn state_mut(&mut self, panel: ModelPanel) -> &mut ListState {
        match panel {
            ModelPanel::Favorites => &mut self.fav_state,
            ModelPanel::Available => &mut self.avail_state,
        }
    }

    /// The composite id (see `composite_id`) of the model at this row —
    /// what gets stored in `current_model`/favorites/last-used, not the raw
    /// `Model.id` (which can collide across backends).
    fn id_at(&self, panel: ModelPanel, index: usize) -> Option<String> {
        let list = match panel {
            ModelPanel::Favorites => self.favorite_models(),
            ModelPanel::Available => self.available_models(),
        };
        list.get(index).map(|m| super::composite_id(m))
    }

    /// Context window of the active model, if known.
    pub(crate) fn context_limit(&self) -> Option<u64> {
        let id = self.current_model.as_deref()?;
        self.models
            .iter()
            .find(|m| super::composite_id(m) == id)
            .and_then(|m| m.context_length)
    }

    /// Tokens used by the current session. Exact (from the provider's usage on
    /// the last response) when idle; a ~4-chars/token estimate while streaming or
    /// before the first response.
    /// Estimate is what would actually be *sent* — system/memory prompt, the
    /// compaction digest (if any), and only the tail after it, not the full
    /// (possibly much larger) on-screen scrollback.
    pub(crate) fn context_used(&self) -> u64 {
        if !self.is_streaming()
            && let Some(total) = self.context_total
        {
            return total;
        }
        let mut chars = self.system_prompt().chars().count();
        if let Some(s) = self
            .session
            .as_ref()
            .and_then(|s| s.compact_summary.as_deref())
        {
            chars += s.chars().count();
        }
        if let Some(name) = &self.forced_skill
            && let Some(skill) = self.skills.iter().find(|s| &s.name == name)
        {
            chars += std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).chars().count())
                .unwrap_or(0);
        }
        chars += self
            .effective_messages()
            .iter()
            .map(|m| m.content.chars().count())
            .sum::<usize>();
        if let Some(buf) = self.active_streaming_text() {
            chars += buf.chars().count();
        }
        (chars / 4) as u64
    }

    fn model_supports_reasoning(&self, id: &str) -> bool {
        self.models
            .iter()
            .any(|m| super::composite_id(m) == id && m.supports_reasoning)
    }

    /// Whether the active model accepts image input (unknown model → false).
    pub(crate) fn current_model_supports_images(&self) -> bool {
        self.current_model.as_deref().is_some_and(|id| {
            self.models
                .iter()
                .any(|m| super::composite_id(m) == id && m.supports_images)
        })
    }

    pub(crate) fn reasoning_of(&self, id: &str) -> Option<&str> {
        self.reasoning.get(id).map(String::as_str)
    }

    /// Cycle the focused model's reasoning effort: off → low → medium → high → off.
    /// No-op for models that don't support reasoning.
    pub(crate) fn cycle_reasoning_focused(&mut self) -> Result<()> {
        let selected = self.state_mut(self.model_focus).selected().unwrap_or(0);
        let Some(id) = self.id_at(self.model_focus, selected) else {
            return Ok(());
        };
        if !self.model_supports_reasoning(&id) {
            self.status = format!("{id} has no reasoning setting");
            return Ok(());
        }
        let next = match self.reasoning.get(&id).map(String::as_str) {
            None => Some("low"),
            Some("low") => Some("medium"),
            Some("medium") => Some("high"),
            _ => None,
        };
        self.db.set_reasoning(&id, next)?;
        match next {
            Some(e) => {
                self.reasoning.insert(id.clone(), e.to_string());
                self.status = format!("reasoning {e}: {id}");
            }
            None => {
                self.reasoning.remove(&id);
                self.status = format!("reasoning off: {id}");
            }
        }
        Ok(())
    }

    /// Move the focused panel's selection by `delta` (clamped).
    pub(crate) fn move_model_selection(&mut self, delta: i32) {
        let len = self.panel_len(self.model_focus);
        if len == 0 {
            return;
        }
        let state = self.state_mut(self.model_focus);
        let cur = state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
        state.select(Some(next));
    }

    pub(crate) fn toggle_model_focus(&mut self) {
        self.model_focus = match self.model_focus {
            ModelPanel::Favorites => ModelPanel::Available,
            ModelPanel::Available => ModelPanel::Favorites,
        };
    }

    /// Toggle favorite on the focused selection (Ctrl+S). The item then moves
    /// between panels, so selections are re-clamped.
    pub(crate) fn toggle_favorite_focused(&mut self) -> Result<()> {
        let selected = self.state_mut(self.model_focus).selected().unwrap_or(0);
        let Some(id) = self.id_at(self.model_focus, selected) else {
            return Ok(());
        };
        let now_fav = self.db.toggle_favorite(&id)?;
        if now_fav {
            self.favorites.insert(id.clone());
            self.status = format!("★ favorited {id}");
        } else {
            self.favorites.remove(&id);
            self.status = format!("unfavorited {id}");
        }
        self.clamp_selection(ModelPanel::Favorites);
        self.clamp_selection(ModelPanel::Available);
        Ok(())
    }

    fn clamp_selection(&mut self, panel: ModelPanel) {
        let len = self.panel_len(panel);
        let state = self.state_mut(panel);
        if len == 0 {
            state.select(None);
        } else {
            let cur = state.selected().unwrap_or(0).min(len - 1);
            state.select(Some(cur));
        }
    }

    /// Confirm the focused selection as the active model.
    pub(crate) fn confirm_model(&mut self) -> Result<()> {
        let selected = self.state_mut(self.model_focus).selected().unwrap_or(0);
        if let Some(id) = self.id_at(self.model_focus, selected) {
            self.pick_model(id)?;
        }
        Ok(())
    }

    /// Pick a model by clicking a specific row in a specific panel (mouse).
    /// A click past the end of the list just moves focus there.
    pub(crate) fn pick_model_at(&mut self, panel: ModelPanel, index: usize) -> Result<()> {
        self.model_focus = panel;
        if index >= self.panel_len(panel) {
            return Ok(());
        }
        self.state_mut(panel).select(Some(index));
        if let Some(id) = self.id_at(panel, index) {
            self.pick_model(id)?;
        }
        Ok(())
    }

    fn pick_model(&mut self, id: String) -> Result<()> {
        match self.model_pick_target {
            ModelPickTarget::Session => {
                self.current_model = Some(id.clone());
                if let Some(session) = &self.session {
                    self.db.set_session_model(&session.id, &id)?;
                }
                self.db.mark_model_used(&id)?;
                self.last_used.insert(id.clone(), Utc::now().to_rfc3339());
                self.status = format!("model: {id}");
                self.popup = Popup::None;
            }
            ModelPickTarget::Memory => {
                self.memory_model = id.clone();
                self.db.set_setting("memory_model", &id)?;
                self.status = format!("memory model: {id}");
                // Picked from inside /config — return there rather than closing.
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Transcriber => {
                self.transcriber_model = id.clone();
                self.db.set_setting("transcriber_model", &id)?;
                self.status = format!("image model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Ocr => {
                self.ocr_model = id.clone();
                self.db.set_setting("ocr_model", &id)?;
                self.status = format!("OCR model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Research => {
                self.research_model = id.clone();
                self.db.set_setting("research_model", &id)?;
                self.status = format!("research model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Escalation => {
                self.escalation_model = id.clone();
                self.db.set_setting("escalation_model", &id)?;
                self.status = format!("escalation model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::ImageGen => {
                self.image_gen_model = id.clone();
                self.db.set_setting("image_gen_model", &id)?;
                self.status = format!("image gen model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::VideoGen => {
                self.video_gen_model = id.clone();
                self.db.set_setting("video_gen_model", &id)?;
                self.status = format!("video gen model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::SwarmPersona(row) => {
                if let Some(p) = self.swarm_cache.get_mut(row) {
                    p.model = id.clone();
                }
                if let Some(session) = &self.session {
                    let _ = self.db.save_swarm_personas(&session.id, &self.swarm_cache);
                }
                self.status = format!("persona model: {id}");
                self.popup = Popup::Swarm;
            }
        }
        Ok(())
    }

    /// Disable memory extraction entirely (Backspace on the memory-model row
    /// in `/config`).
    pub(crate) fn clear_memory_model(&mut self) -> Result<()> {
        self.memory_model.clear();
        self.db.set_setting("memory_model", "")?;
        self.status = "memory model cleared — extraction disabled".to_string();
        Ok(())
    }

    /// Disable image transcription entirely (Backspace on the
    /// transcriber-model row in `/config`).
    pub(crate) fn clear_transcriber_model(&mut self) -> Result<()> {
        self.transcriber_model.clear();
        self.db.set_setting("transcriber_model", "")?;
        self.status = "image model cleared — image descriptions disabled".to_string();
        Ok(())
    }

    /// Disable VLM OCR (Backspace on the OCR-model row in `/config`).
    pub(crate) fn clear_ocr_model(&mut self) -> Result<()> {
        self.ocr_model.clear();
        self.db.set_setting("ocr_model", "")?;
        self.status = "OCR model cleared — scanned PDFs use tesseract".to_string();
        Ok(())
    }

    /// Disable image generation (Backspace on the image gen model row in `/config`).
    pub(crate) fn clear_image_gen_model(&mut self) -> Result<()> {
        self.image_gen_model.clear();
        self.db.set_setting("image_gen_model", "")?;
        self.status = "image gen model cleared — generation disabled".to_string();
        Ok(())
    }

    /// Disable video generation (Backspace on the video gen model row in `/config`).
    pub(crate) fn clear_video_gen_model(&mut self) -> Result<()> {
        self.video_gen_model.clear();
        self.db.set_setting("video_gen_model", "")?;
        self.status = "video gen model cleared — generation disabled".to_string();
        Ok(())
    }

    /// Reset the research model to blank (Backspace on its row in
    /// `/config`) — disables `/research`.
    pub(crate) fn clear_research_model(&mut self) -> Result<()> {
        self.research_model.clear();
        self.db.set_setting("research_model", "")?;
        self.status = "research model cleared — /research disabled".to_string();
        Ok(())
    }

    /// Reset the escalation model to blank (Backspace on its row in
    /// `/config`) — /research falls back to the research model for its
    /// contradiction-resolution stage.
    pub(crate) fn clear_escalation_model(&mut self) -> Result<()> {
        self.escalation_model.clear();
        self.db.set_setting("escalation_model", "")?;
        self.status = "escalation model cleared — falls back to research model".to_string();
        Ok(())
    }
}
