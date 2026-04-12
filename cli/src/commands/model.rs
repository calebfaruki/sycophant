use serde_yaml::Value;

use crate::cli::{ModelCmd, ModelSet, ModelSub};
use crate::providers;
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: ModelCmd) -> Result<(), String> {
    match cmd.sub {
        ModelSub::Set(set) => do_set(scope, set),
        ModelSub::List(_) => do_list(scope),
        ModelSub::Delete(del) => do_delete(scope, &del.key),
    }
}

fn do_set(scope: &Scope, cmd: ModelSet) -> Result<(), String> {
    let preset = providers::lookup(&cmd.provider)?;
    let base_url = cmd.base_url.as_deref().unwrap_or(preset.base_url);
    let key = format!("{}.{}", cmd.provider, cmd.model);

    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;
    let models = values::ensure_map(&mut root, "models");

    let mut entry = serde_yaml::Mapping::new();
    entry.insert(
        Value::String("format".into()),
        Value::String(preset.format.into()),
    );
    entry.insert(
        Value::String("model".into()),
        Value::String(cmd.model.clone()),
    );
    entry.insert(
        Value::String("baseUrl".into()),
        Value::String(base_url.into()),
    );

    if let Some(t) = cmd.thinking {
        entry.insert(Value::String("thinking".into()), Value::String(t));
    }

    if let Some(secret_name) = cmd.secret {
        let mut secret = serde_yaml::Mapping::new();
        secret.insert(Value::String("name".into()), Value::String(secret_name));
        if let Some(file_path) = cmd.secret_file {
            secret.insert(Value::String("file".into()), Value::String(file_path));
        } else {
            secret.insert(Value::String("env".into()), Value::String("API_KEY".into()));
        }
        entry.insert(Value::String("secret".into()), Value::Mapping(secret));
    }

    models.insert(Value::String(key.clone()), Value::Mapping(entry));

    values::save(&values_path, &root)?;
    eprintln!("Model '{key}' configured.");
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

    eprintln!("{:<32} {:<12} {:<32} URL", "KEY", "FORMAT", "MODEL");
    for (key, val) in models {
        let name = key.as_str().unwrap_or("");
        let format = val.get("format").and_then(|v| v.as_str()).unwrap_or("");
        let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("");
        let base_url = val.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("{name:<32} {format:<12} {model:<32} {base_url}");
    }

    Ok(())
}

