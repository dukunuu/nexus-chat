use anyhow::{Context as _, Result};
use chrono::Utc;

use crate::provider::BackendTag;
use crate::provider::local::{LocalConfig, LocalRuntime};
use crate::provider::openrouter::OpenRouter;
use crate::provider::serve::{LaunchOptions, RuntimeStatus};

use super::{App, ModelPickTarget, Popup};

const ROUTER_MODEL_IDS: &[&str] = &["typesafe/jev-router", "openrouter/auto"];

impl App {
    /// Rebuild every backend from the on-disk saved credentials (after boot,
    /// or after a settings change). Keeps `saved` and `backends` in sync.
    pub fn rebuild_all_backends(&mut self) {
        self.experiments = self.saved.experiments.clone();
        self.backends = super::Backends::default();
        if let Some(k) = self.saved.openrouter_key.clone() {
            self.backends
                .set(BackendTag::OpenRouter, OpenRouter::openrouter_flavor(k));
        }
        if let Some(config) = self.saved.local.clone() {
            match OpenRouter::configured_local(config) {
                Ok(provider) => self.backends.set(BackendTag::Local, provider),
                Err(error) => self.push_status(format!("local inference: {error}")),
            }
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
        if self.backends.any() {
            self.push_status("loading models…  (/model to pick, /help for commands)".to_string());
        }
    }

    /// `/local` — apply a runtime spec, or report the current setting when
    /// it's empty (the TUI opens its picker for that case instead). Parse
    /// and persistence failures land on the status line: a typo'd runtime
    /// must not take the app down.
    pub fn configure_local(&mut self, spec: &str) {
        if spec.trim().is_empty() {
            let status = match &self.saved.local {
                Some(config) => format!(
                    "local inference: {} at {}",
                    config.provider.label(),
                    config.endpoint()
                ),
                None => "local inference is off — /local <ollama|mlx|mlx-serve|lmstudio|edge0>"
                    .to_string(),
            };
            self.push_status(status);
            return;
        }
        let applied = LocalConfig::from_spec(spec, self.saved.local.as_ref())
            .and_then(|config| self.set_local_runtime(config));
        if let Err(error) = applied {
            self.push_status(format!("local inference: {error}"));
        }
    }

    /// `/local start|stop|restart|status [<runtime>]` — manage the local
    /// provider's own inference server. Every verb ends in a fresh survey, so
    /// the reported state is measured rather than assumed.
    pub fn manage_local_server(&mut self, verb: &str, runtime: &str) {
        if let Err(error) = self.local_server_action(verb, runtime) {
            self.push_status(format!("local server: {error}"));
            return;
        }
        self.local_status_announce = true;
        self.refresh_local_status();
    }

    /// The verb itself. Split out so every failure path is one `?` and lands
    /// on the status line with the same prefix.
    fn local_server_action(&mut self, verb: &str, runtime: &str) -> Result<()> {
        if verb == "status" {
            return Ok(());
        }
        let runtime = if runtime.trim().is_empty() {
            self.saved
                .local
                .as_ref()
                .map(|config| config.provider)
                .context(
                    "local inference is off — /local <ollama|mlx|mlx-serve|lmstudio|edge0> first",
                )?
        } else {
            LocalRuntime::parse(runtime.trim())
                .with_context(|| format!("unknown local runtime {:?}", runtime.trim()))?
        };
        let config = LocalConfig::for_runtime(runtime, self.saved.local.as_ref());
        let port = config
            .port()
            .context("the configured endpoint has no port to serve on")?;
        if matches!(verb, "stop" | "restart") {
            // A restart waits for the port back before rebinding it.
            self.local_servers.stop(runtime, port, verb == "restart")?;
            self.push_status(format!("{} server stopped", runtime.label()));
            if verb == "stop" {
                return Ok(());
            }
        }
        // mlx-lm and edge0 serve exactly one model, so they launch with the
        // selected one; the composite id carries a `local:` prefix.
        let model = self
            .current_model
            .as_deref()
            .and_then(|id| id.strip_prefix("local:"))
            .map(str::to_owned);
        let options = LaunchOptions {
            context_length: config.effective_context(),
            kv_cache_q8: config.kv_cache_q8,
        };
        let argv = self
            .local_servers
            .start(runtime, model.as_deref(), port, options)?;
        self.push_status(format!(
            "{} starting: {argv} — /local status when it answers",
            runtime.label()
        ));
        Ok(())
    }

    /// Probe every local runtime in the background: which endpoints answer,
    /// what their process trees cost, and whether that is over budget. One
    /// survey at a time; the picker and the verbs share the result.
    pub fn refresh_local_status(&mut self) {
        if self.local_status_rx.is_some() {
            return;
        }
        self.local_servers.reap();
        let configured = self.saved.local.clone();
        let managed: Vec<(LocalRuntime, Option<u32>)> = self
            .local_servers
            .managed_runtimes()
            .into_iter()
            .map(|runtime| (runtime, self.local_servers.pid(runtime)))
            .collect();
        // A survey shells out and talks to the network, so it needs a
        // reactor. `cfg(test)` cannot express this: core is a dependency of
        // the TUI's test binary, where it compiles with tests off. Asking the
        // runtime directly is what actually holds, in every frontend and test.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.local_status_rx = Some(rx);
        handle.spawn(async move {
            let _ = tx.send(crate::provider::serve::survey(configured.as_ref(), &managed).await);
        });
    }

    /// A finished survey: cache it for the picker, report it when the user
    /// asked, and warn about anything over budget. Nothing is stopped here —
    /// the budget is advisory, so a server mid-reply is never yanked.
    pub fn on_local_status(&mut self, result: Option<Vec<RuntimeStatus>>) {
        self.local_status_rx = None;
        let Some(statuses) = result else {
            return;
        };
        let announce = std::mem::take(&mut self.local_status_announce);
        self.local_status = statuses;
        if announce {
            let line = self
                .local_status
                .iter()
                .map(RuntimeStatus::summary)
                .collect::<Vec<_>>()
                .join("  ·  ");
            self.push_status(format!("local: {line}"));
        }
        if let Some(over) = self.local_status.iter().find(|status| status.over_budget) {
            let budget = over
                .budget_kb
                .map(crate::provider::serve::format_kb)
                .unwrap_or_default();
            self.push_status(format!(
                "⚠ {} is using {} — over the {budget} budget; /local stop {} to reclaim it",
                over.runtime.label(),
                over.rss_kb
                    .map(crate::provider::serve::format_kb)
                    .unwrap_or_default(),
                over.runtime.token()
            ));
        }
    }

    /// Stop every local server Nexus started. The exit path calls this so
    /// quitting never leaves a multi-gigabyte server behind.
    pub async fn stop_local_servers(&mut self) {
        self.local_servers.stop_all().await;
    }

    /// Persist and activate (or clear) the local runtime, then refresh the
    /// model catalog. The `[local]` block is machine-local, so this only ever
    /// touches this device's config file.
    ///
    /// # Errors
    /// Invalid settings, or a config file that can't be written.
    pub fn set_local_runtime(&mut self, config: Option<LocalConfig>) -> Result<()> {
        if let Some(config) = &config {
            config.validate()?;
        }
        crate::config::save_local_config(config.as_ref())?;
        let status = match &config {
            Some(config) => format!(
                "local inference: {} at {} — loading models…",
                config.provider.label(),
                config.endpoint()
            ),
            None => "local inference disabled".to_string(),
        };
        self.saved.local = config;
        if self.saved.local.is_none() {
            // `fetch_models` can't clear these: with no provider left it
            // returns early, and a stale `local:` selection would fail to
            // route on the next send.
            self.models
                .retain(|model| model.backend != BackendTag::Local);
            if self
                .current_model
                .as_deref()
                .is_some_and(|id| id.starts_with(BackendTag::Local.key_prefix()))
            {
                self.current_model = None;
            }
        }
        self.rebuild_all_backends();
        self.fetch_models();
        self.push_status(status);
        Ok(())
    }

    pub fn resolve_model_backend(&self, id: &str) -> Option<(OpenRouter, String)> {
        self.backends.resolve(id)
    }

    /// The model every background job runs on: the picked `helper_model`, or
    /// "auto" when it is blank. Auto is the cheap model of the backend the
    /// session is on (`OpenRouter::default_helper_for`), or the session's own
    /// model there when that backend has no cheap option. It never moves to
    /// another backend on its own.
    pub fn helper_backend(&self) -> Option<(OpenRouter, String)> {
        self.resolve_utility_model_backend(&self.helper_model)
    }

    /// The default generation model of the active backend (`default` is
    /// `default_image_gen_model` or `default_video_gen_model`), or `None` when
    /// that backend has none or is a local runtime. Generation follows the
    /// session's backend and never silently reaches for another provider.
    pub fn generation_backend(
        &self,
        default: fn(&OpenRouter) -> &'static str,
    ) -> Option<(OpenRouter, String)> {
        let provider = self.active_provider()?;
        if provider.backend_tag() == BackendTag::Local {
            return None;
        }
        let model = default(&provider);
        (!model.is_empty()).then(|| (provider, model.to_string()))
    }

    /// The provider of the session's model, else the first configured backend.
    fn active_provider(&self) -> Option<OpenRouter> {
        self.current_model
            .as_deref()
            .and_then(|id| self.resolve_model_backend(id).map(|(provider, _)| provider))
            .or_else(|| {
                self.backends
                    .configured_tags()
                    .first()
                    .and_then(|tag| self.backends.get(*tag).cloned())
            })
    }

    /// Resolve a feature (non-session) model that may be a bare wire id or a
    /// composite id. Feature models default to the session provider's
    /// research-class default; a composite id picks its own backend.
    pub fn resolve_utility_model_backend(
        &self,
        configured_id: &str,
    ) -> Option<(OpenRouter, String)> {
        self.resolve_feature_model_backend(configured_id, OpenRouter::default_utility_model)
    }

    /// Resolve a feature model by name for a backend that may not be
    /// `OpenRouter` (used for image/video gen where the user may have picked a
    /// non-OpenRouter model).
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

        let provider = self.active_provider()?;
        if provider.backend_tag() == BackendTag::Local {
            // Local runtimes do not have cloud default model names. Only use
            // an installed model, preferring the current chat selection.
            let raw = self
                .current_model
                .as_deref()
                .and_then(|id| self.resolve_model_backend(id))
                .filter(|(backend, _)| backend.backend_tag() == BackendTag::Local)
                .map(|(_, raw)| raw)
                .or_else(|| {
                    self.models
                        .iter()
                        .find(|model| model.backend == BackendTag::Local)
                        .map(|model| model.id.clone())
                })?;
            return Some((provider, raw));
        }
        let tag = provider.backend_tag();
        let auto = default(&provider);
        let listed =
            self.models.is_empty() || self.models.iter().any(|m| m.backend == tag && m.id == auto);
        if auto.is_empty() || !listed {
            // No cheap option here (or the catalog no longer lists it): use
            // the session's own model on this same backend, if it has one.
            if let Some(raw) = self
                .current_model
                .as_deref()
                .and_then(|id| self.resolve_model_backend(id))
                .filter(|(backend, _)| backend.backend_tag() == tag)
                .map(|(_, raw)| raw)
            {
                return Some((provider, raw));
            }
        }
        (!auto.is_empty()).then(|| (provider, auto.to_string()))
    }

