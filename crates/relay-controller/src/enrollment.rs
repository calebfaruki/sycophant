//! One-time enrollment codes: signed JWTs a client redeems to register
//! its public key. Minting lives in `enrollment_watcher` (driven by an
//! Enrollment CR); redemption is verified in `gateway`. Both sign/verify
//! with the per-tenant Ed25519 key.

use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::Status;

/// JWT claims for a one-time enrollment code.
///
/// The relay-controller's `enrollment_watcher` mints a code for an
/// Enrollment CR; the user presents it to a client app; the app calls
/// `RedeemEnrollment` with a freshly generated public key. The gateway
/// validates the code's signature + expiry + claims, then persists the
/// public key. Signed with the per-tenant Ed25519 signing key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentClaims {
    /// Workspace the enrolled client will be scoped to.
    pub workspace: String,
    /// Operator-assigned human-readable client name (e.g. "calebs-iphone").
    pub device_name: String,
    /// UUID for this enrollment code; reserved for one-time-use enforcement.
    pub code_id: String,
    /// Unix-seconds expiry. Short by design (default 1 hour).
    pub exp: i64,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must be after Unix epoch")
        .as_secs() as i64
}

fn signing_key_to_encoding_key(signing_key: &SigningKey) -> EncodingKey {
    let pkcs8_pem = signing_key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("PKCS#8 PEM serialization is infallible");
    EncodingKey::from_ed_pem(pkcs8_pem.as_bytes())
        .expect("EncodingKey from valid PEM is infallible")
}

fn verifying_key_to_decoding_key(verifying_key: &VerifyingKey) -> DecodingKey {
    let spki_pem = verifying_key
        .to_public_key_pem(LineEnding::LF)
        .expect("VerifyingKey → SPKI PEM is infallible");
    DecodingKey::from_ed_pem(spki_pem.as_bytes()).expect("DecodingKey from valid PEM is infallible")
}

/// Sign a one-time enrollment code. Used by the relay-controller's
/// `enrollment_watcher` when minting a code for an Enrollment CR.
/// `ttl_secs` defaults to 3600 (1 hour) at the call site.
pub fn sign_enrollment_code(
    signing_key: &SigningKey,
    workspace: &str,
    device_name: &str,
    code_id: &str,
    ttl_secs: i64,
) -> String {
    let claims = EnrollmentClaims {
        workspace: workspace.to_string(),
        device_name: device_name.to_string(),
        code_id: code_id.to_string(),
        exp: now_secs() + ttl_secs,
    };
    let header = Header::new(Algorithm::EdDSA);
    let encoding_key = signing_key_to_encoding_key(signing_key);
    encode(&header, &claims, &encoding_key).expect("enrollment code encode is infallible")
}

/// Verify an enrollment code. Returns the decoded claims on success; maps any
/// failure (bad signature, expired, missing claim, malformed) to a single
/// `PermissionDenied` status — the caller has no business distinguishing.
#[allow(clippy::result_large_err)]
pub fn verify_enrollment_code(
    verifying_key: &VerifyingKey,
    code: &str,
) -> Result<EnrollmentClaims, Status> {
    let decoding_key = verifying_key_to_decoding_key(verifying_key);
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.required_spec_claims = ["exp".to_string()].into_iter().collect();
    let token_data = decode::<EnrollmentClaims>(code, &decoding_key, &validation)
        .map_err(|_| Status::permission_denied("invalid enrollment code"))?;
    Ok(token_data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> (SigningKey, VerifyingKey) {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        (signing_key, verifying_key)
    }

    #[test]
    fn sign_enrollment_code_round_trips_through_verify() {
        let (sk, vk) = keypair();
        let now = now_secs();
        let code = sign_enrollment_code(&sk, "hello-world", "calebs-iphone", "code-uuid-1", 3600);
        let claims = verify_enrollment_code(&vk, &code).unwrap();
        assert_eq!(claims.workspace, "hello-world");
        assert_eq!(claims.device_name, "calebs-iphone");
        assert_eq!(claims.code_id, "code-uuid-1");
        // Tight bounds: exp must be roughly now + 3600. Lower bound catches a
        // dropped-ttl mutation; upper bound catches a `+ → *` mutation that
        // would explode exp into the year-millions range.
        assert!(claims.exp > now + 3500, "exp too low: {}", claims.exp);
        assert!(claims.exp < now + 3700, "exp too high: {}", claims.exp);
    }

    #[test]
    fn verify_enrollment_code_rejects_wrong_signing_key() {
        let (sk1, _) = keypair();
        let (_, vk2) = keypair();
        let code = sign_enrollment_code(&sk1, "hello-world", "calebs-iphone", "code-1", 3600);
        let err = verify_enrollment_code(&vk2, &code).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn verify_enrollment_code_rejects_expired_code() {
        // Use a clearly-expired ttl (1 hour in the past) — jsonwebtoken's
        // default leeway absorbs small offsets.
        let (sk, vk) = keypair();
        let code = sign_enrollment_code(&sk, "hello-world", "calebs-iphone", "code-1", -3600);
        let err = verify_enrollment_code(&vk, &code).unwrap_err();
        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }
}
