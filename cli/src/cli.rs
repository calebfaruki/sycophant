use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "sycoadm",
    about = "Administer Sycophant clusters, tenants, and components",
    version,
    after_long_help = TOP_LEVEL_HELP
)]
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
    Setup(SetupCmd),
    Destroy(DestroyCmd),
    Upgrade(UpgradeCmd),
    Tenant(TenantCmd),
    /// Join this host to a tenant's headscale tailnet. Run after `tenant up`
    /// provisions the tenant and its headscale instance
    Connect(ConnectCmd),
    /// Install or remove one toolset through its provision and reap commands
    #[command(
        mut_subcommand("up", |c| c.after_long_help(TOOLSET_UP_EXAMPLES)),
        mut_subcommand("down", |c| c.after_long_help(TOOLSET_DOWN_EXAMPLES))
    )]
    Toolset(ComponentCmd),
    /// Install or remove one workspace. `up` takes exactly one -f and replaces
    /// the workspace's instructions folder before provisioning; `down` deletes
    /// the folder after the reap
    #[command(
        mut_subcommand("up", |c| c.after_long_help(WORKSPACE_UP_EXAMPLES)),
        mut_subcommand("down", |c| c.after_long_help(WORKSPACE_DOWN_EXAMPLES))
    )]
    Workspace(ComponentCmd),
    User(UserCmd),
    /// Install or remove one model through its provision and reap commands
    #[command(
        mut_subcommand("up", |c| c.after_long_help(INFERENCE_UP_EXAMPLES)),
        mut_subcommand("down", |c| c.after_long_help(INFERENCE_DOWN_EXAMPLES))
    )]
    Inference(ComponentCmd),
    /// Install or remove one channel through its provision and reap commands
    #[command(
        mut_subcommand("up", |c| c.after_long_help(CHANNEL_UP_EXAMPLES)),
        mut_subcommand("down", |c| c.after_long_help(CHANNEL_DOWN_EXAMPLES))
    )]
    Channel(ComponentCmd),
}

// --- setup / destroy (cluster — no --ns) ---

/// Set up a local Sycophant cluster
//
/// Stand up a sycophant-ready cluster from nothing: ensure the k3d cluster
/// `sycophant` with the apiserver trusting the bundled Dex, install the gVisor
/// node runtime, Cilium, Kyverno, the sycophant cluster layer, and Dex with the
/// operator as its one password user (prompted on first run); scaffold the
/// global config. Idempotent.
#[derive(Args)]
#[command(after_long_help = SETUP_HELP)]
pub(crate) struct SetupCmd {}

/// Delete the cluster and all tenant data (irreversible)
///
/// Delete the sycophant k3d cluster, including every tenant and all data
/// (irreversible). Inverse of `setup`.
#[derive(Args)]
pub(crate) struct DestroyCmd {}

/// Join this host to a tenant's headscale tailnet. Port-forwards headscale,
/// mints a pre-auth key, runs `sudo tailscale up`, and verifies the join.
/// Run once after `sycoadm tenant up`.
#[derive(Args)]
pub(crate) struct ConnectCmd {
    /// tenant name (e.g. `e2e-test`; the namespace `tenant-<name>` is derived)
    #[arg(long)]
    pub tenant: String,
}

// --- upgrade (validate the cluster, upgrade it, re-provision every tenant) ---

/// Check or upgrade the cluster and its tenants
///
/// Validate the cluster, upgrade the cluster chart, then re-provision every
/// tenant through its provision command. Prompts for the operator password. Any
/// failed cluster or tenant check exits non-zero with nothing applied, and the
/// first failure while applying stops the run. `--check` runs validation only,
/// making no changes.
#[derive(Args)]
#[command(after_long_help = UPGRADE_EXAMPLES)]
pub(crate) struct UpgradeCmd {
    /// Validate the cluster and tenants without applying upgrades or other changes
    #[arg(long)]
    pub check: bool,
}

// --- tenant ---

/// Manage tenant lifecycle, secrets, and audits
///
/// Operate on a tenant. `--ns <name>` selects the namespace for secret and
/// audit, and may appear anywhere on the line.
#[derive(Args)]
pub(crate) struct TenantCmd {
    /// Tenant namespace (secret and audit only)
    #[arg(long, global = true)]
    pub ns: Option<String>,
    #[command(subcommand)]
    pub sub: TenantSub,
}

