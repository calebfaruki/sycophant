use std::collections::HashMap;
use std::path::Path;

pub async fn discover_prompts(prompts_dir: &Path) -> Result<HashMap<String, String>, String> {
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
        let system_prompt = load_system_prompt(&path).await?;
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

async fn load_system_prompt(prompt_dir: &Path) -> Result<String, String> {
    let md_files = collect_md_files(prompt_dir).map_err(|e| {
        format!(
            "failed to read prompt directory {}: {e}",
            prompt_dir.display()
        )
    })?;

    if md_files.is_empty() {
        return Err(format!("no .md files found in {}", prompt_dir.display()));
    }

    let mut parts = Vec::new();
    for path in &md_files {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        if !content.trim().is_empty() {
            parts.push(content);
        }
    }

    if parts.is_empty() {
        return Err(format!(
            "all .md files in {} are empty",
            prompt_dir.display()
        ));
    }

    Ok(parts.join("\n\n"))
}

fn collect_md_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    collect_md_files_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_md_files_recursive(
    dir: &Path,
    files: &mut Vec<std::path::PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_md_files_recursive(&path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_returns_one_per_directory() {
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
    async fn discover_skips_files() {
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
    async fn discover_empty_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_prompts(tmp.path()).await.is_err());
    }

    #[tokio::test]
    async fn load_concatenates_md_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("b.md"), "Second").unwrap();
        std::fs::write(tmp.path().join("a.md"), "First").unwrap();
        std::fs::write(tmp.path().join("c.md"), "Third").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "First\n\nSecond\n\nThird");
    }

    #[tokio::test]
    async fn load_skips_empty_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "Content").unwrap();
        std::fs::write(tmp.path().join("b.md"), "").unwrap();
        std::fs::write(tmp.path().join("c.md"), "More content").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "Content\n\nMore content");
    }
}
