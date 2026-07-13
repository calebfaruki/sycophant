use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EmptyDirVolumeSource, EnvVar, EnvVarSource, PersistentVolumeClaimVolumeSource,
    PodSecurityContext, PodSpec, PodTemplateSpec, SecretKeySelector, Volume, VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use shared::hardened_security_context;
use shared::storage::S3Spec;
use std::collections::BTreeMap;

/// Pinned aws-cli image for the one-shot Kernel S3 sync Job. Generic tool
/// image (not project-specific), so it lives here as a const rather than a
/// chart value.
pub const AWS_CLI_IMAGE: &str =
    "amazon/aws-cli@sha256:db8d39443ef512d4724becfac59ec9b7c4f8621e7b3c6200be56cd4fc2dc9570";

/// The per-workspace writer PVC (`kernel-writer-<workspace>`, ReadWriteOnce) is
/// mounted here; the sync Job writes the workspace's persona directly to
/// `/kernels` (the host dir `<base>/<ns>/<workspace>`, the same directory the
/// workspace's read-only serving PV exposes at `/etc/kernels/<namespace>/<workspace>`).
const WRITER_MOUNT_PATH: &str = "/kernels";

fn job_labels(workspace: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/part-of".into(), "sycophant".into());
    labels.insert("app.kubernetes.io/component".into(), "kernel-sync".into());
    labels.insert("sycophant.md/type".into(), "kernel-sync".into());
    labels.insert("sycophant.md/workspace".into(), workspace.into());
    labels
}

