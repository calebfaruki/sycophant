use std::collections::HashMap;
use std::path::Path;

use crate::prompt;

pub(crate) async fn discover_agents(agents_dir: &Path) -> Result<HashMap<String, String>, String> {
    let mut agents = HashMap::new();
    let entries = std::fs::read_dir(agents_dir).map_err(|e| {
        format!(
            "failed to read agents directory {}: {e}",
            agents_dir.display()
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read directory entry: {e}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("invalid agent directory name: {}", path.display()))?
            .to_string();
        let system_prompt = prompt::load_system_prompt(&path).await?;
        agents.insert(name, system_prompt);
    }

    if agents.is_empty() {
        return Err(format!(
            "no agent directories found in {}",
            agents_dir.display()
        ));
    }

    Ok(agents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_agents_returns_hashmap() {
        let tmp = tempfile::tempdir().unwrap();
        let research = tmp.path().join("research");
        let writer = tmp.path().join("writer");
        std::fs::create_dir(&research).unwrap();
        std::fs::create_dir(&writer).unwrap();
        std::fs::write(research.join("prompt.md"), "Research agent").unwrap();
        std::fs::write(writer.join("prompt.md"), "Writer agent").unwrap();

        let agents = discover_agents(tmp.path()).await.unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents["research"], "Research agent");
        assert_eq!(agents["writer"], "Writer agent");
    }

    #[tokio::test]
    async fn discover_agents_skips_files() {
        let tmp = tempfile::tempdir().unwrap();
        let agent = tmp.path().join("myagent");
        std::fs::create_dir(&agent).unwrap();
        std::fs::write(agent.join("prompt.md"), "Agent prompt").unwrap();
        std::fs::write(tmp.path().join("README.md"), "Not an agent").unwrap();

        let agents = discover_agents(tmp.path()).await.unwrap();
        assert_eq!(agents.len(), 1);
        assert!(agents.contains_key("myagent"));
    }

    #[tokio::test]
    async fn discover_agents_empty_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_agents(tmp.path()).await.is_err());
    }
}
