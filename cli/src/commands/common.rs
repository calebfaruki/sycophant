//! Helpers shared across `syco` subcommands: progress output, flag/env
//! resolution, repo-root discovery, and the kubectl idioms (namespace ensure,
//! CR delete) that the content-tier commands all repeat.

use std::env;
use std::path::{Path, PathBuf};

use crate::runner::{run_output, run_silent};

/// Print a cyan `==> step` heading to stderr.
pub(crate) fn step(msg: &str) {
    eprintln!("\n\x1b[1;36m==> {msg}\x1b[0m");
}

/// Print a green check + message to stderr.
pub(crate) fn ok(msg: &str) {
    eprintln!("\x1b[1;32m \u{2713}\x1b[0m {msg}");
}

/// Resolve a value from an explicit flag, then an env var, then a default.
pub(crate) fn resolve_arg(flag: Option<String>, env_var: &str, default: &str) -> String {
    flag.or_else(|| env::var(env_var).ok())
        .unwrap_or_else(|| default.to_string())
}

/// Walk up from the cwd until `marker` (e.g. `charts/kyverno-crds`) is found,
/// returning the directory that contains it. Errors if no ancestor matches.
pub(crate) fn find_repo_root(marker: &Path) -> Result<PathBuf, String> {
    let cwd = env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?;
    let mut dir: &Path = cwd.as_path();
    loop {
        if dir.join(marker).is_dir() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => {
                return Err(format!(
                    "could not find sycophant repo root (looking for {}) starting from {}",
                    marker.display(),
                    cwd.display()
                ));
            }
        }
    }
}

/// Best-effort namespace create so a subsequent `kubectl apply` doesn't fail on
/// a fresh tenant. Silently ignores "already exists".
pub(crate) fn ensure_namespace(namespace: &str) {
    let _ = run_silent("kubectl", &["create", "namespace", namespace]);
}

/// `kubectl delete <kind> <name> -n <ns> --ignore-not-found`. Returns true if the
/// resource existed and was deleted, false if it was already absent. Centralizes
/// the `--ignore-not-found` + output string-match the delete subcommands share.
pub(crate) fn delete_cr(kind: &str, name: &str, namespace: &str) -> Result<bool, String> {
    let result = run_output(
        "kubectl",
        &["delete", kind, name, "-n", namespace, "--ignore-not-found"],
    )?;
    Ok(result.contains("deleted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_arg_prefers_flag() {
        assert_eq!(
            resolve_arg(Some("X".into()), "NO_SUCH_VAR_ABC", "default"),
            "X"
        );
    }

    #[test]
    fn resolve_arg_falls_back_to_default_when_no_env() {
        assert_eq!(resolve_arg(None, "NO_SUCH_VAR_XYZ_12345", "default"), "default");
    }
}
