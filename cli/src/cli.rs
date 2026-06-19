use argh::FromArgs;

#[derive(FromArgs)]
/// Sycophant CLI
pub(crate) struct Cli {
    #[argh(subcommand)]
    pub command: Command,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum Command {
    Init(InitCmd),
    Bootstrap(BootstrapCmd),
    Install(InstallCmd),
    Uninstall(UninstallCmd),
    Up(UpCmd),
    Down(DownCmd),
    Destroy(DestroyCmd),
    Model(ModelCmd),
    Provider(ProviderCmd),
    Client(ClientCmd),
    Secret(SecretCmd),
    Workspace(WorkspaceCmd),
    Chat(ChatCmd),
    Chamber(ChamberCmd),
}

// --- init ---

#[derive(FromArgs)]
#[argh(subcommand, name = "init")]
/// Initialize a sycophant environment
pub(crate) struct InitCmd {
    #[argh(subcommand)]
    pub target: InitTarget,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum InitTarget {
    Global(InitGlobal),
    Local(InitLocal),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "global")]
/// Initialize global scope (release name: sycophant)
pub(crate) struct InitGlobal {}

#[derive(FromArgs)]
#[argh(subcommand, name = "local")]
/// Initialize local scope (release name from directory name)
pub(crate) struct InitLocal {}

// --- bootstrap (optional, substitutable substrate defaults) ---

#[derive(FromArgs)]
#[argh(subcommand, name = "bootstrap")]
/// Install sycophant's default substrate (Cilium + the Kyverno engine) for a
/// fresh cluster. Optional — skip it if you bring your own CNI/Kyverno. Does NOT
/// provision the cluster or the gVisor node runtime.
pub(crate) struct BootstrapCmd {
    /// cilium chart version (default: 1.19.3)
    #[argh(option)]
    pub cilium_version: Option<String>,

    /// kyverno chart version (default: 3.5.3)
    #[argh(option)]
    pub kyverno_version: Option<String>,
}

// --- install ---

#[derive(FromArgs)]
#[argh(subcommand, name = "install")]
/// Install the sycophant cluster scope (CRDs, controllers' RBAC, policies,
/// gVisor RuntimeClass) into the current cluster. Cilium + Kyverno + the gVisor
/// node runtime are prerequisites you provision yourself. Run from the sycophant
/// repo root (charts/ available).
pub(crate) struct InstallCmd {
    /// helm release name for the cluster scope (default: sycophant-quickstart)
    #[argh(option)]
    pub release_name: Option<String>,

    /// namespace for the cluster-scope release (default: default)
    #[argh(option)]
    pub release_namespace: Option<String>,
}

// --- uninstall (cluster scope, destructive) ---

#[derive(FromArgs)]
#[argh(subcommand, name = "uninstall")]
/// Remove the sycophant cluster scope (destructive). Leaves the substrate
/// (Cilium/Kyverno/gVisor) in place.
pub(crate) struct UninstallCmd {
    /// helm release name for the cluster scope (default: sycophant-quickstart)
    #[argh(option)]
    pub release_name: Option<String>,

