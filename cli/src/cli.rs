use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sycoadm", about = "Sycophant CLI", version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
#[allow(
    clippy::large_enum_variant,
    reason = "clap arg structs, parsed once at startup"
)]
pub(crate) enum Command {
    /// Check whether this host satisfies Sycophant's local prerequisites.
    /// This command is diagnostic only and does not change the environment.
    Doctor(DoctorCmd),
    Setup(SetupCmd),
    Destroy(DestroyCmd),
    Upgrade(UpgradeCmd),
    Tenant(TenantCmd),
    /// Join this host to a tenant's headscale tailnet. Run after `tenant up`
    /// provisions the tenant and its headscale instance
    Connect(ConnectCmd),
    /// Install or remove one toolset through its provision and reap commands
    Toolset(ComponentCmd),
    /// Install or remove one workspace through its provision and reap commands
    Workspace(ComponentCmd),
    /// Deliver or remove one workspace's instructions folder. `up` takes
    /// exactly one -f naming the workspace, its tenant and the folder, and
    /// replaces the folder's files in the store; `down` empties it
    Instructions(ComponentCmd),
    User(UserCmd),
    /// Install or remove one model through its provision and reap commands
    Inference(ComponentCmd),
    /// Install or remove one channel through its provision and reap commands
    Channel(ComponentCmd),
}

// --- setup / destroy (cluster — no --ns) ---

/// Run read-only diagnostics for the local Sycophant environment.
#[derive(Args)]
pub(crate) struct DoctorCmd {}

/// Stand up a sycophant-ready cluster from nothing: ensure the k3d cluster
/// `sycophant` with the apiserver trusting the bundled Dex, install the gVisor
/// node runtime, Cilium, Kyverno, the sycophant cluster layer, and Dex with the
/// operator as its one password user (prompted on first run); scaffold the
/// global config. Idempotent.
#[derive(Args)]
pub(crate) struct SetupCmd {}

/// Delete the sycophant k3d cluster, including every tenant and all data
/// (irreversible). Inverse of `setup`.
#[derive(Args)]
pub(crate) struct DestroyCmd {}

/// Join this host to a tenant's headscale tailnet. Port-forwards headscale,
/// mints a pre-auth key, and runs `sudo tailscale up`.
/// Run once after `sycoadm tenant up`.
#[derive(Args)]
pub(crate) struct ConnectCmd {
    /// tenant name (e.g. `hello-world`; the namespace `tenant-<name>` is derived)
    #[arg(long)]
    pub tenant: String,
}

// --- upgrade (validate the cluster, upgrade it, re-provision every tenant) ---

/// Validate the cluster, upgrade the cluster chart, then re-provision every
/// tenant through its provision command. Prompts for the operator password. Any
/// failed cluster or tenant check exits non-zero with nothing applied, and the
/// first failure while applying stops the run. `--check` runs validation only,
/// making no changes.
#[derive(Args)]
pub(crate) struct UpgradeCmd {
    /// run validation only; make no changes
    #[arg(long)]
    pub check: bool,
}

// --- tenant ---

/// Operate on a tenant
#[derive(Args)]
pub(crate) struct TenantCmd {
    #[command(subcommand)]
    pub sub: TenantSub,
}

#[derive(Subcommand)]
pub(crate) enum TenantSub {
    /// Provision or re-provision a tenant through its provision command. Prompts
    /// for the operator password. Renders the tenant provision command from
    /// values (`tenant: {name, capacity}`), writes it, and waits for its
    /// installer, which upgrades the release in place. The capacity quota is set
    /// on first provision and not resized after
    Up(Up),
    Down(TenantDown),
    Remove(TenantRemove),
    Secret(SecretCmd),
}

/// Suspend the tenant: scales it to zero, keeps all data. Prompts for the
/// operator password
#[derive(Args)]
pub(crate) struct TenantDown {
    /// tenant name (the namespace is tenant-<name>)
    pub name: String,
    /// days to keep the suspended tenant before the cluster reaps it
    #[arg(long)]
    pub keepalive: Option<String>,
}

/// Delete the tenant completely, including its PVCs/data (irreversible). The
/// cluster reaps it on its next sweep. Prompts for the operator password
#[derive(Args)]
pub(crate) struct TenantRemove {
    /// tenant name (the namespace is tenant-<name>)
    pub name: String,
}

// --- secret ---

/// Manage secrets
#[derive(Args)]
pub(crate) struct SecretCmd {
    #[command(subcommand)]
    pub sub: SecretSub,
}

