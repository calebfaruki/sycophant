use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Affinity, Container, EmptyDirVolumeSource, EnvVar, KeyToPath, PodAffinity, PodAffinityTerm,
    PodDNSConfig, PodDNSConfigOption, PodSecurityContext, PodSpec, PodTemplateSpec,
    SecretVolumeSource, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use kube::api::PostParams;
use kube::{Api, Client};

use crate::config::{SecretMapping, ToolsetEntry};
use crate::registry::tool_name_to_k8s_segment;
use crate::state::CapabilityGrant;
use crate::{GRANT_CREDENTIAL_PATH, GRANT_MOUNT_PATH, WORKSPACE_MOUNT_PATH};
use shared::hardened_security_context;
use shared::scheduling::SchedulingConfig;

/// Framework default runtime bound for a tool job, applied when an entry sets no
/// `deadlineSeconds` override. Caps a wedged tool pod so it cannot outlive its
/// token.
const TOOL_JOB_DEFAULT_DEADLINE_SECONDS: i64 = 3600;

/// Workspace-label mutual `podAffinity` keyed on
/// `sycophant.md/workspace=<ws>` with hostname topology. Co-locates a
/// tool Job's pod with the workspace's harness pod (which carries the
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
// Tool Job (tool dispatch over the toolset config)
// =========================================================================

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
pub fn build_tool_job(
    tool_name: &str,
    toolset_name: &str,
    entry: &ToolsetEntry,
    call_id: &str,
    namespace: &str,
    controller_addr: &str,
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
    // the tool-job pod presents a toolset-audience token instead of the
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
                    // (from the capability-job component label). Run as the
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
/// CNP selects it alone, never the shared `capability-job` floor.
const DISCOVERY_JOB_LABEL: &str = "discovery";

/// Build the ephemeral discovery Job for a Toolset. It runs the controller's
/// own image under the `discover` subcommand, reads the `md.sycophant.tools`
/// label off `toolset_image`, and reports the tool set back over
/// `ReportDiscoveredTools`. Gated as a `capability-job` (so Kyverno stamps gVisor
/// and the baseline CNP applies) and additionally labelled
/// `sycophant.md/job: discovery` so the discovery registry-egress CNP selects
/// it without widening any capability-job pod. Runtime class is NOT set here —
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

    // Tool-job-audience projected SA token so the report authenticates on the
    // tool-job method set; automount=false suppresses the kubelet default.
    let (auth_volume, auth_mount) =
        shared::podspec::sa_token_volume("discovery-job-auth", shared::auth::TOOL_TOOLSET_AUDIENCE);

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
        "capability-job".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretMapping;
    use shared::scheduling::testing::{assert_scheduling, no_scheduling, test_scheduling};

    // ---- Tool Job tests ----

    const TEST_CALL_ID: &str = "abcdef12-0000-0000-0000-000000000000";
    const TEST_IMAGE: &str = "ghcr.io/test/toolset-git:latest";
    const TEST_TOOLSET: &str = "test-toolset";
    const TEST_WORKSPACE: &str = "test";
    const TEST_WORKSPACE_PVC: &str = "workspace-data-test";

    fn base_entry() -> ToolsetEntry {
        ToolsetEntry {
            image: Some(TEST_IMAGE.into()),
            ..Default::default()
        }
    }

    fn test_job(entry: &ToolsetEntry) -> Job {
        build_tool_job(
            "git-push",
            TEST_TOOLSET,
            entry,
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &no_scheduling(),
            None,
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
        let job = test_job(&base_entry());
        assert_eq!(pod_spec(&job).runtime_class_name, None);
    }

    #[test]
    fn tool_job_has_workspace_label_affinity() {
        let job = test_job(&base_entry());
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
        let job = test_job(&base_entry());
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
        let job = test_job(&base_entry());

        assert_eq!(job.metadata.name.as_deref(), Some("tool-git-push-abcdef12"));
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
            TEST_TOOLSET,
            &base_entry(),
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &no_scheduling(),
            None,
        );
        assert_eq!(
            job.metadata.name.as_deref(),
            Some("tool-read-file-abcdef12"),
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
        let job = test_job(&base_entry());
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
        // The chart's capability-job-baseline CNP selects on these labels — the
        // fail-closed egress floor for every toolset pod depends on them.
        assert_eq!(pod_labels["app.kubernetes.io/component"], "capability-job");
        assert_eq!(pod_labels["app.kubernetes.io/part-of"], "sycophant");
    }

    #[test]
    fn tool_job_has_correct_env_vars() {
        let job = test_job(&base_entry());
        let env = env_map(&job);

        assert_eq!(env["TOOLSET_CONTROLLER_ADDR"], "http://controller:9090");
        assert_eq!(env["TOOLSET_JOB_ID"], TEST_CALL_ID);
        assert_eq!(env["TOOLSET_TOOL_NAME"], "git-push");
        assert!(!env.contains_key("TOOLSET_KEEPALIVE"));
    }

    #[test]
    fn keepalive_tool_job_has_env_and_restart_policy() {
        let mut entry = base_entry();
        entry.keepalive = true;
        let job = test_job(&entry);
        let env = env_map(&job);

        assert_eq!(env.get("TOOLSET_KEEPALIVE"), Some(&"true"));
        assert_eq!(pod_spec(&job).restart_policy.as_deref(), Some("OnFailure"));
    }

    #[test]
    fn fire_and_forget_restart_policy() {
        let job = test_job(&base_entry());
        assert_eq!(pod_spec(&job).restart_policy.as_deref(), Some("Never"));
        assert_eq!(job.spec.as_ref().unwrap().backoff_limit, Some(0));
    }

    #[test]
    fn workspace_pvc_mounted_rw_at_workspace() {
        let job = test_job(&base_entry());
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

    /// The staging mount projects one file, never the Secret as a directory.
    /// Without `subPath` the runtime finds a directory where it expects the
    /// credential and the copy fails.
    #[test]
    fn the_grant_staging_mount_projects_one_file_by_sub_path() {
        let grant = CapabilityGrant {
            secret: "ws-notion-reader".to_string(),
            path: None,
            egress: None,
        };
        let job = build_tool_job(
            "git-push",
            TEST_TOOLSET,
            &base_entry(),
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &no_scheduling(),
            Some(("reader", &grant)),
        );

        let mounts = container(&job).volume_mounts.as_ref().unwrap();
        let staging = mounts
            .iter()
            .find(|m| m.name == "grant-credential")
            .expect("the grant Secret is mounted");
        assert_eq!(
            staging.sub_path.as_deref(),
            Some("credential"),
            "the staging mount names the projected file inside the Secret volume"
        );
        assert!(
            staging.mount_path.ends_with("/credential"),
            "the staging path is the file itself, got {}",
            staging.mount_path
        );
    }

    /// The convention credential target sits under a read-only root filesystem,
    /// so the mount that makes it writable is present on every tool job.
    #[test]
    fn grant_mount_is_a_writable_empty_dir() {
        let job = test_job(&base_entry());
        let volumes = pod_spec(&job).volumes.as_ref().unwrap();
        let vol = volumes
            .iter()
            .find(|v| v.name == "grant")
            .expect("the grant mount is always present");
        assert!(
            vol.empty_dir.is_some(),
            "the grant mount is not Secret-backed"
        );
        assert!(vol.secret.is_none());

        let mounts = container(&job).volume_mounts.as_ref().unwrap();
        let mount = mounts.iter().find(|m| m.name == "grant").unwrap();
        assert_eq!(mount.mount_path, GRANT_MOUNT_PATH);
        assert!(
            !mount.read_only.unwrap_or(false),
            "the runtime copies into it"
        );
    }

    #[test]
    fn ttl_seconds_set() {
        let job = test_job(&base_entry());
        assert_eq!(
            job.spec.as_ref().unwrap().ttl_seconds_after_finished,
            Some(30)
        );
    }

    /// Every tool job bounds its runtime with the framework default deadline so
    /// a wedged tool pod cannot outlive its token. The entry carries no override,
    /// so the default applies. Hardcoded to 3600 so a mutant that changes the
    /// default is caught.
    #[test]
    fn tool_job_sets_default_deadline() {
        let job = test_job(&base_entry());
        assert_eq!(
            job.spec.as_ref().unwrap().active_deadline_seconds,
            Some(3600),
            "a tool job with no deadline override must carry the framework default"
        );
    }

    /// A per-entry `deadline_seconds` overrides the framework default, so an
    /// entry asking for 1800 produces a job bounded at 1800, not 3600. This
    /// proves the override path is wired, not just the default.
    #[test]
    fn tool_job_entry_deadline_overrides_default() {
        let entry = ToolsetEntry {
            image: Some(TEST_IMAGE.into()),
            deadline_seconds: Some(1800),
            ..Default::default()
        };
        let job = test_job(&entry);
        assert_eq!(
            job.spec.as_ref().unwrap().active_deadline_seconds,
            Some(1800),
            "the per-entry override must win over the framework default"
        );
    }

    #[test]
    fn correct_image() {
        let job = test_job(&base_entry());
        assert_eq!(
            container(&job).image.as_deref(),
            Some("ghcr.io/test/toolset-git:latest")
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
        let job = test_job(&base_entry());
        assert!(scrub_env(&job).is_none());
    }

    /// The registry names a file and never an environment variable: every
    /// credential is delivered as a file.
    #[test]
    fn scrub_secrets_env_maps_each_secret_to_its_file() {
        let entry = scrub_secrets_env(&[SecretMapping {
            secret: "ssh-key".to_string(),
            file: "/home/agent/.ssh/id_ed25519".to_string(),
        }])
        .expect("a registered secret produces a registry");
        let json: Vec<serde_json::Value> =
            serde_json::from_str(&entry.value.unwrap()).expect("the registry is a JSON array");
        assert_eq!(json[0]["name"], "ssh-key");
        assert_eq!(json[0]["file"], "/home/agent/.ssh/id_ed25519");
        assert!(json[0].get("env").is_none());
    }

    #[test]
    fn share_process_namespace_disabled() {
        let job = test_job(&base_entry());
        assert_eq!(pod_spec(&job).share_process_namespace, Some(false));
    }

    #[test]
    fn tool_job_has_scheduling_constraints() {
        let sched = test_scheduling("tool");
        let job = build_tool_job(
            "git-push",
            TEST_TOOLSET,
            &base_entry(),
            TEST_CALL_ID,
            "test-ns",
            "http://controller:9090",
            TEST_WORKSPACE,
            TEST_WORKSPACE_PVC,
            &sched,
            None,
        );
        assert_scheduling(pod_spec(&job), "tool");
    }

    #[test]
    fn tool_job_no_scheduling_when_empty() {
        let job = test_job(&base_entry());
        let ps = pod_spec(&job);
        assert!(ps.node_selector.is_none());
        assert!(ps.tolerations.is_none());
    }

    #[test]
    fn tool_job_has_hardened_security_context() {
        let job = test_job(&base_entry());
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
        let job = test_job(&base_entry());
        let psc = pod_spec(&job).security_context.as_ref().unwrap();
        assert_eq!(psc.run_as_non_root, Some(true));
        assert_eq!(psc.run_as_user, Some(1000));
        assert_eq!(psc.fs_group, Some(1000));
    }

    #[test]
    fn tool_job_has_tmp_and_home_mounts() {
        let job = test_job(&base_entry());
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
        let job = test_job(&base_entry());
        let env = env_map(&job);
        assert_eq!(env.get("HOME"), Some(&"/home/agent"));
    }

    #[test]
    fn tool_job_has_container_name() {
        let job = test_job(&base_entry());
        assert_eq!(container(&job).name, "runtime");
    }

    #[test]
    fn tool_job_runs_as_workspace_sa() {
        let job = test_job(&base_entry());
        assert_eq!(
            pod_spec(&job).service_account_name.as_deref(),
            Some("sa-test")
        );
    }

    #[test]
    fn tool_job_disables_kubelet_default_sa_token_mount() {
        let job = test_job(&base_entry());
        assert_eq!(
            pod_spec(&job).automount_service_account_token,
            Some(false),
            "automount=false satisfies the pod VAP and suppresses the kubelet default token"
        );
    }

    #[test]
    fn tool_job_mounts_toolset_audience_projected_token() {
        let job = test_job(&base_entry());
        let ps = pod_spec(&job);
        let auth_vol = ps
            .volumes
            .as_ref()
            .and_then(|vs| vs.iter().find(|v| v.name == "tool-job-auth"))
            .expect("tool-job-auth volume must be present");
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
            Some(shared::auth::TOOL_TOOLSET_AUDIENCE),
        );
        assert_eq!(sat.path, "token");
        assert_eq!(sat.expiration_seconds, Some(3600));

        let mounts = container(&job).volume_mounts.as_ref().unwrap();
        let auth_mount = mounts
            .iter()
            .find(|m| m.name == "tool-job-auth")
            .expect("runtime container must mount tool-job-auth");
        assert_eq!(
            auth_mount.mount_path,
            "/var/run/secrets/kubernetes.io/serviceaccount"
        );
        assert_eq!(auth_mount.read_only, Some(true));
    }
}
