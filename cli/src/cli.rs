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
