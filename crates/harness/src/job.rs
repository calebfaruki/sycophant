use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Affinity, Container, EmptyDirVolumeSource, EnvVar, KeyToPath, PodAffinity, PodAffinityTerm,
    PodSecurityContext, PodSpec, PodTemplateSpec, SecretVolumeSource, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};

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
    dispatch_addr: &str,
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

    let mut env_vars = vec![
        EnvVar {
            name: "TOOLSET_CONTROLLER_ADDR".to_string(),
            value: Some(dispatch_addr.to_string()),
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
            "http://harness:9090",
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

    /// The tool pod dials the harness back for its call assignment, so its
    /// `TOOLSET_CONTROLLER_ADDR` must be the harness dispatch address passed in,
    /// not the toolset controller's. A build that hardcodes or drops the arg
    /// reds this.
    #[test]
    fn harness_tool_job_dials_back_the_passed_dispatch_addr() {
        let job = test_job();
        let env = pod_spec(&job).containers[0].env.as_ref().unwrap();
        let addr = env
            .iter()
            .find(|e| e.name == "TOOLSET_CONTROLLER_ADDR")
            .and_then(|e| e.value.as_deref());
        assert_eq!(addr, Some("http://harness:9090"));
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
}
