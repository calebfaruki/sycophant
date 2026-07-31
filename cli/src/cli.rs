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
    /// tenant namespace (required for every subcommand except `chamber lint`)
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
    Model(ModelCmd),
    Provider(ProviderCmd),
    Enrollment(EnrollmentCmd),
    Kernel(KernelCmd),
    Secret(SecretCmd),
    Workspace(WorkspaceCmd),
    Chamber(ChamberCmd),
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

// --- model ---

/// Manage LLM model configurations
#[derive(Args)]
pub(crate) struct ModelCmd {
    #[command(subcommand)]
    pub sub: ModelSub,
}

#[derive(Subcommand)]
pub(crate) enum ModelSub {
    Set(ModelSet),
    List(ModelList),
    Delete(ModelDelete),
}

/// Add or update a model
#[derive(Args)]
pub(crate) struct ModelSet {
    /// model name as expected by the provider
    pub model: String,

    /// provider name (anthropic, openai, groq, etc.)
    #[arg(long)]
    pub provider: String,

    /// secret name for API key credentials (required; create with `syco tenant secret set`)
    #[arg(long)]
    pub secret: Option<String>,

    /// thinking level (low, medium, high)
    #[arg(long)]
    pub thinking: Option<String>,

    /// override base URL (for custom endpoints)
    #[arg(long)]
    pub base_url: Option<String>,

    /// optional alias names for this model. Each alias becomes a duplicate
    /// model entry pointing at the same provider+model+secret. Repeatable.
    #[arg(long)]
    pub alias: Vec<String>,
}

/// List configured models
#[derive(Args)]
pub(crate) struct ModelList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[arg(long)]
    pub json: bool,
}

/// Remove a model
#[derive(Args)]
pub(crate) struct ModelDelete {
    /// model key (provider.model format)
    pub key: String,
}

// --- provider ---

/// Manage LLM providers and the llm-job egress allowlist
#[derive(Args)]
pub(crate) struct ProviderCmd {
    #[command(subcommand)]
    pub sub: ProviderSub,
}

#[derive(Subcommand)]
pub(crate) enum ProviderSub {
    Set(ProviderSet),
    List(ProviderList),
    Delete(ProviderDelete),
}

/// Add or update a provider. Recomputes the llm-job egress allowlist (the union
/// of all providers' hosts) and applies it from outside the tenant.
#[derive(Args)]
pub(crate) struct ProviderSet {
    /// provider name (anthropic, openai, mistral, groq, ...)
    pub name: String,

    /// secret name for API key credentials (required; create with `syco tenant secret set`)
    #[arg(long)]
    pub secret: Option<String>,

    /// override base URL (for custom endpoints)
    #[arg(long)]
    pub base_url: Option<String>,
}

/// List configured providers
#[derive(Args)]
pub(crate) struct ProviderList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[arg(long)]
    pub json: bool,
}

/// Remove a provider (recomputes/shrinks the llm-job egress allowlist)
#[derive(Args)]
pub(crate) struct ProviderDelete {
    /// provider name
    pub name: String,
}

// --- enrollment ---

/// Manage device enrollment authorizations
#[derive(Args)]
pub(crate) struct EnrollmentCmd {
    #[command(subcommand)]
    pub sub: EnrollmentSub,
}

#[derive(Subcommand)]
pub(crate) enum EnrollmentSub {
    Set(EnrollmentSet),
    List(EnrollmentList),
    Delete(EnrollmentDelete),
}

/// Add or update an enrollment and the workspaces it may act on
#[derive(Args)]
pub(crate) struct EnrollmentSet {
    /// enrollment name (the device identity / signature kid)
    pub name: String,

    /// a workspace this device may act on. Repeatable; at least one required.
    /// The union is the authorized set gated against the per-request workspace
    /// assertion at verify time.
    #[arg(long)]
    pub workspace: Vec<String>,
}

