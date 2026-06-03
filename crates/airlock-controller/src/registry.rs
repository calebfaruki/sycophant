#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredTool {
    pub name: String,
    pub description: Option<String>,
    pub args: Vec<ArgDecl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArgDecl {
    pub name: String,
    pub ty: ArgType,
    pub required: bool,
    pub env: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    String,
    Integer,
    Number,
    Boolean,
}

impl ArgType {
    pub fn as_schema_str(&self) -> &'static str {
        match self {
            ArgType::String => "string",
            ArgType::Integer => "integer",
            ArgType::Number => "number",
            ArgType::Boolean => "boolean",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("failed to parse image reference: {0}")]
    InvalidImageRef(String),
    #[error("registry request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
    #[error("invalid label JSON: {0}")]
    InvalidLabel(String),
    #[error("unexpected registry response: {0}")]
    UnexpectedResponse(String),
}

impl RegistryError {
    /// Whether the failure is plausibly transient. `RequestFailed` covers
    /// DNS/connect/timeout/5xx; `UnexpectedResponse` covers a proxy returning
    /// a 502 HTML page that parses as malformed JSON. Deterministic errors
    /// (`InvalidImageRef`, `InvalidLabel`) are NOT retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            RegistryError::RequestFailed(_) | RegistryError::UnexpectedResponse(_)
        )
    }
}

struct ImageRef {
    registry: String,
    repository: String,
    reference: String,
}

fn parse_image_ref(image: &str) -> ImageRef {
    let (image_part, reference) = if let Some((img, digest)) = image.split_once('@') {
        (img, digest.to_string())
    } else if let Some((img, tag)) = image.rsplit_once(':') {
        (img, tag.to_string())
    } else {
        (image, "latest".to_string())
    };

    let parts: Vec<&str> = image_part.splitn(3, '/').collect();
    let (registry, repository) = match parts.len() {
        1 => (
            "registry-1.docker.io".to_string(),
            format!("library/{}", parts[0]),
        ),
        2 => {
            if parts[0].contains('.') || parts[0].contains(':') {
                (parts[0].to_string(), parts[1].to_string())
            } else {
                (
                    "registry-1.docker.io".to_string(),
                    format!("{}/{}", parts[0], parts[1]),
                )
            }
        }
        _ => (parts[0].to_string(), format!("{}/{}", parts[1], parts[2])),
    };

    ImageRef {
        registry,
        repository,
        reference,
    }
}

fn registry_scheme(registry: &str) -> &'static str {
    let host = registry
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(registry);
    if host == "localhost"
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "host.docker.internal"
        || host.ends_with(".localhost")
        || !host.contains('.')
    {
        "http"
    } else {
        "https"
    }
}

/// Validate a chamber-declared tool name against the Anthropic tool-call
/// API regex `^[A-Za-z0-9_-]{1,64}$`. The canonical tool name is the
/// LLM-facing identifier — kebab-case (`git-status`), PascalCase
/// (`ReadFile`), or snake_case (`read_file`) are all valid. The
/// transformation to a K8s-safe segment (RFC 1123) happens at job-name
/// construction time via [`tool_name_to_k8s_segment`].
fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("tool name must not be empty".into());
    }
    if name.len() > 64 {
        return Err(format!(
            "tool name '{name}' exceeds 64 characters (Anthropic tool-call API limit)"
        ));
    }
    for &b in name.as_bytes() {
        let valid = b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if !valid {
            return Err(format!(
                "tool name '{name}' contains invalid character '{}' \
                 (only [A-Za-z0-9_-] allowed per Anthropic tool-call API)",
                b as char
            ));
        }
    }
    Ok(())
}

