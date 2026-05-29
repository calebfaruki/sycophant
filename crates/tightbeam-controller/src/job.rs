use crate::crd::{ChannelSpec, ModelSpec, ProviderSpec};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, KeyToPath, PodSecurityContext, PodSpec, PodTemplateSpec,
    ProjectedVolumeSource, SecretProjection, SecretVolumeSource, ServiceAccountTokenProjection,
    Volume, VolumeMount, VolumeProjection,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use shared::hardened_security_context;
use shared::scheduling::SchedulingConfig;
use std::collections::BTreeMap;

fn canonical_base_url(format: &str) -> String {
    match format {
        "anthropic" => "https://api.anthropic.com/v1".into(),
        "openai" => "https://api.openai.com/v1".into(),
        "gemini" => "https://generativelanguage.googleapis.com".into(),
        _ => String::new(),
    }
}

fn job_labels(
    type_label: &str,
    name_key: &str,
    name_value: &str,
    component: &str,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/part-of".into(), "sycophant".into());
    labels.insert("app.kubernetes.io/component".into(), component.into());
    labels.insert("sycophant.md/type".into(), type_label.into());
    labels.insert(format!("sycophant.md/{name_key}"), name_value.into());
    labels
}

fn secret_volume(volume_name: &str, mount_path: &str, secret_name: &str) -> (Volume, VolumeMount) {
    let volume = Volume {
        name: volume_name.into(),
        secret: Some(SecretVolumeSource {
            secret_name: Some(secret_name.into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mount = VolumeMount {
        name: volume_name.into(),
        mount_path: mount_path.into(),
        read_only: Some(true),
        ..Default::default()
    };
    (volume, mount)
}

#[allow(clippy::too_many_arguments)]
pub fn build_llm_job(
    model_name: &str,
    model: &ModelSpec,
    provider: &ProviderSpec,
    image: &str,
    controller_addr: &str,
    namespace: &str,
    session_id: &str,
    workspace: &str,
    scheduling: &SchedulingConfig,
) -> Job {
    let job_name = format!("tightbeam-llm-{model_name}-{session_id}");
    let labels = job_labels("llm", "model", model_name, "llm-job");

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
            name: "TIGHTBEAM_CONTROLLER_ADDR".into(),
            value: Some(controller_addr.into()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_MODEL_NAME".into(),
            value: Some(model_name.into()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_JOB_ID".into(),
            value: Some(format!("llm-{model_name}-{session_id}")),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_FORMAT".into(),
            value: Some(provider.format.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_MODEL".into(),
            value: Some(model.model.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_BASE_URL".into(),
            value: Some(base_url),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_WORKSPACE".into(),
            value: Some(workspace.into()),
            ..Default::default()
        },
    ];

    // Wire the in-pod ScrubSet (crates/shared/src/scrub.rs) so the LLM
    // job redacts any occurrence of the API-key value in tracing logs,
    // gRPC chunks, and error messages. The key lives only as a file at
    // `/run/secrets/tightbeam/api-key` — never an env var — so the
    // scrub registry uses the `file` variant. Mirrors the airlock
    // pattern at airlock-controller/src/job.rs:163-181.
    let scrub_entries = serde_json::json!([{
        "name": provider.secret.name,
        "file": "/run/secrets/tightbeam/api-key",
    }]);
    env_vars.push(EnvVar {
        name: "TIGHTBEAM_SCRUB_SECRETS".into(),
        value: Some(
            serde_json::to_string(&scrub_entries).expect("scrub registry serializes"),
        ),
        ..Default::default()
    });

    // Projected volume: kubelet mounts the upstream Secret's `secret_key`
    // value as a file at `/run/secrets/tightbeam/api-key` (mode 0o440).
    // The path is stable regardless of the upstream key name — KeyToPath.path
    // is the consumer-facing filename. The LLM Job reads this file via
    // TIGHTBEAM_API_KEY_PATH (defaulting to that mount path).
    // Mode 0o440 + pod-level fsGroup=1000 so the projected file is owned by
    // root:1000 and the runAsUser=1000 container can read it via group access.
    // Owner-only 0o400 won't work because the LLM Job runs as a non-root user
    // while kubelet mounts files with root ownership by default.
    let secret_volume_name = "tightbeam-secret".to_string();
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
        mount_path: "/run/secrets/tightbeam".into(),
        read_only: Some(true),
        ..Default::default()
    };

    // Custom-audience projected SA token; see shared::auth::SA_TOKEN_PATH.
    // The LLM-job pod's token carries the llm-dispatch audience so that
    // tightbeam's internal listener only accepts it on GetTurn /
    // StreamTurnResult. A workspace-audience token (transponder) cannot
    // reach those methods, even though both pods share the SA name.
    let auth_volume_name = "llm-job-auth".to_string();
    let auth_volume = Volume {
        name: auth_volume_name.clone(),
        projected: Some(ProjectedVolumeSource {
            default_mode: None,
            sources: Some(vec![VolumeProjection {
                service_account_token: Some(ServiceAccountTokenProjection {
                    path: "token".into(),
                    audience: Some(shared::auth::LLM_TIGHTBEAM_AUDIENCE.into()),
                    expiration_seconds: Some(3600),
                }),
                ..Default::default()
            }]),
        }),
        ..Default::default()
    };
    let auth_mount = VolumeMount {
        name: auth_volume_name,
        mount_path: "/var/run/secrets/kubernetes.io/serviceaccount".into(),
        read_only: Some(true),
        ..Default::default()
    };

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.into()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            ttl_seconds_after_finished: Some(30),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("Never".into()),
                    // Bind the LLM Job's identity to the workspace so
                    // tightbeam-controller's GetTurn handler can verify
                    // pending.workspace == caller_workspace (the SA
                    // token's parsed workspace from `sa-<workspace>`).
                    // This prevents cross-workspace dequeue.
                    service_account_name: Some(format!("sa-{workspace}")),
                    // Custom-audience projected token replaces the default.
                    automount_service_account_token: Some(false),
                    security_context: Some(PodSecurityContext {
                        fs_group: Some(1000),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "llm".into(),
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
pub async fn create_llm_job(
    client: &kube::Client,
    model_name: &str,
    model: &ModelSpec,
    provider: &ProviderSpec,
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
    let job = build_llm_job(
        model_name,
        model,
        provider,
        image,
        controller_addr,
        namespace,
        &session_id,
        workspace,
        scheduling,
    );
    let job_name = job.metadata.name.clone().unwrap_or_default();

    let api: kube::Api<Job> = kube::Api::namespaced(client.clone(), namespace);
    api.create(&kube::api::PostParams::default(), &job).await?;

    tracing::info!("created LLM Job {job_name} in namespace {namespace}");
    Ok(job_name)
}

pub fn build_channel_job(
    channel_name: &str,
    spec: &ChannelSpec,
    controller_addr: &str,
    namespace: &str,
    session_id: &str,
    workspace: &str,
    scheduling: &SchedulingConfig,
) -> Job {
    let job_name = format!("tightbeam-channel-{channel_name}-{session_id}");
    let labels = job_labels("channel", "channel", channel_name, "channel-job");
    let (volume, mount) =
        secret_volume("channel-secrets", "/run/secrets/channel", &spec.secret_name);

    Job {
        metadata: ObjectMeta {
            name: Some(job_name),
            namespace: Some(namespace.into()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            ttl_seconds_after_finished: Some(30),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("OnFailure".into()),
                    containers: vec![Container {
                        name: "channel".into(),
                        image: Some(spec.image.clone()),
                        env: Some(vec![
                            EnvVar {
                                name: "TIGHTBEAM_CONTROLLER_ADDR".into(),
                                value: Some(controller_addr.into()),
                                ..Default::default()
                            },
                            EnvVar {
                                name: "TIGHTBEAM_CHANNEL_NAME".into(),
                                value: Some(channel_name.into()),
                                ..Default::default()
                            },
                            EnvVar {
                                name: "TIGHTBEAM_WORKSPACE".into(),
                                value: Some(workspace.into()),
                                ..Default::default()
                            },
                        ]),
                        volume_mounts: Some(vec![mount]),
                        security_context: Some(hardened_security_context()),
                        ..Default::default()
                    }],
                    volumes: Some(vec![volume]),
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
    use crate::crd::{ProviderRef, ProviderSecret};

    use shared::scheduling::testing::{assert_scheduling, no_scheduling, test_scheduling};

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

    fn sample_channel_spec() -> ChannelSpec {
        ChannelSpec {
            channel_type: "discord".into(),
            secret_name: "discord-bot-token".into(),
            image: "ghcr.io/calebfaruki/tightbeam-channel-discord:latest".into(),
        }
    }

    const TEST_IMAGE: &str = "ghcr.io/calebfaruki/tightbeam-llm-job:latest";

    fn env_map(job: &Job) -> BTreeMap<String, String> {
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
    fn llm_job_has_correct_name() {
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://controller:9090",
            "workspace-test",
            "abc123",
            "default",
            &no_scheduling(),
        );
        assert_eq!(
            job.metadata.name.unwrap(),
            "tightbeam-llm-claude-sonnet-abc123"
        );
        assert_eq!(job.metadata.namespace.unwrap(), "workspace-test");
    }

    #[test]
    fn llm_job_has_correct_labels() {
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://controller:9090",
            "ws",
            "s1",
            "default",
            &no_scheduling(),
        );
        let labels = job.metadata.labels.unwrap();
        assert_eq!(labels["app.kubernetes.io/part-of"], "sycophant");
        assert_eq!(labels["sycophant.md/type"], "llm");
        assert_eq!(labels["sycophant.md/model"], "claude-sonnet");
    }

    #[test]
    fn llm_job_env_vars() {
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://controller:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let env = env_map(&job);
        assert_eq!(env["TIGHTBEAM_CONTROLLER_ADDR"], "http://controller:9090");
        assert_eq!(env["TIGHTBEAM_FORMAT"], "anthropic");
        assert_eq!(env["TIGHTBEAM_MODEL"], "claude-sonnet-4-20250514");
        assert_eq!(env["TIGHTBEAM_BASE_URL"], "https://api.anthropic.com/v1");
    }

    #[test]
    fn llm_job_populates_scrub_secrets() {
        // Without this env var, ScrubSet::from_env_var returns an empty
        // no-op set and `sk-ant-`-prefixed API keys land unredacted in
        // the conversation log. Pinning the env-var name + JSON shape +
        // file path catches every disable mutant.
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://controller:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let env = env_map(&job);
        let raw = env
            .get("TIGHTBEAM_SCRUB_SECRETS")
            .expect("TIGHTBEAM_SCRUB_SECRETS must be set on the LLM job pod");
        let parsed: serde_json::Value =
            serde_json::from_str(raw).expect("TIGHTBEAM_SCRUB_SECRETS must parse as JSON");
        let entries = parsed.as_array().expect("registry must be a JSON array");
        assert_eq!(entries.len(), 1, "expected exactly one scrub entry");
        assert_eq!(entries[0]["name"], "anthropic-key");
        assert_eq!(entries[0]["file"], "/run/secrets/tightbeam/api-key");
    }

    #[test]
    fn llm_job_workspace_env_var() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "my-workspace",
            &no_scheduling(),
        );
        let env = env_map(&job);
        assert_eq!(env["TIGHTBEAM_WORKSPACE"], "my-workspace");
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
    fn llm_job_secret_mount_uses_projected_volume_with_key_to_path() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
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
    fn llm_job_no_api_key_env_var() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let container = &job.spec.unwrap().template.spec.unwrap().containers[0];
        let has_api_key = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .any(|e| e.name == "API_KEY");
        assert!(
            !has_api_key,
            "API_KEY env var must not be set; the LLM Job reads the key from the projected volume file"
        );
    }

    #[test]
    fn llm_job_secret_volume_and_mount_share_name() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let volume_name = pod_spec.volumes.as_ref().unwrap()[0].name.clone();
        let mount_name = pod_spec.containers[0].volume_mounts.as_ref().unwrap()[0]
            .name
            .clone();
        assert_eq!(
            volume_name, mount_name,
            "volume.name and volume_mount.name must match for kubelet to bind the projection"
        );
        assert!(
            !volume_name.is_empty(),
            "volume name must be a non-empty string"
        );
    }

    #[test]
    fn llm_job_secret_mount_path_is_stable() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let mount = &pod_spec.containers[0].volume_mounts.as_ref().unwrap()[0];
        assert_eq!(mount.mount_path, "/run/secrets/tightbeam");
        assert_eq!(mount.read_only, Some(true));
    }

    #[test]
    fn llm_job_no_thinking_env_var() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let env = env_map(&job);
        assert!(
            !env.contains_key("TIGHTBEAM_THINKING"),
            "TIGHTBEAM_THINKING env var must not be set; thinking moved into pass-through params"
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
    fn llm_job_base_url_uses_provider_or_canonical_default() {
        let mut provider = sample_provider_spec();
        provider.base_url = Some("https://custom.example.com/v1".into());
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &provider,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        assert_eq!(
            env_map(&job)["TIGHTBEAM_BASE_URL"],
            "https://custom.example.com/v1"
        );

        let mut provider = sample_provider_spec();
        provider.base_url = None;
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &provider,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        assert_eq!(
            env_map(&job)["TIGHTBEAM_BASE_URL"],
            "https://api.anthropic.com/v1"
        );
    }

    #[test]
    fn llm_job_secret_key_defaults_to_api_key() {
        let mut provider = sample_provider_spec();
        provider.secret.key = None;
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &provider,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let item = projected_secret_item(&job);
        assert_eq!(
            item.key, "api-key",
            "absent provider.secret.key must default to 'api-key'"
        );
    }

    #[test]
    fn llm_job_secret_key_explicit_used_when_set() {
        let mut provider = sample_provider_spec();
        provider.secret.key = Some("custom-key".into());
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &provider,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let item = projected_secret_item(&job);
        assert_eq!(item.key, "custom-key");
    }

    #[test]
    fn llm_job_pod_template_has_labels() {
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let template_labels = job.spec.unwrap().template.metadata.unwrap().labels.unwrap();
        assert_eq!(template_labels["sycophant.md/type"], "llm");
        assert_eq!(template_labels["sycophant.md/model"], "claude-sonnet");
    }

    #[test]
    fn llm_job_never_restart_and_ttl() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let spec = job.spec.unwrap();
        assert_eq!(spec.ttl_seconds_after_finished, Some(30));
        assert_eq!(
            spec.template.spec.unwrap().restart_policy.as_deref(),
            Some("Never")
        );
    }

    #[test]
    fn channel_job_has_correct_name_and_labels() {
        let job = build_channel_job(
            "discord-bot",
            &sample_channel_spec(),
            "http://controller:9090",
            "workspace-test",
            "xyz789",
            "default",
            &no_scheduling(),
        );
        assert_eq!(
            job.metadata.name.unwrap(),
            "tightbeam-channel-discord-bot-xyz789"
        );
        let labels = job.metadata.labels.unwrap();
        assert_eq!(labels["app.kubernetes.io/part-of"], "sycophant");
        assert_eq!(labels["sycophant.md/type"], "channel");
        assert_eq!(labels["sycophant.md/channel"], "discord-bot");
    }

    #[test]
    fn channel_job_restart_and_ttl() {
        let job = build_channel_job(
            "d",
            &sample_channel_spec(),
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let spec = job.spec.unwrap();
        assert_eq!(spec.ttl_seconds_after_finished, Some(30));
        assert_eq!(
            spec.template.spec.unwrap().restart_policy.as_deref(),
            Some("OnFailure")
        );
    }

    #[test]
    fn channel_job_secret_mount_is_read_only() {
        let job = build_channel_job(
            "d",
            &sample_channel_spec(),
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let mount = &pod_spec.containers[0].volume_mounts.as_ref().unwrap()[0];
        assert_eq!(mount.read_only, Some(true));
    }

    #[test]
    fn channel_job_mounts_channel_secret() {
        let job = build_channel_job(
            "d",
            &sample_channel_spec(),
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let volume = &pod_spec.volumes.unwrap()[0];
        assert_eq!(volume.name, "channel-secrets");
        assert_eq!(
            volume.secret.as_ref().unwrap().secret_name.as_deref(),
            Some("discord-bot-token")
        );
        let mount = &pod_spec.containers[0].volume_mounts.as_ref().unwrap()[0];
        assert_eq!(mount.name, "channel-secrets");
        assert_eq!(mount.mount_path, "/run/secrets/channel");
    }

    #[test]
    fn channel_job_pod_template_has_labels() {
        let job = build_channel_job(
            "discord",
            &sample_channel_spec(),
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let template_labels = job.spec.unwrap().template.metadata.unwrap().labels.unwrap();
        assert_eq!(template_labels["sycophant.md/type"], "channel");
        assert_eq!(template_labels["sycophant.md/channel"], "discord");
    }

    #[test]
    fn channel_job_workspace_env_var() {
        let job = build_channel_job(
            "d",
            &sample_channel_spec(),
            "http://c:9090",
            "ns",
            "s1",
            "my-workspace",
            &no_scheduling(),
        );
        let env = env_map(&job);
        assert_eq!(env["TIGHTBEAM_WORKSPACE"], "my-workspace");
    }

    #[test]
    fn no_api_key_in_job_spec() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let json = serde_json::to_string(&job).unwrap();
        assert!(
            !json.contains("sk-ant"),
            "API key must never appear in Job spec"
        );
    }

    #[test]
    fn llm_job_has_scheduling_constraints() {
        let sched = test_scheduling("tightbeam");
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &sched,
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        assert_scheduling(&ps, "tightbeam");
    }

    #[test]
    fn channel_job_has_scheduling_constraints() {
        let sched = test_scheduling("tightbeam");
        let job = build_channel_job(
            "d",
            &sample_channel_spec(),
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &sched,
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        assert_scheduling(&ps, "tightbeam");
    }

    #[test]
    fn llm_job_no_scheduling_when_empty() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        assert!(ps.node_selector.is_none());
        assert!(ps.tolerations.is_none());
    }

    #[test]
    fn llm_job_has_hardened_security_context() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        let sc = ps.containers[0].security_context.as_ref().unwrap();
        assert_eq!(sc.run_as_non_root, Some(true));
        assert_eq!(sc.run_as_user, Some(1000));
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(
            sc.capabilities.as_ref().unwrap().drop,
            Some(vec!["ALL".to_string()])
        );
    }

    /// `fs_group: 1000` is load-bearing for the projected secret volume:
    /// the mounted API key file is mode 0o440 owned by root:fs_group, and the
    /// runAsUser=1000 container reads it via group access. Without fs_group,
    /// the LLM container can't read its API key and every call fails at startup.
    #[test]
    fn llm_job_pod_security_context_has_fs_group() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let pod_spec = job.spec.unwrap().template.spec.unwrap();
        let psc = pod_spec
            .security_context
            .as_ref()
            .expect("pod security context must be set");
        assert_eq!(
            psc.fs_group,
            Some(1000),
            "fs_group=1000 lets runAsUser=1000 read the projected secret file via group access"
        );
    }

    fn sample_llm_job() -> Job {
        build_llm_job(
            "m",
            &sample_model_spec(),
            &sample_provider_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        )
    }

    #[test]
    fn llm_job_disables_kubelet_default_sa_token_mount() {
        let pod_spec = sample_llm_job().spec.unwrap().template.spec.unwrap();
        assert_eq!(
            pod_spec.automount_service_account_token,
            Some(false),
            "automount=false suppresses the kubelet kube-apiserver-audience \
             token; the controllers require the llm-dispatch audience"
        );
    }

    #[test]
    fn llm_job_mounts_llm_dispatch_audience_projected_token() {
        let pod_spec = sample_llm_job().spec.unwrap().template.spec.unwrap();
        let auth_vol = pod_spec
            .volumes
            .as_ref()
            .and_then(|vs| vs.iter().find(|v| v.name == "llm-job-auth"))
            .expect("llm-job-auth volume must be present on LLM job pod");
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
            Some(shared::auth::LLM_TIGHTBEAM_AUDIENCE),
            "LLM job token audience must be llm.tightbeam.sycophant.md \
             so a stolen mainframe token cannot reach GetTurn / StreamTurnResult"
        );
        assert_eq!(sat.path, "token");
        assert_eq!(sat.expiration_seconds, Some(3600));

        let llm = pod_spec
            .containers
            .iter()
            .find(|c| c.name == "llm")
            .expect("llm container");
        let mounts = llm.volume_mounts.as_ref().expect("volume_mounts");
        let auth_mount = mounts
            .iter()
            .find(|m| m.name == "llm-job-auth")
            .expect("llm container must mount llm-job-auth");
        assert_eq!(
            auth_mount.mount_path, "/var/run/secrets/kubernetes.io/serviceaccount",
            "must mount at the kubelet-default path so SaTokenInterceptor \
             reads the same SA_TOKEN_PATH unchanged"
        );
        assert_eq!(auth_mount.read_only, Some(true));
    }

    #[test]
    fn llm_job_auth_volume_distinct_from_api_key_volume() {
        let pod_spec = sample_llm_job().spec.unwrap().template.spec.unwrap();
        let volume_names: Vec<&str> = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(volume_names.contains(&"tightbeam-secret"));
        assert!(volume_names.contains(&"llm-job-auth"));
        // Distinct projections; the secret one must not carry a token source.
        let secret_vol = pod_spec
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "tightbeam-secret")
            .unwrap();
        let secret_sources = secret_vol
            .projected
            .as_ref()
            .and_then(|p| p.sources.as_ref())
            .unwrap();
        assert!(
            secret_sources
                .iter()
                .all(|s| s.service_account_token.is_none()),
            "tightbeam-secret projection must not carry a serviceAccountToken"
        );
    }
}
