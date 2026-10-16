//! Model capabilities and context budgets for the main chat loop.

use serde::{Deserialize, Serialize};

use super::{BackendTag, Model, local::LocalConfig};

/// Tool-definition detail appropriate for a model's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Small,
    Medium,
    Large,
}

/// Model-facing tool behavior, without changing persisted tool results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfile {
    pub parallel_calls: bool,
    /// Context window in tokens, as reported by the model catalog.
    pub context: Option<u64>,
    pub tier: Tier,
    /// Maximum tool-result characters in context; `None` preserves the result.
    pub result_cap: Option<usize>,
    pub compact_tools: bool,
}

/// Keep hosted requests unchanged; use conservative defaults for local models.
#[must_use]
pub fn profile_for(model: Option<&Model>, local: Option<&LocalConfig>) -> ModelProfile {
    let context = model.and_then(|model| model.context_length);
    if !model.is_some_and(|model| model.backend == BackendTag::Local) {
        return ModelProfile {
            parallel_calls: true,
            context,
            tier: Tier::Large,
            result_cap: None,
            compact_tools: false,
        };
    }
    let tier = local.and_then(|config| config.tier).unwrap_or(Tier::Small);
    // One eighth of the token window at approximately four characters/token.
    let result_cap = context.map_or(4_000, |context| (context / 2).clamp(2_000, 8_000));
    ModelProfile {
        parallel_calls: local
            .and_then(|config| config.parallel_tool_calls)
            .unwrap_or(false),
        context,
        tier,
        result_cap: Some(usize::try_from(result_cap).expect("cap is at most 8,000")),
        compact_tools: tier == Tier::Small,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(backend: BackendTag, context_length: Option<u64>) -> Model {
        Model {
            id: "test".into(),
            name: "Test".into(),
            reasoning_efforts: Vec::new(),
            context_length,
            supports_images: false,
            supports_image_generation: false,
            supports_video_generation: false,
            backend,
            pricing: None,
        }
    }

    #[test]
    fn hosted_and_missing_models_preserve_existing_behavior() {
        let config: LocalConfig =
            toml::from_str("provider = 'ollama'\nparallel_tool_calls = false\ntier = 'small'")
                .unwrap();
        for backend in [
            BackendTag::OpenRouter,
            BackendTag::OpenAi,
            BackendTag::OpencodeGo,
            BackendTag::Codex,
        ] {
            let profile = profile_for(Some(&model(backend, Some(8_000))), Some(&config));
            assert!(profile.parallel_calls);
            assert_eq!(profile.context, Some(8_000));
            assert_eq!(profile.tier, Tier::Large);
            assert_eq!(profile.result_cap, None);
            assert!(!profile.compact_tools);
        }
        assert_eq!(
            profile_for(None, Some(&config)),
            ModelProfile {
                parallel_calls: true,
                context: None,
                tier: Tier::Large,
                result_cap: None,
                compact_tools: false,
            }
        );
    }

    #[test]
    fn local_defaults_scale_the_result_budget() {
        let config: LocalConfig = toml::from_str("provider = 'ollama'").unwrap();
        for local in [None, Some(&config)] {
            for (context, cap) in [
                (Some(8_000), 4_000),
                (Some(64_000), 8_000),
                (None, 4_000),
                (Some(2_000), 2_000),
                (Some(u64::MAX), 8_000),
            ] {
                let profile = profile_for(Some(&model(BackendTag::Local, context)), local);
                assert!(!profile.parallel_calls);
                assert_eq!(profile.context, context);
                assert_eq!(profile.tier, Tier::Small);
                assert_eq!(profile.result_cap, Some(cap));
                assert!(profile.compact_tools);
            }
        }
    }

    #[test]
    fn local_config_overrides_round_trip_and_follow_the_runtime() {
        for (tier, compact) in [("small", true), ("medium", false), ("large", false)] {
            let config: LocalConfig = toml::from_str(&format!(
                "provider = 'ollama'\nparallel_tool_calls = true\ntier = '{tier}'"
            ))
            .unwrap();
            let encoded = toml::to_string(&config).unwrap();
            let decoded: LocalConfig = toml::from_str(&encoded).unwrap();
            let same = LocalConfig::for_runtime(config.provider, Some(&decoded));
            let profile = profile_for(Some(&model(BackendTag::Local, Some(8_000))), Some(&same));
            assert!(profile.parallel_calls);
            assert_eq!(profile.tier, config.tier.unwrap());
            assert_eq!(profile.compact_tools, compact);
            assert_eq!(profile.result_cap, Some(4_000));
            let switched =
                LocalConfig::for_runtime(super::super::local::LocalRuntime::Mlx, Some(&config));
            assert_eq!(switched.parallel_tool_calls, None);
            assert_eq!(switched.tier, None);
        }
        let defaults: LocalConfig = toml::from_str("provider = 'ollama'").unwrap();
        let encoded = toml::to_string(&defaults).unwrap();
        assert!(!encoded.contains("parallel_tool_calls"));
        assert!(!encoded.contains("tier"));
        assert!(toml::from_str::<LocalConfig>("provider = 'ollama'\ntier = 'huge'").is_err());
    }
}
