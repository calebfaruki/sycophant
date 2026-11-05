use std::fs;
use std::path::{Path, PathBuf};

use include_dir::Dir;

use crate::assets;
use crate::scope::Scope;

pub(crate) fn extract_assets(scope: &Scope) -> Result<(), String> {
    for (name, embedded) in [
        ("sycophant-cluster", &assets::CLUSTER_CHART),
        ("sycophant-tenant", &assets::TENANT_CHART),
        ("sycophant-agent", &assets::AGENT_CHART),
        ("sycophant-inference", &assets::INFERENCE_CHART),
        ("sycophant-toolset", &assets::TOOLSET_CHART),
        ("sycophant-user", &assets::USER_CHART),
        ("sycophant-channel", &assets::CHANNEL_CHART),
        ("sycophant-gvisor", &assets::GVISOR_CHART),
        ("sycophant-dex", &assets::DEX_CHART),
        ("kyverno-crds", &assets::KYVERNO_CRDS_CHART),
        ("sycophant-objectstore", &assets::OBJECTSTORE_CHART),
        ("sycophant-ingress", &assets::INGRESS_CHART),
    ] {
        extract(&scope.chart_dir(name), embedded)?;
    }

    fs::write(scope.version_file(), assets::version())
        .map_err(|e| format!("failed to write version file: {e}"))?;

    Ok(())
}

/// Extract this binary's commands chart and return its directory, so a render
/// never uses a chart a different syco left behind.
pub(crate) fn extract_commands_chart(scope: &Scope) -> Result<PathBuf, String> {
    let dir = scope.commands_chart_dir();
    extract(&dir, &assets::COMMANDS_CHART)?;
    Ok(dir)
}

fn extract(dir: &Path, embedded: &Dir<'static>) -> Result<(), String> {
    // Remove first so files deleted from the embedded chart don't linger from a
    // prior extract — otherwise helm would re-render a since-removed template.
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    embedded
        .extract(dir)
        .map_err(|e| format!("failed to extract chart to {}: {e}", dir.display()))
}
