mod config;
mod scrub_writer;

use std::env;
use std::sync::Arc;
use std::time::Duration;

use config::load_config;
use inference_runtime::InferenceJobService;
use scrub_writer::ScrubMakeWriter;
use serde::Deserialize;
use shared::scrub::ScrubSet;
use tonic::transport::Server;
use toolset_proto::inference_job_server::InferenceJobServer;
use tracing::info;

/// The port every capability-job pod serves its `Job` service on. The
/// per-workspace headless Service and the harness egress policy target it.
const TOOL_JOB_PORT: u16 = 9090;

/// Idle window a never-dialed pod waits before it exits, so a pod the harness
/// never reaches does not linger to its Job deadline.
const IDLE_TIMEOUT_SECONDS: u64 = 600;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load the scrub set FIRST so the tracing subscriber gets a writer that can
    // redact the provider credential even on the first log line — the pod holds
    // the credential, so a raw header logged under RUST_LOG=trace could leak it.
    let scrub_set = Arc::new(ScrubSet::from_env_var("TOOLSET_SCRUB_SECRETS"));
    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_writer(ScrubMakeWriter::new(scrub_set.clone()))
        .init();

    // Stage the provider credential to its target before serving, so a
    // credential that never landed fails the pod rather than looking like a
    // running start.
    stage_credentials()?;

    let (format, base_url, config) = load_config().map_err(|e| anyhow::anyhow!(e))?;
    info!(model = %config.model, "starting inference-runtime server");
    let provider: Arc<dyn model_provider::LlmProvider> = Arc::from(format.build(&base_url));

    // The pod is a pure server: the harness dials it, sends the model-call
    // assignment as the first message on the held stream, reads model events
    // back, and pushes cancel on the same connection. Ingress is netpol-scoped
    // to the same-workspace harness, so the surface authenticates structurally.
    let service = InferenceJobService::new(provider, config);
    let watcher = service.shutdown_watcher();
    let addr = ([0, 0, 0, 0], TOOL_JOB_PORT).into();
    info!(%addr, "serving InferenceJob");

    Server::builder()
        .add_service(InferenceJobServer::new(service))
        .serve_with_shutdown(
            addr,
            // A per-call inference pod is one-shot: it exits as soon as its
            // single call completes, and the idle window only bounds a pod the
            // harness never dials.
            watcher.wait(false, Duration::from_secs(IDLE_TIMEOUT_SECONDS)),
        )
        .await?;

    Ok(())
}

#[derive(Deserialize)]
struct CredentialMapEntry {
    staging: String,
    target: String,
}

/// Copy each staged credential to its target and set mode `0o600`, before the
/// model call runs. A credential that never landed must not look like a
/// successful start, so an unparseable map or a filesystem refusal fails the job
/// naming the cause.
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

    #[test]
    #[serial]
    fn an_empty_credential_map_stages_nothing() {
        env::set_var("TOOLSET_CREDENTIAL_MAP", "");
        stage_credentials().expect("an empty credential map is a no-op, not a failure");
        env::remove_var("TOOLSET_CREDENTIAL_MAP");
    }

    #[test]
    #[serial]
    fn a_malformed_credential_map_fails_naming_the_cause() {
        env::set_var("TOOLSET_CREDENTIAL_MAP", "{not json");
        let err = stage_credentials()
            .expect_err("a credential map that cannot be parsed must fail the job, not be skipped")
            .to_string();
        env::remove_var("TOOLSET_CREDENTIAL_MAP");
        assert!(
            err.contains("TOOLSET_CREDENTIAL_MAP"),
            "the error must name the variable the operator has to fix, got: {err}"
        );
    }
}
