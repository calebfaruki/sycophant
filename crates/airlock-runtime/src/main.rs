use std::env;

use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{AwaitToolCancelRequest, GetToolCallRequest, SendToolResultRequest};
use airlock_runtime::{execute, parts};
use serde::Deserialize;
use shared::scrub;
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
        // run_dispatch can kill the child. Aborted once execution returns.
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

        let (content, is_error, exit_code) =
            match execute::run_dispatch(&tool_name, &assignment.args, working_dir, &cancel).await {
                Ok(r) => {
                    // Split image-marker lines from plain text, read each image
                    // into an image part, and scrub the text part only. Both
                    // the chamber-dispatch path and the in-process builtin path
                    // funnel through this one `CommandResult`, so the marker
                    // convention serves both.
                    let assembled = parts::assemble_tool_answer(&r.stdout, &r.stderr, &scrub_set);
                    let is_error = r.exit_code != 0 || assembled.image_error;
                    (assembled.content, is_error, r.exit_code)
                }
                Err(e) => (
                    vec![proto_common::text_block(
                        scrub_set.apply(&format!("execution error: {e}")),
                    )],
                    true,
                    -1,
                ),
            };

        cancel_watcher.abort();

        client
            .send_tool_result(SendToolResultRequest {
                call_id,
                content,
                is_error,
                exit_code,
            })
            .await?;

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
