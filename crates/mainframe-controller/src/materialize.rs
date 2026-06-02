//! Materialize a Workspace CR into its children: a Pod, a
//! ServiceAccount, and a PersistentVolumeClaim. Each child carries an
//! ownerRef back to the Workspace so cascade delete and the finalizer
//! cleanup work naturally. Network egress for the workspace is owned
//! by the tenant-chart CiliumNetworkPolicy `workspace-egress`
//! (`charts/sycophant-tenant/templates/workspace-netpol.yaml`).

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{PersistentVolumeClaim, Pod, PodSpec, ServiceAccount};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{Api, Patch, PatchParams};
use kube::Client;
use serde_json::{json, Value};

use crate::crd::{KernelSpec, Workspace};

/// Field-manager string used for server-side apply on every child the
/// controller materializes. Other writers (helm, kubectl) own different
/// fields — SSA's conflict resolution lets us coexist if necessary.
pub const FIELD_MANAGER: &str = "mainframe-controller";

const WORKSPACE_GROUP: &str = "sycophant.md";
const WORKSPACE_VERSION: &str = "v1";
const WORKSPACE_KIND: &str = "Workspace";

/// Transponder pods terminate fast — interactive sessions, not HA. The
/// `cluster-transponder-pod-policy` VAP keys on the label pair below,
/// so they MUST be present on the Pod metadata for admission to match.
const WORKSPACE_TERMINATION_GRACE_SECONDS: i64 = 5;

/// CPU/memory limits applied to the transponder sidecar regardless of
/// the workspace's own resource budget.
const TRANSPONDER_CPU_LIMIT: &str = "500m";
const TRANSPONDER_MEMORY_LIMIT: &str = "256Mi";

/// CPU/memory requests+limits for the optional S3-sync init container.
const S3_SYNC_CPU_REQUEST: &str = "100m";
const S3_SYNC_MEMORY_REQUEST: &str = "64Mi";
const S3_SYNC_CPU_LIMIT: &str = "500m";
const S3_SYNC_MEMORY_LIMIT: &str = "256Mi";

const S3_SYNC_IMAGE: &str =
    "amazon/aws-cli@sha256:db8d39443ef512d4724becfac59ec9b7c4f8621e7b3c6200be56cd4fc2dc9570";

/// Materialization-time data the controller learns from its own pod
/// environment (chart-injected) rather than from each Workspace CR.
/// Carried into the spec builders so unit tests can supply explicit
/// values instead of mutating process env.
#[derive(Clone, Debug)]
pub struct MaterializationContext {
    /// Helm release name. Embedded in the workspace pod's affinity
    /// selector so the workspace schedules on the same node as the
    /// per-tenant tightbeam-ctrl. Matches the
    /// `app.kubernetes.io/instance` label helm applies to the
    /// tightbeam-ctrl Pod.
    pub release_name: String,
    /// Transponder pod image.
    pub transponder_image: String,
    pub transponder_tag: String,
    pub transponder_pull_policy: String,
}

impl MaterializationContext {
    pub fn from_env() -> Result<Self, String> {
        let release_name = std::env::var("RELEASE_NAME")
            .map_err(|_| "RELEASE_NAME env var not set".to_string())?;
        let transponder_image = std::env::var("TRANSPONDER_IMAGE")
            .map_err(|_| "TRANSPONDER_IMAGE env var not set".to_string())?;
        let transponder_tag = std::env::var("TRANSPONDER_TAG")
            .map_err(|_| "TRANSPONDER_TAG env var not set".to_string())?;
        let transponder_pull_policy =
            std::env::var("TRANSPONDER_PULL_POLICY").unwrap_or_else(|_| "IfNotPresent".into());
        Ok(Self {
            release_name,
            transponder_image,
            transponder_tag,
            transponder_pull_policy,
        })
    }
}

