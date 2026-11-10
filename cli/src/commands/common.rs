//! Helpers shared across `sycoadm` subcommands: progress output, polling, and the
//! `kubectl` CR-delete idiom the content-tier commands repeat.

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::runner::run_output;

/// Poll `check` every 2s until it holds or `secs` pass.
pub(crate) fn wait_for(
    desc: &str,
    secs: u64,
    mut check: impl FnMut() -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if check() {
            ok(desc);
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out after {secs}s waiting for: {desc}"));
        }
        sleep(Duration::from_secs(2));
    }
}

/// Print a cyan `==> step` heading to stderr.
pub(crate) fn step(msg: &str) {
    eprintln!("\n\x1b[1;36m==> {msg}\x1b[0m");
}

/// Print a green check + message to stderr.
pub(crate) fn ok(msg: &str) {
    eprintln!("\x1b[1;32m \u{2713}\x1b[0m {msg}");
}

/// Print a yellow warning + message to stderr.
pub(crate) fn warn(msg: &str) {
    eprintln!("\x1b[1;33m \u{26a0}\x1b[0m {msg}");
}

/// `kubectl delete <kind> <name> -n <ns> --ignore-not-found`. Returns true if the
/// resource existed and was deleted, false if it was already absent.
pub(crate) fn delete_cr(kind: &str, name: &str, namespace: &str) -> Result<bool, String> {
    let result = run_output(
        "kubectl",
        &["delete", kind, name, "-n", namespace, "--ignore-not-found"],
    )?;
    Ok(result.contains("deleted"))
}
