use std::collections::HashMap;
use std::path::Path;
use tokio::sync::RwLock;

use crate::prompts;

const ROUTER_AGENT: &str = "router";

pub struct PkmState {
    prompts: HashMap<String, String>,
    default_agent: String,
    active_agents: RwLock<HashMap<String, String>>,
}

impl PkmState {
    pub async fn new(pkm_dir: &Path) -> Result<Self, String> {
        // Layout mirrors ~/vault/: agents live in <PKM_DIR>/agents/<agent>/.
        // Future stages add skills/, projects/, etc.
        let agents_dir = pkm_dir.join("agents");
        let prompts = prompts::discover_prompts(&agents_dir).await?;

        if !prompts.contains_key(ROUTER_AGENT) {
            return Err(format!(
                "prompts directory must contain a '{ROUTER_AGENT}' subdirectory"
            ));
        }

        let default_agent = prompts
            .keys()
            .filter(|k| *k != ROUTER_AGENT)
            .min()
            .ok_or_else(|| "no non-router prompt directories found".to_string())?
            .clone();

        Ok(Self {
            prompts,
            default_agent,
            active_agents: RwLock::new(HashMap::new()),
        })
    }

    pub fn prompts(&self) -> &HashMap<String, String> {
        &self.prompts
    }

    pub fn router_prompt(&self) -> &str {
        // Safe by construction: new() validates the router key exists.
        &self.prompts[ROUTER_AGENT]
    }

    pub fn agent_prompt(&self, agent: &str) -> Option<&str> {
        self.prompts.get(agent).map(|s| s.as_str())
    }

    pub async fn active_agent(&self, workspace: &str) -> String {
        let guard = self.active_agents.read().await;
        guard
            .get(workspace)
            .cloned()
            .unwrap_or_else(|| self.default_agent.clone())
    }

    pub async fn set_active_agent(&self, workspace: &str, agent: &str) {
        let mut guard = self.active_agents.write().await;
        guard.insert(workspace.to_string(), agent.to_string());
    }

    /// Build a JSON schema for the system-agent's `select_agent` response.
    /// `agent_name` enum is the registered agents excluding the router itself.
    pub fn select_agent_schema(&self) -> String {
        let mut names: Vec<&str> = self
            .prompts
            .keys()
            .filter(|k| k.as_str() != ROUTER_AGENT)
            .map(|s| s.as_str())
            .collect();
        names.sort();

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "enum": names,
                }
            },
            "required": ["agent_name"],
            "additionalProperties": false
        });
        schema.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(pkm_dir: &Path) {
        let agents_dir = pkm_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        for name in &["router", "research", "writer"] {
            let sub = agents_dir.join(name);
            std::fs::create_dir(&sub).unwrap();
            std::fs::write(sub.join("prompt.md"), format!("{name} prompt")).unwrap();
        }
    }

    #[tokio::test]
    async fn new_loads_prompts() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let state = PkmState::new(tmp.path()).await.unwrap();
        assert_eq!(state.router_prompt(), "router prompt");
        assert_eq!(state.agent_prompt("research"), Some("research prompt"));
    }

    #[tokio::test]
    async fn new_errors_without_router() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("agents").join("research");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("prompt.md"), "x").unwrap();
        let result = PkmState::new(tmp.path()).await;
        match result {
            Err(msg) => assert!(msg.contains("router")),
            Ok(_) => panic!("expected error about missing router"),
        }
    }

    #[tokio::test]
    async fn default_agent_is_alphabetical_first_non_router() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let state = PkmState::new(tmp.path()).await.unwrap();
        // research < writer alphabetically, router excluded
        assert_eq!(state.active_agent("ws-1").await, "research");
    }

    #[tokio::test]
    async fn active_agent_persists() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let state = PkmState::new(tmp.path()).await.unwrap();
        state.set_active_agent("ws-1", "writer").await;
        assert_eq!(state.active_agent("ws-1").await, "writer");
        // Other workspaces still get the default
        assert_eq!(state.active_agent("ws-2").await, "research");
    }

    #[tokio::test]
    async fn select_agent_schema_excludes_router() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let state = PkmState::new(tmp.path()).await.unwrap();
        let schema: serde_json::Value = serde_json::from_str(&state.select_agent_schema()).unwrap();
        let enum_values = schema["properties"]["agent_name"]["enum"]
            .as_array()
            .unwrap();
        let names: Vec<&str> = enum_values.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(!names.contains(&"router"), "router must not be in enum");
        assert!(names.contains(&"research"));
        assert!(names.contains(&"writer"));
    }

    #[tokio::test]
    async fn select_agent_schema_strict_mode_required() {
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path());
        let state = PkmState::new(tmp.path()).await.unwrap();
        let schema: serde_json::Value = serde_json::from_str(&state.select_agent_schema()).unwrap();
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert_eq!(schema["required"], serde_json::json!(["agent_name"]));
        assert_eq!(schema["type"], serde_json::json!("object"));
    }
}
