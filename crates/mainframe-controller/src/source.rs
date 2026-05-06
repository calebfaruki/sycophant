use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use futures::StreamExt;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore};

/// Map of relative path (without prefix) → opaque ETag string. Used by
/// pull_from_store to skip GETs when an object's ETag matches the stored value
/// AND the local file still exists. ETags are treated as opaque equality
/// markers — we don't interpret them as content hashes (S3 multipart uploads
/// produce non-MD5 ETags; Versitygw and other backends vary). Only "did the
/// ETag change since last sync" matters.
type EtagMap = HashMap<String, String>;

pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

pub struct SyncReport {
    pub object_count: u32,
    pub revision: String,
}

pub async fn pull_to(
    endpoint: &str,
    bucket: &str,
    prefix: &str,
    region: &str,
    creds: &S3Credentials,
    dest: &Path,
    etag_state: &Path,
) -> Result<SyncReport, String> {
    let store = build_store(endpoint, bucket, region, creds)?;
    pull_from_store(&store, prefix, dest, etag_state).await
}

async fn pull_from_store(
    store: &dyn ObjectStore,
    prefix: &str,
    dest: &Path,
    etag_state: &Path,
) -> Result<SyncReport, String> {
    let prev_etags = load_etags(etag_state);
    let mut new_etags: EtagMap = HashMap::new();

    let prefix_path = if prefix.is_empty() {
        None
    } else {
        Some(ObjectPath::from(prefix))
    };

    let mut stream = store.list(prefix_path.as_ref());
    let mut count = 0u32;
    let mut entries: Vec<(String, u64, Option<String>)> = Vec::new();

    while let Some(item) = stream.next().await {
        let item = item.map_err(|e| format!("s3 list error: {e}"))?;

        let key = item.location.as_ref();
        let relative = key.strip_prefix(prefix).unwrap_or(key);
        let relative = relative.trim_start_matches('/').to_string();
        let dest_path: PathBuf = dest.join(&relative);
        let current_etag = item.e_tag.clone();

        let skip = should_skip_get(
            current_etag.as_deref(),
            prev_etags.get(&relative).map(|s| s.as_str()),
            dest_path.exists(),
        );

        if !skip {
            let bytes = store
                .get_opts(&item.location, GetOptions::default())
                .await
                .map_err(|e| format!("s3 get {} failed: {e}", item.location))?
                .bytes()
                .await
                .map_err(|e| format!("s3 read {} failed: {e}", item.location))?;

            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("mkdir {} failed: {e}", parent.display()))?;
            }

            atomic_write(&dest_path, &bytes).await?;
        }

        if let Some(etag) = &current_etag {
            new_etags.insert(relative.clone(), etag.clone());
        }

        entries.push((key.to_string(), item.size, current_etag));
        count += 1;
    }

    // List+GET pass succeeded — apply --delete-after semantics. Any orphan on
    // disk (a file present locally but not in the listing) is removed. If the
    // list call or any GET above had errored, we'd have returned early before
    // reaching this point, leaving local state at the last successful sync.
    let synced: HashSet<String> = entries
        .iter()
        .map(|(key, _, _)| {
            let rel = key.strip_prefix(prefix).unwrap_or(key);
            rel.trim_start_matches('/').to_string()
        })
        .collect();
    delete_orphans(dest, &synced).await?;
    prune_empty_dirs_below(dest).await?;

    // Persist the post-sync ETag map. If this fails, the next tick re-fetches
    // everything (no stored ETags to compare against) — bandwidth penalty,
    // self-healing.
    save_etags(etag_state, &new_etags).await?;

    Ok(SyncReport {
        object_count: count,
        revision: revision_for(&entries),
    })
}

/// Decide whether to skip GET on a list entry. Skip only when:
/// 1. The listing returned a non-empty ETag (some backends like Versitygw posix
///    return empty strings; treat those as "no useful info" and always fetch).
/// 2. We have a stored ETag for this path that equals the current one.
/// 3. The local file is still present (otherwise we have nothing to keep).
///
/// Inputs are borrowed strings so the caller can pass `Option<&str>` from
/// either the listing's `e_tag` or the persisted map without cloning.
fn should_skip_get(current: Option<&str>, stored: Option<&str>, exists: bool) -> bool {
    match current {
        Some(etag) if !etag.is_empty() => stored == Some(etag) && exists,
        _ => false,
    }
}

fn load_etags(path: &Path) -> EtagMap {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => EtagMap::new(),
    }
}