/// Build the ephemeral one-shot Job that `aws s3 sync`s a Kernel's S3 source
/// into the workspace's kernel directory on the writer PVC.
///
/// Writing to a hostPath-backed PVC as a non-root pod under the
/// `restricted` PSA depends on the node directory's perms and the pod's
/// fsGroup. This holds for the local single-node hostPath story; the
/// RWX/cloud story is deferred (flagged in the plan). S3 is not exercised in
/// e2e, so the runtime write path here is unvalidated.
pub fn build_s3_sync_job(workspace: &str, namespace: &str, s3: &S3Spec, image: &str) -> Job {
    let job_name = format!("kernel-sync-{workspace}");
    let labels = job_labels(workspace);
    // The writer PV points at <base>/<ns>/<workspace> directly, so sync into the
    // mount root (not a <workspace> subdir of it).
    let sync_target = WRITER_MOUNT_PATH.to_string();
    let writer_claim = format!("kernel-writer-{workspace}");

    let mut env_vars = vec![
        // aws-cli writes config/cache under $HOME; point it at the writable
        // emptyDir so the read-only root filesystem does not block it.
        EnvVar {
            name: "HOME".into(),
            value: Some("/tmp".into()),
            ..Default::default()
        },
        EnvVar {
            name: "ENDPOINT".into(),
            value: Some(s3.endpoint.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "BUCKET".into(),
            value: Some(s3.bucket.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "PREFIX".into(),
            value: Some(s3.prefix.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "AWS_DEFAULT_REGION".into(),
            value: Some(s3.region.clone()),
            ..Default::default()
        },
    ];

    // Credentials flow as env vars sourced from the referenced Secret. Absent
    // credentials means the operator wired creds another way; skip the refs
    // rather than emit a secretKeyRef to a Secret that isn't named.
    if let Some(creds) = &s3.credentials {
        let access_key = creds
            .access_key_id_key
            .clone()
            .unwrap_or_else(|| "access-key-id".into());
        let secret_key = creds
            .secret_access_key_key
            .clone()
            .unwrap_or_else(|| "secret-access-key".into());
        env_vars.push(EnvVar {
            name: "AWS_ACCESS_KEY_ID".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: creds.name.clone(),
                    key: access_key,
                    optional: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        env_vars.push(EnvVar {
            name: "AWS_SECRET_ACCESS_KEY".into(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: creds.name.clone(),
                    key: secret_key,
                    optional: Some(false),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
    }

    let writer_volume = Volume {
        name: "kernel-writer".into(),
        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
            claim_name: writer_claim,
            read_only: Some(false),
        }),
        ..Default::default()
    };
    let writer_mount = VolumeMount {
        name: "kernel-writer".into(),
        mount_path: WRITER_MOUNT_PATH.into(),
        ..Default::default()
    };

    let cache_volume = Volume {
        name: "aws-cache".into(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    };
    let cache_mount = VolumeMount {
        name: "aws-cache".into(),
        mount_path: "/tmp".into(),
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
            backoff_limit: Some(3),
            ttl_seconds_after_finished: Some(120),
            active_deadline_seconds: Some(600),
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    restart_policy: Some("OnFailure".into()),
                    // The sync Job makes no Kubernetes API calls.
                    automount_service_account_token: Some(false),
                    // fsGroup so the runAsUser=1000 container can write the
                    // writer PVC mounted at /kernels.
                    security_context: Some(PodSecurityContext {
                        fs_group: Some(1000),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "sync".into(),
                        image: Some(image.into()),
                        image_pull_policy: Some("IfNotPresent".into()),
                        command: Some(vec!["sh".into(), "-c".into()]),
                        args: Some(vec![format!(
                            "aws --endpoint-url \"$ENDPOINT\" s3 sync \"s3://${{BUCKET}}/${{PREFIX}}\" {sync_target}"
                        )]),
                        env: Some(env_vars),
                        volume_mounts: Some(vec![writer_mount, cache_mount]),
                        security_context: Some(hardened_security_context()),
                        ..Default::default()
                    }],
                    volumes: Some(vec![writer_volume, cache_volume]),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Create the one-shot S3 sync Job for a workspace. The Job name is
/// deterministic (`kernel-sync-<workspace>`), so a concurrent or repeat
/// reconcile that races an existing Job is treated as success (idempotent).
pub async fn create_s3_sync_job(
    client: &kube::Client,
    workspace: &str,
    namespace: &str,
    s3: &S3Spec,
    image: &str,
) -> Result<(), kube::Error> {
    let job = build_s3_sync_job(workspace, namespace, s3, image);
    let job_name = job.metadata.name.clone().unwrap_or_default();
    let api: kube::Api<Job> = kube::Api::namespaced(client.clone(), namespace);

    match api.create(&kube::api::PostParams::default(), &job).await {
        Ok(_) => {
            tracing::info!("created kernel S3 sync Job {job_name} in namespace {namespace}");
            Ok(())
        }
        Err(kube::Error::Api(ae)) if ae.code == 409 => {
            tracing::info!(
                "kernel S3 sync Job {job_name} already exists in namespace {namespace}, treating as success"
            );
            Ok(())
        }
        Err(e) => {
            tracing::warn!("failed to create kernel S3 sync Job {job_name}: {e}");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::storage::SecretRef;

    const TEST_IMAGE: &str = "amazon/aws-cli@sha256:test";

    fn sample_s3(credentials: Option<SecretRef>) -> S3Spec {
        S3Spec {
            endpoint: "http://versitygw:7070".into(),
            bucket: "sycophant-tenants".into(),
            prefix: "tenant-abc/mainframe/".into(),
            region: "us-east-1".into(),
            force_path_style: true,
            credentials,
        }
    }

    fn with_creds() -> S3Spec {
        sample_s3(Some(SecretRef {
            name: "tenant-s3-credentials".into(),
            access_key_id_key: None,
            secret_access_key_key: None,
        }))
    }

    fn container(job: &Job) -> Container {
        job.spec
            .as_ref()
            .unwrap()
            .template
            .spec
            .as_ref()
            .unwrap()
            .containers[0]
            .clone()
    }

    fn env_of(container: &Container, name: &str) -> EnvVar {
        container
            .env
            .as_ref()
            .unwrap()
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("env var {name} must be present"))
            .clone()
    }

    #[test]
    fn sync_job_has_deterministic_name_and_namespace() {
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        assert_eq!(job.metadata.name.unwrap(), "kernel-sync-alice");
        assert_eq!(job.metadata.namespace.unwrap(), "tenant-abc");
    }

    #[test]
    fn sync_job_command_targets_the_writer_mount() {
        // The per-workspace writer PV points at <base>/<ns>/<workspace>, so the
        // sync target is the mount root `/kernels` — NOT `/kernels/<workspace>`.
        // Mutant re-adding the `<workspace>` segment nests the persona one level
        // too deep (/kernels/alice) and the serving pod reads an empty dir.
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        let c = container(&job);
        let args = c.args.as_ref().unwrap();
        let cmd = &args[0];
        assert!(cmd.contains("s3 sync"), "must run `s3 sync`: {cmd}");
        assert!(cmd.contains("${BUCKET}"), "must reference $BUCKET: {cmd}");
        assert!(cmd.contains("${PREFIX}"), "must reference $PREFIX: {cmd}");
        assert!(
            cmd.contains("/kernels"),
            "must sync to the writer mount: {cmd}"
        );
        assert!(
            !cmd.contains("/kernels/alice"),
            "must sync to /kernels, not a nested workspace subdir: {cmd}"
        );
    }

    #[test]
    fn sync_job_env_carries_s3_source() {
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        let c = container(&job);
        assert_eq!(
            env_of(&c, "ENDPOINT").value.unwrap(),
            "http://versitygw:7070"
        );
        assert_eq!(env_of(&c, "BUCKET").value.unwrap(), "sycophant-tenants");
        assert_eq!(env_of(&c, "PREFIX").value.unwrap(), "tenant-abc/mainframe/");
        assert_eq!(env_of(&c, "AWS_DEFAULT_REGION").value.unwrap(), "us-east-1");
        assert_eq!(env_of(&c, "HOME").value.unwrap(), "/tmp");
    }

    #[test]
    fn sync_job_credentials_use_secret_key_ref_with_default_keys() {
        // Mutant: drop the cred env vars and aws-cli falls back to no creds →
        // 403 against the gateway. The secretKeyRef must point at the named
        // Secret with the documented default keys.
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        let c = container(&job);

        let access = env_of(&c, "AWS_ACCESS_KEY_ID");
        let access_ref = access.value_from.unwrap().secret_key_ref.unwrap();
        assert_eq!(access_ref.name, "tenant-s3-credentials");
        assert_eq!(access_ref.key, "access-key-id");
        assert_eq!(access_ref.optional, Some(false));

        let secret = env_of(&c, "AWS_SECRET_ACCESS_KEY");
        let secret_ref = secret.value_from.unwrap().secret_key_ref.unwrap();
        assert_eq!(secret_ref.name, "tenant-s3-credentials");
        assert_eq!(secret_ref.key, "secret-access-key");
        assert_eq!(secret_ref.optional, Some(false));
    }

    #[test]
    fn sync_job_credentials_honor_explicit_keys() {
        let s3 = sample_s3(Some(SecretRef {
            name: "creds".into(),
            access_key_id_key: Some("id".into()),
            secret_access_key_key: Some("secret".into()),
        }));
        let job = build_s3_sync_job("alice", "tenant-abc", &s3, TEST_IMAGE);
        let c = container(&job);
        let access_ref = env_of(&c, "AWS_ACCESS_KEY_ID")
            .value_from
            .unwrap()
            .secret_key_ref
            .unwrap();
        assert_eq!(access_ref.key, "id");
        let secret_ref = env_of(&c, "AWS_SECRET_ACCESS_KEY")
            .value_from
            .unwrap()
            .secret_key_ref
            .unwrap();
        assert_eq!(secret_ref.key, "secret");
    }

    #[test]
    fn sync_job_without_credentials_omits_cred_env_vars() {
        // credentials: None means the operator wired AWS_* another way. Emitting
        // a secretKeyRef to an unnamed Secret would wedge the pod at startup.
        let job = build_s3_sync_job("alice", "tenant-abc", &sample_s3(None), TEST_IMAGE);
        let c = container(&job);
        let names: Vec<&str> = c
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(!names.contains(&"AWS_ACCESS_KEY_ID"));
        assert!(!names.contains(&"AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn sync_job_mounts_writer_pvc_read_write_at_kernels() {
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        let pod = job.spec.as_ref().unwrap().template.spec.as_ref().unwrap();
        let writer_vol = pod
            .volumes
            .as_ref()
            .unwrap()
            .iter()
            .find(|v| v.name == "kernel-writer")
            .expect("writer volume must be present");
        let pvc = writer_vol.persistent_volume_claim.as_ref().unwrap();
        assert_eq!(pvc.claim_name, "kernel-writer-alice");
        assert_eq!(pvc.read_only, Some(false), "writer PVC must be read-write");

        let mount = container(&job)
            .volume_mounts
            .unwrap()
            .into_iter()
            .find(|m| m.name == "kernel-writer")
            .unwrap();
        assert_eq!(mount.mount_path, "/kernels");
    }

    #[test]
    fn sync_job_pod_is_one_shot_and_unprivileged() {
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        let spec = job.spec.as_ref().unwrap();
        assert_eq!(spec.backoff_limit, Some(3));
        assert_eq!(spec.ttl_seconds_after_finished, Some(120));
        assert_eq!(spec.active_deadline_seconds, Some(600));
        let pod = spec.template.spec.as_ref().unwrap();
        assert_eq!(pod.restart_policy.as_deref(), Some("OnFailure"));
        assert_eq!(pod.automount_service_account_token, Some(false));
        assert_eq!(pod.security_context.as_ref().unwrap().fs_group, Some(1000));
        let sc = container(&job).security_context.unwrap();
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(sc.capabilities.unwrap().drop, Some(vec!["ALL".to_string()]));
    }

    #[test]
    fn sync_job_labels_mark_component_and_workspace() {
        let job = build_s3_sync_job("alice", "tenant-abc", &with_creds(), TEST_IMAGE);
        let labels = job.metadata.labels.unwrap();
        assert_eq!(labels["app.kubernetes.io/part-of"], "sycophant");
        assert_eq!(labels["app.kubernetes.io/component"], "kernel-sync");
        assert_eq!(labels["sycophant.md/type"], "kernel-sync");
        assert_eq!(labels["sycophant.md/workspace"], "alice");
    }
}
