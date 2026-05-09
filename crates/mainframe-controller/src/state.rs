use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::crd::Mainframe;

pub struct ControllerState {
    mainframes: RwLock<HashMap<String, Mainframe>>,
    last_generations: RwLock<HashMap<String, i64>>,
}

impl ControllerState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mainframes: RwLock::new(HashMap::new()),
            last_generations: RwLock::new(HashMap::new()),
        })
    }

    pub async fn set_mainframe(&self, name: String, mainframe: Mainframe) {
        self.mainframes.write().await.insert(name, mainframe);
    }

    pub async fn get_mainframe(&self, name: &str) -> Option<Mainframe> {
        self.mainframes.read().await.get(name).cloned()
    }

    pub async fn remove_mainframe(&self, name: &str) {
        self.mainframes.write().await.remove(name);
        self.last_generations.write().await.remove(name);
    }

    pub async fn clear(&self) {
        self.mainframes.write().await.clear();
        self.last_generations.write().await.clear();
    }

    pub async fn list_names(&self) -> Vec<String> {
        self.mainframes.read().await.keys().cloned().collect()
    }

    pub async fn count(&self) -> usize {
        self.mainframes.read().await.len()
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
    use crate::crd::{HostPathSource, MainframeSource, MainframeSpec};

    fn test_mainframe(name: &str) -> Mainframe {
        Mainframe::new(
            name,
            MainframeSpec {
                source: MainframeSource {
                    kind: "HostPath".into(),
                    host_path: Some(HostPathSource {
                        path: format!("/host/sycophant/{name}"),
                    }),
                },
            },
        )
    }

    #[tokio::test]
    async fn count_reflects_insertions() {
        let state = ControllerState::new();
        assert_eq!(state.count().await, 0);
        state
            .set_mainframe("default".into(), test_mainframe("default"))
            .await;
        assert_eq!(state.count().await, 1);
    }

    #[tokio::test]
    async fn remove_drops_mainframe_and_generation() {
        let state = ControllerState::new();
        state
            .set_mainframe("default".into(), test_mainframe("default"))
            .await;
        state.record_generation("default", 7).await;
        state.remove_mainframe("default").await;
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
        state.set_mainframe("a".into(), test_mainframe("a")).await;
        state.set_mainframe("b".into(), test_mainframe("b")).await;
        state.record_generation("a", 1).await;
        state.clear().await;
        assert_eq!(state.count().await, 0);
        assert!(state.last_generation("a").await.is_none());
    }

    #[tokio::test]
    async fn list_names_returns_inserted_keys() {
        let state = ControllerState::new();
        state
            .set_mainframe("alpha".into(), test_mainframe("alpha"))
            .await;
        state
            .set_mainframe("beta".into(), test_mainframe("beta"))
            .await;
        let mut names = state.list_names().await;
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[tokio::test]
    async fn list_names_empty_when_no_mainframes() {
        let state = ControllerState::new();
        assert!(state.list_names().await.is_empty());
    }
}
