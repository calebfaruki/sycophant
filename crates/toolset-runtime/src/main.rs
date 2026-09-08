use std::env;
use std::time::Duration;

use serde::Deserialize;
use tonic::transport::Server;
use toolset_proto::tool_job_server::ToolJobServer;
use toolset_runtime::ToolJobService;
use tracing::info;

/// The port every capability-job pod serves its `Job` service on. The
/// per-workspace headless Service and the harness egress policy target it.
const TOOL_JOB_PORT: u16 = 9090;

/// Idle window a keepalive pod stays warm with no dial before it exits. Matches
/// the harness's keepalive idle bound.
const KEEPALIVE_IDLE_SECONDS: u64 = 600;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let tool_name = env::var("TOOLSET_TOOL_NAME").expect("TOOLSET_TOOL_NAME must be set");
    let keepalive = env::var("TOOLSET_KEEPALIVE").unwrap_or_default() == "true";

    info!(%tool_name, keepalive, "starting toolset-runtime server");

    // Stage the resolved grant to its target before serving, so a credential
    // that never landed fails the pod rather than looking like a running start.
    stage_credentials()?;

    // The pod is a pure server: the harness dials it, sends the tool-call
    // assignment as the first message on the held stream, reads result frames
    // back, and pushes cancel on the same connection. Ingress is netpol-scoped
    // to the same-workspace harness, so the surface authenticates structurally.
    let service = ToolJobService::new(tool_name);
    let watcher = service.shutdown_watcher();
    let addr = ([0, 0, 0, 0], TOOL_JOB_PORT).into();
    info!(%addr, "serving ToolJob");

    Server::builder()
        .add_service(ToolJobServer::new(service))
        .serve_with_shutdown(
            addr,
            watcher.wait(keepalive, Duration::from_secs(KEEPALIVE_IDLE_SECONDS)),
        )
        .await?;

    Ok(())
}

#[derive(Deserialize)]
struct CredentialMapEntry {
    staging: String,
    target: String,
}

