use std::sync::Arc;

use futures::{StreamExt, TryStreamExt};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};

use crate::crd::{TightbeamModel, TightbeamProvider};
use crate::state::ControllerState;

pub async fn watch_models(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    let api: Api<TightbeamModel> = Api::namespaced(client, namespace);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("watcher error: {e}"))?
    {
        match event {
            Event::Apply(model) => {
                let name = model.metadata.name.clone().unwrap_or_default();
                tracing::info!(model = %name, "model applied");
                state.set_model_spec(name, model.spec).await;
            }
            Event::Delete(model) => {
                let name = model.metadata.name.clone().unwrap_or_default();
                tracing::info!(model = %name, "model deleted");
                state.remove_model(&name).await;
            }
            Event::Init => {
                tracing::info!("model watcher initialized");
                state.clear_models().await;
            }
            Event::InitApply(model) => {
                let name = model.metadata.name.clone().unwrap_or_default();
                tracing::info!(model = %name, "model discovered");
                state.set_model_spec(name, model.spec).await;
            }
            Event::InitDone => {
                tracing::info!("model watcher initial sync complete");
                let _ = ready_tx.send(true);
            }
        }
    }

    tracing::warn!("model watcher stream ended");
    Ok(())
}

pub async fn watch_providers(
    client: Client,
    namespace: &str,
    state: Arc<ControllerState>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    let api: Api<TightbeamProvider> = Api::namespaced(client, namespace);
    let mut stream = watcher::watcher(api, watcher::Config::default()).boxed();

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("provider watcher error: {e}"))?
    {
        match event {
            Event::Apply(provider) => {
                let name = provider.metadata.name.clone().unwrap_or_default();
                tracing::info!(provider = %name, "provider applied");
                state.set_provider_spec(name, provider.spec).await;
            }
            Event::Delete(provider) => {
                let name = provider.metadata.name.clone().unwrap_or_default();
                tracing::info!(provider = %name, "provider deleted");
                state.remove_provider(&name).await;
            }
            Event::Init => {
                tracing::info!("provider watcher initialized");
                state.clear_providers().await;
            }
            Event::InitApply(provider) => {
                let name = provider.metadata.name.clone().unwrap_or_default();
                tracing::info!(provider = %name, "provider discovered");
                state.set_provider_spec(name, provider.spec).await;
            }
            Event::InitDone => {
                tracing::info!("provider watcher initial sync complete");
                let _ = ready_tx.send(true);
            }
        }
    }

    tracing::warn!("provider watcher stream ended");
    Ok(())
}
