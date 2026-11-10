//! Path resolution + read for the per-agent instructions directory.
//!
//! Every read is rooted at `<instructions_root>/<agent>/` and refuses to
//! escape that root via `..`, absolute paths, or symlinks. Callers reach
//! content through generic path-based access (read, list) rather than
//! constructing raw filesystem paths.

use std::path::{Path, PathBuf};

/// Convention-driven resolver. Agent identity comes from the runtime
/// config (`config.agent`), fixed at pod start; the resolver never
/// derives it from tool arguments.
pub(crate) struct Instructions {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InstructionsError {
    #[error("not found")]
    NotFound,
    #[error("invalid name: {0}")]
    InvalidName(String),
    #[error("path escapes agent root")]
    PathEscape,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Instructions {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn agent_root(&self, agent: &str) -> Result<PathBuf, InstructionsError> {
        validate_basename(agent)?;
        Ok(self.root.join(agent))
    }

    /// Read an arbitrary nested relative path under the agent root through
    /// the in-process guards (root confinement, no `..` traversal, symlink-escape
    /// rejection). Any extension is readable; the body must be UTF-8 text.
    pub(crate) fn read(&self, agent: &str, rel_path: &str) -> Result<String, InstructionsError> {
        validate_relpath(rel_path)?;
        let agent_dir = self.agent_root(agent)?;
        let full = agent_dir.join(rel_path);
        let canonical = full.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => InstructionsError::NotFound,
            _ => InstructionsError::Io(e),
        })?;
        let canonical_root = agent_dir.canonicalize().map_err(InstructionsError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(InstructionsError::PathEscape);
        }
        std::fs::read_to_string(&canonical).map_err(InstructionsError::Io)
    }

    /// Flat, sorted, capped list of the relative paths of every file under the
    /// agent root, recursing arbitrary depth. Presentation is flat literal
    /// paths, never a nested tree. Dotfiles and dot-named directories are left
    /// out, since `read` refuses them. A missing or unreadable root yields an
    /// empty list rather than an error.
    pub(crate) fn list_tree(&self, agent: &str, cap: usize) -> Vec<String> {
        let agent_dir = match self.agent_root(agent) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let canonical_root = match agent_dir.canonicalize() {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        collect_files(&canonical_root, &canonical_root, &mut out);
        out.sort();
        out.truncate(cap);
        out
    }
}

/// Strip a leading YAML frontmatter block from an instructions file and
/// return the body. The frontmatter is never parsed; it controls nothing.
///
/// Lines split on `\n`. One leading UTF-8 BOM is dropped from line 1, and one
/// trailing `\r` is removed from each line before it is compared. The file
/// has frontmatter iff line 1 is exactly `---`; the closer is the first later
/// line that is exactly `---` (the last line may close with or without a
/// trailing newline). The body is every byte after the closer line's `\n`,
/// empty if the closer ends the file. With no closer the input comes back
/// unchanged. There is no size limit.
pub(crate) fn strip_frontmatter(input: &str) -> String {
    let after_bom = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let mut lines = after_bom.split_inclusive('\n');
    let is_delimiter = |line: &str| {
        let line = line.strip_suffix('\n').unwrap_or(line);
        line.strip_suffix('\r').unwrap_or(line) == "---"
    };
    match lines.next() {
        Some(first) if is_delimiter(first) => {}
        _ => return input.to_string(),
    }
    if lines.by_ref().any(is_delimiter) {
        return lines.collect();
    }
    input.to_string()
}

/// Names accepted as components: non-empty, no path separators, no `..`,
/// no leading dot — anything that resolves to a directory or file
/// basename within the instructions root.
fn validate_basename(name: &str) -> Result<(), InstructionsError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err(InstructionsError::InvalidName(name.to_string()));
    }
    Ok(())
}

