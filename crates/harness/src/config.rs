use std::collections::HashMap;
use std::path::PathBuf;

use shared::toolset::ToolsetSecret;

use crate::conversation::MAX_ARCHIVES;

pub(crate) struct HarnessConfig {
    pub relay_gateway_addr: String,
    /// Root under which this agent's instructions directory lives:
    /// `<instructions_root>/<agent>` holds AGENTS.md, agents/ and skills/.
    pub instructions_root: PathBuf,
    /// This harness's own agent name. Each harness is per-agent and
    /// serves only its own agent's instructions.
    pub agent: String,
    pub max_iterations: u32,
    pub idle_gap_secs: u64,
    /// Namespace this agent's tool Jobs are created in.
    pub namespace: String,
    /// The toolsets this agent binds, from the comma list `TOOLSETS`. A
    /// toolset outside it is never listed.
    pub toolsets: Vec<String>,
    /// Each toolset's Secrets, keyed by toolset, from `TOOLSET_SECRETS`. Every
    /// call of a toolset gets all of its Secrets.
    pub toolset_secrets: HashMap<String, Vec<ToolsetSecret>>,
    /// Scheduling config (runtime class, tolerations, node selector) applied to
    /// spawned tool Jobs, read once at boot from a mounted ConfigMap.
    pub scheduling_file: String,
    /// Name of the per-agent headless Service that publishes each tool pod's
    /// per-pod DNS record. Stamped into each tool Job as its `subdomain`, and the
    /// middle DNS label the harness dials at
    /// `<call-id>.<capability_service>.<namespace>.svc.cluster.local`.
    pub capability_service: String,
    /// This agent's one model key, from `MODEL`.
    pub model: Option<String>,
    /// How many archives a compact keeps, from `ARCHIVES` (1 to `MAX_ARCHIVES`).
    pub archives: u32,
}

