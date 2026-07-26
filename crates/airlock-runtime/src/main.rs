use std::env;

use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{AwaitToolCancelRequest, GetToolCallRequest, ToolResultFrame};
use airlock_runtime::{execute, parts, stdlib};
use serde::Deserialize;
use shared::scrub;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();

    let controller_addr =
        env::var("AIRLOCK_CONTROLLER_ADDR").expect("AIRLOCK_CONTROLLER_ADDR must be set");
    let job_id = env::var("AIRLOCK_JOB_ID").expect("AIRLOCK_JOB_ID must be set");
    let tool_name = env::var("AIRLOCK_TOOL_NAME").expect("AIRLOCK_TOOL_NAME must be set");
    let keepalive = env::var("AIRLOCK_KEEPALIVE").unwrap_or_default() == "true";

    info!(%controller_addr, %job_id, %tool_name, keepalive, "starting airlock-runtime");

    let mut client = shared::retry_with_backoff(10, "airlock-controller-connect", |_| {
        let addr = controller_addr.clone();
        async move { AirlockControllerClient::connect(addr).await }
    })
    .await?;

    stage_credentials();

    let scrub_set = scrub::ScrubSet::from_env_var("AIRLOCK_SCRUB_SECRETS");

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
        // tool runs. The call_id rides the `x-airlock-call-id` request-metadata
        // header, so it is not repeated on every frame (mirrors hangar's
        // `x-hangar-model`). Dropping the producer's `tx` EOFs the request stream.
        let (tx, rx) = tokio::sync::mpsc::channel::<ToolResultFrame>(64);
        let mut request = tonic::Request::new(ReceiverStream::new(rx));
        request.metadata_mut().insert(
            "x-airlock-call-id",
            call_id.parse().map_err(|e| {
                anyhow::anyhow!("call_id {call_id} is not a valid metadata value: {e}")
            })?,
        );

        // The producer forwards frames as the tool produces them. A chamber tool
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

fn stage_credentials() {
    let json = match env::var("AIRLOCK_CREDENTIAL_MAP") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let entries: Vec<CredentialMapEntry> = match serde_json::from_str(&json) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("failed to parse AIRLOCK_CREDENTIAL_MAP: {e}");
            return;
        }
    };
    let home = env::var("HOME").unwrap_or_default();
    for entry in &entries {
        if !home.is_empty() && !entry.target.starts_with(&format!("{home}/")) {
            tracing::warn!(
                target = %entry.target, home = %home,
                "credential target is outside $HOME; chamber runs as non-root and the write may fail. Use a path under $HOME."
            );
        }
        if let Some(parent) = std::path::Path::new(&entry.target).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(target = %entry.target, "failed to create parent dir: {e}");
                continue;
            }
        }
        if let Err(e) = std::fs::copy(&entry.staging, &entry.target) {
            tracing::warn!(
                staging = %entry.staging, target = %entry.target,
                "credential staging failed: {e}"
            );
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&entry.target, std::fs::Permissions::from_mode(0o600));
        }
        info!(target = %entry.target, "credential staged");
    }
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
        env::set_var("AIRLOCK_CREDENTIAL_MAP", map.to_string());
        stage_credentials();
        env::remove_var("AIRLOCK_CREDENTIAL_MAP");

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
        env::set_var("AIRLOCK_CREDENTIAL_MAP", map.to_string());
        stage_credentials();
        env::remove_var("AIRLOCK_CREDENTIAL_MAP");

        assert!(target.exists());
        assert!(target.parent().unwrap().is_dir());
    }

    #[test]
    #[serial]
    fn stage_credentials_no_env_is_noop() {
        env::remove_var("AIRLOCK_CREDENTIAL_MAP");
        stage_credentials();
    }
}
