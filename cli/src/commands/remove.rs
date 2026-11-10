use crate::commands::command;
use crate::commands::common::{ok, wait_for};
use crate::commands::operator::Operator;
use crate::runner::{run_output, run_silent};
use crate::scope::Scope;

/// `sycoadm tenant remove <name>` — remove a tenant completely, including its
/// PVCs and data. Irreversible. Suspends with a zero-day keepalive, so the
/// cluster's reap deletes the namespace, its PVCs, Secrets, and releases on its
/// next sweep.
pub(crate) fn run(scope: &Scope) -> Result<(), String> {
    let ns = scope.release_name()?;
    if !run_silent("kubectl", &["get", "namespace", &ns]) {
        eprintln!("Tenant '{ns}' does not exist; nothing to remove.");
        return Ok(());
    }

    eprintln!("Removing tenant '{ns}' (workloads + data)...");
    command::suspend(&Operator::login(&scope.oidc_dir())?, &ns, Some("0"))?;
    wait_for("reap-after stamped on the tenant namespace", 120, || {
        run_output(
            "kubectl",
            &[
                "get",
                "namespace",
                &ns,
                "-o",
                "jsonpath={.metadata.annotations.sycophant\\.md/reap-after}",
            ],
        )
        .is_ok_and(|a| !a.is_empty())
    })?;
    ok(&format!("{ns} is deleted on the next reap sweep"));
    Ok(())
}
