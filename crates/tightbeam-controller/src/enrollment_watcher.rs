//! Enrollment CR watcher: mints one-time enrollment codes for fresh
//! Enrollments, installs registered public keys into the
//! `ClientSignatureVerifier` cache, and removes them on delete.
//!
//! Two responsibilities:
//!
//! 1. **Mint codes.** When an Enrollment appears with no
//!    `status.publicKey` and no in-flight `status.enrollmentCode`, sign a
//!    one-time code with the controller's signing key and SSA-patch it
//!    onto status. Operators see it via `kubectl get enr -o yaml` and
//!    deliver it out-of-band to the device.
//!
//! 2. **Install public keys.** When an Enrollment carries a registered
//!    `status.publicKey`, decode SEC1-base64 → P-256 VerifyingKey and
//!    insert into the shared registration cache the external listener
//!    consults on every signed request. On delete, remove the entry.
//!
//! `redeem_enrollment` (in `gateway.rs`) writes `publicKey` + `enrolledAt`,
//! clears `enrollmentCode`, and installs the key into the cache
//! synchronously so the device's immediate signed follow-up verifies.
//! This watcher is the durable backstop: on restart or watch relist it
//! re-installs every registered key from the persisted CR status.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use ed25519_dalek::SigningKey;
use futures::{StreamExt, TryStreamExt};
use kube::api::{Patch, PatchParams};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client as KubeClient};
use p256::ecdsa::VerifyingKey;
use serde_json::json;
use shared::client_signature::{ClientRegistration, ClientSignatureVerifier};

use crate::crd::{Enrollment, EnrollmentSpec, EnrollmentStatus};

/// SSA field manager used by every status write the controller performs
/// on Enrollment CRs. Both this watcher (minting codes) and the
/// `redeem_enrollment` handler (writing publicKey + clearing the code)
/// share one manager — they touch disjoint fields and clears in normal
/// operation, so an SSA conflict would be a programming error worth
/// surfacing rather than a normal concurrent-writer case.
pub const FIELD_MANAGER: &str = "tightbeam-controller";

/// Default lifetime for a freshly-minted enrollment code (1 hour).
pub const DEFAULT_ENROLLMENT_TTL_SECS: i64 = 3600;

#[derive(Debug, PartialEq, Eq)]
pub enum EnrollmentAction {
    /// Status has a registered publicKey — install (or refresh) it in
    /// the verifier cache.
    InstallKey,
    /// No publicKey and no in-flight code — mint a fresh code.
    MintCode,
    /// In-flight code present, no publicKey — wait for the device to
    /// redeem.
    NoOp,
}

/// Pure decision: given an Enrollment's current status, what should the
/// watcher do this reconcile? `publicKey` wins over `enrollmentCode`
/// (covers the transient state right after redeem but before any
/// follow-up patch clears the code).
pub fn decide_action(status: Option<&EnrollmentStatus>) -> EnrollmentAction {
    let Some(status) = status else {
        return EnrollmentAction::MintCode;
    };
    if status.public_key.is_some() {
        return EnrollmentAction::InstallKey;
    }
    if status.enrollment_code.is_some() {
        return EnrollmentAction::NoOp;
    }
    EnrollmentAction::MintCode
}

/// Decode a base64-SEC1 public key into a P-256 `VerifyingKey`. Returns
/// `None` on any decode/parse failure — callers treat a malformed key
/// as "skip this Enrollment" rather than crash the watcher.
pub fn parse_public_key_b64(b64: &str) -> Option<VerifyingKey> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    VerifyingKey::from_sec1_bytes(&bytes).ok()
}

/// Build the SSA patch body for minting a fresh enrollment code onto
/// the Enrollment's status subresource. Returns a complete-enough
/// document for `Patch::Apply` against `patch_status`.
pub fn build_mint_patch(name: &str, code: &str, expires_at: i64) -> serde_json::Value {
    json!({
        "apiVersion": "sycophant.md/v1",
        "kind": "Enrollment",
        "metadata": { "name": name },
        "status": {
            "enrollmentCode": code,
            "enrollmentCodeExpiresAt": expires_at,
        }
    })
}

/// Build the SSA patch body for recording a successful enrollment. Sets
/// `publicKey` + `enrolledAt`; deliberately omits `enrollmentCode` and
/// `enrollmentCodeExpiresAt` so SSA (with the shared `FIELD_MANAGER`)
/// removes them — naturally enforcing the one-time-use property since
/// the same writer can't claim the cleared fields on a subsequent
/// redeem attempt.
pub fn build_redeem_patch(
    name: &str,
    public_key_b64: &str,
    enrolled_at_rfc3339: &str,
) -> serde_json::Value {
    json!({
        "apiVersion": "sycophant.md/v1",
        "kind": "Enrollment",
        "metadata": { "name": name },
        "status": {
            "publicKey": public_key_b64,
            "enrolledAt": enrolled_at_rfc3339,
        }
    })
}

