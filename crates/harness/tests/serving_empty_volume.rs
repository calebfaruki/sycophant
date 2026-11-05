//! If the mounted instructions volume is empty, in-process instructions serving fails
//! legibly with a named error rather than silently serving nothing.

use harness::instructions::{Instructions, InstructionsError};

#[test]
fn empty_volume_primary_agent_is_named_not_found() {
    // An empty (populated-but-no-AGENTS.md) agent root.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("ag1")).unwrap();
    let instructions = Instructions::new(tmp.path());

    let err = instructions
        .read_primary_agent("ag1")
        .expect_err("empty instructions volume must not silently serve empty content");
    assert!(
        matches!(err, InstructionsError::NotFound),
        "empty volume must surface a named NotFound error, got {err:?}"
    );
}
