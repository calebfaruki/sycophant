//! Shared PodSpec fragments. Centralizes the security-load-bearing pieces
//! every sycophant worker pod needs so the controllers that spawn them
//! can't drift.

use k8s_openapi::api::core::v1::{
    ProjectedVolumeSource, ServiceAccountTokenProjection, Volume, VolumeMount, VolumeProjection,
};

/// Build the projected single-audience SA-token `Volume` + read-only
/// `VolumeMount` pair a worker pod mounts at the kubelet default path. The
/// token carries exactly `audience`; paired with
/// `automountServiceAccountToken=false` on the PodSpec (kept at the call
/// site) it replaces the kubelet default token. The path, expiration, and
/// read-only mount are fixed here so the spawners can't diverge on the
/// trust boundary.
pub fn sa_token_volume(volume_name: &str, audience: &str) -> (Volume, VolumeMount) {
    let volume = Volume {
        name: volume_name.to_string(),
        projected: Some(ProjectedVolumeSource {
            default_mode: None,
            sources: Some(vec![VolumeProjection {
                service_account_token: Some(ServiceAccountTokenProjection {
                    path: "token".to_string(),
                    audience: Some(audience.to_string()),
                    expiration_seconds: Some(3600),
                }),
                ..Default::default()
            }]),
        }),
        ..Default::default()
    };
    let mount = VolumeMount {
        name: volume_name.to_string(),
        mount_path: "/var/run/secrets/kubernetes.io/serviceaccount".to_string(),
        read_only: Some(true),
        ..Default::default()
    };
    (volume, mount)
}
