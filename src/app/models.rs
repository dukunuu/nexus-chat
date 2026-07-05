use anyhow::Result;
use ratatui::widgets::ListState;

use super::{App, ModelPanel, ModelPickTarget, Popup};
use crate::config;
use crate::provider::Model;
use crate::provider::openrouter::OpenRouter;
use chrono::Utc;

impl App {
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

    fn open_model_picker_impl(&mut self) {
        if self.provider.is_none() {
            self.open_key_prompt();
            return;
        }
        if self.models.is_empty() {
            self.status = "loading models…".to_string();
            self.fetch_models();
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

    pub(super) fn open_key_prompt(&mut self) {
        self.key_input.clear();
        self.status = "paste your OpenRouter key, then Enter".to_string();
        self.popup = Popup::Key;
    }

    /// Save the entered key: persist it, build the provider, fetch models.
    pub(crate) fn confirm_key(&mut self) {
        let key = std::mem::take(&mut self.key_input);
        let key = key.trim().to_string();
        self.popup = Popup::None;
        if key.is_empty() {
            self.status = "no key entered".to_string();
            return;
        }
        if let Err(e) = config::save_key(&key) {
            self.status = format!("could not save key: {e}");
        }
        self.provider = Some(OpenRouter::new(key.clone()));
        self.key = Some(key);
        self.status = "key saved, loading models…".to_string();
        self.fetch_models();
    }

    /// Favorite models matching the search filter, most-recently-used first.
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
            .filter(|m| self.favorites.contains(&m.id) == want_fav)
            .filter(|m| {
                f.is_empty()
                    || m.id.to_lowercase().contains(&f)
                    || m.name.to_lowercase().contains(&f)
            })
            .collect();
        // Most-recently-used first, then alphabetical.
        out.sort_by(|a, b| {
            let ra = self.last_used.get(&a.id).cloned().unwrap_or_default();
            let rb = self.last_used.get(&b.id).cloned().unwrap_or_default();
            rb.cmp(&ra).then_with(|| a.id.cmp(&b.id))
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

    fn id_at(&self, panel: ModelPanel, index: usize) -> Option<String> {
        let list = match panel {
            ModelPanel::Favorites => self.favorite_models(),
            ModelPanel::Available => self.available_models(),
        };
        list.get(index).map(|m| m.id.clone())
    }

    /// Context window of the active model, if known.
    pub(crate) fn context_limit(&self) -> Option<u64> {
        let id = self.current_model.as_deref()?;
        self.models
            .iter()
            .find(|m| m.id == id)
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
        if let Some(s) = self.session.as_ref().and_then(|s| s.compact_summary.as_deref()) {
            chars += s.chars().count();
        }
        if let Some(name) = &self.forced_skill
            && let Some(skill) = self.skills.iter().find(|s| &s.name == name)
        {
            chars += std::fs::read_to_string(skill.dir.join("SKILL.md"))
                .map(|md| crate::skills::skill_body(&md).chars().count())
                .unwrap_or(0);
        }
        chars += self.effective_messages().iter().map(|m| m.content.chars().count()).sum::<usize>();
        if let Some(buf) = &self.streaming {
            chars += buf.chars().count();
        }
        (chars / 4) as u64
    }

    fn model_supports_reasoning(&self, id: &str) -> bool {
        self.models
            .iter()
            .any(|m| m.id == id && m.supports_reasoning)
    }

    /// Whether the active model accepts image input (unknown model → false).
    pub(crate) fn current_model_supports_images(&self) -> bool {
        self.current_model
            .as_deref()
            .is_some_and(|id| self.models.iter().any(|m| m.id == id && m.supports_images))
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
