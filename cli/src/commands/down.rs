use crate::runner::{run_passthrough, run_silent};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let release = scope.release_name()?;

    if !run_silent("helm", &["status", &release, "-n", &release]) {
        eprintln!("Not running.");
        return Ok(());
    }

    eprintln!("Stopping {release}...");
    run_passthrough("helm", &["uninstall", &release, "-n", &release])
}
