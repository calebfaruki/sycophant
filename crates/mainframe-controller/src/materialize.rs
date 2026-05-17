//! Materialize a Workspace CR into its children: a Sandbox CR
//! (agents.x-k8s.io/v1alpha1), a ServiceAccount, a
//! PersistentVolumeClaim, and a NetworkPolicy. Each child carries an
//! ownerRef back to the Workspace so cascade delete and the finalizer
//! cleanup work naturally.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{PersistentVolumeClaim, ServiceAccount};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, Patch, PatchParams, TypeMeta};
use kube::Client;
use serde_json::{json, Value};

use crate::workspace_crd::{Workspace, WorkspaceMainframe};

/// Field-manager string used for server-side apply on every child the
/// controller materializes. Other writers (helm, kubectl) own different
/// fields — SSA's conflict resolution lets us coexist if necessary.
pub const FIELD_MANAGER: &str = "mainframe-controller";

pub const SANDBOX_GROUP: &str = "agents.x-k8s.io";
pub const SANDBOX_VERSION: &str = "v1alpha1";
pub const SANDBOX_KIND: &str = "Sandbox";

pub const WORKSPACE_GROUP: &str = "sycophant.md";
pub const WORKSPACE_VERSION: &str = "v1";
pub const WORKSPACE_KIND: &str = "Workspace";

/// CPU/memory limits applied to the transponder sidecar regardless of
/// the workspace's own resource budget.
pub const TRANSPONDER_CPU_LIMIT: &str = "500m";
pub const TRANSPONDER_MEMORY_LIMIT: &str = "256Mi";

/// CPU/memory requests+limits for the optional S3-sync init container.
pub const S3_SYNC_CPU_REQUEST: &str = "100m";
pub const S3_SYNC_MEMORY_REQUEST: &str = "64Mi";
pub const S3_SYNC_CPU_LIMIT: &str = "500m";
pub const S3_SYNC_MEMORY_LIMIT: &str = "256Mi";

pub const S3_SYNC_IMAGE: &str =
    "amazon/aws-cli@sha256:db8d39443ef512d4724becfac59ec9b7c4f8621e7b3c6200be56cd4fc2dc9570";

