use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Patch, PatchParams};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};
use serde_json::json;
use tracing::{error, info, warn};

use crate::crd::{Mainframe, MainframeCondition, S3Source};
use crate::source::{self, S3Credentials};
use crate::state::ControllerState;

pub async fn watch_mainframes(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> anyhow::Result<()> {
    let api: Api<Mainframe> = Api::namespaced(client.clone(), namespace);
    let watcher_config = watcher::Config::default();
    let mut stream = watcher::watcher(api, watcher_config).boxed();

    while let Some(event) = stream.try_next().await? {
        match event {
            Event::Apply(mf) => {
                let name = mf.metadata.name.clone().unwrap_or_default();
                let generation = mf.metadata.generation.unwrap_or(0);
                if state.last_generation(&name).await == Some(generation) {
                    // Status-patch echo: spec hasn't changed, skip reconcile.
                    state.set_mainframe(name.clone(), mf.clone()).await;
                    continue;
                }
                info!(mainframe = %name, generation, "mainframe applied");
                state.set_mainframe(name.clone(), mf.clone()).await;
                state.record_generation(&name, generation).await;
                reconcile_one(&client, namespace, &state, &name, &mf).await;
            }
            Event::Delete(mf) => {
                let name = mf.metadata.name.clone().unwrap_or_default();
                info!(mainframe = %name, "mainframe deleted");
                state.remove_mainframe(&name).await;
                if let Err(e) = remove_subdir(state.data_dir(), &name).await {
                    warn!(error = %e, "failed to clear mainframe subdir on delete");
                }
            }
            Event::Init => {
                info!("mainframe watcher initialized");
                state.clear().await;
            }
            Event::InitApply(mf) => {
                let name = mf.metadata.name.clone().unwrap_or_default();
                let generation = mf.metadata.generation.unwrap_or(0);
                state.set_mainframe(name.clone(), mf.clone()).await;
                state.record_generation(&name, generation).await;
                reconcile_one(&client, namespace, &state, &name, &mf).await;
            }
            Event::InitDone => {
                let count = state.count().await;
                info!(
                    mainframe_count = count,
                    "mainframe watcher initial sync complete"
                );
                let _ = ready_tx.send(true);
            }
        }
    }

    warn!("mainframe watcher stream ended");
    Ok(())
}

/// Periodic reconcile loop. Sleeps `loop_sleep_seconds` between ticks; each
/// tick re-reconciles every known Mainframe regardless of its configured
/// refresh interval. Stage 2 stub: simple cadence over per-CR scheduling.
pub async fn refresh_loop(
    client: Client,
    namespace: String,
    state: Arc<ControllerState>,
    loop_sleep_seconds: u64,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(loop_sleep_seconds)).await;
        let names = state.list_names().await;
        for name in names {
            if let Some(mf) = state.get_mainframe(&name).await {
                reconcile_one(&client, &namespace, &state, &name, &mf).await;
            }
        }
    }
}

async fn reconcile_one(
    client: &Client,
    namespace: &str,
    state: &Arc<ControllerState>,
    name: &str,
    mf: &Mainframe,
) {
    match try_reconcile(client, namespace, state, name, &mf.spec.source.s3).await {
        Ok(report) => {
            let last_rev = state.last_revision(name).await;
            if last_rev.as_deref() != Some(report.revision.as_str()) {
                info!(
                    mainframe = %name,
                    object_count = report.object_count,
                    revision = %report.revision,
                    "synced from s3"
                );
                state.record_revision(name, report.revision.clone()).await;
            }
            patch_status(
                client,
                namespace,
                name,
                MainframeCondition {
                    type_: "Ready".into(),
                    status: "True".into(),
                    reason: "Synced".into(),
                    message: format!("synced {} objects", report.object_count),
                    last_transition_time: Utc::now().to_rfc3339(),
                },
                Some(report.object_count),
                Some(report.revision),
            )
            .await;
        }
        Err(e) => {
            error!(mainframe = %name, error = %e, "reconcile failed");
            patch_status(
                client,
                namespace,
                name,
                MainframeCondition {
                    type_: "Ready".into(),
                    status: "False".into(),
                    reason: "SyncFailed".into(),
                    message: e,
                    last_transition_time: Utc::now().to_rfc3339(),
                },
                None,
                None,
            )
            .await;
        }
    }
}

async fn try_reconcile(
    client: &Client,
    namespace: &str,
    state: &Arc<ControllerState>,
    name: &str,
    s3: &S3Source,
) -> Result<source::SyncReport, String> {
    let creds = load_credentials(client, namespace, &s3.secret_name).await?;
    let data_dir = PathBuf::from(state.data_dir());
    let dest = data_dir.join(name);
    let etag_state = data_dir.join(".etags").join(format!("{name}.json"));
    tokio::fs::create_dir_all(&dest)
        .await
        .map_err(|e| format!("mkdir {} failed: {e}", dest.display()))?;
    source::pull_to(
        &s3.endpoint,
        &s3.bucket,
        &s3.prefix,
        &s3.region,
        &creds,
        &dest,
        &etag_state,
    )
    .await
}

async fn load_credentials(
    client: &Client,
    namespace: &str,
    secret_name: &str,
) -> Result<S3Credentials, String> {
    let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let secret = api
        .get(secret_name)
        .await
        .map_err(|e| format!("get secret {secret_name}: {e}"))?;
    let data = secret
        .data
        .ok_or_else(|| format!("secret {secret_name} has no data"))?;

    let access_key = data
        .get("AWS_ACCESS_KEY_ID")
        .or_else(|| data.get("access_key_id"))
        .ok_or_else(|| {
            format!("secret {secret_name} missing AWS_ACCESS_KEY_ID or access_key_id key")
        })?;
    let secret_key = data
        .get("AWS_SECRET_ACCESS_KEY")
        .or_else(|| data.get("secret_access_key"))
        .ok_or_else(|| {
            format!("secret {secret_name} missing AWS_SECRET_ACCESS_KEY or secret_access_key key")
        })?;

    let access_key_id = String::from_utf8(access_key.0.clone())
        .map_err(|e| format!("access key not UTF-8: {e}"))?;
    let secret_access_key = String::from_utf8(secret_key.0.clone())
        .map_err(|e| format!("secret key not UTF-8: {e}"))?;

    Ok(S3Credentials {
        access_key_id,
        secret_access_key,
    })
}

async fn patch_status(
    client: &Client,
    namespace: &str,
    name: &str,
    condition: MainframeCondition,
    object_count: Option<u32>,
    revision: Option<String>,
) {
    let api: Api<Mainframe> = Api::namespaced(client.clone(), namespace);
    let now = Utc::now().to_rfc3339();
    let mut status = json!({
        "lastSync": now,
        "conditions": [condition],
    });
    if let Some(count) = object_count {
        status["objectCount"] = json!(count);
    }
    if let Some(rev) = revision {
        status["syncedRevision"] = json!(rev);
    }
    let patch = json!({ "status": status });
    let pp = PatchParams::default();
    if let Err(e) = api.patch_status(name, &pp, &Patch::Merge(&patch)).await {
        warn!(mainframe = %name, error = %e, "failed to patch status");
    }
}

async fn remove_subdir(dir: &str, name: &str) -> Result<(), String> {
    let path = PathBuf::from(dir).join(name);
    if !path.exists() {
        return Ok(());
    }
    tokio::fs::remove_dir_all(&path)
        .await
        .map_err(|e| format!("rmdir {}: {e}", path.display()))
}
