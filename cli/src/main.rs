mod assets;
mod cli;
mod commands;
mod runner;
mod scope;
mod sync;

use std::process;

use cli::Command;

fn main() {
    let cli: cli::Cli = argh::from_env();

    let result = match cli.command {
        Command::Init(cmd) => commands::init::run(cmd),
        Command::Up(_) => with_scope(commands::up::run),
        Command::Down(_) => with_scope(commands::down::run),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn with_scope(f: fn(&scope::Scope) -> Result<(), String>) -> Result<(), String> {
    let scope = scope::resolve()?;
    f(&scope)
}
