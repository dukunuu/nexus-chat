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

use super::{App, KeyTarget, LoginMsg, ModelPanel, ModelPickTarget, Popup};
use crate::config;
use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model};
use chrono::Utc;

impl App {
    /// Rebuild `self.backends` from `self.saved` — every credential on file
    /// becomes a usable backend at once. Called after `self.saved` changes
    /// wholesale (app startup); `/login` updates one slot directly instead.
    pub fn rebuild_all_backends(&mut self) {
        self.backends = super::Backends::default();
        if let Some(k) = self.saved.openrouter_key.clone() {
            self.backends
                .set(BackendTag::OpenRouter, OpenRouter::openrouter_flavor(k));
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
    pub fn resolve_model_backend(&self, id: &str) -> Option<(OpenRouter, String)> {
        self.backends.resolve(id)
    }

    /// Resolve a background utility model setting. If an old bare `OpenRouter`
    /// default would be misrouted to a non-OpenRouter backend, fall back to
    /// that backend's own utility model instead of silently sending an invalid
    /// model id (which breaks memory/title jobs after OpenAI/Codex/Go login).
    pub fn resolve_utility_model_backend(
        &self,
        configured_id: &str,
    ) -> Option<(OpenRouter, String)> {
        self.resolve_feature_model_backend(configured_id, OpenRouter::default_utility_model)
    }

    pub fn resolve_feature_model_backend(
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
    pub fn model_backend_filter_label(&self) -> &'static str {
        self.model_backend_filter
            .map_or("all backends", BackendTag::display_name)
    }

    /// Cycle the model picker's backend filter among whichever backends are
    /// actually configured (`None` = show everything).
    pub fn cycle_model_backend_filter(&mut self) {
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

    pub fn open_model_picker(&mut self) {
        self.model_pick_target = ModelPickTarget::Session;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the memory model
    /// (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_memory(&mut self) {
        self.model_pick_target = ModelPickTarget::Memory;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the transcriber
    /// model (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_transcriber(&mut self) {
        self.model_pick_target = ModelPickTarget::Transcriber;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the image
    /// generation model (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_image_gen(&mut self) {
        self.model_pick_target = ModelPickTarget::ImageGen;
        self.open_model_picker_impl();
    }

    pub fn open_model_picker_for_video_gen(&mut self) {
        self.model_pick_target = ModelPickTarget::VideoGen;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the OCR model
    /// (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_ocr(&mut self) {
        self.model_pick_target = ModelPickTarget::Ocr;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the model for
    /// one row of the active session's `/swarm` roster.
    pub fn open_model_picker_for_swarm_persona(&mut self, row: usize) {
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
    pub fn reset_model_selection(&mut self) {
        self.fav_selected = 0;
        self.avail_selected = 0;
    }

    /// `/login`: open the provider selector. Also what `open_model_picker`
    /// falls back to when nothing's configured yet.
    pub const fn open_login_popup(&mut self) {
        self.login_selected = 0;
        self.popup = Popup::Login;
    }

    pub fn move_login_selection(&mut self, delta: i32) {
        self.login_selected = super::clamp_cursor(self.login_selected, 4, delta);
    }

    /// Enter on a `/login` row: OpenRouter/OpenCode Go/OpenAI activate
    /// immediately from their env var if set, else drop into the key-paste
    /// popup; Codex has no key, so it starts the device-code login instead.
    pub fn confirm_login_selection(&mut self) {
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
    pub const fn key_target_label(&self) -> &'static str {
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
            KeyTarget::OpenRouter => (
                BackendTag::OpenRouter,
                OpenRouter::openrouter_flavor(key.clone()),
            ),
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

    pub fn start_codex_login(&mut self) {
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

    pub fn on_login_result(&mut self, msg: Option<LoginMsg>) {
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
    pub fn confirm_key(&mut self) {
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
    pub fn favorite_models(&self) -> Vec<&Model> {
        self.filtered_panel(true)
    }

    /// Non-favorite models matching the search filter, most-recently-used first.
    pub fn available_models(&self) -> Vec<&Model> {
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

    const fn state_mut(&mut self, panel: ModelPanel) -> &mut usize {
        match panel {
            ModelPanel::Favorites => &mut self.fav_selected,
            ModelPanel::Available => &mut self.avail_selected,
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
    pub fn context_limit(&self) -> Option<u64> {
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
    pub fn context_used(&self) -> u64 {
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
                .map_or(0, |md| crate::skills::skill_body(&md).chars().count());
        }
        chars += self
            .effective_messages()
            .iter()
            // The digest transcript row duplicates `compact_summary` (counted
            // above) — never double-count it.
            .filter(|m| m.role != "compaction")
            .map(|m| m.content.chars().count())
            .sum::<usize>();
        if let Some(buf) = self.active_streaming_text() {
            chars += buf.chars().count();
        }
        (chars / 4) as u64
    }

    /// Whether the active model accepts image input (unknown model → false).
    pub fn current_model_supports_images(&self) -> bool {
        self.current_model.as_deref().is_some_and(|id| {
            self.models
                .iter()
                .any(|m| super::composite_id(m) == id && m.supports_images)
        })
    }

    pub fn reasoning_of(&self, id: &str) -> Option<&str> {
        let effort = self.reasoning.get(id)?.as_str();
        self.effort_accepted(id, effort).then_some(effort)
    }

    /// Cycle the focused model through exactly its catalog-provided enabled
    /// effort values. A model that accepts the explicit `none` wire value
    /// uses that when wrapping from its highest tier, so "off" still disables
    /// models whose provider enables reasoning by default.
    pub fn cycle_reasoning_focused(&mut self) -> Result<()> {
        let selected = *self.state_mut(self.model_focus);
        let Some(id) = self.id_at(self.model_focus, selected) else {
            return Ok(());
        };
        let efforts = self
            .models
            .iter()
            .find(|m| super::composite_id(m) == id)
            .map(|m| m.reasoning_efforts.clone())
            .unwrap_or_default();
        let enabled: Vec<_> = efforts
            .iter()
            .copied()
            .filter(|effort| *effort != crate::provider::ReasoningEffort::None)
            .collect();
        if enabled.is_empty() {
            if self.reasoning.contains_key(&id) {
                self.db.set_reasoning(&id, None)?;
                self.reasoning.remove(&id);
                self.status = format!("cleared stale reasoning setting: {id}");
            } else {
                self.status = format!("{id} has no reasoning setting");
            }
            return Ok(());
        }

        // A missing, explicit-none, or stale stored value starts at the first
        // enabled tier. The final tier wraps to explicit `none` when accepted,
        // otherwise it removes the parameter as before.
        let stored = self.reasoning.get(&id).map(String::as_str);
        let pos = stored.and_then(|s| enabled.iter().position(|e| e.as_str() == s));
        let next = match pos {
            Some(i) if i + 1 < enabled.len() => Some(enabled[i + 1]),
            Some(_) if efforts.contains(&crate::provider::ReasoningEffort::None) => {
                Some(crate::provider::ReasoningEffort::None)
            }
            Some(_) => None,
            None => Some(enabled[0]),
        };
        let next = next.map(super::super::provider::ReasoningEffort::as_str);
        let accepted = efforts
            .iter()
            .map(|effort| {
                if *effort == crate::provider::ReasoningEffort::None {
                    "off"
                } else {
                    effort.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        self.db.set_reasoning(&id, next)?;
        match next {
            Some("none") => {
                self.reasoning.insert(id.clone(), "none".to_string());
                self.status = format!("reasoning off (accepts {accepted}): {id}");
            }
            Some(effort) => {
                self.reasoning.insert(id.clone(), effort.to_string());
                self.status = format!("reasoning {effort} (accepts {accepted}): {id}");
            }
            None => {
                self.reasoning.remove(&id);
                self.status = format!("reasoning off (accepts {accepted}): {id}");
            }
        }
        Ok(())
    }

    /// Whether `effort` is in `model`'s accepted reasoning set, so a stored
    /// value is only sent when the model actually accepts it. Unknown models
    /// (not in the loaded catalog) accept anything — never silently drop a
    /// stored value just because the catalog isn't fetched yet.
    pub fn effort_accepted(&self, model: &str, effort: &str) -> bool {
        self.models
            .iter()
            .find(|m| super::composite_id(m) == model)
            .is_none_or(|m| m.reasoning_efforts.iter().any(|e| e.as_str() == effort))
    }

    /// The focused model's exact accepted effort values for the picker hint.
    /// None when the focused row has no reasoning mode.
    pub fn focused_reasoning_hint(&self) -> Option<String> {
        let selected = match self.model_focus {
            ModelPanel::Favorites => self.fav_selected,
            ModelPanel::Available => self.avail_selected,
        };
        let id = self.id_at(self.model_focus, selected)?;
        let efforts = self
            .models
            .iter()
            .find(|m| super::composite_id(m) == id)?
            .reasoning_efforts
            .as_slice();
        if efforts.is_empty() {
            return None;
        }
        Some(format!(
            "accepts {}",
            efforts
                .iter()
                .map(|effort| {
                    if *effort == crate::provider::ReasoningEffort::None {
                        "off"
                    } else {
                        effort.as_str()
                    }
                })
                .collect::<Vec<_>>()
                .join("/")
        ))
    }

    /// Move the focused panel's selection by `delta` (clamped).
    pub fn move_model_selection(&mut self, delta: i32) {
        let len = self.panel_len(self.model_focus);
        if len == 0 {
            return;
        }
        let state = self.state_mut(self.model_focus);
        let cur = *state;
        let next = (cur as i32 + delta).clamp(0, len as i32 - 1) as usize;
        *state = next;
    }

    pub const fn toggle_model_focus(&mut self) {
        self.model_focus = match self.model_focus {
            ModelPanel::Favorites => ModelPanel::Available,
            ModelPanel::Available => ModelPanel::Favorites,
        };
    }

    /// Toggle favorite on the focused selection (Ctrl+S). The item then moves
    /// between panels, so selections are re-clamped.
    pub fn toggle_favorite_focused(&mut self) -> Result<()> {
        let selected = *self.state_mut(self.model_focus);
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
            *state = 0;
        } else {
            *state = (*state).min(len - 1);
        }
    }

    /// Confirm the focused selection as the active model.
    pub fn confirm_model(&mut self) -> Result<()> {
        let selected = *self.state_mut(self.model_focus);
        if let Some(id) = self.id_at(self.model_focus, selected) {
            self.pick_model(&id)?;
        }
        Ok(())
    }

    /// Pick a model by clicking a specific row in a specific panel (mouse).
    /// A click past the end of the list just moves focus there.
    pub fn pick_model_at(&mut self, panel: ModelPanel, index: usize) -> Result<()> {
        self.model_focus = panel;
        if index >= self.panel_len(panel) {
            return Ok(());
        }
        *self.state_mut(panel) = index;
        if let Some(id) = self.id_at(panel, index) {
            self.pick_model(&id)?;
        }
        Ok(())
    }

    pub fn pick_model(&mut self, id: &str) -> Result<()> {
        match self.model_pick_target {
            ModelPickTarget::Session => {
                self.current_model = Some(id.to_string());
                if let Some(session) = &self.session {
                    self.db.set_session_model(&session.id, id)?;
                }
                self.db.mark_model_used(id)?;
                self.last_used
                    .insert(id.to_string(), Utc::now().to_rfc3339());
                self.status = format!("model: {id}");
                self.popup = Popup::None;
            }
            ModelPickTarget::Memory => {
                self.memory_model = id.to_string();
                self.db.set_setting("memory_model", id)?;
                self.status = format!("memory model: {id}");
                // Picked from inside /config — return there rather than closing.
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Transcriber => {
                self.transcriber_model = id.to_string();
                self.db.set_setting("transcriber_model", id)?;
                self.status = format!("image model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::Ocr => {
                self.ocr_model = id.to_string();
                self.db.set_setting("ocr_model", id)?;
                self.status = format!("OCR model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::ImageGen => {
                self.image_gen_model = id.to_string();
                self.db.set_setting("image_gen_model", id)?;
                self.status = format!("image gen model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::VideoGen => {
                self.video_gen_model = id.to_string();
                self.db.set_setting("video_gen_model", id)?;
                self.status = format!("video gen model: {id}");
                self.popup = Popup::Settings;
            }
            ModelPickTarget::SwarmPersona(row) => {
                if let Some(p) = self.swarm_cache.get_mut(row) {
                    p.model = id.to_string();
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
    pub fn clear_memory_model(&mut self) -> Result<()> {
        self.memory_model.clear();
        self.db.set_setting("memory_model", "")?;
        self.status = "memory model cleared — extraction disabled".to_string();
        Ok(())
    }

    /// Disable image transcription entirely (Backspace on the
    /// transcriber-model row in `/config`).
    pub fn clear_transcriber_model(&mut self) -> Result<()> {
        self.transcriber_model.clear();
        self.db.set_setting("transcriber_model", "")?;
        self.status = "image model cleared — image descriptions disabled".to_string();
        Ok(())
    }

    /// Disable VLM OCR (Backspace on the OCR-model row in `/config`).
    pub fn clear_ocr_model(&mut self) -> Result<()> {
        self.ocr_model.clear();
        self.db.set_setting("ocr_model", "")?;
        self.status = "OCR model cleared — scanned PDFs use tesseract".to_string();
        Ok(())
    }

    /// Disable image generation (Backspace on the image gen model row in `/config`).
    pub fn clear_image_gen_model(&mut self) -> Result<()> {
        self.image_gen_model.clear();
        self.db.set_setting("image_gen_model", "")?;
        self.status = "image gen model cleared — generation disabled".to_string();
        Ok(())
    }

    /// Disable video generation (Backspace on the video gen model row in `/config`).
    pub fn clear_video_gen_model(&mut self) -> Result<()> {
        self.video_gen_model.clear();
        self.db.set_setting("video_gen_model", "")?;
        self.status = "video gen model cleared — generation disabled".to_string();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_space() -> crate::space::Space {
        crate::space::Space {
            root: std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn model(id: &str, backend: BackendTag) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend,
            pricing: None,
        }
    }

    fn app_with_two_backends() -> App {
        let mut a = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            Some("sk-or-test-key"),
            test_space(),
        );
        a.backends
            .set(BackendTag::OpenAi, OpenRouter::openai("oa".into()));
        a.backends.set(
            BackendTag::OpenRouter,
            OpenRouter::openrouter_flavor("or".into()),
        );
        a
    }

    #[test]
    fn resolve_model_backend_picks_the_backend_owning_the_model() {
        let mut a = app_with_two_backends();
        a.models = vec![
            model("gpt-4.1-mini", BackendTag::OpenAi),
            model("anthropic/claude-sonnet-4.5", BackendTag::OpenRouter),
        ];
        // OpenAI models are addressed by their composite `openai:` prefix; a
        // bare id prefers OpenRouter for backwards compatibility.
        let (p, raw) = a.resolve_model_backend("openai:gpt-4.1-mini").unwrap();
        assert_eq!(p.backend_tag(), BackendTag::OpenAi);
        assert_eq!(raw, "gpt-4.1-mini");
        let (p, raw) = a
            .resolve_model_backend("anthropic/claude-sonnet-4.5")
            .unwrap();
        assert_eq!(p.backend_tag(), BackendTag::OpenRouter);
        assert_eq!(raw, "anthropic/claude-sonnet-4.5");
    }

    #[test]
    fn resolve_model_backend_strips_the_backend_prefix() {
        let mut a = app_with_two_backends();
        a.models = vec![model("gpt-4.1-mini", BackendTag::OpenAi)];
        let (p, raw) = a.resolve_model_backend("openai:gpt-4.1-mini").unwrap();
        assert_eq!(p.backend_tag(), BackendTag::OpenAi);
        assert_eq!(raw, "gpt-4.1-mini");
    }

    #[test]
    fn resolved_model_looks_valid_restricts_slashy_bare_ids_to_openrouter() {
        let a = App::new(
            crate::db::Db::open_in_memory().unwrap(),
            Some("k"),
            test_space(),
        );
        // Empty catalog: a slashy bare id is only valid on OpenRouter (a
        // legacy OpenRouter id must not silently resolve against OpenAI).
        assert!(a.resolved_model_looks_valid(
            "google/gemini-2.5-flash",
            BackendTag::OpenRouter,
            "google/gemini-2.5-flash"
        ));
        assert!(!a.resolved_model_looks_valid(
            "google/gemini-2.5-flash",
            BackendTag::OpenAi,
            "google/gemini-2.5-flash"
        ));
        // Non-slashy bare ids are fine on any backend.
        assert!(a.resolved_model_looks_valid("gpt-4.1-mini", BackendTag::OpenAi, "gpt-4.1-mini"));
    }

    #[test]
    fn backend_filter_cycles_only_through_configured_backends() {
        let mut a = app_with_two_backends();
        assert_eq!(a.model_backend_filter_label(), "all backends");
        a.cycle_model_backend_filter();
        let first = a.model_backend_filter;
        assert!(first.is_some());
        assert!(!a.model_backend_filter_label().contains("all"));
        a.cycle_model_backend_filter();
        a.cycle_model_backend_filter();
        a.cycle_model_backend_filter();
        assert_eq!(a.model_backend_filter, first); // wraps, never leaves the configured set
    }

    #[test]
    fn favorites_sort_first_in_available_models() {
        let mut a = app_with_two_backends();
        a.models = vec![
            model("a/one", BackendTag::OpenRouter),
            model("b/two", BackendTag::OpenRouter),
            model("c/three", BackendTag::OpenRouter),
        ];
        a.db.toggle_favorite("b/two").unwrap();
        a.load_prefs();
        a.last_used
            .insert("a/one".into(), "2026-01-01T00:00:00Z".into());
        a.last_used
            .insert("c/three".into(), "2026-01-02T00:00:00Z".into());
        let favs: Vec<&str> = a.favorite_models().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(favs, vec!["b/two"]);
        // Available panel = non-favorites, most-recently-used first.
        let avail: Vec<&str> = a.available_models().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(avail, vec!["c/three", "a/one"]);
    }
}