/// Validate an arbitrary nested relative path for the generic reader. Splits on
/// `/` and rejects any component that is empty, `.`, `..`, backslash-bearing, or
/// leading-dot; rejects absolute paths. `/` between components is allowed. The
/// canonicalize + `starts_with(root)` guard in `read` still catches symlink
/// and traversal escape; this pre-check rejects the obvious cases early.
fn validate_relpath(rel: &str) -> Result<(), InstructionsError> {
    if rel.is_empty() || rel.starts_with('/') {
        return Err(InstructionsError::InvalidName(rel.to_string()));
    }
    for component in rel.split('/') {
        if component.is_empty() || component.contains('\\') || component.starts_with('.') {
            return Err(InstructionsError::InvalidName(rel.to_string()));
        }
    }
    Ok(())
}

/// Slice a body to a 1-based `[offset, offset+limit)` line window. Omitting both
/// returns the body verbatim. Offsets past the end clamp to an empty slice.
pub(crate) fn slice_lines(body: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return body.to_string();
    }
    page(body.lines().collect(), offset, limit).join("\n")
}

/// The 1-based `offset`/`limit` window of `items`: no offset starts at the
/// first (0 counts as 1), no limit takes the rest.
pub(crate) fn page<T>(items: Vec<T>, offset: Option<usize>, limit: Option<usize>) -> Vec<T> {
    let skip = offset.unwrap_or(1).saturating_sub(1);
    let take = limit.unwrap_or(usize::MAX);
    items.into_iter().skip(skip).take(take).collect()
}

