use std::collections::BTreeMap;

use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Affinity, ConfigMapEnvSource, ConfigMapProjection, ConfigMapVolumeSource, Container,
    ContainerPort, DownwardAPIProjection, DownwardAPIVolumeFile, EmptyDirVolumeSource,
    EnvFromSource, EnvVar, KeyToPath, ObjectFieldSelector, PersistentVolumeClaimVolumeSource,
    PodAffinity, PodAffinityTerm, PodSecurityContext, PodSpec, PodTemplateSpec, Probe,
    ProjectedVolumeSource, ResourceRequirements, SeccompProfile, SecretVolumeSource,
    ServiceAccountTokenProjection, TCPSocketAction, Toleration, Volume, VolumeMount,
    VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use shared::hardened_security_context;
use shared::scheduling::SchedulingConfig;
use shared::toolset::{
    tool_name_to_k8s_segment, ToolsetEntry, ToolsetSecret, WORKSPACE_MOUNT_PATH,
};

/// Writable credential mount root under the pod's read-only root filesystem,
/// for a toolset Secret whose target lies beneath it.
const CREDENTIALS_MOUNT_PATH: &str = "/run/secrets/credentials";

/// Framework default runtime bound for a tool job, applied when an entry sets no
/// `deadlineSeconds` override. Caps a wedged tool pod so it cannot outlive its
/// token.
const TOOL_JOB_DEFAULT_DEADLINE_SECONDS: i64 = 3600;

/// Whether the harness stamps `name` on a tool container: `HOME` and the
/// `TOOLSET_` runtime contract. A tool arg mapped to one would overwrite it.
pub(crate) fn is_reserved_env(name: &str) -> bool {
    name == "HOME" || name.starts_with("TOOLSET_")
}

/// Which Secret a value came from and where it lands. Names and paths only; the
/// harness never reads a secret value.
struct SecretMapping {
    secret: String,
    file: String,
}

