use std::io::{self, IsTerminal, Read};

use crate::cli::{SecretCmd, SecretSub};
use crate::runner::{run_output, run_silent, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: SecretCmd) -> Result<(), String> {
    match cmd.sub {
        SecretSub::Set(set) => do_set(scope, &set.name),
        SecretSub::List(_) => do_list(scope),
    }
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

    if value.is_empty() {
        return Err("stdin was empty, no secret value provided".into());
    }

    let _ = run_silent("kubectl", &["create", "namespace", &namespace]);

    let yaml = build_secret_yaml(name, &namespace, &value);
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;
    eprintln!("Secret '{name}' created.");
    Ok(())
}

fn do_list(scope: &Scope) -> Result<(), String> {
    let namespace = scope.release_name()?;

    let output = run_output(
        "kubectl",
        &[
            "get",
            "secrets",
            "-n",
            &namespace,
            "-l",
            "sycophant.io/type=secret",
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}{end}",
        ],
    )?;

    if output.trim().is_empty() {
        eprintln!("No secrets configured.");
        return Ok(());
    }

    eprintln!("NAME");
    for line in output.trim().lines() {
        eprintln!("{line}");
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
    sycophant.io/type: secret
stringData:
  {name}: {escaped}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(yaml.contains("sycophant.io/type: secret"));
    }

    #[test]
    fn build_secret_yaml_special_characters() {
        let yaml = build_secret_yaml("test", "ns", "value with \"quotes\" and \\ backslash");
        assert!(yaml.contains("test:"));
        assert!(yaml.contains("quotes"));
    }
}
