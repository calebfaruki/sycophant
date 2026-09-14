use std::fs;

use crate::runner::run_passthrough;
use crate::scope::Scope;

/// Per-tenant values scaffold, written only-if-absent by `tenant up` so edits
/// survive re-runs. Toolsets are declared in this file; the remaining content
/// (clients) is applied separately via `syco tenant <noun> … --ns <name>`.
const SCAFFOLD_VALUES: &str = r#"# Sycophant tenant values.yaml
# Edit this file, then run: syco tenant up --ns <name>
# Toolsets are declared here. The rest is managed separately (so platform
# upgrades never prune it):
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

    // `up` sets nothing instructions-related: instructions.prefix defaults to <ns>/<ws> in
    // the chart. The values file (toolsets and any per-workspace overrides)
    // rides along on `-f` — no CLI-side kubectl read.
    let args = helm_args(&release, &chart_str, &values_str);

    eprintln!("Deploying tenant {release}...");
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_passthrough("helm", &arg_refs)
}

/// Helm `upgrade --install` args for the tenant chart.
fn helm_args(release: &str, chart_dir: &str, values_file: &str) -> Vec<String> {
    vec![
        "upgrade".into(),
        "--install".into(),
        release.into(),
        chart_dir.into(),
        "-n".into(),
        release.into(),
        // helm needs the namespace to exist to store its release; the chart's
        // tenant-ns.yaml (namespace.create=true) then reconciles the perimeter
        // labels onto it — so it's created secured, not bare.
        "--create-namespace".into(),
        "-f".into(),
        values_file.into(),
    ]
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
    fn helm_args_emit_nothing_instructions_related() {
        // `up` sets nothing instructions-related against the new schema: the retired
        // `--set-string harness.instructions.hostPathBase=…` is gone (instructions.prefix
        // defaults to <ns>/<ws> in the chart). A mutant re-adding it is caught.
        let args = helm_args("acme", "/charts/tenant", "/cfg/acme/values.yaml");
        assert!(
            !args.iter().any(|a| a.contains("hostPathBase")),
            "up must not emit hostPathBase: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--set-string"),
            "up must set nothing instructions-related: {args:?}"
        );
        // The values file still rides along on -f.
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-f" && w[1] == "/cfg/acme/values.yaml"),
            "up must pass the values file on -f: {args:?}"
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
