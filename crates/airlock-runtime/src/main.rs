use std::collections::HashMap;
use std::env;

use airlock_proto::airlock_controller_client::AirlockControllerClient;
use airlock_proto::{GetToolCallRequest, SendToolResultRequest};
use airlock_runtime::{execute, scrub};
use serde::Deserialize;
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

    let mut client = {
        let mut connected = None;
        for attempt in 1..=10u64 {
            match AirlockControllerClient::connect(controller_addr.clone()).await {
                Ok(c) => {
                    connected = Some(c);
                    break;
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(attempt, error = %e, "controller not ready, retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
        connected.unwrap()
    };

    stage_credentials();

    let scrub_set = scrub::ScrubSet::from_env();

    loop {
        let assignment = client
            .get_tool_call(GetToolCallRequest {
                job_id: job_id.clone(),
                tool_name: tool_name.clone(),
            })
            .await?
            .into_inner();

        info!(call_id = %assignment.call_id, "received tool call assignment");

        let working_dir = if assignment.working_dir.is_empty() {
            "/workspace"
        } else {
            &assignment.working_dir
        };

        let params: HashMap<String, serde_json::Value> =
            serde_json::from_str(&assignment.input_json).unwrap_or_default();
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let (output, is_error, exit_code) =
            match execute::execute_command_execve(command, working_dir).await {
                Ok(r) => {
                    let combined = if r.stderr.is_empty() {
                        r.stdout
                    } else {
                        format!("{}{}", r.stdout, r.stderr)
                    };
                    (combined, r.exit_code != 0, r.exit_code)
                }
                Err(e) => (format!("execution error: {e}"), true, -1),
            };

        let output = scrub_set.apply(&output);

        client
            .send_tool_result(SendToolResultRequest {
                call_id: assignment.call_id,
                output,
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
    for entry in &entries {
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