    /// namespace for the cluster-scope release (default: default)
    #[argh(option)]
    pub release_namespace: Option<String>,
}

// --- up / down / destroy (tenant) ---

#[derive(FromArgs)]
#[argh(subcommand, name = "up")]
/// Deploy or update the tenant for the current scope (data-safe)
pub(crate) struct UpCmd {}

#[derive(FromArgs)]
#[argh(subcommand, name = "down")]
/// Scale the current tenant to zero — stops compute, keeps all data
pub(crate) struct DownCmd {}

#[derive(FromArgs)]
#[argh(subcommand, name = "destroy")]
/// Destroy a tenant completely, including its PVCs/data (irreversible)
pub(crate) struct DestroyCmd {
    /// tenant name (its namespace)
    #[argh(positional)]
    pub tenant: String,
}

// --- model ---

#[derive(FromArgs)]
#[argh(subcommand, name = "model")]
/// Manage LLM model configurations
pub(crate) struct ModelCmd {
    #[argh(subcommand)]
    pub sub: ModelSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum ModelSub {
    Set(ModelSet),
    List(ModelList),
    Delete(ModelDelete),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Add or update a model
pub(crate) struct ModelSet {
    /// model name as expected by the provider
    #[argh(positional)]
    pub model: String,

    /// provider name (anthropic, openai, groq, etc.)
    #[argh(option)]
    pub provider: String,

    /// secret name for API key credentials (required; create with `syco secret set`)
    #[argh(option)]
    pub secret: Option<String>,

    /// thinking level (low, medium, high)
    #[argh(option)]
    pub thinking: Option<String>,

    /// override base URL (for custom endpoints)
    #[argh(option)]
    pub base_url: Option<String>,

    /// optional alias names for this model. Each alias becomes a duplicate
    /// model entry pointing at the same provider+model+secret. Use this when
    /// you want a model addressable by short or capability-shaped names
    /// (e.g., `--alias smart --alias default`). Repeatable.
    #[argh(option)]
    pub alias: Vec<String>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured models
pub(crate) struct ModelList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Remove a model
pub(crate) struct ModelDelete {
    /// model key (provider.model format)
    #[argh(positional)]
    pub key: String,
}

// --- provider ---

#[derive(FromArgs)]
#[argh(subcommand, name = "provider")]
/// Manage LLM providers and the llm-job egress allowlist
pub(crate) struct ProviderCmd {
    #[argh(subcommand)]
    pub sub: ProviderSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum ProviderSub {
    Set(ProviderSet),
    List(ProviderList),
    Delete(ProviderDelete),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Add or update a provider. Recomputes the llm-job egress allowlist (the union
/// of all providers' hosts) and applies it from outside the tenant.
pub(crate) struct ProviderSet {
    /// provider name (anthropic, openai, mistral, groq, ...)
    #[argh(positional)]
    pub name: String,

    /// secret name for API key credentials (required; create with `syco secret set`)
    #[argh(option)]
    pub secret: Option<String>,

    /// override base URL (for custom endpoints)
    #[argh(option)]
    pub base_url: Option<String>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured providers
pub(crate) struct ProviderList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Remove a provider (recomputes/shrinks the llm-job egress allowlist)
pub(crate) struct ProviderDelete {
    /// provider name
    #[argh(positional)]
    pub name: String,
}

// --- client ---

#[derive(FromArgs)]
#[argh(subcommand, name = "client")]
/// Manage external-client enrollment authorizations
pub(crate) struct ClientCmd {
    #[argh(subcommand)]
    pub sub: ClientSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum ClientSub {
    Set(ClientSet),
    List(ClientList),
    Delete(ClientDelete),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Add or update a client and the workspaces it may act on
pub(crate) struct ClientSet {
    /// client name (the device identity / signature kid)
    #[argh(positional)]
    pub name: String,

    /// a workspace this client may act on. Repeatable; at least one required.
    /// The union is the authorized set gated against the per-request workspace
    /// assertion at verify time.
    #[argh(option)]
    pub workspace: Vec<String>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured clients
pub(crate) struct ClientList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Remove a client
pub(crate) struct ClientDelete {
    /// client name
    #[argh(positional)]
    pub name: String,
}

// --- secret ---

#[derive(FromArgs)]
#[argh(subcommand, name = "secret")]
/// Manage secrets
pub(crate) struct SecretCmd {
    #[argh(subcommand)]
    pub sub: SecretSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum SecretSub {
    Set(SecretSet),
    List(SecretList),
    Delete(SecretDelete),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Create a secret from stdin
pub(crate) struct SecretSet {
    /// secret name
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List secrets
pub(crate) struct SecretList {
    /// emit JSON to stdout instead of human-readable list to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Delete a secret
pub(crate) struct SecretDelete {
    /// secret name
    #[argh(positional)]
    pub name: String,
}

// --- workspace ---

#[derive(FromArgs)]
#[argh(subcommand, name = "workspace")]
/// Manage workspaces
pub(crate) struct WorkspaceCmd {
    #[argh(subcommand)]
    pub sub: WorkspaceSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum WorkspaceSub {
    Create(WorkspaceCreate),
    List(WorkspaceList),
    Show(WorkspaceShow),
    Delete(WorkspaceDelete),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "create")]
/// Create a new workspace
pub(crate) struct WorkspaceCreate {
    /// workspace name
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured workspaces
pub(crate) struct WorkspaceList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "show")]
/// Show workspace details
pub(crate) struct WorkspaceShow {
    /// workspace name
    #[argh(positional)]
    pub name: String,

    /// emit JSON to stdout instead of human-readable output to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Delete a workspace
pub(crate) struct WorkspaceDelete {
    /// workspace name
    #[argh(positional)]
    pub name: String,
}

// --- chat ---

#[derive(FromArgs)]
#[argh(subcommand, name = "chat")]
/// Send a message to a workspace (reads from stdin)
pub(crate) struct ChatCmd {
    /// workspace name
    #[argh(positional)]
    pub workspace: String,
}

// --- chamber ---

#[derive(FromArgs)]
#[argh(subcommand, name = "chamber")]
/// Manage airlock chambers (set/list/delete) and lint chamber images
pub(crate) struct ChamberCmd {
    #[argh(subcommand)]
    pub sub: ChamberSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum ChamberSub {
    Set(ChamberSet),
    List(ChamberList),
    Delete(ChamberDelete),
    Lint(ChamberLint),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Add or update a chamber. Its egress CiliumNetworkPolicy is applied alongside
/// the CR by syco (from outside the tenant), composing on the chart baseline.
pub(crate) struct ChamberSet {
    /// chamber name (CR metadata.name; referenced by workspaces[].chambers)
    #[argh(positional)]
    pub name: String,

    /// OCI image exposing the md.sycophant.tools LABEL. Omit for a no-tool
    /// chamber (e.g. a pure-egress placeholder).
    #[argh(option)]
    pub image: Option<String>,

    /// egress allowlist entry as domain:port (repeatable).
    /// e.g. --egress notion.com:443 --egress github.com:22
    #[argh(option)]
    pub egress: Vec<String>,

    /// credential mapping secret=NAME,env=VAR or secret=NAME,file=PATH
    /// (repeatable; exactly one of env/file per entry)
    #[argh(option)]
    pub credential: Vec<String>,

    /// keep the chamber pod alive for the workspace lifetime (hot-path tools
    /// like git); default false (spawn-per-call)
    #[argh(switch)]
    pub keepalive: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured chambers
pub(crate) struct ChamberList {
    /// emit JSON to stdout instead of human-readable table to stderr
    #[argh(switch)]
    pub json: bool,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Delete a chamber (also deletes its egress CNP)
pub(crate) struct ChamberDelete {
    /// chamber name
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "lint")]
/// Statically check a chamber directory for shell-injection vulnerabilities
/// in its dispatch and Makefile against the LABEL-declared schema vars.
pub(crate) struct ChamberLint {
    /// path to the chamber directory (must contain a Dockerfile with the
    /// md.sycophant.tools LABEL; dispatch and Makefile are linted if present)
    #[argh(positional)]
    pub path: String,
}
