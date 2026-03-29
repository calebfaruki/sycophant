use std::io::{self, Write};

use super::util::{decode_field, delete_secret};
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub fn run(_scope: &Scope, args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        Some("set") => {
            let name = args.get(1).ok_or("usage: syco llm set <name>")?;
            set(name)
        }
        Some("list") => list(),
        Some("delete") => {
            let name = args.get(1).ok_or("usage: syco llm delete <name>")?;
            delete(name)
        }
        _ => Err("usage: syco llm <set|list|delete>".into()),
    }
}

fn prompt_with_default(label: &str, default: &str) -> Result<String, String> {
    eprint!("{label} [{default}]: ");
    io::stderr().flush().ok();
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("failed to read input: {e}"))?;
    let input = input.trim();
    if input.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(input.to_string())
    }
}

fn set(name: &str) -> Result<(), String> {
    let provider = prompt_with_default("Provider", "anthropic")?;
    let model = prompt_with_default("Model", "claude-sonnet-4-20250514")?;
    let max_tokens = prompt_with_default("Max tokens", "8192")?;
    let api_key = rpassword::prompt_password("API key: ")
        .map_err(|e| format!("failed to read API key: {e}"))?;
    if api_key.is_empty() {
        return Err("API key cannot be empty.".into());
    }

    let yaml = format!(
        r#"apiVersion: v1
kind: Secret
metadata:
  name: sycophant-llm-{name}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.io/type: llm
stringData:
  provider: {provider}
  model: {model}
  max-tokens: "{max_tokens}"
  api-key: {api_key}
"#
    );

    run_stdin("kubectl", &["apply", "-f", "-"], &yaml)?;
    eprintln!("LLM provider '{name}' configured.");
    Ok(())
}

fn list() -> Result<(), String> {
    let output = run_output(
        "kubectl",
        &[
            "get",
            "secrets",
            "-l",
            "sycophant.io/type=llm",
            "-o",
            "json",
        ],
    )?;

    let json: serde_json::Value =
        serde_json::from_str(&output).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let items = json["items"].as_array();
    match items {
        Some(items) if !items.is_empty() => {
            eprintln!("{:<12} {:<12} MODEL", "NAME", "PROVIDER");
            for item in items {
                let full_name = item["metadata"]["name"].as_str().unwrap_or("");
                let name = full_name
                    .strip_prefix("sycophant-llm-")
                    .unwrap_or(full_name);
                let provider = decode_field(item, "provider");
                let model = decode_field(item, "model");
                eprintln!("{name:<12} {provider:<12} {model}");
            }
        }
        _ => eprintln!("No LLM providers configured."),
    }

    Ok(())
}

fn delete(name: &str) -> Result<(), String> {
    delete_secret("sycophant-llm-", name, "LLM provider")
}
