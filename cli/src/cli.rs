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
