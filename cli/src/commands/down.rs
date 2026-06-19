use crate::runner::{run_passthrough, run_silent};
use crate::scope::Scope;

/// `syco down` — scale the tenant to zero. Stops all compute (the singleton
/// controllers + the workspace) but leaves the release, PVCs, secrets, and CRs
/// in place. Reverse with `syco up`. Data is never touched here; use
/// `syco destroy <tenant>` to remove data.
pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let ns = scope.release_name()?;

    if !run_silent("kubectl", &["get", "namespace", &ns]) {
        eprintln!("Tenant namespace '{ns}' does not exist; nothing to scale down.");
        return Ok(());
    }

    eprintln!("Scaling tenant '{ns}' to zero...");
    run_passthrough(
        "kubectl",
        &[
            "scale",
            "deployment",
            "--all",
            "-n",
            &ns,
            "--replicas=0",
        ],
    )
}
