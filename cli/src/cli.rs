use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "syco", about = "Sycophant CLI", version)]
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
    Toolset(ToolsetCmd),
}

// --- setup / destroy (cluster — no --ns) ---

/// Stand up a sycophant-ready cluster from nothing: ensure the k3d cluster
/// `sycophant`, install the gVisor node runtime, Cilium, Kyverno, and the
/// sycophant cluster layer; scaffold the global config. Idempotent.
#[derive(Args)]
pub(crate) struct SetupCmd {}

/// Delete the sycophant k3d cluster, including every tenant and all data
/// (irreversible). Inverse of `setup`.
#[derive(Args)]
pub(crate) struct DestroyCmd {}

// --- upgrade (validate cluster + all tenants, then upgrade everything) ---

/// Validate the cluster and every tenant, then upgrade the whole platform in
/// one shot (cluster first). `--check` runs validation only, making no changes.
#[derive(Args)]
pub(crate) struct UpgradeCmd {
    /// run validation only; make no changes
    #[arg(long)]
    pub check: bool,
}

// --- tenant (everything namespace-scoped; --ns selects the namespace) ---

/// Operate on a tenant (namespace). `--ns <name>` selects it and may appear
/// anywhere on the line (it is a global flag declared once here).
#[derive(Args)]
pub(crate) struct TenantCmd {
    /// tenant namespace (required for every subcommand)
    #[arg(long, global = true)]
    pub ns: Option<String>,
    #[command(subcommand)]
    pub sub: TenantSub,
}

#[derive(Subcommand)]
pub(crate) enum TenantSub {
    Up(TenantUp),
    Down(TenantDown),
    Remove(TenantRemove),
    Secret(SecretCmd),
    Workspace(WorkspaceCmd),
    Audit(AuditCmd),
}

/// Deploy or update the tenant (data-safe)
#[derive(Args)]
pub(crate) struct TenantUp {}

/// Scale the tenant to zero — stops compute, keeps all data
#[derive(Args)]
pub(crate) struct TenantDown {}

/// Delete the tenant completely, including its PVCs/data (irreversible)
#[derive(Args)]
pub(crate) struct TenantRemove {}

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

// --- workspace ---

/// Manage workspaces
#[derive(Args)]
pub(crate) struct WorkspaceCmd {
    #[command(subcommand)]
    pub sub: WorkspaceSub,
}

#[derive(Subcommand)]
pub(crate) enum WorkspaceSub {
    Create(WorkspaceCreate),
    List(WorkspaceList),
    Show(WorkspaceShow),
    Delete(WorkspaceDelete),
}

/// Create a new workspace
#[derive(Args)]
pub(crate) struct WorkspaceCreate {
    /// workspace name
    pub name: String,
}

/// List configured workspaces
#[derive(Args)]
pub(crate) struct WorkspaceList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[arg(long)]
    pub json: bool,
}

/// Show workspace details
#[derive(Args)]
pub(crate) struct WorkspaceShow {
    /// workspace name
    pub name: String,

    /// emit JSON to stdout instead of human-readable output to stderr
    #[arg(long)]
    pub json: bool,
}

/// Delete a workspace
#[derive(Args)]
pub(crate) struct WorkspaceDelete {
    /// workspace name
    pub name: String,
}

// --- audit ---

/// Audit a running workspace against the security clauses (gVisor isolation,
/// secret scrubbing, egress containment, L7 DNS allowlist, credential isolation,
/// tool execution, workspace SA). Probes the live sandbox — the workspace must
/// already have been exercised by a tool-calling message so the toolset pod exists.
#[derive(Args)]
pub(crate) struct AuditCmd {
    /// workspace name
    pub workspace: String,
}

// --- toolset (cluster-independent — no --ns) ---

/// Inspect and build from toolset images
#[derive(Args)]
pub(crate) struct ToolsetCmd {
    #[command(subcommand)]
    pub sub: ToolsetSub,
}

#[derive(Subcommand)]
pub(crate) enum ToolsetSub {
    Lint(ToolsetLint),
    Manifest(ToolsetManifest),
}

/// Statically check a toolset directory for shell-injection vulnerabilities
/// in its dispatch and Makefile against the tools.yaml schema vars.
#[derive(Args)]
pub(crate) struct ToolsetLint {
    /// path to the toolset directory (must contain a Dockerfile and a
    /// tools.yaml; dispatch and Makefile are linted if present)
    pub path: String,
}

/// Read a built toolset image's baked schema (`/etc/toolset/tools.yaml`) and
/// emit its capability-manifest content to stdout. Fails closed (non-zero, no
/// output) for an image carrying no baked schema.
#[derive(Args)]
pub(crate) struct ToolsetManifest {
    /// image reference to read the baked schema from (must be present locally)
    pub image: String,

    /// owning toolset name recorded on each tool. Defaults to the image
    /// reference's repository basename.
    #[arg(long)]
    pub toolset: Option<String>,

    /// path to a grants file (YAML/JSON map of grant-name -> {secret, path?,
    /// egress?}) merged onto every tool in the emitted manifest.
    #[arg(long)]
    pub grants: Option<String>,
}