/// Build the OwnerReference set on each materialized child. K8s GC uses
/// this to cascade deletion when the Workspace is removed (after the
/// finalizer confirms the Pod is gone).
fn workspace_owner_ref(workspace: &Workspace) -> OwnerReference {
    OwnerReference {
        api_version: format!("{}/{}", WORKSPACE_GROUP, WORKSPACE_VERSION),
        kind: WORKSPACE_KIND.to_string(),
        name: workspace.metadata.name.clone().unwrap_or_default(),
        uid: workspace.metadata.uid.clone().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

/// Common labels for every materialized child. Mirrors what the chart's
/// `sycophant.workspaceLabels` helper produced for the legacy
/// templates. Per ADR 018 decision 2, the pod is now the transponder
/// (not the "workspace") — `app.kubernetes.io/component=transponder`.
/// The `sycophant.md/workspace` label is the workspace identity used
/// by the mutual `podAffinity` selector that co-locates the workspace's
/// PVC mounters (airlock chamber jobs).
fn workspace_labels(name: &str, release: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/managed-by".into(), "Helm".into());
    labels.insert("app.kubernetes.io/instance".into(), release.to_string());
    labels.insert("app.kubernetes.io/part-of".into(), "sycophant".into());
    labels.insert("app.kubernetes.io/component".into(), "transponder".into());
    labels.insert("app.kubernetes.io/name".into(), name.to_string());
    labels.insert("sycophant.md/workspace".into(), name.to_string());
    labels
}

fn child_metadata(
    name: String,
    namespace: &str,
    workspace: &Workspace,
    release: &str,
) -> ObjectMeta {
    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    ObjectMeta {
        name: Some(name),
        namespace: Some(namespace.to_string()),
        labels: Some(workspace_labels(ws_name, release)),
        owner_references: Some(vec![workspace_owner_ref(workspace)]),
        ..Default::default()
    }
}

/// Per-workspace ServiceAccount. Pod identity for the transponder pod.
fn service_account_for(namespace: &str, workspace: &Workspace, release: &str) -> ServiceAccount {
    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    let mut meta = child_metadata(format!("sa-{ws_name}"), namespace, workspace, release);
    meta.labels
        .get_or_insert_with(BTreeMap::new)
        .insert("sycophant.md/type".into(), "workspace-sa".into());
    ServiceAccount {
        metadata: meta,
        ..Default::default()
    }
}

/// Per-workspace PVC at `/workspace` inside the workspace pod
/// (writable scratch). Fixed 1Gi; `Workspace.spec.storage` is accepted
/// in the schema but not yet consumed here.
fn pvc_for(namespace: &str, workspace: &Workspace, release: &str) -> PersistentVolumeClaim {
    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    let meta = child_metadata(
        format!("{ws_name}-workspace-data"),
        namespace,
        workspace,
        release,
    );

    let storage = workspace
        .spec
        .storage
        .clone()
        .unwrap_or_else(|| "1Gi".to_string());
    let mut requests = BTreeMap::new();
    requests.insert("storage".to_string(), Quantity(storage));

    PersistentVolumeClaim {
        metadata: meta,
        spec: Some(k8s_openapi::api::core::v1::PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            resources: Some(k8s_openapi::api::core::v1::VolumeResourceRequirements {
                requests: Some(requests),
                limits: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Per-workspace transponder Pod. Single container post-ADR 018;
/// tool calls cross the cluster network to airlock-ctrl, which spawns
/// per-call chamber Jobs whose filesystem effects land on the workspace
/// PVC. The
/// `cluster-transponder-pod-policy` VAP enforces the security envelope
/// (gvisor, drop ALL, runAsNonRoot, resource limits, hostPath
/// whitelist for `kernel` only, etc.) at admission time, keyed on the
/// `app.kubernetes.io/part-of=sycophant` + `app.kubernetes.io/component=
/// transponder` label pair this function sets.
fn pod_for(namespace: &str, ctx: &MaterializationContext, workspace: &Workspace) -> Pod {
    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    let labels = workspace_labels(ws_name, &ctx.release_name);

    let mut volumes = vec![
        json!({ "name": "tmp", "emptyDir": {} }),
        json!({ "name": "agent-home", "emptyDir": {} }),
        json!({
            "name": "workspace-data",
            "persistentVolumeClaim": { "claimName": format!("{ws_name}-workspace-data") }
        }),
        // Three custom-audience projected SA tokens, one per (consumer ×
        // verifier) pair. Each carries a single audience so a stolen token
        // does not unlock the other verifiers. The LLM-job pod mints its
        // own llm-dispatch token in tightbeam-controller/job.rs.
        json!({
            "name": "transponder-auth",
            "projected": {
                "sources": [
                    { "serviceAccountToken": {
                        "path": "token",
                        "audience": shared::auth::TRANSPONDER_TIGHTBEAM_AUDIENCE,
                        "expirationSeconds": 3600
                    }}
                ]
            }
        }),
        json!({
            "name": "transponder-airlock-auth",
            "projected": {
                "sources": [
                    { "serviceAccountToken": {
                        "path": "token",
                        "audience": shared::auth::TRANSPONDER_AIRLOCK_AUDIENCE,
                        "expirationSeconds": 3600
                    }}
                ]
            }
        }),
    ];

    // Optional kernel volume + init container for S3-kind.
    let mut init_containers: Vec<Value> = Vec::new();
    let has_kernel = workspace.spec.kernel.is_some();
    if let Some(kernel) = workspace.spec.kernel.as_ref() {
        match kernel.kind.as_str() {
            "HostPath" => {
                let path = kernel
                    .host_path
                    .as_ref()
                    .map(|hp| hp.path.clone())
                    .unwrap_or_default();
                volumes.push(json!({
                    "name": "kernel",
                    "hostPath": { "path": path, "type": "Directory" }
                }));
            }
            "S3" => {
                volumes.push(json!({ "name": "kernel", "emptyDir": {} }));
                volumes.push(json!({ "name": "aws-cache", "emptyDir": {} }));
                init_containers.push(s3_sync_init_container(kernel));
            }
            _ => {
                // Unknown kind: emit no volume; transponder mount stays
                // gated by `has_kernel` so the pod still starts.
            }
        }
    }

    // Single container: transponder. Per ADR 018 the mainframe-runtime
    // sidecar is gone; ALL tool calls (stdlib and tenant-defined) flow
    // through airlock-controller, which dispatches to chamber pods.
    let mut transponder = json!({
        "name": "transponder",
        "image": format!("{}:{}", ctx.transponder_image, ctx.transponder_tag),
        "imagePullPolicy": ctx.transponder_pull_policy,
        "securityContext": {
            "readOnlyRootFilesystem": true,
            "allowPrivilegeEscalation": false,
            "capabilities": { "drop": ["ALL"] }
        },
        "resources": {
            "limits": { "cpu": TRANSPONDER_CPU_LIMIT, "memory": TRANSPONDER_MEMORY_LIMIT }
        },
        "env": [
            { "name": "TIGHTBEAM_CONTROLLER_ADDR",   "value": "http://tightbeam-ctrl:9090" },
            { "name": "AIRLOCK_CONTROLLER_ADDR",     "value": "http://airlock-ctrl:9090" }
        ],
    });
    // Two audience-bound auth token mounts (one per controller the transponder
    // calls). Kernel mount is appended only when the workspace declares one.
    // Paths must match `shared::auth::TRANSPONDER_TIGHTBEAM_TOKEN_PATH` and
    // `TRANSPONDER_AIRLOCK_TOKEN_PATH` (parent dirs).
    let mut transponder_mounts = vec![
        json!({
            "name": "transponder-auth",
            "mountPath": "/var/run/secrets/transponder/tightbeam",
            "readOnly": true
        }),
        json!({
            "name": "transponder-airlock-auth",
            "mountPath": "/var/run/secrets/transponder/airlock",
            "readOnly": true
        }),
    ];
    if has_kernel {
        transponder_mounts.push(json!({
            "name": "kernel", "mountPath": "/etc/kernel", "readOnly": true
        }));
    }
    transponder["volumeMounts"] = json!(transponder_mounts);
    transponder["ports"] = json!([{ "name": "healthz", "containerPort": 8080 }]);
    transponder["readinessProbe"] = json!({
        "httpGet": { "path": "/healthz", "port": "healthz" },
        "initialDelaySeconds": 5,
        "periodSeconds": 5,
        "failureThreshold": 3,
    });
    transponder["livenessProbe"] = json!({
        "httpGet": { "path": "/healthz", "port": "healthz" },
        "initialDelaySeconds": 10,
        "periodSeconds": 10,
        "failureThreshold": 3,
    });

    let spec_json = json!({
        "runtimeClassName": "gvisor",
        "serviceAccountName": format!("sa-{ws_name}"),
        // Suppress kubelet's default kube-apiserver-audience token mount;
        // we supply our own custom-audience projected tokens via the
        // `transponder-*-auth` volumes above. Workspace VAP forbids the
        // default audience (charts/sycophant-cluster/templates/workspace-vap.yaml).
        "automountServiceAccountToken": false,
        "terminationGracePeriodSeconds": WORKSPACE_TERMINATION_GRACE_SECONDS,
        "tolerations": [
            {
                "key": "sycophant.md/workload",
                "operator": "Equal",
                "value": "workspace",
                "effect": "NoSchedule"
            }
        ],
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": 1000,
            "runAsGroup": 1000,
            "fsGroup": 1000,
            "seccompProfile": { "type": "RuntimeDefault" }
        },
        "initContainers": init_containers,
        "containers": [transponder],
        "volumes": volumes
    });

    let spec: PodSpec = serde_json::from_value(spec_json)
        .expect("workspace PodSpec construction must produce valid JSON");

    Pod {
        metadata: ObjectMeta {
            name: Some(ws_name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            owner_references: Some(vec![workspace_owner_ref(workspace)]),
            ..Default::default()
        },
        spec: Some(spec),
        ..Default::default()
    }
}

/// Apply all four child resources for a Workspace via server-side
/// apply. Idempotent: re-applying the same Workspace is a no-op at the
/// K8s API layer (SSA returns the existing object unchanged).
pub async fn materialize_children(
    client: &Client,
    namespace: &str,
    ctx: &MaterializationContext,
    workspace: &Workspace,
) -> anyhow::Result<()> {
    let sa = service_account_for(namespace, workspace, &ctx.release_name);
    let pvc = pvc_for(namespace, workspace, &ctx.release_name);
    let pod = pod_for(namespace, ctx, workspace);

    let pp = PatchParams::apply(FIELD_MANAGER).force();

    let sa_name = sa.metadata.name.clone().unwrap_or_default();
    let sa_api: Api<ServiceAccount> = Api::namespaced(client.clone(), namespace);
    sa_api.patch(&sa_name, &pp, &Patch::Apply(&sa)).await?;

    let pvc_name = pvc.metadata.name.clone().unwrap_or_default();
    let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), namespace);
    pvc_api.patch(&pvc_name, &pp, &Patch::Apply(&pvc)).await?;

    let pod_name = pod.metadata.name.clone().unwrap_or_default();
    let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    pod_api.patch(&pod_name, &pp, &Patch::Apply(&pod)).await?;

    Ok(())
}

/// True when the workspace Pod still exists in the namespace. The
/// finalizer (see `finalizer.rs`) uses this as the gate: the
/// Workspace's deletion isn't reported complete until the Pod is gone.
pub async fn pod_child_exists(
    client: &Client,
    namespace: &str,
    workspace_name: &str,
) -> anyhow::Result<bool> {
    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    match api.get_opt(workspace_name).await? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

fn s3_sync_init_container(kernel: &KernelSpec) -> Value {
    let s3 = kernel.s3.as_ref();
    let endpoint = s3.map(|s| s.endpoint.clone()).unwrap_or_default();
    let bucket = s3.map(|s| s.bucket.clone()).unwrap_or_default();
    let prefix = s3.map(|s| s.prefix.clone()).unwrap_or_default();
    let region = s3
        .map(|s| s.region.clone())
        .unwrap_or_else(|| "us-east-1".to_string());
    let creds = s3.and_then(|s| s.credentials.as_ref());
    let secret_name = creds.map(|c| c.name.clone()).unwrap_or_default();
    let access_key = creds
        .and_then(|c| c.access_key_id_key.clone())
        .unwrap_or_else(|| "access-key-id".to_string());
    let secret_key = creds
        .and_then(|c| c.secret_access_key_key.clone())
        .unwrap_or_else(|| "secret-access-key".to_string());

    json!({
        "name": "kernel-sync",
        "image": S3_SYNC_IMAGE,
        "imagePullPolicy": "IfNotPresent",
        "securityContext": {
            "readOnlyRootFilesystem": true,
            "allowPrivilegeEscalation": false,
            "capabilities": { "drop": ["ALL"] }
        },
        "resources": {
            "requests": { "cpu": S3_SYNC_CPU_REQUEST, "memory": S3_SYNC_MEMORY_REQUEST },
            "limits":   { "cpu": S3_SYNC_CPU_LIMIT,   "memory": S3_SYNC_MEMORY_LIMIT }
        },
        "command": ["/bin/sh", "-c"],
        "args": [
            "aws --endpoint-url \"$ENDPOINT\" s3 sync \"s3://${BUCKET}/${PREFIX}\" /etc/kernel"
        ],
        "env": [
            { "name": "HOME", "value": "/tmp" },
            { "name": "ENDPOINT", "value": endpoint },
            { "name": "BUCKET", "value": bucket },
            { "name": "PREFIX", "value": prefix },
            { "name": "AWS_DEFAULT_REGION", "value": region },
            {
                "name": "AWS_ACCESS_KEY_ID",
                "valueFrom": {
                    "secretKeyRef": { "name": secret_name, "key": access_key }
                }
            },
            {
                "name": "AWS_SECRET_ACCESS_KEY",
                "valueFrom": {
                    "secretKeyRef": { "name": secret_name.clone(), "key": secret_key }
                }
            }
        ],
        "volumeMounts": [
            { "name": "kernel", "mountPath": "/etc/kernel" },
            { "name": "aws-cache", "mountPath": "/tmp" }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::WorkspaceSpec;
    use shared::storage::{HostPathSpec, S3Spec, SecretRef};

    fn make_workspace(name: &str, uid: &str, spec: WorkspaceSpec) -> Workspace {
        let mut w = Workspace::new(name, spec);
        w.metadata.uid = Some(uid.to_string());
        w
    }

    fn minimal_spec() -> WorkspaceSpec {
        WorkspaceSpec {
            transponder: None,
            storage: None,
            kernel: None,
            kernels: vec![],
            chambers: vec![],
        }
    }

    fn ctx() -> MaterializationContext {
        MaterializationContext {
            release_name: "test".into(),
            transponder_image: "ghcr.io/sycophant/transponder".into(),
            transponder_tag: "v0.1".into(),
            transponder_pull_policy: "IfNotPresent".into(),
        }
    }

    fn assert_owner_ref(refs: Option<&Vec<OwnerReference>>, ws_name: &str, uid: &str) {
        let refs = refs.expect("ownerReferences must be set");
        assert_eq!(refs.len(), 1);
        let o = &refs[0];
        assert_eq!(o.api_version, "sycophant.md/v1");
        assert_eq!(o.kind, "Workspace");
        assert_eq!(o.name, ws_name);
        assert_eq!(o.uid, uid);
        assert_eq!(o.controller, Some(true));
        assert_eq!(o.block_owner_deletion, Some(true));
    }

    #[test]
    fn pod_carries_workspace_owner_ref() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        assert_owner_ref(pod.metadata.owner_references.as_ref(), "demo", "abc-123");
    }

    #[test]
    fn pod_name_and_namespace_match_workspace() {
        // The finalizer's `pod_child_exists` looks up the Pod by
        // workspace name; mismatched name would leak Pods.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        assert_eq!(pod.metadata.name.as_deref(), Some("demo"));
        assert_eq!(pod.metadata.namespace.as_deref(), Some("e2e-test"));
    }

    #[test]
    fn pod_carries_transponder_label_pair_for_vap_scope() {
        // The cluster-transponder-pod-policy VAP's matchConditions key
        // on this label pair (per ADR 018 decision 2 the component is
        // `transponder`, not `workspace`). Dropping either label
        // silently stops the VAP from matching — the controller-emitted
        // Pod would pass admission unvalidated. This test is the
        // structural guarantee that the labels are always present.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let labels = pod.metadata.labels.as_ref().expect("labels present");
        assert_eq!(
            labels.get("app.kubernetes.io/part-of").map(String::as_str),
            Some("sycophant"),
            "VAP scoping requires app.kubernetes.io/part-of=sycophant"
        );
        assert_eq!(
            labels
                .get("app.kubernetes.io/component")
                .map(String::as_str),
            Some("transponder"),
            "VAP scoping requires app.kubernetes.io/component=transponder"
        );
    }

    #[test]
    fn pod_carries_sycophant_workspace_label_for_mutual_affinity() {
        // The `sycophant.md/workspace=<ws>` label is the anchor for the
        // mutual podAffinity selector that co-locates the workspace's
        // PVC mounters (airlock chamber jobs). Deleting
        // it would silently break RWO mount correctness on multi-node
        // clusters.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let labels = pod.metadata.labels.as_ref().expect("labels present");
        assert_eq!(
            labels.get("sycophant.md/workspace").map(String::as_str),
            Some("demo")
        );
    }

    // The 6 sandbox-field tests below verify the materializer produces a
    // workspace pod with each required security field set. Without them,
    // a regression in this file (e.g. a refactor that drops `runtimeClassName`)
    // only surfaces when the VAP rejects the pod at admission time — slow
    // feedback, pointing at the chart rather than the broken Rust.

    #[test]
    fn pod_uses_gvisor_runtime_class() {
        let pod = pod_value("demo", minimal_spec());
        assert_eq!(
            pod.pointer("/spec/runtimeClassName")
                .and_then(|v| v.as_str()),
            Some("gvisor"),
        );
    }

    #[test]
    fn pod_runs_as_non_root_via_pod_security_context() {
        let pod = pod_value("demo", minimal_spec());
        assert_eq!(
            pod.pointer("/spec/securityContext/runAsNonRoot")
                .and_then(|v| v.as_bool()),
            Some(true),
        );
    }

    #[test]
    fn pod_uses_seccomp_runtime_default() {
        let pod = pod_value("demo", minimal_spec());
        assert_eq!(
            pod.pointer("/spec/securityContext/seccompProfile/type")
                .and_then(|v| v.as_str()),
            Some("RuntimeDefault"),
        );
    }

    #[test]
    fn every_container_drops_all_capabilities() {
        // S3 kernel fixture so the kernel-sync init container is exercised
        // alongside the transponder container.
        let pod = pod_value("demo", s3_kernel_spec());
        let containers = all_containers(&pod);
        assert!(
            containers.len() >= 2,
            "expected transponder + kernel-sync init",
        );
        for c in containers {
            let name = c["name"].as_str().unwrap_or("<unnamed>");
            let drops = c
                .pointer("/securityContext/capabilities/drop")
                .and_then(|v| v.as_array())
                .unwrap_or_else(|| panic!("container `{name}` missing capabilities.drop"));
            let drops: Vec<&str> = drops.iter().filter_map(|d| d.as_str()).collect();
            assert_eq!(drops, vec!["ALL"], "container `{name}` must drop=[ALL]");
        }
    }

    #[test]
    fn every_container_disables_root_filesystem_writes() {
        let pod = pod_value("demo", s3_kernel_spec());
        let containers = all_containers(&pod);
        assert!(
            containers.len() >= 2,
            "expected transponder + kernel-sync init",
        );
        for c in containers {
            let name = c["name"].as_str().unwrap_or("<unnamed>");
            assert_eq!(
                c.pointer("/securityContext/readOnlyRootFilesystem")
                    .and_then(|v| v.as_bool()),
                Some(true),
                "container `{name}` must set readOnlyRootFilesystem=true",
            );
        }
    }

    #[test]
    fn every_container_blocks_privilege_escalation() {
        let pod = pod_value("demo", s3_kernel_spec());
        let containers = all_containers(&pod);
        assert!(
            containers.len() >= 2,
            "expected transponder + kernel-sync init",
        );
        for c in containers {
            let name = c["name"].as_str().unwrap_or("<unnamed>");
            assert_eq!(
                c.pointer("/securityContext/allowPrivilegeEscalation")
                    .and_then(|v| v.as_bool()),
                Some(false),
                "container `{name}` must set allowPrivilegeEscalation=false",
            );
        }
    }

    #[test]
    fn pod_terminates_in_five_seconds() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        assert_eq!(
            pod.spec
                .as_ref()
                .and_then(|s| s.termination_grace_period_seconds),
            Some(WORKSPACE_TERMINATION_GRACE_SECONDS),
        );
    }

    #[test]
    fn sa_pvc_all_carry_owner_ref() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sa = service_account_for("e2e-test", &ws, "test");
        let pvc = pvc_for("e2e-test", &ws, "test");
        assert_owner_ref(sa.metadata.owner_references.as_ref(), "demo", "abc-123");
        assert_owner_ref(pvc.metadata.owner_references.as_ref(), "demo", "abc-123");
    }

    #[test]
    fn sa_name_uses_sa_prefix_per_chart_convention() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sa = service_account_for("e2e-test", &ws, "test");
        assert_eq!(sa.metadata.name.as_deref(), Some("sa-demo"));
        assert_eq!(sa.metadata.namespace.as_deref(), Some("e2e-test"));
        let labels = sa.metadata.labels.as_ref().expect("labels present");
        assert_eq!(
            labels.get("sycophant.md/type").map(String::as_str),
            Some("workspace-sa")
        );
        assert_eq!(
            labels.get("app.kubernetes.io/name").map(String::as_str),
            Some("demo")
        );
    }

    #[test]
    fn pvc_access_modes_is_read_write_once() {
        // Kills the mutant deleting access_modes from PVC spec. RWO is
        // load-bearing: the workspace pod is single-instance per workspace;
        // RWX would silently allow multi-attach if another workload appeared.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pvc = pvc_for("e2e-test", &ws, "test");
        let access_modes = pvc
            .spec
            .as_ref()
            .and_then(|s| s.access_modes.as_ref())
            .expect("access_modes must be set on workspace PVC");
        assert_eq!(access_modes, &vec!["ReadWriteOnce".to_string()]);
    }

    #[test]
    fn pvc_request_uses_default_1gi_when_workspace_storage_absent() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pvc = pvc_for("e2e-test", &ws, "test");
        let storage = pvc
            .spec
            .as_ref()
            .and_then(|s| s.resources.as_ref())
            .and_then(|r| r.requests.as_ref())
            .and_then(|m| m.get("storage"))
            .map(|q| q.0.clone())
            .unwrap_or_default();
        assert_eq!(storage, "1Gi");
    }

    #[test]
    fn pvc_request_uses_workspace_storage_when_set() {
        let mut spec = minimal_spec();
        spec.storage = Some("5Gi".into());
        let ws = make_workspace("demo", "abc-123", spec);
        let pvc = pvc_for("e2e-test", &ws, "test");
        let storage = pvc
            .spec
            .as_ref()
            .and_then(|s| s.resources.as_ref())
            .and_then(|r| r.requests.as_ref())
            .and_then(|m| m.get("storage"))
            .map(|q| q.0.clone())
            .unwrap_or_default();
        assert_eq!(storage, "5Gi");
    }

    #[test]
    fn pvc_name_uses_workspace_data_suffix_per_chart_convention() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pvc = pvc_for("e2e-test", &ws, "test");
        assert_eq!(pvc.metadata.name.as_deref(), Some("demo-workspace-data"));
    }

    /// Render the typed Pod to JSON for pointer-based assertions in the
    /// remaining tests. Beats hand-walking `Option<Spec> -> Option<Vec>`
    /// for every nested field.
    fn pod_json(pod: &Pod) -> Value {
        serde_json::to_value(pod).expect("Pod serializes to JSON")
    }

    #[test]
    fn pod_has_no_podaffinity_post_adr_018() {
        // Per ADR 018 Stage 3: the transponder schedules freely. The old
        // affinity pin to tightbeam-ctrl is gone (controllers stay off
        // adversarial-pod nodes via their own podAntiAffinity, not by
        // the transponder pinning to them).
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        assert!(
            value.pointer("/spec/affinity").is_none(),
            "transponder pod must not carry any affinity rules"
        );
    }

    #[test]
    fn pod_has_single_transponder_container() {
        // Per ADR 018 the mainframe-runtime sidecar is dropped. Pin
        // single-container shape so a regression that re-adds a sidecar
        // gets caught here.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        let containers = value
            .pointer("/spec/containers")
            .and_then(|c| c.as_array())
            .expect("containers array present");
        assert_eq!(containers.len(), 1, "expected single container (transponder)");
        assert_eq!(containers[0]["name"], "transponder");
    }

    #[test]
    fn pod_transponder_container_uses_ctx_image_tag() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        let transponder_image = value
            .pointer("/spec/containers/0/image")
            .expect("transponder container image present");
        assert_eq!(transponder_image, "ghcr.io/sycophant/transponder:v0.1");
    }

    #[test]
    fn pod_with_hostpath_kernel_emits_volume_and_no_init() {
        let mut spec = minimal_spec();
        spec.kernel = Some(KernelSpec {
            kind: "HostPath".into(),
            host_path: Some(HostPathSpec {
                path: "/host/sycophant/demo".into(),
            }),
            s3: None,
        });
        let ws = make_workspace("demo", "abc-123", spec);
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        let volumes = value
            .pointer("/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("volumes present");
        let kernel_vol = volumes
            .iter()
            .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("kernel"))
            .expect("kernel volume present");
        assert_eq!(kernel_vol["hostPath"]["path"], "/host/sycophant/demo");
        assert_eq!(kernel_vol["hostPath"]["type"], "Directory");
        // No S3 sync init container for HostPath. `initContainers` may
        // be absent from the typed Pod when empty, which is correct.
        let init_count = value
            .pointer("/spec/initContainers")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(
            init_count, 0,
            "HostPath kernel should not emit an init container"
        );
    }

    #[test]
    fn pod_with_s3_kernel_emits_init_container_and_aws_cache_volume() {
        let mut spec = minimal_spec();
        spec.kernel = Some(KernelSpec {
            kind: "S3".into(),
            host_path: None,
            s3: Some(S3Spec {
                endpoint: "http://versitygw:7070".into(),
                bucket: "sycophant-tenants".into(),
                prefix: "tenant-abc/mainframe/".into(),
                region: "us-east-1".into(),
                force_path_style: true,
                credentials: Some(SecretRef {
                    name: "tenant-s3-credentials".into(),
                    access_key_id_key: None,
                    secret_access_key_key: None,
                }),
            }),
        });
        let ws = make_workspace("demo", "abc-123", spec);
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        let volumes = value
            .pointer("/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("volumes present");
        let names: Vec<&str> = volumes
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"kernel"), "kernel volume present");
        assert!(
            names.contains(&"aws-cache"),
            "aws-cache volume present for S3"
        );

        let init = value
            .pointer("/spec/initContainers/0")
            .expect("S3 init container at index 0");
        assert_eq!(init["name"], "kernel-sync");
        assert_eq!(init["image"], S3_SYNC_IMAGE);
        let env = init["env"].as_array().expect("init container env list");
        let endpoint = env.iter().find(|e| e["name"] == "ENDPOINT").unwrap();
        assert_eq!(endpoint["value"], "http://versitygw:7070");
    }

    #[test]
    fn pod_without_kernel_omits_kernel_volume_and_mounts() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        let volumes = value
            .pointer("/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("volumes present");
        let names: Vec<&str> = volumes
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(!names.contains(&"kernel"));
        assert!(!names.contains(&"aws-cache"));
    }

    #[test]
    fn pod_has_no_conversation_log_mount() {
        // The transponder pod must not mount tightbeam's conversation
        // log via filesystem; history is read over gRPC via
        // tightbeam.GetConversationHistory. Defends against an
        // accidental re-introduction of the shared-PVC seam.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let pod = pod_for("e2e-test", &ctx(), &ws);
        let value = pod_json(&pod);
        let mounts = value
            .pointer("/spec/containers/0/volumeMounts")
            .and_then(|v| v.as_array())
            .expect("transponder volume mounts present");
        assert!(
            !mounts
                .iter()
                .any(|m| m.get("name").and_then(|n| n.as_str()) == Some("conversation-log")),
            "transponder pod must not mount the conversation-log PVC"
        );
        let volumes = value
            .pointer("/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("pod volumes present");
        assert!(
            !volumes
                .iter()
                .any(|v| v.get("name").and_then(|n| n.as_str()) == Some("conversation-log")),
            "transponder pod spec must not declare the conversation-log volume"
        );
    }

    fn pod_value(ws_name: &str, spec: WorkspaceSpec) -> Value {
        let ws = make_workspace(ws_name, "abc-123", spec);
        let pod = pod_for("e2e-test", &ctx(), &ws);
        serde_json::to_value(&pod).expect("Pod -> Value serializable")
    }

    /// Workspace spec that triggers the S3 kernel path, so the
    /// `kernel-sync` init container is materialized alongside the two
    /// regular containers. Used by the per-container sandbox-field tests.
    fn s3_kernel_spec() -> WorkspaceSpec {
        let mut spec = minimal_spec();
        spec.kernel = Some(KernelSpec {
            kind: "S3".into(),
            host_path: None,
            s3: Some(S3Spec {
                endpoint: "http://versitygw:7070".into(),
                bucket: "sycophant-tenants".into(),
                prefix: "tenant-abc/mainframe/".into(),
                region: "us-east-1".into(),
                force_path_style: true,
                credentials: Some(SecretRef {
                    name: "tenant-s3-credentials".into(),
                    access_key_id_key: None,
                    secret_access_key_key: None,
                }),
            }),
        });
        spec
    }

    /// Concatenation of `spec.containers` + `spec.initContainers` so the
    /// sandbox-field tests can iterate every actual container the
    /// workspace pod runs (matches the VAP's `allContainers` variable).
    fn all_containers(pod: &Value) -> Vec<&Value> {
        let mut out: Vec<&Value> = Vec::new();
        if let Some(cs) = pod.pointer("/spec/containers").and_then(|v| v.as_array()) {
            out.extend(cs.iter());
        }
        if let Some(ics) = pod
            .pointer("/spec/initContainers")
            .and_then(|v| v.as_array())
        {
            out.extend(ics.iter());
        }
        out
    }

    fn named_volume<'a>(pod: &'a Value, name: &str) -> &'a Value {
        pod.pointer("/spec/volumes")
            .and_then(|v| v.as_array())
            .and_then(|vs| {
                vs.iter()
                    .find(|v| v.get("name").and_then(|n| n.as_str()) == Some(name))
            })
            .unwrap_or_else(|| panic!("volume `{name}` must be present on workspace pod"))
    }

    fn container<'a>(pod: &'a Value, name: &str) -> &'a Value {
        pod.pointer("/spec/containers")
            .and_then(|v| v.as_array())
            .and_then(|cs| {
                cs.iter()
                    .find(|c| c.get("name").and_then(|n| n.as_str()) == Some(name))
            })
            .unwrap_or_else(|| panic!("container `{name}` must exist"))
    }

    #[test]
    fn pod_disables_kubelet_default_sa_token_mount() {
        let pod = pod_value("demo", minimal_spec());
        assert_eq!(
            pod.pointer("/spec/automountServiceAccountToken"),
            Some(&Value::Bool(false)),
            "automountServiceAccountToken=false suppresses the kubelet \
             kube-apiserver-audience token mount; the workspace VAP rejects \
             that audience"
        );
    }

    #[test]
    fn transponder_auth_volume_carries_mainframe_tightbeam_audience() {
        let pod = pod_value("demo", minimal_spec());
        let vol = named_volume(&pod, "transponder-auth");
        let sources = vol
            .pointer("/projected/sources")
            .and_then(|s| s.as_array())
            .expect("projected sources present");
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].pointer("/serviceAccountToken/path"),
            Some(&Value::String("token".into()))
        );
        assert_eq!(
            sources[0].pointer("/serviceAccountToken/audience"),
            Some(&Value::String(
                shared::auth::TRANSPONDER_TIGHTBEAM_AUDIENCE.into()
            )),
            "transponder-auth token must carry the transponder.tightbeam audience; \
             a stolen transponder-tightbeam token must not unlock airlock"
        );
        assert_eq!(
            sources[0].pointer("/serviceAccountToken/expirationSeconds"),
            Some(&Value::Number(3600.into()))
        );

        let transponder = container(&pod, "transponder");
        let mounts = transponder
            .get("volumeMounts")
            .and_then(|m| m.as_array())
            .expect("transponder volumeMounts present");
        let auth_mount = mounts
            .iter()
            .find(|m| m.get("name").and_then(|n| n.as_str()) == Some("transponder-auth"))
            .expect("transponder must mount transponder-auth");
        assert_eq!(
            auth_mount.get("mountPath").and_then(|p| p.as_str()),
            Some("/var/run/secrets/transponder/tightbeam"),
            "must mount where SaTokenInterceptor's TRANSPONDER_TIGHTBEAM_TOKEN_PATH parent is"
        );
        assert_eq!(
            auth_mount.get("readOnly").and_then(|r| r.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn transponder_airlock_auth_volume_carries_transponder_airlock_audience() {
        let pod = pod_value("demo", minimal_spec());
        let vol = named_volume(&pod, "transponder-airlock-auth");
        let sources = vol
            .pointer("/projected/sources")
            .and_then(|s| s.as_array())
            .expect("projected sources present");
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].pointer("/serviceAccountToken/audience"),
            Some(&Value::String(
                shared::auth::TRANSPONDER_AIRLOCK_AUDIENCE.into()
            )),
            "transponder-airlock-auth token must carry the transponder.airlock audience"
        );

        let transponder = container(&pod, "transponder");
        let mounts = transponder
            .get("volumeMounts")
            .and_then(|m| m.as_array())
            .expect("transponder volumeMounts present");
        let mount = mounts
            .iter()
            .find(|m| m.get("name").and_then(|n| n.as_str()) == Some("transponder-airlock-auth"))
            .expect("transponder must mount transponder-airlock-auth");
        assert_eq!(
            mount.get("mountPath").and_then(|p| p.as_str()),
            Some("/var/run/secrets/transponder/airlock")
        );
        assert_eq!(mount.get("readOnly").and_then(|r| r.as_bool()), Some(true));
    }

    #[test]
    fn transponder_auth_volumes_have_distinct_names_and_audiences() {
        let pod = pod_value("demo", minimal_spec());
        let tb = named_volume(&pod, "transponder-auth");
        let al = named_volume(&pod, "transponder-airlock-auth");
        let tb_aud = tb
            .pointer("/projected/sources/0/serviceAccountToken/audience")
            .and_then(|s| s.as_str())
            .expect("tightbeam audience present");
        let al_aud = al
            .pointer("/projected/sources/0/serviceAccountToken/audience")
            .and_then(|s| s.as_str())
            .expect("airlock audience present");
        assert_ne!(
            tb_aud, al_aud,
            "transponder-auth and transponder-airlock-auth must carry distinct audiences \
             so leaking one does not unlock the other verifier"
        );
    }

    #[test]
    fn pod_auth_mount_works_without_kernel() {
        // Catches a regression where the auth mount gets gated on has_kernel.
        let spec = WorkspaceSpec {
            kernel: None,
            ..minimal_spec()
        };
        let pod = pod_value("no-mf", spec);
        let _ = named_volume(&pod, "transponder-auth");
        let _ = named_volume(&pod, "transponder-airlock-auth");
        let transponder = container(&pod, "transponder");
        let mounts = transponder
            .get("volumeMounts")
            .and_then(|m| m.as_array())
            .expect("transponder volumeMounts present");
        let mount_names: Vec<&str> = mounts
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(
            mount_names.contains(&"transponder-auth"),
            "tightbeam-audience auth mount is unconditional, must be present without kernel"
        );
        assert!(
            mount_names.contains(&"transponder-airlock-auth"),
            "airlock-audience auth mount is unconditional, must be present without kernel"
        );
    }

    #[test]
    fn pod_with_hostpath_kernel_keeps_both_mounts() {
        let spec = WorkspaceSpec {
            kernel: Some(crate::crd::KernelSpec {
                kind: "HostPath".into(),
                host_path: Some(HostPathSpec {
                    path: "/etc/kernel".into(),
                }),
                s3: None,
            }),
            ..minimal_spec()
        };
        let pod = pod_value("hp", spec);
        let transponder = container(&pod, "transponder");
        let mounts = transponder
            .get("volumeMounts")
            .and_then(|m| m.as_array())
            .expect("transponder volumeMounts present");
        let names: Vec<&str> = mounts
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(
            names.contains(&"transponder-auth"),
            "tightbeam-audience auth mount must remain when mainframe is present"
        );
        assert!(
            names.contains(&"transponder-airlock-auth"),
            "airlock-audience auth mount must remain when mainframe is present"
        );
        assert!(
            names.contains(&"kernel"),
            "kernel mount must remain when mainframe is declared"
        );
    }
}