async fn save_etags(path: &Path, etags: &EtagMap) -> Result<(), String> {
    let json =
        serde_json::to_string(etags).map_err(|e| format!("serialize etags failed: {e}"))?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir {} failed: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, json)
        .await
        .map_err(|e| format!("write etag tmp {} failed: {e}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("rename etag {} -> {} failed: {e}", tmp.display(), path.display()))?;
    Ok(())
}

async fn delete_orphans(dest: &Path, synced: &HashSet<String>) -> Result<(), String> {
    for path in collect_local_files(dest).await? {
        let rel = path
            .strip_prefix(dest)
            .map_err(|e| format!("strip_prefix {} failed: {e}", path.display()))?;
        let rel_str = rel.to_string_lossy();
        if !synced.contains(rel_str.as_ref()) {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| format!("rm orphan {} failed: {e}", path.display()))?;
        }
    }
    Ok(())
}

async fn collect_local_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("readdir {} failed: {e}", dir.display())),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("readdir entry: {e}"))?
        {
            let path = entry.path();
            let ft = entry
                .file_type()
                .await
                .map_err(|e| format!("filetype {}: {e}", path.display()))?;
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                result.push(path);
            }
        }
    }
    Ok(result)
}

async fn prune_empty_dirs_below(root: &Path) -> Result<(), String> {
    // Collect every subdirectory under root, then attempt rmdir deepest-first.
    // POSIX rmdir on a non-empty directory fails with ENOTEMPTY, which we treat
    // as "skip" — only fully-empty dirs get removed. Root itself is left alone.
    let mut all_dirs: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("readdir {} failed: {e}", dir.display())),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| format!("readdir entry: {e}"))?
        {
            let path = entry.path();
            if entry
                .file_type()
                .await
                .map_err(|e| format!("filetype {}: {e}", path.display()))?
                .is_dir()
            {
                all_dirs.push(path.clone());
                stack.push(path);
            }
        }
    }
    all_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for dir in all_dirs {
        let _ = tokio::fs::remove_dir(&dir).await;
    }
    Ok(())
}

