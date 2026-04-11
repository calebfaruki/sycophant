use serde_yaml::Value;

use crate::cli::{WorkspaceCmd, WorkspaceCreate, WorkspaceSub};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: WorkspaceCmd) -> Result<(), String> {
    match cmd.sub {
        WorkspaceSub::Create(create) => do_create(scope, create),
        WorkspaceSub::List(_) => do_list(scope),
        WorkspaceSub::Show(show) => do_show(scope, &show.name),
    }
}

const DEFAULT_IMAGE: &str = "sycophant-workspace-tools";
const DEFAULT_TAG: &str = "latest";

fn split_image_tag(input: &str) -> (&str, &str) {
    match input.rfind(':') {
        Some(pos) if pos > 0 && pos < input.len() - 1 && !input[pos + 1..].contains('/') => {
            (&input[..pos], &input[pos + 1..])
        }
        _ => (input, "latest"),
    }
}

fn do_create(scope: &Scope, cmd: WorkspaceCreate) -> Result<(), String> {
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;
    let workspaces = values::ensure_map(&mut root, "workspaces");

    let key = Value::String(cmd.name.clone());
    if workspaces.contains_key(&key) {
        return Err(format!("Workspace \"{}\" already exists.", cmd.name));
    }

    let (image, tag) = match &cmd.image {
        Some(img) => split_image_tag(img),
        None => (DEFAULT_IMAGE, DEFAULT_TAG),
    };

    let mut entry = serde_yaml::Mapping::new();
    entry.insert(Value::String("image".into()), Value::String(image.into()));
    entry.insert(Value::String("tag".into()), Value::String(tag.into()));
    entry.insert(Value::String("agents".into()), Value::Sequence(vec![]));

    workspaces.insert(key, Value::Mapping(entry));

    values::save(&values_path, &root)?;
    eprintln!("Created workspace \"{}\".", cmd.name);
    Ok(())
}

fn format_agents(val: &Value) -> String {
    match val.get("agents").and_then(|a| a.as_sequence()) {
        Some(seq) if !seq.is_empty() => seq
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => "-".into(),
    }
}

fn format_image(val: &Value) -> String {
    let image = val.get("image").and_then(|v| v.as_str()).unwrap_or("");
    let tag = val.get("tag").and_then(|v| v.as_str()).unwrap_or("latest");
    if image.is_empty() {
        return "-".into();
    }
    format!("{image}:{tag}")
}

fn do_list(scope: &Scope) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;

    let workspaces = match root.get("workspaces").and_then(|v| v.as_mapping()) {
        Some(m) if !m.is_empty() => m,
        _ => {
            eprintln!("No workspaces configured.");
            return Ok(());
        }
    };

    eprintln!("{:<16} {:<44} AGENTS", "NAME", "IMAGE");
    for (key, val) in workspaces {
        let name = key.as_str().unwrap_or("");
        let image = format_image(val);
        let agents = format_agents(val);
        eprintln!("{name:<16} {image:<44} {agents}");
    }
    Ok(())
}

