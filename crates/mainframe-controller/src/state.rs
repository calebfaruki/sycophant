use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::crd::Mainframe;

pub struct ControllerState {
    mainframes: RwLock<HashMap<String, Mainframe>>,
    last_revisions: RwLock<HashMap<String, String>>,
    last_generations: RwLock<HashMap<String, i64>>,
    kube_client: Option<kube::Client>,
    namespace: String,
    data_dir: String,
}

impl ControllerState {
    pub fn new(
        kube_client: Option<kube::Client>,
        namespace: String,
        data_dir: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            mainframes: RwLock::new(HashMap::new()),
            last_revisions: RwLock::new(HashMap::new()),
            last_generations: RwLock::new(HashMap::new()),
            kube_client,
            namespace,
            data_dir,
        })
    }

    pub fn kube_client(&self) -> Option<&kube::Client> {
        self.kube_client.as_ref()
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }

    pub async fn set_mainframe(&self, name: String, mainframe: Mainframe) {
        self.mainframes.write().await.insert(name, mainframe);
    }

    pub async fn get_mainframe(&self, name: &str) -> Option<Mainframe> {
        self.mainframes.read().await.get(name).cloned()
    }

    pub async fn remove_mainframe(&self, name: &str) {
        self.mainframes.write().await.remove(name);
        self.last_revisions.write().await.remove(name);
        self.last_generations.write().await.remove(name);
    }

    pub async fn clear(&self) {
        self.mainframes.write().await.clear();
        self.last_revisions.write().await.clear();
        self.last_generations.write().await.clear();
    }

    pub async fn list_names(&self) -> Vec<String> {
        self.mainframes.read().await.keys().cloned().collect()
    }

    pub async fn count(&self) -> usize {
        self.mainframes.read().await.len()
    }

    pub async fn record_revision(&self, name: &str, revision: String) {
        self.last_revisions
            .write()
            .await
            .insert(name.to_string(), revision);
    }

    pub async fn last_revision(&self, name: &str) -> Option<String> {
        self.last_revisions.read().await.get(name).cloned()
    }

    pub async fn record_generation(&self, name: &str, generation: i64) {
        self.last_generations
            .write()
            .await
            .insert(name.to_string(), generation);
    }

    pub async fn last_generation(&self, name: &str) -> Option<i64> {
        self.last_generations.read().await.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{MainframeSource, MainframeSpec, S3Source};

    fn test_mainframe(name: &str) -> Mainframe {
        Mainframe::new(
            name,
            MainframeSpec {
                source: MainframeSource {
                    s3: S3Source {
                        endpoint: "http://localhost:9000".into(),
                        bucket: "test".into(),
                        prefix: String::new(),
                        region: "us-east-1".into(),
                        secret_name: "creds".into(),
                    },
                },
            },
        )
    }

    #[tokio::test]
    async fn count_reflects_insertions() {
        let state = ControllerState::new(None, String::new(), "/tmp".into());
        assert_eq!(state.count().await, 0);
        state
            .set_mainframe("default".into(), test_mainframe("default"))
            .await;
        assert_eq!(state.count().await, 1);
    }

    #[tokio::test]
    async fn remove_drops_mainframe_and_revision() {
        let state = ControllerState::new(None, String::new(), "/tmp".into());
        state
            .set_mainframe("default".into(), test_mainframe("default"))
            .await;
        state.record_revision("default", "abc".into()).await;
        state.remove_mainframe("default").await;
        assert_eq!(state.count().await, 0);
        assert!(state.last_revision("default").await.is_none());
    }

    #[tokio::test]
    async fn revision_round_trip() {
        let state = ControllerState::new(None, String::new(), "/tmp".into());
        state.record_revision("default", "abc".into()).await;
        assert_eq!(state.last_revision("default").await.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn clear_empties_state() {
        let state = ControllerState::new(None, String::new(), "/tmp".into());
        state.set_mainframe("a".into(), test_mainframe("a")).await;
        state.set_mainframe("b".into(), test_mainframe("b")).await;
        state.record_revision("a", "rev".into()).await;
        state.clear().await;
        assert_eq!(state.count().await, 0);
        assert!(state.last_revision("a").await.is_none());
    }
}
