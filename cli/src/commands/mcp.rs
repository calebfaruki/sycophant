use super::util::{decode_field, delete_secret, parse_flag};
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(_scope: &Scope, args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()) {
        Some("set") => {
            let name = args.get(1).ok_or("usage: syco mcp set <name> --url <url> [--auth-token-file <path>] [--tools <t1,t2>]")?;
            set(name, &args[2..])
        }
        Some("list") => list(),
        Some("delete") => {
            let name = args.get(1).ok_or("usage: syco mcp delete <name>")?;
            delete(name)
        }
        _ => Err("usage: syco mcp <set|list|delete>".into()),
    }
}

fn set(name: &str, args: &[String]) -> Result<(), String> {
    let url = parse_flag(args, "--url").ok_or("--url is required")?;
    if url.is_empty() {
        return Err("--url cannot be empty".into());
    }

    let auth_token = match parse_flag(args, "--auth-token-file") {
        Some(path) => {
            let content =
                std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
            Some(content.trim().to_string())
        }
        None => None,
    };

    let tools = parse_flag(args, "--tools").map(|t| {
        t.split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    });

    let mut string_data = format!("  url: {url}\n");
    if let Some(ref token) = auth_token {
        string_data.push_str(&format!("  auth_token: {token}\n"));
    }
    if let Some(ref tools) = tools {
        string_data.push_str(&format!(
            "  tools: |\n    {}\n",
            tools.replace('\n', "\n    ")
        ));
    }

    let yaml = format!(
        r#"apiVersion: v1
kind: Secret
metadata:
  name: sycophant-mcp-{name}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.io/type: mcp
stringData:
{string_data}"#
    );

    run_stdin("kubectl", &["apply", "-f", "-"], &yaml)?;
    eprintln!("MCP server '{name}' configured.");
    Ok(())
}

fn list() -> Result<(), String> {
    let output = run_output(
        "kubectl",
        &[
            "get",
            "secrets",
            "-l",
            "sycophant.io/type=mcp",
            "-o",
            "json",
        ],
    )?;

    let json: serde_json::Value =
        serde_json::from_str(&output).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let items = json["items"].as_array();
    match items {
        Some(items) if !items.is_empty() => {
            eprintln!("{:<16} URL", "NAME");
            for item in items {
                let full_name = item["metadata"]["name"].as_str().unwrap_or("");
                let name = full_name
                    .strip_prefix("sycophant-mcp-")
                    .unwrap_or(full_name);
                let url = decode_field(item, "url");
                eprintln!("{name:<16} {url}");
            }
        }
        _ => eprintln!("No MCP servers configured."),
    }

    Ok(())
}

fn delete(name: &str) -> Result<(), String> {
    delete_secret("sycophant-mcp-", name, "MCP server")
}
