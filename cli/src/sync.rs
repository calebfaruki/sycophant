use std::fs;

use crate::assets;
use crate::runner::run_output;
use crate::scope::Scope;

pub fn extract_assets(scope: &Scope) -> Result<(), String> {
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

pub fn auto_sync(scope: &Scope) -> Result<(), String> {
    let current = assets::version();
    let installed = fs::read_to_string(scope.version_file())
        .unwrap_or_default()
        .trim()
        .to_string();

    if installed == current {
        return Ok(());
    }

    extract_assets(scope)?;
    eprintln!("sycophant updated to {current}.");
    check_redeploy();

    Ok(())
}

pub fn sycophant_releases() -> Vec<String> {
    let output = match run_output("helm", &["list", "-o", "json"]) {
        Ok(text) if !text.is_empty() => text,
        _ => return Vec::new(),
    };
    let entries: Vec<serde_json::Value> = match serde_json::from_str(&output) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    entries
        .iter()
        .filter(|e| {
            e["chart"]
                .as_str()
                .is_some_and(|c| c.starts_with("sycophant-"))
        })
        .filter_map(|e| e["name"].as_str().map(String::from))
        .collect()
}

fn check_redeploy() {
    if !sycophant_releases().is_empty() {
        eprintln!("Run `syco up` to redeploy with updated charts.");
    }
}
