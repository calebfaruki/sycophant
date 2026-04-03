use std::fs;

use crate::assets;
use crate::scope::Scope;

pub(crate) fn extract_assets(scope: &Scope) -> Result<(), String> {
    let charts_dir = scope.charts_dir();
    fs::create_dir_all(&charts_dir)
        .map_err(|e| format!("failed to create {}: {e}", charts_dir.display()))?;
    assets::CHARTS
        .extract(&charts_dir)
        .map_err(|e| format!("failed to extract charts: {e}"))?;

    let examples_dir = scope.examples_dir();
    fs::create_dir_all(&examples_dir)
        .map_err(|e| format!("failed to create {}: {e}", examples_dir.display()))?;
    assets::EXAMPLES
        .extract(&examples_dir)
        .map_err(|e| format!("failed to extract examples: {e}"))?;

    fs::write(scope.version_file(), assets::version())
        .map_err(|e| format!("failed to write version file: {e}"))?;

    Ok(())
}

pub(crate) fn auto_sync(scope: &Scope) -> Result<(), String> {
    let current = assets::version();
    let installed = fs::read_to_string(scope.version_file())
        .unwrap_or_default()
        .trim()
        .to_string();

    if installed == current {
        return Ok(());
    }

    extract_assets(scope)?;
    eprintln!("Charts updated to {current}.");

    Ok(())
}
