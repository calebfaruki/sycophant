//! `syco tenant kernel set/list/delete` — per-workspace kernel (persona content)
//! path overrides.
//!
//! A workspace's persona content (AGENTS.md, agents/*.md, skills/*.md) is
//! delivered on an operator-populated read-only volume: the chart mounts the
//! host directory at `/etc/kernels/<namespace>/<workspace>`, sourced from the
//! convention path `<hostPathBase>/<namespace>/<workspace>` or a custom
//! directory when the workspace declares `kernel.path`. That value lives in the
//! tenant values file (the same file `syco tenant up` deploys), so `set` writes
//! `workspaces.<ws>.kernel.path`, and `list`/`delete` read and edit it.

use serde::Serialize;
use serde_yaml::Value;

use crate::cli::{KernelCmd, KernelList, KernelSet, KernelSub};
use crate::scope::Scope;
use crate::values;

pub(crate) fn run(scope: &Scope, cmd: KernelCmd) -> Result<(), String> {
    match cmd.sub {
        KernelSub::Set(set) => do_set(scope, set),
        KernelSub::List(list) => do_list(scope, list),
        KernelSub::Delete(del) => do_delete(scope, &del.workspace),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct KernelEntry {
    workspace: String,
    path: String,
}

/// Parsed, validated kernel source. Delivery is host-path only: content is
/// served from the operator-populated read-only volume. `path` is an optional
/// absolute override of the host source directory; `None` serves the convention
/// path <kernels-root>/<namespace>/<workspace>.
#[derive(Debug, PartialEq)]
struct KernelSource {
    path: Option<String>,
}

/// Validate the host-path source. `--path` is an optional absolute override of
/// the host source dir (absent → convention default).
fn parse_kernel_source(cmd: &KernelSet) -> Result<KernelSource, String> {
    // `--path` is an optional override of the host source dir. When given it
    // must be absolute (mirrors the values schema `^/.+`); absent → convention
    // default <hostPathBase>/<namespace>/<workspace>.
    if let Some(p) = &cmd.path {
        if !p.starts_with('/') {
            return Err(format!(
                "--path must be an absolute path (start with '/'), got {p:?}."
            ));
        }
    }
    Ok(KernelSource {
        path: cmd.path.clone(),
    })
}

/// `set`'s custom override path. `set` requires an explicit `--path`; reverting
/// a workspace to the convention default is `delete`'s job, not an empty `set`.
fn require_override_path(source: KernelSource) -> Result<String, String> {
    source.path.ok_or_else(|| {
        "kernel set requires --path; use 'kernel delete' to clear a workspace's override."
            .to_string()
    })
}

fn not_declared(workspace: &str) -> String {
    format!(
        "Workspace \"{workspace}\" is not declared. \
         Run: syco tenant workspace create {workspace} --ns <name>"
    )
}

/// A declared workspace's mapping, ready for editing. Coerces a bare
/// (`null`/scalar) workspace entry to an empty mapping. Errors when the
/// workspace isn't declared in the values file.
fn workspace_entry_mut<'a>(
    root: &'a mut Value,
    workspace: &str,
) -> Result<&'a mut serde_yaml::Mapping, String> {
    let workspaces = root
        .get_mut("workspaces")
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| not_declared(workspace))?;
    let entry = workspaces
        .get_mut(workspace)
        .ok_or_else(|| not_declared(workspace))?;
    if !entry.is_mapping() {
        *entry = Value::Mapping(serde_yaml::Mapping::new());
    }
    Ok(entry.as_mapping_mut().unwrap())
}

/// Write `workspaces.<ws>.kernel.path` (the custom host source dir). The chart
/// renders that workspace's read-only serving PV from it.
fn set_kernel_path(root: &mut Value, workspace: &str, path: &str) -> Result<(), String> {
    let ws = workspace_entry_mut(root, workspace)?;
    let mut kernel = serde_yaml::Mapping::new();
    kernel.insert(
        Value::String("path".into()),
        Value::String(path.to_string()),
    );
    ws.insert(Value::String("kernel".into()), Value::Mapping(kernel));
    Ok(())
}

/// Remove any `kernel` override from a workspace (revert to the convention
/// path). Returns whether one existed. Errors when the workspace isn't declared.
fn clear_kernel(root: &mut Value, workspace: &str) -> Result<bool, String> {
    let ws = workspace_entry_mut(root, workspace)?;
    Ok(ws.remove("kernel").is_some())
}