/// Materialization-time data the controller learns from its own pod
/// environment (chart-injected) rather than from each Workspace CR.
/// Carried into the spec builders so unit tests can supply explicit
/// values instead of mutating process env.
#[derive(Clone, Debug)]
pub struct MaterializationContext {
    /// Helm release name. Embedded in the Sandbox pod-affinity selector
    /// so the workspace pod schedules on the same node as the per-tenant
    /// tightbeam-ctrl. Matches the `app.kubernetes.io/instance` label
    /// helm applies to the tightbeam-ctrl Pod.
    pub release_name: String,
    /// Transponder sidecar image. Chart-installed (one transponder
    /// image per tenant); the legacy chart template read this from
    /// `.Values.transponder.image`.
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
/// finalizer confirms the Sandbox child is gone, per Q14).
pub fn workspace_owner_ref(workspace: &Workspace) -> OwnerReference {
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
/// templates.
fn workspace_labels(name: &str, release: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/managed-by".into(), "Helm".into());
    labels.insert("app.kubernetes.io/instance".into(), release.to_string());
    labels.insert("app.kubernetes.io/part-of".into(), "sycophant".into());
    labels.insert("app.kubernetes.io/component".into(), "workspace".into());
    labels.insert("app.kubernetes.io/name".into(), name.to_string());
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

/// Per-workspace ServiceAccount. Pod identity for the workspace pod;
/// kubelet auto-mounts the token at the canonical SA path so the
/// transponder + mainframe-runtime containers can authenticate to
/// tightbeam-ctrl / airlock-ctrl over gRPC.
pub fn service_account_for(
    namespace: &str,
    workspace: &Workspace,
    release: &str,
) -> ServiceAccount {
    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    let mut meta = child_metadata(format!("sa-{ws_name}"), namespace, workspace, release);
    meta.labels
        .get_or_insert_with(BTreeMap::new)
        .insert("sycophant.io/type".into(), "workspace-sa".into());
    ServiceAccount {
        metadata: meta,
        ..Default::default()
    }
}

/// Per-workspace PVC at `/workspace` inside the workspace pod
/// (writable scratch). Fixed 1Gi; `Workspace.spec.storage` is accepted
/// in the schema but not yet consumed here.
pub fn pvc_for(namespace: &str, workspace: &Workspace, release: &str) -> PersistentVolumeClaim {
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

/// Per-workspace NetworkPolicy. Default-deny egress except DNS,
/// tightbeam-ctrl:9090, and airlock-ctrl:9090. Faithful reproduction of
/// the legacy template; selector targets the workspace pod by its
/// `app.kubernetes.io/name` label.
pub fn network_policy_for(namespace: &str, workspace: &Workspace, release: &str) -> NetworkPolicy {
    use k8s_openapi::api::networking::v1::{
        NetworkPolicyEgressRule, NetworkPolicyPeer, NetworkPolicyPort, NetworkPolicySpec,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
    use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    let meta = child_metadata(
        format!("{ws_name}-workspace"),
        namespace,
        workspace,
        release,
    );

    let mut pod_match = BTreeMap::new();
    pod_match.insert("app.kubernetes.io/name".into(), ws_name.to_string());
    pod_match.insert("app.kubernetes.io/part-of".into(), "sycophant".into());

    let mut dns_ns_match = BTreeMap::new();
    dns_ns_match.insert("kubernetes.io/metadata.name".into(), "kube-system".into());
    let mut dns_pod_match = BTreeMap::new();
    dns_pod_match.insert("k8s-app".into(), "kube-dns".into());

    let mut tightbeam_match = BTreeMap::new();
    tightbeam_match.insert("app.kubernetes.io/name".into(), "tightbeam-ctrl".into());
    tightbeam_match.insert("app.kubernetes.io/part-of".into(), "sycophant".into());

    let mut airlock_match = BTreeMap::new();
    airlock_match.insert("app.kubernetes.io/name".into(), "airlock-ctrl".into());
    airlock_match.insert("app.kubernetes.io/part-of".into(), "sycophant".into());

    let dns_ports = vec![
        NetworkPolicyPort {
            port: Some(IntOrString::Int(53)),
            protocol: Some("UDP".into()),
            end_port: None,
        },
        NetworkPolicyPort {
            port: Some(IntOrString::Int(53)),
            protocol: Some("TCP".into()),
            end_port: None,
        },
    ];
    let grpc_port = vec![NetworkPolicyPort {
        port: Some(IntOrString::Int(9090)),
        protocol: Some("TCP".into()),
        end_port: None,
    }];

    NetworkPolicy {
        metadata: meta,
        spec: Some(NetworkPolicySpec {
            pod_selector: Some(LabelSelector {
                match_labels: Some(pod_match),
                ..Default::default()
            }),
            policy_types: Some(vec!["Egress".into()]),
            egress: Some(vec![
                NetworkPolicyEgressRule {
                    to: Some(vec![NetworkPolicyPeer {
                        namespace_selector: Some(LabelSelector {
                            match_labels: Some(dns_ns_match),
                            ..Default::default()
                        }),
                        pod_selector: Some(LabelSelector {
                            match_labels: Some(dns_pod_match),
                            ..Default::default()
                        }),
                        ip_block: None,
                    }]),
                    ports: Some(dns_ports),
                },
                NetworkPolicyEgressRule {
                    to: Some(vec![NetworkPolicyPeer {
                        pod_selector: Some(LabelSelector {
                            match_labels: Some(tightbeam_match),
                            ..Default::default()
                        }),
                        namespace_selector: None,
                        ip_block: None,
                    }]),
                    ports: Some(grpc_port.clone()),
                },
                NetworkPolicyEgressRule {
                    to: Some(vec![NetworkPolicyPeer {
                        pod_selector: Some(LabelSelector {
                            match_labels: Some(airlock_match),
                            ..Default::default()
                        }),
                        namespace_selector: None,
                        ip_block: None,
                    }]),
                    ports: Some(grpc_port),
                },
            ]),
            ingress: None,
        }),
        ..Default::default()
    }
}

/// Sandbox CR (agents.x-k8s.io/v1alpha1). The agent-sandbox controller
/// (third-party, in `agent-sandbox-system`) materializes the actual
/// pod from this spec. Returned as a `DynamicObject` so we don't drag
/// the agent-sandbox type definitions into this crate.
pub fn sandbox_for(
    namespace: &str,
    ctx: &MaterializationContext,
    workspace: &Workspace,
) -> DynamicObject {
    let ws_name = workspace.metadata.name.as_deref().unwrap_or_default();
    let labels = workspace_labels(ws_name, &ctx.release_name);

    let mut volumes = vec![
        json!({ "name": "tmp", "emptyDir": {} }),
        json!({ "name": "agent-home", "emptyDir": {} }),
        json!({
            "name": "workspace-data",
            "persistentVolumeClaim": { "claimName": format!("{ws_name}-workspace-data") }
        }),
    ];

    // Optional mainframe volume + init container for S3-kind.
    let mut init_containers: Vec<Value> = Vec::new();
    let has_mainframe = workspace.spec.mainframe.is_some();
    if let Some(mainframe) = workspace.spec.mainframe.as_ref() {
        match mainframe.kind.as_str() {
            "HostPath" => {
                let path = mainframe
                    .host_path
                    .as_ref()
                    .map(|hp| hp.path.clone())
                    .unwrap_or_default();
                volumes.push(json!({
                    "name": "mainframe",
                    "hostPath": { "path": path, "type": "Directory" }
                }));
            }
            "S3" => {
                volumes.push(json!({ "name": "mainframe", "emptyDir": {} }));
                volumes.push(json!({ "name": "aws-cache", "emptyDir": {} }));
                init_containers.push(s3_sync_init_container(mainframe));
            }
            _ => {
                // Unknown kind: emit no volume; transponder mount stays
                // gated by `has_mainframe` so the pod still starts.
            }
        }
    }

    // Containers: transponder + mainframe-runtime.
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
            { "name": "TIGHTBEAM_CONTROLLER_ADDR", "value": "http://tightbeam-ctrl:9090" },
            { "name": "AIRLOCK_CONTROLLER_ADDR",   "value": "http://airlock-ctrl:9090" }
        ],
    });
    if has_mainframe {
        transponder["volumeMounts"] = json!([
            { "name": "mainframe", "mountPath": "/etc/mainframe", "readOnly": true }
        ]);
    }

    let cpu = workspace.spec.cpu.clone().unwrap_or_default();
    let memory = workspace.spec.memory.clone().unwrap_or_default();
    let image = format!("{}:{}", workspace.spec.image, workspace.spec.tag);
    let pull_policy = workspace
        .spec
        .pull_policy
        .clone()
        .unwrap_or_else(|| "IfNotPresent".into());

    let mut runtime_mounts = vec![
        json!({ "name": "workspace-data", "mountPath": "/workspace" }),
        json!({ "name": "tmp", "mountPath": "/tmp" }),
        json!({ "name": "agent-home", "mountPath": "/home/agent" }),
    ];
    if has_mainframe {
        runtime_mounts.push(json!({
            "name": "mainframe", "mountPath": "/etc/mainframe", "readOnly": true
        }));
    }

    let mut resources = json!({});
    if !cpu.is_empty() || !memory.is_empty() {
        let mut limits = serde_json::Map::new();
        let mut requests = serde_json::Map::new();
        if !cpu.is_empty() {
            limits.insert("cpu".into(), Value::String(cpu.clone()));
            requests.insert("cpu".into(), Value::String(cpu));
        }
        if !memory.is_empty() {
            limits.insert("memory".into(), Value::String(memory.clone()));
            requests.insert("memory".into(), Value::String(memory));
        }
        resources = json!({ "limits": limits, "requests": requests });
    }

    let mainframe_runtime = json!({
        "name": "mainframe-runtime",
        "image": image,
        "imagePullPolicy": pull_policy,
        "securityContext": {
            "readOnlyRootFilesystem": true,
            "allowPrivilegeEscalation": false,
            "capabilities": { "drop": ["ALL"] }
        },
        "resources": resources,
        "env": [
            { "name": "HOME", "value": "/home/agent" }
        ],
        "readinessProbe": {
            "exec": { "command": ["nc", "-z", "127.0.0.1", "50051"] },
            "initialDelaySeconds": 2,
            "periodSeconds": 5
        },
        "livenessProbe": {
            "exec": { "command": ["nc", "-z", "127.0.0.1", "50051"] },
            "initialDelaySeconds": 10,
            "periodSeconds": 10
        },
        "volumeMounts": runtime_mounts,
        "workingDir": "/workspace"
    });

    let spec = json!({
        "replicas": 1,
        "podTemplate": {
            "metadata": { "labels": labels.clone() },
            "spec": {
                "runtimeClassName": "gvisor",
                "serviceAccountName": format!("sa-{ws_name}"),
                "tolerations": [
                    {
                        "key": "sycophant.io/workload",
                        "operator": "Equal",
                        "value": "workspace",
                        "effect": "NoSchedule"
                    }
                ],
                "affinity": {
                    "podAffinity": {
                        "requiredDuringSchedulingIgnoredDuringExecution": [
                            {
                                "labelSelector": {
                                    "matchLabels": {
                                        "app.kubernetes.io/name": "tightbeam-ctrl",
                                        "app.kubernetes.io/instance": ctx.release_name
                                    }
                                },
                                "topologyKey": "kubernetes.io/hostname"
                            }
                        ]
                    }
                },
                "securityContext": {
                    "runAsNonRoot": true,
                    "runAsUser": 1000,
                    "runAsGroup": 1000,
                    "fsGroup": 1000
                },
                "initContainers": init_containers,
                "containers": [transponder, mainframe_runtime],
                "volumes": volumes
            }
        }
    });

    let gvk = GroupVersionKind {
        group: SANDBOX_GROUP.to_string(),
        version: SANDBOX_VERSION.to_string(),
        kind: SANDBOX_KIND.to_string(),
    };
    let api_resource = ApiResource::from_gvk(&gvk);
    let mut obj = DynamicObject::new(ws_name, &api_resource);
    obj.types = Some(TypeMeta {
        api_version: format!("{}/{}", SANDBOX_GROUP, SANDBOX_VERSION),
        kind: SANDBOX_KIND.to_string(),
    });
    obj.metadata.namespace = Some(namespace.to_string());
    obj.metadata.labels = Some(labels);
    obj.metadata.owner_references = Some(vec![workspace_owner_ref(workspace)]);
    obj.data = serde_json::json!({ "spec": spec });
    obj
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
    let netpol = network_policy_for(namespace, workspace, &ctx.release_name);
    let sandbox = sandbox_for(namespace, ctx, workspace);

    let pp = PatchParams::apply(FIELD_MANAGER).force();

    let sa_name = sa.metadata.name.clone().unwrap_or_default();
    let sa_api: Api<ServiceAccount> = Api::namespaced(client.clone(), namespace);
    sa_api.patch(&sa_name, &pp, &Patch::Apply(&sa)).await?;

    let pvc_name = pvc.metadata.name.clone().unwrap_or_default();
    let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), namespace);
    pvc_api.patch(&pvc_name, &pp, &Patch::Apply(&pvc)).await?;

