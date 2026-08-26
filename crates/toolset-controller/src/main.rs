use std::sync::Arc;

use tonic::transport::Server;
use tracing::{error, info};

use toolset_controller::audience_layer::RequiredAudienceLayer;
use toolset_controller::grpc::{ControllerService, VerifierPair};
use toolset_controller::state::{ControllerState, PromptConfig, ToolsetConfig, WorkspaceBindings};
use toolset_controller::watcher::K8sDiscoverySpawner;
use toolset_controller::{keepalive, registry, watcher};
use toolset_proto::toolset_controller_client::ToolsetControllerClient;
use toolset_proto::toolset_controller_server::ToolsetControllerServer;
use toolset_proto::{DiscoveredArgMsg, DiscoveredToolMsg, ReportDiscoveredToolsRequest};

/// Single gRPC listener (9090): K8s ServiceAccount tokens via TokenReview.
/// Reachable from in-cluster pods only via NetworkPolicy. The internet-facing
/// gateway lives in relay-controller; this controller serves in-cluster
/// callers (the harness and the spawned tool jobs).
const GRPC_PORT: u16 = 9090;

/// Controller config, read from the chart-set environment (see
/// charts/sycophant-tenant/templates/toolset-ctrl.yaml).
struct Config {
    namespace: String,
    controller_addr: String,
    toolset_config_file: String,
    prompt_config_file: String,
    bindings_file: Option<String>,
    scheduling_file: String,
}

