use crate::cli::{TenantCmd, TenantSub};
use crate::commands::{audit, down, kernel, remove, secret, toolset, up, workspace};
use crate::scope::Scope;

/// Dispatch a `syco tenant <…> --ns <name>` subcommand. `--ns` is a single
/// global flag on `tenant` (declared once, may appear anywhere on the line);
/// the dispatcher resolves it into the per-tenant scope. `toolset lint` is the
/// one local subcommand that needs no namespace.
pub(crate) fn run(cmd: TenantCmd) -> Result<(), String> {
    let TenantCmd { ns, sub } = cmd;
    let scope = || -> Result<Scope, String> { Scope::for_tenant(require_ns(&ns)?) };
    match sub {
        TenantSub::Up(_) => up::run(&scope()?),
        TenantSub::Down(_) => down::run(&scope()?),
        TenantSub::Remove(_) => remove::run(require_ns(&ns)?),
        TenantSub::Kernel(c) => kernel::run(&scope()?, c),
        TenantSub::Secret(c) => secret::run(&scope()?, c),
        TenantSub::Workspace(c) => workspace::run(&scope()?, c),
        TenantSub::Toolset(c) => toolset::run(c),
        TenantSub::Audit(c) => audit::run(&scope()?, c),
    }
}

fn require_ns(ns: &Option<String>) -> Result<&str, String> {
    ns.as_deref()
        .ok_or_else(|| "--ns <name> is required for this command".to_string())
}