/// List configured enrollments
#[derive(Args)]
pub(crate) struct EnrollmentList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[arg(long)]
    pub json: bool,
}

/// Remove an enrollment
#[derive(Args)]
pub(crate) struct EnrollmentDelete {
    /// enrollment name
    pub name: String,
}

// --- kernel ---

/// Manage per-workspace kernel (persona content) sources
#[derive(Args)]
pub(crate) struct KernelCmd {
    #[command(subcommand)]
    pub sub: KernelSub,
}

#[derive(Subcommand)]
#[allow(
    clippy::large_enum_variant,
    reason = "clap arg structs, parsed once at startup"
)]
pub(crate) enum KernelSub {
    Set(KernelSet),
    List(KernelList),
    Delete(KernelDelete),
}

/// Set or update a workspace's kernel source. Authors a host-path Kernel CR; run
/// `syco tenant up` afterwards to deliver it on the read-only serving volume.
#[derive(Args)]
pub(crate) struct KernelSet {
    /// workspace this kernel belongs to (the Kernel CR metadata.name)
    pub workspace: String,

    /// override the host source directory (absolute path). Absent →
    /// convention default <hostPathBase>/<namespace>/<workspace>.
    #[arg(long)]
    pub path: Option<String>,
}

/// List configured kernels
#[derive(Args)]
pub(crate) struct KernelList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[arg(long)]
    pub json: bool,
}

/// Remove a workspace's kernel
#[derive(Args)]
pub(crate) struct KernelDelete {
    /// workspace name (the Kernel CR metadata.name)
    pub workspace: String,
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
/// already have been exercised by a tool-calling message so the chamber pod exists.
#[derive(Args)]
pub(crate) struct AuditCmd {
    /// workspace name
    pub workspace: String,
}

// --- chamber ---

/// Manage airlock chambers (set/list/delete) and lint chamber images
#[derive(Args)]
pub(crate) struct ChamberCmd {
    #[command(subcommand)]
    pub sub: ChamberSub,
}

#[derive(Subcommand)]
pub(crate) enum ChamberSub {
    Set(ChamberSet),
    List(ChamberList),
    Delete(ChamberDelete),
    Lint(ChamberLint),
}

/// Add or update a chamber. Its egress CiliumNetworkPolicy is applied alongside
/// the CR by syco (from outside the tenant), composing on the chart baseline.
#[derive(Args)]
pub(crate) struct ChamberSet {
    /// chamber name (CR metadata.name; referenced by workspaces[].chambers)
    pub name: String,

    /// OCI image exposing the md.sycophant.tools LABEL. Omit for a no-tool
    /// chamber (e.g. a pure-egress placeholder).
    #[arg(long)]
    pub image: Option<String>,

    /// egress allowlist entry as domain:port (repeatable).
    /// e.g. --egress notion.com:443 --egress github.com:22
    #[arg(long)]
    pub egress: Vec<String>,

    /// credential mapping secret=NAME,env=VAR or secret=NAME,file=PATH
    /// (repeatable; exactly one of env/file per entry)
    #[arg(long)]
    pub credential: Vec<String>,

    /// keep the chamber pod alive for the workspace lifetime (hot-path tools
    /// like git); default false (spawn-per-call)
    #[arg(long)]
    pub keepalive: bool,
}

/// List configured chambers
#[derive(Args)]
pub(crate) struct ChamberList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[arg(long)]
    pub json: bool,
}

/// Delete a chamber (also deletes its egress CNP)
#[derive(Args)]
pub(crate) struct ChamberDelete {
    /// chamber name
    pub name: String,
}

/// Statically check a chamber directory for shell-injection vulnerabilities
/// in its dispatch and Makefile against the LABEL-declared schema vars.
#[derive(Args)]
pub(crate) struct ChamberLint {
    /// path to the chamber directory (must contain a Dockerfile with the
    /// md.sycophant.tools LABEL; dispatch and Makefile are linted if present)
    pub path: String,
}