/// Workspaces carrying an explicit `kernel.path` override, with that path.
/// Workspaces on the convention default have no override and aren't listed.
fn kernel_entries(workspaces: Option<&serde_yaml::Mapping>) -> Vec<KernelEntry> {
    let Some(workspaces) = workspaces else {
        return Vec::new();
    };
    workspaces
        .iter()
        .filter_map(|(k, v)| {
            let workspace = k.as_str()?.to_string();
            let path = v.get("kernel")?.get("path")?.as_str()?.to_string();
            Some(KernelEntry { workspace, path })
        })
        .collect()
}

fn do_set(scope: &Scope, cmd: KernelSet) -> Result<(), String> {
    let source = parse_kernel_source(&cmd)?;
    let path = require_override_path(source)?;
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;

    set_kernel_path(&mut root, &cmd.workspace, &path)?;
    values::save(&values_path, &root)?;

    let namespace = scope.release_name()?;
    eprintln!("Kernel for workspace '{}' configured.", cmd.workspace);
    eprintln!("Run `syco tenant up --ns {namespace}` to deliver it.");
    Ok(())
}

fn do_list(scope: &Scope, cmd: KernelList) -> Result<(), String> {
    let root = values::load(&scope.values_file())?;
    let workspaces = root.get("workspaces").and_then(Value::as_mapping);
    let entries = kernel_entries(workspaces);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No kernel overrides configured.");
        return Ok(());
    }

    eprintln!("{:<40} PATH", "WORKSPACE");
    for e in &entries {
        eprintln!("{:<40} {}", e.workspace, e.path);
    }
    Ok(())
}

