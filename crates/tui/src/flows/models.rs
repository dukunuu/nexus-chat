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

use nexus_core::app::{KeyTarget, ModelPanel, ModelPickTarget, Popup};
use nexus_core::config;
use nexus_core::provider::{BackendTag, Model, openrouter::OpenRouter};

use crate::app_view::AppView;

impl AppView {
    /// Label for the model picker's current backend filter.
    pub fn model_backend_filter_label(&self) -> &'static str {
        self.model_backend_filter
            .map_or("all backends", BackendTag::display_name)
    }

    /// Cycle the model picker's backend filter among whichever backends are
    /// actually configured (`None` = show everything).
    pub fn cycle_model_backend_filter(&mut self) {
        let tags = self.core.backends.configured_tags();
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
        self.core.model_pick_target = ModelPickTarget::Session;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the memory model
    /// (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_memory(&mut self) {
        self.core.model_pick_target = ModelPickTarget::Memory;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the transcriber
    /// model (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_transcriber(&mut self) {
        self.core.model_pick_target = ModelPickTarget::Transcriber;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the image
    /// generation model (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_image_gen(&mut self) {
        self.core.model_pick_target = ModelPickTarget::ImageGen;
        self.open_model_picker_impl();
    }

    pub fn open_model_picker_for_video_gen(&mut self) {
        self.core.model_pick_target = ModelPickTarget::VideoGen;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the OCR model
    /// (in `/config`) instead of the active session's model.
    pub fn open_model_picker_for_ocr(&mut self) {
        self.core.model_pick_target = ModelPickTarget::Ocr;
        self.open_model_picker_impl();
    }

    /// Open the same model picker, but a confirmed pick sets the model for
    /// one row of the active session's `/swarm` roster.
    pub fn open_model_picker_for_swarm_persona(&mut self, row: usize) {
        self.core.model_pick_target = ModelPickTarget::SwarmPersona(row);
        self.open_model_picker_impl();
    }

    fn open_model_picker_impl(&mut self) {
        if !self.core.backends.any() {
            self.open_login_popup();
            return;
        }
        if self.core.models.is_empty() {
            self.push_status("loading models…".to_string());
            self.core.fetch_models();
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
    pub fn open_login_popup(&mut self) {
        self.login_selected = 0;
        self.popup = Popup::Login;
    }

    pub fn move_login_selection(&mut self, delta: i32) {
        self.login_selected = nexus_core::app::clamp_cursor(self.login_selected, 4, delta);
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
            _ => self.core.start_codex_login(),
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
        self.push_status(format!("paste your {label} API key, then Enter"));
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
            self.push_status(format!("could not save key: {e}"));
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
            KeyTarget::OpenRouter => self.core.saved.openrouter_key = Some(key),
            KeyTarget::OpenAi => self.core.saved.openai_key = Some(key),
            KeyTarget::OpencodeGo => self.core.saved.opencode_key = Some(key),
        }
        self.core.backends.set(backend_tag, provider);
        self.popup = Popup::None;
        self.push_status("key saved, loading models…".to_string());
        self.core.fetch_models();
    }

    /// Save the key pasted into `Popup::Key`, for whichever backend
    /// `self.key_target` says it's for.
    pub fn confirm_key(&mut self) {
        let key = std::mem::take(&mut self.key_input);
        let key = key.trim().to_string();
        if key.is_empty() {
            self.push_status("no key entered".to_string());
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
            .core
            .models
            .iter()
            .filter(|m| self.model_backend_filter.is_none_or(|t| m.backend == t))
            .filter(|m| {
                if self.core.model_pick_target == ModelPickTarget::ImageGen {
                    m.supports_image_generation
                } else {
                    true
                }
            })
            .filter(|m| {
                if self.core.model_pick_target == ModelPickTarget::VideoGen {
                    m.supports_video_generation
                } else {
                    true
                }
            })
            .filter(|m| {
                self.core
                    .favorites
                    .contains(&nexus_core::app::composite_id(m))
                    == want_fav
            })
            .filter(|m| {
                f.is_empty()
                    || m.id.to_lowercase().contains(&f)
                    || m.name.to_lowercase().contains(&f)
            })
            .collect();
        // Most-recently-used first, then alphabetical.
        out.sort_by(|a, b| {
            let (ca, cb) = (
                nexus_core::app::composite_id(a),
                nexus_core::app::composite_id(b),
            );
            let ra = self.core.last_used.get(&ca).cloned().unwrap_or_default();
            let rb = self.core.last_used.get(&cb).cloned().unwrap_or_default();
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
        list.get(index).map(|m| nexus_core::app::composite_id(m))
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
            .core
            .models
            .iter()
            .find(|m| nexus_core::app::composite_id(m) == id)
            .map(|m| m.reasoning_efforts.clone())
            .unwrap_or_default();
        let enabled: Vec<_> = efforts
            .iter()
            .copied()
            .filter(|effort| *effort != nexus_core::provider::ReasoningEffort::None)
            .collect();
        if enabled.is_empty() {
            if self.core.reasoning.contains_key(&id) {
                self.core.db.set_reasoning(&id, None)?;
                self.core.reasoning.remove(&id);
                self.push_status(format!("cleared stale reasoning setting: {id}"));
            } else {
                self.push_status(format!("{id} has no reasoning setting"));
            }
            return Ok(());
        }

        // A missing, explicit-none, or stale stored value starts at the first
        // enabled tier. The final tier wraps to explicit `none` when accepted,
        // otherwise it removes the parameter as before.
        let stored = self.core.reasoning.get(&id).map(String::as_str);
        let pos = stored.and_then(|s| enabled.iter().position(|e| e.as_str() == s));
        let next = match pos {
            Some(i) if i + 1 < enabled.len() => Some(enabled[i + 1]),
            Some(_) if efforts.contains(&nexus_core::provider::ReasoningEffort::None) => {
                Some(nexus_core::provider::ReasoningEffort::None)
            }
            Some(_) => None,
            None => Some(enabled[0]),
        };
        let next = next.map(nexus_core::provider::ReasoningEffort::as_str);
        let accepted = efforts
            .iter()
            .map(|effort| {
                if *effort == nexus_core::provider::ReasoningEffort::None {
                    "off"
                } else {
                    effort.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        self.core.db.set_reasoning(&id, next)?;
        match next {
            Some("none") => {
                self.core.reasoning.insert(id.clone(), "none".to_string());
                self.push_status(format!("reasoning off (accepts {accepted}): {id}"));
            }
            Some(effort) => {
                self.core.reasoning.insert(id.clone(), effort.to_string());
                self.push_status(format!("reasoning {effort} (accepts {accepted}): {id}"));
            }
            None => {
                self.core.reasoning.remove(&id);
                self.push_status(format!("reasoning off (accepts {accepted}): {id}"));
            }
        }
        Ok(())
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
            .core
            .models
            .iter()
            .find(|m| nexus_core::app::composite_id(m) == id)?
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
                    if *effort == nexus_core::provider::ReasoningEffort::None {
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
        let now_fav = self.core.db.toggle_favorite(&id)?;
        if now_fav {
            self.core.favorites.insert(id.clone());
            self.push_status(format!("★ favorited {id}"));
        } else {
            self.core.favorites.remove(&id);
            self.push_status(format!("unfavorited {id}"));
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

    /// Confirm the focused selection as the active model, then route back to
    /// whichever popup the pick was opened from.
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

    /// The view half of `App::pick_model`: run the domain pick, then route
    /// the popup back to where the pick was opened from.
    pub fn pick_model(&mut self, id: &str) -> Result<()> {
        let target = self.core.model_pick_target;
        self.core.pick_model(id)?;
        self.popup = nexus_core::app::App::popup_after_pick(target);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::app::App;
    use nexus_core::db::Db;
    use nexus_core::provider::{BackendTag, Model};
    use nexus_core::space::Space;

    fn test_space() -> Space {
        Space {
            root: std::env::temp_dir().join(format!("nexus-flow-test-{}", uuid::Uuid::new_v4())),
        }
    }

    fn test_app() -> AppView {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("sk-or-test-key"), test_space()));
        a.core.models = vec![
            Model {
                id: "a/one".into(),
                name: "One".into(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
            Model {
                id: "b/two".into(),
                name: "Two".into(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
        ];
        a
    }

    #[test]
    fn reasoning_cycle_uses_explicit_none_and_clears_stale_preferences() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.core.models = vec![
            Model {
                id: "xhigh-model".into(),
                name: "X".into(),
                reasoning_efforts: nexus_core::provider::ReasoningEffort::WITH_XHIGH_AND_NONE
                    .to_vec(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
            Model {
                id: "no-reasoning".into(),
                name: "N".into(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
        ];
        a.model_focus = ModelPanel::Available;
        let xhigh_index = a
            .available_models()
            .iter()
            .position(|model| model.id == "xhigh-model")
            .unwrap();
        a.avail_selected = xhigh_index;
        for expected in ["low", "medium", "high", "xhigh", "none", "low"] {
            a.cycle_reasoning_focused().unwrap();
            assert_eq!(a.reasoning_of("xhigh-model"), Some(expected));
        }

        a.reasoning
            .insert("no-reasoning".to_string(), "low".to_string());
        a.db.set_reasoning("no-reasoning", Some("low")).unwrap();
        let no_reasoning_index = a
            .available_models()
            .iter()
            .position(|model| model.id == "no-reasoning")
            .unwrap();
        a.avail_selected = no_reasoning_index;
        // Invalid stored values are hidden immediately and Ctrl+T removes the
        // stale database preference rather than leaving an un-clearable badge.
        assert_eq!(a.reasoning_of("no-reasoning"), None);
        a.cycle_reasoning_focused().unwrap();
        assert!(!a.reasoning.contains_key("no-reasoning"));
        assert!(
            a.db.load_model_prefs()
                .unwrap()
                .iter()
                .find(|pref| pref.id == "no-reasoning")
                .is_some_and(|pref| pref.reasoning.is_none())
        );
    }

    #[test]
    fn reasoning_cycles_models_own_effort_list() {
        // A Claude-like model accepts an extra `minimal` tier before `low`;
        // the cycle must walk exactly the model's own list, not a global one.
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.core.models = vec![Model {
            id: "anthropic/claude-sonnet-4.5".into(),
            name: "Sonnet".into(),
            reasoning_efforts: nexus_core::provider::ReasoningEffort::WITH_MINIMAL.to_vec(),
            context_length: Some(1000),
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        }];
        a.model_focus = ModelPanel::Available;
        a.avail_selected = 0;
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(
            a.reasoning_of("anthropic/claude-sonnet-4.5"),
            Some("minimal")
        );
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(a.reasoning_of("anthropic/claude-sonnet-4.5"), Some("low"));
        a.cycle_reasoning_focused().unwrap();
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(a.reasoning_of("anthropic/claude-sonnet-4.5"), Some("high"));
        a.cycle_reasoning_focused().unwrap(); // high -> off
        assert_eq!(a.reasoning_of("anthropic/claude-sonnet-4.5"), None);
    }

    #[test]
    fn reasoning_cycles_only_for_supporting_models() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.core.models = vec![Model {
            id: "r/model".into(),
            name: "R".into(),
            reasoning_efforts: nexus_core::provider::ReasoningEffort::STANDARD.to_vec(),
            context_length: Some(1000),
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        }];
        a.model_focus = ModelPanel::Available;
        a.avail_selected = 0;
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(a.reasoning_of("r/model"), Some("low"));
        a.cycle_reasoning_focused().unwrap();
        a.cycle_reasoning_focused().unwrap();
        assert_eq!(a.reasoning_of("r/model"), Some("high"));
        a.cycle_reasoning_focused().unwrap(); // high -> off
        assert_eq!(a.reasoning_of("r/model"), None);
        // persisted
        assert!(
            a.db.load_model_prefs()
                .unwrap()
                .iter()
                .any(|p| p.id == "r/model")
        );
    }

    #[test]
    fn focused_reasoning_hint_lists_accepted_values() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.core.models = vec![Model {
            id: "claude".into(),
            name: "C".into(),
            reasoning_efforts: nexus_core::provider::ReasoningEffort::WITH_MINIMAL.to_vec(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        }];
        a.model_focus = ModelPanel::Available;
        a.avail_selected = 0;
        assert_eq!(
            a.focused_reasoning_hint().as_deref(),
            Some("accepts minimal/low/medium/high")
        );
        // A model without reasoning gets no hint.
        a.core.models[0].reasoning_efforts.clear();
        assert_eq!(a.focused_reasoning_hint(), None);
    }

    #[test]
    fn toggle_favorite_persists_and_moves_panel() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.core.models = vec![Model {
            id: "a/one".into(),
            name: "One".into(),
            reasoning_efforts: Vec::new(),
            context_length: None,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend: BackendTag::OpenRouter,
            pricing: None,
        }];
        a.model_focus = ModelPanel::Available;
        a.avail_selected = 0;
        a.toggle_favorite_focused().unwrap();
        assert!(a.favorites.contains("a/one"));
        assert_eq!(a.favorite_models().len(), 1);
        assert_eq!(a.available_models().len(), 0);

        a.model_focus = ModelPanel::Favorites;
        a.fav_selected = 0;
        a.toggle_favorite_focused().unwrap();
        assert!(!a.favorites.contains("a/one"));
    }

    #[test]
    fn picking_memory_model_sets_it_and_returns_to_settings() {
        let mut a = test_app();
        let original_model = a.current_model.clone();
        a.open_model_picker_for_memory();
        assert_eq!(a.popup, Popup::Model);
        assert!(a.model_pick_target == ModelPickTarget::Memory);

        a.pick_model_at(ModelPanel::Available, 0).unwrap();
        assert_eq!(a.memory_model, "a/one");
        assert_eq!(a.popup, Popup::Settings); // back to /config, not closed
        assert_eq!(a.current_model, original_model); // session model untouched
        assert_eq!(
            a.db.load_settings()
                .unwrap()
                .iter()
                .find(|(k, _)| k == "memory_model")
                .map(|(_, v)| v.clone()),
            Some("a/one".to_string())
        );
    }

    #[test]
    fn pick_model_at_sets_current_and_closes() {
        let mut a = test_app();
        a.popup = Popup::Model;
        a.pick_model_at(ModelPanel::Available, 0).unwrap();
        assert!(a.current_model.is_some());
        assert_eq!(a.popup, Popup::None);
    }

    #[test]
    fn filter_narrows_available() {
        let mut a = test_app();
        a.model_filter = "two".into();
        let f = a.available_models();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, "b/two");
    }

    #[test]
    fn panels_split_favorites_from_available_by_recency() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, Some("k"), test_space()));
        a.core.models = vec![
            Model {
                id: "a/one".into(),
                name: "One".into(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
            Model {
                id: "b/two".into(),
                name: "Two".into(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
            Model {
                id: "c/three".into(),
                name: "Three".into(),
                reasoning_efforts: Vec::new(),
                context_length: None,
                supports_images: false,
                supports_image_generation: false,
                supports_video_generation: false,
                backend: BackendTag::OpenRouter,
                pricing: None,
            },
        ];
        // three is favorite; two was used more recently than one.
        a.core.favorites.insert("c/three".into());
        a.last_used
            .insert("a/one".into(), "2026-01-01T00:00:00Z".into());
        a.last_used
            .insert("b/two".into(), "2026-02-01T00:00:00Z".into());

        let favs: Vec<&str> = a.favorite_models().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(favs, vec!["c/three"]);
        let avail: Vec<&str> = a.available_models().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(avail, vec!["b/two", "a/one"]); // recency first
    }

    #[test]
    fn model_picker_without_key_opens_login_popup() {
        let db = Db::open_in_memory().unwrap();
        let mut a = AppView::new(App::new(db, None, test_space()));
        a.open_model_picker();
        assert_eq!(a.popup, Popup::Login);
    }
}
