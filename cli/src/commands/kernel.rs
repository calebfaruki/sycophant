//! `syco tenant kernel set/list/delete` — content-tier Kernel CR management.
//!
//! A Kernel binds per-workspace (`metadata.name == workspace`). A workspace's
//! persona content (AGENTS.md, agents/*.md, skills/*.md) is delivered on an
//! operator-populated read-only volume: the chart mounts the host directory at
//! `/etc/kernels/<namespace>/<workspace>`, sourced from the convention path
//! `<hostPathBase>/<namespace>/<workspace>` or a custom directory when `--path`
//! overrides it. `set` authors the CR via `kubectl apply`.

use serde::Serialize;

use crate::cli::{KernelCmd, KernelList, KernelSet, KernelSub};
use crate::commands::common;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: KernelCmd) -> Result<(), String> {
    match cmd.sub {
        KernelSub::Set(set) => do_set(scope, set),
        KernelSub::List(list) => do_list(scope, list),
        KernelSub::Delete(del) => do_delete(scope, &del.workspace),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KernelEntry {
    pub workspace: String,
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
    // must be absolute (mirrors the CRD `^/.+` schema); absent → convention
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

/// Build a `Kernel` CR for `kubectl apply`. `workspace` is the metadata name (the
/// per-workspace binding). Spec-only — `status` is controller-owned.
fn build_kernel_cr(workspace: &str, namespace: &str, source: &KernelSource) -> String {
    let name_q = serde_json::to_string(workspace).unwrap_or_default();
    let ns_q = serde_json::to_string(namespace).unwrap_or_default();
    let spec_body = match &source.path {
        Some(p) => {
            let p_q = serde_json::to_string(p).unwrap_or_default();
            format!("spec:\n  hostPath:\n    path: {p_q}\n")
        }
        // No override → convention default; the spec carries no fields.
        None => "spec: {}\n".to_string(),
    };
    format!(
        r#"apiVersion: sycophant.md/v1
kind: Kernel
metadata:
  name: {name_q}
  namespace: {ns_q}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: kernel
{spec_body}"#
    )
}

/// Refuse to author a Kernel for a workspace the tenant values don't declare.
/// Tolerant of a missing/unreadable values file (GitOps operators declare
/// workspaces outside the CLI scaffold, so the check can't be load-bearing).
fn ensure_workspace_declared(scope: &Scope, workspace: &str) -> Result<(), String> {
    let Ok(root) = crate::values::load(&scope.values_file()) else {
        return Ok(());
    };
    let workspaces = root.get("workspaces").and_then(|v| v.as_mapping());
    crate::commands::workspace::workspace_show_data(workspaces, workspace)
        .map(|_| ())
        .map_err(|_| {
            format!(
                "Workspace \"{workspace}\" is not declared. \
                 Run: syco tenant workspace create {workspace} --ns <name>"
            )
        })
}

fn do_set(scope: &Scope, cmd: KernelSet) -> Result<(), String> {
    let source = parse_kernel_source(&cmd)?;
    ensure_workspace_declared(scope, &cmd.workspace)?;
    let namespace = scope.release_name()?;

    let yaml = build_kernel_cr(&cmd.workspace, &namespace, &source);
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;

    eprintln!("Kernel for workspace '{}' configured.", cmd.workspace);
    eprintln!("Run `syco tenant up --ns {namespace}` to deliver it.");
    Ok(())
}

fn do_list(scope: &Scope, cmd: KernelList) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let output = run_output(
        "kubectl",
        &[
            "get",
            "kernels.sycophant.md",
            "-n",
            &namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
        ],
    )?;
    let entries = parse_kernel_list(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No kernels configured.");
        return Ok(());
    }

    eprintln!("{:<40}", "WORKSPACE");
    for e in &entries {
        eprintln!("{:<40}", e.workspace);
    }
    Ok(())
}

/// Parse the tab-separated `kubectl get kernels` jsonpath output into entries.
pub(crate) fn parse_kernel_list(kubectl_output: &str) -> Vec<KernelEntry> {
    common::parse_tab_rows(kubectl_output)
        .iter()
        .map(|c| KernelEntry {
            workspace: common::col(c, 0),
        })
        .collect()
}

fn do_delete(scope: &Scope, workspace: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    if common::delete_cr("kernel.sycophant.md", workspace, &namespace)? {
        eprintln!("Kernel for workspace '{workspace}' deleted.");
    } else {
        eprintln!("Kernel for workspace '{workspace}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn parse(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).expect("builder output must be valid YAML")
    }

    /// `parse_kernel_source` enforces the CRD `^/.+` shape: an absolute `--path`
    /// override is accepted, a relative one is rejected, and no override is the
    /// convention default. Kills the mutant that deletes the `!` on the
    /// absolute-path guard (which would flip the check and author a relative
    /// override the CRD rejects at admission).
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

    /// Bare host-path authoring emits a spec with no source-shaped fields — no
    /// `kind` discriminator and no `s3` block (the CRD no longer defines them). A
    /// mutant re-adding the `kind` line or an `s3` block is caught here.
    #[test]
    fn hostpath_cr_omits_kind_and_s3() {
        let v = parse(&build_kernel_cr("ws", "dev", &KernelSource { path: None }));
        assert_eq!(v["kind"].as_str(), Some("Kernel"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("ws"));
        assert_eq!(v["metadata"]["namespace"].as_str(), Some("dev"));
        assert!(
            v["spec"].get("kind").is_none(),
            "authored CR must not carry a kind discriminator"
        );
        assert!(
            v["spec"].get("s3").is_none(),
            "authored CR must not carry an s3 block"
        );
        assert!(
            v["spec"].get("hostPath").is_none(),
            "no override → no hostPath block (convention default)"
        );
        assert!(v.get("status").is_none(), "builder is spec-only");
    }

    /// The optional `--path` override is authored under `hostPath.path` and is the
    /// only source-shaped field. A mutant dropping the override loses the
    /// operator's custom directory.
    #[test]
    fn hostpath_cr_emits_path_override() {
        let v = parse(&build_kernel_cr(
            "ws",
            "dev",
            &KernelSource {
                path: Some("/custom/dir".into()),
            },
        ));
        assert_eq!(v["spec"]["hostPath"]["path"].as_str(), Some("/custom/dir"));
        assert!(v["spec"].get("kind").is_none());
        assert!(v["spec"].get("s3").is_none());
    }

    #[test]
    fn cr_has_operator_labels_not_helm() {
        let v = parse(&build_kernel_cr("ws", "dev", &KernelSource { path: None }));
        assert_eq!(
            v["metadata"]["labels"]["sycophant.md/type"].as_str(),
            Some("kernel")
        );
        assert!(v["metadata"]["labels"]["app.kubernetes.io/managed-by"].is_null());
    }
}
