use crate::commands::command;
use crate::commands::common::wait_for;
use crate::commands::operator::Operator;
use crate::runner::{run_output, run_silent};
use crate::scope::Scope;

/// `sycoadm tenant down <name> [--keepalive <days>]` — write the suspend command.
/// The cluster scales every tenant Deployment to zero and keeps the release,
/// PVCs, and secrets. Use `sycoadm tenant remove <name>` to remove data.
pub(crate) fn run(scope: &Scope, keepalive: Option<&str>) -> Result<(), String> {
    let ns = scope.release_name()?;

    if !run_silent("kubectl", &["get", "namespace", &ns]) {
        eprintln!("Tenant namespace '{ns}' does not exist; nothing to scale down.");
        return Ok(());
    }

    eprintln!("Suspending tenant '{ns}'...");
    command::suspend(&Operator::login(&scope.oidc_dir())?, &ns, keepalive)?;
    wait_for("tenant Deployments scaled to zero", 120, || {
        run_output(
            "kubectl",
            &[
                "get",
                "deploy",
                "-n",
                &ns,
                "-o",
                "jsonpath={range .items[*]}{.spec.replicas}{\" \"}{end}",
            ],
        )
        .is_ok_and(|r| r.split_whitespace().all(|n| n == "0"))
    })
}
