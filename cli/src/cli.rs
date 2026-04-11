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
    Agent(AgentCmd),
    Secret(SecretCmd),
    Workspace(WorkspaceCmd),
}

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
/// Initialize local scope in current directory
pub(crate) struct InitLocal {
    /// release name
    #[argh(positional)]
    pub name: String,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "up")]
/// Deploy to cluster
pub(crate) struct UpCmd {}

#[derive(FromArgs)]
#[argh(subcommand, name = "down")]
/// Stop and remove from cluster
pub(crate) struct DownCmd {}

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
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Add or update a model in values.yaml
pub(crate) struct ModelSet {
    /// model config name
    #[argh(positional)]
    pub name: String,

    /// provider format (anthropic or openai)
    #[argh(option)]
    pub format: String,

    /// model identifier
    #[argh(option)]
    pub model: String,

    /// API endpoint URL
    #[argh(option)]
    pub base_url: String,

    /// thinking level (low, medium, high)
    #[argh(option)]
    pub thinking: Option<String>,

    /// secret name for credentials
    #[argh(option)]
    pub secret: Option<String>,

    /// mount secret as env var (mutually exclusive with --secret-file)
    #[argh(option)]
    pub secret_env: Option<String>,

    /// mount secret as file (mutually exclusive with --secret-env)
    #[argh(option)]
    pub secret_file: Option<String>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured models
pub(crate) struct ModelList {}

#[derive(FromArgs)]
#[argh(subcommand, name = "agent")]
/// Manage agent configurations
pub(crate) struct AgentCmd {
    #[argh(subcommand)]
    pub sub: AgentSub,
}

#[derive(FromArgs)]
#[argh(subcommand)]
pub(crate) enum AgentSub {
    Set(AgentSet),
    List(AgentList),
}

#[derive(FromArgs)]
#[argh(subcommand, name = "set")]
/// Add or update an agent in values.yaml
pub(crate) struct AgentSet {
    /// agent name
    #[argh(positional)]
    pub name: String,

    /// model config name (must match a key in models)
    #[argh(option)]
    pub model: String,

    /// path to prompt directory
    #[argh(option)]
    pub prompt: String,

    /// agent description (used by auto-router for multi-agent workspaces)
    #[argh(option)]
    pub description: Option<String>,
}

#[derive(FromArgs)]
#[argh(subcommand, name = "list")]
/// List configured agents
pub(crate) struct AgentList {}

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
}

#[derive(FromArgs)]
#[argh(subcommand, name = "create")]
/// Create a new workspace
pub(crate) struct WorkspaceCreate {
    /// workspace name
    #[argh(positional)]
    pub name: String,

    /// container image (format: image:tag, default: sycophant-workspace-tools:latest)
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