/// Convert an LLM-facing tool name to a K8s name segment (RFC 1123:
/// `[a-z0-9]([-a-z0-9]*[a-z0-9])?`). Used to build airlock-spawned Job
/// names from PascalCase / camelCase / snake_case canonical identifiers.
///
/// Rules:
/// - `_` → `-`
/// - Uppercase becomes lowercase; a `-` is inserted before it when the
///   previous character is lowercase or a digit (camelCase boundary), or
///   when the previous character is uppercase and the next is lowercase
///   (acronym-to-Title boundary, e.g. `XMLHttp` → `xml-http`).
/// - Leading/trailing hyphens are trimmed to satisfy RFC 1123.
///
/// Examples: `Bash` → `bash`, `ReadFile` → `read-file`,
/// `ListDirectory` → `list-directory`, `read_file` → `read-file`,
/// `XMLHttpRequest` → `xml-http-request`, `git-status` → `git-status`.
pub fn tool_name_to_k8s_segment(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = String::with_capacity(bytes.len() + 4);
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'_' {
            if !out.ends_with('-') {
                out.push('-');
            }
            continue;
        }
        if b.is_ascii_uppercase() {
            let prev_lower_or_digit =
                i > 0 && (bytes[i - 1].is_ascii_lowercase() || bytes[i - 1].is_ascii_digit());
            let prev_upper = i > 0 && bytes[i - 1].is_ascii_uppercase();
            let next_lower = bytes
                .get(i + 1)
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false);
            if !out.ends_with('-') && (prev_lower_or_digit || (prev_upper && next_lower)) {
                out.push('-');
            }
            out.push((b as char).to_ascii_lowercase());
        } else {
            out.push(b as char);
        }
    }
    out.trim_matches('-').to_string()
}

pub fn parse_tools_label(label_value: &str) -> Result<Vec<DiscoveredTool>, RegistryError> {
    let parsed: serde_json::Value = serde_json::from_str(label_value)
        .map_err(|e| RegistryError::InvalidLabel(format!("not valid JSON: {e}")))?;

    let array = parsed
        .as_array()
        .ok_or_else(|| RegistryError::InvalidLabel("label must be a JSON array".into()))?;

    let mut tools = Vec::new();
    for (i, entry) in array.iter().enumerate() {
        let obj = entry.as_object().ok_or_else(|| {
            RegistryError::InvalidLabel(format!(
                "tool entry {i} must be an object with 'name', 'description', and 'args'"
            ))
        })?;

        let name = obj
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| RegistryError::InvalidLabel(format!("tool entry {i} missing 'name'")))?
            .to_string();

        validate_tool_name(&name).map_err(RegistryError::InvalidLabel)?;

        let description = obj
            .get("description")
            .and_then(|d| d.as_str())
            .map(String::from);

        let args_obj = obj.get("args").and_then(|a| a.as_object()).ok_or_else(|| {
            RegistryError::InvalidLabel(format!(
                "tool '{name}' missing 'args' object (use {{}} for zero-arg tools)"
            ))
        })?;

        let mut args = Vec::new();
        for (arg_name, arg_value) in args_obj {
            let arg_obj = arg_value.as_object().ok_or_else(|| {
                RegistryError::InvalidLabel(format!(
                    "tool '{name}' arg '{arg_name}' must be an object"
                ))
            })?;

            let ty_str = arg_obj
                .get("type")
                .and_then(|t| t.as_str())
                .ok_or_else(|| {
                    RegistryError::InvalidLabel(format!(
                        "tool '{name}' arg '{arg_name}' missing 'type'"
                    ))
                })?;

            let ty = match ty_str {
                "string" => ArgType::String,
                "integer" => ArgType::Integer,
                "number" => ArgType::Number,
                "boolean" => ArgType::Boolean,
                other => {
                    return Err(RegistryError::InvalidLabel(format!(
                        "tool '{name}' arg '{arg_name}' has unknown type '{other}' (expected string, integer, number, boolean)"
                    )));
                }
            };

            let env = arg_obj
                .get("env")
                .and_then(|e| e.as_str())
                .ok_or_else(|| {
                    RegistryError::InvalidLabel(format!(
                        "tool '{name}' arg '{arg_name}' missing 'env' (the environment variable name to pass the value to the Makefile recipe)"
                    ))
                })?
                .to_string();

            let required = arg_obj
                .get("required")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);

            let arg_desc = arg_obj
                .get("description")
                .and_then(|d| d.as_str())
                .map(String::from);

            args.push(ArgDecl {
                name: arg_name.clone(),
                ty,
                required,
                env,
                description: arg_desc,
            });
        }

        tools.push(DiscoveredTool {
            name,
            description,
            args,
        });
    }

    Ok(tools)
}

