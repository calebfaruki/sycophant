use serde_yaml::Value;

use crate::cli::{ModelCmd, ModelSet, ModelSub};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: ModelCmd) -> Result<(), String> {
    match cmd.sub {
        ModelSub::Set(set) => do_set(scope, set),
        ModelSub::List(_) => do_list(scope),
    }
}

fn do_set(scope: &Scope, cmd: ModelSet) -> Result<(), String> {
    if cmd.secret_env.is_some() && cmd.secret_file.is_some() {
        return Err("--secret-env and --secret-file are mutually exclusive".into());
    }

    if (cmd.secret_env.is_some() || cmd.secret_file.is_some()) && cmd.secret.is_none() {
        return Err("--secret is required when using --secret-env or --secret-file".into());
    }

    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;
    let models = values::ensure_map(&mut root, "models");

    let mut entry = serde_yaml::Mapping::new();
    entry.insert(Value::String("format".into()), Value::String(cmd.format));
    entry.insert(Value::String("model".into()), Value::String(cmd.model));
    entry.insert(Value::String("baseUrl".into()), Value::String(cmd.base_url));

    if let Some(t) = cmd.thinking {
        entry.insert(Value::String("thinking".into()), Value::String(t));
    }

    if let Some(sn) = cmd.secret {
        let mut secret = serde_yaml::Mapping::new();
        secret.insert(Value::String("name".into()), Value::String(sn));
        if let Some(env) = cmd.secret_env {
            secret.insert(Value::String("env".into()), Value::String(env));
        }
        if let Some(file) = cmd.secret_file {
            secret.insert(Value::String("file".into()), Value::String(file));
        }
        entry.insert(Value::String("secret".into()), Value::Mapping(secret));
    }

    models.insert(Value::String(cmd.name.clone()), Value::Mapping(entry));

    values::save(&values_path, &root)?;
    eprintln!("Model '{}' configured.", cmd.name);
    Ok(())
}

