use anyhow::{Context as _, Result};
use chrono::Utc;

use crate::provider::BackendTag;
use crate::provider::local::{LocalConfig, LocalRuntime};
use crate::provider::openrouter::OpenRouter;
use crate::provider::serve::RuntimeStatus;

use super::{App, ModelPickTarget, Popup};

impl App {
    /// Rebuild every backend from the on-disk saved credentials (after boot,
    /// or after a settings change). Keeps `saved` and `backends` in sync.
    pub fn rebuild_all_backends(&mut self) {
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
                None => "local inference is off — /local <ollama|mlx|lmstudio|edge0>".to_string(),
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
                .context("local inference is off — /local <ollama|mlx|lmstudio|edge0> first")?
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
        // MLX and edge0 serve exactly one model, so they launch with the
        // selected one; the composite id carries a `local:` prefix.
        let model = self
            .current_model
            .as_deref()
            .and_then(|id| id.strip_prefix("local:"))
            .map(str::to_owned);
        let argv = self.local_servers.start(runtime, model.as_deref(), port)?;
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
        Some((provider.clone(), default(&provider).to_string()))
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
        let streaming = self.is_streaming();
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
        let estimate = (chars / 4) as u64;
        // The character estimate is intentionally conservative about wire
        // overhead and provider tokenization. Do not let it make the status
        // bar visibly shrink at the start of a new request: the previous
        // completed request is a better lower bound until this one reports
        // exact usage. Compaction/model/session boundaries clear
        // `context_total` before a genuinely smaller context is shown.
        if streaming {
            self.context_total
                .map_or(estimate, |total| total.max(estimate))
        } else {
            estimate
        }
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

    /// Set the active model (or a feature model, per the pick target). The
    /// view layer owns the popup routing; this is the domain half.
    pub fn pick_model(&mut self, id: &str) -> Result<()> {
        match self.model_pick_target {
            ModelPickTarget::Session => {
                if self.current_model.as_deref() != Some(id) {
                    self.bump_cache_epoch();
                }
                self.current_model = Some(id.to_string());
                if let Some(session) = &self.session {
                    self.db.set_session_model(&session.id, id)?;
                }
                self.db.mark_model_used(id)?;
                self.last_used
                    .insert(id.to_string(), Utc::now().to_rfc3339());
                self.push_status(format!("model: {id}"));
            }
            ModelPickTarget::Memory => {
                self.memory_model = id.to_string();
                self.db.set_setting("memory_model", id)?;
                self.push_status(format!("memory model: {id}"));
            }
            ModelPickTarget::Transcriber => {
                self.transcriber_model = id.to_string();
                self.db.set_setting("transcriber_model", id)?;
                self.push_status(format!("image model: {id}"));
            }
            ModelPickTarget::Ocr => {
                self.ocr_model = id.to_string();
                self.db.set_setting("ocr_model", id)?;
                self.push_status(format!("OCR model: {id}"));
            }
            ModelPickTarget::ImageGen => {
                self.image_gen_model = id.to_string();
                self.db.set_setting("image_gen_model", id)?;
                self.push_status(format!("image gen model: {id}"));
            }
            ModelPickTarget::VideoGen => {
                self.video_gen_model = id.to_string();
                self.db.set_setting("video_gen_model", id)?;
                self.push_status(format!("video gen model: {id}"));
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
        Ok(())
    }

    /// The popup a confirmed pick should return to, given the pick target —
    /// view-layer helper (the picker opens from `/config` for feature models).
    pub fn popup_after_pick(target: ModelPickTarget) -> Popup {
        match target {
            ModelPickTarget::Session => Popup::None,
            ModelPickTarget::Memory
            | ModelPickTarget::Transcriber
            | ModelPickTarget::Ocr
            | ModelPickTarget::ImageGen
            | ModelPickTarget::VideoGen => Popup::Settings,
            ModelPickTarget::SwarmPersona(_) => Popup::Swarm,
        }
    }

    /// Disable memory extraction entirely (Backspace on the memory-model row
    /// in `/config`).
    pub fn clear_memory_model(&mut self) -> Result<()> {
        self.memory_model.clear();
        self.db.set_setting("memory_model", "")?;
        self.push_status("memory model cleared — extraction disabled".to_string());
        Ok(())
    }

    /// Disable image transcription entirely (Backspace on the
    /// transcriber-model row in `/config`).
    pub fn clear_transcriber_model(&mut self) -> Result<()> {
        self.transcriber_model.clear();
        self.db.set_setting("transcriber_model", "")?;
        self.push_status("image model cleared — image descriptions disabled".to_string());
        Ok(())
    }

    /// Disable VLM OCR (Backspace on the OCR-model row in `/config`).
    pub fn clear_ocr_model(&mut self) -> Result<()> {
        self.ocr_model.clear();
        self.db.set_setting("ocr_model", "")?;
        self.push_status("OCR model cleared — scanned PDFs use tesseract".to_string());
        Ok(())
    }

    /// Disable image generation (Backspace on the image gen model row in `/config`).
    pub fn clear_image_gen_model(&mut self) -> Result<()> {
        self.image_gen_model.clear();
        self.db.set_setting("image_gen_model", "")?;
        self.push_status("image gen model cleared — generation disabled".to_string());
        Ok(())
    }

    /// Disable video generation (Backspace on the video gen model row in `/config`).
    pub fn clear_video_gen_model(&mut self) -> Result<()> {
        self.video_gen_model.clear();
        self.db.set_setting("video_gen_model", "")?;
        self.push_status("video gen model cleared — generation disabled".to_string());
        Ok(())
    }

    /// `/login`: start the `OpenAI` Codex device-code login (the only backend
    /// without a plain API key). Domain side: spawns the task and owns the
    /// result channel; the view layer shows the selector.
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
