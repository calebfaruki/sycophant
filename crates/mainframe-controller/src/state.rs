use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::crd::Source;

pub struct ControllerState {
    sources: RwLock<HashMap<String, Source>>,
    last_generations: RwLock<HashMap<String, i64>>,
}

impl ControllerState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sources: RwLock::new(HashMap::new()),
            last_generations: RwLock::new(HashMap::new()),
        })
    }

    pub async fn set_source(&self, name: String, source: Source) {
        self.sources.write().await.insert(name, source);
    }

    pub async fn get_source(&self, name: &str) -> Option<Source> {
        self.sources.read().await.get(name).cloned()
    }

    pub async fn remove_source(&self, name: &str) {
        self.sources.write().await.remove(name);
        self.last_generations.write().await.remove(name);
    }

    pub async fn clear(&self) {
        self.sources.write().await.clear();
        self.last_generations.write().await.clear();
    }

    pub async fn list_names(&self) -> Vec<String> {
        self.sources.read().await.keys().cloned().collect()
    }

    pub async fn count(&self) -> usize {
        self.sources.read().await.len()
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
    use crate::crd::{HostPathSource, SourceSpec};

    fn test_source(name: &str) -> Source {
        Source::new(
            name,
            SourceSpec {
                kind: "HostPath".into(),
                host_path: Some(HostPathSource {
                    path: format!("/host/sycophant/{name}"),
                }),
            },
        )
    }

    #[tokio::test]
    async fn count_reflects_insertions() {
        let state = ControllerState::new();
        assert_eq!(state.count().await, 0);
        state
            .set_source("default".into(), test_source("default"))
            .await;
        assert_eq!(state.count().await, 1);
    }

    #[tokio::test]
    async fn remove_drops_source_and_generation() {
        let state = ControllerState::new();
        state
            .set_source("default".into(), test_source("default"))
            .await;
        state.record_generation("default", 7).await;
        state.remove_source("default").await;
        assert_eq!(state.count().await, 0);
        assert!(state.last_generation("default").await.is_none());
    }

    #[tokio::test]
    async fn generation_round_trip() {
        let state = ControllerState::new();
        state.record_generation("default", 42).await;
        assert_eq!(state.last_generation("default").await, Some(42));
    }

    #[tokio::test]
    async fn clear_empties_state() {
        let state = ControllerState::new();
        state.set_source("a".into(), test_source("a")).await;
        state.set_source("b".into(), test_source("b")).await;
        state.record_generation("a", 1).await;
        state.clear().await;
        assert_eq!(state.count().await, 0);
        assert!(state.last_generation("a").await.is_none());
    }

    #[tokio::test]
    async fn list_names_returns_inserted_keys() {
        let state = ControllerState::new();
        state.set_source("alpha".into(), test_source("alpha")).await;
        state.set_source("beta".into(), test_source("beta")).await;
        let mut names = state.list_names().await;
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn list_names_empty_when_no_sources() {
        let state = ControllerState::new();
        assert!(state.list_names().await.is_empty());
    }
}