fn do_delete(scope: &Scope, key: &str) -> Result<(), String> {
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;

    let models = root
        .get_mut("models")
        .and_then(|v| v.as_mapping_mut())
        .ok_or("no models configured")?;

    let yaml_key = Value::String(key.into());
    if models.remove(&yaml_key).is_none() {
        return Err(format!("Model \"{key}\" not found."));
    }

    values::save(&values_path, &root)?;
    eprintln!("Model '{key}' deleted.");
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
        model: &str,
        provider: &str,
        secret: Option<&str>,
        secret_file: Option<&str>,
        thinking: Option<&str>,
        base_url: Option<&str>,
    ) -> ModelSet {
        ModelSet {
            model: model.into(),
            provider: provider.into(),
            secret: secret.map(String::from),
            secret_file: secret_file.map(String::from),
            thinking: thinking.map(String::from),
            base_url: base_url.map(String::from),
        }
    }

    #[test]
    fn set_with_provider_preset() {
        let (scope, dir) = tmp_scope("set-preset");
        write_values(&scope, "models: {}\n");
        let cmd = make_set("haiku-4-5-20251001", "anthropic", None, None, None, None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let m = &root["models"]["anthropic.haiku-4-5-20251001"];
        assert_eq!(m["format"].as_str().unwrap(), "anthropic");
        assert_eq!(m["model"].as_str().unwrap(), "haiku-4-5-20251001");
        assert_eq!(
            m["baseUrl"].as_str().unwrap(),
            "https://api.anthropic.com/v1"
        );
        assert!(m.get("secret").is_none());
        cleanup(&dir);
    }

    #[test]
    fn set_with_custom_base_url() {
        let (scope, dir) = tmp_scope("set-custom-url");
        write_values(&scope, "models: {}\n");
        let cmd = make_set(
            "gpt-5",
            "openai",
            None,
            None,
            None,
            Some("http://localhost:8080/v1"),
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["models"]["openai.gpt-5"]["baseUrl"].as_str().unwrap(),
            "http://localhost:8080/v1"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_unknown_provider_errors() {
        let (scope, dir) = tmp_scope("set-unknown");
        write_values(&scope, "models: {}\n");
        let cmd = make_set("model", "nonexistent", None, None, None, None);
        let err = do_set(&scope, cmd).unwrap_err();
        assert!(err.contains("unknown provider"));
        assert!(err.contains("anthropic"));
        cleanup(&dir);
    }

    #[test]
    fn set_with_secret() {
        let (scope, dir) = tmp_scope("set-secret");
        write_values(&scope, "models: {}\n");
        let cmd = make_set("haiku", "anthropic", Some("my-key"), None, None, None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let secret = &root["models"]["anthropic.haiku"]["secret"];
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
            "haiku",
            "anthropic",
            Some("my-key"),
            Some("/run/secrets/key"),
            None,
            None,
        );
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let secret = &root["models"]["anthropic.haiku"]["secret"];
        assert_eq!(secret["file"].as_str().unwrap(), "/run/secrets/key");
        assert!(secret.get("env").is_none());
        cleanup(&dir);
    }

    #[test]
    fn set_key_format() {
        let (scope, dir) = tmp_scope("set-key-format");
        write_values(&scope, "models: {}\n");
        let cmd = make_set("haiku-4-5-20251001", "anthropic", None, None, None, None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert!(root["models"]["anthropic.haiku-4-5-20251001"].is_mapping());
        cleanup(&dir);
    }

    #[test]
    fn set_with_thinking() {
        let (scope, dir) = tmp_scope("set-thinking");
        write_values(&scope, "models: {}\n");
        let cmd = make_set("haiku", "anthropic", None, None, Some("high"), None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["models"]["anthropic.haiku"]["thinking"]
                .as_str()
                .unwrap(),
            "high"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_preserves_other_models() {
        let (scope, dir) = tmp_scope("set-preserve");
        write_values(
            &scope,
            "models:\n  existing.model:\n    format: openai\n    model: gpt\n    baseUrl: http://x\n",
        );
        let cmd = make_set("haiku", "anthropic", None, None, None, None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert!(root["models"]["existing.model"].is_mapping());
        assert!(root["models"]["anthropic.haiku"].is_mapping());
        cleanup(&dir);
    }

    #[test]
    fn delete_existing() {
        let (scope, dir) = tmp_scope("delete-existing");
        write_values(
            &scope,
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\n",
        );
        do_delete(&scope, "anthropic.haiku").unwrap();
        let root = read_values(&scope);
        assert!(root["models"].as_mapping().unwrap().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn delete_nonexistent_errors() {
        let (scope, dir) = tmp_scope("delete-missing");
        write_values(&scope, "models: {}\n");
        let err = do_delete(&scope, "anthropic.haiku").unwrap_err();
        assert!(err.contains("not found"));
        cleanup(&dir);
    }

    #[test]
    fn delete_preserves_other_models() {
        let (scope, dir) = tmp_scope("delete-preserve");
        write_values(
            &scope,
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\n  openai.gpt:\n    format: openai\n    model: gpt\n    baseUrl: http://y\n",
        );
        do_delete(&scope, "anthropic.haiku").unwrap();
        let root = read_values(&scope);
        assert!(root["models"]["openai.gpt"].is_mapping());
        assert!(root["models"].as_mapping().unwrap().len() == 1);
        cleanup(&dir);
    }
}
