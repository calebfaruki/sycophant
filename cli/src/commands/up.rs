use std::fs;

use crate::runner::{run_passthrough, run_silent, run_stdin};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let release = scope.release_name()?;
    let chart_dir = scope.charts_dir();
    let values_file = scope.values_file();

    if !values_file.exists() {
        return Err(format!(
            "values.yaml not found at {}",
            values_file.display()
        ));
    }

    let root = values::load(&values_file)?;
    validate(&root)?;
    apply_prompt_configmaps(&root, &release)?;

    let chart_str = chart_dir.to_string_lossy().to_string();
    let values_str = values_file.to_string_lossy().to_string();

    eprintln!("Deploying {release}...");
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            &release,
            &chart_str,
            "-n",
            &release,
            "--create-namespace",
            "-f",
            &values_str,
        ],
    )
}

fn validate(root: &serde_yaml::Value) -> Result<(), String> {
    let models = root.get("models").and_then(|v| v.as_mapping());
    if models.is_none_or(|m| m.is_empty()) {
        return Err(
            "No models configured. Run: syco model set <model> --provider <provider> --secret <secret>"
                .into(),
        );
    }
    let model_keys: Vec<String> = models
        .unwrap()
        .keys()
        .filter_map(|k| k.as_str().map(String::from))
        .collect();

    if let Some(agents) = root.get("agents").and_then(|v| v.as_mapping()) {
        for (agent_key, agent_val) in agents {
            let agent_name = agent_key.as_str().unwrap_or("");
            let model_ref = agent_val
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !model_keys.iter().any(|k| k == model_ref) {
                return Err(format!(
                    "Agent \"{agent_name}\" references model \"{model_ref}\" which does not exist."
                ));
            }

            let prompt_path = agent_val
                .get("prompt")
                .and_then(|v| v.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !prompt_path.is_empty() && !std::path::Path::new(prompt_path).is_dir() {
                return Err(format!(
                    "Agent \"{agent_name}\" prompt path \"{prompt_path}\" does not exist."
                ));
            }
        }
    }

    if let Some(workspaces) = root.get("workspaces").and_then(|v| v.as_mapping()) {
        let agent_keys: Vec<String> = root
            .get("agents")
            .and_then(|v| v.as_mapping())
            .map(|m| {
                m.keys()
                    .filter_map(|k| k.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        for (ws_key, ws_val) in workspaces {
            let ws_name = ws_key.as_str().unwrap_or("");
            if let Some(agents) = ws_val.get("agents").and_then(|v| v.as_sequence()) {
                for agent_val in agents {
                    let agent_name = agent_val.as_str().unwrap_or("");
                    if !agent_keys.iter().any(|k| k == agent_name) {
                        return Err(format!(
                            "Workspace \"{ws_name}\" references agent \"{agent_name}\" which does not exist."
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

fn apply_prompt_configmaps(root: &serde_yaml::Value, namespace: &str) -> Result<(), String> {
    let agents = match root.get("agents").and_then(|v| v.as_mapping()) {
        Some(a) if !a.is_empty() => a,
        _ => return Ok(()),
    };

    // Ensure namespace exists (ignore failure — already exists is fine)
    run_silent("kubectl", &["create", "namespace", namespace]);

    for (key, val) in agents {
        let name = key.as_str().unwrap_or("");
        if name.is_empty() {
            continue;
        }

        let prompt_path = match val
            .get("prompt")
            .and_then(|v| v.get("path"))
            .and_then(|v| v.as_str())
        {
            Some(p) => p,
            None => continue,
        };

        let cm_name = format!("sycophant-prompt-{name}");
        let yaml = build_configmap_yaml(&cm_name, namespace, prompt_path)?;
        run_stdin("kubectl", &["apply", "-n", namespace, "-f", "-"], &yaml)?;
        eprintln!("Prompt '{name}' applied.");
    }

    Ok(())
}

fn build_configmap_yaml(
    cm_name: &str,
    namespace: &str,
    prompt_path: &str,
) -> Result<String, String> {
    let meta = fs::metadata(prompt_path)
        .map_err(|e| format!("cannot read prompt path '{prompt_path}': {e}"))?;
    if !meta.is_dir() {
        return Err(format!("prompt path '{prompt_path}' is not a directory"));
    }

    let mut entries: Vec<_> = fs::read_dir(prompt_path)
        .map_err(|e| format!("cannot read directory '{prompt_path}': {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("error reading directory '{prompt_path}': {e}"))?;

    entries.sort_by_key(|e| e.file_name());

    let mut md_files: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.ends_with(".md") {
            return Err(format!(
                "'{file_name}' in '{prompt_path}' is not a .md file. Prompt directories must contain only .md files."
            ));
        }
        let content = fs::read_to_string(entry.path())
            .map_err(|e| format!("failed to read '{file_name}': {e}"))?;
        md_files.push((file_name, content));
    }

    if md_files.is_empty() {
        return Err(format!("'{prompt_path}' contains no .md files."));
    }

    let mut data_section = String::new();
    for (filename, content) in &md_files {
        data_section.push_str(&format!("  {filename}: |\n"));
        for line in content.lines() {
            if line.is_empty() {
                data_section.push('\n');
            } else {
                data_section.push_str(&format!("    {line}\n"));
            }
        }
    }

    Ok(format!(
        r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: {cm_name}
  namespace: {namespace}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.io/type: prompt
data:
{data_section}"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("syco-up-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn build_configmap_single_file() {
        let dir = make_temp_dir("cm-single");
        fs::write(dir.join("identity.md"), "You are a helper.\n").unwrap();
        let yaml = build_configmap_yaml("sycophant-prompt-coder", "default", dir.to_str().unwrap())
            .unwrap();
        assert!(yaml.contains("name: sycophant-prompt-coder"));
        assert!(yaml.contains("namespace: default"));
        assert!(yaml.contains("identity.md: |"));
        assert!(yaml.contains("    You are a helper."));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_configmap_multiple_files_sorted() {
        let dir = make_temp_dir("cm-multi");
        fs::write(dir.join("z-last.md"), "last").unwrap();
        fs::write(dir.join("a-first.md"), "first").unwrap();
        let yaml = build_configmap_yaml("test", "ns", dir.to_str().unwrap()).unwrap();
        let a_pos = yaml.find("a-first.md").unwrap();
        let z_pos = yaml.find("z-last.md").unwrap();
        assert!(a_pos < z_pos);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_configmap_rejects_non_md() {
        let dir = make_temp_dir("cm-non-md");
        fs::write(dir.join("prompt.md"), "ok").unwrap();
        fs::write(dir.join("notes.txt"), "bad").unwrap();
        let err = build_configmap_yaml("test", "ns", dir.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a .md file"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_configmap_rejects_empty_dir() {
        let dir = make_temp_dir("cm-empty");
        let err = build_configmap_yaml("test", "ns", dir.to_str().unwrap()).unwrap_err();
        assert!(err.contains("no .md files"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_configmap_rejects_file_not_dir() {
        let dir = make_temp_dir("cm-file");
        let file = dir.join("not-a-dir.md");
        fs::write(&file, "content").unwrap();
        let err = build_configmap_yaml("test", "ns", file.to_str().unwrap()).unwrap_err();
        assert!(err.contains("is not a directory"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_configmap_rejects_missing_path() {
        let err = build_configmap_yaml("test", "ns", "/nonexistent/path").unwrap_err();
        assert!(err.contains("cannot read"));
    }

    #[test]
    fn build_configmap_has_labels() {
        let dir = make_temp_dir("cm-labels");
        fs::write(dir.join("prompt.md"), "content").unwrap();
        let yaml = build_configmap_yaml("test", "ns", dir.to_str().unwrap()).unwrap();
        assert!(yaml.contains("app.kubernetes.io/part-of: sycophant"));
        assert!(yaml.contains("sycophant.io/type: prompt"));
        fs::remove_dir_all(&dir).unwrap();
    }

    // -- validate --

    #[test]
    fn validate_no_models_errors() {
        let root: serde_yaml::Value = serde_yaml::from_str("models: {}\n").unwrap();
        let err = validate(&root).unwrap_err();
        assert!(err.contains("No models configured"));
    }

    #[test]
    fn validate_agent_references_missing_model() {
        let root: serde_yaml::Value = serde_yaml::from_str(
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\nagents:\n  hello:\n    model: nonexistent\n    prompt:\n      path: .\n",
        )
        .unwrap();
        let err = validate(&root).unwrap_err();
        assert!(err.contains("references model"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn validate_workspace_references_missing_agent() {
        let root: serde_yaml::Value = serde_yaml::from_str(
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\nworkspaces:\n  dev:\n    agents:\n      - nonexistent\n",
        )
        .unwrap();
        let err = validate(&root).unwrap_err();
        assert!(err.contains("references agent"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn validate_valid_config_passes() {
        let dir = make_temp_dir("validate-ok");
        let prompt_dir = dir.join("prompt");
        fs::create_dir(&prompt_dir).unwrap();
        fs::write(prompt_dir.join("prompt.md"), "hello").unwrap();
        let yaml = format!(
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\nagents:\n  hello:\n    model: anthropic.haiku\n    prompt:\n      path: {}\nworkspaces:\n  dev:\n    agents:\n      - hello\n",
            prompt_dir.display()
        );
        let root: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        validate(&root).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_agent_prompt_path_missing() {
        let root: serde_yaml::Value = serde_yaml::from_str(
            "models:\n  anthropic.haiku:\n    format: anthropic\n    model: haiku\n    baseUrl: http://x\nagents:\n  hello:\n    model: anthropic.haiku\n    prompt:\n      path: /nonexistent/path\n",
        )
        .unwrap();
        let err = validate(&root).unwrap_err();
        assert!(err.contains("prompt path"));
        assert!(err.contains("does not exist"));
    }
}
