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
    Up(UpCmd),
    Down(DownCmd),
    Model(ModelCmd),
    Secret(SecretCmd),
    Workspace(WorkspaceCmd),
    Chat(ChatCmd),
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

// --- up / down ---

#[derive(FromArgs)]
#[argh(subcommand, name = "up")]
/// Deploy to cluster
pub(crate) struct UpCmd {}

#[derive(FromArgs)]
#[argh(subcommand, name = "down")]
/// Stop and remove from cluster
pub(crate) struct DownCmd {}

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

    /// secret name for API key credentials
    #[argh(option)]
    pub secret: Option<String>,

    /// mount secret as file instead of env var
    #[argh(option)]
    pub secret_file: Option<String>,

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
pub(crate) struct ModelList {}

#[derive(FromArgs)]
#[argh(subcommand, name = "delete")]
/// Remove a model
pub(crate) struct ModelDelete {
    /// model key (provider.model format)
    #[argh(positional)]
    pub key: String,
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
pub(crate) struct SecretList {}

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

    /// container image (format: image:tag, default: sycophant-mainframe-runtime:latest)
    #[argh(option)]
    pub image: Option<String>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured workspaces
pub(crate) struct WorkspaceList {}

#[derive(FromArgs)]
#[argh(subcommand, name = "show")]
/// Show workspace details
pub(crate) struct WorkspaceShow {
    /// workspace name
    #[argh(positional)]
    pub name: String,
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
