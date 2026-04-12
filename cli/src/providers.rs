#[derive(Debug)]
pub(crate) struct ProviderPreset {
    pub name: &'static str,
    pub format: &'static str,
    pub base_url: &'static str,
}

const PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "anthropic",
        format: "anthropic",
        base_url: "https://api.anthropic.com/v1",
    },
    ProviderPreset {
        name: "openai",
        format: "openai",
        base_url: "https://api.openai.com/v1",
    },
    ProviderPreset {
        name: "openrouter",
        format: "openai",
        base_url: "https://openrouter.ai/api/v1",
    },
    ProviderPreset {
        name: "groq",
        format: "openai",
        base_url: "https://api.groq.com/openai/v1",
    },
    ProviderPreset {
        name: "mistral",
        format: "openai",
        base_url: "https://api.mistral.ai/v1",
    },
    ProviderPreset {
        name: "deepseek",
        format: "openai",
        base_url: "https://api.deepseek.com/v1",
    },
    ProviderPreset {
        name: "xai",
        format: "openai",
        base_url: "https://api.x.ai/v1",
    },
    ProviderPreset {
        name: "cerebras",
        format: "openai",
        base_url: "https://api.cerebras.ai/v1",
    },
    ProviderPreset {
        name: "together",
        format: "openai",
        base_url: "https://api.together.xyz/v1",
    },
    ProviderPreset {
        name: "fireworks",
        format: "openai",
        base_url: "https://api.fireworks.ai/inference/v1",
    },
    ProviderPreset {
        name: "perplexity",
        format: "openai",
        base_url: "https://api.perplexity.ai",
    },
];

pub(crate) fn lookup(name: &str) -> Result<&'static ProviderPreset, String> {
    PROVIDERS.iter().find(|p| p.name == name).ok_or_else(|| {
        let names: Vec<&str> = PROVIDERS.iter().map(|p| p.name).collect();
        format!(
            "unknown provider \"{name}\". available: {}",
            names.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_anthropic() {
        let p = lookup("anthropic").unwrap();
        assert_eq!(p.format, "anthropic");
        assert_eq!(p.base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn lookup_openai() {
        let p = lookup("openai").unwrap();
        assert_eq!(p.format, "openai");
        assert_eq!(p.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn lookup_unknown_errors() {
        let err = lookup("nonexistent").unwrap_err();
        assert!(err.contains("unknown provider"));
        assert!(err.contains("anthropic"));
        assert!(err.contains("openai"));
    }

    #[test]
    fn all_providers_have_nonempty_fields() {
        for p in PROVIDERS {
            assert!(!p.name.is_empty());
            assert!(!p.format.is_empty());
            assert!(!p.base_url.is_empty());
        }
    }
}
