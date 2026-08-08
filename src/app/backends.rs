//! Every backend the app can be logged into at once. Replaces the old
//! single active `self.provider` — since `/model` now merges every
//! configured backend's models into one list, sending a request has to
//! resolve *which* backend a picked model belongs to, not just use "the"
//! provider.

use crate::provider::openrouter::OpenRouter;
use crate::provider::{BackendTag, Model};

#[derive(Clone, Default)]
pub struct Backends {
    pub openrouter: Option<OpenRouter>,
    pub openai: Option<OpenRouter>,
    pub opencode: Option<OpenRouter>,
    pub codex: Option<OpenRouter>,
}

impl Backends {
    pub fn any(&self) -> bool {
        self.openrouter.is_some()
            || self.openai.is_some()
            || self.opencode.is_some()
            || self.codex.is_some()
    }

    pub fn get(&self, tag: BackendTag) -> Option<&OpenRouter> {
        match tag {
            BackendTag::OpenRouter => self.openrouter.as_ref(),
            BackendTag::OpenAi => self.openai.as_ref(),
            BackendTag::OpencodeGo => self.opencode.as_ref(),
            BackendTag::Codex => self.codex.as_ref(),
        }
    }

    pub fn set(&mut self, tag: BackendTag, provider: OpenRouter) {
        match tag {
            BackendTag::OpenRouter => self.openrouter = Some(provider),
            BackendTag::OpenAi => self.openai = Some(provider),
            BackendTag::OpencodeGo => self.opencode = Some(provider),
            BackendTag::Codex => self.codex = Some(provider),
        }
    }

    /// Which backend tags are currently configured, in a fixed display
    /// order — used to drive the model picker's backend filter.
    pub fn configured_tags(&self) -> Vec<BackendTag> {
        [
            BackendTag::OpenRouter,
            BackendTag::OpenAi,
            BackendTag::OpencodeGo,
            BackendTag::Codex,
        ]
        .into_iter()
        .filter(|t| self.get(*t).is_some())
        .collect()
    }

    /// Split a composite model id (see `composite_id`) into the backend
    /// that owns it and the raw id to actually send in a request, and
    /// clone out that backend's provider. `None` if the composite id's
    /// backend isn't configured (e.g. its login was removed).
    pub fn resolve(&self, composite_id: &str) -> Option<(OpenRouter, String)> {
        for tag in [
            BackendTag::OpenAi,
            BackendTag::OpencodeGo,
            BackendTag::Codex,
        ] {
            if let Some(raw) = composite_id.strip_prefix(tag.key_prefix()) {
                return self.get(tag).cloned().map(|p| (p, raw.to_string()));
            }
        }
        // No known prefix matched => a bare id. Prefer OpenRouter for
        // backwards compatibility with existing stored model choices; if it
        // isn't configured (tests/single-backend setups), use the first
        // configured backend so unprefixed defaults still work.
        self.get(BackendTag::OpenRouter)
            .or_else(|| self.configured_tags().first().and_then(|t| self.get(*t)))
            .cloned()
            .map(|p| (p, composite_id.to_string()))
    }
}

/// The key used to persist/compare a model choice (favorites, last-used,
/// current-model, per-feature model settings): bare id for OpenRouter
/// (unprefixed, so existing users' saved data keeps working untouched),
/// prefixed for the other three backends since their raw ids can collide.
pub fn composite_id(m: &Model) -> String {
    format!("{}{}", m.backend.key_prefix(), m.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_routes_prefixed_models_to_their_backend_even_with_openrouter_configured() {
        let mut backends = Backends::default();
        backends.set(
            BackendTag::OpenRouter,
            OpenRouter::openrouter_flavor("or".into()),
        );
        backends.set(BackendTag::OpenAi, OpenRouter::openai("oa".into()));
        backends.set(BackendTag::OpencodeGo, OpenRouter::opencode_go("go".into()));
        backends.set(BackendTag::Codex, OpenRouter::openai_codex("codex".into()));

        let (provider, raw) = backends.resolve("openai:gpt-4.1-mini").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::OpenAi);
        assert_eq!(raw, "gpt-4.1-mini");

        let (provider, raw) = backends.resolve("opencode:deepseek-v4-flash").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::OpencodeGo);
        assert_eq!(raw, "deepseek-v4-flash");

        let (provider, raw) = backends.resolve("codex:gpt-5.4-mini").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::Codex);
        assert_eq!(raw, "gpt-5.4-mini");
    }

    #[test]
    fn resolve_keeps_bare_ids_on_openrouter_for_legacy_settings() {
        let mut backends = Backends::default();
        backends.set(
            BackendTag::OpenRouter,
            OpenRouter::openrouter_flavor("or".into()),
        );
        backends.set(BackendTag::OpenAi, OpenRouter::openai("oa".into()));

        let (provider, raw) = backends.resolve("google/gemini-2.5-flash-lite").unwrap();
        assert_eq!(provider.backend_tag(), BackendTag::OpenRouter);
        assert_eq!(raw, "google/gemini-2.5-flash-lite");
    }
}
