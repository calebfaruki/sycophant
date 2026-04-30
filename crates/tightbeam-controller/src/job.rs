use crate::crd::{TightbeamChannelSpec, TightbeamModelSpec};
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, EnvVar, EnvVarSource, PodSpec, PodTemplateSpec, SecretKeySelector,
    SecretVolumeSource, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use std::collections::BTreeMap;
use sycophant_scheduling::SchedulingConfig;

fn job_labels(
    type_label: &str,
    name_key: &str,
    name_value: &str,
    component: &str,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/part-of".into(), "sycophant".into());
    labels.insert("app.kubernetes.io/component".into(), component.into());
    labels.insert("tightbeam.dev/type".into(), type_label.into());
    labels.insert(format!("tightbeam.dev/{name_key}"), name_value.into());
    labels
}

fn hardened_security_context() -> SecurityContext {
    SecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(1000),
        read_only_root_filesystem: Some(true),
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    }
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

pub fn build_llm_job(
    model_name: &str,
    spec: &TightbeamModelSpec,
    image: &str,
    controller_addr: &str,
    namespace: &str,
    session_id: &str,
    workspace: &str,
    scheduling: &SchedulingConfig,
) -> Job {
    let job_name = format!("tightbeam-llm-{model_name}-{session_id}");
    let labels = job_labels("llm", "model", model_name, "llm-job");

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
            value: Some(spec.format.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_MODEL".into(),
            value: Some(spec.model.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_BASE_URL".into(),
            value: Some(spec.base_url.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "TIGHTBEAM_WORKSPACE".into(),
            value: Some(workspace.into()),
            ..Default::default()
        },
    ];

    if let Some(ref thinking) = spec.thinking {
        env_vars.push(EnvVar {
            name: "TIGHTBEAM_THINKING".into(),
            value: Some(thinking.clone()),
            ..Default::default()
        });
    }

    let mut volumes = Vec::new();
    let mut volume_mounts = Vec::new();

    if let Some(ref secret) = spec.secret {
        if secret.env.is_some() {
            env_vars.push(EnvVar {
                name: "API_KEY".into(),
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: secret.name.clone(),
                        key: secret.name.clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            });
        } else if let Some(ref file_path) = secret.file {
            let (vol, mount) = secret_volume("llm-secret-file", file_path, &secret.name);
            volumes.push(vol);
            volume_mounts.push(mount);
        }
    }

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
                    containers: vec![Container {
                        name: "llm".into(),
                        image: Some(image.into()),
                        env: Some(env_vars),
                        security_context: Some(hardened_security_context()),
                        volume_mounts: if volume_mounts.is_empty() {
                            None
                        } else {
                            Some(volume_mounts)
                        },
                        ..Default::default()
                    }],
                    volumes: if volumes.is_empty() {
                        None
                    } else {
                        Some(volumes)
                    },
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

pub async fn create_llm_job(
    client: &kube::Client,
    model_name: &str,
    spec: &TightbeamModelSpec,
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
        spec,
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
    spec: &TightbeamChannelSpec,
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
    use crate::crd::SecretBinding;
    use k8s_openapi::api::core::v1::Toleration;

    fn no_scheduling() -> SchedulingConfig {
        SchedulingConfig::default()
    }

    fn test_scheduling(workload: &str) -> SchedulingConfig {
        SchedulingConfig {
            node_selector: BTreeMap::from([("sycophant.io/workload".into(), workload.into())]),
            tolerations: vec![Toleration {
                key: Some("sycophant.io/workload".into()),
                operator: Some("Equal".into()),
                value: Some(workload.into()),
                effect: Some("NoSchedule".into()),
                ..Default::default()
            }],
        }
    }

    fn assert_scheduling(pod_spec: &PodSpec, workload: &str) {
        let ns = pod_spec
            .node_selector
            .as_ref()
            .expect("node_selector must be set");
        assert_eq!(ns.get("sycophant.io/workload"), Some(&workload.to_string()));
        assert_eq!(ns.len(), 1);

        let tols = pod_spec
            .tolerations
            .as_ref()
            .expect("tolerations must be set");
        assert_eq!(tols.len(), 1);
        assert_eq!(tols[0].key.as_deref(), Some("sycophant.io/workload"));
        assert_eq!(tols[0].value.as_deref(), Some(workload));
        assert_eq!(tols[0].operator.as_deref(), Some("Equal"));
        assert_eq!(tols[0].effect.as_deref(), Some("NoSchedule"));
    }

    fn sample_model_spec() -> TightbeamModelSpec {
        TightbeamModelSpec {
            format: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            thinking: None,
            secret: Some(SecretBinding {
                name: "anthropic-key".into(),
                env: Some("API_KEY".into()),
                file: None,
            }),
        }
    }

    fn sample_channel_spec() -> TightbeamChannelSpec {
        TightbeamChannelSpec {
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
            TEST_IMAGE,
            "http://controller:9090",
            "ws",
            "s1",
            "default",
            &no_scheduling(),
        );
        let labels = job.metadata.labels.unwrap();
        assert_eq!(labels["app.kubernetes.io/part-of"], "sycophant");
        assert_eq!(labels["tightbeam.dev/type"], "llm");
        assert_eq!(labels["tightbeam.dev/model"], "claude-sonnet");
    }

    #[test]
    fn llm_job_env_vars() {
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
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
    fn llm_job_workspace_env_var() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
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

    #[test]
    fn llm_job_secret_as_env_var() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let container = &job.spec.unwrap().template.spec.unwrap().containers[0];
        let api_key_env = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == "API_KEY")
            .unwrap();
        let secret_ref = api_key_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(secret_ref.name, "anthropic-key");
    }

    #[test]
    fn llm_job_no_secret_when_none() {
        let mut spec = sample_model_spec();
        spec.secret = None;
        let job = build_llm_job(
            "m",
            &spec,
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
        assert!(!has_api_key);
    }

    #[test]
    fn llm_job_thinking_env_var() {
        let mut spec = sample_model_spec();
        spec.thinking = Some("high".into());
        let job = build_llm_job(
            "m",
            &spec,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let env = env_map(&job);
        assert_eq!(env["TIGHTBEAM_THINKING"], "high");
    }

    #[test]
    fn llm_job_pod_template_has_labels() {
        let job = build_llm_job(
            "claude-sonnet",
            &sample_model_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let template_labels = job.spec.unwrap().template.metadata.unwrap().labels.unwrap();
        assert_eq!(template_labels["tightbeam.dev/type"], "llm");
        assert_eq!(template_labels["tightbeam.dev/model"], "claude-sonnet");
    }

    #[test]
    fn llm_job_never_restart_and_ttl() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
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
        assert_eq!(labels["tightbeam.dev/type"], "channel");
        assert_eq!(labels["tightbeam.dev/channel"], "discord-bot");
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
        assert_eq!(template_labels["tightbeam.dev/type"], "channel");
        assert_eq!(template_labels["tightbeam.dev/channel"], "discord");
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

    #[test]
    fn secret_volume_is_read_only() {
        let mut spec = sample_model_spec();
        spec.secret = Some(SecretBinding {
            name: "key".into(),
            env: None,
            file: Some("/run/secrets/key".into()),
        });
        let job = build_llm_job(
            "m",
            &spec,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        let mount = &ps.containers[0].volume_mounts.as_ref().unwrap()[0];
        assert_eq!(mount.read_only, Some(true));
    }

    #[test]
    fn llm_job_secret_key_selector_has_key() {
        let job = build_llm_job(
            "m",
            &sample_model_spec(),
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let container = &job.spec.unwrap().template.spec.unwrap().containers[0];
        let api_key_env = container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == "API_KEY")
            .unwrap();
        let secret_ref = api_key_env
            .value_from
            .as_ref()
            .unwrap()
            .secret_key_ref
            .as_ref()
            .unwrap();
        assert_eq!(secret_ref.key, "anthropic-key");
    }

    #[test]
    fn llm_job_has_volumes_when_secret_file() {
        let mut spec = sample_model_spec();
        spec.secret = Some(SecretBinding {
            name: "key".into(),
            env: None,
            file: Some("/run/secrets/key".into()),
        });
        let job = build_llm_job(
            "m",
            &spec,
            TEST_IMAGE,
            "http://c:9090",
            "ns",
            "s1",
            "default",
            &no_scheduling(),
        );
        let ps = job.spec.unwrap().template.spec.unwrap();
        assert!(ps.volumes.is_some());
        assert!(!ps.volumes.unwrap().is_empty());
    }
}
