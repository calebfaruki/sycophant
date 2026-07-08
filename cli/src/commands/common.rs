//! Helpers shared across `syco` subcommands: progress output and the `kubectl`
//! CR-delete idiom the content-tier commands repeat. The tenant namespace is
//! created declaratively by the chart (`syco tenant up`), never here.

use crate::runner::run_output;

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

/// Split `kubectl ... -o jsonpath` tab/newline output into rows of trimmed
/// columns, dropping blank lines. Read columns with [`col`], which yields "" past
/// the end so a row with fewer columns than expected still maps cleanly.
pub(crate) fn parse_tab_rows(kubectl_output: &str) -> Vec<Vec<String>> {
    kubectl_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|line| line.split('\t').map(|c| c.trim().to_string()).collect())
        .collect()
}

/// Column `i` of a parsed row (see [`parse_tab_rows`]), or "" if past the end.
pub(crate) fn col(row: &[String], i: usize) -> String {
    row.get(i).cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tab_rows_trims_columns_and_drops_blank_lines() {
        let rows = parse_tab_rows("  a\tS3 \n\n  \nb\n");
        assert_eq!(
            rows,
            vec![
                vec!["a".to_string(), "S3".to_string()],
                vec!["b".to_string()]
            ]
        );
        assert_eq!(col(&rows[0], 1), "S3");
        assert_eq!(col(&rows[1], 1), ""); // past end → ""
    }
}
