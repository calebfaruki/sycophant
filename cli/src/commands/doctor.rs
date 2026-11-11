use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Find the existing Sycophant preflight script.
fn find_preflight_script() -> Result<PathBuf, String> {
    let current_dir =
        env::current_dir().map_err(|err| format!("Cannot determine current directory: {err}"))?;

    for directory in current_dir.ancestors() {
        let script = directory.join("scripts/preflight.sh");

        if script.is_file() {
            return Ok(script);
        }
    }

    Err("Could not find scripts/preflight.sh. \
         Run doctor from inside a Sycophant repository checkout."
        .to_string())
}

/// Run read-only diagnostics using Sycophant's existing preflight script.
pub(crate) fn run() -> Result<(), String> {
    println!("Sycophant Doctor");
    println!();

    let script = find_preflight_script()?;

    run_preflight_command(Command::new("bash").arg(&script).env(
        "ARCH",
        env::var("ARCH").unwrap_or_else(|_| env::consts::ARCH.to_string()),
    ))
}

/// Execute the diagnostic command and report its result.
fn run_preflight_command(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|err| format!("Failed to run preflight: {err}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Environment diagnostics failed (exit status: {status})"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_succeeds_when_preflight_passes() {
        let result = run_preflight_command(Command::new("bash").arg("-c").arg("exit 0"));

        assert!(result.is_ok());
    }

    #[test]
    fn doctor_fails_when_preflight_fails() {
        let result = run_preflight_command(Command::new("bash").arg("-c").arg("exit 1"));

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Environment diagnostics failed"));
    }
}
