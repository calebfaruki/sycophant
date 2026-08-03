//! Path resolution + read for the per-workspace kernel directory.
//!
//! Every read is rooted at `<kernels_root>/<workspace>/` and refuses to
//! escape that root via `..`, absolute paths, or symlinks. Skill and agent
//! requests are funneled through typed resolvers so callers never construct
//! raw filesystem paths.

use std::path::{Path, PathBuf};

/// Convention-driven resolver. Workspace identity comes from the runtime
/// config (`config.workspace`), fixed at pod start; the kernel never
/// derives it from tool arguments.
pub struct Kernel {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("not found")]
    NotFound,
    #[error("invalid name: {0}")]
    InvalidName(String),
    #[error("path escapes workspace root")]
    PathEscape,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Kernel {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Read the primary persona file (`AGENTS.md`) for the given workspace.
    pub fn read_primary_agent(&self, workspace: &str) -> Result<String, KernelError> {
        self.read_md(workspace, Path::new("AGENTS.md"))
    }

    /// Read a sub-agent's persona file (`agents/<name>.md`).
    pub fn read_agent(&self, workspace: &str, name: &str) -> Result<String, KernelError> {
        validate_basename(name)?;
        let rel = Path::new("agents").join(format!("{name}.md"));
        self.read_md(workspace, &rel)
    }

    /// Read a skill file (`skills/<name>.md`).
    pub fn read_skill(&self, workspace: &str, name: &str) -> Result<String, KernelError> {
        validate_basename(name)?;
        let rel = Path::new("skills").join(format!("{name}.md"));
        self.read_md(workspace, &rel)
    }

    /// Enumerate skill names (basenames of `.md` files under `skills/`).
    pub fn list_skills(&self, workspace: &str) -> Result<Vec<String>, KernelError> {
        self.list_md_basenames(workspace, "skills")
    }

    /// Enumerate sub-agent names (basenames of `.md` files under `agents/`).
    pub fn list_agents(&self, workspace: &str) -> Result<Vec<String>, KernelError> {
        self.list_md_basenames(workspace, "agents")
    }

    fn workspace_root(&self, workspace: &str) -> Result<PathBuf, KernelError> {
        validate_basename(workspace)?;
        Ok(self.root.join(workspace))
    }

    fn read_md(&self, workspace: &str, rel: &Path) -> Result<String, KernelError> {
        let ws_root = self.workspace_root(workspace)?;
        let full = ws_root.join(rel);
        // Guard against symlinks pointing outside the workspace root.
        let canonical = full.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => KernelError::NotFound,
            _ => KernelError::Io(e),
        })?;
        let canonical_root = ws_root.canonicalize().map_err(KernelError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(KernelError::PathEscape);
        }
        if canonical.extension().and_then(|s| s.to_str()) != Some("md") {
            return Err(KernelError::InvalidName(rel.display().to_string()));
        }
        std::fs::read_to_string(&canonical).map_err(KernelError::Io)
    }

    fn list_md_basenames(&self, workspace: &str, subdir: &str) -> Result<Vec<String>, KernelError> {
        let ws_root = self.workspace_root(workspace)?;
        let dir = ws_root.join(subdir);
        let read = std::fs::read_dir(&dir);
        let read = match read {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(KernelError::Io(e)),
        };
        let mut names: Vec<String> = read
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    return None;
                }
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .collect();
        names.sort();
        Ok(names)
    }
}

/// Extract a short description from a markdown blob: the first non-empty,
/// non-heading paragraph, trimmed and collapsed to a single line. Used by
/// `ListAgents` so the orchestrator's `Agents()` tool can present
/// human-readable choices.
pub fn first_paragraph(body: &str) -> String {
    let mut buf = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !buf.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(trimmed);
    }
    buf
}