#[derive(Subcommand)]
pub(crate) enum TenantSub {
    /// Provision or re-provision a tenant from a values file
    ///
    /// Provision or re-provision a tenant through its provision command. Prompts
    /// for the operator password. Renders the tenant provision command from
    /// values (`tenant: {name, capacity}`), writes it, and waits for its
    /// installer, which upgrades the release in place. The capacity quota is set
    /// on first provision and not resized after
    #[command(after_long_help = TENANT_UP_EXAMPLES)]
    Up(Up),
    /// Temporarily stop the tenant from running,  does not delete its stored data.
    #[command(after_long_help = TENANT_DOWN_EXAMPLES)]
    Down(TenantDown),
    /// Delete a tenant completely, including its persistent data (irreversible)
    #[command(after_long_help = TENANT_REMOVE_EXAMPLES)]
    Remove(TenantRemove),
    Secret(SecretCmd),
    /// Audit a running workspace against Sycophant security controls
    #[command(after_long_help = TENANT_AUDIT_EXAMPLES)]
    Audit(AuditCmd),
}

/// Temporarily stop a tenant without deleting its stored data
///
// Suspend the tenant: scales it to zero, keeps all data. Prompts for the
/// operator password
#[derive(Args)]
pub(crate) struct TenantDown {
    /// Tenant name (the namespace is tenant-<name>)
    pub name: String,
    /// Days to keep the suspended tenant before the cluster deletes it.
    /// Without this flag the tenant stays suspended with no deadline
    #[arg(long)]
    pub keepalive: Option<String>,
}

/// Delete a tenant completely, including its persistent data (irreversible)
///
// Delete the tenant completely, including its PVCs/data (irreversible). The
/// cluster reaps it on its next sweep. Prompts for the operator password
#[derive(Args)]
pub(crate) struct TenantRemove {
    /// Tenant name (the namespace is tenant-<name>)
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
}

/// List secrets
#[derive(Args)]
pub(crate) struct SecretList {
    /// emit JSON to stdout instead of human-readable list to stderr
    #[arg(long)]
    pub json: bool,
}

/// Delete a secret
#[derive(Args)]
pub(crate) struct SecretDelete {
    /// secret name
    pub name: String,
}

// --- audit ---

/// Audit a running workspace against Sycophant security controls
///
// Audit a running workspace against the security clauses (gVisor isolation,
/// secret scrubbing, egress containment, L7 DNS allowlist, credential isolation,
/// tool execution, workspace SA). Probes the live sandbox — the workspace must
/// already have been exercised by a tool-calling message so the toolset pod exists.
#[derive(Args)]
pub(crate) struct AuditCmd {
    /// workspace name
    pub workspace: String,
}

// --- provision commands rendered from a values file ---

/// Provision a framework user from a values file
///
/// Install a framework user through its provision command
#[derive(Args)]
pub(crate) struct UserCmd {
    #[command(subcommand)]
    pub sub: UserSub,
}

#[derive(Subcommand)]
pub(crate) enum UserSub {
    /// Install or update a user from a values file
    ///
    /// Render the provision command from values, write it as the operator, and
    /// wait for its installer. Prompts for the operator password. A leftover
    /// command of the same name is replaced; a rejected command or a failed
    /// install exits non-zero
    #[command(after_long_help = USER_UP_EXAMPLES)]
    Up(Up),
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
    /// Path to a values file used to configure the resource or operation.
    /// Repeatable; later files override earlier files.
    #[arg(short = 'f', long = "file", required = true)]
    pub files: Vec<String>,
    /// Override a value from the supplied configuration using <kind>.<key>=<value>.
    /// Repeatable; overrides are applied after all values files.
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
    /// Install or update a component from a values file
    ///
    /// Render the provision command from values, write it as the operator, and
    /// wait for its installer. Prompts for the operator password. A leftover
    /// command of the same name is replaced; a rejected command or a failed
    /// install exits non-zero
    Up(Up),
    /// Remove the component and wait for its uninstall to complete
    Down(ComponentDown),
}

/// Remove a component by name and wait for its uninstall to complete
///
// Remove a component: write its reap command as the operator and wait for the
/// uninstall. Prompts for the operator password. A failed uninstall leaves the
/// reap command and exits non-zero
#[derive(Args)]
pub(crate) struct ComponentDown {
    /// the component name (the command name suffix)
    pub name: String,
    /// Tenant name (the namespace is tenant-<name>)
    #[arg(long)]
    pub tenant: String,
}

// --- long help: examples (shown by --help, not -h) ---

const TOP_LEVEL_HELP: &str = "\
Environment:
  SYCO_EMAIL       Operator email; skips the email prompt
  SYCO_PASSWORD    Operator password; skips the password prompt
  SYCO_CONFIG_DIR  Config root (default: ~/.config/sycophant)

Examples:
  # Create a local cluster, then a tenant and a workspace in it
  sycoadm setup
  sycoadm tenant up -f examples/sycoadm/tenants/e2e-test.yaml
  sycoadm workspace up -f examples/sycoadm/workspaces/hello-world.yaml

  # Show help for one command
  sycoadm workspace up --help";

