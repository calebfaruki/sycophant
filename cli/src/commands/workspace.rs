use serde::Serialize;
use serde_yaml::Value;

use crate::cli::{WorkspaceCmd, WorkspaceCreate, WorkspaceList, WorkspaceShow, WorkspaceSub};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: WorkspaceCmd) -> Result<(), String> {
    match cmd.sub {
        WorkspaceSub::Create(create) => do_create(scope, create),
        WorkspaceSub::List(list) => do_list(scope, list),
        WorkspaceSub::Show(show) => do_show(scope, show),
        WorkspaceSub::Delete(del) => do_ws_delete(scope, &del.name),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceEntry {
    pub name: String,
    pub image: String,
    pub tag: String,
}

pub(crate) fn workspace_list_data(workspaces: Option<&serde_yaml::Mapping>) -> Vec<WorkspaceEntry> {
    let Some(workspaces) = workspaces else {
        return Vec::new();
    };
    workspaces
        .iter()
        .filter_map(|(k, v)| {
            let name = k.as_str()?.to_string();
            Some(WorkspaceEntry {
                name,
                image: v
                    .get("image")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
                tag: v
                    .get("tag")
                    .and_then(|s| s.as_str())
                    .unwrap_or("latest")
                    .to_string(),
            })
        })
        .collect()
}

pub(crate) fn workspace_show_data(
    workspaces: Option<&serde_yaml::Mapping>,
    name: &str,
) -> Result<WorkspaceEntry, String> {
    let workspaces = workspaces.ok_or_else(|| format!("Workspace \"{name}\" not found."))?;
    let entry = workspaces
        .get(Value::String(name.into()))
        .ok_or_else(|| format!("Workspace \"{name}\" not found."))?;
    Ok(WorkspaceEntry {
        name: name.to_string(),
        image: entry
            .get("image")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        tag: entry
            .get("tag")
            .and_then(|s| s.as_str())
            .unwrap_or("latest")
            .to_string(),
    })
}

const DEFAULT_IMAGE: &str = "sycophant-mainframe-runtime";
const DEFAULT_TAG: &str = "latest";

fn split_image_tag(input: &str) -> (&str, &str) {
    let Some(pos) = input.rfind(':') else {
        return (input, "latest");
    };
    let tag_start = pos + 1;
    if pos == 0 || tag_start >= input.len() || input[tag_start..].contains('/') {
        return (input, "latest");
    }
    (&input[..pos], &input[tag_start..])
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

fn do_list(scope: &Scope, cmd: WorkspaceList) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;
    let workspaces = root.get("workspaces").and_then(|v| v.as_mapping());

    if cmd.json {
        let entries = workspace_list_data(workspaces);
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        Ok(())
    } else {
        render_workspace_list(workspaces, &mut std::io::stderr())
            .map_err(|e| format!("write failed: {e}"))
    }
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

fn do_show(scope: &Scope, cmd: WorkspaceShow) -> Result<(), String> {
    let values_path = scope.values_file();
    let root = values::load(&values_path)?;
    let workspaces = root.get("workspaces").and_then(|v| v.as_mapping());

    let entry = workspace_show_data(workspaces, &cmd.name)?;

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entry).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
    } else {
        let image = if entry.image.is_empty() {
            "-".to_string()
        } else {
            format!("{}:{}", entry.image, entry.tag)
        };
        eprintln!("Name:         {}", entry.name);
        eprintln!("Image:        {image}");
    }

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
    fn split_with_slash_immediately_before_colon() {
        // Catches `pos + 1 → pos - 1` mutations on the guard's slice.
        // For "foo/:bar" with pos=4 (the colon):
        //   pos + 1 (correct):  input[5..] = "bar"   — no '/' → guard passes,
        //                       returns ("foo/", "bar")
        //   pos - 1 (mutant):   input[3..] = "/:bar" — has '/' → guard fails,
        //                       falls to wildcard, returns ("foo/:bar", "latest")
        // Pinning the correct behavior catches the slice-arithmetic mutation.
        assert_eq!(split_image_tag("foo/:bar"), ("foo/", "bar"));
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
        assert_eq!(ws["image"].as_str().unwrap(), "sycophant-mainframe-runtime");
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
            image: Some("ghcr.io/calebfaruki/mainframe-runtime:v1".into()),
        };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let ws = &root["workspaces"]["dev"];
        assert_eq!(
            ws["image"].as_str().unwrap(),
            "ghcr.io/calebfaruki/mainframe-runtime"
        );
        assert_eq!(ws["tag"].as_str().unwrap(), "v1");
        cleanup(&dir);
    }

    // -- list --

    fn list_cmd() -> WorkspaceList {
        WorkspaceList { json: false }
    }

    fn show_cmd(name: &str) -> WorkspaceShow {
        WorkspaceShow {
            name: name.into(),
            json: false,
        }
    }

    #[test]
    fn list_no_workspaces() {
        let (scope, dir) = tmp_scope("list-empty");
        write_values(&scope, "workspaces: {}\n");
        do_list(&scope, list_cmd()).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_workspaces_key_missing() {
        let (scope, dir) = tmp_scope("list-no-key");
        write_values(&scope, "models: {}\n");
        do_list(&scope, list_cmd()).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn list_values_file_missing() {
        let (scope, dir) = tmp_scope("list-no-file");
        let err = do_list(&scope, list_cmd()).unwrap_err();
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
        do_show(&scope, show_cmd("dev")).unwrap();
        cleanup(&dir);
    }

    #[test]
    fn show_nonexistent() {
        let (scope, dir) = tmp_scope("show-missing");
        write_values(&scope, "workspaces: {}\n");
        let err = do_show(&scope, show_cmd("dev")).unwrap_err();
        assert!(err.contains("not found"));
        cleanup(&dir);
    }

    #[test]
    fn show_no_workspaces_key() {
        let (scope, dir) = tmp_scope("show-no-key");
        write_values(&scope, "models: {}\n");
        let err = do_show(&scope, show_cmd("dev")).unwrap_err();
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

    // -- workspace_list_data / workspace_show_data --

    #[test]
    fn workspace_list_data_returns_empty_for_none() {
        assert_eq!(workspace_list_data(None), Vec::<WorkspaceEntry>::new());
    }

    #[test]
    fn workspace_list_data_returns_empty_for_empty_mapping() {
        let mapping = serde_yaml::Mapping::new();
        assert_eq!(
            workspace_list_data(Some(&mapping)),
            Vec::<WorkspaceEntry>::new()
        );
    }

    #[test]
    fn workspace_list_data_extracts_fields() {
        let mut mapping = serde_yaml::Mapping::new();
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(Value::String("image".into()), Value::String("tools".into()));
        entry.insert(Value::String("tag".into()), Value::String("v1".into()));
        mapping.insert(Value::String("dev".into()), Value::Mapping(entry));

        let entries = workspace_list_data(Some(&mapping));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "dev");
        assert_eq!(entries[0].image, "tools");
        assert_eq!(entries[0].tag, "v1");
    }

    #[test]
    fn workspace_list_data_defaults_tag_to_latest() {
        let mut mapping = serde_yaml::Mapping::new();
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(Value::String("image".into()), Value::String("tools".into()));
        mapping.insert(Value::String("dev".into()), Value::Mapping(entry));

        let entries = workspace_list_data(Some(&mapping));
        assert_eq!(entries[0].tag, "latest");
    }

    #[test]
    fn workspace_show_data_returns_existing() {
        let mut mapping = serde_yaml::Mapping::new();
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(Value::String("image".into()), Value::String("tools".into()));
        entry.insert(Value::String("tag".into()), Value::String("v1".into()));
        mapping.insert(Value::String("dev".into()), Value::Mapping(entry));

        let result = workspace_show_data(Some(&mapping), "dev").unwrap();
        assert_eq!(result.name, "dev");
        assert_eq!(result.image, "tools");
        assert_eq!(result.tag, "v1");
    }

    #[test]
    fn workspace_show_data_errors_on_missing() {
        let mapping = serde_yaml::Mapping::new();
        let err = workspace_show_data(Some(&mapping), "dev").unwrap_err();
        assert!(err.contains("not found"));
        assert!(err.contains("dev"));
    }

    #[test]
    fn workspace_show_data_errors_on_no_workspaces_block() {
        let err = workspace_show_data(None, "dev").unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn workspace_entry_serializes_to_camel_case_json() {
        let entry = WorkspaceEntry {
            name: "dev".into(),
            image: "tools".into(),
            tag: "v1".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"name\":\"dev\""));
        assert!(json.contains("\"image\":\"tools\""));
        assert!(json.contains("\"tag\":\"v1\""));
    }
}
