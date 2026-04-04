mod assets;
mod cli;
mod commands;
mod runner;
mod scope;
mod sync;
mod values;

use std::process;

use cli::Command;

fn main() {
    let cli: cli::Cli = argh::from_env();

    let result = match cli.command {
        Command::Init(cmd) => commands::init::run(cmd),
        Command::Up(_) => with_scope(commands::up::run),
        Command::Down(_) => with_scope(commands::down::run),
        Command::Model(cmd) => with_scope(|s| commands::model::run(s, cmd)),
        Command::Agent(cmd) => with_scope(|s| commands::agent::run(s, cmd)),
        Command::Secret(cmd) => with_scope(|s| commands::secret::run(s, cmd)),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn with_scope<F>(f: F) -> Result<(), String>
where
    F: FnOnce(&scope::Scope) -> Result<(), String>,
{
    let scope = scope::resolve()?;
    f(&scope)
}
