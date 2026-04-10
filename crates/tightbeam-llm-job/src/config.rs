use tightbeam_providers::{Format, ProviderConfig, ThinkingBudget};

pub(crate) fn load_config() -> Result<(Format, String, ProviderConfig), String> {
    let format_str = std::env::var("TIGHTBEAM_FORMAT")
        .map_err(|_| "TIGHTBEAM_FORMAT must be set".to_string())?;
    let format: Format = serde_json::from_str(&format!("\"{format_str}\""))
        .map_err(|e| format!("invalid format \"{format_str}\": {e}"))?;

    let model =
        std::env::var("TIGHTBEAM_MODEL").map_err(|_| "TIGHTBEAM_MODEL must be set".to_string())?;
    let base_url = std::env::var("TIGHTBEAM_BASE_URL")
        .map_err(|_| "TIGHTBEAM_BASE_URL must be set".to_string())?;
    let api_key = std::env::var("API_KEY").unwrap_or_default();

    let thinking = std::env::var("TIGHTBEAM_THINKING")
        .ok()
        .and_then(|s| serde_json::from_str::<ThinkingBudget>(&format!("\"{s}\"")).ok());

    let config = ProviderConfig {
        model,
        api_key,
        max_tokens: 8192,
        thinking,
    };

    Ok((format, base_url, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        for key in &[
            "TIGHTBEAM_FORMAT",
            "TIGHTBEAM_MODEL",
            "TIGHTBEAM_BASE_URL",
            "API_KEY",
            "TIGHTBEAM_THINKING",
        ] {
            std::env::remove_var(key);
        }
    }

    fn set_required_env() {
        std::env::set_var("TIGHTBEAM_FORMAT", "anthropic");
        std::env::set_var("TIGHTBEAM_MODEL", "claude-sonnet-4-20250514");
        std::env::set_var("TIGHTBEAM_BASE_URL", "https://api.anthropic.com/v1");
    }

    #[test]
    fn load_config_valid() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        std::env::set_var("API_KEY", "sk-test");
        let (format, base_url, config) = load_config().unwrap();
        assert_eq!(format, Format::Anthropic);
        assert_eq!(base_url, "https://api.anthropic.com/v1");
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.api_key, "sk-test");
        assert_eq!(config.max_tokens, 8192);
        assert!(config.thinking.is_none());
        clear_env();
    }

    #[test]
    fn load_config_with_thinking() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        std::env::set_var("TIGHTBEAM_THINKING", "high");
        let (_, _, config) = load_config().unwrap();
        assert_eq!(config.thinking, Some(ThinkingBudget::High));
        clear_env();
    }

    #[test]
    fn load_config_missing_format_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("TIGHTBEAM_MODEL", "m");
        std::env::set_var("TIGHTBEAM_BASE_URL", "http://x");
        assert!(load_config().is_err());
        clear_env();
    }

    #[test]
    fn load_config_invalid_format_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("TIGHTBEAM_FORMAT", "banana");
        std::env::set_var("TIGHTBEAM_MODEL", "m");
        std::env::set_var("TIGHTBEAM_BASE_URL", "http://x");
        assert!(load_config().is_err());
        clear_env();
    }

    #[test]
    fn load_config_api_key_defaults_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        let (_, _, config) = load_config().unwrap();
        assert!(config.api_key.is_empty());
        clear_env();
    }
}
