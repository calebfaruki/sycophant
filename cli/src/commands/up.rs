use std::fs;
use std::path::Path;

use crate::runner::run_passthrough;
use crate::scope::Scope;

/// Per-tenant values scaffold, written only-if-absent by `tenant up` so edits
/// survive re-runs. Toolsets are declared in this file; the remaining content
/// (kernels/clients) is applied separately via `syco tenant <noun> … --ns <name>`.
const SCAFFOLD_VALUES: &str = r#"# Sycophant tenant values.yaml
# Edit this file, then run: syco tenant up --ns <name>
# Toolsets are declared here. The rest is managed separately (so platform
# upgrades never prune it):
#   syco tenant kernel set <ws> [--path <dir>] --ns <name>
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
        eprintln!("Scaffolded {} — edit it and re-run.", values_file.display());
    }

    let chart_str = chart_dir.to_string_lossy().to_string();
    let values_str = values_file.to_string_lossy().to_string();

    // Kernel content root: point the chart's hostPath base at the CLI's
    // bind-mounted kernels dir (setup.rs mounts this into the node), and ensure
    // the per-tenant subdir exists so operators can drop agent files.
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
    // Each workspace's optional custom kernel path lives in the values file
    // (`workspaces.<ws>.kernel.path`, authored by `syco tenant kernel set`),
    // which the chart reads directly to render that workspace's serving PV. It
    // rides along on the `-f <values>` below — no CLI-side kubectl read.
    args.push("-f".into());
    args.push(values_str);

    eprintln!("Deploying tenant {release}...");
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_passthrough("helm", &arg_refs)
}

/// Helm `--set-string` value pointing the chart's kernel hostPath base at the
/// CLI's bind-mounted kernels dir. The chart appends `/<namespace>/<workspace>`.
fn hostpath_base_set_arg(kernels_dir: &Path) -> String {
    format!("harness.kernels.hostPathBase={}", kernels_dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaffold() -> serde_yaml::Value {
        serde_yaml::from_str(SCAFFOLD_VALUES).expect("scaffold must be valid YAML")
    }

    #[test]
    fn scaffold_has_workspaces() {
        // Mutant dropping the `workspaces` key is caught here.
        assert!(scaffold().get("workspaces").is_some());
    }

    #[test]
    fn hostpath_base_arg_names_the_kernels_dir() {
        // Mutant dropping the key or pointing elsewhere breaks kernel delivery:
        // the chart appends /<ns>/<ws> to this base, so the mount would resolve
        // to the wrong node path and agents would never load.
        let arg = hostpath_base_set_arg(Path::new("/home/u/.config/sycophant/kernels"));
        assert_eq!(
            arg,
            "harness.kernels.hostPathBase=/home/u/.config/sycophant/kernels"
        );
    }

    #[test]
    fn scaffold_omits_schema_invalid_keys() {
        // The tenant values.schema.json is additionalProperties:false and rejects
        // these root keys (all content applied via syco/kubectl); scaffolding any
        // would make `tenant up` fail chart validation. Mutant adding one is caught.
        let v = scaffold();
        for key in ["models", "providers", "channels", "clients"] {
            assert!(
                v.get(key).is_none(),
                "scaffold must not contain root key `{key}`"
            );
        }
    }
}
