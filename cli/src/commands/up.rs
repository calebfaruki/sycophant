use std::fs;

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

    eprintln!("Deploying tenant {release}...");
    run_passthrough(
        "helm",
        &[
            "upgrade",
            "--install",
            &release,
            &chart_str,
            "-n",
            &release,
            // helm needs the namespace to exist to store its release; the chart's
            // tenant-ns.yaml (namespace.create=true) then reconciles the perimeter
            // labels onto it — so it's created secured, not bare.
            "--create-namespace",
            "-f",
            &values_str,
        ],
    )
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
