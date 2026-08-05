use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Affinity, Container, EmptyDirVolumeSource, EnvVar, EnvVarSource, KeyToPath, PodAffinity,
    PodAffinityTerm, PodDNSConfig, PodDNSConfigOption, PodSecurityContext, PodSpec,
    PodTemplateSpec, ProjectedVolumeSource, SecretKeySelector, SecretProjection,
    SecretVolumeSource, Volume, VolumeMount, VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::api::PostParams;
use kube::{Api, Client};

use crate::crd::{ModelSpec, ProviderSpec, ToolsetSpec};
use crate::registry::tool_name_to_k8s_segment;
use crate::WORKSPACE_MOUNT_PATH;
use shared::hardened_security_context;
use shared::scheduling::SchedulingConfig;

/// Workspace-label mutual `podAffinity` keyed on
/// `sycophant.md/workspace=<ws>` with hostname topology. Co-locates a
/// tool-worker Job's pod with the workspace's harness pod (which carries the
/// matching `sycophant.md/workspace` label) so kubelet can attach the shared
/// workspace PVC on the same node. K8s special-cases self-referencing affinity
/// so the first pod with this label schedules freely.
fn workspace_affinity(workspace_name: &str) -> Affinity {
    let mut match_labels = BTreeMap::new();
    match_labels.insert(
        "sycophant.md/workspace".to_string(),
        workspace_name.to_string(),
    );
    Affinity {
        pod_affinity: Some(PodAffinity {
            required_during_scheduling_ignored_during_execution: Some(vec![PodAffinityTerm {
                label_selector: Some(LabelSelector {
                    match_labels: Some(match_labels),
                    ..Default::default()
                }),
                topology_key: "kubernetes.io/hostname".to_string(),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

// =========================================================================
// Tool-worker Job (tool dispatch over the Toolset CRD)
// =========================================================================

#[allow(clippy::too_many_arguments)]
pub fn build_tool_job(
    tool_name: &str,
    image: &str,
    toolset_name: &str,
    toolset_spec: &ToolsetSpec,
    call_id: &str,
    namespace: &str,
    controller_addr: &str,
    workspace_name: &str,
    workspace_pvc: &str,
    scheduling: &SchedulingConfig,
) -> Job {
    let job_name = format!(
        "airlock-{}-{}",
        tool_name_to_k8s_segment(tool_name),
        &call_id[..8]
    );
    let keepalive = toolset_spec.keepalive;

    let mut env_vars = vec![
        EnvVar {
            name: "TOOLSET_CONTROLLER_ADDR".to_string(),
            value: Some(controller_addr.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_JOB_ID".to_string(),
            value: Some(call_id.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_TOOL_NAME".to_string(),
            value: Some(tool_name.to_string()),
            ..Default::default()
        },
    ];

    if keepalive {
        env_vars.push(EnvVar {
            name: "TOOLSET_KEEPALIVE".to_string(),
            value: Some("true".to_string()),
            ..Default::default()
        });
    }

    env_vars.push(EnvVar {
        name: "HOME".to_string(),
        value: Some("/home/agent".to_string()),
        ..Default::default()
    });

    let mut volumes = Vec::new();
    let mut volume_mounts = Vec::new();

    volumes.push(Volume {
        name: "tmp".to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    });
    volume_mounts.push(VolumeMount {
        name: "tmp".to_string(),
        mount_path: "/tmp".to_string(),
        ..Default::default()
    });
    volumes.push(Volume {
        name: "home".to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    });
    volume_mounts.push(VolumeMount {
        name: "home".to_string(),
        mount_path: "/home/agent".to_string(),
        ..Default::default()
    });

    // Workspace PVC — always present, mounted RW at /workspace.
    volumes.push(Volume {
        name: "workspace".to_string(),
        persistent_volume_claim: Some(
            k8s_openapi::api::core::v1::PersistentVolumeClaimVolumeSource {
                claim_name: workspace_pvc.to_string(),
                read_only: None,
            },
        ),
        ..Default::default()
    });
    volume_mounts.push(VolumeMount {
        name: "workspace".to_string(),
        mount_path: WORKSPACE_MOUNT_PATH.to_string(),
        ..Default::default()
    });

    // Credentials from the Toolset.
    let mut credential_map: Vec<serde_json::Value> = Vec::new();
    for (i, cred) in toolset_spec.credentials.iter().enumerate() {
        if let Some(ref env_name) = cred.env {
            env_vars.push(EnvVar {
                name: env_name.clone(),
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: cred.secret.clone(),
                        key: cred.secret.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            });
        } else if let Some(ref file_path) = cred.file {
            let vol_name = format!("cred-{i}");
            let basename = std::path::Path::new(file_path)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(&cred.secret)
                .to_string();
            let items = Some(vec![KeyToPath {
                key: cred.secret.clone(),
                path: basename.clone(),
                ..Default::default()
            }]);
            let staging_path = format!("/tmp/credentials/{vol_name}/{basename}");
            let target_path = file_path.clone();
            volumes.push(Volume {
                name: vol_name.clone(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(cred.secret.clone()),
                    items,
                    default_mode: Some(0o444),
                    ..Default::default()
                }),
                ..Default::default()
            });
            volume_mounts.push(VolumeMount {
                name: vol_name,
                mount_path: staging_path.clone(),
                sub_path: Some(basename),
                read_only: Some(true),
                ..Default::default()
            });
            credential_map
                .push(serde_json::json!({"staging": staging_path, "target": target_path}));
        }
    }
    if !credential_map.is_empty() {
        env_vars.push(EnvVar {
            name: "TOOLSET_CREDENTIAL_MAP".to_string(),
            value: Some(serde_json::to_string(&credential_map).unwrap()),
            ..Default::default()
        });
    }

    if !toolset_spec.credentials.is_empty() {
        let scrub_entries: Vec<serde_json::Value> = toolset_spec
            .credentials
            .iter()
            .map(|cred| {
                let mut entry = serde_json::json!({"name": cred.secret});
                if let Some(ref env_name) = cred.env {
                    entry["env"] = serde_json::json!(env_name);
                } else if let Some(ref file_path) = cred.file {
                    entry["file"] = serde_json::json!(file_path);
                }
                entry
            })
            .collect();
        env_vars.push(EnvVar {
            name: "TOOLSET_SCRUB_SECRETS".to_string(),
            value: Some(serde_json::to_string(&scrub_entries).unwrap()),
            ..Default::default()
        });
    }

    // Custom-audience projected SA token mounted at the kubelet-default path so
    // the tool-worker pod presents a toolset-audience token instead of the
    // namespace default SA token. automountServiceAccountToken=false (below)
    // suppresses the kubelet default; the pod VAP requires that.
    let (auth_volume, auth_mount) = shared::podspec::sa_token_volume(
        "airlock-job-auth",
        shared::auth::TOOLSET_TOOLSET_AUDIENCE,
    );
    volumes.push(auth_volume);
    volume_mounts.push(auth_mount);

    let container = Container {
        name: "runtime".to_string(),
        image: Some(image.to_string()),
        env: Some(env_vars),
        volume_mounts: Some(volume_mounts),
        security_context: Some(hardened_security_context()),
        ..Default::default()
    };

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    labels.insert("sycophant.md/tool".to_string(), tool_name.to_string());
    labels.insert("sycophant.md/call-id".to_string(), call_id.to_string());
    labels.insert("sycophant.md/toolset".to_string(), toolset_name.to_string());
    labels.insert(
        "sycophant.md/workspace".to_string(),
        workspace_name.to_string(),
    );

    let mut pod_labels = BTreeMap::new();
    pod_labels.insert(
        "app.kubernetes.io/component".to_string(),
        "airlock-job".to_string(),
    );
    pod_labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    pod_labels.insert("sycophant.md/toolset".to_string(), toolset_name.to_string());
    pod_labels.insert("sycophant.md/tool".to_string(), tool_name.to_string());
    pod_labels.insert(
        "sycophant.md/workspace".to_string(),
        workspace_name.to_string(),
    );

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(JobSpec {
            ttl_seconds_after_finished: Some(30),
            backoff_limit: Some(0),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some(if keepalive {
                        "OnFailure".to_string()
                    } else {
                        "Never".to_string()
                    }),
                    // runtimeClassName stamped by Kyverno mutate at admission
                    // (from the airlock-job component label). Run as the
                    // workspace SA so the pod presents the toolset-audience
                    // projected token, not the namespace default SA token.
                    service_account_name: Some(format!("sa-{workspace_name}")),
                    automount_service_account_token: Some(false),
                    security_context: Some(PodSecurityContext {
                        run_as_non_root: Some(true),
                        run_as_user: Some(1000),
                        fs_group: Some(1000),
                        ..Default::default()
                    }),
                    share_process_namespace: Some(false),
                    containers: vec![container],
                    volumes: Some(volumes),
                    affinity: Some(workspace_affinity(workspace_name)),
                    node_selector: if scheduling.node_selector.is_empty() {
                        None
                    } else {
                        Some(scheduling.node_selector.clone())
                    },
                    tolerations: if scheduling.tolerations.is_empty() {
                        None
                    } else {
                        Some(scheduling.tolerations.clone())
                    },
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

pub async fn create_job(client: &Client, namespace: &str, job: &Job) -> anyhow::Result<Job> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), namespace);
    let result = jobs.create(&PostParams::default(), job).await?;
    Ok(result)
}

// =========================================================================
// Discovery Job (reads a Toolset image's tool label off the registry)
// =========================================================================

/// Discriminator label the discovery-Job pod carries so its registry-egress
/// CNP selects it alone, never the shared `airlock-job` tool-worker floor.
const DISCOVERY_JOB_LABEL: &str = "discovery";

/// Build the ephemeral discovery Job for a Toolset. It runs the controller's
/// own image under the `discover` subcommand, reads the `md.sycophant.tools`
/// label off `toolset_image`, and reports the tool set back over
/// `ReportDiscoveredTools`. Gated as an `airlock-job` (so Kyverno stamps gVisor
/// and the baseline CNP applies) and additionally labelled
/// `sycophant.md/job: discovery` so the discovery registry-egress CNP selects
/// it without widening any tool-worker pod. Runtime class is NOT set here —
/// admission stamps it.
pub fn build_discovery_job(
    toolset_name: &str,
    toolset_image: &str,
    namespace: &str,
    controller_addr: &str,
    workspace_name: &str,
    scheduling: &SchedulingConfig,
) -> Job {
    // The discovery pod runs the controller's own first-party image. The chart
    // sets TOOLSET_CONTROLLER_IMAGE on the controller pod so it can spawn a copy
    // of itself; unset only in unit tests, which never assert the image.
    let controller_image = std::env::var("TOOLSET_CONTROLLER_IMAGE").unwrap_or_default();

    let env_vars = vec![
        EnvVar {
            name: "TOOLSET_TOOLSET_NAME".to_string(),
            value: Some(toolset_name.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_IMAGE".to_string(),
            value: Some(toolset_image.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_CONTROLLER_ADDR".to_string(),
            value: Some(controller_addr.to_string()),
            ..Default::default()
        },
    ];

    // Worker-audience projected SA token so the report authenticates on the
    // worker method set; automount=false suppresses the kubelet default.
    let (auth_volume, auth_mount) = shared::podspec::sa_token_volume(
        "discovery-job-auth",
        shared::auth::TOOLSET_TOOLSET_AUDIENCE,
    );

    let container = Container {
        name: "discovery".to_string(),
        image: Some(controller_image),
        args: Some(vec!["discover".to_string()]),
        env: Some(env_vars),
        volume_mounts: Some(vec![auth_mount]),
        security_context: Some(hardened_security_context()),
        ..Default::default()
    };

    let mut pod_labels = BTreeMap::new();
    pod_labels.insert(
        "app.kubernetes.io/component".to_string(),
        "airlock-job".to_string(),
    );
    pod_labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    pod_labels.insert(
        "sycophant.md/job".to_string(),
        DISCOVERY_JOB_LABEL.to_string(),
    );
    pod_labels.insert("sycophant.md/toolset".to_string(), toolset_name.to_string());
    pod_labels.insert(
        "sycophant.md/workspace".to_string(),
        workspace_name.to_string(),
    );

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    labels.insert(
        "sycophant.md/job".to_string(),
        DISCOVERY_JOB_LABEL.to_string(),
    );
    labels.insert("sycophant.md/toolset".to_string(), toolset_name.to_string());

    Job {
        metadata: ObjectMeta {
            generate_name: Some(format!(
                "discovery-{}-",
                tool_name_to_k8s_segment(toolset_name)
            )),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(JobSpec {
            ttl_seconds_after_finished: Some(30),
            backoff_limit: Some(0),
            // Bound a wedged discovery pod. The in-Job retry backoff tops out at
            // ~15.5s; this leaves ample headroom without matching the token TTL.
            active_deadline_seconds: Some(120),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_string()),
                    service_account_name: Some(format!("sa-{workspace_name}")),
                    automount_service_account_token: Some(false),
                    // ndots:1 so external registry hosts resolve as-is instead of
                    // expanding into cluster search domains the L7 DNS rule denies.
                    dns_config: Some(PodDNSConfig {
                        options: Some(vec![PodDNSConfigOption {
                            name: Some("ndots".to_string()),
                            value: Some("1".to_string()),
                        }]),
                        ..Default::default()
                    }),
                    security_context: Some(PodSecurityContext {
                        run_as_non_root: Some(true),
                        run_as_user: Some(1000),
                        fs_group: Some(1000),
                        ..Default::default()
                    }),
                    containers: vec![container],
                    volumes: Some(vec![auth_volume]),
                    node_selector: if scheduling.node_selector.is_empty() {
                        None
                    } else {
                        Some(scheduling.node_selector.clone())
                    },
                    tolerations: if scheduling.tolerations.is_empty() {
                        None
                    } else {
                        Some(scheduling.tolerations.clone())
                    },
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

// =========================================================================
// Prompt-worker Job (turn dispatch over the Model/Provider CRDs)
// =========================================================================

fn canonical_base_url(format: &str) -> String {
    match format {
        "anthropic" => "https://api.anthropic.com/v1".into(),
        "openai" => "https://api.openai.com/v1".into(),
        "gemini" => "https://generativelanguage.googleapis.com".into(),
        _ => String::new(),
    }
}

/// Build the credentialed prompt-worker Job for a turn. Gated as an
/// `airlock-job` (so Kyverno stamps gVisor and the airlock-job baseline CNP
/// applies) and labelled `sycophant.md/toolset: prompt-<provider>`, whose
/// per-toolset egress CNP pins which provider the worker may reach. The
/// controller never reads the provider secret: kubelet mounts it as a file.
#[allow(clippy::too_many_arguments)]
pub fn build_prompt_job(
    model_name: &str,
    model: &ModelSpec,
    provider: &ProviderSpec,
    prompt_toolset: &str,
    image: &str,
    controller_addr: &str,
    namespace: &str,
    session_id: &str,
    workspace: &str,
    scheduling: &SchedulingConfig,
) -> Job {
    let job_name = format!("toolset-prompt-{model_name}-{session_id}");

    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/part-of".into(), "sycophant".to_string());
    labels.insert(
        "app.kubernetes.io/component".into(),
        "airlock-job".to_string(),
    );
    labels.insert("sycophant.md/type".into(), "prompt".to_string());
    labels.insert("sycophant.md/model".into(), model_name.to_string());
    labels.insert("sycophant.md/toolset".into(), prompt_toolset.to_string());
    labels.insert("sycophant.md/workspace".into(), workspace.to_string());

    let base_url = provider
        .base_url
        .clone()
        .unwrap_or_else(|| canonical_base_url(&provider.format));

    let secret_key = provider
        .secret
        .key
        .clone()
        .unwrap_or_else(|| "api-key".into());

    let mut env_vars = vec![
        EnvVar {
            name: "TOOLSET_CONTROLLER_ADDR".into(),
            value: Some(controller_addr.into()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_MODEL_NAME".into(),
            value: Some(model_name.into()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_JOB_ID".into(),
            value: Some(format!("prompt-{model_name}-{session_id}")),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_FORMAT".into(),
            value: Some(provider.format.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_MODEL".into(),
            value: Some(model.model.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_BASE_URL".into(),
            value: Some(base_url),
            ..Default::default()
        },
        EnvVar {
            name: "TOOLSET_WORKSPACE".into(),
            value: Some(workspace.into()),
            ..Default::default()
        },
    ];

    if let Some(params) = &model.params {
        env_vars.push(EnvVar {
            name: "TOOLSET_PARAMS".into(),
            value: Some(
                serde_json::to_string(params).expect("Model.params serializes deterministically"),
            ),
            ..Default::default()
        });
    }

    // Wire the in-pod ScrubSet so the prompt worker redacts any occurrence of
    // the API-key value in logs, chunks, and error messages. The key lives only
    // as a file at `/run/secrets/toolset/api-key` — never an env var — so the
    // scrub registry uses the `file` variant.
    let scrub_entries = serde_json::json!([{
        "name": provider.secret.name,
        "file": "/run/secrets/toolset/api-key",
    }]);
    env_vars.push(EnvVar {
        name: "TOOLSET_SCRUB_SECRETS".into(),
        value: Some(serde_json::to_string(&scrub_entries).expect("scrub registry serializes")),
        ..Default::default()
    });

    // Projected volume: kubelet mounts the provider Secret's key value as a
    // file at `/run/secrets/toolset/api-key` (mode 0o440). Mode 0o440 +
    // pod-level fsGroup=1000 so the runAsUser=1000 container reads it via group
    // access (kubelet mounts root-owned).
    let secret_volume_name = "toolset-secret".to_string();
    let projected_volume = Volume {
        name: secret_volume_name.clone(),
        projected: Some(ProjectedVolumeSource {
            default_mode: Some(0o440),
            sources: Some(vec![VolumeProjection {
                secret: Some(SecretProjection {
                    name: provider.secret.name.clone(),
                    items: Some(vec![KeyToPath {
                        key: secret_key,
                        path: "api-key".into(),
                        mode: Some(0o440),
                    }]),
                    optional: Some(false),
                }),
                ..Default::default()
            }]),
        }),
        ..Default::default()
    };
    let secret_mount = VolumeMount {
        name: secret_volume_name,
        mount_path: "/run/secrets/toolset".into(),
        read_only: Some(true),
        ..Default::default()
    };

    // Custom-audience projected SA token: the prompt-worker pod's token carries
    // the toolset audience so the controller only accepts it on the worker
    // methods. A harness-audience token cannot reach them.
    let (auth_volume, auth_mount) =
        shared::podspec::sa_token_volume("prompt-job-auth", shared::auth::TOOLSET_TOOLSET_AUDIENCE);

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.into()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            ttl_seconds_after_finished: Some(30),
            // A worker holds an in-flight turn; a silent K8s-driven respawn
            // would race a second pod for the same assignment. Fail instead.
            backoff_limit: Some(0),
            // Coarse platform backstop: bound a wedged/zombie worker. Matches
            // the projected SA-token lifetime (3600s).
            active_deadline_seconds: Some(3600),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".into()),
                    // No runtimeClassName here: Kyverno stamps gVisor from the
                    // airlock-job component label at admission.
                    service_account_name: Some(format!("sa-{workspace}")),
                    automount_service_account_token: Some(false),
                    // ndots:1 so external provider hosts resolve as-is; default
                    // ndots:5 expands to cluster search domains the per-host L7
                    // DNS allowlist denies, stalling resolution.
                    dns_config: Some(PodDNSConfig {
                        options: Some(vec![PodDNSConfigOption {
                            name: Some("ndots".into()),
                            value: Some("1".into()),
                        }]),
                        ..Default::default()
                    }),
                    security_context: Some(PodSecurityContext {
                        run_as_non_root: Some(true),
                        run_as_user: Some(1000),
                        fs_group: Some(1000),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "prompt".into(),
                        image: Some(image.into()),
                        env: Some(env_vars),
                        volume_mounts: Some(vec![secret_mount, auth_mount]),
                        security_context: Some(hardened_security_context()),
                        ..Default::default()
                    }],
                    volumes: Some(vec![projected_volume, auth_volume]),
                    node_selector: if scheduling.node_selector.is_empty() {
                        None
                    } else {
                        Some(scheduling.node_selector.clone())
                    },
                    tolerations: if scheduling.tolerations.is_empty() {
                        None
                    } else {
                        Some(scheduling.tolerations.clone())
                    },
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn create_prompt_job(
    client: &Client,
    model_name: &str,
    model: &ModelSpec,
    provider: &ProviderSpec,
    prompt_toolset: &str,
    image: &str,
    controller_addr: &str,
    namespace: &str,
    workspace: &str,
    scheduling: &SchedulingConfig,
) -> Result<String, kube::Error> {
    let session_id = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let job = build_prompt_job(
        model_name,
        model,
        provider,
        prompt_toolset,
        image,
        controller_addr,
        namespace,
        &session_id,
        workspace,
        scheduling,
    );
    let job_name = job.metadata.name.clone().unwrap_or_default();

    let api: Api<Job> = Api::namespaced(client.clone(), namespace);
    api.create(&PostParams::default(), &job).await?;

    tracing::info!("created prompt Job {job_name} in namespace {namespace}");
    Ok(job_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{CredentialMapping, ProviderRef, ProviderSecret};
    use shared::scheduling::testing::{assert_scheduling, no_scheduling, test_scheduling};

    // ---- Tool-worker Job tests ----

    const TEST_CALL_ID: &str = "abcdef12-0000-0000-0000-000000000000";
    const TEST_IMAGE: &str = "ghcr.io/test/airlock-git:latest";
    const TEST_TOOLSET: &str = "test-toolset";
    const TEST_WORKSPACE: &str = "test";
    const TEST_WORKSPACE_PVC: &str = "test-workspace-data";

    fn base_toolset_spec() -> ToolsetSpec {
        ToolsetSpec {
            image: Some(TEST_IMAGE.into()),
            credentials: vec![],
            egress: vec![],
            keepalive: false,
        }
    }

    fn test_job(toolset_spec: &ToolsetSpec) -> Job {
        build_tool_job(
            "git-push",
            TEST_IMAGE,
            TEST_TOOLSET,
            toolset_spec,
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &no_scheduling(),
        )
    }

    fn pod_spec(job: &Job) -> &PodSpec {
        job.spec.as_ref().unwrap().template.spec.as_ref().unwrap()
    }

    fn container(job: &Job) -> &Container {
        &pod_spec(job).containers[0]
    }

    fn env_map(job: &Job) -> BTreeMap<&str, &str> {
        container(job)
            .env
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|e| e.value.as_deref().map(|v| (e.name.as_str(), v)))
            .collect()
    }

    #[test]
    fn tool_job_does_not_set_runtime_class() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(pod_spec(&job).runtime_class_name, None);
    }

    #[test]
    fn tool_job_has_workspace_label_affinity() {
        let job = test_job(&base_toolset_spec());
        let affinity = pod_spec(&job).affinity.as_ref().expect("affinity present");
        let term = &affinity
            .pod_affinity
            .as_ref()
            .expect("podAffinity present")
            .required_during_scheduling_ignored_during_execution
            .as_ref()
            .expect("required term present")[0];
        assert_eq!(term.topology_key, "kubernetes.io/hostname");
        let match_labels = term
            .label_selector
            .as_ref()
            .and_then(|s| s.match_labels.as_ref())
            .expect("matchLabels present");
        assert_eq!(
            match_labels
                .get("sycophant.md/workspace")
                .map(String::as_str),
            Some(TEST_WORKSPACE)
        );
    }

    #[test]
    fn tool_job_pod_template_carries_workspace_label() {
        let job = test_job(&base_toolset_spec());
        let labels = job
            .spec
            .as_ref()
            .unwrap()
            .template
            .metadata
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap();
        assert_eq!(
            labels.get("sycophant.md/workspace").map(String::as_str),
            Some(TEST_WORKSPACE)
        );
    }

    #[test]
    fn tool_job_has_correct_metadata() {
        let job = test_job(&base_toolset_spec());

        assert_eq!(
            job.metadata.name.as_deref(),
            Some("airlock-git-push-abcdef12")
        );
        assert_eq!(job.metadata.namespace.as_deref(), Some("test-ns"));

        let labels = job.metadata.labels.as_ref().unwrap();
        assert_eq!(labels["app.kubernetes.io/part-of"], "sycophant");
        assert_eq!(labels["sycophant.md/tool"], "git-push");
        assert_eq!(labels["sycophant.md/toolset"], "test-toolset");
    }

    #[test]
    fn tool_job_name_kebab_cases_pascal_case_tool_name() {
        let job = build_tool_job(
            "ReadFile",
            TEST_IMAGE,
            TEST_TOOLSET,
            &base_toolset_spec(),
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &no_scheduling(),
        );
        assert_eq!(
            job.metadata.name.as_deref(),
            Some("airlock-read-file-abcdef12"),
            "job name must be RFC 1123-valid kebab-case"
        );
        let labels = job.metadata.labels.as_ref().unwrap();
        assert_eq!(
            labels["sycophant.md/tool"], "ReadFile",
            "label keeps the canonical LLM-facing identifier"
        );
        let env = env_map(&job);
        assert_eq!(
            env.get("TOOLSET_TOOL_NAME"),
            Some(&"ReadFile"),
            "runtime receives the canonical tool name"
        );
    }

    #[test]
    fn tool_job_pod_template_has_toolset_and_component_labels() {
        let job = test_job(&base_toolset_spec());
        let pod_labels = job
            .spec
            .as_ref()
            .unwrap()
            .template
            .metadata
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap();
        assert_eq!(pod_labels["sycophant.md/toolset"], "test-toolset");
        assert_eq!(pod_labels["sycophant.md/tool"], "git-push");
        // The chart's airlock-job-baseline CNP selects on these labels — the
        // fail-closed egress floor for every toolset pod depends on them.
        assert_eq!(pod_labels["app.kubernetes.io/component"], "airlock-job");
        assert_eq!(pod_labels["app.kubernetes.io/part-of"], "sycophant");
    }

    #[test]
    fn tool_job_has_correct_env_vars() {
        let job = test_job(&base_toolset_spec());
        let env = env_map(&job);

        assert_eq!(env["TOOLSET_CONTROLLER_ADDR"], "http://controller:9090");
        assert_eq!(env["TOOLSET_JOB_ID"], TEST_CALL_ID);
        assert_eq!(env["TOOLSET_TOOL_NAME"], "git-push");
        assert!(!env.contains_key("TOOLSET_KEEPALIVE"));
    }

    #[test]
    fn keepalive_tool_job_has_env_and_restart_policy() {
        let mut toolset = base_toolset_spec();
        toolset.keepalive = true;
        let job = test_job(&toolset);
        let env = env_map(&job);

        assert_eq!(env.get("TOOLSET_KEEPALIVE"), Some(&"true"));
        assert_eq!(pod_spec(&job).restart_policy.as_deref(), Some("OnFailure"));
    }

    #[test]
    fn fire_and_forget_restart_policy() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(pod_spec(&job).restart_policy.as_deref(), Some("Never"));
        assert_eq!(job.spec.as_ref().unwrap().backoff_limit, Some(0));
    }

    #[test]
    fn workspace_pvc_mounted_rw_at_workspace() {
        let job = test_job(&base_toolset_spec());
        let volumes = pod_spec(&job).volumes.as_ref().unwrap();
        let ws_vol = volumes.iter().find(|v| v.name == "workspace").unwrap();
        let pvc = ws_vol.persistent_volume_claim.as_ref().unwrap();
        assert_eq!(pvc.claim_name, TEST_WORKSPACE_PVC);
        assert!(!pvc.read_only.unwrap_or(false), "PVC must be RW");

        let mounts = container(&job).volume_mounts.as_ref().unwrap();
        let ws_mount = mounts.iter().find(|m| m.name == "workspace").unwrap();
        assert_eq!(ws_mount.mount_path, "/workspace");
        assert!(!ws_mount.read_only.unwrap_or(false), "mount must be RW");
    }

    #[test]
    fn credential_env_mode() {
        let mut toolset = base_toolset_spec();
        toolset.credentials.push(CredentialMapping {
            secret: "github-token".to_string(),
            env: Some("GITHUB_TOKEN".to_string()),
            file: None,
        });

        let job = test_job(&toolset);
        let env_vars = container(&job).env.as_ref().unwrap();
        let gh_env = env_vars.iter().find(|e| e.name == "GITHUB_TOKEN").unwrap();

        let secret_ref = gh_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(secret_ref.name, "github-token");
        assert_eq!(secret_ref.key, "github-token");
    }

    #[test]
    fn credential_file_mode() {
        let mut toolset = base_toolset_spec();
        toolset.credentials.push(CredentialMapping {
            secret: "git-ssh-key".to_string(),
            env: None,
            file: Some("/home/agent/.ssh/id_ed25519".to_string()),
        });

        let job = test_job(&toolset);
        let volumes = pod_spec(&job).volumes.as_ref().unwrap();
        let cred_vol = volumes.iter().find(|v| v.name == "cred-0").unwrap();
        let secret = cred_vol.secret.as_ref().unwrap();
        assert_eq!(secret.secret_name.as_deref(), Some("git-ssh-key"));
        assert_eq!(secret.items.as_ref().unwrap()[0].key, "git-ssh-key");
        assert_eq!(secret.default_mode, Some(0o444));

        let mounts = container(&job).volume_mounts.as_ref().unwrap();
        let cred_mount = mounts.iter().find(|m| m.name == "cred-0").unwrap();
        assert_eq!(cred_mount.mount_path, "/tmp/credentials/cred-0/id_ed25519");
        assert_eq!(cred_mount.sub_path.as_deref(), Some("id_ed25519"));
        assert_eq!(cred_mount.read_only, Some(true));

        let env = env_map(&job);
        let raw = &env["TOOLSET_CREDENTIAL_MAP"];
        let map: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert_eq!(map[0]["staging"], "/tmp/credentials/cred-0/id_ed25519");
        assert_eq!(map[0]["target"], "/home/agent/.ssh/id_ed25519");
    }

    #[test]
    fn ttl_seconds_set() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(
            job.spec.as_ref().unwrap().ttl_seconds_after_finished,
            Some(30)
        );
    }

    #[test]
    fn correct_image() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(
            container(&job).image.as_deref(),
            Some("ghcr.io/test/airlock-git:latest")
        );
    }

    fn scrub_env(job: &Job) -> Option<String> {
        container(job)
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == "TOOLSET_SCRUB_SECRETS")
            .and_then(|e| e.value.clone())
    }

    #[test]
    fn scrub_secrets_env_var_absent_for_zero_credential_toolset() {
        let job = test_job(&base_toolset_spec());
        assert!(scrub_env(&job).is_none());
    }

    #[test]
    fn scrub_secrets_env_var_set_for_credentialed_toolset() {
        let mut toolset = base_toolset_spec();
        toolset.credentials.push(CredentialMapping {
            secret: "db-url".to_string(),
            env: Some("DATABASE_URL".to_string()),
            file: None,
        });
        let job = test_job(&toolset);
        assert!(scrub_env(&job).is_some());
    }

    #[test]
    fn scrub_secrets_env_maps_correctly() {
        let mut toolset = base_toolset_spec();
        toolset.credentials.push(CredentialMapping {
            secret: "stripe-key".to_string(),
            env: Some("STRIPE_KEY".to_string()),
            file: None,
        });
        let job = test_job(&toolset);
        let json: Vec<serde_json::Value> = serde_json::from_str(&scrub_env(&job).unwrap()).unwrap();
        assert_eq!(json[0]["name"], "stripe-key");
        assert_eq!(json[0]["env"], "STRIPE_KEY");
        assert!(json[0].get("file").is_none());
    }

    #[test]
    fn scrub_secrets_file_maps_correctly() {
        let mut toolset = base_toolset_spec();
        toolset.credentials.push(CredentialMapping {
            secret: "ssh-key".to_string(),
            env: None,
            file: Some("/home/agent/.ssh/id_ed25519".to_string()),
        });
        let job = test_job(&toolset);
        let json: Vec<serde_json::Value> = serde_json::from_str(&scrub_env(&job).unwrap()).unwrap();
        assert_eq!(json[0]["name"], "ssh-key");
        assert_eq!(json[0]["file"], "/home/agent/.ssh/id_ed25519");
        assert!(json[0].get("env").is_none());
    }

    #[test]
    fn share_process_namespace_disabled() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(pod_spec(&job).share_process_namespace, Some(false));
    }

    #[test]
    fn tool_job_has_scheduling_constraints() {
        let sched = test_scheduling("airlock");
        let job = build_tool_job(
            "git-push",
            TEST_IMAGE,
            TEST_TOOLSET,
            &base_toolset_spec(),
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &sched,
        );
        assert_scheduling(pod_spec(&job), "airlock");
    }

    #[test]
    fn tool_job_no_scheduling_when_empty() {
        let job = test_job(&base_toolset_spec());
        let ps = pod_spec(&job);
        assert!(ps.node_selector.is_none());
        assert!(ps.tolerations.is_none());
    }

    #[test]
    fn tool_job_has_hardened_security_context() {
        let job = test_job(&base_toolset_spec());
        let sc = container(&job).security_context.as_ref().unwrap();
        assert_eq!(sc.run_as_non_root, Some(true));
        assert_eq!(sc.run_as_user, Some(1000));
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(
            sc.capabilities.as_ref().unwrap().drop,
            Some(vec!["ALL".to_string()])
        );
    }

    #[test]
    fn tool_job_has_pod_security_context() {
        let job = test_job(&base_toolset_spec());
        let psc = pod_spec(&job).security_context.as_ref().unwrap();
        assert_eq!(psc.run_as_non_root, Some(true));
        assert_eq!(psc.run_as_user, Some(1000));
        assert_eq!(psc.fs_group, Some(1000));
    }

    #[test]
    fn tool_job_has_tmp_and_home_mounts() {
        let job = test_job(&base_toolset_spec());
        let vols = pod_spec(&job).volumes.as_ref().unwrap();
        let mounts = container(&job).volume_mounts.as_ref().unwrap();

        assert!(vols
            .iter()
            .any(|v| v.name == "tmp" && v.empty_dir.is_some()));
        assert!(vols
            .iter()
            .any(|v| v.name == "home" && v.empty_dir.is_some()));
        assert!(mounts
            .iter()
            .any(|m| m.name == "tmp" && m.mount_path == "/tmp"));
        assert!(mounts
            .iter()
            .any(|m| m.name == "home" && m.mount_path == "/home/agent"));
    }

    #[test]
    fn tool_job_has_home_env() {
        let job = test_job(&base_toolset_spec());
        let env = env_map(&job);
        assert_eq!(env.get("HOME"), Some(&"/home/agent"));
    }

    #[test]
    fn tool_job_has_container_name() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(container(&job).name, "runtime");
    }

    #[test]
    fn tool_job_runs_as_workspace_sa() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(
            pod_spec(&job).service_account_name.as_deref(),
            Some("sa-test")
        );
    }

    #[test]
    fn tool_job_disables_kubelet_default_sa_token_mount() {
        let job = test_job(&base_toolset_spec());
        assert_eq!(
            pod_spec(&job).automount_service_account_token,
            Some(false),
            "automount=false satisfies the pod VAP and suppresses the kubelet default token"
        );
    }

    #[test]
    fn tool_job_mounts_toolset_audience_projected_token() {
        let job = test_job(&base_toolset_spec());
        let ps = pod_spec(&job);
        let auth_vol = ps
            .volumes
            .as_ref()
            .and_then(|vs| vs.iter().find(|v| v.name == "airlock-job-auth"))
            .expect("airlock-job-auth volume must be present");
        let sources = auth_vol
            .projected
            .as_ref()
            .and_then(|p| p.sources.as_ref())
            .expect("projected sources");
        assert_eq!(sources.len(), 1);
        let sat = sources[0]
            .service_account_token
            .as_ref()
            .expect("serviceAccountToken source");
        assert_eq!(
            sat.audience.as_deref(),
            Some(shared::auth::TOOLSET_TOOLSET_AUDIENCE),
        );
        assert_eq!(sat.path, "token");
        assert_eq!(sat.expiration_seconds, Some(3600));

        let mounts = container(&job).volume_mounts.as_ref().unwrap();
        let auth_mount = mounts
            .iter()
            .find(|m| m.name == "airlock-job-auth")
            .expect("runtime container must mount airlock-job-auth");
        assert_eq!(
            auth_mount.mount_path,
            "/var/run/secrets/kubernetes.io/serviceaccount"
        );
        assert_eq!(auth_mount.read_only, Some(true));
    }

    // ---- Prompt-worker Job tests ----

    const PROMPT_IMAGE: &str = "ghcr.io/test/prompt-toolset:latest";
    const PROMPT_TOOLSET: &str = "prompt-anthropic";

    fn sample_model_spec() -> ModelSpec {
        ModelSpec {
            provider_ref: ProviderRef {
                name: "anthropic".into(),
            },
            model: "claude-sonnet-4-20250514".into(),
            params: None,
        }
    }

    fn sample_provider_spec() -> ProviderSpec {
        ProviderSpec {
            format: "anthropic".into(),
            base_url: Some("https://api.anthropic.com/v1".into()),
            secret: ProviderSecret {
                name: "anthropic-key".into(),
                key: None,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prompt_job_with(
        model: &ModelSpec,
        provider: &ProviderSpec,
        workspace: &str,
        scheduling: &SchedulingConfig,
    ) -> Job {
        build_prompt_job(
            "claude-sonnet",
            model,
            provider,
            PROMPT_TOOLSET,
            PROMPT_IMAGE,
            "http://controller:9090",
            "ns",
            "s1",
            workspace,
            scheduling,
        )
    }

    fn sample_prompt_job() -> Job {
        prompt_job_with(
            &sample_model_spec(),
            &sample_provider_spec(),
            "default",
            &no_scheduling(),
        )
    }

    fn prompt_env(job: &Job) -> BTreeMap<String, String> {
        job.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|e| e.value.as_ref().map(|v| (e.name.clone(), v.clone())))
            .collect()
    }

    #[test]
    fn prompt_job_does_not_set_runtime_class() {
        // gVisor is stamped by Kyverno from the airlock-job component label.
        assert_eq!(
            sample_prompt_job()
                .spec
                .unwrap()
                .template
                .spec
                .unwrap()
                .runtime_class_name,
            None,
        );
    }

    #[test]
    fn prompt_job_gated_as_airlock_job_with_toolset_and_workspace_labels() {
        let job = prompt_job_with(
            &sample_model_spec(),
            &sample_provider_spec(),
            "my-workspace",
            &no_scheduling(),
        );
        let labels = job.metadata.labels.clone().unwrap();
        assert_eq!(labels["app.kubernetes.io/part-of"], "sycophant");
        assert_eq!(
            labels["app.kubernetes.io/component"], "airlock-job",
            "prompt jobs must be gated as airlock-job so Kyverno stamps gVisor and the baseline CNP applies"
        );
        assert_eq!(labels["sycophant.md/type"], "prompt");
        assert_eq!(labels["sycophant.md/model"], "claude-sonnet");
        assert_eq!(
            labels["sycophant.md/toolset"], PROMPT_TOOLSET,
            "the prompt toolset label pins the provider egress CNP"
        );
        assert_eq!(
            labels["sycophant.md/workspace"], "my-workspace",
            "the workspace label must be non-empty"
        );
    }

    #[test]
    fn prompt_job_has_correct_name_and_namespace() {
        let job = build_prompt_job(
            "claude-sonnet",
            &sample_model_spec(),
            &sample_provider_spec(),
            PROMPT_TOOLSET,
            PROMPT_IMAGE,
            "http://controller:9090",
            "workspace-test",
            "abc123",
            "default",
            &no_scheduling(),
        );
        assert_eq!(
            job.metadata.name.unwrap(),
            "toolset-prompt-claude-sonnet-abc123"
        );
        assert_eq!(job.metadata.namespace.unwrap(), "workspace-test");
    }

    #[test]
    fn prompt_job_spec_hardening() {
        let spec = sample_prompt_job().spec.unwrap();
        assert_eq!(spec.backoff_limit, Some(0));
        assert_eq!(spec.active_deadline_seconds, Some(3600));
        assert_eq!(spec.ttl_seconds_after_finished, Some(30));
        assert_eq!(
            spec.template.spec.unwrap().restart_policy.as_deref(),
            Some("Never")
        );
    }

    #[test]
    fn prompt_job_sets_ndots_1() {
        let dns = sample_prompt_job()
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .dns_config
            .unwrap();
        let opt = &dns.options.unwrap()[0];
        assert_eq!(opt.name.as_deref(), Some("ndots"));
        assert_eq!(opt.value.as_deref(), Some("1"));
    }

    #[test]
    fn prompt_job_env_vars_from_provider_and_model() {
        let env = prompt_env(&sample_prompt_job());
        assert_eq!(env["TOOLSET_CONTROLLER_ADDR"], "http://controller:9090");
        assert_eq!(env["TOOLSET_MODEL_NAME"], "claude-sonnet");
        assert_eq!(env["TOOLSET_FORMAT"], "anthropic");
        assert_eq!(env["TOOLSET_MODEL"], "claude-sonnet-4-20250514");
        assert_eq!(env["TOOLSET_BASE_URL"], "https://api.anthropic.com/v1");
        assert_eq!(env["TOOLSET_WORKSPACE"], "default");
    }

    #[test]
    fn prompt_job_params_env_set_when_model_has_params() {
        let mut model = sample_model_spec();
        let mut params = serde_json::Map::new();
        params.insert("temperature".into(), serde_json::json!(0.7));
        model.params = Some(params);
        let job = prompt_job_with(&model, &sample_provider_spec(), "default", &no_scheduling());
        let env = prompt_env(&job);
        let raw = env.get("TOOLSET_PARAMS").expect("params env set");
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.get("temperature"), Some(&serde_json::json!(0.7)));
    }

    #[test]
    fn prompt_job_params_env_absent_when_model_has_no_params() {
        let env = prompt_env(&sample_prompt_job());
        assert!(!env.contains_key("TOOLSET_PARAMS"));
    }

    #[test]
    fn prompt_job_base_url_uses_provider_or_canonical_default() {
        let mut provider = sample_provider_spec();
        provider.base_url = Some("https://custom.example.com/v1".into());
        let job = prompt_job_with(&sample_model_spec(), &provider, "default", &no_scheduling());
        assert_eq!(
            prompt_env(&job)["TOOLSET_BASE_URL"],
            "https://custom.example.com/v1"
        );

        let mut provider = sample_provider_spec();
        provider.base_url = None;
        let job = prompt_job_with(&sample_model_spec(), &provider, "default", &no_scheduling());
        assert_eq!(
            prompt_env(&job)["TOOLSET_BASE_URL"],
            "https://api.anthropic.com/v1"
        );
    }

    #[test]
    fn canonical_base_url_returns_format_specific_endpoint() {
        assert_eq!(
            canonical_base_url("anthropic"),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(canonical_base_url("openai"), "https://api.openai.com/v1");
        assert_eq!(
            canonical_base_url("gemini"),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(canonical_base_url("unknown"), "");
    }

    #[test]
    fn prompt_job_populates_scrub_secrets() {
        let env = prompt_env(&sample_prompt_job());
        let raw = env
            .get("TOOLSET_SCRUB_SECRETS")
            .expect("TOOLSET_SCRUB_SECRETS must be set on the prompt job pod");
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        let entries = parsed.as_array().expect("registry must be a JSON array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "anthropic-key");
        assert_eq!(entries[0]["file"], "/run/secrets/toolset/api-key");
    }

    fn projected_secret_item(job: &Job) -> KeyToPath {
        let pod_spec = job.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        let volumes = pod_spec.volumes.as_ref().unwrap();
        let projected = volumes[0].projected.as_ref().unwrap();
        let sources = projected.sources.as_ref().unwrap();
        let secret_proj = sources[0].secret.as_ref().unwrap();
        secret_proj.items.as_ref().unwrap()[0].clone()
    }

    #[test]
    fn prompt_job_secret_mount_uses_projected_volume_with_key_to_path() {
        let job = sample_prompt_job();
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let volume = &pod_spec.volumes.as_ref().unwrap()[0];
        let projected = volume.projected.as_ref().unwrap();
        let sources = projected.sources.as_ref().unwrap();
        let secret_proj = sources[0].secret.as_ref().unwrap();
        assert_eq!(secret_proj.name, "anthropic-key");
        let item = &secret_proj.items.as_ref().unwrap()[0];
        assert_eq!(item.path, "api-key");
        assert_eq!(item.mode, Some(0o440));
        assert_eq!(projected.default_mode, Some(0o440));
    }

    #[test]
    fn prompt_job_no_api_key_env_var() {
        let env = prompt_env(&sample_prompt_job());
        assert!(!env.contains_key("API_KEY"));
    }

    #[test]
    fn prompt_job_secret_mount_path_is_stable() {
        let job = sample_prompt_job();
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let mount = &pod_spec.containers[0].volume_mounts.as_ref().unwrap()[0];
        assert_eq!(mount.mount_path, "/run/secrets/toolset");
        assert_eq!(mount.read_only, Some(true));
    }

    #[test]
    fn prompt_job_secret_key_defaults_to_api_key() {
        let mut provider = sample_provider_spec();
        provider.secret.key = None;
        let job = prompt_job_with(&sample_model_spec(), &provider, "default", &no_scheduling());
        assert_eq!(projected_secret_item(&job).key, "api-key");
    }

    #[test]
    fn prompt_job_secret_key_explicit_used_when_set() {
        let mut provider = sample_provider_spec();
        provider.secret.key = Some("custom-key".into());
        let job = prompt_job_with(&sample_model_spec(), &provider, "default", &no_scheduling());
        assert_eq!(projected_secret_item(&job).key, "custom-key");
    }

    #[test]
    fn no_api_key_in_prompt_job_spec() {
        let json = serde_json::to_string(&sample_prompt_job()).unwrap();
        assert!(
            !json.contains("sk-ant"),
            "API key must never appear in Job spec"
        );
    }

    #[test]
    fn prompt_job_runs_as_workspace_sa() {
        let job = prompt_job_with(
            &sample_model_spec(),
            &sample_provider_spec(),
            "my-workspace",
            &no_scheduling(),
        );
        assert_eq!(
            job.spec
                .unwrap()
                .template
                .spec
                .unwrap()
                .service_account_name
                .as_deref(),
            Some("sa-my-workspace")
        );
    }

    #[test]
    fn prompt_job_disables_kubelet_default_sa_token_mount() {
        let pod_spec = sample_prompt_job().spec.unwrap().template.spec.unwrap();
        assert_eq!(pod_spec.automount_service_account_token, Some(false));
    }

    #[test]
    fn prompt_job_mounts_toolset_audience_projected_token() {
        let pod_spec = sample_prompt_job().spec.unwrap().template.spec.unwrap();
        let auth_vol = pod_spec
            .volumes
            .as_ref()
            .and_then(|vs| vs.iter().find(|v| v.name == "prompt-job-auth"))
            .expect("prompt-job-auth volume must be present");
        let sources = auth_vol
            .projected
            .as_ref()
            .and_then(|p| p.sources.as_ref())
            .expect("projected sources");
        let sat = sources[0]
            .service_account_token
            .as_ref()
            .expect("serviceAccountToken source");
        assert_eq!(
            sat.audience.as_deref(),
            Some(shared::auth::TOOLSET_TOOLSET_AUDIENCE),
            "prompt job token audience must be the toolset audience"
        );
        assert_eq!(sat.path, "token");

        let container = &pod_spec.containers[0];
        let mounts = container.volume_mounts.as_ref().unwrap();
        let auth_mount = mounts.iter().find(|m| m.name == "prompt-job-auth").unwrap();
        assert_eq!(
            auth_mount.mount_path,
            "/var/run/secrets/kubernetes.io/serviceaccount"
        );
        assert_eq!(auth_mount.read_only, Some(true));
    }

    #[test]
    fn prompt_job_auth_volume_distinct_from_secret_volume() {
        let pod_spec = sample_prompt_job().spec.unwrap().template.spec.unwrap();
        let volume_names: Vec<&str> = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(volume_names.contains(&"toolset-secret"));
        assert!(volume_names.contains(&"prompt-job-auth"));
        let secret_vol = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "toolset-secret")
            .unwrap();
        let secret_sources = secret_vol
            .projected
            .as_ref()
            .unwrap()
            .sources
            .as_ref()
            .unwrap();
        assert!(
            secret_sources
                .iter()
                .all(|s| s.service_account_token.is_none()),
            "toolset-secret projection must not carry a serviceAccountToken"
        );
    }

    #[test]
    fn prompt_job_has_hardened_security_context() {
        let job = sample_prompt_job();
        let ps = job.spec.unwrap().template.spec.unwrap();
        let sc = ps.containers[0].security_context.as_ref().unwrap();
        assert_eq!(sc.run_as_non_root, Some(true));
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
    }

    #[test]
    fn prompt_job_pod_security_context_has_fs_group() {
        let job = sample_prompt_job();
        let psc = job
            .spec
            .unwrap()
            .template
            .spec
            .unwrap()
            .security_context
            .expect("pod security context must be set");
        assert_eq!(psc.fs_group, Some(1000));
    }

    #[test]
    fn prompt_job_has_scheduling_constraints() {
        let sched = test_scheduling("hangar");
        let job = prompt_job_with(
            &sample_model_spec(),
            &sample_provider_spec(),
            "default",
            &sched,
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        assert_scheduling(&ps, "hangar");
    }

    #[test]
    fn prompt_job_no_scheduling_when_empty() {
        let job = sample_prompt_job();
        let ps = job.spec.unwrap().template.spec.unwrap();
        assert!(ps.node_selector.is_none());
        assert!(ps.tolerations.is_none());
    }
}
