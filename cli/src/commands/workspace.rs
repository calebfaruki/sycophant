use serde_yaml::Value;

use crate::cli::{WorkspaceCmd, WorkspaceCreate, WorkspaceSub};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: WorkspaceCmd) -> Result<(), String> {
    match cmd.sub {
        WorkspaceSub::Create(create) => do_create(scope, create),
        WorkspaceSub::List(_) => do_list(scope),
        WorkspaceSub::Show(show) => do_show(scope, &show.name),
        WorkspaceSub::Delete(del) => do_ws_delete(scope, &del.name),
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

    workspaces.insert(key, Value::Mapping(entry));

    values::save(&values_path, &root)?;
    eprintln!("Created workspace \"{}\".", cmd.name);
    Ok(())
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
    render_workspace_list(
        root.get("workspaces").and_then(|v| v.as_mapping()),
        &mut std::io::stderr(),
    )
    .map_err(|e| format!("write failed: {e}"))
}

fn render_workspace_list<W: std::io::Write>(
    workspaces: Option<&serde_yaml::Mapping>,
    out: &mut W,
) -> std::io::Result<()> {
    let workspaces = match workspaces {
        Some(m) if !m.is_empty() => m,
        _ => {
            writeln!(out, "No workspaces configured.")?;
            return Ok(());
        }
    };

    writeln!(out, "{:<16} IMAGE", "NAME")?;
    for (key, val) in workspaces {
        let name = key.as_str().unwrap_or("");
        let image = format_image(val);
        writeln!(out, "{name:<16} {image}")?;
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

    eprintln!("Name:         {name}");
    eprintln!("Image:        {image}");

    Ok(())
}

fn do_ws_delete(scope: &Scope, name: &str) -> Result<(), String> {
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;

    let workspaces = root
        .get_mut("workspaces")
        .and_then(|v| v.as_mapping_mut())
        .ok_or("no workspaces configured")?;

    let yaml_key = Value::String(name.into());
    if workspaces.remove(&yaml_key).is_none() {
        return Err(format!("Workspace \"{name}\" not found."));
    }

    values::save(&values_path, &root)?;
    eprintln!("Workspace '{name}' deleted.");
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

    #[test]
    fn split_leading_colon_is_not_a_tag() {
        // Catches `pos > 0 → pos >= 0` mutation. With the original guard,
        // a leading colon (pos == 0) fails the guard and we hit the wildcard.
        // With the mutation, pos == 0 would pass and we'd split into
        // ("", "foo"), which is wrong.
        assert_eq!(split_image_tag(":foo"), (":foo", "latest"));
    }

    #[test]
    fn render_list_empty_mapping_says_none_configured() {
        // Catches `match guard !m.is_empty()` mutations on do_list.
        let mapping = serde_yaml::Mapping::new();
        let mut out = Vec::new();
        render_workspace_list(Some(&mapping), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("No workspaces configured"));
    }

    #[test]
    fn render_list_none_says_none_configured() {
        let mut out = Vec::new();
        render_workspace_list(None, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("No workspaces configured"));
    }

    #[test]
    fn render_list_with_entries_prints_them() {
        let mut mapping = serde_yaml::Mapping::new();
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(Value::String("image".into()), Value::String("tools".into()));
        entry.insert(Value::String("tag".into()), Value::String("v1".into()));
        mapping.insert(Value::String("dev".into()), Value::Mapping(entry));

        let mut out = Vec::new();
        render_workspace_list(Some(&mapping), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("NAME"));
        assert!(s.contains("dev"));
        assert!(s.contains("tools:v1"));
        assert!(!s.contains("No workspaces configured"));
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
        assert!(
            ws.as_mapping().unwrap().get("agents").is_none(),
            "fresh workspace must not seed an `agents` field"
        );
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
        let yaml: Value = serde_yaml::from_str("name: ws").unwrap();
        assert_eq!(format_image(&yaml), "-");
    }

    // -- delete --

    #[test]
    fn delete_existing_workspace() {
        let (scope, dir) = tmp_scope("delete-ws");
        write_values(
            &scope,
            "workspaces:\n  dev:\n    image: tools\n    tag: latest\n    agents: []\n",
        );
        do_ws_delete(&scope, "dev").unwrap();
        let root = read_values(&scope);
        assert!(root["workspaces"].as_mapping().unwrap().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn delete_nonexistent_workspace_errors() {
        let (scope, dir) = tmp_scope("delete-ws-missing");
        write_values(&scope, "workspaces: {}\n");
        let err = do_ws_delete(&scope, "dev").unwrap_err();
        assert!(err.contains("not found"));
        cleanup(&dir);
    }
}
