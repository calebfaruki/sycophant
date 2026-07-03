use std::fs;

use crate::assets;
use crate::scope::Scope;

pub(crate) fn extract_assets(scope: &Scope) -> Result<(), String> {
    for (dir, embedded) in [
        (scope.cluster_chart_dir(), &assets::CLUSTER_CHART),
        (scope.tenant_chart_dir(), &assets::TENANT_CHART),
        (scope.gvisor_chart_dir(), &assets::GVISOR_CHART),
        (scope.kyverno_crds_chart_dir(), &assets::KYVERNO_CRDS_CHART),
    ] {
        // Remove first so files deleted from the embedded chart don't linger from a
        // prior extract — otherwise helm would re-render a since-removed template.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        embedded
            .extract(&dir)
            .map_err(|e| format!("failed to extract chart to {}: {e}", dir.display()))?;
    }

    fs::write(scope.version_file(), assets::version())
        .map_err(|e| format!("failed to write version file: {e}"))?;

    Ok(())
}
