mod assets;
mod cli;
mod cnp;
mod commands;
mod providers;
mod runner;
mod scope;
mod sync;
mod values;

use std::process;

use clap::Parser;
use cli::Command;

fn main() {
    // clap handles `--help`/`--version` (exit 0) and usage errors (exit 2)
    // natively; we keep exit 1 for runtime errors below.
    let cli = cli::Cli::parse();

    let result = match cli.command {
        Command::Setup(cmd) => commands::setup::run(cmd),
        Command::Destroy(_) => commands::destroy::run(),
        Command::Tenant(cmd) => commands::tenant::run(cmd),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}