impl HarnessConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        let relay_gateway_addr =
            std::env::var("RELAY_GATEWAY_ADDR").map_err(|_| "RELAY_GATEWAY_ADDR is required")?;

        let instructions_root = std::env::var("INSTRUCTIONS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/instructions"));

        let agent = std::env::var("AGENT_NAME").map_err(|_| "AGENT_NAME is required")?;

        let max_iterations = std::env::var("MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        // Max silence between inference events before a turn is failed as
        // wedged. A local model's prompt read and thinking send no frames, so
        // this must outlast a slow CPU model, not just the job's 10s heartbeat.
        let idle_gap_secs = std::env::var("IDLE_GAP_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);

        let namespace =
            std::env::var("TOOLSET_NAMESPACE").unwrap_or_else(|_| "default".to_string());

        let toolsets = comma_list(&std::env::var("TOOLSETS").unwrap_or_default());

        let toolset_secrets = match std::env::var("TOOLSET_SECRETS") {
            Ok(raw) if !raw.trim().is_empty() => {
                parse_toolset_secrets(&raw).map_err(|e| format!("TOOLSET_SECRETS: {e}"))?
            }
            _ => HashMap::new(),
        };

        let scheduling_file = std::env::var("TOOLSET_SCHEDULING_FILE")
            .unwrap_or_else(|_| "/etc/sycophant/scheduling/scheduling.yaml".to_string());

        let capability_service = std::env::var("CAPABILITY_SERVICE_NAME")
            .map_err(|_| "CAPABILITY_SERVICE_NAME is required")?;

        let model = std::env::var("MODEL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let archives = parse_archives(std::env::var("ARCHIVES").ok().as_deref())?;

        Ok(Self {
            relay_gateway_addr,
            instructions_root,
            agent,
            max_iterations,
            idle_gap_secs,
            namespace,
            toolsets,
            toolset_secrets,
            scheduling_file,
            capability_service,
            model,
            archives,
        })
    }
}

fn parse_archives(raw: Option<&str>) -> Result<u32, String> {
    let raw = raw.ok_or("ARCHIVES is required")?;
    match raw.trim().parse::<u32>() {
        Ok(n) if (1..=MAX_ARCHIVES).contains(&n) => Ok(n),
        _ => Err(format!(
            "ARCHIVES must be a whole number from 1 to {MAX_ARCHIVES}, got {raw:?}"
        )),
    }
}

fn comma_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse `TOOLSET_SECRETS`, a flat JSON object of
/// `secret.<toolset>.<secret-name>` keys whose values are mount paths, into
/// Secrets keyed by toolset. The key splits in three, so a Secret name may hold
/// dots. Each Secret passes the `ToolsetSecret` checks, and two Secrets of one
/// toolset may not share a path.
pub(crate) fn parse_toolset_secrets(
    raw: &str,
) -> Result<HashMap<String, Vec<ToolsetSecret>>, String> {
    let flat: HashMap<String, String> =
        serde_json::from_str(raw).map_err(|e| format!("not a flat JSON object: {e}"))?;
    let mut out: HashMap<String, Vec<ToolsetSecret>> = HashMap::new();
    for (key, path) in flat {
        let parts: Vec<&str> = key.splitn(3, '.').collect();
        let ["secret", toolset, name] = parts[..] else {
            return Err(format!("{key} is not secret.<toolset>.<secret-name>"));
        };
        let secret =
            ToolsetSecret::new(name.to_string(), path).map_err(|e| format!("{key}: {e}"))?;
        let secrets = out.entry(toolset.to_string()).or_default();
        if secrets.iter().any(|s| s.path == secret.path) {
            return Err(format!(
                "{key}: two Secrets of toolset {toolset} mount at {}",
                secret.path
            ));
        }
        secrets.push(secret);
    }
    for secrets in out.values_mut() {
        secrets.sort_by(|a, b| a.secret.cmp(&b.secret));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolset_secrets_split_the_key_in_three_and_refuse_a_duplicate_path_or_another_key() {
        let secrets = parse_toolset_secrets(
            r#"{"secret.ssh.my.key":"/home/agent/.ssh/id_ed25519","secret.ssh.demo-key":"/home/agent/.ssh/demo","secret.git.git-key":"/home/agent/.git-credentials"}"#,
        )
        .unwrap();

        assert_eq!(secrets.len(), 2);
        let mut ssh: Vec<(String, String)> = secrets["ssh"]
            .iter()
            .map(|s| (s.secret.clone(), s.path.clone()))
            .collect();
        ssh.sort();
        assert_eq!(
            ssh,
            vec![
                ("demo-key".to_string(), "/home/agent/.ssh/demo".to_string()),
                (
                    "my.key".to_string(),
                    "/home/agent/.ssh/id_ed25519".to_string()
                ),
            ]
        );
        let git: Vec<&str> = secrets["git"].iter().map(|s| s.secret.as_str()).collect();
        assert_eq!(git, vec!["git-key"]);

        assert!(parse_toolset_secrets(
            r#"{"secret.ssh.demo-key":"/home/agent/.ssh/id","secret.ssh.local-key":"/home/agent/.ssh/id"}"#
        )
        .is_err());
        assert_eq!(
            parse_toolset_secrets(r#"{"env.ssh.k":"/p"}"#).unwrap_err(),
            "env.ssh.k is not secret.<toolset>.<secret-name>"
        );
    }

    #[test]
    fn parse_archives_accepts_1_to_1000_and_names_archives_otherwise() {
        assert_eq!(parse_archives(Some("1")), Ok(1));
        assert_eq!(parse_archives(Some("1000")), Ok(1000));
        assert_eq!(
            parse_archives(None),
            Err("ARCHIVES is required".to_string())
        );
        for bad in ["0", "1001", "x", ""] {
            assert_eq!(
                parse_archives(Some(bad)),
                Err(format!(
                    "ARCHIVES must be a whole number from 1 to 1000, got {bad:?}"
                )),
                "{bad:?}"
            );
        }
    }
}