/// Build a `ClientRegistration` from a verified key + spec workspaces.
/// Pure so the watcher's apply-path can be tested without kube I/O.
pub fn registration_from(spec: &EnrollmentSpec, vk: VerifyingKey) -> ClientRegistration {
    ClientRegistration {
        verifying_key: vk,
        workspaces: spec.workspaces.clone(),
    }
}

/// Absolute unix-seconds expiry for a code minted at `now_secs`.
/// Separated from `chrono::Utc::now()` so the addition is testable.
pub fn expires_at(now_secs: i64) -> i64 {
    now_secs + DEFAULT_ENROLLMENT_TTL_SECS
}

/// Watch Enrollment CRs in `namespace` and keep the verifier's cache in
/// sync, minting codes for fresh Enrollments along the way. `ready_tx`
/// signals when the initial sync has completed.
pub async fn watch_enrollments(
    client: KubeClient,
    namespace: &str,
    signing_key: Arc<SigningKey>,
    verifier: Arc<ClientSignatureVerifier>,
    ready_tx: tokio::sync::watch::Sender<bool>,
) -> Result<(), String> {
    let api: Api<Enrollment> = Api::namespaced(client, namespace);
    let mut stream = watcher::watcher(api.clone(), watcher::Config::default()).boxed();
    let registrations = verifier.registrations();

    // During a relist (Init..InitDone), installs accumulate here instead
    // of the live map; InitDone swaps this in atomically. The live map
    // is never emptied, so a just-redeemed key can't transiently vanish.
    let mut scratch: HashMap<String, ClientRegistration> = HashMap::new();

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("enrollment watcher error: {e}"))?
    {
        match event {
            Event::Init => {
                tracing::info!("enrollment watcher initialized");
                scratch.clear();
            }
            Event::InitApply(cr) => {
                handle_apply(&api, &cr, &signing_key, &mut scratch).await;
            }
            Event::Apply(cr) => {
                let mut live = registrations.write().await;
                handle_apply(&api, &cr, &signing_key, &mut live).await;
            }
            Event::Delete(cr) => {
                let name = cr.metadata.name.clone().unwrap_or_default();
                tracing::info!(enrollment = %name, "enrollment deleted");
                registrations.write().await.remove(&name);
            }
            Event::InitDone => {
                tracing::info!("enrollment watcher initial sync complete");
                let mut live = registrations.write().await;
                on_init_done(&mut live, &mut scratch);
                drop(live);
                let _ = ready_tx.send(true);
            }
        }
    }

    tracing::warn!("enrollment watcher stream ended");
    Ok(())
}

async fn handle_apply(
    api: &Api<Enrollment>,
    cr: &Enrollment,
    signing_key: &SigningKey,
    target: &mut HashMap<String, ClientRegistration>,
) {
    let Some(name) = cr.metadata.name.clone() else {
        tracing::warn!("enrollment has no name; skipping");
        return;
    };

    match decide_action(cr.status.as_ref()) {
        EnrollmentAction::InstallKey => {
            apply_install(cr, &name, target);
        }
        EnrollmentAction::MintCode => {
            // EnrollmentClaims.workspace is informational only — the
            // per-request workspace assertion (x-sig-workspace) is what
            // gates authorization. We stamp the first authorized
            // workspace so the field has a sensible value for logs.
            let workspace = cr.spec.workspaces.first().cloned().unwrap_or_default();
            let code_id = uuid::Uuid::new_v4().to_string();
            let code = crate::enrollment::sign_enrollment_code(
                signing_key,
                &workspace,
                &name,
                &code_id,
                DEFAULT_ENROLLMENT_TTL_SECS,
            );
            let expires = expires_at(chrono::Utc::now().timestamp());
            let patch = build_mint_patch(&name, &code, expires);
            let pp = PatchParams::apply(FIELD_MANAGER).force();
            match api.patch_status(&name, &pp, &Patch::Apply(&patch)).await {
                Ok(_) => tracing::info!(enrollment = %name, "minted enrollment code"),
                Err(e) => tracing::error!(enrollment = %name, "patch_status failed: {e}"),
            }
        }
        EnrollmentAction::NoOp => {
            tracing::debug!(enrollment = %name, "enrollment awaiting redeem");
        }
    }
}

/// Map effect of `Event::InitDone`: atomically replace the live map with
/// the relist scratch. Replace, not merge — a key absent from the relist
/// is dropped (self-heals on the next incremental Apply). `scratch` is
/// left empty for the next relist.
fn on_init_done(
    live: &mut HashMap<String, ClientRegistration>,
    scratch: &mut HashMap<String, ClientRegistration>,
) {
    *live = std::mem::take(scratch);
}

