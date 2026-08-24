use std::env;

use proto_common::ToolResultFrame;
use serde::Deserialize;
use shared::auth::SaTokenInterceptor;
use shared::scrub;
use tokio_stream::wrappers::ReceiverStream;
use toolset_proto::{AwaitToolCancelRequest, GetToolCallRequest};
use toolset_runtime::{execute, parts, stdlib};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let controller_addr =
        env::var("TOOLSET_CONTROLLER_ADDR").expect("TOOLSET_CONTROLLER_ADDR must be set");
    let job_id = env::var("TOOLSET_JOB_ID").expect("TOOLSET_JOB_ID must be set");
    let tool_name = env::var("TOOLSET_TOOL_NAME").expect("TOOLSET_TOOL_NAME must be set");
    let keepalive = env::var("TOOLSET_KEEPALIVE").unwrap_or_default() == "true";

    info!(%controller_addr, %job_id, %tool_name, keepalive, "starting toolset-runtime");

    // The client carries the pod's kubelet-projected `tool.toolset` SA token
    // as a Bearer header on every RPC. The controller verifies it via
    // TokenReview and binds the caller to sa-<workspace> — the tool job's
    // identity.
    let mut client = toolset_runtime::connect_authenticated(
        &controller_addr,
        SaTokenInterceptor::default_path(),
    )
    .await?;

    stage_credentials()?;

    let scrub_set = scrub::ScrubSet::from_env_var("TOOLSET_SCRUB_SECRETS");

    loop {
        let assignment = client
            .get_tool_call(GetToolCallRequest {
                job_id: job_id.clone(),
                tool_name: tool_name.clone(),
            })
            .await?
            .into_inner();

        let call_id = assignment.call_id.clone();
        info!(call_id = %call_id, "received tool call assignment");

        let working_dir = if assignment.working_dir.is_empty() {
            "/workspace"
        } else {
            &assignment.working_dir
        };

        // Open the cancel channel for this call: a watcher long-polls
        // AwaitToolCancel and fires the local token when a cancel arrives, so
        // the running child can be killed. Aborted once execution returns.
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_watcher = {
            let mut cancel_client = client.clone();
            let watch_token = cancel.clone();
            let watch_call_id = call_id.clone();
            tokio::spawn(async move {
                if cancel_client
                    .await_tool_cancel(AwaitToolCancelRequest {
                        call_id: watch_call_id,
                    })
                    .await
                    .is_ok()
                {
                    watch_token.cancel();
                }
            })
        };

        // Client-stream the call's typed output frames to the controller as the
        // tool runs. The call_id rides the `x-toolset-call-id` request-metadata
        // header, so it is not repeated on every frame (mirrors the prompt
        // job's `x-toolset-model`). Dropping the producer's `tx` EOFs the
        // request stream.
        let (tx, rx) = tokio::sync::mpsc::channel::<ToolResultFrame>(64);
        let mut request = tonic::Request::new(ReceiverStream::new(rx));
        request.metadata_mut().insert(
            "x-toolset-call-id",
            call_id.parse().map_err(|e| {
                anyhow::anyhow!("call_id {call_id} is not a valid metadata value: {e}")
            })?,
        );

        // The producer forwards frames as the tool produces them. A toolset tool
        // streams live line-by-line through `stream_frames`; an in-process
        // builtin completes to one `CommandResult` and is framed at once. Both
        // apply the marker convention and the per-frame scrub, and both feed the
        // same request stream the RPC drains concurrently.
        let producer = async {
            if stdlib::BUILTIN_NAMES.contains(&tool_name.as_str()) {
                let result = stdlib::dispatch_builtin(
                    &tool_name,
                    &assignment.args,
                    working_dir,
                    stdlib::DEFAULT_MAX_OUTPUT_CHARS,
                    &cancel,
                )
                .await;
                for frame in
                    parts::frames_for(&result.stdout, &result.stderr, result.exit_code, &scrub_set)
                {
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
            } else {
                let cmd =
                    execute::compose_dispatch_command(&tool_name, &assignment.args, working_dir);
                execute::stream_frames(cmd, &cancel, None, &scrub_set, tx).await;
            }
        };

        let (_, ack) = tokio::join!(producer, client.stream_tool_result(request));
        cancel_watcher.abort();
        ack?;

        if !keepalive {
            info!("fire-and-forget mode, exiting");
            break;
        }
    }

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