fn do_show(scope: &Scope, name: &str) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;

    let workspaces = root
        .get("workspaces")
        .and_then(|v| v.as_mapping())
        .ok_or_else(|| format!("Workspace \"{name}\" not found."))?;

    let entry = workspaces
        .get(Value::String(name.into()))
        .ok_or_else(|| format!("Workspace \"{name}\" not found."))?;

    let image = format_image(entry);
    let agents = format_agents(entry);

    eprintln!("Name:         {name}");
    eprintln!("Image:        {image}");
    eprintln!("Agents:       {agents}");

    if let Some(router) = entry.get("routerModel").and_then(|v| v.as_str()) {
        eprintln!("Router model: {router}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_scope(name: &str) -> (Scope, PathBuf) {
        let dir = std::env::temp_dir().join(format!("syco-ws-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        (Scope { root: dir.clone() }, dir)
    }

    fn write_values(scope: &Scope, content: &str) {
        fs::write(scope.values_file(), content).unwrap();
    }

    fn read_values(scope: &Scope) -> Value {
        values::load(&scope.values_file()).unwrap()
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // -- split_image_tag --

    #[test]
    fn split_standard() {
        assert_eq!(split_image_tag("tools:v2"), ("tools", "v2"));
    }

    #[test]
    fn split_no_colon() {
        assert_eq!(split_image_tag("tools"), ("tools", "latest"));
    }

    #[test]
    fn split_registry_with_port() {
        assert_eq!(
            split_image_tag("registry:5000/tools:v3"),
            ("registry:5000/tools", "v3")
        );
    }

    #[test]
    fn split_registry_with_port_no_tag() {
        assert_eq!(
            split_image_tag("registry:5000/tools"),
            ("registry:5000/tools", "latest")
        );
    }

    #[test]
    fn split_trailing_colon() {
        assert_eq!(split_image_tag("tools:"), ("tools:", "latest"));
    }

    #[test]
    fn split_ghcr_no_tag() {
        assert_eq!(
            split_image_tag("ghcr.io/org/image"),
            ("ghcr.io/org/image", "latest")
        );
    }

    #[test]
    fn split_ghcr_with_tag() {
        assert_eq!(
            split_image_tag("ghcr.io/org/image:sha-abc123"),
            ("ghcr.io/org/image", "sha-abc123")
        );
    }

    // -- create --

    #[test]
    fn create_default_image() {
        let (scope, dir) = tmp_scope("create-default");
        write_values(&scope, "workspaces: {}\n");
        let cmd = WorkspaceCreate {
            name: "dev".into(),
            image: None,
        };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let ws = &root["workspaces"]["dev"];
        assert_eq!(ws["image"].as_str().unwrap(), "sycophant-workspace-tools");
        assert_eq!(ws["tag"].as_str().unwrap(), "latest");
        assert!(ws["agents"].as_sequence().unwrap().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn create_custom_image() {
        let (scope, dir) = tmp_scope("create-custom");
        write_values(&scope, "workspaces: {}\n");
        let cmd = WorkspaceCreate {
            name: "staging".into(),
            image: Some("custom-tools:v2".into()),
        };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let ws = &root["workspaces"]["staging"];
        assert_eq!(ws["image"].as_str().unwrap(), "custom-tools");
        assert_eq!(ws["tag"].as_str().unwrap(), "v2");
        cleanup(&dir);
    }

    #[test]
    fn create_image_no_tag_defaults_to_latest() {
        let (scope, dir) = tmp_scope("create-no-tag");
        write_values(&scope, "workspaces: {}\n");
        let cmd = WorkspaceCreate {
            name: "dev".into(),
            image: Some("my-tools".into()),
        };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert_eq!(root["workspaces"]["dev"]["tag"].as_str().unwrap(), "latest");
        cleanup(&dir);
    }

    #[test]
    fn create_duplicate_errors() {
        let (scope, dir) = tmp_scope("create-dup");
        write_values(
            &scope,
            "workspaces:\n  dev:\n    image: tools\n    tag: latest\n    agents: []\n",
        );
        let cmd = WorkspaceCreate {
            name: "dev".into(),
            image: None,
        };
        let err = do_create(&scope, cmd).unwrap_err();
        assert!(err.contains("already exists"));
        cleanup(&dir);
    }

    #[test]
    fn create_ensures_workspaces_key() {
        let (scope, dir) = tmp_scope("create-no-key");
        write_values(&scope, "models: {}\n");
        let cmd = WorkspaceCreate {
            name: "dev".into(),
            image: None,
        };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert!(root["workspaces"]["dev"].is_mapping());
        cleanup(&dir);
    }

    #[test]
    fn create_ghcr_image() {
        let (scope, dir) = tmp_scope("create-ghcr");
        write_values(&scope, "workspaces: {}\n");
        let cmd = WorkspaceCreate {
            name: "dev".into(),
            image: Some("ghcr.io/calebfaruki/workspace-tools:v1".into()),
        };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let ws = &root["workspaces"]["dev"];
        assert_eq!(
            ws["image"].as_str().unwrap(),
            "ghcr.io/calebfaruki/workspace-tools"
        );
        assert_eq!(ws["tag"].as_str().unwrap(), "v1");
        cleanup(&dir);
    }

    // -- list --

    #[test]
    fn list_no_workspaces() {
        let (scope, dir) = tmp_scope("list-empty");
        write_values(&scope, "workspaces: {}\n");
        do_list(&scope).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_workspaces_key_missing() {
        let (scope, dir) = tmp_scope("list-no-key");
        write_values(&scope, "models: {}\n");
        do_list(&scope).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_values_file_missing() {
        let (scope, dir) = tmp_scope("list-no-file");
        let err = do_list(&scope).unwrap_err();
        assert!(err.contains("failed to read"));
        cleanup(&dir);
    }

    // -- show --

    #[test]
    fn show_existing_workspace() {
        let (scope, dir) = tmp_scope("show-exists");
        write_values(
            &scope,
            "workspaces:\n  dev:\n    image: tools\n    tag: v1\n    agents:\n      - coder\n",
        );
        do_show(&scope, "dev").unwrap();
        cleanup(&dir);
    }

    #[test]
    fn show_with_router_model() {
        let (scope, dir) = tmp_scope("show-router");
        write_values(
            &scope,
            "workspaces:\n  dev:\n    image: tools\n    tag: v1\n    agents:\n      - a\n      - b\n    routerModel: haiku\n",
        );
        do_show(&scope, "dev").unwrap();
        cleanup(&dir);
    }

    #[test]
    fn show_nonexistent() {
        let (scope, dir) = tmp_scope("show-missing");
        write_values(&scope, "workspaces: {}\n");
        let err = do_show(&scope, "dev").unwrap_err();
        assert!(err.contains("not found"));
        cleanup(&dir);
    }

    #[test]
    fn show_no_workspaces_key() {
        let (scope, dir) = tmp_scope("show-no-key");
        write_values(&scope, "models: {}\n");
        let err = do_show(&scope, "dev").unwrap_err();
        assert!(err.contains("not found"));
        cleanup(&dir);
    }

    // -- format helpers --

    #[test]
    fn format_agents_with_entries() {
        let yaml: Value = serde_yaml::from_str("agents: [coder, reviewer]").unwrap();
        assert_eq!(format_agents(&yaml), "coder, reviewer");
    }

    #[test]
    fn format_agents_empty() {
        let yaml: Value = serde_yaml::from_str("agents: []").unwrap();
        assert_eq!(format_agents(&yaml), "-");
    }

    #[test]
    fn format_agents_missing() {
        let yaml: Value = serde_yaml::from_str("image: tools").unwrap();
        assert_eq!(format_agents(&yaml), "-");
    }

    #[test]
    fn format_image_standard() {
        let yaml: Value = serde_yaml::from_str("image: tools\ntag: v2").unwrap();
        assert_eq!(format_image(&yaml), "tools:v2");
    }

    #[test]
    fn format_image_missing_tag() {
        let yaml: Value = serde_yaml::from_str("image: tools").unwrap();
        assert_eq!(format_image(&yaml), "tools:latest");
    }

    #[test]
    fn format_image_missing() {
        let yaml: Value = serde_yaml::from_str("agents: []").unwrap();
        assert_eq!(format_image(&yaml), "-");
    }
}