impl Config {
    fn from_env() -> Self {
        Self {
            namespace: std::env::var("TOOLSET_NAMESPACE").unwrap_or_else(|_| "default".into()),
            controller_addr: std::env::var("TOOLSET_CONTROLLER_ADDR")
                .unwrap_or_else(|_| format!("http://0.0.0.0:{GRPC_PORT}")),
            toolset_config_file: std::env::var("TOOLSET_CONFIG_FILE")
                .unwrap_or_else(|_| "/etc/sycophant/toolset-config/toolsets.yaml".into()),
            prompt_config_file: std::env::var("PROMPT_CONFIG_FILE")
                .unwrap_or_else(|_| "/etc/sycophant/toolset-config/prompt.yaml".into()),
            bindings_file: std::env::var("TOOLSET_BINDINGS_FILE").ok(),
            scheduling_file: std::env::var("TOOLSET_SCHEDULING_FILE")
                .unwrap_or_else(|_| "/etc/sycophant/scheduling.yaml".into()),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().json().with_target(false).init();
    // Pin the rustls 0.23 CryptoProvider; it refuses to auto-pick with multiple
    // compiled in.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // Discovery subcommand: the ephemeral discovery Job runs this same image as
    // `toolset-controller discover`. It reads a toolset image's tool label off
    // the registry and reports the tool set back over ReportDiscoveredTools,
    // then exits. The long-lived controller never reaches the registry.
    if std::env::args().nth(1).as_deref() == Some("discover") {
        return run_discover().await;
    }

    let config = Config::from_env();

    let kube_client = shared::try_init_kube_client()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    let controller_addr = config.controller_addr.clone();

    let scheduling =
        shared::scheduling::SchedulingConfig::load_or_default(&config.scheduling_file, true)
            .map_err(|e| anyhow::anyhow!(e))?;

    // Spawner for ephemeral discovery Jobs. The controller holds no registry
    // egress; each Job carries the reach and reports its tools back.
    let spawner: Arc<dyn watcher::DiscoverySpawner> = Arc::new(K8sDiscoverySpawner {
        client: kube_client.clone(),
        namespace: config.namespace.clone(),
        controller_addr: controller_addr.clone(),
        scheduling: scheduling.clone(),
    });

    let state = ControllerState::new(
        Some(kube_client.clone()),
        config.namespace.clone(),
        controller_addr,
        scheduling,
    );

    // Two TokenReview verifiers, one per audience. The audience layer stamps a
    // RequiredAudience on each request; the handler picks the matching one.
    let verifiers = VerifierPair {
        harness: Arc::new(shared::auth::K8sTokenVerifier::new(
            kube_client.clone(),
            shared::auth::HARNESS_TOOLSET_AUDIENCE,
        )),
        tool_job: Arc::new(shared::auth::K8sTokenVerifier::new(
            kube_client.clone(),
            shared::auth::TOOL_TOOLSET_AUDIENCE,
        )),
    };

    // A configured-but-malformed file is fatal. Empty bindings bind no
    // workspace to any toolset, so no discovery Job ever spawns and the tool
    // registry stays empty — an agent that converses and never acts, with one
    // error line to explain it.
    let bindings = match &config.bindings_file {
        Some(path) if std::path::Path::new(path).exists() => {
            let b = WorkspaceBindings::load(path).map_err(|e| {
                anyhow::anyhow!("failed to load workspace bindings from {path}: {e}")
            })?;
            info!(path = %path, "loaded workspace bindings");
            b
        }
        _ => {
            info!("no bindings file, workspace scoping disabled");
            WorkspaceBindings::empty()
        }
    };

    // The operator's toolset config, read once. There is no watch: a config
    // change rolls this pod. An unreadable or malformed file is fatal — serving
    // with an empty config would silently refuse every turn.
    let toolsets = ToolsetConfig::load(&config.toolset_config_file).map_err(|e| {
        anyhow::anyhow!(
            "failed to load toolset config from {}: {e}",
            config.toolset_config_file
        )
    })?;
    info!(
        path = %config.toolset_config_file,
        toolsets = ?toolsets.names(),
        "loaded toolset config"
    );

    // The prompt configuration section, read the same way and equally fatal:
    // serving with an empty prompt config would silently refuse every turn.
    let prompt = PromptConfig::load(&config.prompt_config_file).map_err(|e| {
        anyhow::anyhow!(
            "failed to load prompt config from {}: {e}",
            config.prompt_config_file
        )
    })?;
    info!(
        path = %config.prompt_config_file,
        profiles = ?prompt.names(),
        "loaded prompt config"
    );

    // Register every toolset and drive tool discovery once, before serving, so
    // the first request sees a populated registry.
    watcher::reconcile_toolsets(&state, spawner.as_ref(), &bindings, &toolsets).await;

    // Keepalive: reconcile existing tool jobs, then run the idle sweeps and
    // reactive Job watches. Must fire AFTER the config load so the reconcile
    // resolves per-toolset keepalive against a populated registry.
    {
        let state = state.clone();
        let client = kube_client.clone();
        let ns = config.namespace.clone();
        tokio::spawn(async move {
            if let Err(e) = keepalive::reconcile_tool_jobs(&client, &ns, &state).await {
                error!(error = %e, "reconcile_tool_jobs failed; cleanup loop operates on partial state");
            }
            if let Err(e) = keepalive::reconcile_prompt_jobs(&client, &ns, &state).await {
                error!(error = %e, "reconcile_prompt_jobs failed; cleanup loop operates on partial state");
            }
            tokio::spawn(keepalive::tool_cleanup_loop(state.clone()));
            keepalive::prompt_cleanup_loop(state).await;
        });
    }

    spawn_job_watch(
        "tool-jobs",
        state.clone(),
        kube_client.clone(),
        config.namespace.clone(),
        |client, ns, state| async move { keepalive::watch_tool_jobs(client, &ns, state).await },
    );
    spawn_job_watch(
        "prompt-jobs",
        state.clone(),
        kube_client.clone(),
        config.namespace.clone(),
        |client, ns, state| async move { keepalive::watch_prompt_jobs(client, &ns, state).await },
    );

    let addr = format!("0.0.0.0:{GRPC_PORT}").parse()?;
    let service = ControllerService::new(state, Some(verifiers), bindings, prompt);

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<ToolsetControllerServer<ControllerService>>()
        .await;

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(toolset_proto::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    info!(%addr, namespace = %config.namespace, "starting toolset-controller");

    Server::builder()
        .layer(RequiredAudienceLayer)
        .add_service(reflection)
        .add_service(health_service)
        .add_service(ToolsetControllerServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}

/// Spawn a retrying tool-job lifecycle watch.
fn spawn_job_watch<F, Fut>(
    name: &'static str,
    state: Arc<ControllerState>,
    client: kube::Client,
    namespace: String,
    run: F,
) where
    F: Fn(kube::Client, String, Arc<ControllerState>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    let run = Arc::new(run);
    shared::watcher_retry::spawn_watcher_task(name, move || {
        let client = client.clone();
        let ns = namespace.clone();
        let state = state.clone();
        let run = run.clone();
        async move { (*run)(client, ns, state).await }
    });
}

/// Path the kubelet mounts the discovery Job's projected tool-job-audience SA
/// token at (`automountServiceAccountToken=false` + projected token per the
/// house pattern).
const TOOL_JOB_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// One-shot discovery: read the toolset image's tool label off the registry and
/// report it to the controller over `ReportDiscoveredTools`. Retries transient
/// registry errors in-Job (the existing `is_retryable` split); a malformed image
/// reference or tool label is terminal and exits non-zero without reporting.
async fn run_discover() -> anyhow::Result<()> {
    let toolset_name = std::env::var("TOOLSET_TOOLSET_NAME")
        .map_err(|_| anyhow::anyhow!("TOOLSET_TOOLSET_NAME not set"))?;
    let image =
        std::env::var("TOOLSET_IMAGE").map_err(|_| anyhow::anyhow!("TOOLSET_IMAGE not set"))?;
    let controller_addr = std::env::var("TOOLSET_CONTROLLER_ADDR")
        .map_err(|_| anyhow::anyhow!("TOOLSET_CONTROLLER_ADDR not set"))?;
    let token = std::fs::read_to_string(TOOL_JOB_TOKEN_PATH)
        .map_err(|e| {
            anyhow::anyhow!("failed to read tool-job token at {TOOL_JOB_TOKEN_PATH}: {e}")
        })?
        .trim()
        .to_string();

    let discovered =
        watcher::retry_discovery(
            &image,
            |i| async move { registry::discover_tools(&i).await },
        )
        .await
        .map_err(|e| anyhow::anyhow!("discovery failed for {image}: {e}"))?;

    let tools: Vec<DiscoveredToolMsg> = discovered
        .into_iter()
        .map(|d| DiscoveredToolMsg {
            name: d.name,
            description: d.description.unwrap_or_default(),
            args: d
                .args
                .into_iter()
                .map(|a| DiscoveredArgMsg {
                    name: a.name,
                    r#type: a.ty.as_schema_str().to_string(),
                    required: a.required,
                    env: a.env,
                    description: a.description.unwrap_or_default(),
                })
                .collect(),
        })
        .collect();
    let count = tools.len();

    // Parsed once: a malformed header is deterministic, so it must not be
    // retried, and MetadataValue is cheap to clone per attempt.
    let authorization: tonic::metadata::MetadataValue<tonic::metadata::Ascii> =
        format!("Bearer {token}")
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid authorization metadata: {e}"))?;

    // Reconnect on every attempt. The controller's Service publishes no ready
    // endpoint until its readiness probe passes, which cannot happen before it
    // serves, so the first connect from a boot-time discovery Job is refused.
    watcher::retry_report(&toolset_name, || {
        let addr = controller_addr.clone();
        let toolset_name = toolset_name.clone();
        let tools = tools.clone();
        let authorization = authorization.clone();
        async move {
            let mut client = ToolsetControllerClient::connect(addr)
                .await
                .map_err(watcher::ReportError::Transport)?;
            let mut request = tonic::Request::new(ReportDiscoveredToolsRequest {
                toolset_name,
                tools,
            });
            request
                .metadata_mut()
                .insert("authorization", authorization);
            client
                .report_discovered_tools(request)
                .await
                .map_err(watcher::ReportError::Rpc)?;
            Ok(())
        }
    })
    .await?;
    info!(toolset = %toolset_name, %image, count, "reported discovered tools");
    Ok(())
}
