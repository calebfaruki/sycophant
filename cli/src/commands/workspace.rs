use std::fs;

use super::util::parse_flag;
use crate::runner::{run_check, run_output, run_passthrough, run_stdin};
use crate::scope::Scope;
use crate::sync::sycophant_releases;

const PROMPT_CM_PREFIX: &str = "sycophant-prompt-";
const PROMPT_SET_USAGE: &str =
    "usage: syco workspace prompt set <workspace> <agent-name> <path> [--description \"...\"]";

pub(crate) fn run(scope: &Scope, args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        Some("set") => {
            let name = args
                .get(1)
                .ok_or("usage: syco workspace set <name> --image <image:tag>")?;
            workspace_set(name, &args[2..])
        }
        Some("llm") => match args.get(1).map(|s| s.as_str()) {
            Some("set") => {
                let workspace = args
                    .get(2)
                    .ok_or("usage: syco workspace llm set <workspace> <llm-name>")?;
                let llm_name = args
                    .get(3)
                    .ok_or("usage: syco workspace llm set <workspace> <llm-name>")?;
                llm_set(workspace, llm_name)
            }
            _ => Err("usage: syco workspace llm set <workspace> <llm-name>".into()),
        },
        Some("mcp") => match args.get(1).map(|s| s.as_str()) {
            Some("set") => {
                let workspace = args
                    .get(2)
                    .ok_or("usage: syco workspace mcp set <workspace> <mcp1,mcp2,...>")?;
                let mcp_list = args
                    .get(3)
                    .ok_or("usage: syco workspace mcp set <workspace> <mcp1,mcp2,...>")?;
                mcp_set(workspace, mcp_list)
            }
            _ => Err("usage: syco workspace mcp set <workspace> <mcp1,mcp2,...>".into()),
        },
        Some("tools") => match args.get(1).map(|s| s.as_str()) {
            Some("set") => {
                let workspace = args
                    .get(2)
                    .ok_or("usage: syco workspace tools set <workspace> <tool1,tool2,...>")?;
                let tools_list = args
                    .get(3)
                    .ok_or("usage: syco workspace tools set <workspace> <tool1,tool2,...>")?;
                tools_set(workspace, tools_list)
            }
            _ => Err("usage: syco workspace tools set <workspace> <tool1,tool2,...>".into()),
        },
        Some("up") => {
            let workspace = args.get(1).ok_or("usage: syco workspace up <workspace>")?;
            workspace_up(scope, workspace)
        }
        Some("down") => {
            let workspace = args
                .get(1)
                .ok_or("usage: syco workspace down <workspace>")?;
            workspace_down(workspace)
        }
        Some("list") => {
            workspace_list();
            Ok(())
        }
        Some("prompt") => match args.get(1).map(|s| s.as_str()) {
            Some("set") => {
                let workspace = args.get(2).ok_or(PROMPT_SET_USAGE)?;
                let agent_name = args.get(3).ok_or(PROMPT_SET_USAGE)?;
                let path = args.get(4).ok_or(PROMPT_SET_USAGE)?;
                prompt_set(workspace, agent_name, path, &args[2..])
            }
            Some("list") => {
                let workspace = args
                    .get(2)
                    .ok_or("usage: syco workspace agent list <workspace>")?;
                prompt_list(workspace)
            }
            Some("delete") => {
                let workspace = args
                    .get(2)
                    .ok_or("usage: syco workspace agent delete <workspace> <agent-name>")?;
                let agent_name = args
                    .get(3)
                    .ok_or("usage: syco workspace agent delete <workspace> <agent-name>")?;
                prompt_delete(workspace, agent_name)
            }
            _ => Err("usage: syco workspace agent <set|list|delete>".into()),
        },
        _ => Err("usage: syco workspace <set|llm|mcp|tools|up|down|list|agent>".into()),
    }
}

// --- workspace config ---

fn workspace_set(name: &str, args: &[String]) -> Result<(), String> {
    let image_flag = parse_flag(args, "--image").ok_or("--image is required")?;
    let (image, tag) = match image_flag.rsplit_once(':') {
        Some((img, t)) => (img, t),
        None => (image_flag, "latest"),
    };

    let yaml = format!(
        r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: sycophant-workspace-{name}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.io/type: workspace
data:
  image: {image}
  tag: {tag}
"#
    );

    run_stdin("kubectl", &["apply", "-f", "-"], &yaml)?;
    eprintln!("Workspace '{name}' configured ({image}:{tag}).");
    Ok(())
}