#[derive(Subcommand)]
pub(crate) enum SecretSub {
    Set(SecretSet),
    List(SecretList),
    Delete(SecretDelete),
}

/// Create a secret from stdin
#[derive(Args)]
pub(crate) struct SecretSet {
    /// secret name
    pub name: String,
    /// tenant namespace
    #[arg(long)]
    pub ns: String,
}

/// List secrets
#[derive(Args)]
pub(crate) struct SecretList {
    /// emit JSON to stdout instead of human-readable list to stderr
    #[arg(long)]
    pub json: bool,
    /// tenant namespace
    #[arg(long)]
    pub ns: String,
}

/// Delete a secret
#[derive(Args)]
pub(crate) struct SecretDelete {
    /// secret name
    pub name: String,
    /// tenant namespace
    #[arg(long)]
    pub ns: String,
}

// --- provision commands rendered from a values file ---

/// Install or remove a user through its provision and reap commands
#[derive(Args)]
pub(crate) struct UserCmd {
    #[command(subcommand)]
    pub sub: UserSub,
}

#[derive(Subcommand)]
pub(crate) enum UserSub {
    /// Render the provision command from values, write it as the operator, and
    /// wait for its installer. Prompts for the operator password. A leftover
    /// command of the same name is replaced; a rejected command or a failed
    /// install exits non-zero
    Up(Up),
    Down(UserDown),
}

/// Remove a user: write its reap command as the operator and wait for
/// the uninstall. Prompts for the operator password. A failed uninstall leaves
/// the reap command and exits non-zero
#[derive(Args)]
pub(crate) struct UserDown {
    /// the user's issuer-prefixed subject, <issuer>#<sub>
    pub subject: String,
    /// tenant name (the namespace is tenant-<name>)
    #[arg(long)]
    pub tenant: String,
}

/// The arguments of every `up`.
#[derive(Args)]
pub(crate) struct Up {
    #[command(flatten)]
    pub values: Values,
}

/// The values a provision command renders from (see charts/sycophant-commands).
#[derive(Args)]
pub(crate) struct Values {
    /// a values file keyed by kind; repeatable, later files override earlier
    #[arg(short = 'f', long = "file", required = true)]
    pub files: Vec<String>,
    /// one string value, <kind>.<key>=<value>; repeatable, overrides the files
    #[arg(long = "set")]
    pub set: Vec<String>,
}

impl Values {
    /// The `helm template` arguments, in the order given.
    pub(crate) fn helm_args(&self) -> Vec<String> {
        let files = self
            .files
            .iter()
            .flat_map(|f| ["-f".to_string(), f.clone()]);
        let sets = self
            .set
            .iter()
            .flat_map(|s| ["--set-string".to_string(), s.clone()]);
        files.chain(sets).collect()
    }
}

// --- components (toolset, workspace, inference, channel) ---

#[derive(Args)]
pub(crate) struct ComponentCmd {
    #[command(subcommand)]
    pub sub: ComponentSub,
}

#[derive(Subcommand)]
pub(crate) enum ComponentSub {
    /// Render the provision command from values, write it as the operator, and
    /// wait for its installer. Prompts for the operator password. A leftover
    /// command of the same name is replaced; a rejected command or a failed
    /// install exits non-zero
    Up(Up),
    Down(ComponentDown),
}

/// Remove a component: write its reap command as the operator and wait for the
/// uninstall. Prompts for the operator password. A failed uninstall leaves the
/// reap command and exits non-zero
#[derive(Args)]
pub(crate) struct ComponentDown {
    /// the component name (the command name suffix)
    pub name: String,
    /// tenant name (the namespace is tenant-<name>)
    #[arg(long)]
    pub tenant: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_keep_file_order_then_sets() {
        let cli = Cli::try_parse_from([
            "sycoadm",
            "toolset",
            "up",
            "-f",
            "a.yaml",
            "--set",
            "toolset.tenant=t",
            "-f",
            "b.yaml",
        ])
        .unwrap();
        let Command::Toolset(ComponentCmd {
            sub: ComponentSub::Up(up),
        }) = cli.command
        else {
            panic!("not toolset up");
        };
        assert_eq!(
            up.values.helm_args(),
            [
                "-f",
                "a.yaml",
                "-f",
                "b.yaml",
                "--set-string",
                "toolset.tenant=t"
            ]
        );
    }

    #[test]
    fn doctor_is_recognized_as_a_top_level_command() {
        let cli =
            Cli::try_parse_from(["sycoadm", "doctor"]).expect("doctor should be a valid command");

        assert!(matches!(cli.command, Command::Doctor(_)));
    }
}
