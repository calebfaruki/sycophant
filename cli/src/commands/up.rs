use std::fs;
use std::path::Path;

use crate::commands::common;
use crate::runner::{run_output, run_passthrough};
use crate::scope::Scope;

/// Per-tenant values scaffold, written only-if-absent by `tenant up` so edits
/// survive re-runs. Content (models/chambers/clients) is applied separately via
/// `syco tenant <noun> … --ns <name>`, so the chart values stay schema-minimal.
const SCAFFOLD_VALUES: &str = r#"# Sycophant tenant values.yaml
# Edit this file, then run: syco tenant up --ns <name>
# Content is managed separately (so platform upgrades never prune it):
#   syco tenant model set <model> --provider <p> --secret <name> --ns <name>
#   syco tenant chamber set <name> --image <ref> --ns <name>
#   syco tenant kernel set <ws> --kind s3 --endpoint <url> --bucket <b> … --ns <name>
#   syco tenant client set <name> --workspace <ws> --ns <name>
workspaces: {}
"#;

/// `syco tenant up --ns <t>` — deploy or update the tenant (data-safe).
pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let release = scope.release_name()?;

    // The global config (charts) is written by `syco setup`; be defensive if a
    // tenant op is the first thing run.
    if !scope.tenant_chart_dir().is_dir() {
        crate::sync::extract_assets(&Scope::global()?)?;
    }
    let chart_dir = scope.tenant_chart_dir();
    let values_file = scope.values_file();

    // Scaffold per-tenant values only-if-absent (preserves edits across re-runs).
    if !values_file.exists() {
        if let Some(parent) = values_file.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        fs::write(&values_file, SCAFFOLD_VALUES)
            .map_err(|e| format!("failed to write {}: {e}", values_file.display()))?;
        eprintln!(
            "Scaffolded {} — edit it (or add models) and re-run.",
            values_file.display()
        );
    }

    // Models are only needed to serve a workspace. An empty-workspace `up` just
    // establishes the namespace + controllers (the chart owns the namespace, so
    // it must run before content is applied) — that doesn't need a model.
    let values_yaml = fs::read_to_string(&values_file).unwrap_or_default();
    if values_have_workspaces(&values_yaml) {
        validate_models(&release)?;
    }

    let chart_str = chart_dir.to_string_lossy().to_string();
    let values_str = values_file.to_string_lossy().to_string();

    // Kernel content root: point the chart's hostPath base at the CLI's
    // bind-mounted kernels dir (setup.rs mounts this into the node), and ensure
    // the per-tenant subdir exists so operators can drop persona files.
    let kernels_base = scope.kernels_dir();
    let tenant_kernels = kernels_base.join(&release);
    fs::create_dir_all(&tenant_kernels)
        .map_err(|e| format!("failed to create {}: {e}", tenant_kernels.display()))?;

    let mut args: Vec<String> = vec![
        "upgrade".into(),
        "--install".into(),
        release.clone(),
        chart_str,
        "-n".into(),
        release.clone(),
        // helm needs the namespace to exist to store its release; the chart's
        // tenant-ns.yaml (namespace.create=true) then reconciles the perimeter
        // labels onto it — so it's created secured, not bare.
        "--create-namespace".into(),
        "--set-string".into(),
        hostpath_base_set_arg(&kernels_base),
    ];
    // Each workspace's kernel kind (+ optional custom path) comes from its Kernel
    // CR, read here and passed as per-workspace values — CLI-only, no chart
    // `lookup`. The chart renders that workspace's serving PV (and, for S3, a
    // writer PV) from it.
    args.extend(kernel_set_args(&read_kernel_specs(&release)));
    args.push("-f".into());
    args.push(values_str);

    eprintln!("Deploying tenant {release}...");
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_passthrough("helm", &arg_refs)
}

/// Helm `--set-string` value pointing the chart's kernel hostPath base at the
/// CLI's bind-mounted kernels dir. The chart appends `/<namespace>/<workspace>`.
fn hostpath_base_set_arg(kernels_dir: &Path) -> String {
    format!("mainframe.kernels.hostPathBase={}", kernels_dir.display())
}

/// Read each workspace's kernel spec from the applied Kernel CRs:
/// `(workspace, kind, optional custom path)`. CLI-only — no chart `lookup`.
/// Tolerant of a cold cluster (kubectl error → no specs).
fn read_kernel_specs(namespace: &str) -> Vec<(String, String, Option<String>)> {
    let out = match run_output(
        "kubectl",
        &[
            "get",
            "kernels.sycophant.md",
            "-n",
            namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\t\"}{.spec.kind}{\"\\t\"}{.spec.hostPath.path}{\"\\n\"}{end}",
        ],
    ) {
        Ok(out) => out,
        Err(_) => return Vec::new(),
    };
    kernel_specs_from_rows(common::parse_tab_rows(&out))
}

/// Pure: `(workspace, kind, optional path)` for each row with a workspace + kind.
fn kernel_specs_from_rows(rows: Vec<Vec<String>>) -> Vec<(String, String, Option<String>)> {
    rows.into_iter()
        .filter_map(|row| {
            let ws = common::col(&row, 0);
            let kind = common::col(&row, 1);
            if ws.is_empty() || kind.is_empty() {
                return None;
            }
            let path = common::col(&row, 2);
            let path = (!path.is_empty()).then_some(path);
            Some((ws, kind, path))
        })
        .collect()
}

