use crate::cli::{TenantCmd, TenantSub};
use crate::commands::command::find_tenant_ns;
use crate::commands::{down, remove, secret, up};
use crate::scope::Scope;

pub(crate) fn run(cmd: TenantCmd) -> Result<(), String> {
    match cmd.sub {
        TenantSub::Up(c) => up::run(&c.values),
        TenantSub::Down(c) => down::run(
            &Scope::for_tenant(&find_tenant_ns(&c.name, None)?)?,
            c.keepalive.as_deref(),
        ),
        TenantSub::Remove(c) => remove::run(&Scope::for_tenant(&find_tenant_ns(&c.name, None)?)?),
        TenantSub::Secret(c) => secret::run(c),
    }
}
