mod assets;
mod commands;
mod runner;
mod scope;
mod sync;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    let result = match args.get(1).map(|s| s.as_str()) {
        Some("init") => commands::init::run(&args[2..]),
        Some(cmd @ ("up" | "down" | "status" | "llm" | "mcp" | "workspace")) => {
            let scope = match scope::resolve() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            };
            match cmd {
                "up" => commands::up::run(&scope),
                "down" => commands::down::run(&scope),
                "status" => commands::status::run(&scope),
                "llm" => commands::llm::run(&scope, &args[2..]),
                "mcp" => commands::mcp::run(&scope, &args[2..]),
                "workspace" => commands::workspace::run(&scope, &args[2..]),
                _ => unreachable!(),
            }
        }
        _ => {
            eprintln!("Usage: syco <init|up|down|status|workspace|llm|mcp>");
            process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}