/// Pure: helm `--set-string` args wiring each workspace's kernel kind (+ optional
/// custom path) under `workspaces.<ws>.kernel`. The chart renders that workspace's
/// serving PV from it, and — for `s3` — a writer PV. `kind` is normalized to the
/// chart's lowercase enum (`hostpath`/`s3`).
fn kernel_set_args(specs: &[(String, String, Option<String>)]) -> Vec<String> {
    let mut args = Vec::new();
    for (ws, kind, path) in specs {
        args.push("--set-string".into());
        args.push(format!("workspaces.{ws}.kernel.kind={}", normalize_kind(kind)));
        if let Some(p) = path {
            args.push("--set-string".into());
            args.push(format!("workspaces.{ws}.kernel.path={p}"));
        }
    }
    args
}

/// Normalize a Kernel CR `spec.kind` (`HostPath`/`S3`) to the chart values enum.
fn normalize_kind(kind: &str) -> String {
    match kind {
        "HostPath" => "hostpath".to_string(),
        "S3" => "s3".to_string(),
        other => other.to_lowercase(),
    }
}

/// Preflight: refuse to deploy when no Model CRs exist in the namespace, since
/// the runtime would have nothing to route turns to. Tolerant of a cold cluster
/// where the CRDs aren't installed yet — a kubectl error skips the check.
fn validate_models(namespace: &str) -> Result<(), String> {
    match run_output(
        "kubectl",
        &[
            "get",
            "models.sycophant.md",
            "-n",
            namespace,
            "-o",
            "jsonpath={.items[*].metadata.name}",
        ],
    ) {
        Ok(out) => validate_models_output(&out),
        Err(_) => Ok(()),
    }
}

/// True if the tenant values declare at least one workspace.
fn values_have_workspaces(values_yaml: &str) -> bool {
    serde_yaml::from_str::<serde_yaml::Value>(values_yaml)
        .ok()
        .and_then(|v| v.get("workspaces").cloned())
        .and_then(|w| w.as_mapping().cloned())
        .map(|m| !m.is_empty())
        .unwrap_or(false)
}

fn validate_models_output(jsonpath_out: &str) -> Result<(), String> {
    if jsonpath_out.trim().is_empty() {
        return Err(
            "No models configured. Run: syco tenant model set <model> --provider <provider> --secret <secret> --ns <name>"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_models_output_empty_errors() {
        let err = validate_models_output("").unwrap_err();
        assert!(err.contains("No models configured"));
        assert!(validate_models_output("   ").is_err());
    }

    #[test]
    fn validate_models_output_nonempty_passes() {
        validate_models_output("anthropic.haiku default").unwrap();
    }

    #[test]
    fn values_have_workspaces_only_when_nonempty_map() {
        // Mutant flipping the emptiness check is caught here: the scaffold's
        // `workspaces: {}` must NOT require a model (the bootstrap `up`).
        assert!(!values_have_workspaces("workspaces: {}"));
        assert!(!values_have_workspaces("workspaces:\n"));
        assert!(!values_have_workspaces("other: 1"));
        assert!(values_have_workspaces(
            "workspaces:\n  hello-world:\n    chambers: []"
        ));
    }

    fn scaffold() -> serde_yaml::Value {
        serde_yaml::from_str(SCAFFOLD_VALUES).expect("scaffold must be valid YAML")
    }

    #[test]
    fn scaffold_has_workspaces() {
        // Mutant dropping the `workspaces` key is caught here.
        assert!(scaffold().get("workspaces").is_some());
    }

    #[test]
    fn kernel_specs_from_rows_keeps_workspace_and_kind() {
        // Rows without a workspace or kind are dropped; a missing path column
        // means "convention default" (None), a present one is the override.
        let rows = vec![
            vec!["web".into(), "HostPath".into(), "/custom/web".into()],
            vec!["data".into(), "S3".into()], // no path → None
            vec!["".into(), "S3".into()],     // no workspace → skip
            vec!["nope".into()],              // no kind → skip
        ];
        assert_eq!(
            kernel_specs_from_rows(rows),
            vec![
                ("web".into(), "HostPath".into(), Some("/custom/web".into())),
                ("data".into(), "S3".into(), None),
            ]
        );
    }

    #[test]
    fn kernel_set_args_injects_kind_and_optional_path() {
        // Mutant dropping the path branch loses a custom-dir override; a bad
        // normalize yields a schema-invalid enum the chart rejects.
        assert!(kernel_set_args(&[]).is_empty());
        let args = kernel_set_args(&[
            ("web".into(), "S3".into(), Some("/x".into())),
            ("api".into(), "HostPath".into(), None),
        ]);
        assert_eq!(
            args,
            vec![
                "--set-string",
                "workspaces.web.kernel.kind=s3",
                "--set-string",
                "workspaces.web.kernel.path=/x",
                "--set-string",
                "workspaces.api.kernel.kind=hostpath",
            ]
        );
    }

    #[test]
    fn hostpath_base_arg_names_the_kernels_dir() {
        // Mutant dropping the key or pointing elsewhere breaks kernel delivery:
        // the chart appends /<ns>/<ws> to this base, so the mount would resolve
        // to the wrong node path and personas would never load.
        let arg = hostpath_base_set_arg(Path::new("/home/u/.config/sycophant/kernels"));
        assert_eq!(
            arg,
            "mainframe.kernels.hostPathBase=/home/u/.config/sycophant/kernels"
        );
    }

    #[test]
    fn scaffold_omits_schema_invalid_keys() {
        // The tenant values.schema.json is additionalProperties:false and rejects
        // these root keys (all content applied via syco/kubectl); scaffolding any
        // would make `tenant up` fail chart validation. Mutant adding one is caught.
        let v = scaffold();
        for key in ["models", "providers", "channels", "chambers", "clients"] {
            assert!(
                v.get(key).is_none(),
                "scaffold must not contain root key `{key}`"
            );
        }
    }
}