/// Agent-label mutual `podAffinity` keyed on
/// `sycophant.md/agent=<agent>` with hostname topology. Co-locates a
/// tool Job's pod with the agent's harness pod so kubelet can attach the
/// shared workspace PVC on the same node.
fn agent_affinity(agent_name: &str) -> Affinity {
    let mut match_labels = BTreeMap::new();
    match_labels.insert("sycophant.md/agent".to_string(), agent_name.to_string());
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
    agent_name: &str,
    workspace_pvc: &str,
    scheduling: &SchedulingConfig,
    secrets: &[ToolsetSecret],
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

    // A credential target under the read-only root filesystem needs a
    // writable mount for the runtime to copy into.
    volumes.push(Volume {
        name: "credentials".to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    });
    volume_mounts.push(VolumeMount {
        name: "credentials".to_string(),
        mount_path: CREDENTIALS_MOUNT_PATH.to_string(),
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

    // Every Secret of the toolset is the pod's credentials, in name order.
    // Each stages read-only under /tmp and the runtime copies it to its target,
    // because neither Secret file mode a direct mount can produce is both owned
    // by the runtime user and unreadable to its group.
    let mut secrets: Vec<&ToolsetSecret> = secrets.iter().collect();
    secrets.sort_by(|a, b| a.secret.cmp(&b.secret));
    let mut credential_map = Vec::with_capacity(secrets.len());
    let mut scrub_mappings = Vec::with_capacity(secrets.len());
    for (i, secret) in secrets.into_iter().enumerate() {
        let vol_name = format!("credential-{i}");
        let basename = secret_basename(&secret.path, &secret.secret);
        let staging_path = format!("/tmp/credentials/{vol_name}/{basename}");

        volumes.push(secret_volume(&vol_name, &secret.secret, &basename));
        volume_mounts.push(VolumeMount {
            name: vol_name,
            mount_path: staging_path.clone(),
            sub_path: Some(basename),
            read_only: Some(true),
            ..Default::default()
        });
        credential_map.push(serde_json::json!({"staging": staging_path, "target": secret.path}));
        // The registry names the target the runtime reads the value back
        // from, not the staging copy.
        scrub_mappings.push(SecretMapping {
            secret: secret.secret.clone(),
            file: secret.path.clone(),
        });
    }
    if !credential_map.is_empty() {
        env_vars.push(EnvVar {
            name: "TOOLSET_CREDENTIAL_MAP".to_string(),
            value: Some(serde_json::Value::Array(credential_map).to_string()),
            ..Default::default()
        });
    }
    // The credentials the pod holds must not leave it in tool output, gRPC
    // chunks, or log lines.
    if let Some(scrub) = scrub_secrets_env(&scrub_mappings) {
        env_vars.push(scrub);
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
    labels.insert("sycophant.md/agent".to_string(), agent_name.to_string());

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
    pod_labels.insert("sycophant.md/agent".to_string(), agent_name.to_string());

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
                    // the call id and the subdomain is the agent headless
                    // Service, so the record resolves to this pod alone.
                    hostname: Some(call_id.to_string()),
                    subdomain: Some(service_name.to_string()),
                    restart_policy: Some(if keepalive {
                        "OnFailure".to_string()
                    } else {
                        "Never".to_string()
                    }),
                    // runtimeClassName stamped by Kyverno mutate at admission
                    // (from the capability-job component label). Run as the agent
                    // unprivileged SA so the pod presents the toolset-audience
                    // projected token, not the namespace default SA token, and
                    // carries no RBAC of its own.
                    service_account_name: Some(shared::auth::unprivileged_sa(agent_name)),
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
                    affinity: Some(agent_affinity(agent_name)),
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
/// is the per-agent headless Service, so the harness dials it at
/// `<call-id>.<service>.<namespace>.svc.cluster.local` once the pod is Ready.
/// The pod holds the provider credential and the external egress; the harness
/// dials it and streams the model events back. The pod carries the
/// `sycophant.md/job-kind: inference` label the Job CREATE gate admits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_inference_job(
    call_id: &str,
    namespace: &str,
    service_name: &str,
    agent_name: &str,
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
    // job's credential staging. A destination that needs no credential names none.
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
    labels.insert("sycophant.md/agent".to_string(), agent_name.to_string());

    let mut pod_labels = BTreeMap::new();
    pod_labels.insert(
        "app.kubernetes.io/component".to_string(),
        "capability-job".to_string(),
    );
    pod_labels.insert(
        "app.kubernetes.io/part-of".to_string(),
        "sycophant".to_string(),
    );
    pod_labels.insert("sycophant.md/agent".to_string(), agent_name.to_string());
    // The Job CREATE gate admits `job-kind: inference`; the label is what routes
    // this Job into the inference arm of the allowlist.
    pod_labels.insert("sycophant.md/job-kind".to_string(), "inference".to_string());
    // The model key, never the provider model string. The per-model egress
    // policy selects the pod by this label, so a pod carrying no model label
    // matches no provider hole and stays on the fail-closed baseline floor.
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
                    // Run as the agent unprivileged SA the gate pins; the pod
                    // authenticates no outbound sycophant call, so it mounts no
                    // projected token.
                    service_account_name: Some(shared::auth::unprivileged_sa(agent_name)),
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

/// The Job for sub-agent `sub_agent`'s harness at path `<sub_agent>/<id>`,
/// started by `agent` with `mandate`. Admission stamps its labels and owner
/// and checks every other part of it.
pub(crate) fn build_dispatch_job(
    agent: &str,
    sub_agent: &str,
    id: &str,
    mandate: &str,
    namespace: &str,
    image: &str,
) -> Job {
    let host = format!("{sub_agent}-{id}");
    let instructions_dir = format!("/etc/instructions/{sub_agent}");
    let instructions_volume = format!("instructions-{sub_agent}");
    let env_from = Some(vec![EnvFromSource {
        config_map_ref: Some(ConfigMapEnvSource {
            name: format!("ag-{sub_agent}-harness"),
            optional: Some(false),
        }),
        ..Default::default()
    }]);
    let limits = |cpu: &str, memory: &str| {
        Some(ResourceRequirements {
            limits: Some(BTreeMap::from([
                ("cpu".to_string(), Quantity(cpu.to_string())),
                ("memory".to_string(), Quantity(memory.to_string())),
            ])),
            ..Default::default()
        })
    };
    let mount = |name: &str, path: &str, read_only: bool| VolumeMount {
        name: name.to_string(),
        mount_path: path.to_string(),
        read_only: read_only.then_some(true),
        ..Default::default()
    };
    let (store_token, mut store_token_mount) =
        shared::podspec::sa_token_volume("objectstore-token", "sycophant-objectstore");
    store_token_mount.mount_path = "/var/run/secrets/objectstore".to_string();
    let sync = Container {
        name: "instructions-sync".to_string(),
        image: Some(image.to_string()),
        security_context: Some(hardened_security_context()),
        resources: limits("250m", "128Mi"),
        command: Some(vec!["/bin/rclone".to_string()]),
        args: Some(vec![
            "sync".to_string(),
            "$(INSTRUCTIONS_SOURCE)".to_string(),
            instructions_dir.clone(),
        ]),
        env_from: env_from.clone(),
        volume_mounts: Some(vec![
            mount(&instructions_volume, &instructions_dir, false),
            mount("instructions-sync-scratch", "/tmp", false),
            store_token_mount,
        ]),
        ..Default::default()
    };
    let env = |name: &str, value: &str| EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    };
    let harness = Container {
        name: "harness".to_string(),
        image: Some(image.to_string()),
        security_context: Some(hardened_security_context()),
        resources: limits("500m", "256Mi"),
        env_from,
        env: Some(vec![
            env("DISPATCH_PATH", &format!("{sub_agent}/{id}")),
            env("MANDATE", mandate),
        ]),
        ports: Some(vec![ContainerPort {
            name: Some("dispatch".to_string()),
            container_port: crate::DISPATCH_LINK_PORT as i32,
            ..Default::default()
        }]),
        readiness_probe: Some(Probe {
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::Int(crate::DISPATCH_LINK_PORT as i32),
                ..Default::default()
            }),
            ..Default::default()
        }),
        volume_mounts: Some(vec![
            VolumeMount {
                sub_path: Some(format!("{sub_agent}/{id}")),
                ..mount("conversation-data", crate::CONVERSATION_DIR, false)
            },
            mount(&instructions_volume, &instructions_dir, true),
            mount("scheduling", "/etc/sycophant/scheduling", true),
            mount(
                "kube-api-token",
                "/var/run/secrets/kubernetes.io/serviceaccount",
                true,
            ),
        ]),
        ..Default::default()
    };
    let empty_dir = |name: &str| Volume {
        name: name.to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    };
    let volumes = vec![
        Volume {
            name: "conversation-data".to_string(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: format!("ag-{agent}-conversation"),
                ..Default::default()
            }),
            ..Default::default()
        },
        empty_dir(&instructions_volume),
        empty_dir("instructions-sync-scratch"),
        store_token,
        Volume {
            name: "scheduling".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: "toolset-scheduling".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
        kube_api_token_volume(),
    ];
    Job {
        metadata: ObjectMeta {
            name: Some(host.clone()),
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(0),
            ttl_seconds_after_finished: Some(30),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(BTreeMap::from([
                        ("sycophant.md/agent".to_string(), agent.to_string()),
                        ("sycophant.md/job-kind".to_string(), "dispatch".to_string()),
                    ])),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    // The record `<sub>-<id>.ag-<agent>-dispatch.<ns>.svc.cluster.local`
                    // the agent's harness attaches to.
                    hostname: Some(host),
                    subdomain: Some(format!("ag-{agent}-dispatch")),
                    restart_policy: Some("Never".to_string()),
                    service_account_name: Some(format!("ag-{sub_agent}")),
                    automount_service_account_token: Some(false),
                    termination_grace_period_seconds: Some(5),
                    security_context: Some(PodSecurityContext {
                        run_as_non_root: Some(true),
                        run_as_user: Some(1000),
                        run_as_group: Some(1000),
                        fs_group: Some(1000),
                        seccomp_profile: Some(SeccompProfile {
                            type_: "RuntimeDefault".to_string(),
                            localhost_profile: None,
                        }),
                        ..Default::default()
                    }),
                    tolerations: Some(vec![Toleration {
                        key: Some("sycophant.md/workload".to_string()),
                        operator: Some("Equal".to_string()),
                        value: Some("agent".to_string()),
                        effect: Some("NoSchedule".to_string()),
                        ..Default::default()
                    }]),
                    affinity: Some(agent_affinity(agent)),
                    init_containers: Some(vec![sync]),
                    containers: vec![harness],
                    volumes: Some(volumes),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The `kube-api-token` volume of harness.yaml, in the exact shape the pod
/// VAP pins: the apiserver token, the cluster CA and the namespace.
fn kube_api_token_volume() -> Volume {
    let item = |key: &str| KeyToPath {
        key: key.to_string(),
        path: key.to_string(),
        ..Default::default()
    };
    Volume {
        name: "kube-api-token".to_string(),
        projected: Some(ProjectedVolumeSource {
            default_mode: Some(420),
            sources: Some(vec![
                VolumeProjection {
                    service_account_token: Some(ServiceAccountTokenProjection {
                        path: "token".to_string(),
                        audience: None,
                        expiration_seconds: Some(3600),
                    }),
                    ..Default::default()
                },
                VolumeProjection {
                    config_map: Some(ConfigMapProjection {
                        name: "kube-root-ca.crt".to_string(),
                        items: Some(vec![item("ca.crt")]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                VolumeProjection {
                    downward_api: Some(DownwardAPIProjection {
                        items: Some(vec![DownwardAPIVolumeFile {
                            path: "namespace".to_string(),
                            field_ref: Some(ObjectFieldSelector {
                                field_path: "metadata.namespace".to_string(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }]),
                    }),
                    ..Default::default()
                },
            ]),
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
pub(crate) mod tests {
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
            "ag-test-workspace",
            &SchedulingConfig::default(),
            &[],
        )
    }

    /// The harness-created capability-job pod runs as `ag-<agent>-unprivileged`, the
    /// zero-RBAC run identity the gate pins.
    #[test]
    fn harness_tool_job_runs_as_unprivileged_agent_sa() {
        let job = test_job();
        assert_eq!(
            pod_spec(&job).service_account_name.as_deref(),
            Some("ag-test-unprivileged"),
        );
    }

    /// The harness stamps the pod's per-pod DNS coordinates so
    /// the agent headless Service publishes `<call-id>.<service>`. The
    /// hostname is the call id and the subdomain is the headless Service name
    /// the harness passes in (and later dials). A build that leaves either
    /// unset publishes no per-pod A record, so the harness has nothing to dial.
    #[test]
    fn tool_job_pod_carries_per_pod_dns_hostname_and_subdomain() {
        let entry = ToolsetEntry {
            image: Some("ghcr.io/test/toolset:latest".into()),
            keepalive: true,
            ..Default::default()
        };
        let secrets = [ToolsetSecret {
            secret: "git-ssh".into(),
            path: "/home/agent/.ssh/id_ed25519".into(),
        }];
        let job = build_tool_job(
            "git-push",
            "test-toolset",
            &entry,
            TEST_CALL_ID,
            "test-ns",
            "capability-test",
            "test",
            "ag-test-workspace",
            &SchedulingConfig::default(),
            &secrets,
        );
        let spec = pod_spec(&job);
        for env in spec.containers[0].env.as_deref().unwrap() {
            assert!(is_reserved_env(&env.name), "{} is not reserved", env.name);
        }
        assert_eq!(
            spec.hostname.as_deref(),
            Some(TEST_CALL_ID),
            "the pod hostname must be the call id so the headless Service names its per-pod record",
        );
        assert_eq!(
            spec.subdomain.as_deref(),
            Some("capability-test"),
            "the pod subdomain must be the agent headless Service",
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
            // The model key, deliberately distinct from the provider model
            // string below so a build that confuses the two is caught.
            "deepseek-v4-flash",
            &SchedulingConfig::default(),
            "ghcr.io/sycophant/inference-runtime:1",
            "openai",
            "deepseek/deepseek-v4-flash",
            "https://openrouter.ai/api/v1",
            secret,
        )
    }

    pub(crate) fn container_env<'a>(job: &'a Job, name: &str) -> Option<&'a str> {
        pod_spec(job).containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.value.as_deref())
    }

    /// The inference-runtime pod stamps the same per-pod DNS coordinates the
    /// tool job does, so the agent headless Service publishes
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
            "the pod subdomain must be the agent headless Service",
        );
    }

    /// The pod carries `job-kind: inference`, the value the Job CREATE gate
    /// admits into the inference arm. A build that omits it or stamps another
    /// value is refused at admission.
    ///
    /// It also carries `sycophant.md/model` set to the model key the harness
    /// dispatched, never the provider model string. The per-model egress
    /// policy selects the inference pod by exactly this label, so a build that
    /// stamps the provider model id here, or omits the label, matches no
    /// provider-egress rule and leaves the job on the fail-closed baseline
    /// floor.
    #[test]
    fn inference_job_pod_labels_its_kind_component_and_model_key() {
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
        assert_eq!(
            labels.get("sycophant.md/model").map(String::as_str),
            Some("deepseek-v4-flash"),
            "the model label must be the resolver key the egress policy selects on",
        );
        // The key is not the provider model id. Stamping `config.model()` here
        // would label the pod for a model no inference egress CNP is named for.
        assert_ne!(
            labels.get("sycophant.md/model").map(String::as_str),
            Some("deepseek/deepseek-v4-flash"),
            "the model label must be the map key, never the provider model id",
        );
    }

    /// The pod runs as the agent's zero-RBAC unprivileged SA and mounts no
    /// projected token: it serves the harness and authenticates no outbound
    /// sycophant call.
    #[test]
    fn inference_job_runs_as_unprivileged_agent_sa_with_no_token() {
        let spec = pod_spec(&test_inference_job(None)).clone();
        assert_eq!(
            spec.service_account_name.as_deref(),
            Some("ag-test-unprivileged"),
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

    fn toolset_job(toolset: &str, secrets: &[shared::toolset::ToolsetSecret]) -> Job {
        let entry = ToolsetEntry {
            image: Some("ghcr.io/test/toolset:latest".into()),
            ..Default::default()
        };
        build_tool_job(
            "ssh-run",
            toolset,
            &entry,
            TEST_CALL_ID,
            "test-ns",
            "capability-test",
            "test",
            "ag-test-workspace",
            &SchedulingConfig::default(),
            secrets,
        )
    }

    fn secret_volumes(job: &Job) -> Vec<(String, String)> {
        pod_spec(job)
            .volumes
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| {
                let secret = v.secret.as_ref()?;
                Some((v.name.clone(), secret.secret_name.clone().unwrap()))
            })
            .collect()
    }

    fn mount_path<'a>(job: &'a Job, volume: &str) -> Option<&'a str> {
        pod_spec(job).containers[0]
            .volume_mounts
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|m| m.name == volume)
            .map(|m| m.mount_path.as_str())
    }

    fn env_json(job: &Job, name: &str) -> Vec<serde_json::Value> {
        let raw = container_env(job, name).unwrap_or_else(|| panic!("{name} is not set"));
        serde_json::from_str(raw).unwrap()
    }

    /// Every call of a toolset gets all of that toolset's Secrets: one Secret
    /// volume per Secret, each staged at its own path, each with one
    /// credential-map entry and one scrub entry. The pod carries the agent and
    /// toolset labels the per-toolset egress policy selects on.
    #[test]
    fn a_toolset_with_two_secrets_mounts_stages_and_scrubs_both() {
        use shared::toolset::ToolsetSecret;
        let secrets = [
            ToolsetSecret {
                secret: "demo-key".into(),
                path: "/home/agent/.ssh/id_ed25519".into(),
            },
            ToolsetSecret {
                secret: "local-key".into(),
                path: "/home/agent/.ssh/local".into(),
            },
        ];
        let job = toolset_job("ssh", &secrets);

        assert_eq!(
            secret_volumes(&job),
            vec![
                ("credential-0".to_string(), "demo-key".to_string()),
                ("credential-1".to_string(), "local-key".to_string()),
            ],
        );
        let first = "/tmp/credentials/credential-0/id_ed25519";
        let second = "/tmp/credentials/credential-1/local";
        assert_eq!(mount_path(&job, "credential-0"), Some(first));
        assert_eq!(mount_path(&job, "credential-1"), Some(second));

        assert_eq!(
            env_json(&job, "TOOLSET_CREDENTIAL_MAP"),
            vec![
                serde_json::json!({"staging": first, "target": "/home/agent/.ssh/id_ed25519"}),
                serde_json::json!({"staging": second, "target": "/home/agent/.ssh/local"}),
            ],
        );
        assert_eq!(
            env_json(&job, "TOOLSET_SCRUB_SECRETS"),
            vec![
                serde_json::json!({"name": "demo-key", "file": "/home/agent/.ssh/id_ed25519"}),
                serde_json::json!({"name": "local-key", "file": "/home/agent/.ssh/local"}),
            ],
        );

        assert_eq!(
            mount_path(&job, "credentials"),
            Some("/run/secrets/credentials")
        );
        assert!(
            pod_spec(&job)
                .volumes
                .as_deref()
                .unwrap()
                .iter()
                .any(|v| v.name == "credentials" && v.empty_dir.is_some()),
            "the writable credentials mount must be an emptyDir",
        );

        let labels = pod_spec_labels(&job);
        assert_eq!(
            labels.get("sycophant.md/toolset").map(String::as_str),
            Some("ssh")
        );
        assert_eq!(
            labels.get("sycophant.md/agent").map(String::as_str),
            Some("test")
        );
    }

    /// A toolset with no Secrets mounts none and hands the runtime no
    /// credential map or scrub registry to act on.
    #[test]
    fn a_toolset_with_no_secrets_mounts_no_secret_and_sets_no_map() {
        let job = toolset_job("git", &[]);

        assert!(
            secret_volumes(&job).is_empty(),
            "{:?}",
            secret_volumes(&job)
        );
        assert_eq!(container_env(&job, "TOOLSET_CREDENTIAL_MAP"), None);
        assert_eq!(container_env(&job, "TOOLSET_SCRUB_SECRETS"), None);
    }

    /// Agent `one` starts sub-agent `sub` at path `sub/0a1b2c3d`.
    fn sub_agent_job() -> Job {
        build_dispatch_job(
            "one",
            "sub",
            "0a1b2c3d",
            "summarize the repo",
            "tenant-a",
            "sycophant-harness:local",
        )
    }

    fn all_containers(spec: &PodSpec) -> impl Iterator<Item = &Container> {
        spec.init_containers
            .iter()
            .flatten()
            .chain(&spec.containers)
    }

    #[test]
    fn sub_agent_job_never_retries_and_has_no_deadline() {
        let job = sub_agent_job();
        let spec = job.spec.as_ref().expect("the Job has a spec");
        let restart = pod_spec(&job).restart_policy.as_deref();
        assert_eq!(
            (spec.backoff_limit, restart, spec.active_deadline_seconds),
            (Some(0), Some("Never"), None),
            "want backoffLimit 0, restartPolicy Never and no deadline"
        );
    }

    #[test]
    fn sub_agent_job_mounts_the_conversation_volume_only_at_its_own_folder() {
        let job = sub_agent_job();
        let spec = pod_spec(&job);
        let claimed: Vec<&str> = spec
            .volumes
            .iter()
            .flatten()
            .filter(|v| {
                v.persistent_volume_claim
                    .as_ref()
                    .map(|c| c.claim_name.as_str())
                    == Some("ag-one-conversation")
            })
            .map(|v| v.name.as_str())
            .collect();
        let mounts: Vec<(&str, Option<&str>)> = all_containers(spec)
            .flat_map(|c| c.volume_mounts.iter().flatten())
            .filter(|m| claimed.contains(&m.name.as_str()))
            .map(|m| (m.mount_path.as_str(), m.sub_path.as_deref()))
            .collect();
        assert_eq!(
            mounts,
            [("/var/lib/harness/conversations", Some("sub/0a1b2c3d"))],
            "the conversation claim is mounted once, at the sub-agent's folder"
        );
    }

    #[test]
    fn sub_agent_job_carries_no_relay_env_or_token() {
        let job = sub_agent_job();
        let spec = pod_spec(&job);
        let relay_env: Vec<&str> = all_containers(spec)
            .flat_map(|c| c.env.iter().flatten())
            .map(|e| e.name.as_str())
            .filter(|n| n.starts_with("RELAY"))
            .collect();
        let audiences: Vec<&str> = spec
            .volumes
            .iter()
            .flatten()
            .filter_map(|v| v.projected.as_ref())
            .flat_map(|p| p.sources.iter().flatten())
            .filter_map(|s| s.service_account_token.as_ref())
            .filter_map(|t| t.audience.as_deref())
            .collect();
        assert!(relay_env.is_empty(), "relay env {relay_env:?}");
        assert!(
            !audiences.contains(&shared::auth::HARNESS_RELAY_AUDIENCE),
            "a relay token in {audiences:?}"
        );
    }
}