    let netpol_name = netpol.metadata.name.clone().unwrap_or_default();
    let netpol_api: Api<NetworkPolicy> = Api::namespaced(client.clone(), namespace);
    netpol_api
        .patch(&netpol_name, &pp, &Patch::Apply(&netpol))
        .await?;

    let sandbox_name = sandbox.metadata.name.clone().unwrap_or_default();
    let sandbox_api: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &ApiResource::from_gvk(&GroupVersionKind {
            group: SANDBOX_GROUP.to_string(),
            version: SANDBOX_VERSION.to_string(),
            kind: SANDBOX_KIND.to_string(),
        }),
    );
    sandbox_api
        .patch(&sandbox_name, &pp, &Patch::Apply(&sandbox))
        .await?;

    Ok(())
}

/// True when the Sandbox child for `workspace_name` still exists in the
/// namespace. The finalizer (see `finalizer.rs`) uses this as the gate:
/// the Workspace's deletion isn't reported complete until the Sandbox
/// (and the agent-sandbox-controller-managed pod behind it) is gone.
pub async fn sandbox_child_exists(
    client: &Client,
    namespace: &str,
    workspace_name: &str,
) -> anyhow::Result<bool> {
    let api: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &ApiResource::from_gvk(&GroupVersionKind {
            group: SANDBOX_GROUP.to_string(),
            version: SANDBOX_VERSION.to_string(),
            kind: SANDBOX_KIND.to_string(),
        }),
    );
    match api.get_opt(workspace_name).await? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

