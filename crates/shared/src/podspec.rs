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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull the single projected SA-token projection out of the Volume so
    /// the audience/expiration assertions read the value the pod actually
    /// mounts, not a fixture echo.
    fn token_projection(volume: &Volume) -> &ServiceAccountTokenProjection {
        volume
            .projected
            .as_ref()
            .expect("projected source")
            .sources
            .as_ref()
            .expect("sources")
            .first()
            .expect("one projection")
            .service_account_token
            .as_ref()
            .expect("sa-token projection")
    }

    /// Kills podspec.rs:18 (volume.name field). Deleting the field defaults
    /// it to the empty string, so the Volume name no longer matches the
    /// VolumeMount name and the pod would fail to bind the mount.
    #[test]
    fn volume_name_matches_requested_name() {
        let (volume, _mount) = sa_token_volume("relay-sa-token", "relay.internal");
        assert_eq!(volume.name, "relay-sa-token");
    }

    /// Kills podspec.rs:33 (mount.name field). A defaulted empty mount name
    /// references no Volume, so the projected token never lands at the
    /// mount path.
    #[test]
    fn mount_name_matches_volume_name() {
        let (volume, mount) = sa_token_volume("relay-sa-token", "relay.internal");
        assert_eq!(mount.name, "relay-sa-token");
        assert_eq!(mount.name, volume.name);
    }

    /// Kills podspec.rs:34 (mount.mount_path field). A defaulted empty path
    /// would mount the token somewhere other than the kubelet default
    /// location clients read from.
    #[test]
    fn mount_path_is_the_kubelet_default() {
        let (_volume, mount) = sa_token_volume("relay-sa-token", "relay.internal");
        assert_eq!(
            mount.mount_path,
            "/var/run/secrets/kubernetes.io/serviceaccount"
        );
    }

    /// Kills podspec.rs:35 (mount.read_only field). Defaulting it to None
    /// drops the read-only guarantee on the token mount.
    #[test]
    fn mount_is_read_only() {
        let (_volume, mount) = sa_token_volume("relay-sa-token", "relay.internal");
        assert_eq!(mount.read_only, Some(true));
    }

    /// Strengthens the fixture: the projected token must carry exactly the
    /// requested single audience and the fixed 3600s expiration, so a drift
    /// on either value in the projection is caught.
    #[test]
    fn projected_token_carries_audience_and_fixed_expiration() {
        let (volume, _mount) = sa_token_volume("relay-sa-token", "relay.internal");
        let projection = token_projection(&volume);
        assert_eq!(projection.audience.as_deref(), Some("relay.internal"));
        assert_eq!(projection.expiration_seconds, Some(3600));
    }
}
