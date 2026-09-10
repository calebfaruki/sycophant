use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Affinity, Container, ContainerPort, EmptyDirVolumeSource, EnvVar, KeyToPath, PodAffinity,
    PodAffinityTerm, PodSecurityContext, PodSpec, PodTemplateSpec, Probe, SecretVolumeSource,
    TCPSocketAction, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use shared::hardened_security_context;
use shared::scheduling::SchedulingConfig;
use shared::toolset::{
    tool_name_to_k8s_segment, CapabilityGrant, ToolsetEntry, WORKSPACE_MOUNT_PATH,
};

/// Writable credential mount root the runtime copies the resolved grant into,
/// under the pod's read-only root filesystem.
const GRANT_MOUNT_PATH: &str = "/run/secrets/grant";

/// Default credential target under `GRANT_MOUNT_PATH`, used when a grant names
/// no path of its own.
const GRANT_CREDENTIAL_PATH: &str = "/run/secrets/grant/credential";

/// Framework default runtime bound for a tool job, applied when an entry sets no
/// `deadlineSeconds` override. Caps a wedged tool pod so it cannot outlive its
/// token.
const TOOL_JOB_DEFAULT_DEADLINE_SECONDS: i64 = 3600;

/// Which Secret a value came from and where it lands. Names and paths only; the
/// harness never reads a secret value.
struct SecretMapping {
    secret: String,
    file: String,
}

/// Workspace-label mutual `podAffinity` keyed on
/// `sycophant.md/workspace=<ws>` with hostname topology. Co-locates a
/// tool Job's pod with the workspace's harness pod so kubelet can attach the
/// shared workspace PVC on the same node.
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

/// Append one env var per `env` key. `image` and `keepalive` are separate
/// entry attributes, so they never reach this loop.
fn push_forwarded_env(env_vars: &mut Vec<EnvVar>, entry: &ToolsetEntry) {
    for (name, value) in entry.forwarded_env() {
        env_vars.push(EnvVar {
            name,
            value: Some(value),
            ..Default::default()
        });
    }
}

