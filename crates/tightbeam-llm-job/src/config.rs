use tightbeam_providers::{Format, ProviderConfig};

const DEFAULT_API_KEY_PATH: &str = "/run/secrets/tightbeam/api-key";

pub(crate) fn load_config() -> Result<(Format, String, ProviderConfig), String> {
    let format_str = std::env::var("TIGHTBEAM_FORMAT")
        .map_err(|_| "TIGHTBEAM_FORMAT must be set".to_string())?;
    let format: Format = serde_json::from_str(&format!("\"{format_str}\""))
        .map_err(|e| format!("invalid format \"{format_str}\": {e}"))?;

    let model =
        std::env::var("TIGHTBEAM_MODEL").map_err(|_| "TIGHTBEAM_MODEL must be set".to_string())?;
    let base_url = std::env::var("TIGHTBEAM_BASE_URL")
        .map_err(|_| "TIGHTBEAM_BASE_URL must be set".to_string())?;

    let api_key_path = std::env::var("TIGHTBEAM_API_KEY_PATH")
        .unwrap_or_else(|_| DEFAULT_API_KEY_PATH.to_string());
    let api_key = std::fs::read_to_string(&api_key_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let config = ProviderConfig { model, api_key };

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
            "TIGHTBEAM_API_KEY_PATH",
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
    fn load_config_reads_api_key_from_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let key_path = tmp.path().join("api-key");
        std::fs::write(&key_path, "sk-test\n").unwrap();
        std::env::set_var("TIGHTBEAM_API_KEY_PATH", key_path.to_str().unwrap());
        let (_, _, config) = load_config().unwrap();
        assert_eq!(config.api_key, "sk-test");
        clear_env();
    }

    #[test]
    fn load_config_api_key_defaults_empty_when_file_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        std::env::set_var(
            "TIGHTBEAM_API_KEY_PATH",
            "/nonexistent/path/that/should/not/exist/anywhere",
        );
        let (_, _, config) = load_config().unwrap();
        assert!(config.api_key.is_empty());
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
}
