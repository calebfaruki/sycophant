use std::collections::HashMap;
use std::path::Path;

use crate::prompt;

pub(crate) async fn discover_prompts(
    prompts_dir: &Path,
) -> Result<HashMap<String, String>, String> {
    let mut prompts = HashMap::new();
    let entries = std::fs::read_dir(prompts_dir).map_err(|e| {
        format!(
            "failed to read prompts directory {}: {e}",
            prompts_dir.display()
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
            .ok_or_else(|| format!("invalid prompt directory name: {}", path.display()))?
            .to_string();
        let system_prompt = prompt::load_system_prompt(&path).await?;
        prompts.insert(name, system_prompt);
    }

    if prompts.is_empty() {
        return Err(format!(
            "no prompt directories found in {}",
            prompts_dir.display()
        ));
    }

    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_prompts_returns_hashmap() {
        let tmp = tempfile::tempdir().unwrap();
        let research = tmp.path().join("research");
        let writer = tmp.path().join("writer");
        std::fs::create_dir(&research).unwrap();
        std::fs::create_dir(&writer).unwrap();
        std::fs::write(research.join("prompt.md"), "Research prompt").unwrap();
        std::fs::write(writer.join("prompt.md"), "Writer prompt").unwrap();

        let prompts = discover_prompts(tmp.path()).await.unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts["research"], "Research prompt");
        assert_eq!(prompts["writer"], "Writer prompt");
    }

    #[tokio::test]
    async fn discover_prompts_skips_files() {
        let tmp = tempfile::tempdir().unwrap();
        let prompt = tmp.path().join("myprompt");
        std::fs::create_dir(&prompt).unwrap();
        std::fs::write(prompt.join("prompt.md"), "System prompt").unwrap();
        std::fs::write(tmp.path().join("README.md"), "Not a prompt").unwrap();

        let prompts = discover_prompts(tmp.path()).await.unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts.contains_key("myprompt"));
    }

    #[tokio::test]
    async fn discover_prompts_empty_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_prompts(tmp.path()).await.is_err());
    }
}