fn do_list(scope: &Scope) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;

    let models = match root.get("models").and_then(|v| v.as_mapping()) {
        Some(m) if !m.is_empty() => m,
        _ => {
            eprintln!("No models configured.");
            return Ok(());
        }
    };

    eprintln!("{:<16} {:<12} {:<32} URL", "NAME", "FORMAT", "MODEL");
    for (key, val) in models {
        let name = key.as_str().unwrap_or("");
        let format = val.get("format").and_then(|v| v.as_str()).unwrap_or("");
        let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("");
        let base_url = val.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("{name:<16} {format:<12} {model:<32} {base_url}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_scope(name: &str) -> (Scope, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("syco-model-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let scope = Scope { root: dir.clone() };
        (scope, dir)
    }

    fn write_values(scope: &Scope, content: &str) {
        fs::write(scope.values_file(), content).unwrap();
    }

    fn read_values(scope: &Scope) -> Value {
        values::load(&scope.values_file()).unwrap()
    }

    fn cleanup(dir: &std::path::Path) {
        fs::remove_dir_all(dir).unwrap();
    }

    fn make_set(
        name: &str,
        format: &str,
        model: &str,
        base_url: &str,
        thinking: Option<&str>,
        secret: Option<&str>,
        secret_env: Option<&str>,
        secret_file: Option<&str>,
    ) -> ModelSet {
        ModelSet {
            name: name.into(),
            format: format.into(),
            model: model.into(),
            base_url: base_url.into(),
            thinking: thinking.map(String::from),
            secret: secret.map(String::from),
            secret_env: secret_env.map(String::from),
            secret_file: secret_file.map(String::from),
        }
    }

    #[test]
    fn set_creates_model_with_required_fields() {
        let (scope, dir) = tmp_scope("set-required");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "claude-sonnet-4-20250514",
            "https://api.anthropic.com/v1",
            None,
            None,
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let m = &root["models"]["default"];
        assert_eq!(m["format"].as_str().unwrap(), "anthropic");
        assert_eq!(m["model"].as_str().unwrap(), "claude-sonnet-4-20250514");
        assert_eq!(
            m["baseUrl"].as_str().unwrap(),
            "https://api.anthropic.com/v1"
        );
        assert!(m.get("thinking").is_none());
        assert!(m.get("secret").is_none());
        cleanup(&dir);
    }

    #[test]
    fn set_with_thinking() {
        let (scope, dir) = tmp_scope("set-thinking");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "claude",
            "https://api.anthropic.com/v1",
            Some("high"),
            None,
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["models"]["default"]["thinking"].as_str().unwrap(),
            "high"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_with_secret_env() {
        let (scope, dir) = tmp_scope("set-secret-env");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "claude",
            "https://api.anthropic.com/v1",
            None,
            Some("my-key"),
            Some("API_KEY"),
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let secret = &root["models"]["default"]["secret"];
        assert_eq!(secret["name"].as_str().unwrap(), "my-key");
        assert_eq!(secret["env"].as_str().unwrap(), "API_KEY");
        assert!(secret.get("file").is_none());
        cleanup(&dir);
    }

    #[test]
    fn set_with_secret_file() {
        let (scope, dir) = tmp_scope("set-secret-file");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "openai",
            "gpt-5",
            "https://api.openai.com/v1",
            None,
            Some("openai-key"),
            None,
            Some("/run/secrets/key"),
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let secret = &root["models"]["default"]["secret"];
        assert_eq!(secret["name"].as_str().unwrap(), "openai-key");
        assert_eq!(secret["file"].as_str().unwrap(), "/run/secrets/key");
        assert!(secret.get("env").is_none());
        cleanup(&dir);
    }

    #[test]
    fn set_secret_env_and_file_mutually_exclusive() {
        let (scope, dir) = tmp_scope("set-mutex");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "x",
            "http://x",
            None,
            Some("k"),
            Some("E"),
            Some("/f"),
        );
        let err = do_set(&scope, cmd).unwrap_err();
        assert!(err.contains("mutually exclusive"));
        cleanup(&dir);
    }

    #[test]
    fn set_secret_env_without_secret_name_errors() {
        let (scope, dir) = tmp_scope("set-env-no-name");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "x",
            "http://x",
            None,
            None,
            Some("E"),
            None,
        );
        let err = do_set(&scope, cmd).unwrap_err();
        assert!(err.contains("--secret is required"));
        cleanup(&dir);
    }

    #[test]
    fn set_secret_file_without_secret_name_errors() {
        let (scope, dir) = tmp_scope("set-file-no-name");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "x",
            "http://x",
            None,
            None,
            None,
            Some("/f"),
        );
        let err = do_set(&scope, cmd).unwrap_err();
        assert!(err.contains("--secret is required"));
        cleanup(&dir);
    }

    #[test]
    fn set_overwrites_existing_model() {
        let (scope, dir) = tmp_scope("set-overwrite");
        write_values(
            &scope,
            "models:\n  default:\n    format: openai\n    model: old\n    baseUrl: http://old\n",
        );
        let cmd = make_set(
            "default",
            "anthropic",
            "new",
            "http://new",
            None,
            None,
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["models"]["default"]["format"].as_str().unwrap(),
            "anthropic"
        );
        assert_eq!(root["models"]["default"]["model"].as_str().unwrap(), "new");
        cleanup(&dir);
    }

    #[test]
    fn set_preserves_other_models() {
        let (scope, dir) = tmp_scope("set-preserve");
        write_values(
            &scope,
            "models:\n  existing:\n    format: openai\n    model: gpt-5\n    baseUrl: http://openai\n",
        );
        let cmd = make_set(
            "second",
            "anthropic",
            "claude",
            "http://anthropic",
            None,
            None,
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["models"]["existing"]["model"].as_str().unwrap(),
            "gpt-5"
        );
        assert_eq!(
            root["models"]["second"]["model"].as_str().unwrap(),
            "claude"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_creates_models_map_if_missing() {
        let (scope, dir) = tmp_scope("set-no-map");
        write_values(&scope, "agents: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "x",
            "http://x",
            None,
            None,
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert!(root["models"]["default"].is_mapping());
        cleanup(&dir);
    }

    #[test]
    fn set_preserves_other_top_level_keys() {
        let (scope, dir) = tmp_scope("set-preserve-keys");
        write_values(
            &scope,
            "models: {}\nagents:\n  coder:\n    model: default\n",
        );
        let cmd = make_set(
            "default",
            "anthropic",
            "x",
            "http://x",
            None,
            None,
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["agents"]["coder"]["model"].as_str().unwrap(),
            "default"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_values_file_missing_errors() {
        let (scope, dir) = tmp_scope("set-no-file");
        let cmd = make_set("default", "x", "x", "x", None, None, None, None);
        let err = do_set(&scope, cmd).unwrap_err();
        assert!(err.contains("failed to read"));
        cleanup(&dir);
    }

    #[test]
    fn set_secret_name_only_no_env_or_file() {
        let (scope, dir) = tmp_scope("set-secret-name-only");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "default",
            "anthropic",
            "x",
            "http://x",
            None,
            Some("my-key"),
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let secret = &root["models"]["default"]["secret"];
        assert_eq!(secret["name"].as_str().unwrap(), "my-key");
        assert!(secret.get("env").is_none());
        assert!(secret.get("file").is_none());
        cleanup(&dir);
    }

    #[test]
    fn list_no_models() {
        let (scope, dir) = tmp_scope("list-empty");
        write_values(&scope, "models: {}\n");
        do_list(&scope).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_with_models() {
        let (scope, dir) = tmp_scope("list-models");
        write_values(
            &scope,
            "models:\n  default:\n    format: anthropic\n    model: claude-sonnet-4-20250514\n    baseUrl: https://api.anthropic.com/v1\n",
        );
        do_list(&scope).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_values_file_missing_errors() {
        let (scope, dir) = tmp_scope("list-no-file");
        let err = do_list(&scope).unwrap_err();
        assert!(err.contains("failed to read"));
        cleanup(&dir);
    }

    #[test]
    fn list_models_key_missing_shows_none() {
        let (scope, dir) = tmp_scope("list-no-key");
        write_values(&scope, "agents: {}\n");
        do_list(&scope).unwrap();
        cleanup(&dir);
    }
}