/// Copy each staged credential to its target and set mode `0o600`, before any
/// tool runs. A credential that never landed must not look like a successful
/// start, so an unparseable map or a filesystem refusal fails the job naming
/// the cause.
fn stage_credentials() -> anyhow::Result<()> {
    let json = match env::var("TOOLSET_CREDENTIAL_MAP") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(()),
    };
    let entries: Vec<CredentialMapEntry> = serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("failed to parse TOOLSET_CREDENTIAL_MAP: {e}"))?;
    for entry in &entries {
        if let Some(parent) = std::path::Path::new(&entry.target).parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                anyhow::anyhow!(
                    "credential target {}: cannot create parent directory: {e}",
                    entry.target
                )
            })?;
        }
        std::fs::copy(&entry.staging, &entry.target)
            .map_err(|e| anyhow::anyhow!("credential target {}: copy failed: {e}", entry.target))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&entry.target, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| {
                    anyhow::anyhow!(
                        "credential target {}: cannot restrict to 0600: {e}",
                        entry.target
                    )
                })?;
        }
        info!(target = %entry.target, "credential staged");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    #[test]
    #[serial]
    fn stage_credentials_copies_file_with_0600() {
        let tmp = tempfile::TempDir::new().unwrap();
        let staging = tmp.path().join("staging.key");
        let target = tmp.path().join("sub/dir/target.key");
        fs::write(&staging, "SECRET_KEY_DATA").unwrap();

        let map = serde_json::json!([{
            "staging": staging.to_str().unwrap(),
            "target": target.to_str().unwrap(),
        }]);
        env::set_var("TOOLSET_CREDENTIAL_MAP", map.to_string());
        stage_credentials().expect("staging must succeed");
        env::remove_var("TOOLSET_CREDENTIAL_MAP");

        assert!(target.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "SECRET_KEY_DATA");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    #[serial]
    fn stage_credentials_creates_parent_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let staging = tmp.path().join("key");
        let target = tmp.path().join("a/b/c/key");
        fs::write(&staging, "data").unwrap();

        let map = serde_json::json!([{
            "staging": staging.to_str().unwrap(),
            "target": target.to_str().unwrap(),
        }]);
        env::set_var("TOOLSET_CREDENTIAL_MAP", map.to_string());
        stage_credentials().expect("staging must succeed");
        env::remove_var("TOOLSET_CREDENTIAL_MAP");

        assert!(target.exists());
        assert!(target.parent().unwrap().is_dir());
    }

    #[test]
    #[serial]
    fn stage_credentials_no_env_is_noop() {
        env::remove_var("TOOLSET_CREDENTIAL_MAP");
        stage_credentials().expect("no credential map is a no-op");
    }

    /// A `tracing` writer that keeps every emitted line in memory.
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogs;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `stage_credentials` with `value` verbatim in the environment and
    /// return what it logged alongside its outcome.
    fn staged_raw(value: &str) -> (anyhow::Result<()>, String) {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish();
        env::set_var("TOOLSET_CREDENTIAL_MAP", value);
        let result = tracing::subscriber::with_default(subscriber, stage_credentials);
        env::remove_var("TOOLSET_CREDENTIAL_MAP");
        (result, logs.text())
    }

    /// Run `stage_credentials` with `map` in the environment and return what it
    /// logged alongside its outcome.
    fn staged(map: serde_json::Value) -> (anyhow::Result<()>, String) {
        staged_raw(&map.to_string())
    }

    /// The logs of a staging run that must succeed.
    fn staged_logs(map: serde_json::Value) -> String {
        let (result, logs) = staged(map);
        result.expect("staging must succeed");
        logs
    }

    /// An empty credential map means the same thing as an absent one: this job
    /// resolved no grant. It must stage nothing and say nothing — a parse
    /// complaint on every grantless tool job is the log noise that trains
    /// operators to ignore the warning that matters.
    ///
    /// Breaks if the emptiness guard is dropped and the empty string is handed
    /// to the JSON parser.
    #[test]
    #[serial]
    fn an_empty_credential_map_stages_nothing_and_says_nothing() {
        let (result, logs) = staged_raw("");

        result.expect("an empty credential map is a no-op, not a failure");
        assert!(
            logs.is_empty(),
            "a grantless job has no credential map to complain about, logged: {logs}"
        );
    }

    /// A malformed credential map means the controller and the runtime disagree
    /// about the wire shape. A job that starts anyway runs tools without the
    /// credential the call resolved, so the parse failure fails the job like
    /// every other staging failure.
    ///
    /// Breaks if the parse error is warned about and skipped rather than
    /// propagated.
    #[test]
    #[serial]
    fn a_malformed_credential_map_fails_staging_naming_the_cause() {
        let (result, _) = staged_raw("{not json");

        let err = result
            .expect_err("a credential map that cannot be parsed must fail the job, not be skipped")
            .to_string();
        assert!(
            err.contains("TOOLSET_CREDENTIAL_MAP"),
            "the error must name the variable the operator has to fix, got: {err}"
        );
    }

    /// A credential target outside `$HOME` is normal: the convention target sits
    /// on its own writable mount. The runtime attempts the copy and reports what
    /// the filesystem did, so a target that stages successfully says nothing. A
    /// spurious warning on every credentialed tool job trains operators to
    /// ignore the one that matters.
    ///
    /// Breaks if the target is prechecked against a path prefix rather than
    /// simply copied to.
    #[test]
    #[serial]
    fn writable_target_outside_home_stages_without_a_target_path_warning() {
        let home = tempfile::TempDir::new().unwrap();
        let mount = tempfile::TempDir::new().unwrap();
        let staging = mount.path().join("staged");
        let target = mount.path().join("credential");
        fs::write(&staging, "SECRET_KEY_DATA").unwrap();
        env::set_var("HOME", home.path());

        let logs = staged_logs(serde_json::json!([{
            "staging": staging.to_str().unwrap(),
            "target": target.to_str().unwrap(),
        }]));

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "SECRET_KEY_DATA",
            "the credential must reach its target"
        );
        assert!(
            !logs.contains("WARN"),
            "a writable target outside $HOME must stage silently, logged: {logs}"
        );
    }

    /// The keep arm: staging that silently does nothing would pass the test
    /// above. A credential that never landed must not look like a successful
    /// start, so the job fails and the error names the target and the cause.
    ///
    /// Breaks if a copy failure is warned about and skipped rather than
    /// propagated.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn an_unwritable_target_fails_staging_with_an_error_naming_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::TempDir::new().unwrap();
        let mount = tempfile::TempDir::new().unwrap();
        let staging = mount.path().join("staged");
        fs::write(&staging, "SECRET_KEY_DATA").unwrap();

        let locked = mount.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        let canary = locked.join("canary");
        if fs::write(&canary, b"x").is_ok() {
            let _ = fs::remove_file(&canary);
            eprintln!("skipping: running as root, no path is unwritable");
            return;
        }
        let target = locked.join("credential");
        env::set_var("HOME", home.path());

        let (result, _) = staged(serde_json::json!([{
            "staging": staging.to_str().unwrap(),
            "target": target.to_str().unwrap(),
        }]));

        let err = result
            .expect_err("a credential that cannot be written must fail the job, not be skipped")
            .to_string();
        assert!(
            err.contains(target.to_str().unwrap()),
            "the error must name the target the operator has to fix, got: {err}"
        );
        assert!(
            err.contains("Permission denied") || err.contains("denied"),
            "the error must carry the filesystem's own cause, got: {err}"
        );
    }
}
