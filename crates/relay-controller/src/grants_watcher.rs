//! Watch the `grants` ConfigMap and keep the relay's live authorization
//! table in sync.
//!
//! Revocation cannot wait for a pod roll, so this is a hot reload: every
//! delivery replaces the table wholesale. The raw `watcher::Event` stream is
//! consumed rather than `watch_object` so the re-list boundary stays visible
//! and a re-list swaps atomically instead of leaking a half-built map.
//!
//! Rows that fail validation are reported twice: in the log, and as a
//! Warning Event on the ConfigMap itself. ConfigMaps have no status
//! subresource, so `kubectl describe configmap grants` is the operator's
//! only surface.

use std::sync::Arc;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::runtime::events::{Event, EventType, Recorder, Reporter};
use kube::runtime::watcher;
use kube::{Api, Client as KubeClient, Resource};
use tokio::sync::RwLock;

use crate::grants::{apply_delivery, GrantsTable, RowError, GRANTS_CONFIGMAP_NAME};

/// `reportingController` on every Event this module publishes.
const REPORTING_CONTROLLER: &str = "relay-ctrl";

/// Watch the grants ConfigMap in `namespace`, swapping `table` on every
/// delivery. `ready_tx` fires once the initial sync has landed.
pub async fn watch_grants(
    client: KubeClient,
    namespace: &str,
    table: Arc<RwLock<GrantsTable>>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let config =
        watcher::Config::default().fields(&format!("metadata.name={GRANTS_CONFIGMAP_NAME}"));
    let mut stream = watcher::watcher(api, config).boxed();

    // Accumulated across a re-list (Init..InitDone) so the live table is
    // never transiently emptied under a caller.
    let mut scratch: Option<ConfigMap> = None;

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("grants watcher error: {e}"))?
    {
        match event {
            watcher::Event::Init => scratch = None,
            watcher::Event::InitApply(cm) => scratch = Some(cm),
            watcher::Event::InitDone => {
                match scratch.take() {
                    Some(cm) => install(&client, namespace, &cm, &table).await,
                    // No ConfigMap on the cluster: an empty table, which
                    // authorizes nobody. Never a reason to keep the old one.
                    None => *table.write().await = GrantsTable::default(),
                }
                let _ = ready_tx.send(true);
            }
            watcher::Event::Apply(cm) => install(&client, namespace, &cm, &table).await,
            watcher::Event::Delete(_) => {
                tracing::warn!("grants ConfigMap deleted; every grant row is revoked");
                *table.write().await = GrantsTable::default();
            }
        }
    }

    tracing::warn!("grants watcher stream ended");
    Ok(())
}

/// Swap one delivery into the live table and report its bad rows.
async fn install(
    client: &KubeClient,
    namespace: &str,
    cm: &ConfigMap,
    table: &Arc<RwLock<GrantsTable>>,
) {
    let (parsed, errors) = apply_delivery(cm);
    tracing::info!(
        rows = parsed.len(),
        rejected = errors.len(),
        "grants delivery applied"
    );
    *table.write().await = parsed;

    for err in &errors {
        tracing::warn!(row = %err.key, reason = %err.reason, "grant row rejected");
    }
    if let Err(e) = publish_row_errors(client, namespace, cm, &errors).await {
        tracing::warn!(error = %e, "failed to publish grant row Warning Events");
    }
}

/// Raise one Warning Event per rejected row on the grants ConfigMap, each
/// naming the row key and why it was rejected. A clean delivery publishes
/// nothing.
pub async fn publish_row_errors(
    client: &KubeClient,
    namespace: &str,
    grants: &ConfigMap,
    errors: &[RowError],
) -> Result<(), String> {
    if errors.is_empty() {
        return Ok(());
    }

    let mut reference = grants.object_ref(&());
    if reference.namespace.is_none() {
        reference.namespace = Some(namespace.to_string());
    }

    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: REPORTING_CONTROLLER.into(),
            instance: None,
        },
    );

    for err in errors {
        recorder
            .publish(
                &Event {
                    type_: EventType::Warning,
                    reason: "InvalidGrantRow".into(),
                    // Per-row so each rejected row stays separately
                    // addressable rather than collapsing into one series.
                    action: format!("ValidateRow:{}", err.key),
                    note: Some(format!(
                        "grant row \"{}\" rejected and treated as absent: {}",
                        err.key, err.reason
                    )),
                    secondary: None,
                },
                &reference,
            )
            .await
            .map_err(|e| format!("publishing Warning Event for row {}: {e}", err.key))?;
    }

    Ok(())
}
