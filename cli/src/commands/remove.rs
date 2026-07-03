use crate::runner::{run_passthrough, run_silent};

/// `syco tenant remove --ns <t>` — remove a tenant completely, including its
/// PVCs and data. Irreversible. Uninstalls the tenant's helm release, then
/// deletes the namespace, which reaps the PVCs (conversation log + workspace
/// files), Secrets, and CRs.
pub(crate) fn run(ns: &str) -> Result<(), String> {
    if !run_silent("kubectl", &["get", "namespace", ns]) {
        eprintln!("Tenant '{ns}' does not exist; nothing to remove.");
        return Ok(());
    }

    eprintln!("Removing tenant '{ns}' (workloads + data)...");
    // Best-effort release removal; the namespace delete reaps the rest.
    let _ = run_silent("helm", &["uninstall", ns, "-n", ns]);
    run_passthrough(
        "kubectl",
        &["delete", "namespace", ns, "--ignore-not-found"],
    )
}