fn s3_sync_init_container(mainframe: &WorkspaceMainframe) -> Value {
    let s3 = mainframe.s3.as_ref();
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
        "name": "mainframe-sync",
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
            "aws --endpoint-url \"$ENDPOINT\" s3 sync \"s3://${BUCKET}/${PREFIX}\" /etc/mainframe"
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
            { "name": "mainframe", "mountPath": "/etc/mainframe" },
            { "name": "aws-cache", "mountPath": "/tmp" }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_crd::WorkspaceSpec;
    use shared::storage::{HostPathSpec, S3Spec, SecretRef};

    fn make_workspace(name: &str, uid: &str, spec: WorkspaceSpec) -> Workspace {
        let mut w = Workspace::new(name, spec);
        w.metadata.uid = Some(uid.to_string());
        w
    }

    fn minimal_spec() -> WorkspaceSpec {
        WorkspaceSpec {
            image: "ghcr.io/calebfaruki/transponder".into(),
            tag: "v0.1".into(),
            pull_policy: None,
            cpu: Some("0.5".into()),
            memory: Some("1Gi".into()),
            storage: None,
            mainframe: None,
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
    fn sandbox_carries_workspace_owner_ref() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        assert_owner_ref(
            sandbox.metadata.owner_references.as_ref(),
            "demo",
            "abc-123",
        );
    }

    #[test]
    fn sa_pvc_netpol_all_carry_owner_ref() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sa = service_account_for("e2e-test", &ws, "test");
        let pvc = pvc_for("e2e-test", &ws, "test");
        let netpol = network_policy_for("e2e-test", &ws, "test");
        assert_owner_ref(sa.metadata.owner_references.as_ref(), "demo", "abc-123");
        assert_owner_ref(pvc.metadata.owner_references.as_ref(), "demo", "abc-123");
        assert_owner_ref(netpol.metadata.owner_references.as_ref(), "demo", "abc-123");
    }

    #[test]
    fn sa_name_uses_sa_prefix_per_chart_convention() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sa = service_account_for("e2e-test", &ws, "test");
        assert_eq!(sa.metadata.name.as_deref(), Some("sa-demo"));
        assert_eq!(sa.metadata.namespace.as_deref(), Some("e2e-test"));
        let labels = sa.metadata.labels.as_ref().expect("labels present");
        assert_eq!(
            labels.get("sycophant.io/type").map(String::as_str),
            Some("workspace-sa")
        );
        assert_eq!(
            labels.get("app.kubernetes.io/name").map(String::as_str),
            Some("demo")
        );
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

    #[test]
    fn netpol_name_and_pod_selector_per_chart_convention() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let netpol = network_policy_for("e2e-test", &ws, "test");
        assert_eq!(netpol.metadata.name.as_deref(), Some("demo-workspace"));
        let pod_match = netpol
            .spec
            .as_ref()
            .and_then(|s| s.pod_selector.as_ref())
            .and_then(|ls| ls.match_labels.as_ref())
            .expect("podSelector matchLabels present");
        assert_eq!(
            pod_match.get("app.kubernetes.io/name").map(String::as_str),
            Some("demo")
        );
        assert_eq!(
            pod_match
                .get("app.kubernetes.io/part-of")
                .map(String::as_str),
            Some("sycophant")
        );
    }

    #[test]
    fn netpol_egress_includes_dns_tightbeam_airlock() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let netpol = network_policy_for("e2e-test", &ws, "test");
        let egress = netpol
            .spec
            .as_ref()
            .and_then(|s| s.egress.as_ref())
            .expect("egress rules present");
        assert_eq!(
            egress.len(),
            3,
            "exactly three egress rules: DNS + tightbeam + airlock"
        );
    }

    #[test]
    fn sandbox_pod_template_carries_workspace_cpu_memory_in_runtime_container() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let runtime = sandbox
            .data
            .pointer("/spec/podTemplate/spec/containers/1")
            .expect("mainframe-runtime is container index 1");
        assert_eq!(runtime["name"], "mainframe-runtime");
        let requests = &runtime["resources"]["requests"];
        assert_eq!(requests["cpu"], "0.5");
        assert_eq!(requests["memory"], "1Gi");
        let limits = &runtime["resources"]["limits"];
        assert_eq!(limits["cpu"], "0.5");
        assert_eq!(limits["memory"], "1Gi");
    }

    #[test]
    fn sandbox_uses_release_name_in_pod_affinity_for_tightbeam_colocation() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let affinity = sandbox
            .data
            .pointer(
                "/spec/podTemplate/spec/affinity/podAffinity/requiredDuringSchedulingIgnoredDuringExecution/0/labelSelector/matchLabels",
            )
            .expect("pod affinity matchLabels present");
        assert_eq!(affinity["app.kubernetes.io/name"], "tightbeam-ctrl");
        assert_eq!(affinity["app.kubernetes.io/instance"], "test");
    }

    #[test]
    fn sandbox_runtime_container_uses_workspace_image_tag() {
        let mut spec = minimal_spec();
        spec.image = "ghcr.io/me/runtime".into();
        spec.tag = "v9".into();
        let ws = make_workspace("demo", "abc-123", spec);
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let runtime_image = sandbox
            .data
            .pointer("/spec/podTemplate/spec/containers/1/image")
            .expect("runtime container image present");
        assert_eq!(runtime_image, "ghcr.io/me/runtime:v9");
    }

    #[test]
    fn sandbox_transponder_container_uses_ctx_image_tag() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let transponder_image = sandbox
            .data
            .pointer("/spec/podTemplate/spec/containers/0/image")
            .expect("transponder container image present");
        assert_eq!(transponder_image, "ghcr.io/sycophant/transponder:v0.1");
    }

    #[test]
    fn sandbox_with_hostpath_mainframe_emits_volume_and_mounts() {
        let mut spec = minimal_spec();
        spec.mainframe = Some(WorkspaceMainframe {
            kind: "HostPath".into(),
            host_path: Some(HostPathSpec {
                path: "/host/sycophant/demo".into(),
            }),
            s3: None,
        });
        let ws = make_workspace("demo", "abc-123", spec);
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let volumes = sandbox
            .data
            .pointer("/spec/podTemplate/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("volumes present");
        let mainframe_vol = volumes
            .iter()
            .find(|v| v.get("name").and_then(|n| n.as_str()) == Some("mainframe"))
            .expect("mainframe volume present");
        assert_eq!(mainframe_vol["hostPath"]["path"], "/host/sycophant/demo");
        assert_eq!(mainframe_vol["hostPath"]["type"], "Directory");
        // No S3 sync init container for HostPath.
        let init = sandbox
            .data
            .pointer("/spec/podTemplate/spec/initContainers")
            .and_then(|v| v.as_array())
            .expect("initContainers list present (possibly empty)");
        assert!(
            init.is_empty(),
            "HostPath mainframe should not emit an init container"
        );
    }

    #[test]
    fn sandbox_with_s3_mainframe_emits_init_container_and_aws_cache_volume() {
        let mut spec = minimal_spec();
        spec.mainframe = Some(WorkspaceMainframe {
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
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let volumes = sandbox
            .data
            .pointer("/spec/podTemplate/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("volumes present");
        let names: Vec<&str> = volumes
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"mainframe"), "mainframe volume present");
        assert!(
            names.contains(&"aws-cache"),
            "aws-cache volume present for S3"
        );

        let init = sandbox
            .data
            .pointer("/spec/podTemplate/spec/initContainers/0")
            .expect("S3 init container at index 0");
        assert_eq!(init["name"], "mainframe-sync");
        assert_eq!(init["image"], S3_SYNC_IMAGE);
        let env = init["env"].as_array().expect("init container env list");
        let endpoint = env.iter().find(|e| e["name"] == "ENDPOINT").unwrap();
        assert_eq!(endpoint["value"], "http://versitygw:7070");
    }

    #[test]
    fn sandbox_without_mainframe_omits_mainframe_volume_and_mounts() {
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let volumes = sandbox
            .data
            .pointer("/spec/podTemplate/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("volumes present");
        let names: Vec<&str> = volumes
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(!names.contains(&"mainframe"));
        assert!(!names.contains(&"aws-cache"));
    }

    #[test]
    fn sandbox_runtime_container_has_no_conversation_log_mount() {
        // Bug 3 reframe: the workspace pod no longer reads tightbeam's
        // conversation log via filesystem. Replaced by the transponder's
        // `recent_turns` built-in tool that calls
        // tightbeam.GetConversationHistory over the existing gRPC
        // channel. Defends against an accidental re-introduction of
        // the shared-PVC seam.
        let ws = make_workspace("demo", "abc-123", minimal_spec());
        let sandbox = sandbox_for("e2e-test", &ctx(), &ws);
        let mounts = sandbox
            .data
            .pointer("/spec/podTemplate/spec/containers/1/volumeMounts")
            .and_then(|v| v.as_array())
            .expect("runtime volume mounts present");
        assert!(
            !mounts
                .iter()
                .any(|m| m.get("name").and_then(|n| n.as_str()) == Some("conversation-log")),
            "workspace pod must not mount the conversation-log PVC"
        );
        let volumes = sandbox
            .data
            .pointer("/spec/podTemplate/spec/volumes")
            .and_then(|v| v.as_array())
            .expect("pod volumes present");
        assert!(
            !volumes
                .iter()
                .any(|v| v.get("name").and_then(|n| n.as_str()) == Some("conversation-log")),
            "workspace pod spec must not declare the conversation-log volume"
        );
    }
}
