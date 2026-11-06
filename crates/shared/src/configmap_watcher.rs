//! Watch a labeled set of ConfigMaps and rebuild a live table from the whole
//! set on every delivery.
//!
//! A ConfigMap added after boot (a new toolset, say) takes
//! effect without a pod roll, so the whole label set is watched and every
//! present ConfigMap merged on each delivery. The raw `watcher::Event` stream is consumed rather than
//! `watch_object` so the re-list boundary stays visible and a re-list swaps
//! atomically instead of leaking a half-built table.
//!
//! `merge` folds the current set into a fresh table and returns the rejection
//! reasons for any ConfigMap it dropped; `subject` names the delivery in logs.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::runtime::watcher;
use kube::{Api, Client as KubeClient};
use tokio::sync::RwLock;

/// Watch the ConfigMaps in `namespace` matching `label`, rebuilding `table` via
/// `merge` on every delivery. `ready_tx` fires once the initial sync lands.
pub async fn watch_configmaps<T, M>(
    client: KubeClient,
    namespace: &str,
    label: &str,
    subject: &str,
    table: Arc<RwLock<T>>,
    ready_tx: tokio::sync::watch::Sender<bool>,
    merge: M,
) -> Result<(), String>
where
    T: Send + Sync,
    M: Fn(&BTreeMap<String, ConfigMap>) -> (T, Vec<String>),
{
    let api: Api<ConfigMap> = Api::namespaced(client, namespace);
    let config = watcher::Config::default().labels(label);
    let mut stream = watcher::watcher(api, config).boxed();

    // The live ConfigMap set, keyed by name. A re-list rebuilds it into
    // `scratch` first so the live table is never transiently emptied.
    let mut current: BTreeMap<String, ConfigMap> = BTreeMap::new();
    let mut scratch: BTreeMap<String, ConfigMap> = BTreeMap::new();

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("{subject} watcher error: {e}"))?
    {
        match event {
            watcher::Event::Init => scratch.clear(),
            watcher::Event::InitApply(cm) => {
                scratch.insert(key(&cm), cm);
            }
            watcher::Event::InitDone => {
                current = std::mem::take(&mut scratch);
                install(&current, &table, &merge, subject).await;
                let _ = ready_tx.send(true);
            }
            watcher::Event::Apply(cm) => {
                current.insert(key(&cm), cm);
                install(&current, &table, &merge, subject).await;
            }
            watcher::Event::Delete(cm) => {
                current.remove(&key(&cm));
                install(&current, &table, &merge, subject).await;
            }
        }
    }

    tracing::warn!("{} watcher stream ended", subject);
    Ok(())
}

/// Merge the `data_key` entry of every ConfigMap, parsed by `parse`, into one
/// map. A ConfigMap missing the key or failing to parse is reported and skipped,
/// never emptying the ones that parsed. One release owns each name, so
/// overlapping entries are equal by construction.
pub fn merge_configmaps<'a, I, V, F>(
    cms: I,
    data_key: &str,
    parse: F,
) -> (HashMap<String, V>, Vec<String>)
where
    I: IntoIterator<Item = &'a ConfigMap>,
    F: Fn(&str) -> Result<HashMap<String, V>, String>,
{
    let mut map = HashMap::new();
    let mut errors = Vec::new();
    for cm in cms {
        let name = cm.metadata.name.as_deref().unwrap_or("<unnamed>");
        let Some(raw) = cm.data.as_ref().and_then(|d| d.get(data_key)) else {
            errors.push(format!("{name}: no {data_key} key"));
            continue;
        };
        match parse(raw) {
            Ok(parsed) => map.extend(parsed),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    (map, errors)
}

/// Parse each ConfigMap's whole flat data with `parse`, keyed by its
/// `app.kubernetes.io/name` label. A ConfigMap without the label or failing to
/// parse is reported and skipped.
pub fn merge_labelled<'a, I, V, F>(cms: I, parse: F) -> (HashMap<String, V>, Vec<String>)
where
    I: IntoIterator<Item = &'a ConfigMap>,
    F: Fn(&BTreeMap<String, String>) -> Result<V, String>,
{
    let mut map = HashMap::new();
    let mut errors = Vec::new();
    let empty = BTreeMap::new();
    for cm in cms {
        let name = cm.metadata.name.as_deref().unwrap_or("<unnamed>");
        let Some(key) = cm
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get("app.kubernetes.io/name"))
        else {
            errors.push(format!("{name}: no app.kubernetes.io/name label"));
            continue;
        };
        match parse(cm.data.as_ref().unwrap_or(&empty)) {
            Ok(value) => {
                map.insert(key.clone(), value);
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    (map, errors)
}

fn key(cm: &ConfigMap) -> String {
    cm.metadata.name.clone().unwrap_or_default()
}

/// Rebuild the merged table from the current ConfigMap set and swap it in.
async fn install<T, M>(
    current: &BTreeMap<String, ConfigMap>,
    table: &Arc<RwLock<T>>,
    merge: &M,
    subject: &str,
) where
    M: Fn(&BTreeMap<String, ConfigMap>) -> (T, Vec<String>),
{
    let (merged, errors) = merge(current);
    tracing::info!(
        configmaps = current.len(),
        rejected = errors.len(),
        "{} delivery applied",
        subject
    );
    for err in &errors {
        tracing::warn!(reason = %err, "{} ConfigMap skipped", subject);
    }
    *table.write().await = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "entry.yaml";

    fn cm(name: &str, data: Option<&str>) -> ConfigMap {
        ConfigMap {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            data: data.map(|d| BTreeMap::from([(KEY.to_string(), d.to_string())])),
            ..Default::default()
        }
    }

    fn merge(cms: &[ConfigMap]) -> (HashMap<String, String>, Vec<String>) {
        merge_configmaps(cms, KEY, |raw| {
            serde_yaml::from_str(raw).map_err(|e| e.to_string())
        })
    }

    #[test]
    fn merge_unions_every_configmap() {
        let (merged, errors) = merge(&[cm("a", Some("a: one\n")), cm("b", Some("b: two\n"))]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(merged.get("a").map(String::as_str), Some("one"));
        assert_eq!(merged.get("b").map(String::as_str), Some("two"));
    }

    #[test]
    fn merge_skips_a_bad_configmap_without_dropping_the_good_ones() {
        let (merged, errors) =
            merge(&[cm("good", Some("good: x\n")), cm("bad", Some("{not yaml"))]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("bad: "));
        assert!(merged.contains_key("good"));
    }

    #[test]
    fn merge_reports_a_configmap_missing_its_data_key() {
        let (merged, errors) = merge(&[cm("empty", None)]);
        assert_eq!(errors, vec![format!("empty: no {KEY} key")]);
        assert!(merged.is_empty());
    }
}
