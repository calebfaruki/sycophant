use std::fs;

use crate::runner::{run_output, run_passthrough};
use crate::scope::Scope;
use crate::sync::sycophant_releases;

pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let releases = sycophant_releases();
    if releases.is_empty() {
        eprintln!("No workspaces configured. Use `syco workspace up <name>` to deploy one.");
        return Ok(());
    }

    let chart_dir = scope.charts_dir();
    let chart_str = chart_dir.to_string_lossy().to_string();

    for release in &releases {
        eprintln!("Upgrading {release}...");
        let values = run_output("helm", &["get", "values", release, "-o", "yaml"])?;
        let tmp = std::env::temp_dir().join(format!("syco-{release}-values.yaml"));
        let tmp_str = tmp.to_string_lossy().to_string();
        fs::write(&tmp, &values).map_err(|e| format!("failed to write temp values: {e}"))?;
        run_passthrough(
            "helm",
            &["upgrade", "--install", release, &chart_str, "-f", &tmp_str],
        )?;
    }

    Ok(())
}