/// Names accepted as components: non-empty, no path separators, no `..`,
/// no leading dot. Same shape applied to skill names, agent names, and
/// workspace names — anything that resolves to a directory or file
/// basename within the kernel root.
fn validate_basename(name: &str) -> Result<(), KernelError> {
    if name.is_empty() {
        return Err(KernelError::InvalidName(name.to_string()));
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(KernelError::InvalidName(name.to_string()));
    }
    if name.starts_with('.') {
        return Err(KernelError::InvalidName(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_md(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    #[test]
    fn read_primary_agent_returns_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/AGENTS.md", "# Persona\n\nHello.");
        let kernel = Kernel::new(tmp.path());
        let content = kernel.read_primary_agent("ws1").unwrap();
        assert!(content.contains("Hello."));
    }

    #[test]
    fn read_agent_returns_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/agents/alice.md", "alice persona");
        let kernel = Kernel::new(tmp.path());
        assert_eq!(kernel.read_agent("ws1", "alice").unwrap(), "alice persona");
    }

    #[test]
    fn read_skill_returns_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/skills/classify.md", "classify body");
        let kernel = Kernel::new(tmp.path());
        assert_eq!(
            kernel.read_skill("ws1", "classify").unwrap(),
            "classify body"
        );
    }

    #[test]
    fn missing_file_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("ws1")).unwrap();
        let kernel = Kernel::new(tmp.path());
        assert!(matches!(
            kernel.read_skill("ws1", "missing"),
            Err(KernelError::NotFound)
        ));
    }

    #[test]
    fn rejects_path_traversal_in_name() {
        let tmp = tempfile::tempdir().unwrap();
        let kernel = Kernel::new(tmp.path());
        assert!(matches!(
            kernel.read_skill("ws1", "../etc/passwd"),
            Err(KernelError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_dotfile_names() {
        let tmp = tempfile::tempdir().unwrap();
        let kernel = Kernel::new(tmp.path());
        assert!(matches!(
            kernel.read_skill("ws1", ".secret"),
            Err(KernelError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_symlink_escaping_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.md");
        fs::write(&outside, "leaked").unwrap();
        let ws = tmp.path().join("ws1/skills");
        fs::create_dir_all(&ws).unwrap();
        let link = ws.join("evil.md");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let kernel = Kernel::new(tmp.path());
        let err = kernel.read_skill("ws1", "evil").unwrap_err();
        assert!(matches!(err, KernelError::PathEscape));
    }

    #[test]
    fn list_skills_returns_sorted_basenames() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/skills/zeta.md", "z");
        write_md(tmp.path(), "ws1/skills/alpha.md", "a");
        write_md(tmp.path(), "ws1/skills/notes.txt", "ignored");
        let kernel = Kernel::new(tmp.path());
        let names = kernel.list_skills("ws1").unwrap();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn list_agents_returns_sorted_basenames() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/agents/bob.md", "b");
        write_md(tmp.path(), "ws1/agents/alice.md", "a");
        let kernel = Kernel::new(tmp.path());
        let names = kernel.list_agents("ws1").unwrap();
        assert_eq!(names, vec!["alice", "bob"]);
    }

    #[test]
    fn list_skills_empty_when_directory_missing() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("ws1")).unwrap();
        let kernel = Kernel::new(tmp.path());
        assert!(kernel.list_skills("ws1").unwrap().is_empty());
    }

    #[test]
    fn first_paragraph_skips_headings_and_blank_lines() {
        let body = "# Title\n\n## Section\n\nThis is the description.\n\nNext paragraph.\n";
        assert_eq!(first_paragraph(body), "This is the description.");
    }

    #[test]
    fn first_paragraph_joins_consecutive_lines() {
        let body = "First line.\nSecond line.\n\nLater paragraph.";
        assert_eq!(first_paragraph(body), "First line. Second line.");
    }

    #[test]
    fn first_paragraph_empty_when_only_headings() {
        let body = "# Just a heading\n## And another\n";
        assert_eq!(first_paragraph(body), "");
    }

    #[test]
    fn validate_basename_rejects_path_separators() {
        assert!(validate_basename("foo/bar").is_err());
        assert!(validate_basename("foo\\bar").is_err());
    }

    #[test]
    fn validate_basename_accepts_normal_names() {
        assert!(validate_basename("alice").is_ok());
        assert!(validate_basename("classify").is_ok());
        assert!(validate_basename("foo-bar_baz").is_ok());
    }
}