fn patch_workspace(workspace: &str, key: &str, value: &str) -> Result<(), String> {
    let cm_name = format!("sycophant-workspace-{workspace}");
    let patch = format!(r#"{{"data":{{"{key}":"{value}"}}}}"#);
    run_check("kubectl", &["patch", "configmap", &cm_name, "-p", &patch])?;
    Ok(())
}

fn llm_set(workspace: &str, llm_name: &str) -> Result<(), String> {
    patch_workspace(workspace, "llm", llm_name)?;
    eprintln!("LLM for workspace '{workspace}' set to '{llm_name}'.");
    Ok(())
}

fn mcp_set(workspace: &str, mcp_list: &str) -> Result<(), String> {
    patch_workspace(workspace, "mcp", mcp_list)?;
    eprintln!("MCP servers for workspace '{workspace}' set to '{mcp_list}'.");
    Ok(())
}

fn tools_set(workspace: &str, tools_list: &str) -> Result<(), String> {
    patch_workspace(workspace, "tools", tools_list)?;
    eprintln!("Tools for workspace '{workspace}' set to '{tools_list}'.");
    Ok(())
}

// --- workspace lifecycle ---

fn workspace_up(scope: &Scope, workspace: &str) -> Result<(), String> {
    let ws_output = run_output(
        "kubectl",
        &[
            "get",
            "configmap",
            &format!("sycophant-workspace-{workspace}"),
            "-o",
            "json",
        ],
    )?;
    let ws_json: serde_json::Value =
        serde_json::from_str(&ws_output).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let agents_label = format!("sycophant.io/type=prompt,sycophant.io/workspace={workspace}");
    let agents_output = run_output(
        "kubectl",
        &["get", "configmaps", "-l", &agents_label, "-o", "json"],
    )?;
    let agents_json: serde_json::Value =
        serde_json::from_str(&agents_output).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let agents: Vec<(String, String)> = agents_json["items"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|item| {
            let prefix = PROMPT_CM_PREFIX;
            let full_name = item["metadata"]["name"].as_str().unwrap_or("");
            let name = full_name.strip_prefix(prefix).unwrap_or(full_name);
            let desc = item["metadata"]["annotations"]["sycophant.io/description"]
                .as_str()
                .unwrap_or("");
            (name.to_string(), desc.to_string())
        })
        .collect();

    let values = build_workspace_values(workspace, &ws_json, &agents)?;

    let tmp = std::env::temp_dir().join(format!("syco-{workspace}-values.yaml"));
    let tmp_str = tmp.to_string_lossy().to_string();
    fs::write(&tmp, &values).map_err(|e| format!("failed to write temp values: {e}"))?;

    let chart_dir = scope.charts_dir();
    let chart_str = chart_dir.to_string_lossy().to_string();

    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            workspace,
            &chart_str,
            "-f",
            &tmp_str,
        ],
    )?;

    Ok(())
}

fn build_workspace_values(
    workspace: &str,
    ws_json: &serde_json::Value,
    agents: &[(String, String)],
) -> Result<String, String> {
    let data = &ws_json["data"];
    let image = data["image"]
        .as_str()
        .ok_or("workspace ConfigMap missing 'image'")?;
    let tag = data["tag"].as_str().unwrap_or("latest");
    let llm = data["llm"].as_str().ok_or(format!(
        "LLM not set. Run: syco workspace llm set {workspace} <llm-name>"
    ))?;

    if agents.is_empty() {
        return Err(format!(
            "No agents configured. Run: syco workspace agent set {workspace} <name> <path>"
        ));
    }

    let tools = data["tools"].as_str().unwrap_or("");
    let mcp_str = data["mcp"].as_str().unwrap_or("");

    let mut yaml = format!(
        r#"workspaces:
  {workspace}:
    image: {image}
    tag: {tag}
    llm: {llm}
"#
    );

    if !tools.is_empty() {
        yaml.push_str(&format!("    tools: \"{tools}\"\n"));
    }

    if !mcp_str.is_empty() {
        yaml.push_str("    mcp:\n");
        for mcp in mcp_str.split(',') {
            let mcp = mcp.trim();
            if !mcp.is_empty() {
                yaml.push_str(&format!("      - {mcp}\n"));
            }
        }
    }

    yaml.push_str("    agents:\n");
    for (name, desc) in agents {
        if desc.is_empty() {
            yaml.push_str(&format!("      - name: {name}\n"));
        } else {
            yaml.push_str(&format!(
                "      - name: {name}\n        description: \"{desc}\"\n"
            ));
        }
    }

    Ok(yaml)
}

