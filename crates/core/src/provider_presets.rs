//! Curated catalog of provider presets used by the TUI `/configure` flow.
//!
//! Each preset bundles the non-negotiable knobs needed to connect to one
//! provider family: the wire API, the default base URL (if any), and the
//! environment variable names we'll look for as a fallback. The actual
//! model list is NOT baked in — users type their own model slug in v1.

use crate::ProviderWireApi;

/// One curated provider entry shown in the `/configure` provider picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPreset {
    /// Stable identifier used as the key in `model_providers` config.
    pub id: &'static str,
    /// Human-readable label shown in the picker.
    pub display_name: &'static str,
    /// Which wire API this preset speaks.
    pub wire_api: ProviderWireApi,
    /// Default base URL. `None` means "user must supply their own".
    pub default_base_url: Option<&'static str>,
    /// Environment variables we'll check as fallback API-key sources.
    pub api_key_env_vars: &'static [&'static str],
    /// Short help line shown under the display name.
    pub description: &'static str,
    /// Whether this preset represents the "custom/BYO endpoint" sentinel.
    pub is_custom: bool,
}

/// Returns every preset in display order.
///
/// First-party providers come first, OpenAI-compatible aggregators follow,
/// and `Custom` sits at the bottom as an escape hatch.
pub fn all_presets() -> &'static [ProviderPreset] {
    &[
        ProviderPreset {
            id: "anthropic",
            display_name: "Anthropic (Claude)",
            wire_api: ProviderWireApi::AnthropicMessages,
            default_base_url: Some("https://api.anthropic.com"),
            api_key_env_vars: &["ANTHROPIC_API_KEY", "LPA_API_KEY"],
            description: "Claude models — Sonnet, Opus, Haiku",
            is_custom: false,
        },
        ProviderPreset {
            id: "openai",
            display_name: "OpenAI",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: Some("https://api.openai.com"),
            api_key_env_vars: &["OPENAI_API_KEY", "LPA_API_KEY"],
            description: "GPT models — gpt-4o, gpt-5, o1 family",
            is_custom: false,
        },
        ProviderPreset {
            id: "google",
            display_name: "Google Gemini",
            wire_api: ProviderWireApi::GoogleGenerateContent,
            default_base_url: Some("https://generativelanguage.googleapis.com"),
            api_key_env_vars: &["GOOGLE_API_KEY", "GEMINI_API_KEY", "LPA_API_KEY"],
            description: "Gemini models — 2.5 Pro, 2.5 Flash",
            is_custom: false,
        },
        ProviderPreset {
            id: "openrouter",
            display_name: "OpenRouter",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: Some("https://openrouter.ai/api/v1"),
            api_key_env_vars: &["OPENROUTER_API_KEY", "LPA_API_KEY"],
            description: "Unified gateway — free + paid models from many vendors",
            is_custom: false,
        },
        ProviderPreset {
            id: "groq",
            display_name: "Groq",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: Some("https://api.groq.com/openai/v1"),
            api_key_env_vars: &["GROQ_API_KEY", "LPA_API_KEY"],
            description: "Very fast inference — Llama, Mixtral, Gemma, Qwen",
            is_custom: false,
        },
        ProviderPreset {
            id: "together",
            display_name: "Together AI",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: Some("https://api.together.xyz/v1"),
            api_key_env_vars: &["TOGETHER_API_KEY", "LPA_API_KEY"],
            description: "Open-weight models at scale — Llama, Qwen, DeepSeek",
            is_custom: false,
        },
        ProviderPreset {
            id: "mistral",
            display_name: "Mistral",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: Some("https://api.mistral.ai/v1"),
            api_key_env_vars: &["MISTRAL_API_KEY", "LPA_API_KEY"],
            description: "Mistral models — Large, Medium, Codestral",
            is_custom: false,
        },
        ProviderPreset {
            id: "ollama",
            display_name: "Ollama (local)",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: Some("http://localhost:11434/v1"),
            api_key_env_vars: &[],
            description: "Run models locally — no API key needed",
            is_custom: false,
        },
        ProviderPreset {
            id: "custom",
            display_name: "Custom endpoint",
            wire_api: ProviderWireApi::OpenAIChatCompletions,
            default_base_url: None,
            api_key_env_vars: &["LPA_API_KEY"],
            description: "Any OpenAI-compatible endpoint",
            is_custom: true,
        },
    ]
}

/// Looks up a preset by its stable id.
pub fn preset_by_id(id: &str) -> Option<&'static ProviderPreset> {
    all_presets().iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn catalog_contains_all_expected_presets() {
        let ids: Vec<_> = all_presets().iter().map(|p| p.id).collect();
        assert_eq!(
            ids,
            vec![
                "anthropic",
                "openai",
                "google",
                "openrouter",
                "groq",
                "together",
                "mistral",
                "ollama",
                "custom",
            ]
        );
    }

    #[test]
    fn openrouter_preset_has_expected_defaults() {
        let preset = preset_by_id("openrouter").expect("openrouter preset");
        assert_eq!(
            preset.default_base_url,
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(preset.wire_api, ProviderWireApi::OpenAIChatCompletions);
        assert!(preset.api_key_env_vars.contains(&"OPENROUTER_API_KEY"));
        assert!(!preset.is_custom);
    }

    #[test]
    fn custom_preset_has_no_default_base_url() {
        let preset = preset_by_id("custom").expect("custom preset");
        assert!(preset.default_base_url.is_none());
        assert!(preset.is_custom);
    }

    #[test]
    fn ollama_preset_has_no_api_key_env_vars() {
        let preset = preset_by_id("ollama").expect("ollama preset");
        assert!(preset.api_key_env_vars.is_empty());
        assert!(preset.default_base_url.unwrap().starts_with("http://"));
    }

    #[test]
    fn preset_by_id_returns_none_for_unknown() {
        assert!(preset_by_id("does-not-exist").is_none());
    }
}
