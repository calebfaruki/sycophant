mod assets;
mod cli;
mod commands;
mod interrupt;
mod runner;
mod scope;
mod sync;

use std::process;

use clap::Parser;
use cli::Command;

fn main() {
    install_crash_reporter();

    // clap handles `--help`/`--version` (exit 0) and usage errors (exit 2)
    // natively; we keep exit 1 for runtime errors below.
    let cli = cli::Cli::parse();

    let result = match cli.command {
        Command::Doctor(_) => commands::doctor::run(),
        Command::Setup(cmd) => commands::setup::run(cmd),
        Command::Destroy(_) => commands::destroy::run(),
        Command::Upgrade(cmd) => commands::upgrade::run(cmd),
        Command::Tenant(cmd) => commands::tenant::run(cmd),
        Command::Connect(cmd) => commands::connect::run(cmd),
        Command::Toolset(cmd) => component("toolset", cmd),
        Command::Workspace(cmd) => component("workspace", cmd),
        Command::Instructions(cmd) => component("instructions", cmd),
        Command::User(cli::UserCmd { sub }) => match sub {
            cli::UserSub::Up(c) => commands::command::up("user", &c.values),
            cli::UserSub::Down(c) => commands::command::user_down(&c.subject, &c.tenant),
        },
        Command::Inference(cmd) => component("inference", cmd),
        Command::Channel(cmd) => component("channel", cmd),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}

fn component(kind: &str, cmd: cli::ComponentCmd) -> Result<(), String> {
    match cmd.sub {
        cli::ComponentSub::Up(c) => commands::command::up(kind, &c.values),
        cli::ComponentSub::Down(c) => commands::command::down(kind, &c.name, &c.tenant),
    }
}

fn install_crash_reporter() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = serde_json::json!({
            "panic": info.to_string(),
            "backtrace": backtrace.to_string(),
        });
        let body = serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string());
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("syco-debug-{secs}.json");

        // Crash reports land under the syco config dir (<config>/crash),
        // never the operator's cwd. Fall back to the temp dir if HOME is unset or
        // the config dir isn't writable.
        let path = scope::config_dir()
            .map(|dir| dir.join("crash"))
            .filter(|dir| std::fs::create_dir_all(dir).is_ok())
            .map(|dir| dir.join(&name))
            .filter(|p| std::fs::write(p, &body).is_ok())
            .unwrap_or_else(|| {
                let tmp = std::env::temp_dir().join(&name);
                let _ = std::fs::write(&tmp, &body);
                tmp
            });
        eprintln!("syco panicked; crash report written to {}", path.display());
    }));
}