fn do_delete(scope: &Scope, workspace: &str) -> Result<(), String> {
    let values_path = scope.values_file();
    let mut root = values::load(&values_path)?;

    // An undeclared workspace or an absent override both mean "nothing to
    // delete" — report not-found and leave the file untouched.
    match clear_kernel(&mut root, workspace) {
        Ok(true) => {
            values::save(&values_path, &root)?;
            eprintln!("Kernel override for workspace '{workspace}' deleted.");
        }
        Ok(false) | Err(_) => {
            eprintln!("Kernel override for workspace '{workspace}' not found.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn values(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).expect("test values must be valid YAML")
    }

    /// `parse_kernel_source` enforces the schema `^/.+` shape: an absolute
    /// `--path` override is accepted, a relative one is rejected, and no
    /// override is the convention default. Kills the mutant that deletes the `!`
    /// on the absolute-path guard (which would flip the check and accept a
    /// relative override the values schema rejects).
    #[test]
    fn parse_kernel_source_requires_absolute_path() {
        let abs = KernelSet {
            workspace: "ws".into(),
            path: Some("/abs/dir".into()),
        };
        assert_eq!(
            parse_kernel_source(&abs).unwrap(),
            KernelSource {
                path: Some("/abs/dir".into())
            }
        );

        let rel = KernelSet {
            workspace: "ws".into(),
            path: Some("relative/dir".into()),
        };
        let err = parse_kernel_source(&rel).unwrap_err();
        assert!(
            err.contains("absolute"),
            "relative --path must be rejected, got: {err}"
        );

        let none = KernelSet {
            workspace: "ws".into(),
            path: None,
        };
        assert_eq!(
            parse_kernel_source(&none).unwrap(),
            KernelSource { path: None }
        );
    }

    /// `set` requires an explicit `--path`: an absolute override resolves to that
    /// path, while an absent `--path` errors and points at `delete` rather than
    /// silently clearing the override. Kills the mutant that restores the old
    /// clear-on-empty routing.
    #[test]
    fn require_override_path_demands_explicit_path() {
        assert_eq!(
            require_override_path(KernelSource {
                path: Some("/abs/dir".into())
            })
            .unwrap(),
            "/abs/dir"
        );

        let err = require_override_path(KernelSource { path: None }).unwrap_err();
        assert!(
            err.contains("--path") && err.contains("delete"),
            "empty `set` must error and point at `delete`, got: {err}"
        );
    }

    /// `kernel set` is host-path only: a workspace plus an optional absolute
    /// `--path` is the entire surface, with no `--kind` argument and no S3 flags.
    /// Each removed flag is caught by its rejection assertion, so a mutant
    /// re-adding `--kind` or any S3 flag fails this test.
    #[test]
    fn kernel_set_is_hostpath_only() {
        // Host-path authoring needs only the workspace, optionally an absolute --path.
        assert!(
            Cli::try_parse_from(["syco", "tenant", "kernel", "set", "ws"]).is_ok(),
            "`kernel set <ws>` must parse without --kind"
        );
        assert!(
            Cli::try_parse_from(["syco", "tenant", "kernel", "set", "ws", "--path", "/abs"])
                .is_ok(),
            "`kernel set <ws> --path <abs>` must parse"
        );

        for (flag, val) in [
            ("--kind", "s3"),
            ("--endpoint", "http://gw"),
            ("--bucket", "b"),
            ("--prefix", "p/"),
            ("--region", "us-east-1"),
            ("--force-path-style", "true"),
            ("--credentials", "creds"),
            ("--access-key-id-key", "id"),
            ("--secret-access-key-key", "secret"),
        ] {
            let r = Cli::try_parse_from([
                "syco", "tenant", "kernel", "set", "ws", "--path", "/abs", flag, val,
            ]);
            assert!(r.is_err(), "`kernel set` must reject {flag}");
        }
    }

    /// `set_kernel_path` writes the override under `workspaces.<ws>.kernel.path`
    /// and nowhere else. A mutant targeting the wrong workspace or dropping the
    /// `path` key is caught here.
    #[test]
    fn set_kernel_path_writes_override() {
        let mut root = values("workspaces:\n  web: {}\n  api: {}\n");
        set_kernel_path(&mut root, "web", "/custom/web").unwrap();
        assert_eq!(
            root["workspaces"]["web"]["kernel"]["path"].as_str(),
            Some("/custom/web")
        );
        // The untargeted workspace is untouched.
        assert!(root["workspaces"]["api"].get("kernel").is_none());
    }

    /// A workspace declared as a bare key (`dev:` → null) is coerced to a
    /// mapping before the override is written. A mutant dropping the coercion
    /// would panic or lose the write.
    #[test]
    fn set_kernel_path_coerces_bare_workspace() {
        let mut root = values("workspaces:\n  dev:\n");
        set_kernel_path(&mut root, "dev", "/k/dev").unwrap();
        assert_eq!(
            root["workspaces"]["dev"]["kernel"]["path"].as_str(),
            Some("/k/dev")
        );
    }

    /// Editing an undeclared workspace errors rather than silently creating a
    /// partial entry. Guards both `set` and `delete` (both route through
    /// `workspace_entry_mut`).
    #[test]
    fn set_kernel_path_requires_declared_workspace() {
        let mut root = values("workspaces:\n  web: {}\n");
        let err = set_kernel_path(&mut root, "ghost", "/x").unwrap_err();
        assert!(err.contains("not declared"), "got: {err}");

        let mut no_ws = values("models: {}\n");
        assert!(set_kernel_path(&mut no_ws, "web", "/x").is_err());
    }

    /// `clear_kernel` removes an existing override and reports it; a workspace
    /// with no override reports `false` and stays untouched. A mutant inverting
    /// the return would misreport delete outcomes.
    #[test]
    fn clear_kernel_removes_override_and_reports() {
        let mut with = values("workspaces:\n  web:\n    kernel:\n      path: /x\n");
        assert!(clear_kernel(&mut with, "web").unwrap());
        assert!(with["workspaces"]["web"].get("kernel").is_none());

        let mut without = values("workspaces:\n  web: {}\n");
        assert!(!clear_kernel(&mut without, "web").unwrap());
    }

    /// `kernel_entries` lists only workspaces with an explicit `kernel.path`
    /// override, pairing each with its path. Convention-default workspaces (no
    /// override) are omitted. Kills mutants that list every workspace or drop
    /// the path.
    #[test]
    fn kernel_entries_lists_only_overrides() {
        let root = values(
            "workspaces:\n  web:\n    kernel:\n      path: /custom/web\n  api: {}\n  bare:\n",
        );
        let workspaces = root.get("workspaces").and_then(Value::as_mapping);
        let entries = kernel_entries(workspaces);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].workspace, "web");
        assert_eq!(entries[0].path, "/custom/web");
    }

    #[test]
    fn kernel_entries_empty_for_no_workspaces() {
        assert!(kernel_entries(None).is_empty());
        let root = values("workspaces: {}\n");
        assert!(kernel_entries(root.get("workspaces").and_then(Value::as_mapping)).is_empty());
    }
}