/// Recurse `dir`, pushing each regular file's path relative to `root`. Symlinks
/// are skipped (neither followed nor listed), keeping the listing confined.
/// Leading-dot entries are skipped too, the rule `validate_relpath` applies.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
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
    use crate::test_doubles::write_md;

    #[test]
    fn validate_basename_rejects_path_separators() {
        assert!(validate_basename("foo/bar").is_err());
        assert!(validate_basename("foo\\bar").is_err());
    }

    #[test]
    fn read_returns_nested_path_body() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ag1/agents/team/scribe.md", "nested body");
        let instructions = Instructions::new(tmp.path());
        assert_eq!(
            instructions.read("ag1", "agents/team/scribe.md").unwrap(),
            "nested body"
        );
    }

    // offset and limit are 1-based line numbers: `offset=2, limit=2` returns
    // lines 2 and 3 only.
    #[test]
    fn slice_lines_offset_and_limit_are_one_based_line_numbers() {
        let slice = slice_lines("l1\nl2\nl3\nl4\nl5\n", Some(2), Some(2));
        assert_eq!(slice, "l2\nl3");
    }

    // A `..` component must be rejected before any read.
    #[test]
    fn read_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("ag1")).unwrap();
        std::fs::write(tmp.path().join("outside.md"), "leaked").unwrap();
        let instructions = Instructions::new(tmp.path());
        let err = instructions.read("ag1", "../outside.md").unwrap_err();
        assert!(
            matches!(
                err,
                InstructionsError::PathEscape | InstructionsError::InvalidName(_)
            ),
            "a traversal path must be rejected as an escape, got {err:?}"
        );
    }

    // A symlink whose target escapes the agent root must be rejected by the
    // canonicalize + `starts_with(root)` guard, or the target file's body leaks.
    #[test]
    fn read_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.md");
        std::fs::write(&outside, "leaked").unwrap();
        let agents_dir = tmp.path().join("ag1/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, agents_dir.join("evil.md")).unwrap();
        let instructions = Instructions::new(tmp.path());
        let err = instructions.read("ag1", "agents/evil.md").unwrap_err();
        assert!(
            matches!(err, InstructionsError::PathEscape),
            "a symlink escaping the root must be rejected, got {err:?}"
        );
    }

    // A nested fixture enumerates to flat, sorted relative paths that carry
    // their `/` separators: the walk is recursive and the result is flat.
    #[test]
    fn list_tree_returns_flat_sorted_nested_paths() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ag1/AGENTS.md", "root");
        write_md(tmp.path(), "ag1/agents/team/scribe.md", "s");
        write_md(tmp.path(), "ag1/skills/foo/SKILL.md", "k");
        let instructions = Instructions::new(tmp.path());
        let paths = instructions.list_tree("ag1", 100);
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

    // Every listed path is one `read` accepts: a dotfile, and every file under
    // a dot-named directory, is left out of the list.
    #[test]
    fn list_tree_leaves_out_paths_read_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ag1/AGENTS.md", "root");
        write_md(tmp.path(), "ag1/.notes/a.md", "hidden dir");
        write_md(tmp.path(), "ag1/dir/.env", "hidden file");
        write_md(tmp.path(), "ag1/dir/ok.md", "ok");
        let instructions = Instructions::new(tmp.path());
        let paths = instructions.list_tree("ag1", 100);
        assert_eq!(
            paths,
            vec!["AGENTS.md".to_string(), "dir/ok.md".to_string()],
            "dotfiles and dot-named directories are not listed"
        );
        for path in &paths {
            assert!(
                instructions.read("ag1", path).is_ok(),
                "every listed path reads: {path}"
            );
        }
    }

    // The cap bounds how many paths are returned.
    #[test]
    fn list_tree_respects_cap() {
        let tmp = tempfile::tempdir().unwrap();
        write_md(tmp.path(), "ag1/a.md", "a");
        write_md(tmp.path(), "ag1/b.md", "b");
        write_md(tmp.path(), "ag1/c.md", "c");
        write_md(tmp.path(), "ag1/d.md", "d");
        let instructions = Instructions::new(tmp.path());
        assert_eq!(
            instructions.list_tree("ag1", 2).len(),
            2,
            "list_tree returns exactly the cap when more files exist"
        );
    }

    /// Frontmatter has no size limit: a 64 KiB block is stripped whole, so the
    /// harness never meets frontmatter the uploader admitted but it cannot
    /// strip.
    #[test]
    fn strip_frontmatter_strips_a_64_kib_frontmatter() {
        let mut input = String::from("---\ndescription: big\n");
        while input.len() < 64 * 1024 {
            input.push_str("# pad pad pad pad pad pad pad pad\n");
        }
        input.push_str("---\nbody line\n");
        assert_eq!(strip_frontmatter(&input), "body line\n");
    }

    /// No closer means no frontmatter: the input comes back unchanged.
    #[test]
    fn strip_frontmatter_without_a_closer_returns_the_input_unchanged() {
        let input = "---\ntitle: x\nbody";
        assert_eq!(strip_frontmatter(input), input);
    }

    /// Line 1 must be exactly `---`; a trailing space, a fourth dash or any
    /// other suffix is not an opener.
    #[test]
    fn strip_frontmatter_with_a_bad_opener_returns_the_input_unchanged() {
        for input in [
            "--- \nk: v\n---\nbody",
            "----\nk: v\n---\nbody",
            "---x\nk: v\n---\nbody",
        ] {
            assert_eq!(strip_frontmatter(input), input, "{input:?}");
        }
    }

    /// One leading UTF-8 BOM is dropped from line 1, so `BOM---` opens, and
    /// the body carries no BOM.
    #[test]
    fn strip_frontmatter_accepts_a_bom_before_the_opener_and_drops_it() {
        let body = strip_frontmatter("\u{FEFF}---\nk: v\n---\nbody");
        assert_eq!(body, "body");
        assert!(!body.starts_with('\u{FEFF}'));
    }

    /// One trailing `\r` is removed before each compare, so a CRLF file's
    /// opener and closer count.
    #[test]
    fn strip_frontmatter_strips_a_crlf_frontmatter() {
        assert_eq!(strip_frontmatter("---\r\nk: v\r\n---\r\nbody"), "body");
    }

    /// A closer on the last line with no trailing newline ends the file, so
    /// the body is empty.
    #[test]
    fn strip_frontmatter_with_a_closer_ending_the_file_returns_an_empty_body() {
        assert_eq!(strip_frontmatter("---\nk: v\n---"), "");
    }

    #[test]
    fn page_is_one_based_offset_and_limit() {
        let entries = vec!["a", "b", "c"];
        assert_eq!(page(entries.clone(), None, None), ["a", "b", "c"]);
        assert_eq!(page(entries.clone(), Some(2), None), ["b", "c"]);
        assert_eq!(page(entries.clone(), None, Some(1)), ["a"]);
        assert_eq!(
            page(entries.clone(), Some(0), None),
            page(entries, Some(1), None)
        );
    }
}
