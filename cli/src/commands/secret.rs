use std::io::{self, IsTerminal, Read};

use serde::Serialize;

use crate::cli::{SecretCmd, SecretList, SecretSub};
use crate::commands::common;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: SecretCmd) -> Result<(), String> {
    match cmd.sub {
        SecretSub::Set(set) => do_set(scope, &set.name),
        SecretSub::List(list) => do_list(scope, list),
        SecretSub::Delete(del) => do_delete(scope, &del.name),
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SecretEntry {
    pub name: String,
}

pub(crate) fn parse_secret_list(kubectl_output: &str) -> Vec<SecretEntry> {
    kubectl_output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| SecretEntry {
            name: s.to_string(),
        })
        .collect()
}

fn do_set(scope: &Scope, name: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;

    if io::stdin().is_terminal() {
        return Err("Secret value must be provided via stdin.\n  \
             API key:  echo $API_KEY | syco secret set <name>\n  \
             File:     syco secret set <name> < path/to/file"
            .into());
    }

    let mut value = String::new();
    io::stdin()
        .read_to_string(&mut value)
        .map_err(|e| format!("failed to read stdin: {e}"))?;

    reject_blank(&value)?;

    let yaml = build_secret_yaml(name, &namespace, &value);
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;
    eprintln!("Secret '{name}' created.");
    Ok(())
}

fn do_list(scope: &Scope, cmd: SecretList) -> Result<(), String> {
    let namespace = scope.release_name()?;

    let output = run_output(
        "kubectl",
        &[
            "get",
            "secrets",
            "-n",
            &namespace,
            "-l",
            "sycophant.md/type=secret",
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
        ],
    )?;

    let entries = parse_secret_list(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No secrets configured.");
        return Ok(());
    }

    eprintln!("NAME");
    for entry in &entries {
        eprintln!("{}", entry.name);
    }

    Ok(())
}

/// Whitespace-only counts as blank: the consumer trims before use.
fn reject_blank(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("stdin was empty, no secret value provided".into());
    }
    Ok(())
}

fn build_secret_yaml(name: &str, namespace: &str, value: &str) -> String {
    let escaped = serde_json::to_string(value).unwrap_or_default();

    format!(
        r#"apiVersion: v1
kind: Secret
metadata:
  name: {name}
  namespace: {namespace}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: secret
stringData:
  {name}: {escaped}
"#
    )
}

fn do_delete(scope: &Scope, name: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    if common::delete_cr("secret", name, &namespace)? {
        eprintln!("Secret '{name}' deleted.");
    } else {
        eprintln!("Secret '{name}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_blank_refuses_whitespace_only_input() {
        assert!(reject_blank("").is_err());
        assert!(reject_blank("\n").is_err());
        assert!(reject_blank("  \t\n ").is_err());
    }

    #[test]
    fn reject_blank_accepts_a_key_carrying_a_trailing_newline() {
        assert!(reject_blank("sk-abc123\n").is_ok());
    }

    #[test]
    fn build_secret_yaml_entry_key_matches_name() {
        let yaml = build_secret_yaml("my-api-key", "dev", "sk-abc123");
        assert!(yaml.contains("name: my-api-key"));
        assert!(yaml.contains("namespace: dev"));
        assert!(yaml.contains("my-api-key:"));
        assert!(yaml.contains("sk-abc123"));
    }

    #[test]
    fn build_secret_yaml_multiline_value() {
        let pem =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nbase64data\n-----END OPENSSH PRIVATE KEY-----\n";
        let yaml = build_secret_yaml("ssh-key", "dev", pem);
        assert!(yaml.contains("ssh-key:"));
        assert!(yaml.contains("BEGIN OPENSSH PRIVATE KEY"));
    }

    #[test]
    fn build_secret_yaml_has_labels() {
        let yaml = build_secret_yaml("test", "ns", "val");
        assert!(yaml.contains("app.kubernetes.io/part-of: sycophant"));
        assert!(yaml.contains("sycophant.md/type: secret"));
    }

    #[test]
    fn build_secret_yaml_special_characters() {
        let yaml = build_secret_yaml("test", "ns", "value with \"quotes\" and \\ backslash");
        assert!(yaml.contains("test:"));
        assert!(yaml.contains("quotes"));
    }

    #[test]
    fn parse_secret_list_returns_empty_for_empty_input() {
        assert_eq!(parse_secret_list(""), Vec::<SecretEntry>::new());
        assert_eq!(parse_secret_list("\n\n  \n"), Vec::<SecretEntry>::new());
    }

    #[test]
    fn parse_secret_list_keeps_each_line_as_entry() {
        let input = "alpha\nbeta\ngamma\n";
        let entries = parse_secret_list(input);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "alpha");
        assert_eq!(entries[1].name, "beta");
        assert_eq!(entries[2].name, "gamma");
    }

    #[test]
    fn parse_secret_list_strips_blanks_within() {
        let input = "alpha\n\nbeta\n   \ngamma\n";
        let entries = parse_secret_list(input);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn parse_secret_list_trims_whitespace_per_line() {
        let input = "  alpha  \n\tbeta\t\n";
        let entries = parse_secret_list(input);
        assert_eq!(entries[0].name, "alpha");
        assert_eq!(entries[1].name, "beta");
    }
}
