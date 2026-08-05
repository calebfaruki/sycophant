//! `syco provider set/list/delete` — content-tier Provider CR management.
//!
//! `set`/`delete` author only the Provider CR; llm-job egress is enforced by the
//! chart baseline, not recomputed here.

use serde::Serialize;

use crate::cli::{ProviderCmd, ProviderList, ProviderSet, ProviderSub};
use crate::commands::common;
use crate::providers;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: ProviderCmd) -> Result<(), String> {
    match cmd.sub {
        ProviderSub::Set(set) => do_set(scope, set),
        ProviderSub::List(list) => do_list(scope, list),
        ProviderSub::Delete(del) => do_delete(scope, &del.name),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderEntry {
    pub name: String,
    pub format: String,
    pub base_url: String,
}

fn do_set(scope: &Scope, cmd: ProviderSet) -> Result<(), String> {
    let preset = providers::lookup(&cmd.name)?;
    let secret_name = cmd.secret.as_deref().ok_or_else(|| {
        "--secret <name> is required (the provider needs credentials).\n  \
         Create one first:  echo $API_KEY | syco secret set <name>"
            .to_string()
    })?;
    let namespace = scope.release_name()?;

    let yaml = crate::commands::model::build_provider_cr(
        preset,
        &namespace,
        cmd.base_url.as_deref(),
        secret_name,
    );
    run_stdin("kubectl", &["apply", "-n", &namespace, "-f", "-"], &yaml)?;

    eprintln!("Provider '{}' configured.", cmd.name);
    Ok(())
}

fn do_list(scope: &Scope, cmd: ProviderList) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let output = run_output(
        "kubectl",
        &[
            "get",
            "providers.sycophant.md",
            "-n",
            &namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\t\"}{.spec.format}{\"\\t\"}{.spec.baseUrl}{\"\\n\"}{end}",
        ],
    )?;
    let entries = parse_provider_entries(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No providers configured.");
        return Ok(());
    }

    eprintln!("{:<16} {:<12} BASE URL", "NAME", "FORMAT");
    for e in &entries {
        eprintln!("{:<16} {:<12} {}", e.name, e.format, e.base_url);
    }
    Ok(())
}

/// Parse the tab-separated `kubectl get providers` jsonpath output into entries.
pub(crate) fn parse_provider_entries(kubectl_output: &str) -> Vec<ProviderEntry> {
    kubectl_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let name = cols.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(ProviderEntry {
                name,
                format: cols.next().unwrap_or_default().trim().to_string(),
                base_url: cols.next().unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

fn do_delete(scope: &Scope, name: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let deleted = common::delete_cr("provider.sycophant.md", name, &namespace)?;

    if deleted {
        eprintln!("Provider '{name}' deleted.");
    } else {
        eprintln!("Provider '{name}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_entries_splits_columns() {
        let entries =
            parse_provider_entries("anthropic\tanthropic\thttps://api.anthropic.com/v1\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "anthropic");
        assert_eq!(entries[0].format, "anthropic");
        assert_eq!(entries[0].base_url, "https://api.anthropic.com/v1");
    }

    #[test]
    fn parse_provider_entries_empty_input() {
        assert!(parse_provider_entries("").is_empty());
        assert!(parse_provider_entries("  \n \n").is_empty());
    }

    #[test]
    fn provider_entry_serializes_camel_case() {
        let e = ProviderEntry {
            name: "anthropic".into(),
            format: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"baseUrl\":\"https://api.anthropic.com/v1\""));
    }
}