/// Install an `InstallKey` CR's registered key into `target`; skips a
/// malformed key.
fn apply_install(cr: &Enrollment, name: &str, target: &mut HashMap<String, ClientRegistration>) {
    let Some(b64) = cr.status.as_ref().and_then(|s| s.public_key.as_ref()) else {
        return;
    };
    let Some(vk) = parse_public_key_b64(b64) else {
        tracing::warn!(enrollment = %name, "enrollment publicKey malformed; not installing");
        return;
    };
    tracing::info!(enrollment = %name, "installing enrollment public key");
    target.insert(name.to_string(), registration_from(&cr.spec, vk));
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::ObjectMeta;
    use p256::ecdsa::SigningKey as P256SigningKey;
    use p256::elliptic_curve::rand_core::OsRng;

    fn fresh_status_with_public_key(b64: &str) -> EnrollmentStatus {
        EnrollmentStatus {
            public_key: Some(b64.to_string()),
            ..Default::default()
        }
    }

    fn random_public_key_b64() -> String {
        let sk = P256SigningKey::random(&mut OsRng);
        let sec1 = sk
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        base64::engine::general_purpose::STANDARD.encode(&sec1)
    }

    fn install_key_cr(name: &str, public_key_b64: &str) -> Enrollment {
        Enrollment {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: EnrollmentSpec {
                workspaces: vec!["hello-world".into()],
            },
            status: Some(fresh_status_with_public_key(public_key_b64)),
        }
    }

    #[test]
    fn decide_action_with_no_status_returns_mint_code() {
        assert_eq!(decide_action(None), EnrollmentAction::MintCode);
    }

    #[test]
    fn decide_action_with_default_status_returns_mint_code() {
        let status = EnrollmentStatus::default();
        assert_eq!(decide_action(Some(&status)), EnrollmentAction::MintCode);
    }

    #[test]
    fn decide_action_with_public_key_returns_install_key() {
        let status = fresh_status_with_public_key("anything");
        assert_eq!(decide_action(Some(&status)), EnrollmentAction::InstallKey);
    }

    #[test]
    fn decide_action_with_only_enrollment_code_returns_noop() {
        let status = EnrollmentStatus {
            enrollment_code: Some("code".into()),
            ..Default::default()
        };
        assert_eq!(decide_action(Some(&status)), EnrollmentAction::NoOp);
    }

    #[test]
    fn decide_action_with_both_public_key_and_enrollment_code_returns_install_key() {
        // Transient state right after a redeem patch lands publicKey
        // but before any code-clearing patch arrives. publicKey wins.
        let status = EnrollmentStatus {
            public_key: Some("pk".into()),
            enrollment_code: Some("code".into()),
            ..Default::default()
        };
        assert_eq!(decide_action(Some(&status)), EnrollmentAction::InstallKey);
    }

    #[test]
    fn parse_public_key_b64_round_trips_valid_sec1() {
        let sk = P256SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let sec1 = vk.to_encoded_point(false).as_bytes().to_vec();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&sec1);

        let decoded = parse_public_key_b64(&b64).expect("valid key must decode");
        assert_eq!(decoded.to_encoded_point(false), vk.to_encoded_point(false));
    }

    #[test]
    fn parse_public_key_b64_returns_none_for_invalid_base64() {
        assert!(parse_public_key_b64("!!!not base64!!!").is_none());
    }

    #[test]
    fn parse_public_key_b64_returns_none_for_valid_base64_but_invalid_sec1() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"not a key");
        assert!(parse_public_key_b64(&b64).is_none());
    }

    #[test]
    fn parse_public_key_b64_returns_none_for_empty_string() {
        assert!(parse_public_key_b64("").is_none());
    }

    #[test]
    fn build_mint_patch_carries_apiversion_kind_and_name() {
        let patch = build_mint_patch("enr-alpha", "code-1", 1_700_000_000);
        assert_eq!(patch["apiVersion"], "sycophant.md/v1");
        assert_eq!(patch["kind"], "Enrollment");
        assert_eq!(patch["metadata"]["name"], "enr-alpha");
    }

    #[test]
    fn build_mint_patch_sets_status_enrollment_code_and_expires_at() {
        let patch = build_mint_patch("c", "code-1", 1_700_000_000);
        assert_eq!(patch["status"]["enrollmentCode"], "code-1");
        assert_eq!(patch["status"]["enrollmentCodeExpiresAt"], 1_700_000_000);
    }

    #[test]
    fn build_mint_patch_does_not_set_public_key_or_enrolled_at() {
        let patch = build_mint_patch("c", "code-1", 1_700_000_000);
        assert!(patch["status"].get("publicKey").is_none());
        assert!(patch["status"].get("enrolledAt").is_none());
    }

    #[test]
    fn build_redeem_patch_carries_apiversion_kind_and_name() {
        let patch = build_redeem_patch("enr-alpha", "pk-b64", "2026-05-17T12:00:00Z");
        assert_eq!(patch["apiVersion"], "sycophant.md/v1");
        assert_eq!(patch["kind"], "Enrollment");
        assert_eq!(patch["metadata"]["name"], "enr-alpha");
    }

    #[test]
    fn build_redeem_patch_sets_status_public_key_and_enrolled_at() {
        let patch = build_redeem_patch("c", "pk-b64", "2026-05-17T12:00:00Z");
        assert_eq!(patch["status"]["publicKey"], "pk-b64");
        assert_eq!(patch["status"]["enrolledAt"], "2026-05-17T12:00:00Z");
    }

    #[test]
    fn build_redeem_patch_omits_enrollment_code_fields() {
        let patch = build_redeem_patch("c", "pk-b64", "ts");
        assert!(patch["status"].get("enrollmentCode").is_none());
        assert!(patch["status"].get("enrollmentCodeExpiresAt").is_none());
    }

    #[test]
    fn registration_from_copies_workspaces_and_carries_key() {
        let sk = P256SigningKey::random(&mut OsRng);
        let vk = *sk.verifying_key();
        let spec = EnrollmentSpec {
            workspaces: vec!["alpha".into(), "beta".into()],
        };
        let reg = registration_from(&spec, vk);
        assert_eq!(
            reg.workspaces,
            vec!["alpha".to_string(), "beta".to_string()]
        );
        assert_eq!(
            reg.verifying_key.to_encoded_point(false),
            vk.to_encoded_point(false)
        );
    }

    #[test]
    fn default_enrollment_ttl_secs_is_one_hour() {
        assert_eq!(DEFAULT_ENROLLMENT_TTL_SECS, 3600);
    }

    #[test]
    fn expires_at_adds_ttl_to_now() {
        assert_eq!(expires_at(100), 100 + DEFAULT_ENROLLMENT_TTL_SECS);
    }

    #[test]
    fn expires_at_at_zero_returns_ttl() {
        assert_eq!(expires_at(0), DEFAULT_ENROLLMENT_TTL_SECS);
    }

    #[test]
    fn field_manager_is_tightbeam_controller() {
        assert_eq!(FIELD_MANAGER, "tightbeam-controller");
    }

    // Load-bearing: breaks if `on_init_done` (the `Event::InitDone` map
    // effect) merges into the live map instead of replacing it. The
    // relist snapshot is authoritative; a key absent from it is
    // intentionally dropped (self-heals on the next incremental Apply).
    #[test]
    fn init_done_replaces_live_with_relist_scratch() {
        // Live pre-seeded with X (as a stale redeem-install), the relist
        // contains only Y.
        let x_key = random_public_key_b64();
        let x_vk = parse_public_key_b64(&x_key).unwrap();
        let mut live: HashMap<String, ClientRegistration> = HashMap::new();
        let spec_x = EnrollmentSpec {
            workspaces: vec!["ws-x".into()],
        };
        live.insert("client-x".into(), registration_from(&spec_x, x_vk));

        // Relist delivers only Y into scratch, then InitDone swaps.
        let mut scratch: HashMap<String, ClientRegistration> = HashMap::new();

        let y_key = random_public_key_b64();
        let cr_y = install_key_cr("client-y", &y_key);
        apply_install(&cr_y, "client-y", &mut scratch);

        on_init_done(&mut live, &mut scratch);

        assert_eq!(live.len(), 1, "InitDone must replace, not merge");
        assert!(live.contains_key("client-y"));
        assert!(
            !live.contains_key("client-x"),
            "X absent from the relist is dropped by replace semantics"
        );
    }

    // Guards the seam: an InstallKey CR routed during init lands in
    // scratch, never in the live map. Breaks if `apply_install` is
    // pointed at the wrong target during the init phase.
    #[test]
    fn init_apply_install_lands_in_scratch_not_live() {
        let live: HashMap<String, ClientRegistration> = HashMap::new();
        let mut scratch: HashMap<String, ClientRegistration> = HashMap::new();

        let y_key = random_public_key_b64();
        let cr_y = install_key_cr("client-y", &y_key);
        apply_install(&cr_y, "client-y", &mut scratch);

        assert!(scratch.contains_key("client-y"));
        assert!(
            live.is_empty(),
            "init-phase install must not touch the live map"
        );
    }

    // Malformed key on the install path is skipped, not inserted.
    #[test]
    fn apply_install_skips_malformed_public_key() {
        let mut target: HashMap<String, ClientRegistration> = HashMap::new();
        let cr = install_key_cr("client-bad", "!!!not a key!!!");
        apply_install(&cr, "client-bad", &mut target);
        assert!(target.is_empty());
    }
}
