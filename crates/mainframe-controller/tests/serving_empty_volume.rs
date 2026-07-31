//! If the mounted kernel volume is empty, kernel serving fails legibly with a
//! named error rather than silently serving nothing. This locks the pre-existing
//! behavior against a mutant that swallows the error; the serving path is
//! unchanged by the sync-Job removal.

use mainframe_controller::kernel::{Kernel, KernelError};

#[test]
fn empty_volume_primary_agent_is_named_not_found() {
    // An empty (populated-but-no-AGENTS.md) workspace root.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("ws1")).unwrap();
    let kernel = Kernel::new(tmp.path());

    let err = kernel
        .read_primary_agent("ws1")
        .expect_err("empty kernel volume must not silently serve empty content");
    assert!(
        matches!(err, KernelError::NotFound),
        "empty volume must surface a named NotFound error, got {err:?}"
    );
}
