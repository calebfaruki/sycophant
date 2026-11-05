//! Fixtures shared by the relay-controller integration tests.

// Each test binary uses a subset of these.
#[allow(dead_code)]
pub(crate) mod apiserver;
#[allow(dead_code)]
pub(crate) mod harness;

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

pub(crate) const NAMESPACE: &str = "tenant-acme";

/// One relay-identity ConfigMap, as the user chart renders it: `subject`,
/// `agents`, and one `handle.<channel>` key per bound handle. A `None`
/// subject leaves the `subject` key out.
pub(crate) fn identity_cm(
    name: &str,
    subject: Option<&str>,
    agents: &str,
    handles: &[(&str, &str)],
) -> ConfigMap {
    let mut data = BTreeMap::new();
    if let Some(s) = subject {
        data.insert("subject".to_string(), s.to_string());
    }
    data.insert("agents".to_string(), agents.to_string());
    for (channel, handle) in handles {
        data.insert(format!("handle.{channel}"), handle.to_string());
    }
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.into()),
            namespace: Some(NAMESPACE.into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}
