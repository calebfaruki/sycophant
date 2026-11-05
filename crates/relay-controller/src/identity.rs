//! Identity parsing for the `relay-identity` ConfigMaps — the relay's
//! agent-list and handle-binding table.
//!
//! One ConfigMap is one subject: `subject`, `agents` (a comma list), and
//! `handle.<channel>` keys naming that channel's platform user ID. The table
//! answers two questions and only these: which agents a subject reaches
//! (ListAgents), and which subject a channel handle resolves to (the
//! channel path). Authorization itself is not here — every request is checked
//! by a Kubernetes review; this table only names the list and resolves
//! senders.
//!
//! Validation is invalid-is-absent: a ConfigMap that does not parse does not
//! exist, and it never suppresses the subjects beside it. There is no
//! default, no last-known-good, and no carry-forward from an earlier
//! delivery, because a typo in one ConfigMap must not block a revocation made
//! in the same edit.
//!
//! Pure — no I/O. `identity_watcher` owns the delivery side.

use std::collections::{BTreeMap, HashMap};

use k8s_openapi::api::core::v1::ConfigMap;

/// A ConfigMap that failed validation, named so the operator can find it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowError {
    pub key: String,
    pub reason: String,
}

/// The live identity table. Replaced wholesale on every delivery.
#[derive(Clone, Debug, Default)]
pub struct IdentityTable {
    by_subject: HashMap<String, Vec<String>>,
    handles: HashMap<(String, String), String>,
}

impl IdentityTable {
    /// The subject's whole granted agent list, from the watched data.
    pub fn agents_for(&self, subject: &str) -> Vec<String> {
        self.by_subject.get(subject).cloned().unwrap_or_default()
    }

    /// The subject a `(channel, platform user ID)` pair resolves to. A handle
    /// no operator bound resolves to nothing.
    pub fn resolve_handle(&self, channel: &str, handle: &str) -> Option<&str> {
        self.handles
            .get(&(channel.to_string(), handle.to_string()))
            .map(String::as_str)
    }
}

/// One ConfigMap's data: the subject, its agent list, and its
/// `(channel, platform user ID)` handle pairs.
struct SubjectRow {
    subject: String,
    agents: Vec<String>,
    handles: Vec<(String, String)>,
}

/// Parse and validate one ConfigMap, independently of every other.
fn parse_configmap(data: &BTreeMap<String, String>) -> Result<SubjectRow, String> {
    let subject = data
        .get("subject")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "no subject key".to_string())?
        .trim()
        .to_string();
    let agents = data
        .get("agents")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|agent| !agent.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut handles = Vec::new();
    for (key, value) in data {
        if let Some(channel) = key.strip_prefix("handle.") {
            if channel.is_empty() || value.trim().is_empty() {
                return Err(format!(
                    "handle key \"{key}\" has an empty channel or value"
                ));
            }
            handles.push((channel.to_string(), value.trim().to_string()));
        }
    }
    Ok(SubjectRow {
        subject,
        agents,
        handles,
    })
}

/// Build the table one delivery carries, plus the errors it raised. The
/// returned table is the whole truth: a subject absent here is revoked,
/// whether the operator deleted the ConfigMap or broke it.
///
/// A handle claimed by two subjects is absent for both and reported, so the
/// operator can fix the binding instead of one person silently receiving the
/// other's messages.
pub fn apply_delivery(cms: &[ConfigMap]) -> (IdentityTable, Vec<RowError>) {
    let mut by_subject: HashMap<String, Vec<String>> = HashMap::new();
    // (channel, handle) → the subjects claiming it, in delivery order.
    let mut handle_claims: HashMap<(String, String), Vec<String>> = HashMap::new();
    let mut errors = Vec::new();

    for cm in cms {
        let name = cm
            .metadata
            .name
            .clone()
            .unwrap_or_else(|| "<unnamed>".to_string());
        match parse_configmap(&cm.data.clone().unwrap_or_default()) {
            Ok(row) => {
                by_subject.insert(row.subject.clone(), row.agents);
                for (channel, handle) in row.handles {
                    handle_claims
                        .entry((channel, handle))
                        .or_default()
                        .push(row.subject.clone());
                }
            }
            Err(reason) => errors.push(RowError { key: name, reason }),
        }
    }

    // A handle two subjects claim resolves to neither.
    let mut handles = HashMap::new();
    for ((channel, handle), subjects) in handle_claims {
        if subjects.len() == 1 {
            handles.insert((channel, handle), subjects[0].clone());
        } else {
            errors.push(RowError {
                key: format!("handle.{channel}.{handle}"),
                reason: format!(
                    "handle \"{handle}\" on channel \"{channel}\" is claimed by multiple subjects: {}",
                    subjects.join(", ")
                ),
            });
        }
    }

    (
        IdentityTable {
            by_subject,
            handles,
        },
        errors,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cm(name: &str, data: &[(&str, &str)]) -> ConfigMap {
        ConfigMap {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            data: Some(
                data.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            ..Default::default()
        }
    }

    #[test]
    fn empty_agents_key_yields_empty_list() {
        let (table, errors) = apply_delivery(&[cm(
            "user-1",
            &[("subject", "https://issuer.example#alice"), ("agents", "")],
        )]);
        assert!(errors.is_empty());
        assert!(table.agents_for("https://issuer.example#alice").is_empty());
    }
}
