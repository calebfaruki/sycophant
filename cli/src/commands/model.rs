use serde::Serialize;

use crate::cli::{ModelCmd, ModelList, ModelSet, ModelSub};
use crate::commands::common;
use crate::providers;
use crate::runner::{run_output, run_stdin};
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: ModelCmd) -> Result<(), String> {
    match cmd.sub {
        ModelSub::Set(set) => do_set(scope, set),
        ModelSub::List(list) => do_list(scope, list),
        ModelSub::Delete(del) => do_delete(scope, &del.key),
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelEntry {
    pub key: String,
    pub provider: String,
    pub model: String,
}

/// Build a `Provider` CR for `kubectl apply`. Format + default baseUrl come from
/// the provider preset; `base_url_override` (from --base-url) wins when set.
/// `spec.secret.key` is set equal to `secret_name` because `syco secret set <name>`
/// stores the value under the data key `<name>` (see secret.rs::build_secret_yaml).
pub(crate) fn build_provider_cr(
    preset: &providers::ProviderPreset,
    namespace: &str,
    base_url_override: Option<&str>,
    secret_name: &str,
) -> String {
    let base_url = base_url_override.unwrap_or(preset.base_url);
    let base_url = serde_json::to_string(base_url).unwrap_or_default();
    let secret = serde_json::to_string(secret_name).unwrap_or_default();
    format!(
        r#"apiVersion: sycophant.md/v1
kind: Provider
metadata:
  name: {name}
  namespace: {namespace}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: provider
spec:
  format: {format}
  baseUrl: {base_url}
  secret:
    name: {secret}
    key: {secret}
"#,
        name = preset.name,
        format = preset.format,
    )
}

/// Map an arbitrary provider/model identifier into a DNS-1123-subdomain-safe
/// `metadata.name`: lowercase, keep `[a-z0-9.-]`, replace every other character
/// with `-`, then trim leading/trailing `-`/`.`. Only the k8s object name is
/// sanitized; `spec.model` keeps the raw tag (Ollama tags carry `:` `/` `_`).
fn sanitize_k8s_name(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches(|c| c == '-' || c == '.').to_string()
}

/// Build a `Model` CR for `kubectl apply`. `name` is the metadata name (the
/// canonical `<provider>.<model>` key or an alias). `--thinking` maps to the
/// provider-passthrough `spec.params.output_config.effort`.
fn build_model_cr(
    name: &str,
    namespace: &str,
    provider: &str,
    model: &str,
    thinking: Option<&str>,
) -> String {
    let name = sanitize_k8s_name(name);
    let model_q = serde_json::to_string(model).unwrap_or_default();
    let mut out = format!(
        r#"apiVersion: sycophant.md/v1
kind: Model
metadata:
  name: {name}
  namespace: {namespace}
  labels:
    app.kubernetes.io/part-of: sycophant
    sycophant.md/type: model
spec:
  providerRef:
    name: {provider}
  model: {model_q}
"#,
    );
    if let Some(effort) = thinking {
        let effort_q = serde_json::to_string(effort).unwrap_or_default();
        out.push_str(&format!(
            "  params:\n    output_config:\n      effort: {effort_q}\n"
        ));
    }
    out
}

fn do_set(scope: &Scope, cmd: ModelSet) -> Result<(), String> {
    let preset = providers::lookup(&cmd.provider)?;
    let secret_name = cmd.secret.as_deref().ok_or_else(|| {
        "--secret <name> is required (the provider needs credentials).\n  \
         Create one first:  echo $API_KEY | syco secret set <name>"
            .to_string()
    })?;
    let namespace = scope.release_name()?;
    let key = sanitize_k8s_name(&format!("{}.{}", cmd.provider, cmd.model));
    common::ensure_namespace(&namespace);

    // Upsert the Provider CR (idempotent: keyed by metadata.name == provider).
    let provider_yaml = build_provider_cr(preset, &namespace, cmd.base_url.as_deref(), secret_name);
    run_stdin(
        "kubectl",
        &["apply", "-n", &namespace, "-f", "-"],
        &provider_yaml,
    )?;

    // Keep the llm-job egress union current with the provider set.
    crate::cnp::reconcile_llm_egress_cnp(&namespace)?;

    // Upsert the canonical Model CR, then one per alias.
    for model_name in std::iter::once(key.as_str()).chain(cmd.alias.iter().map(String::as_str)) {
        let model_yaml = build_model_cr(
            model_name,
            &namespace,
            &cmd.provider,
            &cmd.model,
            cmd.thinking.as_deref(),
        );
        run_stdin(
            "kubectl",
            &["apply", "-n", &namespace, "-f", "-"],
            &model_yaml,
        )?;
    }

    if cmd.alias.is_empty() {
        eprintln!("Model '{key}' configured (provider '{}').", cmd.provider);
    } else {
        eprintln!(
            "Model '{key}' configured with aliases: {}.",
            cmd.alias.join(", ")
        );
    }
    Ok(())
}

fn do_list(scope: &Scope, cmd: ModelList) -> Result<(), String> {
    let namespace = scope.release_name()?;
    let output = run_output(
        "kubectl",
        &[
            "get",
            "models.sycophant.md",
            "-n",
            &namespace,
            "-o",
            "jsonpath={range .items[*]}{.metadata.name}{\"\\t\"}{.spec.providerRef.name}{\"\\t\"}{.spec.model}{\"\\n\"}{end}",
        ],
    )?;
    let entries = parse_model_list(&output);

    if cmd.json {
        let json =
            serde_json::to_string_pretty(&entries).map_err(|e| format!("serialize failed: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    if entries.is_empty() {
        eprintln!("No models configured.");
        return Ok(());
    }

    eprintln!("{:<40} {:<12} MODEL", "KEY", "PROVIDER");
    for e in &entries {
        eprintln!("{:<40} {:<12} {}", e.key, e.provider, e.model);
    }
    Ok(())
}

/// Parse the tab-separated `kubectl get models` jsonpath output into entries.
pub(crate) fn parse_model_list(kubectl_output: &str) -> Vec<ModelEntry> {
    kubectl_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let key = cols.next()?.trim().to_string();
            if key.is_empty() {
                return None;
            }
            Some(ModelEntry {
                key,
                provider: cols.next().unwrap_or_default().trim().to_string(),
                model: cols.next().unwrap_or_default().trim().to_string(),
            })
        })
        .collect()
}

fn do_delete(scope: &Scope, key: &str) -> Result<(), String> {
    let namespace = scope.release_name()?;
    if common::delete_cr("model.sycophant.md", key, &namespace)? {
        eprintln!("Model '{key}' deleted.");
    } else {
        eprintln!("Model '{key}' not found.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).expect("builder output must be valid YAML")
    }

    #[test]
    fn provider_cr_uses_preset_format_and_default_base_url() {
        let p = providers::lookup("anthropic").unwrap();
        let v = parse(&build_provider_cr(p, "dev", None, "my-key"));
        assert_eq!(v["kind"].as_str(), Some("Provider"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("anthropic"));
        assert_eq!(v["metadata"]["namespace"].as_str(), Some("dev"));
        assert_eq!(v["spec"]["format"].as_str(), Some("anthropic"));
        assert_eq!(
            v["spec"]["baseUrl"].as_str(),
            Some("https://api.anthropic.com/v1")
        );
    }

    #[test]
    fn provider_cr_base_url_override_wins() {
        let p = providers::lookup("openai").unwrap();
        let v = parse(&build_provider_cr(
            p,
            "dev",
            Some("http://localhost:8080/v1"),
            "k",
        ));
        // Override must replace the preset default (not be ignored).
        assert_eq!(v["spec"]["baseUrl"].as_str(), Some("http://localhost:8080/v1"));
    }

    #[test]
    fn provider_cr_secret_key_equals_secret_name() {
        // `syco secret set <name>` stores the value under data key `<name>`, and the
        // LLM job defaults provider.secret.key to "api-key". The Provider CR must
        // therefore set key == name, or the job reads the wrong (missing) data key.
        let p = providers::lookup("anthropic").unwrap();
        let v = parse(&build_provider_cr(p, "dev", None, "sycophant-llm-anthropic"));
        assert_eq!(v["spec"]["secret"]["name"].as_str(), Some("sycophant-llm-anthropic"));
        assert_eq!(v["spec"]["secret"]["key"].as_str(), Some("sycophant-llm-anthropic"));
    }

    #[test]
    fn model_cr_minimal_has_provider_ref_and_model() {
        let v = parse(&build_model_cr(
            "anthropic.haiku",
            "dev",
            "anthropic",
            "claude-haiku-4-5",
            None,
        ));
        assert_eq!(v["kind"].as_str(), Some("Model"));
        assert_eq!(v["metadata"]["name"].as_str(), Some("anthropic.haiku"));
        assert_eq!(v["spec"]["providerRef"]["name"].as_str(), Some("anthropic"));
        assert_eq!(v["spec"]["model"].as_str(), Some("claude-haiku-4-5"));
        assert!(v["spec"].get("params").is_none(), "no params without --thinking");
    }

    #[test]
    fn model_cr_provider_ref_is_nested_not_flat() {
        // Mutation guard: the field is spec.providerRef.name, NOT spec.provider.
        let v = parse(&build_model_cr("k", "dev", "anthropic", "m", None));
        assert!(v["spec"].get("provider").is_none());
        assert_eq!(v["spec"]["providerRef"]["name"].as_str(), Some("anthropic"));
    }

    #[test]
    fn model_cr_thinking_maps_to_params_output_config_effort() {
        let v = parse(&build_model_cr("k", "dev", "anthropic", "m", Some("high")));
        assert_eq!(
            v["spec"]["params"]["output_config"]["effort"].as_str(),
            Some("high")
        );
    }

    #[test]
    fn model_cr_alias_name_uses_alias_with_real_model() {
        // An alias is an independent Model CR: metadata.name = alias, spec.model = real.
        let v = parse(&build_model_cr("smart", "dev", "anthropic", "claude-haiku-4-5", None));
        assert_eq!(v["metadata"]["name"].as_str(), Some("smart"));
        assert_eq!(v["spec"]["model"].as_str(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn sanitize_k8s_name_maps_illegal_chars_and_trims() {
        // Ollama tags carry ':' '/' '_' — all illegal in a k8s metadata.name.
        assert_eq!(
            sanitize_k8s_name("openai.qwen3-abliterated:8b-v2"),
            "openai.qwen3-abliterated-8b-v2"
        );
        assert_eq!(
            sanitize_k8s_name("openai.huihui_ai/qwen3-abliterated:8b-v2"),
            "openai.huihui-ai-qwen3-abliterated-8b-v2"
        );
        // dot preserved (the provider.model separator); uppercase lowercased.
        assert_eq!(sanitize_k8s_name("OpenAI.Foo"), "openai.foo");
        // leading/trailing separators trimmed.
        assert_eq!(sanitize_k8s_name(":x:"), "x");
    }

    #[test]
    fn model_cr_colon_tag_name_is_rfc1123_but_spec_model_keeps_tag() {
        // RED before the fix: metadata.name would carry ':' '/' '_' and break apply.
        let v = parse(&build_model_cr(
            "openai.huihui_ai/qwen3-abliterated:8b-v2",
            "dev",
            "openai",
            "huihui_ai/qwen3-abliterated:8b-v2",
            None,
        ));
        let name = v["metadata"]["name"].as_str().unwrap();
        assert_eq!(name, "openai.huihui-ai-qwen3-abliterated-8b-v2");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'),
            "metadata.name must be DNS-1123-safe, got {name}"
        );
        // Mutation guard: spec.model must retain the raw Ollama tag verbatim.
        assert_eq!(
            v["spec"]["model"].as_str(),
            Some("huihui_ai/qwen3-abliterated:8b-v2")
        );
    }

    #[test]
    fn parse_model_list_empty_input() {
        assert_eq!(parse_model_list(""), Vec::new());
        assert_eq!(parse_model_list("  \n \n"), Vec::new());
    }

    #[test]
    fn parse_model_list_splits_tab_columns() {
        let entries = parse_model_list("anthropic.haiku\tanthropic\tclaude-haiku-4-5\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "anthropic.haiku");
        assert_eq!(entries[0].provider, "anthropic");
        assert_eq!(entries[0].model, "claude-haiku-4-5");
    }

    #[test]
    fn model_entry_serializes_to_camel_case_json() {
        let entry = ModelEntry {
            key: "anthropic.haiku".into(),
            provider: "anthropic".into(),
            model: "claude-haiku-4-5".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"key\":\"anthropic.haiku\""));
        assert!(json.contains("\"provider\":\"anthropic\""));
        assert!(json.contains("\"model\":\"claude-haiku-4-5\""));
    }
}
