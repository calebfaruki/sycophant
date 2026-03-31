use crate::runner::run_passthrough;
use crate::scope::Scope;
use crate::sync::sycophant_releases;

pub(crate) fn run(_scope: &Scope) -> Result<(), String> {
    let releases = sycophant_releases();
    if releases.is_empty() {
        eprintln!("No workspaces running.");
        return Ok(());
    }

    for release in &releases {
        eprintln!("Stopping {release}...");
        run_passthrough("helm", &["uninstall", release])?;
    }

    Ok(())
}
