use crate::cli::DestroyCmd;
use crate::runner::{run_passthrough, run_silent};

/// `syco destroy <tenant>` — remove a tenant completely, including its PVCs and
/// data. Irreversible. Uninstalls the tenant's helm release, then deletes the
/// namespace, which reaps the PVCs (conversation log + workspace files), Secrets,
/// and CRs.
pub(crate) fn run(cmd: DestroyCmd) -> Result<(), String> {
    let tenant = cmd.tenant;

    if !run_silent("kubectl", &["get", "namespace", &tenant]) {
        eprintln!("Tenant '{tenant}' does not exist; nothing to destroy.");
        return Ok(());
    }

    eprintln!("Destroying tenant '{tenant}' (workloads + data)...");
    // Best-effort clean release removal; the namespace delete reaps the rest.
    let _ = run_silent("helm", &["uninstall", &tenant, "-n", &tenant]);
    run_passthrough(
        "kubectl",
        &["delete", "namespace", &tenant, "--ignore-not-found"],
    )
}