pub async fn discover_tools(image_ref: &str) -> Result<Vec<DiscoveredTool>, RegistryError> {
    let parsed = parse_image_ref(image_ref);
    let client = reqwest::Client::new();

    // Get auth token for public registries
    let token = if parsed.registry == "registry-1.docker.io" {
        let token_url = format!(
            "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{}:pull",
            parsed.repository
        );
        let resp: serde_json::Value = client.get(&token_url).send().await?.json().await?;
        resp.get("token").and_then(|t| t.as_str()).map(String::from)
    } else if parsed.registry == "ghcr.io" {
        let token_url = format!(
            "https://ghcr.io/token?scope=repository:{}:pull&service=ghcr.io",
            parsed.repository
        );
        let resp: serde_json::Value = client.get(&token_url).send().await?.json().await?;
        resp.get("token").and_then(|t| t.as_str()).map(String::from)
    } else {
        None
    };

    // Fetch manifest
    let scheme = registry_scheme(&parsed.registry);
    let manifest_url = format!(
        "{scheme}://{}/v2/{}/manifests/{}",
        parsed.registry, parsed.repository, parsed.reference
    );
    let mut req = client.get(&manifest_url).header(
        "Accept",
        "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json",
    );
    if let Some(ref token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let manifest: serde_json::Value = req.send().await?.json().await?;

    // If it's an index/manifest list, get the first manifest
    let manifest = if manifest.get("manifests").is_some() {
        let first = manifest["manifests"]
            .as_array()
            .and_then(|m| m.first())
            .ok_or_else(|| RegistryError::UnexpectedResponse("empty manifest list".into()))?;
        let digest = first["digest"]
            .as_str()
            .ok_or_else(|| RegistryError::UnexpectedResponse("manifest missing digest".into()))?;
        let url = format!(
            "{scheme}://{}/v2/{}/manifests/{}",
            parsed.registry, parsed.repository, digest
        );
        let mut req = client.get(&url).header(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        );
        if let Some(ref token) = token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req.send().await?.json().await?
    } else {
        manifest
    };

    // Get config digest
    let config_digest = manifest["config"]["digest"].as_str().ok_or_else(|| {
        RegistryError::UnexpectedResponse("manifest missing config digest".into())
    })?;

    // Fetch config blob
    let config_url = format!(
        "{scheme}://{}/v2/{}/blobs/{}",
        parsed.registry, parsed.repository, config_digest
    );
    let mut req = client.get(&config_url);
    if let Some(ref token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let config: serde_json::Value = req.send().await?.json().await?;

    // Read label
    let label = config
        .get("config")
        .and_then(|c| c.get("Labels"))
        .and_then(|l| l.get("md.sycophant.tools"))
        .and_then(|v| v.as_str());

    match label {
        Some(value) => parse_tools_label(value),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_label_per_tool_args() {
        let label = r#"[
            {
                "name": "notion-search",
                "description": "Search Notion",
                "args": {
                    "query": {
                        "type": "string",
                        "required": true,
                        "env": "QUERY",
                        "description": "Search query"
                    }
                }
            }
        ]"#;
        let tools = parse_tools_label(label).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "notion-search");
        assert_eq!(tools[0].description.as_deref(), Some("Search Notion"));
        assert_eq!(tools[0].args.len(), 1);
        let a = &tools[0].args[0];
        assert_eq!(a.name, "query");
        assert_eq!(a.ty, ArgType::String);
        assert!(a.required);
        assert_eq!(a.env, "QUERY");
        assert_eq!(a.description.as_deref(), Some("Search query"));
    }

    #[test]
    fn parse_label_zero_arg_tool() {
        let label = r#"[{"name": "notion-whoami", "description": "Bot identity", "args": {}}]"#;
        let tools = parse_tools_label(label).unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].args.is_empty());
    }

    #[test]
    fn parse_label_bare_string_rejected() {
        let err = parse_tools_label(r#"["git"]"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must be an object"),
            "expected rejection of bare string, got: {msg}"
        );
    }

    #[test]
    fn parse_label_object_missing_args_rejected() {
        let label = r#"[{"name": "git", "description": "git tool"}]"#;
        let err = parse_tools_label(label).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing 'args'"), "got: {msg}");
    }

    #[test]
    fn parse_label_object_missing_name_rejected() {
        let label = r#"[{"description": "no name", "args": {}}]"#;
        let err = parse_tools_label(label).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing 'name'"), "got: {msg}");
    }

    #[test]
    fn parse_label_arg_missing_type_rejected() {
        let label = r#"[{"name": "x", "args": {"q": {"env": "Q"}}}]"#;
        let err = parse_tools_label(label).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing 'type'"), "got: {msg}");
    }

    #[test]
    fn parse_label_arg_missing_env_rejected() {
        let label = r#"[{"name": "x", "args": {"q": {"type": "string"}}}]"#;
        let err = parse_tools_label(label).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing 'env'"), "got: {msg}");
    }

    #[test]
    fn parse_label_arg_unknown_type_rejected() {
        let label = r#"[{"name": "x", "args": {"q": {"type": "wat", "env": "Q"}}}]"#;
        let err = parse_tools_label(label).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown type"), "got: {msg}");
    }

    #[test]
    fn parse_label_arg_non_object_rejected() {
        let label = r#"[{"name": "x", "args": {"q": "string"}}]"#;
        let err = parse_tools_label(label).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must be an object"), "got: {msg}");
    }

    #[test]
    fn parse_label_arg_required_defaults_false() {
        let label = r#"[{"name": "x", "args": {"q": {"type": "string", "env": "Q"}}}]"#;
        let tools = parse_tools_label(label).unwrap();
        assert!(!tools[0].args[0].required);
    }

    #[test]
    fn parse_label_arg_types_all() {
        let label = r#"[{
            "name": "x",
            "args": {
                "s": {"type": "string",  "env": "S"},
                "i": {"type": "integer", "env": "I"},
                "n": {"type": "number",  "env": "N"},
                "b": {"type": "boolean", "env": "B"}
            }
        }]"#;
        let tools = parse_tools_label(label).unwrap();
        let by_name: std::collections::HashMap<_, _> = tools[0]
            .args
            .iter()
            .map(|a| (a.name.as_str(), a.ty))
            .collect();
        assert_eq!(by_name["s"], ArgType::String);
        assert_eq!(by_name["i"], ArgType::Integer);
        assert_eq!(by_name["n"], ArgType::Number);
        assert_eq!(by_name["b"], ArgType::Boolean);
    }

    #[test]
    fn parse_label_empty_array() {
        let tools = parse_tools_label("[]").unwrap();
        assert!(tools.is_empty());
    }

    #[test]
    fn parse_label_not_json() {
        assert!(parse_tools_label("not json").is_err());
    }

    #[test]
    fn parse_label_not_array() {
        let err = parse_tools_label(r#"{"name": "x"}"#).unwrap_err();
        assert!(err.to_string().contains("must be a JSON array"));
    }

    #[test]
    fn parse_image_ref_full() {
        let r = parse_image_ref("ghcr.io/org/image:v1");
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "org/image");
        assert_eq!(r.reference, "v1");
    }

    #[test]
    fn parse_image_ref_with_digest() {
        let r = parse_image_ref("ghcr.io/org/image@sha256:abc123");
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "org/image");
        assert_eq!(r.reference, "sha256:abc123");
    }

    #[test]
    fn parse_image_ref_docker_hub() {
        let r = parse_image_ref("alpine/git:latest");
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "alpine/git");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_image_ref_docker_hub_official() {
        let r = parse_image_ref("alpine:3.21");
        assert_eq!(r.registry, "registry-1.docker.io");
        assert_eq!(r.repository, "library/alpine");
        assert_eq!(r.reference, "3.21");
    }

    #[test]
    fn parse_image_ref_no_tag() {
        let r = parse_image_ref("ghcr.io/org/image");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_image_ref_two_part_with_dot_uses_first_as_registry() {
        // Catches `||`→`&&` mutation at parse_image_ref:43.
        let r = parse_image_ref("ghcr.io/foo:tag");
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "foo");
        assert_eq!(r.reference, "tag");
    }

    #[test]
    fn parse_image_ref_two_part_with_port_uses_first_as_registry() {
        // Catches `||`→`&&` mutation at parse_image_ref:43 (other operand).
        let r = parse_image_ref("localhost:5000/foo:tag");
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "foo");
        assert_eq!(r.reference, "tag");
    }

    #[test]
    fn scheme_localhost_is_http() {
        assert_eq!(registry_scheme("localhost:5000"), "http");
        assert_eq!(registry_scheme("localhost"), "http");
    }

    #[test]
    fn scheme_loopback_is_http() {
        assert_eq!(registry_scheme("127.0.0.1:5000"), "http");
        assert_eq!(registry_scheme("[::1]:5000"), "http");
    }

    #[test]
    fn scheme_docker_internal_is_http() {
        assert_eq!(registry_scheme("host.docker.internal:5000"), "http");
    }

    #[test]
    fn scheme_dotlocalhost_is_http() {
        assert_eq!(registry_scheme("k3d-registry.localhost:5555"), "http");
        assert_eq!(registry_scheme("my-reg.localhost"), "http");
    }

    #[test]
    fn scheme_remote_is_https() {
        assert_eq!(registry_scheme("ghcr.io"), "https");
        assert_eq!(registry_scheme("registry-1.docker.io"), "https");
        assert_eq!(registry_scheme("my-registry.example.com:5000"), "https");
    }

    #[test]
    fn scheme_bare_hostname_is_http() {
        // In-cluster k8s service names have no FQDN dots, so bare hostnames
        // are assumed to be in-cluster registries speaking HTTP. This is the
        // canonical kind/k3d local-registry pattern.
        assert_eq!(registry_scheme("sycophant-registry:5000"), "http");
        assert_eq!(registry_scheme("kind-registry:5000"), "http");
        assert_eq!(registry_scheme("my-registry"), "http");
    }

    #[test]
    fn validate_tool_name_accepts_all_anthropic_api_styles() {
        for name in [
            "x",
            "ssh-exec",
            "notion-search",
            "git-log",
            "a1",
            "ab-cd-ef",
            "Bash",
            "ReadFile",
            "ListDirectory",
            "read_file",
            "XMLHttpRequest",
        ] {
            assert!(
                validate_tool_name(name).is_ok(),
                "expected '{name}' to validate, got {:?}",
                validate_tool_name(name)
            );
        }
    }

    #[test]
    fn validate_tool_name_rejects_invalid_characters() {
        for name in ["foo bar", "foo.bar", "foo/bar", "foo:bar"] {
            assert!(
                validate_tool_name(name).is_err(),
                "expected '{name}' to be rejected"
            );
        }
    }

    #[test]
    fn validate_tool_name_rejects_empty_and_overlong() {
        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name(&"a".repeat(65)).is_err());
        assert!(validate_tool_name(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn tool_name_to_k8s_segment_lowercases_single_word() {
        assert_eq!(tool_name_to_k8s_segment("Bash"), "bash");
        assert_eq!(tool_name_to_k8s_segment("git"), "git");
    }

    #[test]
    fn tool_name_to_k8s_segment_splits_pascal_case() {
        assert_eq!(tool_name_to_k8s_segment("ReadFile"), "read-file");
        assert_eq!(tool_name_to_k8s_segment("ListDirectory"), "list-directory");
        assert_eq!(tool_name_to_k8s_segment("WriteFile"), "write-file");
    }

    #[test]
    fn tool_name_to_k8s_segment_converts_snake_to_kebab() {
        assert_eq!(tool_name_to_k8s_segment("read_file"), "read-file");
        assert_eq!(tool_name_to_k8s_segment("list_directory"), "list-directory");
    }

    #[test]
    fn tool_name_to_k8s_segment_preserves_kebab() {
        assert_eq!(tool_name_to_k8s_segment("git-status"), "git-status");
        assert_eq!(tool_name_to_k8s_segment("notion-search"), "notion-search");
    }

    #[test]
    fn tool_name_to_k8s_segment_handles_acronym_boundaries() {
        assert_eq!(
            tool_name_to_k8s_segment("XMLHttpRequest"),
            "xml-http-request"
        );
        assert_eq!(tool_name_to_k8s_segment("HTTPServer"), "http-server");
    }

    #[test]
    fn tool_name_to_k8s_segment_trims_leading_underscore() {
        assert_eq!(tool_name_to_k8s_segment("_foo"), "foo");
    }

    #[test]
    fn is_retryable_true_for_unexpected_response() {
        let e = RegistryError::UnexpectedResponse("502 page".into());
        assert!(e.is_retryable());
    }

    #[test]
    fn is_retryable_false_for_invalid_label() {
        let e = RegistryError::InvalidLabel("bad json".into());
        assert!(!e.is_retryable());
    }

    #[test]
    fn is_retryable_false_for_invalid_image_ref() {
        let e = RegistryError::InvalidImageRef("bad ref".into());
        assert!(!e.is_retryable());
    }
}
