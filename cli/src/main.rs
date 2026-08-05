mod assets;
mod cli;
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
    install_crash_reporter();

    // clap handles `--help`/`--version` (exit 0) and usage errors (exit 2)
    // natively; we keep exit 1 for runtime errors below.
    let cli = cli::Cli::parse();

    let result = match cli.command {
        Command::Setup(cmd) => commands::setup::run(cmd),
        Command::Destroy(_) => commands::destroy::run(),
        Command::Upgrade(cmd) => commands::upgrade::run(cmd),
        Command::Tenant(cmd) => commands::tenant::run(cmd),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
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

        // Crash reports land under the syco config dir (~/.config/sycophant/crash),
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
