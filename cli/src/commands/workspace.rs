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
}

pub(crate) fn workspace_list_data(workspaces: Option<&serde_yaml::Mapping>) -> Vec<WorkspaceEntry> {
    let Some(workspaces) = workspaces else {
        return Vec::new();
    };
    workspaces
        .iter()
        .filter_map(|(k, _)| {
            let name = k.as_str()?.to_string();
            Some(WorkspaceEntry { name })
        })
        .collect()
}

pub(crate) fn workspace_show_data(
    workspaces: Option<&serde_yaml::Mapping>,
    name: &str,
) -> Result<WorkspaceEntry, String> {
    let workspaces = workspaces.ok_or_else(|| format!("Workspace \"{name}\" not found."))?;
    workspaces
        .get(Value::String(name.into()))
        .ok_or_else(|| format!("Workspace \"{name}\" not found."))?;
    Ok(WorkspaceEntry {
        name: name.to_string(),
    })
}

fn do_create(scope: &Scope, cmd: WorkspaceCreate) -> Result<(), String> {
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;
    let workspaces = values::ensure_map(&mut root, "workspaces");

    let key = Value::String(cmd.name.clone());
    if workspaces.contains_key(&key) {
        return Err(format!("Workspace \"{}\" already exists.", cmd.name));
    }

    workspaces.insert(key, Value::Mapping(serde_yaml::Mapping::new()));

    values::save(&values_path, &root)?;
    eprintln!("Created workspace \"{}\".", cmd.name);
    Ok(())
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

    writeln!(out, "NAME")?;
    for (key, _) in workspaces {
        let name = key.as_str().unwrap_or("");
        writeln!(out, "{name}")?;
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
        eprintln!("Name:         {}", entry.name);
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
        (
            Scope {
                root: dir.clone(),
                tenant: None,
            },
            dir,
        )
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

    #[test]
    fn render_list_empty_mapping_says_none_configured() {
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
    fn render_list_with_entries_prints_names() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            Value::String("dev".into()),
            Value::Mapping(serde_yaml::Mapping::new()),
        );

        let mut out = Vec::new();
        render_workspace_list(Some(&mapping), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("NAME"));
        assert!(s.contains("dev"));
        assert!(!s.contains("No workspaces configured"));
    }

    // -- create --

    #[test]
    fn create_writes_empty_mapping() {
        let (scope, dir) = tmp_scope("create-default");
        write_values(&scope, "workspaces: {}\n");
        let cmd = WorkspaceCreate { name: "dev".into() };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        let ws = &root["workspaces"]["dev"];
        assert!(ws.is_mapping());
        assert!(
            ws.as_mapping().unwrap().is_empty(),
            "fresh workspace must be an empty mapping (schema permits empty object)"
        );
        cleanup(&dir);
    }

    #[test]
    fn create_duplicate_errors() {
        let (scope, dir) = tmp_scope("create-dup");
        write_values(&scope, "workspaces:\n  dev: {}\n");
        let cmd = WorkspaceCreate { name: "dev".into() };
        let err = do_create(&scope, cmd).unwrap_err();
        assert!(err.contains("already exists"));
        cleanup(&dir);
    }

    #[test]
    fn create_ensures_workspaces_key() {
        let (scope, dir) = tmp_scope("create-no-key");
        write_values(&scope, "models: {}\n");
        let cmd = WorkspaceCreate { name: "dev".into() };
        do_create(&scope, cmd).unwrap();
        let root = read_values(&scope);
        assert!(root["workspaces"]["dev"].is_mapping());
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
        write_values(&scope, "workspaces:\n  dev: {}\n");
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

    // -- delete --

    #[test]
    fn delete_existing_workspace() {
        let (scope, dir) = tmp_scope("delete-ws");
        write_values(&scope, "workspaces:\n  dev: {}\n");
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
    fn workspace_list_data_extracts_names() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            Value::String("dev".into()),
            Value::Mapping(serde_yaml::Mapping::new()),
        );

        let entries = workspace_list_data(Some(&mapping));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "dev");
    }

    #[test]
    fn workspace_show_data_returns_existing() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            Value::String("dev".into()),
            Value::Mapping(serde_yaml::Mapping::new()),
        );

        let result = workspace_show_data(Some(&mapping), "dev").unwrap();
        assert_eq!(result.name, "dev");
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
        let entry = WorkspaceEntry { name: "dev".into() };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"name\":\"dev\""));
    }
}
