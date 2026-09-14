//! Path resolution + read for the per-workspace instructions directory.
//!
//! Every read is rooted at `<instructions_root>/<workspace>/` and refuses to
//! escape that root via `..`, absolute paths, or symlinks. Callers reach
//! content through generic path-based access (read, list, dispatch) rather than
//! constructing raw filesystem paths.

use std::path::{Path, PathBuf};

/// Convention-driven resolver. Workspace identity comes from the runtime
/// config (`config.workspace`), fixed at pod start; the resolver never
/// derives it from tool arguments.
pub struct Instructions {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum InstructionsError {
    #[error("not found")]
    NotFound,
    #[error("invalid name: {0}")]
    InvalidName(String),
    #[error("path escapes workspace root")]
    PathEscape,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Instructions {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Read the primary agent file (`AGENTS.md`) for the given workspace.
    pub fn read_primary_agent(&self, workspace: &str) -> Result<String, InstructionsError> {
        self.read_md(workspace, Path::new("AGENTS.md"))
    }

    fn workspace_root(&self, workspace: &str) -> Result<PathBuf, InstructionsError> {
        validate_basename(workspace)?;
        Ok(self.root.join(workspace))
    }

    /// Read an arbitrary nested relative path under the workspace root through
    /// the in-process guards (root confinement, no `..` traversal, symlink-escape
    /// rejection). `offset`/`limit` are 1-based line numbers applied after read;
    /// omitting both returns the whole body. Unlike the typed readers this does
    /// not require a `.md` extension, so attachments in a folder are readable.
    pub fn read(
        &self,
        workspace: &str,
        rel_path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, InstructionsError> {
        let body = self.read_rel(workspace, rel_path)?;
        Ok(slice_lines(&body, offset, limit))
    }

    /// Flat, sorted, capped list of the relative paths of every file under the
    /// workspace root, recursing arbitrary depth. Presentation is flat literal
    /// paths, never a nested tree. A missing or unreadable root yields an empty
    /// list rather than an error.
    pub fn list_tree(&self, workspace: &str, cap: usize) -> Vec<String> {
        let ws_root = match self.workspace_root(workspace) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let canonical_root = match ws_root.canonicalize() {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        collect_files(&canonical_root, &canonical_root, &mut out);
        out.sort();
        out.truncate(cap);
        out
    }

    /// Resolve and read a nested relative path, confined to the workspace root.
    /// Canonicalize + `starts_with(root)` catches symlink and traversal escape;
    /// `validate_relpath` rejects `..`, empty, backslash, and dotfile components
    /// up front.
    fn read_rel(&self, workspace: &str, rel_path: &str) -> Result<String, InstructionsError> {
        validate_relpath(rel_path)?;
        let ws_root = self.workspace_root(workspace)?;
        let full = ws_root.join(rel_path);
        let canonical = full.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => InstructionsError::NotFound,
            _ => InstructionsError::Io(e),
        })?;
        let canonical_root = ws_root.canonicalize().map_err(InstructionsError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(InstructionsError::PathEscape);
        }
        std::fs::read_to_string(&canonical).map_err(InstructionsError::Io)
    }

    fn read_md(&self, workspace: &str, rel: &Path) -> Result<String, InstructionsError> {
        let ws_root = self.workspace_root(workspace)?;
        let full = ws_root.join(rel);
        // Guard against symlinks pointing outside the workspace root.
        let canonical = full.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => InstructionsError::NotFound,
            _ => InstructionsError::Io(e),
        })?;
        let canonical_root = ws_root.canonicalize().map_err(InstructionsError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(InstructionsError::PathEscape);
        }
        if canonical.extension().and_then(|s| s.to_str()) != Some("md") {
            return Err(InstructionsError::InvalidName(rel.display().to_string()));
        }
        std::fs::read_to_string(&canonical).map_err(InstructionsError::Io)
    }
}

