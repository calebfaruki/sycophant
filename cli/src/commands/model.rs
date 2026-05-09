use serde::Serialize;
use serde_yaml::Value;

use crate::cli::{ModelCmd, ModelList, ModelSet, ModelSub};
use crate::providers;
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: ModelCmd) -> Result<(), String> {
    match cmd.sub {
        ModelSub::Set(set) => do_set(scope, set),
        ModelSub::List(list) => do_list(scope, list),
        ModelSub::Delete(del) => do_delete(scope, &del.key),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelEntry {
    pub key: String,
    pub format: String,
    pub model: String,
    pub base_url: String,
}

pub(crate) fn model_list_data(models: Option<&serde_yaml::Mapping>) -> Vec<ModelEntry> {
    let Some(models) = models else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?.to_string();
            Some(ModelEntry {
                key,
                format: v
                    .get("format")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
                model: v
                    .get("model")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
                base_url: v
                    .get("baseUrl")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
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

    // Each alias becomes an independent duplicate entry (same content, different
    // key). The chart's tightbeam-models.yaml template iterates `.Values.models`
    // by key, so each entry renders as its own TightbeamModel CRD with its own
    // ModelSlot / LLM Job lifecycle. Heavy alias use multiplies LLM Jobs;
    // recommend at most 1–2 aliases per canonical model.
    models.insert(Value::String(key.clone()), Value::Mapping(entry.clone()));
    for alias in &cmd.alias {
        models.insert(Value::String(alias.clone()), Value::Mapping(entry.clone()));
    }

    values::save(&values_path, &root)?;
    if cmd.alias.is_empty() {
        eprintln!("Model '{key}' configured.");
    } else {
        eprintln!(
            "Model '{key}' configured with aliases: {}.",
            cmd.alias.join(", ")
        );
    }
    Ok(())
}

fn do_list(scope: &Scope, cmd: ModelList) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;
    let models = root.get("models").and_then(|v| v.as_mapping());

    if cmd.json {
        let entries = model_list_data(models);
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        Ok(())
    } else {
        render_model_list(models, &mut std::io::stderr()).map_err(|e| format!("write failed: {e}"))
    }
}

fn render_model_list<W: std::io::Write>(
    models: Option<&serde_yaml::Mapping>,
    out: &mut W,
) -> std::io::Result<()> {
    let models = match models {
        Some(m) if !m.is_empty() => m,
        _ => {
            writeln!(out, "No models configured.")?;
            return Ok(());
        }
    };

    writeln!(out, "{:<32} {:<12} {:<32} URL", "KEY", "FORMAT", "MODEL")?;
    for (key, val) in models {
        let name = key.as_str().unwrap_or("");
        let format = val.get("format").and_then(|v| v.as_str()).unwrap_or("");
        let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("");
        let base_url = val.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
        writeln!(out, "{name:<32} {format:<12} {model:<32} {base_url}")?;
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
            alias: Vec::new(),
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
    fn set_with_aliases_writes_duplicate_entries() {
        let (scope, dir) = tmp_scope("set-aliases");
        write_values(&scope, "models: {}\n");
        let mut cmd = make_set("haiku-4-5", "anthropic", Some("my-key"), None, None, None);
        cmd.alias = vec!["smart".into(), "default".into()];
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let canonical = &root["models"]["anthropic.haiku-4-5"];
        let alias_smart = &root["models"]["smart"];
        let alias_default = &root["models"]["default"];
        assert!(canonical.is_mapping());
        assert!(alias_smart.is_mapping());
        assert!(alias_default.is_mapping());
        assert_eq!(canonical["model"], alias_smart["model"]);
        assert_eq!(canonical["baseUrl"], alias_default["baseUrl"]);
        assert_eq!(canonical["secret"], alias_smart["secret"]);
        cleanup(&dir);
    }

    #[test]
    fn delete_alias_leaves_canonical_intact() {
        let (scope, dir) = tmp_scope("delete-alias");
        write_values(&scope, "models: {}\n");
        let mut cmd = make_set("haiku", "anthropic", Some("my-key"), None, None, None);
        cmd.alias = vec!["default".into()];
        do_set(&scope, cmd).unwrap();
        do_delete(&scope, "default").unwrap();
        let root = read_values(&scope);
        assert!(root["models"]["anthropic.haiku"].is_mapping());
        assert!(root["models"].get("default").is_none());
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

    #[test]
    fn render_model_list_empty_says_none_configured() {
        // Catches `match guard !m.is_empty()` mutations on do_list.
        let mapping = serde_yaml::Mapping::new();
        let mut out = Vec::new();
        render_model_list(Some(&mapping), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("No models configured"));
    }

    #[test]
    fn render_model_list_none_says_none_configured() {
        let mut out = Vec::new();
        render_model_list(None, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("No models configured"));
    }

    #[test]
    fn render_model_list_with_entries_prints_them() {
        let mut mapping = serde_yaml::Mapping::new();
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            Value::String("format".into()),
            Value::String("anthropic".into()),
        );
        entry.insert(Value::String("model".into()), Value::String("haiku".into()));
        entry.insert(
            Value::String("baseUrl".into()),
            Value::String("https://api.anthropic.com/v1".into()),
        );
        mapping.insert(
            Value::String("anthropic.haiku".into()),
            Value::Mapping(entry),
        );

        let mut out = Vec::new();
        render_model_list(Some(&mapping), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("KEY"));
        assert!(s.contains("anthropic.haiku"));
        assert!(s.contains("anthropic"));
        assert!(s.contains("haiku"));
        assert!(s.contains("https://api.anthropic.com/v1"));
        assert!(!s.contains("No models configured"));
    }

    #[test]
    fn model_list_data_returns_empty_for_none() {
        assert_eq!(model_list_data(None), Vec::<ModelEntry>::new());
    }

    #[test]
    fn model_list_data_returns_empty_for_empty_mapping() {
        let mapping = serde_yaml::Mapping::new();
        assert_eq!(model_list_data(Some(&mapping)), Vec::<ModelEntry>::new());
    }

    #[test]
    fn model_list_data_extracts_fields() {
        let mut mapping = serde_yaml::Mapping::new();
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            Value::String("format".into()),
            Value::String("anthropic".into()),
        );
        entry.insert(Value::String("model".into()), Value::String("haiku".into()));
        entry.insert(
            Value::String("baseUrl".into()),
            Value::String("https://api.anthropic.com/v1".into()),
        );
        mapping.insert(
            Value::String("anthropic.haiku".into()),
            Value::Mapping(entry),
        );

        let entries = model_list_data(Some(&mapping));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "anthropic.haiku");
        assert_eq!(entries[0].format, "anthropic");
        assert_eq!(entries[0].model, "haiku");
        assert_eq!(entries[0].base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn model_list_data_substitutes_empty_strings_for_missing_fields() {
        let mut mapping = serde_yaml::Mapping::new();
        let entry = serde_yaml::Mapping::new();
        mapping.insert(Value::String("partial".into()), Value::Mapping(entry));
        let entries = model_list_data(Some(&mapping));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "partial");
        assert_eq!(entries[0].format, "");
        assert_eq!(entries[0].model, "");
        assert_eq!(entries[0].base_url, "");
    }

    #[test]
    fn model_entry_serializes_to_camel_case_json() {
        let entry = ModelEntry {
            key: "anthropic.haiku".into(),
            format: "anthropic".into(),
            model: "haiku".into(),
            base_url: "https://api.anthropic.com/v1".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"baseUrl\":\"https://api.anthropic.com/v1\""));
        assert!(!json.contains("base_url"));
        assert!(json.contains("\"key\":\"anthropic.haiku\""));
    }

    #[test]
    fn model_list_data_preserves_yaml_insertion_order() {
        let mut mapping = serde_yaml::Mapping::new();
        for k in ["zeta", "alpha", "beta"] {
            let entry = serde_yaml::Mapping::new();
            mapping.insert(Value::String(k.into()), Value::Mapping(entry));
        }
        let entries = model_list_data(Some(&mapping));
        let keys: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["zeta", "alpha", "beta"]);
    }
}