/// The file name a Secret's single data key is projected under, taken from the
/// target path so the mount can `subPath` it.
fn secret_basename(file_path: &str, secret_name: &str) -> String {
    std::path::Path::new(file_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(secret_name)
        .to_string()
}

/// One Secret-backed volume projecting exactly the data key named for the
/// Secret itself. Never the whole Secret as a directory.
fn secret_volume(vol_name: &str, secret_name: &str, basename: &str) -> Volume {
    Volume {
        name: vol_name.to_string(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(secret_name.to_string()),
            items: Some(vec![KeyToPath {
                key: secret_name.to_string(),
                path: basename.to_string(),
                ..Default::default()
            }]),
            default_mode: Some(0o440),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The in-pod scrub registry: which Secret each value came from and where it
/// landed, so the job redacts it from logs and chunks. Names and paths only.
fn scrub_secrets_env(secrets: &[SecretMapping]) -> Option<EnvVar> {
    if secrets.is_empty() {
        return None;
    }
    let entries: Vec<serde_json::Value> = secrets
        .iter()
        .map(|secret| serde_json::json!({"name": secret.secret, "file": secret.file}))
        .collect();
    Some(EnvVar {
        name: "TOOLSET_SCRUB_SECRETS".to_string(),
        value: Some(serde_json::to_string(&entries).expect("scrub registry serializes")),
        ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_tool_job(
    tool_name: &str,
    toolset_name: &str,
    entry: &ToolsetEntry,
    call_id: &str,
    namespace: &str,
    service_name: &str,
    workspace_name: &str,
    workspace_pvc: &str,
    scheduling: &SchedulingConfig,
    grant: Option<(&str, &CapabilityGrant)>,
) -> Job {
    let job_name = format!(
        "tool-{}-{}",
        tool_name_to_k8s_segment(tool_name),
        &call_id[..8]
    );
    let image = entry.image.clone().unwrap_or_default();
    let keepalive = entry.keepalive;

    let mut env_vars = vec![EnvVar {
        name: "TOOLSET_TOOL_NAME".to_string(),
        value: Some(tool_name.to_string()),
        ..Default::default()
    }];

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

    // The convention credential target sits under a read-only root filesystem,
    // so the runtime needs a writable mount to copy into.
    volumes.push(Volume {
        name: "grant".to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    });
    volume_mounts.push(VolumeMount {
        name: "grant".to_string(),
        mount_path: GRANT_MOUNT_PATH.to_string(),
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

    push_forwarded_env(&mut env_vars, entry);

    // The resolved grant is the pod's only credential. Its Secret stages
    // read-only under /tmp and the runtime copies it to the target, because
    // neither Secret file mode a direct mount can produce is both owned by the
    // runtime user and unreadable to its group.
    if let Some((_, grant)) = grant {
        let target_path = grant
            .path
            .clone()
            .unwrap_or_else(|| GRANT_CREDENTIAL_PATH.to_string());
        let vol_name = "grant-credential".to_string();
        let basename = secret_basename(&target_path, &grant.secret);
        let staging_path = format!("/tmp/credentials/{vol_name}/{basename}");

        volumes.push(secret_volume(&vol_name, &grant.secret, &basename));
        volume_mounts.push(VolumeMount {
            name: vol_name,
            mount_path: staging_path.clone(),
            sub_path: Some(basename),
            read_only: Some(true),
            ..Default::default()
        });
        env_vars.push(EnvVar {
            name: "TOOLSET_CREDENTIAL_MAP".to_string(),
            value: Some(
                serde_json::json!([{"staging": staging_path, "target": target_path}]).to_string(),
            ),
            ..Default::default()
        });

        // The credential the pod holds must not leave it in tool output, gRPC
        // chunks, or log lines. The registry names the target the runtime reads
        // the value back from, not the staging copy.
        if let Some(scrub) = scrub_secrets_env(&[SecretMapping {
            secret: grant.secret.clone(),
            file: target_path,
        }]) {
            env_vars.push(scrub);
        }
    }

    // Custom-audience projected SA token mounted at the kubelet-default path so
    // the capability-job pod presents a toolset-audience token instead of the
    // namespace default SA token. automountServiceAccountToken=false (below)
    // suppresses the kubelet default; the pod VAP requires that.
    let (auth_volume, auth_mount) =
        shared::podspec::sa_token_volume("tool-job-auth", shared::auth::TOOL_TOOLSET_AUDIENCE);
    volumes.push(auth_volume);
    volume_mounts.push(auth_mount);

    let container = Container {
        name: "runtime".to_string(),
        image: Some(image),
        env: Some(env_vars),
        volume_mounts: Some(volume_mounts),
        security_context: Some(hardened_security_context()),
        ports: Some(vec![ContainerPort {
            container_port: crate::TOOL_JOB_PORT as i32,
            name: Some("tool-job".to_string()),
            ..Default::default()
        }]),
        // The headless Service publishes the pod's per-pod A record only once the
        // pod is Ready, so the harness dials nothing until the server is listening.
        readiness_probe: Some(Probe {
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::Int(crate::TOOL_JOB_PORT as i32),
                ..Default::default()
            }),
            ..Default::default()
        }),
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
        "capability-job".to_string(),
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
    // The per-grant network policy selects this label together with the
    // workspace and toolset labels. Only a pod holding a credential carries it.
    if let Some((grant_name, _)) = grant {
        pod_labels.insert("sycophant.md/grant".to_string(), grant_name.to_string());
    }

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
            active_deadline_seconds: Some(
                entry
                    .deadline_seconds
                    .map(|s| s as i64)
                    .unwrap_or(TOOL_JOB_DEFAULT_DEADLINE_SECONDS),
            ),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    // The per-pod DNS record the harness dials:
                    // `<call-id>.<service>.<ns>.svc.cluster.local`. The hostname is
                    // the call id and the subdomain is the workspace headless
                    // Service, so the record resolves to this pod alone.
                    hostname: Some(call_id.to_string()),
                    subdomain: Some(service_name.to_string()),
                    restart_policy: Some(if keepalive {
                        "OnFailure".to_string()
                    } else {
                        "Never".to_string()
                    }),
                    // runtimeClassName stamped by Kyverno mutate at admission
                    // (from the capability-job component label). Run as the workspace
                    // unprivileged SA so the pod presents the toolset-audience
                    // projected token, not the namespace default SA token, and
                    // carries no RBAC of its own.
                    service_account_name: Some(format!("unprivileged-{workspace_name}")),
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

/// Writable credential mount root the inference runtime copies the provider
/// secret into, under the pod's read-only root filesystem.
const PROVIDER_MOUNT_PATH: &str = "/run/secrets/provider";

/// Default provider-credential target under `PROVIDER_MOUNT_PATH`.
const PROVIDER_CREDENTIAL_PATH: &str = "/run/secrets/provider/credential";

/// Framework runtime bound for an inference job. Caps a wedged model call so the
/// pod cannot outlive its purpose.
const INFERENCE_JOB_DEADLINE_SECONDS: i64 = 3600;

/// Build a per-call inference-runtime Job the harness dials. Mirrors
/// [`build_tool_job`]: the pod's `hostname` is the call id and its `subdomain`
/// is the per-workspace headless Service, so the harness dials it at
/// `<call-id>.<service>.<namespace>.svc.cluster.local` once the pod is Ready.
/// The pod holds the provider credential and the external egress; the harness
/// dials it and streams the model events back. The pod carries the
/// `sycophant.md/job-kind: inference` label the Job CREATE gate admits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_inference_job(
    call_id: &str,
    namespace: &str,
    service_name: &str,
    workspace_name: &str,
    model_key: &str,
    scheduling: &SchedulingConfig,
    image: &str,
    format: &str,
    model: &str,
    base_url: &str,
    secret: Option<&str>,
) -> Job {
    let job_name = format!("inference-{}", &call_id[..8]);

    let mut env_vars = vec![
        EnvVar {
            name: "INFERENCE_BASE_URL".to_string(),
            value: Some(base_url.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "INFERENCE_FORMAT".to_string(),
            value: Some(format.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "INFERENCE_MODEL".to_string(),
            value: Some(model.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "HOME".to_string(),
            value: Some("/home/agent".to_string()),
            ..Default::default()
        },
    ];

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

    // The provider credential lands under a read-only root filesystem, so the
    // runtime needs a writable mount to copy the staged Secret into.
    volumes.push(Volume {
        name: "provider".to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    });
    volume_mounts.push(VolumeMount {
        name: "provider".to_string(),
        mount_path: PROVIDER_MOUNT_PATH.to_string(),
        ..Default::default()
    });

    // The provider Secret is the pod's only credential. It stages read-only
    // under /tmp and the runtime copies it to the target, mirroring the tool
    // job's grant staging. A destination that needs no credential names none.
    if let Some(secret) = secret {
        let target_path = PROVIDER_CREDENTIAL_PATH.to_string();
        let vol_name = "provider-credential".to_string();
        let basename = secret_basename(&target_path, secret);
        let staging_path = format!("/tmp/credentials/{vol_name}/{basename}");

        volumes.push(secret_volume(&vol_name, secret, &basename));
        volume_mounts.push(VolumeMount {
            name: vol_name,
            mount_path: staging_path.clone(),
            sub_path: Some(basename),
            read_only: Some(true),
            ..Default::default()
        });
        env_vars.push(EnvVar {
            name: "TOOLSET_CREDENTIAL_MAP".to_string(),
            value: Some(
                serde_json::json!([{"staging": staging_path, "target": target_path}]).to_string(),
            ),
            ..Default::default()
        });
        env_vars.push(EnvVar {
            name: "INFERENCE_API_KEY_FILE".to_string(),
            value: Some(target_path.clone()),
            ..Default::default()
        });
        if let Some(scrub) = scrub_secrets_env(&[SecretMapping {
            secret: secret.to_string(),
            file: target_path,
        }]) {
            env_vars.push(scrub);
        }
    }

    let container = Container {
        name: "runtime".to_string(),
        image: Some(image.to_string()),
        env: Some(env_vars),
        volume_mounts: Some(volume_mounts),
        security_context: Some(hardened_security_context()),
        ports: Some(vec![ContainerPort {
            container_port: crate::TOOL_JOB_PORT as i32,
            name: Some("inference".to_string()),
            ..Default::default()
        }]),
        // The headless Service publishes the pod's per-pod A record only once the
        // pod is Ready, so the harness dials nothing until the server is listening.
        readiness_probe: Some(Probe {
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::Int(crate::TOOL_JOB_PORT as i32),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    labels.insert("sycophant.md/call-id".to_string(), call_id.to_string());
    labels.insert(
        "sycophant.md/workspace".to_string(),
        workspace_name.to_string(),
    );

    let mut pod_labels = BTreeMap::new();
    pod_labels.insert(
        "app.kubernetes.io/component".to_string(),
        "capability-job".to_string(),
    );
    pod_labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    pod_labels.insert(
        "sycophant.md/workspace".to_string(),
        workspace_name.to_string(),
    );
    // The Job CREATE gate admits `job-kind: inference`; the label is what routes
    // this Job into the inference arm of the allowlist.
    pod_labels.insert("sycophant.md/job-kind".to_string(), "inference".to_string());
    // The resolver KEY (the `.Values.model` map key), never the provider model
    // string. The per-model egress policy selects the pod by this label, so a
    // pod carrying no model label matches no provider hole and stays on the
    // fail-closed baseline floor.
    pod_labels.insert("sycophant.md/model".to_string(), model_key.to_string());

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
            active_deadline_seconds: Some(INFERENCE_JOB_DEADLINE_SECONDS),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(pod_labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    // The per-pod DNS record the harness dials:
                    // `<call-id>.<service>.<ns>.svc.cluster.local`.
                    hostname: Some(call_id.to_string()),
                    subdomain: Some(service_name.to_string()),
                    restart_policy: Some("Never".to_string()),
                    // runtimeClassName stamped by Kyverno mutate at admission.
                    // Run as the workspace unprivileged SA the gate pins; the pod
                    // authenticates no outbound sycophant call, so it mounts no
                    // projected token.
                    service_account_name: Some(format!("unprivileged-{workspace_name}")),
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

/// Create a tool Job in `namespace`. The harness's Role grants `create` on
/// `batch/jobs` and nothing else; the Job is reaped by its own
/// `ttlSecondsAfterFinished`, never deleted by the harness.
pub(crate) async fn create_job(
    client: &kube::Client,
    namespace: &str,
    job: &Job,
) -> Result<Job, kube::Error> {
    let jobs: kube::Api<Job> = kube::Api::namespaced(client.clone(), namespace);
    jobs.create(&kube::api::PostParams::default(), job).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CALL_ID: &str = "abcdef12-0000-0000-0000-000000000000";

    fn pod_spec(job: &Job) -> &PodSpec {
        job.spec.as_ref().unwrap().template.spec.as_ref().unwrap()
    }

    fn test_job() -> Job {
        let entry = ToolsetEntry {
            image: Some("ghcr.io/test/toolset:latest".into()),
            ..Default::default()
        };
        build_tool_job(
            "git-push",
            "test-toolset",
            &entry,
            TEST_CALL_ID,
            "test-ns",
            "capability-test",
            "test",
            "workspace-data-test",
            &SchedulingConfig::default(),
            None,
        )
    }

    /// The harness-created capability-job pod runs as `unprivileged-<workspace>`, the
    /// zero-RBAC run identity the gate pins. The controller copy keeps
    /// `sa-<workspace>` for its coexisting path.
    #[test]
    fn harness_tool_job_runs_as_unprivileged_workspace_sa() {
        let job = test_job();
        assert_eq!(
            pod_spec(&job).service_account_name.as_deref(),
            Some("unprivileged-test"),
        );
    }

    /// The harness stamps the pod's per-pod DNS coordinates so
    /// the workspace headless Service publishes `<call-id>.<service>`. The
    /// hostname is the call id and the subdomain is the headless Service name
    /// the harness passes in (and later dials). A build that leaves either
    /// unset publishes no per-pod A record, so the harness has nothing to dial.
    #[test]
    fn tool_job_pod_carries_per_pod_dns_hostname_and_subdomain() {
        let entry = ToolsetEntry {
            image: Some("ghcr.io/test/toolset:latest".into()),
            ..Default::default()
        };
        let job = build_tool_job(
            "git-push",
            "test-toolset",
            &entry,
            TEST_CALL_ID,
            "test-ns",
            // The former dispatch-addr slot now carries the headless Service.
            "capability-test",
            "test",
            "workspace-data-test",
            &SchedulingConfig::default(),
            None,
        );
        let spec = pod_spec(&job);
        assert_eq!(
            spec.hostname.as_deref(),
            Some(TEST_CALL_ID),
            "the pod hostname must be the call id so the headless Service names its per-pod record",
        );
        assert_eq!(
            spec.subdomain.as_deref(),
            Some("capability-test"),
            "the pod subdomain must be the workspace headless Service",
        );
    }

    #[test]
    fn harness_tool_job_is_reaped_by_ttl_not_delete() {
        let job = test_job();
        let spec = job.spec.as_ref().unwrap();
        assert_eq!(spec.ttl_seconds_after_finished, Some(30));
        assert_eq!(
            spec.active_deadline_seconds,
            Some(TOOL_JOB_DEFAULT_DEADLINE_SECONDS),
        );
    }

    fn test_inference_job(secret: Option<&str>) -> Job {
        build_inference_job(
            TEST_CALL_ID,
            "test-ns",
            "capability-test",
            "test",
            // The resolver KEY (the `.Values.model` map key), deliberately
            // distinct from the provider model string below so a build that
            // confuses the two is caught.
            "deepseek-v4-flash",
            &SchedulingConfig::default(),
            "ghcr.io/sycophant/inference-runtime:1",
            "openai",
            "deepseek/deepseek-v4-flash",
            "https://openrouter.ai/api/v1",
            secret,
        )
    }

    fn container_env<'a>(job: &'a Job, name: &str) -> Option<&'a str> {
        pod_spec(job).containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.value.as_deref())
    }

    /// The inference-runtime pod stamps the same per-pod DNS coordinates the
    /// tool job does, so the workspace headless Service publishes
    /// `<call-id>.<service>` and the harness dials it. A build that leaves
    /// either unset publishes no per-pod A record.
    #[test]
    fn inference_job_pod_carries_per_pod_dns_hostname_and_subdomain() {
        let job = test_inference_job(Some("sycophant-llm-openrouter"));
        let spec = pod_spec(&job);
        assert_eq!(
            spec.hostname.as_deref(),
            Some(TEST_CALL_ID),
            "the pod hostname must be the call id so the headless Service names its per-pod record",
        );
        assert_eq!(
            spec.subdomain.as_deref(),
            Some("capability-test"),
            "the pod subdomain must be the workspace headless Service",
        );
    }

    /// The pod carries `job-kind: inference`, the value the Job CREATE gate
    /// admits into the inference arm. A build that omits it or stamps another
    /// value is refused at admission.
    #[test]
    fn inference_job_pod_carries_the_inference_job_kind_label() {
        let job = test_inference_job(None);
        let labels = pod_spec_labels(&job);
        assert_eq!(
            labels.get("sycophant.md/job-kind").map(String::as_str),
            Some("inference"),
        );
        assert_eq!(
            labels
                .get("app.kubernetes.io/component")
                .map(String::as_str),
            Some("capability-job"),
        );
    }

    /// The pod carries `sycophant.md/model` set to the resolver KEY — the
    /// `.Values.model` map key the harness dispatched, never the provider model
    /// string. The per-model egress policy selects the inference pod by exactly
    /// this label, so a build that stamps the provider model id here, or omits
    /// the label, matches no provider-egress rule and leaves the job on the
    /// fail-closed baseline floor.
    #[test]
    fn inference_job_pod_carries_the_model_key_label() {
        let job = test_inference_job(None);
        let labels = pod_spec_labels(&job);
        assert_eq!(
            labels.get("sycophant.md/model").map(String::as_str),
            Some("deepseek-v4-flash"),
            "the model label must be the resolver key the egress policy selects on",
        );
        // The key is not the provider model id. Stamping `config.model()` here
        // would label the pod for a model no inference-egress CNP is named for.
        assert_ne!(
            labels.get("sycophant.md/model").map(String::as_str),
            Some("deepseek/deepseek-v4-flash"),
            "the model label must be the map key, never the provider model id",
        );
    }

    /// The pod runs as the workspace's zero-RBAC unprivileged SA and mounts no
    /// projected token: it serves the harness and authenticates no outbound
    /// sycophant call.
    #[test]
    fn inference_job_runs_as_unprivileged_workspace_sa_with_no_token() {
        let spec = pod_spec(&test_inference_job(None)).clone();
        assert_eq!(
            spec.service_account_name.as_deref(),
            Some("unprivileged-test"),
        );
        assert_eq!(spec.automount_service_account_token, Some(false));
    }

    /// The named provider Secret is mounted by reference and staged for the
    /// runtime to copy; the credential map and key-file env point the runtime
    /// at it. A build that names the wrong Secret would fail the Job gate's
    /// allowlist.
    #[test]
    fn inference_job_stages_the_named_provider_secret() {
        let job = test_inference_job(Some("sycophant-llm-openrouter"));
        let spec = pod_spec(&job);
        let mounts_secret = spec.volumes.as_ref().unwrap().iter().any(|v| {
            v.secret
                .as_ref()
                .and_then(|s| s.secret_name.as_deref())
                .map(|n| n == "sycophant-llm-openrouter")
                .unwrap_or(false)
        });
        assert!(mounts_secret, "the named provider Secret must be mounted");
        assert_eq!(
            container_env(&job, "INFERENCE_API_KEY_FILE"),
            Some(PROVIDER_CREDENTIAL_PATH),
        );
        assert!(container_env(&job, "TOOLSET_CREDENTIAL_MAP").is_some());
        assert!(container_env(&job, "TOOLSET_SCRUB_SECRETS").is_some());
    }

    /// A destination that needs no credential mounts no Secret and sets no
    /// credential env, mirroring a remote in-cluster target.
    #[test]
    fn inference_job_mounts_no_secret_when_none_is_named() {
        let job = test_inference_job(None);
        let spec = pod_spec(&job);
        let has_secret = spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .any(|v| v.secret.is_some());
        assert!(!has_secret, "no Secret volume when the config names none");
        assert_eq!(container_env(&job, "INFERENCE_API_KEY_FILE"), None);
        assert_eq!(container_env(&job, "TOOLSET_CREDENTIAL_MAP"), None);
    }

    /// The provider wire coordinates ride the pod as env the runtime reads to
    /// build its provider call.
    #[test]
    fn inference_job_carries_the_provider_wire_coordinates() {
        let job = test_inference_job(None);
        assert_eq!(
            container_env(&job, "INFERENCE_BASE_URL"),
            Some("https://openrouter.ai/api/v1"),
        );
        assert_eq!(container_env(&job, "INFERENCE_FORMAT"), Some("openai"));
        assert_eq!(
            container_env(&job, "INFERENCE_MODEL"),
            Some("deepseek/deepseek-v4-flash"),
        );
    }

    fn pod_spec_labels(job: &Job) -> &BTreeMap<String, String> {
        job.spec
            .as_ref()
            .unwrap()
            .template
            .metadata
            .as_ref()
            .unwrap()
            .labels
            .as_ref()
            .unwrap()
    }
}
