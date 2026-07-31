//! The controller contains no code that constructs or creates a Job. The S3 sync
//! Job builder (`job.rs`) is deleted and no reconcile path builds a Job. A mutant
//! re-introducing Job construction in any source file is caught here.

use std::path::{Path, PathBuf};

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn controller_source_constructs_no_job() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    assert!(
        !files.is_empty(),
        "expected controller source files under src/"
    );

    // Markers of Job construction/creation. `batch::v1` and `api::batch` cover the
    // k8s Job type imports; `create_s3_sync_job` covers the builder and its caller.
    let markers = ["create_s3_sync_job", "batch::v1", "api::batch"];
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for marker in markers {
            assert!(
                !text.contains(marker),
                "{} constructs or creates a Job (matched {marker:?}); AC4 requires no Job code",
                file.display()
            );
        }
    }
}