const SETUP_HELP: &str = "\
Environment:
  SYCO_EMAIL               Operator email; skips the email prompt
  SYCO_PASSWORD            Operator password; skips both password prompts
  SYCO_SKIP_BUILD          Truthy: skip the image build (bring your own images)
  SYCO_K3D_ARGS            Extra `k3d cluster create` arguments, space-separated
  SYCO_AUTH_CONFIG_APPEND  File appended to the apiserver auth config
  SYCO_KYVERNO_VALUES      Extra Helm values file for Kyverno
  SYCO_CLUSTER_VALUES      Extra Helm values file for the cluster chart
  SYCO_CILIUM_PRELOAD      Truthy: import the Cilium image from local Docker

  Truthy means set, non-empty, and not 0.";

const UPGRADE_EXAMPLES: &str = "\
Examples:
  # Validate the cluster and every tenant without changing anything
  sycoadm upgrade --check

  # Upgrade the cluster chart and re-provision every tenant
  sycoadm upgrade";

const TENANT_UP_EXAMPLES: &str = "\
Examples:
  # Provision the example tenant (namespace tenant-e2e-test)
  sycoadm tenant up -f examples/sycoadm/tenants/e2e-test.yaml

  # Reuse the same file for a different tenant name
  sycoadm tenant up -f examples/sycoadm/tenants/e2e-test.yaml \\
      --set tenant.name=staging";

const TENANT_DOWN_EXAMPLES: &str = "\
Examples:
  # Suspend a tenant with no deletion deadline
  sycoadm tenant down e2e-test

  # Suspend a tenant and let the cluster delete it after 7 days
  sycoadm tenant down e2e-test --keepalive 7";

const TENANT_REMOVE_EXAMPLES: &str = "\
Examples:
  # Delete the tenant and all of its data
  sycoadm tenant remove e2e-test";

const TENANT_AUDIT_EXAMPLES: &str = "\
Examples:
  # Audit a workspace after it has made at least one tool call
  sycoadm tenant audit hello-world --ns tenant-e2e-test";

const USER_UP_EXAMPLES: &str = "\
Examples:
  # Bind the example user to the hello-world workspace
  sycoadm user up -f examples/sycoadm/users/alice.yaml

  # Override the subject (<issuer>#<sub>) from the file
  sycoadm user up -f examples/sycoadm/users/alice.yaml \\
      --set \"user.subject=<issuer>#<sub>\"";

const TOOLSET_UP_EXAMPLES: &str = "\
Examples:
  # Install a toolset; the second file supplies its image and tool list
  sycoadm toolset up -f examples/sycoadm/toolsets/stdlib/values.yaml \\
      -f stdlib-image.yaml

  # Install the same toolset into another tenant
  sycoadm toolset up -f examples/sycoadm/toolsets/stdlib/values.yaml \\
      -f stdlib-image.yaml --set toolset.tenant=staging";

const TOOLSET_DOWN_EXAMPLES: &str = "\
Examples:
  # Remove the stdlib toolset from tenant e2e-test
  sycoadm toolset down stdlib --tenant e2e-test";

const WORKSPACE_UP_EXAMPLES: &str = "\
Examples:
  # Install a workspace (its models and toolsets must already be installed)
  sycoadm workspace up -f examples/sycoadm/workspaces/hello-world.yaml

  # Install it into another tenant
  sycoadm workspace up -f examples/sycoadm/workspaces/hello-world.yaml \\
      --set workspace.tenant=staging";

const WORKSPACE_DOWN_EXAMPLES: &str = "\
Examples:
  # Remove the hello-world workspace from tenant e2e-test
  sycoadm workspace down hello-world --tenant e2e-test";

const INFERENCE_UP_EXAMPLES: &str = "\
Examples:
  # Install an in-cluster model
  sycoadm inference up -f examples/sycoadm/inference/local.yaml

  # Install a remote model (its API key secret must already exist)
  sycoadm inference up -f examples/sycoadm/inference/deepseek-v4-flash.yaml";

const INFERENCE_DOWN_EXAMPLES: &str = "\
Examples:
  # Remove the local model from tenant e2e-test
  sycoadm inference down local --tenant e2e-test";

const CHANNEL_UP_EXAMPLES: &str = "\
Examples:
  # Install the app channel
  sycoadm channel up -f examples/sycoadm/channels/app.yaml";

const CHANNEL_DOWN_EXAMPLES: &str = "\
Examples:
  # Remove the app channel from tenant e2e-test
  sycoadm channel down app --tenant e2e-test";

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
    fn top_level_help_explains_key_commands() {
        use clap::CommandFactory;

        let help = Cli::command().render_help().to_string();
        for expected in [
            "Administer Sycophant clusters",
            "Set up a local Sycophant cluster",
            "Delete the cluster and all tenant data (irreversible)",
            "Check or upgrade the cluster and its tenants",
            "Manage tenant lifecycle, secrets, and audits",
            "Provision a framework user from a values file",
        ] {
            assert!(help.contains(expected), "help missing: {expected}");
        }
    }
}
