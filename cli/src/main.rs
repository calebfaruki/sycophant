mod assets;
mod cli;
mod commands;
mod grpc;
mod providers;
mod runner;
mod scope;
mod sync;
mod values;

use std::process;

use argh::FromArgs;
use cli::Command;

fn main() {
    // Linux exit-code convention: 0 success, 1 runtime error, 2 usage error.
    // argh's default `from_env()` exits 1 on parse failures, which collapses
    // usage errors into runtime errors. Handle parse failures explicitly so
    // `--help` still exits 0 and parse errors get the conventional 2.
    let args: Vec<String> = std::env::args().collect();
    let cmd_name = [args[0].as_str()];
    let arg_strs: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let cli: cli::Cli = match cli::Cli::from_args(&cmd_name, &arg_strs) {
        Ok(c) => c,
        Err(early_exit) => match early_exit.status {
            Ok(()) => {
                print!("{}", early_exit.output);
                process::exit(0);
            }
            Err(()) => {
                eprintln!("{}", early_exit.output);
                process::exit(2);
            }
        },
    };

    let result = match cli.command {
        Command::Init(cmd) => commands::init::run(cmd),
        Command::Up(_) => with_scope(commands::up::run),
        Command::Down(_) => with_scope(commands::down::run),
        Command::Model(cmd) => with_scope(|s| commands::model::run(s, cmd)),
        Command::Secret(cmd) => with_scope(|s| commands::secret::run(s, cmd)),
        Command::Workspace(cmd) => with_scope(|s| commands::workspace::run(s, cmd)),
        Command::Chat(cmd) => with_scope(|s| commands::chat::run(s, cmd)),
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