fn workspace_down(workspace: &str) -> Result<(), String> {
    match run_check("helm", &["uninstall", workspace]) {
        Ok(()) => {
            eprintln!("Workspace '{workspace}' stopped.");
            Ok(())
        }
        Err(e) if e.contains("not found") => {
            eprintln!("Workspace '{workspace}' is not running.");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn workspace_list() {
    let releases = sycophant_releases();
    if releases.is_empty() {
        eprintln!("No workspaces running.");
    } else {
        eprintln!("NAME");
        for name in &releases {
            eprintln!("{name}");
        }
    }
}

// --- agent commands ---

fn prompt_set(
    workspace: &str,
    agent_name: &str,
    path: &str,
    args: &[String],
) -> Result<(), String> {
    let description = parse_flag(args, "--description").unwrap_or(agent_name);
    let yaml = build_prompt_yaml(workspace, agent_name, path, description)?;
    run_stdin("kubectl", &["apply", "-f", "-"], &yaml)?;
    eprintln!("Prompt '{agent_name}' configured for workspace '{workspace}'.");
    Ok(())
}

fn build_prompt_yaml(
    workspace: &str,
    agent_name: &str,
    path: &str,
    description: &str,
) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
    if !meta.is_dir() {
        return Err(format!("'{path}' is not a directory"));
    }

    let entries: Vec<_> = fs::read_dir(path)
        .map_err(|e| format!("cannot read directory '{path}': {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("error reading directory '{path}': {e}"))?;

    let mut md_files: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.ends_with(".md") {
            return Err(format!(
                "'{file_name}' is not a .md file. Prompt directories must contain only .md files."
            ));
        }
        let content = fs::read_to_string(entry.path())
            .map_err(|e| format!("failed to read '{file_name}': {e}"))?;
        md_files.push((file_name, content));
    }

    if md_files.is_empty() {
        return Err(format!(
            "'{path}' contains no .md files. At least one is required."
        ));
    }

    md_files.sort_by(|a, b| a.0.cmp(&b.0));

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
  name: sycophant-prompt-{agent_name}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.io/type: prompt
    sycophant.io/workspace: {workspace}
  annotations:
    sycophant.io/description: "{description}"
data:
{data_section}"#
    ))
}

fn prompt_list(workspace: &str) -> Result<(), String> {
    let label = format!("sycophant.io/type=prompt,sycophant.io/workspace={workspace}");
    let output = run_output(
        "kubectl",
        &["get", "configmaps", "-l", &label, "-o", "json"],
    )?;

    let json: serde_json::Value =
        serde_json::from_str(&output).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let prefix = PROMPT_CM_PREFIX;
    let items = json["items"].as_array();
    match items {
        Some(items) if !items.is_empty() => {
            eprintln!("{:<20} DESCRIPTION", "NAME");
            for item in items {
                let full_name = item["metadata"]["name"].as_str().unwrap_or("");
                let name = full_name.strip_prefix(prefix).unwrap_or(full_name);
                let description = item["metadata"]["annotations"]["sycophant.io/description"]
                    .as_str()
                    .unwrap_or("");
                eprintln!("{name:<20} {description}");
            }
        }
        _ => eprintln!("No prompts configured for workspace '{workspace}'."),
    }

    Ok(())
}

fn prompt_delete(workspace: &str, agent_name: &str) -> Result<(), String> {
    let cm_name = format!("sycophant-prompt-{agent_name}");
    match run_check("kubectl", &["delete", "configmap", &cm_name]) {
        Ok(()) => {
            eprintln!("Prompt '{agent_name}' deleted from workspace '{workspace}'.");
            Ok(())
        }
        Err(e) if e.contains("NotFound") || e.contains("not found") => {
            eprintln!("Prompt '{agent_name}' not found in workspace '{workspace}'.");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn make_temp_dir() -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("syco-test-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // --- prompt yaml tests ---

    #[test]
    fn build_yaml_valid_single_file() {
        let dir = make_temp_dir();
        fs::write(dir.join("identity.md"), "You are a researcher.\n").unwrap();
        let yaml =
            build_prompt_yaml("dev", "research", dir.to_str().unwrap(), "Research agent").unwrap();
        assert!(yaml.contains("name: sycophant-prompt-research"));
        assert!(yaml.contains("sycophant.io/workspace: dev"));
        assert!(yaml.contains("sycophant.io/description: \"Research agent\""));
        assert!(yaml.contains("identity.md: |"));
        assert!(yaml.contains("    You are a researcher."));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_yaml_rejects_non_md_file() {
        let dir = make_temp_dir();
        fs::write(dir.join("identity.md"), "ok").unwrap();
        fs::write(dir.join("notes.txt"), "bad").unwrap();
        let err = build_prompt_yaml("dev", "research", dir.to_str().unwrap(), "desc").unwrap_err();
        assert!(err.contains("not a .md file"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_yaml_rejects_empty_dir() {
        let dir = make_temp_dir();
        let err = build_prompt_yaml("dev", "research", dir.to_str().unwrap(), "desc").unwrap_err();
        assert!(err.contains("no .md files"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_yaml_rejects_file_not_dir() {
        let dir = make_temp_dir();
        let file_path = dir.join("not-a-dir.md");
        fs::write(&file_path, "content").unwrap();
        let err =
            build_prompt_yaml("dev", "research", file_path.to_str().unwrap(), "desc").unwrap_err();
        assert!(err.contains("is not a directory"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_yaml_sorts_files() {
        let dir = make_temp_dir();
        fs::write(dir.join("z-last.md"), "last").unwrap();
        fs::write(dir.join("a-first.md"), "first").unwrap();
        let yaml = build_prompt_yaml("dev", "test", dir.to_str().unwrap(), "desc").unwrap();
        let a_pos = yaml.find("a-first.md").unwrap();
        let z_pos = yaml.find("z-last.md").unwrap();
        assert!(a_pos < z_pos);
        fs::remove_dir_all(&dir).unwrap();
    }

    // --- workspace values tests ---

    fn mock_ws_json(
        image: &str,
        tag: &str,
        llm: Option<&str>,
        tools: Option<&str>,
        mcp: Option<&str>,
    ) -> serde_json::Value {
        let mut data = serde_json::Map::new();
        data.insert("image".into(), serde_json::Value::String(image.into()));
        data.insert("tag".into(), serde_json::Value::String(tag.into()));
        if let Some(l) = llm {
            data.insert("llm".into(), serde_json::Value::String(l.into()));
        }
        if let Some(t) = tools {
            data.insert("tools".into(), serde_json::Value::String(t.into()));
        }
        if let Some(m) = mcp {
            data.insert("mcp".into(), serde_json::Value::String(m.into()));
        }
        serde_json::json!({ "data": data })
    }

    #[test]
    fn workspace_values_valid() {
        let ws = mock_ws_json(
            "my-img",
            "v1",
            Some("anthropic"),
            Some("bash,read_file"),
            Some("github"),
        );
        let agents = vec![
            ("research".to_string(), "Investigates topics".to_string()),
            ("writer".to_string(), "Drafts docs".to_string()),
        ];
        let yaml = build_workspace_values("dev", &ws, &agents).unwrap();
        assert!(yaml.contains("image: my-img"));
        assert!(yaml.contains("tag: v1"));
        assert!(yaml.contains("llm: anthropic"));
        assert!(yaml.contains("tools: \"bash,read_file\""));
        assert!(yaml.contains("- github"));
        assert!(yaml.contains("name: research"));
        assert!(yaml.contains("description: \"Investigates topics\""));
        assert!(yaml.contains("name: writer"));
    }

    #[test]
    fn workspace_values_missing_llm() {
        let ws = mock_ws_json("my-img", "v1", None, None, None);
        let agents = vec![("research".to_string(), "desc".to_string())];
        let err = build_workspace_values("dev", &ws, &agents).unwrap_err();
        assert!(err.contains("LLM not set"));
    }

    #[test]
    fn workspace_values_no_agents() {
        let ws = mock_ws_json("my-img", "v1", Some("anthropic"), None, None);
        let agents: Vec<(String, String)> = vec![];
        let err = build_workspace_values("dev", &ws, &agents).unwrap_err();
        assert!(err.contains("No agents configured"));
    }

    #[test]
    fn workspace_values_no_optional_fields() {
        let ws = mock_ws_json("my-img", "latest", Some("anthropic"), None, None);
        let agents = vec![("hello".to_string(), "".to_string())];
        let yaml = build_workspace_values("dev", &ws, &agents).unwrap();
        assert!(!yaml.contains("tools:"));
        assert!(!yaml.contains("mcp:"));
        assert!(yaml.contains("name: hello"));
        assert!(!yaml.contains("description:"));
    }
}