fn revision_for(entries: &[(String, u64, Option<String>)]) -> String {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = DefaultHasher::new();
    for (key, size, etag) in &sorted {
        key.hash(&mut hasher);
        size.hash(&mut hasher);
        etag.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn build_store(
    endpoint: &str,
    bucket: &str,
    region: &str,
    creds: &S3Credentials,
) -> Result<Box<dyn ObjectStore>, String> {
    let allow_http = endpoint.starts_with("http://");
    let store = AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_bucket_name(bucket)
        .with_region(region)
        .with_access_key_id(&creds.access_key_id)
        .with_secret_access_key(&creds.secret_access_key)
        .with_allow_http(allow_http)
        .build()
        .map_err(|e| format!("s3 client build failed: {e}"))?;
    Ok(Box::new(store))
}

async fn atomic_write(dest: &Path, contents: &[u8]) -> Result<(), String> {
    let tmp = dest.with_extension("tmp");
    tokio::fs::write(&tmp, contents)
        .await
        .map_err(|e| format!("write {} failed: {e}", tmp.display()))?;
    tokio::fs::rename(&tmp, dest)
        .await
        .map_err(|e| format!("rename {} -> {} failed: {e}", tmp.display(), dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::memory::InMemory;
    use object_store::{PutOptions, PutPayload};

    async fn fake_store_with(items: &[(&str, &[u8])]) -> InMemory {
        let store = InMemory::new();
        for (key, body) in items {
            store
                .put_opts(
                    &ObjectPath::from(*key),
                    PutPayload::from(bytes::Bytes::copy_from_slice(body)),
                    PutOptions::default(),
                )
                .await
                .unwrap();
        }
        store
    }

    #[tokio::test]
    async fn round_trip_files() {
        let store = fake_store_with(&[
            ("AGENTS.md", b"hello world\n"),
            ("agents/alice.md", b"# Alice\n\nyou are warm.\n"),
            ("agents/bob.md", b"# Bob\n\nyou are dry.\n"),
        ])
        .await;

        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");
        let report = pull_from_store(&store, "", dir.path(), &etags)
            .await
            .unwrap();

        assert_eq!(report.object_count, 3);
        assert!(!report.revision.is_empty());

        let entry = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(entry, "hello world\n");
        let alice = std::fs::read_to_string(dir.path().join("agents/alice.md")).unwrap();
        assert!(alice.contains("warm"));
    }

    #[tokio::test]
    async fn revision_changes_with_content() {
        let store_a = fake_store_with(&[("file.md", b"a")]).await;
        let store_b =
            fake_store_with(&[("file.md", b"different content with different size")]).await;

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let etags_a = dir_a.path().join(".etags.json");
        let etags_b = dir_b.path().join(".etags.json");
        let report_a = pull_from_store(&store_a, "", dir_a.path(), &etags_a)
            .await
            .unwrap();
        let report_b = pull_from_store(&store_b, "", dir_b.path(), &etags_b)
            .await
            .unwrap();

        assert_ne!(
            report_a.revision, report_b.revision,
            "different sizes should produce different revisions"
        );
    }

    #[tokio::test]
    async fn empty_bucket_returns_zero_objects() {
        let store = fake_store_with(&[]).await;
        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");
        let report = pull_from_store(&store, "", dir.path(), &etags)
            .await
            .unwrap();
        assert_eq!(report.object_count, 0);
    }

    #[tokio::test]
    async fn prefix_filters_listing() {
        let store = fake_store_with(&[
            ("inside/a.md", b"a"),
            ("inside/b.md", b"b"),
            ("outside/c.md", b"c"),
        ])
        .await;

        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");
        let report = pull_from_store(&store, "inside", dir.path(), &etags)
            .await
            .unwrap();

        assert_eq!(report.object_count, 2);
        assert!(dir.path().join("a.md").exists());
        assert!(dir.path().join("b.md").exists());
        assert!(!dir.path().join("c.md").exists());
    }

    #[tokio::test]
    async fn sibling_subdirs_do_not_overlap() {
        let store_a = fake_store_with(&[("AGENTS.md", b"A")]).await;
        let store_b = fake_store_with(&[("AGENTS.md", b"B")]).await;
        let root = tempfile::tempdir().unwrap();
        let dir_a = root.path().join("workspace-a");
        let dir_b = root.path().join("workspace-b");
        let etags_a = root.path().join(".etags-a.json");
        let etags_b = root.path().join(".etags-b.json");

        pull_from_store(&store_a, "", &dir_a, &etags_a).await.unwrap();
        pull_from_store(&store_b, "", &dir_b, &etags_b).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(dir_a.join("AGENTS.md")).unwrap(),
            "A"
        );
        assert_eq!(
            std::fs::read_to_string(dir_b.join("AGENTS.md")).unwrap(),
            "B"
        );
    }

    #[tokio::test]
    async fn orphan_files_are_deleted_after_sync() {
        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");

        // First sync: A, B, C all present.
        let store_a = fake_store_with(&[
            ("a.md", b"A"),
            ("b.md", b"B"),
            ("c.md", b"C"),
        ])
        .await;
        pull_from_store(&store_a, "", dir.path(), &etags).await.unwrap();
        assert!(dir.path().join("a.md").exists());
        assert!(dir.path().join("b.md").exists());
        assert!(dir.path().join("c.md").exists());

        // Second sync against a store without B — B must disappear locally.
        let store_b = fake_store_with(&[("a.md", b"A"), ("c.md", b"C")]).await;
        pull_from_store(&store_b, "", dir.path(), &etags).await.unwrap();
        assert!(dir.path().join("a.md").exists());
        assert!(!dir.path().join("b.md").exists(), "orphan b.md should be removed");
        assert!(dir.path().join("c.md").exists());
    }

    #[tokio::test]
    async fn empty_subdirs_are_pruned_after_orphan_removal() {
        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");

        let store_a = fake_store_with(&[
            ("agents/alice.md", b"alice"),
            ("agents/bob.md", b"bob"),
        ])
        .await;
        pull_from_store(&store_a, "", dir.path(), &etags).await.unwrap();
        assert!(dir.path().join("agents").is_dir());

        // Second sync: agents/ is empty in source. Both files should be gone,
        // and the now-empty agents/ directory should be pruned.
        let store_b = fake_store_with(&[]).await;
        pull_from_store(&store_b, "", dir.path(), &etags).await.unwrap();
        assert!(!dir.path().join("agents/alice.md").exists());
        assert!(!dir.path().join("agents/bob.md").exists());
        assert!(!dir.path().join("agents").exists(), "empty agents/ should be pruned");
    }

    #[tokio::test]
    async fn dest_root_is_not_pruned_when_emptied() {
        // Root dir of the workspace's mainframe must survive even if all files
        // and subdirs are removed — try_reconcile recreates it each call but we
        // shouldn't churn it unnecessarily.
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let etags = dir.path().join(".etags.json");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let store_a = fake_store_with(&[("solo.md", b"x")]).await;
        pull_from_store(&store_a, "", &workspace, &etags).await.unwrap();

        let store_b = fake_store_with(&[]).await;
        pull_from_store(&store_b, "", &workspace, &etags).await.unwrap();

        assert!(workspace.is_dir(), "workspace root must persist");
    }

    #[test]
    fn should_skip_get_requires_nonempty_matching_etag_and_local_file() {
        // Standard skip case: matching non-empty ETags, local file present.
        assert!(should_skip_get(Some("abc"), Some("abc"), true));

        // Empty ETag never skips (Versitygw posix backend returns "" for files
        // placed via hostPath; treating empty as "match" would mask edits).
        assert!(!should_skip_get(Some(""), Some(""), true));

        // No current ETag: no info, always fetch.
        assert!(!should_skip_get(None, Some("abc"), true));

        // No stored ETag (first sync, or sidecar wiped): always fetch.
        assert!(!should_skip_get(Some("abc"), None, true));

        // Mismatched ETags: content changed, fetch.
        assert!(!should_skip_get(Some("abc"), Some("xyz"), true));

        // Local file missing despite matching ETags: re-fetch to repopulate.
        assert!(!should_skip_get(Some("abc"), Some("abc"), false));
    }

    #[tokio::test]
    async fn etag_match_skips_get_and_preserves_local() {
        // Sentinel approach: after first sync, locally overwrite the file with
        // a sentinel that does NOT match what the store would return on a GET.
        // If the second sync skips GET (ETag matches), the sentinel survives.
        // If it re-fetches, the sentinel is overwritten.
        let store = fake_store_with(&[("a.md", b"original")]).await;
        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");

        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "original"
        );

        std::fs::write(dir.path().join("a.md"), "sentinel").unwrap();

        // Second sync against the same store. ETag is unchanged → skip GET.
        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "sentinel",
            "second sync should skip GET on unchanged ETag, leaving the sentinel intact"
        );
    }

    #[tokio::test]
    async fn etag_change_triggers_refetch() {
        // When the source content changes, the listing returns a new ETag and
        // the controller re-fetches. Using one store with a mutating PUT so the
        // ETag actually changes across syncs (separate InMemory instances would
        // both return the same counter-derived ETag, masking the test).
        let store = InMemory::new();
        let key = ObjectPath::from("a.md");
        store
            .put_opts(
                &key,
                PutPayload::from(bytes::Bytes::copy_from_slice(b"v1")),
                PutOptions::default(),
            )
            .await
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");

        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "v1"
        );

        // Re-PUT the object with new content; ETag changes.
        store
            .put_opts(
                &key,
                PutPayload::from(bytes::Bytes::copy_from_slice(b"v2")),
                PutOptions::default(),
            )
            .await
            .unwrap();

        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "v2",
            "changed ETag must trigger re-fetch"
        );
    }

    #[tokio::test]
    async fn missing_local_file_triggers_refetch_even_with_matching_etag() {
        // Defense against a corrupt-state scenario: ETag is stored but local
        // file was manually removed (e.g., principal wiped /data/mainframe/foo
        // without clearing the .etags map). Next sync must re-fetch, not skip.
        let store = fake_store_with(&[("a.md", b"content")]).await;
        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");

        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();
        assert!(dir.path().join("a.md").exists());

        std::fs::remove_file(dir.path().join("a.md")).unwrap();

        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();
        assert!(
            dir.path().join("a.md").exists(),
            "missing local file should be re-fetched despite stored ETag"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.md")).unwrap(),
            "content"
        );
    }

    #[tokio::test]
    async fn etag_map_persists_across_syncs() {
        let store = fake_store_with(&[("a.md", b"x"), ("b.md", b"y")]).await;
        let dir = tempfile::tempdir().unwrap();
        let etags = dir.path().join(".etags.json");

        pull_from_store(&store, "", dir.path(), &etags).await.unwrap();

        // ETag map must exist on disk.
        assert!(etags.is_file(), ".etags.json should be written after sync");
        let map: EtagMap =
            serde_json::from_str(&std::fs::read_to_string(&etags).unwrap()).unwrap();
        assert!(map.contains_key("a.md"));
        assert!(map.contains_key("b.md"));
    }

    #[tokio::test]
    async fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        atomic_write(&path, b"first").await.unwrap();
        atomic_write(&path, b"second").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }

    #[test]
    fn revision_is_stable_for_same_listing() {
        let entries = vec![
            ("a".to_string(), 10, Some("etag1".to_string())),
            ("b".to_string(), 20, None),
        ];
        assert_eq!(revision_for(&entries), revision_for(&entries));
    }

    #[test]
    fn revision_ignores_listing_order() {
        let entries_a = vec![("a".to_string(), 10, None), ("b".to_string(), 20, None)];
        let entries_b = vec![("b".to_string(), 20, None), ("a".to_string(), 10, None)];
        assert_eq!(revision_for(&entries_a), revision_for(&entries_b));
    }
}