    /// Whether a resolved feature model id actually exists in the catalog
    /// (or the catalog is empty/unknown — never silently drop a feature just
    /// because the catalog isn't fetched yet).
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
            return backend != BackendTag::Local
                && (backend == BackendTag::OpenRouter || !original_id.contains('/'));
        }
        self.models
            .iter()
            .any(|m| m.backend == backend && m.id == raw)
    }

    /// Fetch every configured backend's catalog concurrently and merge them
    /// into one list. A backend that fails is dropped from the merge (its
    /// error is only surfaced if *every* backend failed) — one flaky login
    /// shouldn't blank out the models of the others. Public so the view layer
    /// can re-trigger a fetch from the model picker.
    pub fn fetch_models(&mut self) {
        let providers: Vec<OpenRouter> = [
            self.backends.openrouter.clone(),
            self.backends.openai.clone(),
            self.backends.opencode.clone(),
            self.backends.codex.clone(),
            self.backends.local.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if providers.is_empty() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.models_rx = Some(rx);
        tokio::spawn(async move {
            let mut set = tokio::task::JoinSet::new();
            for p in providers {
                set.spawn(async move { p.list_models().await });
            }
            let mut merged = Vec::new();
            let mut errors = Vec::new();
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(Ok(models)) => merged.extend(models),
                    Ok(Err(e)) => errors.push(e.to_string()),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            let result = if merged.is_empty() && !errors.is_empty() {
                Err(errors.join("; "))
            } else {
                merged.sort_by(|a, b| a.id.cmp(&b.id));
                Ok(merged)
            };
            let _ = tx.send(result);
        });
    }

    /// Context window of the active model, if known.
    pub fn context_limit(&self) -> Option<u64> {
        let id = self.current_model.as_deref()?;
        self.models
            .iter()
            .find(|m| super::composite_id(m) == id)
            .and_then(|m| m.context_length)
    }

    /// Tokens used by the current session: the provider's latest exact count
    /// plus an estimate of only what was added since it (a streaming reply,
    /// tool results). Estimating the whole conversation instead overshot on
    /// JSON-heavy tool rows and then snapped back to the exact count every
    /// turn. With no exact count yet (new session, model or prompt switch)
    /// the whole conversation is estimated, scaled by the last exact/estimate
    /// ratio so the switch-over doesn't jump.
    pub fn context_used(&self) -> u64 {
        let streaming = self.is_streaming();
        if let (Some(total), Some(basis)) = (self.context_total, self.context_basis)
            && total == basis.exact
        {
            if !streaming && self.messages.len() == basis.messages {
                return total;
            }
            let delta = self.context_chars() as f64 - basis.chars as f64;
            let tokens = total as f64 + self.calibrated(delta / 4.0);
            return tokens.max(0.0).round() as u64;
        }
        if !streaming && let Some(total) = self.context_total {
            return total;
        }
        let estimate = self.calibrated(self.context_chars() as f64 / 4.0) as u64;
        // Do not let a fresh estimate make the bar shrink at the start of a
        // request: the previous completed request is a better lower bound
        // until this one reports exact usage.
        if streaming {
            self.context_total
                .map_or(estimate, |total| total.max(estimate))
        } else {
            estimate
        }
    }

    /// Scale a ~4-chars/token estimate by what the provider last reported.
    fn calibrated(&self, tokens: f64) -> f64 {
        tokens * self.context_ratio.unwrap_or(1.0)
    }

    /// What the next request will carry, in characters: the system/memory
    /// prompt, the compaction digest (if any), a forced skill's body, the
    /// effective conversation tail, and the reply streaming now.
    pub(crate) fn context_chars(&self) -> usize {
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
        chars += self.effective_conversation_chars();
        if let Some(buf) = self.active_streaming_text() {
            chars += buf.chars().count();
        }
        chars
    }

    /// Record an exact count reported by one of `task`'s requests. Within a
    /// turn the conversation only grows, so a smaller count from the same
    /// task is a nested request (a `task` sub-agent's own prompt) and must
    /// not move the bar; the first count of a turn is always taken.
    pub(crate) fn observe_context(&mut self, task: super::ChatTaskId, total: u64) {
        if self
            .context_basis
            .is_some_and(|basis| basis.task == task && total < basis.exact)
        {
            return;
        }
        let chars = self.context_chars();
        if chars > 0 && total > 0 {
            self.context_ratio = Some((total as f64 / (chars as f64 / 4.0)).clamp(0.5, 2.0));
        }
        self.context_total = Some(total);
        self.context_basis = Some(super::ContextBasis {
            exact: total,
            chars,
            messages: self.messages.len(),
            task,
        });
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

    /// Set the active model (or a feature model, per the pick target).
    pub fn pick_model(&mut self, id: &str) -> Result<()> {
        match self.model_pick_target {
            ModelPickTarget::Session => {
                if self.current_model.as_deref() != Some(id) {
                    self.bump_cache_epoch();
                }
                let before = self.active_provider().map(|p| p.backend_tag());
                self.current_model = Some(id.to_string());
                let backend_changed = before != self.active_provider().map(|p| p.backend_tag());
                if backend_changed && !self.is_streaming() {
                    // Media generation and the helper model follow the
                    // session's backend, so the toolbox is rebuilt for it.
                    self.refresh_toolbox();
                }
                if let Some(session) = &self.session {
                    self.db.set_session_model(&session.id, id)?;
                }
                self.db.mark_model_used(id)?;
                self.last_used
                    .insert(id.to_string(), Utc::now().to_rfc3339());
                self.push_status(format!("model: {id}"));
                if !self.router_note_shown && ROUTER_MODEL_IDS.contains(&id) {
                    self.router_note_shown = true;
                    self.push_status("router models pick a model per request · prompt caching is less effective · compare with nexus eval --compare-model first");
                }
            }
            ModelPickTarget::Helper => {
                self.helper_model = id.to_string();
                self.db.set_setting("helper_model", id)?;
                self.push_status(format!("helper model: {id}"));
            }
            ModelPickTarget::SwarmPersona(row) => {
                if let Some(p) = self.swarm_cache.get_mut(row) {
                    p.model = id.to_string();
                }
                if let Some(session) = &self.session {
                    let _ = self.db.save_swarm_personas(&session.id, &self.swarm_cache);
                }
                self.push_status(format!("persona model: {id}"));
            }
        }
        if self.work_mode == super::WorkMode::AgentAuto
            && matches!(
                self.model_pick_target,
                ModelPickTarget::Session | ModelPickTarget::Helper
            )
        {
            self.refresh_toolbox();
        }
        Ok(())
    }

    /// The popup a confirmed pick should return to, given the pick target —
    /// view-layer helper (the picker opens from `/config` for feature models).
    pub fn popup_after_pick(target: ModelPickTarget) -> Popup {
        match target {
            ModelPickTarget::Session => Popup::None,
            ModelPickTarget::Helper => Popup::Settings,
            ModelPickTarget::SwarmPersona(_) => Popup::Swarm,
        }
    }

    /// Return the helper model to auto (Backspace on its row in `/config`).
    pub fn clear_helper_model(&mut self) -> Result<()> {
        self.helper_model.clear();
        self.db.set_setting("helper_model", "")?;
        self.push_status("helper model: auto".to_string());
        if self.work_mode == super::WorkMode::AgentAuto {
            self.refresh_toolbox();
        }
        Ok(())
    }

    /// `/login`: start the `OpenAI` Codex device-code login (the only backend
    /// without a plain API key): spawns the task and owns the result channel.
    pub fn start_codex_login(&mut self) {
        // A previous login task can be left around after cancellation/timeout while
        // the UI has no useful way to resume it. Starting again should replace the
        // receiver instead of trapping the user behind a stale "already running" gate.
        self.login_rx = None;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.login_rx = Some(rx);
        self.push_status("starting OpenAI Codex login…".to_string());
        tokio::spawn(async move {
            let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let forward = tx.clone();
            tokio::spawn(async move {
                while let Some(s) = status_rx.recv().await {
                    let _ = forward.send(super::LoginMsg::Status(s));
                }
            });
            let result = crate::config::login_openai_codex_device(status_tx)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(super::LoginMsg::Done(result));
        });
    }

    pub fn on_login_result(&mut self, msg: Option<super::LoginMsg>) {
        match msg {
            Some(super::LoginMsg::Status(s)) => self.push_status(s),
            Some(super::LoginMsg::Done(Ok(creds))) => {
                self.login_rx = None;
                self.backends.set(
                    BackendTag::Codex,
                    OpenRouter::openai_codex(creds.access.clone()),
                );
                self.saved.codex = Some(creds);
                self.push_status("OpenAI Codex login saved, loading models…".to_string());
                self.fetch_models();
                self.refresh_toolbox();
            }
            Some(super::LoginMsg::Done(Err(e))) => {
                self.login_rx = None;
                self.push_status(format!("OpenAI Codex login failed: {e}"));
            }
            None => self.login_rx = None,
        }
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use crate::app::{AppCommand, AppEvent};

    #[test]
    fn router_note_is_once_per_run_and_only_for_session_selection() {
        for router in ["typesafe/jev-router", "openrouter/auto"] {
            let mut app = crate::app::tests::app_with_key();
            app.model_pick_target = ModelPickTarget::Helper;
            app.pick_model(router).unwrap();
            assert!(!app.router_note_shown);
            app.model_pick_target = ModelPickTarget::Session;
            let session = app
                .db
                .create_session("router", "a/one", &app.active_space.id, "chat")
                .unwrap();
            app.session = Some(session.clone());
            app.execute(AppCommand::SetModel { id: "a/one".into() })
                .unwrap();
            assert!(!app.router_note_shown);
            app.pending_events.clear();
            for id in [router, router, "a/one", "openrouter/auto"] {
                app.execute(AppCommand::SetModel { id: id.into() }).unwrap();
                assert_eq!(app.current_model.as_deref(), Some(id));
                assert_eq!(app.db.get_session(&session.id).unwrap().unwrap().model, id);
            }
            let notes: Vec<_> = app
                .pending_events
                .iter()
                .filter_map(|event| match event {
                    AppEvent::Status(text) if text.starts_with("router models") => Some(text),
                    _ => None,
                })
                .collect();
            assert_eq!(notes.len(), 1);
            assert_eq!(
                notes[0],
                "router models pick a model per request · prompt caching is less effective · compare with nexus eval --compare-model first"
            );
            let root = app.space.root.clone();
            drop(app);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
