use model_provider::{Format, ProviderConfig};

/// Default provider-credential target. The harness stages the provider Secret
/// here and sets `INFERENCE_API_KEY_FILE` to the same path; a job that names no
/// secret leaves the file absent, which reads as an empty credential.
const DEFAULT_API_KEY_FILE: &str = "/run/secrets/provider/credential";

/// Read the provider wire coordinates the harness stamped into the inference
/// job's env (`INFERENCE_BASE_URL`/`INFERENCE_FORMAT`/`INFERENCE_MODEL`) and the
/// staged credential (`INFERENCE_API_KEY_FILE`). Fail-closed: a missing or
/// unparseable coordinate fails the pod naming the variable to fix.
pub(crate) fn load_config() -> Result<(Format, String, ProviderConfig), String> {
    let format_str = std::env::var("INFERENCE_FORMAT")
        .map_err(|_| "INFERENCE_FORMAT must be set".to_string())?;
    let format: Format = serde_json::from_str(&format!("\"{format_str}\""))
        .map_err(|e| format!("invalid format \"{format_str}\": {e}"))?;

    let model =
        std::env::var("INFERENCE_MODEL").map_err(|_| "INFERENCE_MODEL must be set".to_string())?;
    let base_url = std::env::var("INFERENCE_BASE_URL")
        .map_err(|_| "INFERENCE_BASE_URL must be set".to_string())?;

    let api_key_file = std::env::var("INFERENCE_API_KEY_FILE")
        .unwrap_or_else(|_| DEFAULT_API_KEY_FILE.to_string());
    // Absent means the model config declared no secret; blank means a broken one.
    let api_key = match std::fs::read_to_string(&api_key_file) {
        Ok(contents) => match contents.trim() {
            "" => return Err(format!("provider credential {api_key_file} is empty")),
            key => key.to_string(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(format!(
                "provider credential {api_key_file} unreadable: {e}"
            ))
        }
    };

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
            "INFERENCE_FORMAT",
            "INFERENCE_MODEL",
            "INFERENCE_BASE_URL",
            "INFERENCE_API_KEY_FILE",
        ] {
            std::env::remove_var(key);
        }
    }

    fn set_required_env() {
        std::env::set_var("INFERENCE_FORMAT", "anthropic");
        std::env::set_var("INFERENCE_MODEL", "claude-sonnet-4-20250514");
        std::env::set_var("INFERENCE_BASE_URL", "https://api.anthropic.com/v1");
    }

    #[test]
    fn load_config_reads_api_key_from_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let key_path = tmp.path().join("credential");
        std::fs::write(&key_path, "sk-test\n").unwrap();
        std::env::set_var("INFERENCE_API_KEY_FILE", key_path.to_str().unwrap());
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
            "INFERENCE_API_KEY_FILE",
            "/nonexistent/path/that/should/not/exist/anywhere",
        );
        let (_, _, config) = load_config().unwrap();
        assert!(config.api_key.is_empty());
        clear_env();
    }

    #[test]
    fn load_config_blank_api_key_file_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let key_path = tmp.path().join("credential");
        std::fs::write(&key_path, "\n  \n").unwrap();
        std::env::set_var("INFERENCE_API_KEY_FILE", key_path.to_str().unwrap());
        let Err(err) = load_config() else {
            panic!("a blank credential is not a credential");
        };
        assert!(
            err.contains(key_path.to_str().unwrap()),
            "the error names the path an operator must fix, got: {err}"
        );
        clear_env();
    }

    // A directory, not a mode-000 file: CI running as root would read that.
    #[test]
    fn load_config_unreadable_api_key_file_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        set_required_env();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("INFERENCE_API_KEY_FILE", tmp.path().to_str().unwrap());
        let Err(err) = load_config() else {
            panic!("an unreadable credential is fatal");
        };
        assert!(
            err.contains(tmp.path().to_str().unwrap()),
            "the error names the path an operator must fix, got: {err}"
        );
        clear_env();
    }

    #[test]
    fn load_config_missing_format_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("INFERENCE_MODEL", "m");
        std::env::set_var("INFERENCE_BASE_URL", "http://x");
        assert!(load_config().is_err());
        clear_env();
    }

    #[test]
    fn load_config_invalid_format_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("INFERENCE_FORMAT", "banana");
        std::env::set_var("INFERENCE_MODEL", "m");
        std::env::set_var("INFERENCE_BASE_URL", "http://x");
        assert!(load_config().is_err());
        clear_env();
    }
}