/// Extract a short description from a markdown blob: the first non-empty,
/// non-heading paragraph, trimmed and collapsed to a single line. Used by the
/// generic `list` verb so the orchestrator can present human-readable choices.
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
/// no leading dot — anything that resolves to a directory or file
/// basename within the instructions root.
fn validate_basename(name: &str) -> Result<(), InstructionsError> {
    if name.is_empty() {
        return Err(InstructionsError::InvalidName(name.to_string()));
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(InstructionsError::InvalidName(name.to_string()));
    }
    if name.starts_with('.') {
        return Err(InstructionsError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Validate an arbitrary nested relative path for the generic reader. Splits on
/// `/` and rejects any component that is empty, `.`, `..`, backslash-bearing, or
/// leading-dot; rejects absolute paths. `/` between components is allowed. The
/// canonicalize + `starts_with(root)` guard in `read_rel` still catches symlink
/// and traversal escape; this pre-check rejects the obvious cases early.
fn validate_relpath(rel: &str) -> Result<(), InstructionsError> {
    if rel.is_empty() || rel.starts_with('/') {
        return Err(InstructionsError::InvalidName(rel.to_string()));
    }
    for component in rel.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains('\\')
            || component.starts_with('.')
        {
            return Err(InstructionsError::InvalidName(rel.to_string()));
        }
    }
    Ok(())
}

/// Slice a body to a 1-based `[offset, offset+limit)` line window. Omitting both
/// returns the body verbatim. Offsets past the end clamp to an empty slice.
fn slice_lines(body: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return body.to_string();
    }
    let lines: Vec<&str> = body.lines().collect();
    let start = offset
        .map(|o| o.saturating_sub(1))
        .unwrap_or(0)
        .min(lines.len());
    let end = match limit {
        Some(l) => start.saturating_add(l).min(lines.len()),
        None => lines.len(),
    };
    lines[start..end].join("\n")
}

/// Recurse `dir`, pushing each regular file's path relative to `root`. Symlinks
/// are skipped (neither followed nor listed), keeping the listing confined.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(root, &path, out);
        } else if file_type.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                if let Some(s) = rel.to_str() {
                    out.push(s.to_string());
                }
            }
        }
    }
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
        write_md(tmp.path(), "ws1/AGENTS.md", "# Agent\n\nHello.");
        let instructions = Instructions::new(tmp.path());
        let content = instructions.read_primary_agent("ws1").unwrap();
        assert!(content.contains("Hello."));
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

    // ---- Generic path-based read verb ----
    //
    // The instruction content is an arbitrary directory tree, not two flat
    // lists. The generic `read(workspace, rel_path, offset, limit)` verb reads
    // any file under the workspace root through the in-process guards. These
    // tests pin its behavior.

    // A nested relative path must resolve and return the file body. Today the
    // only readers funnel through `validate_basename`, which rejects any `/`,
    // so a nested instruction file is unreachable. Materiality: a reader that
    // kept the single-component basename rule reds this.
    #[test]
    fn read_returns_nested_path_body() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/agents/team/scribe.md", "nested body");
        let instructions = Instructions::new(tmp.path());
        assert_eq!(
            instructions
                .read("ws1", "agents/team/scribe.md", None, None)
                .unwrap(),
            "nested body"
        );
    }

    // offset and limit are 1-based line numbers: `offset=2, limit=2` returns
    // lines 2 and 3 only. Materiality: 0-based slicing, byte offsets, or
    // ignoring limit each red this.
    #[test]
    fn read_offset_and_limit_are_one_based_line_numbers() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/AGENTS.md", "l1\nl2\nl3\nl4\nl5\n");
        let instructions = Instructions::new(tmp.path());
        let slice = instructions
            .read("ws1", "AGENTS.md", Some(2), Some(2))
            .unwrap();
        assert!(slice.contains("l2"), "line 2 is in the window: {slice:?}");
        assert!(slice.contains("l3"), "line 3 is in the window: {slice:?}");
        assert!(
            !slice.contains("l1"),
            "line 1 precedes the window: {slice:?}"
        );
        assert!(
            !slice.contains("l4"),
            "line 4 follows the window: {slice:?}"
        );
        assert!(
            !slice.contains("l5"),
            "line 5 follows the window: {slice:?}"
        );
    }

    // A `..` component must be rejected before any read. Materiality: dropping
    // the traversal guard resolves the parent path and returns Ok/NotFound
    // instead of an escape rejection.
    #[test]
    fn read_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("ws1")).unwrap();
        std::fs::write(tmp.path().join("outside.md"), "leaked").unwrap();
        let instructions = Instructions::new(tmp.path());
        let err = instructions
            .read("ws1", "../outside.md", None, None)
            .unwrap_err();
        assert!(
            matches!(
                err,
                InstructionsError::PathEscape | InstructionsError::InvalidName(_)
            ),
            "a traversal path must be rejected as an escape, got {err:?}"
        );
    }

    // A symlink whose target escapes the workspace root must be rejected by the
    // canonicalize + `starts_with(root)` guard. Materiality: dropping the
    // symlink guard leaks the target file's body.
    #[test]
    fn read_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.md");
        std::fs::write(&outside, "leaked").unwrap();
        let ws = tmp.path().join("ws1/agents");
        std::fs::create_dir_all(&ws).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, ws.join("evil.md")).unwrap();
        let instructions = Instructions::new(tmp.path());
        let err = instructions
            .read("ws1", "agents/evil.md", None, None)
            .unwrap_err();
        assert!(
            matches!(err, InstructionsError::PathEscape),
            "a symlink escaping the root must be rejected, got {err:?}"
        );
    }

    // ---- Recursive flat path map ----
    //
    // Navigation presentation is flat literal paths, never a nested tree.
    // `list_tree` recurses the confined root and returns sorted, capped, flat
    // relative paths.

    // A nested fixture enumerates to flat, sorted relative paths that carry
    // their `/` separators. Materiality: a non-recursive walk omits the nested
    // files; a nested/tree presentation would break the flat-path assertion.
    #[test]
    fn list_tree_returns_flat_sorted_nested_paths() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/AGENTS.md", "root");
        write_md(tmp.path(), "ws1/agents/team/scribe.md", "s");
        write_md(tmp.path(), "ws1/skills/foo/SKILL.md", "k");
        let instructions = Instructions::new(tmp.path());
        let paths = instructions.list_tree("ws1", 100);
        assert_eq!(
            paths,
            vec![
                "AGENTS.md".to_string(),
                "agents/team/scribe.md".to_string(),
                "skills/foo/SKILL.md".to_string(),
            ],
            "flat, sorted, literal relative paths of the whole tree"
        );
    }

    // The cap bounds how many paths are returned. Materiality: ignoring the cap
    // returns all four and reds the length bound.
    #[test]
    fn list_tree_respects_cap() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ws1/a.md", "a");
        write_md(tmp.path(), "ws1/b.md", "b");
        write_md(tmp.path(), "ws1/c.md", "c");
        write_md(tmp.path(), "ws1/d.md", "d");
        let instructions = Instructions::new(tmp.path());
        assert!(
            instructions.list_tree("ws1", 2).len() <= 2,
            "list_tree must not return more than the cap"
        );
    }
}
