use std::collections::HashMap;
use std::path::PathBuf;

use shared::toolset::CapabilityGrant;

pub(crate) struct HarnessConfig {
    pub relay_gateway_addr: String,
    /// Root under which this agent's instructions directory lives. The chart
    /// mounts the read-only instructions PVC so that `<instructions_root>/<agent>`
    /// holds AGENTS.md, agents/, and skills/.
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
    /// This agent's grants, keyed by toolset then grant name, from `GRANTS`.
    pub grants: HashMap<String, HashMap<String, CapabilityGrant>>,
    /// Scheduling config (runtime class, tolerations, node selector) applied to
    /// spawned tool Jobs, read once at boot from a mounted ConfigMap.
    pub scheduling_file: String,
    /// Name of the per-agent headless Service that publishes each tool pod's
    /// per-pod DNS record. Stamped into each tool Job as its `subdomain`, and the
    /// middle DNS label the harness dials at
    /// `<call-id>.<capability_service>.<namespace>.svc.cluster.local`.
    pub capability_service: String,
    /// The model keys this agent may name, from the comma list `MODELS`.
    /// A turn naming any other key is refused.
    pub models: Vec<String>,
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

        let grants = match std::env::var("GRANTS") {
            Ok(raw) if !raw.trim().is_empty() => {
                parse_grants(&raw).map_err(|e| format!("GRANTS: {e}"))?
            }
            _ => HashMap::new(),
        };

        let scheduling_file = std::env::var("TOOLSET_SCHEDULING_FILE")
            .unwrap_or_else(|_| "/etc/sycophant/scheduling/scheduling.yaml".to_string());

        let capability_service = std::env::var("CAPABILITY_SERVICE_NAME")
            .map_err(|_| "CAPABILITY_SERVICE_NAME is required")?;

        let models = comma_list(&std::env::var("MODELS").unwrap_or_default());

        Ok(Self {
            relay_gateway_addr,
            instructions_root,
            agent,
            max_iterations,
            idle_gap_secs,
            namespace,
            toolsets,
            grants,
            scheduling_file,
            capability_service,
            models,
        })
    }
}

fn comma_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse `GRANTS`, a JSON object of `grant.<toolset>.<grant>.<field>` keys, into
/// grants keyed by toolset then grant name. Each grant passes the
/// `CapabilityGrant` checks.
pub(crate) fn parse_grants(
    raw: &str,
) -> Result<HashMap<String, HashMap<String, CapabilityGrant>>, String> {
    let flat: HashMap<String, String> =
        serde_json::from_str(raw).map_err(|e| format!("not a flat JSON object: {e}"))?;
    let mut fields: HashMap<(String, String), serde_json::Map<String, serde_json::Value>> =
        HashMap::new();
    for (key, value) in flat {
        let parts: Vec<&str> = key.splitn(4, '.').collect();
        let ["grant", toolset, grant, field] = parts[..] else {
            return Err(format!("{key} is not grant.<toolset>.<grant>.<field>"));
        };
        fields
            .entry((toolset.to_string(), grant.to_string()))
            .or_default()
            .insert(field.to_string(), value.into());
    }
    let mut out: HashMap<String, HashMap<String, CapabilityGrant>> = HashMap::new();
    for ((toolset, grant), fields) in fields {
        let parsed: CapabilityGrant = serde_json::from_value(fields.into())
            .map_err(|e| format!("grant.{toolset}.{grant}: {e}"))?;
        out.entry(toolset).or_default().insert(grant, parsed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use shared::toolset::CapabilityGrant;

    use super::*;

    #[test]
    fn grants_parse_by_toolset_then_grant_and_refuse_an_invalid_grant() {
        let grants: HashMap<String, HashMap<String, CapabilityGrant>> = parse_grants(
            r#"{"grant.ssh.demo.secret":"demo-key","grant.ssh.demo.egress":"github.com","grant.ssh.demo.path":"/home/agent/.ssh/id","grant.notion.reader.secret":"notion-reader"}"#,
        )
        .unwrap();

        assert_eq!(
            grants["ssh"]["demo"],
            CapabilityGrant {
                secret: "demo-key".into(),
                path: Some("/home/agent/.ssh/id".into()),
                egress: Some("github.com".into()),
            }
        );
        assert_eq!(grants["notion"]["reader"].secret, "notion-reader");
        assert_eq!(grants.len(), 2);

        assert!(parse_grants(
            r#"{"grant.ssh.demo.secret":"demo-key","grant.ssh.demo.path":"/workspace"}"#
        )
        .is_err());
    }
}
