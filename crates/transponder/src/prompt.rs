use std::path::Path;

pub(crate) async fn load_system_prompt(prompt_dir: &Path) -> Result<String, String> {
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
    async fn single_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("prompt.md"), "You are a helper.").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "You are a helper.");
    }

    #[tokio::test]
    async fn multiple_files_sorted_by_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("b.md"), "Second").unwrap();
        std::fs::write(tmp.path().join("a.md"), "First").unwrap();
        std::fs::write(tmp.path().join("c.md"), "Third").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "First\n\nSecond\n\nThird");
    }

    #[tokio::test]
    async fn nested_directories_sort_naturally() {
        let tmp = tempfile::tempdir().unwrap();
        let dir1 = tmp.path().join("stages").join("01_init");
        let dir2 = tmp.path().join("stages").join("02_run");
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("run.md"), "Run phase").unwrap();
        std::fs::write(dir1.join("init.md"), "Init phase").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "Init phase\n\nRun phase");
    }

    #[tokio::test]
    async fn non_md_files_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("prompt.md"), "Keep this").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "Ignore this").unwrap();
        std::fs::write(tmp.path().join("data.json"), "{}").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "Keep this");
    }

    #[tokio::test]
    async fn empty_directory_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let err = load_system_prompt(tmp.path()).await.unwrap_err();
        assert!(err.contains("no .md files"));
    }

    #[tokio::test]
    async fn all_empty_md_files_errors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "").unwrap();
        std::fs::write(tmp.path().join("b.md"), "   \n  ").unwrap();

        let err = load_system_prompt(tmp.path()).await.unwrap_err();
        assert!(err.contains("empty"));
    }

    #[tokio::test]
    async fn empty_files_skipped_among_non_empty() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "Content").unwrap();
        std::fs::write(tmp.path().join("b.md"), "").unwrap();
        std::fs::write(tmp.path().join("c.md"), "More content").unwrap();

        let result = load_system_prompt(tmp.path()).await.unwrap();
        assert_eq!(result, "Content\n\nMore content");
    }
}
