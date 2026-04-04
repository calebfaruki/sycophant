use serde_yaml::Value;

use crate::cli::{AgentCmd, AgentSet, AgentSub};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: AgentCmd) -> Result<(), String> {
    match cmd.sub {
        AgentSub::Set(set) => do_set(scope, set),
        AgentSub::List(_) => do_list(scope),
    }
}

fn do_set(scope: &Scope, cmd: AgentSet) -> Result<(), String> {
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;
    let agents = values::ensure_map(&mut root, "agents");

    let mut entry = serde_yaml::Mapping::new();
    entry.insert(Value::String("model".into()), Value::String(cmd.model));

    let mut prompt = serde_yaml::Mapping::new();
    prompt.insert(Value::String("path".into()), Value::String(cmd.prompt));
    entry.insert(Value::String("prompt".into()), Value::Mapping(prompt));

    if let Some(desc) = cmd.description {
        entry.insert(Value::String("description".into()), Value::String(desc));
    }

    agents.insert(Value::String(cmd.name.clone()), Value::Mapping(entry));

    values::save(&values_path, &root)?;
    eprintln!("Agent '{}' configured.", cmd.name);
    Ok(())
}

fn do_list(scope: &Scope) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;

    let agents = match root.get("agents").and_then(|v| v.as_mapping()) {
        Some(a) if !a.is_empty() => a,
        _ => {
            eprintln!("No agents configured.");
            return Ok(());
        }
    };

    eprintln!(
        "{:<16} {:<16} {:<32} DESCRIPTION",
        "NAME", "MODEL", "PROMPT"
    );
    for (key, val) in agents {
        let name = key.as_str().unwrap_or("");
        let model = val.get("model").and_then(|v| v.as_str()).unwrap_or("");
        let prompt_path = val
            .get("prompt")
            .and_then(|v| v.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = val
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        eprintln!("{name:<16} {model:<16} {prompt_path:<32} {description}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_scope(name: &str) -> (Scope, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("syco-agent-{}-{}", name, std::process::id()));
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

    fn make_set(name: &str, model: &str, prompt: &str, description: Option<&str>) -> AgentSet {
        AgentSet {
            name: name.into(),
            model: model.into(),
            prompt: prompt.into(),
            description: description.map(String::from),
        }
    }

    #[test]
    fn set_creates_agent_with_required_fields() {
        let (scope, dir) = tmp_scope("set-required");
        write_values(&scope, "agents: {}\n");
        let cmd = make_set("coder", "haiku", "./prompts/coder", None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let a = &root["agents"]["coder"];
        assert_eq!(a["model"].as_str().unwrap(), "haiku");
        assert_eq!(a["prompt"]["path"].as_str().unwrap(), "./prompts/coder");
        assert!(a.get("description").is_none());
        cleanup(&dir);
    }

    #[test]
    fn set_with_description() {
        let (scope, dir) = tmp_scope("set-desc");
        write_values(&scope, "agents: {}\n");
        let cmd = make_set("coder", "haiku", "./prompts/coder", Some("Writes code"));
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["agents"]["coder"]["description"].as_str().unwrap(),
            "Writes code"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_overwrites_existing_agent() {
        let (scope, dir) = tmp_scope("set-overwrite");
        write_values(
            &scope,
            "agents:\n  coder:\n    model: old\n    prompt:\n      path: ./old\n",
        );
        let cmd = make_set("coder", "new-model", "./new-prompts", None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["agents"]["coder"]["model"].as_str().unwrap(),
            "new-model"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_preserves_other_agents() {
        let (scope, dir) = tmp_scope("set-preserve");
        write_values(
            &scope,
            "agents:\n  existing:\n    model: haiku\n    prompt:\n      path: ./existing\n",
        );
        let cmd = make_set("new-agent", "sonnet", "./prompts/new", None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["agents"]["existing"]["model"].as_str().unwrap(),
            "haiku"
        );
        assert_eq!(
            root["agents"]["new-agent"]["model"].as_str().unwrap(),
            "sonnet"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_creates_agents_map_if_missing() {
        let (scope, dir) = tmp_scope("set-no-map");
        write_values(&scope, "models: {}\n");
        let cmd = make_set("coder", "haiku", "./prompts/coder", None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert!(root["agents"]["coder"].is_mapping());
        cleanup(&dir);
    }

    #[test]
    fn set_preserves_other_top_level_keys() {
        let (scope, dir) = tmp_scope("set-preserve-keys");
        write_values(
            &scope,
            "agents: {}\nmodels:\n  haiku:\n    format: anthropic\n",
        );
        let cmd = make_set("coder", "haiku", "./prompts/coder", None);
        do_set(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(
            root["models"]["haiku"]["format"].as_str().unwrap(),
            "anthropic"
        );
        cleanup(&dir);
    }

    #[test]
    fn set_values_file_missing_errors() {
        let (scope, dir) = tmp_scope("set-no-file");
        let cmd = make_set("coder", "haiku", "./prompts/coder", None);
        let err = do_set(&scope, cmd).unwrap_err();
        assert!(err.contains("failed to read"));
        cleanup(&dir);
    }

    #[test]
    fn list_no_agents() {
        let (scope, dir) = tmp_scope("list-empty");
        write_values(&scope, "agents: {}\n");
        do_list(&scope).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_with_agents() {
        let (scope, dir) = tmp_scope("list-agents");
        write_values(
            &scope,
            "agents:\n  coder:\n    model: haiku\n    prompt:\n      path: ./prompts/coder\n    description: Writes code\n",
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
    fn list_agents_key_missing_shows_none() {
        let (scope, dir) = tmp_scope("list-no-key");
        write_values(&scope, "models: {}\n");
        do_list(&scope).unwrap();
        cleanup(&dir);
    }
}
