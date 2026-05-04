//! Shared primitives used across the workspace.

pub mod auth;
pub mod scheduling;

use k8s_openapi::api::core::v1::{Capabilities, SecurityContext};

pub fn hardened_security_context() -> SecurityContext {
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

const SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// Try to initialize a kube client.
///
/// Returns `Ok(Some(client))` in-cluster or when a kubeconfig is available.
/// Returns `Ok(None)` for local dev (no SA token, `try_default` failed).
/// Returns `Err` if running in-cluster (SA token present) but client init failed.
pub async fn try_init_kube_client() -> Result<Option<kube::Client>, String> {
    let sa_token_exists = std::path::Path::new(SA_TOKEN_PATH).exists();
    match kube::Client::try_default().await {
        Ok(c) => {
            tracing::info!("k8s client initialized");
            Ok(Some(c))
        }
        Err(e) if sa_token_exists => Err(format!(
            "running in-cluster but kube client init failed: {e}"
        )),
        Err(_) => {
            tracing::info!("no kube client available (local dev)");
            Ok(None)
        }
    }
}

/// Retry an async operation up to `max_attempts` times with linear backoff.
///
/// Sleeps `attempt` seconds between failures (1s before retry 2, 2s before retry 3, ...).
/// Returns the operation's `Ok` on first success. Returns the operation's last `Err`
/// once `max_attempts` have been exhausted. The closure receives the 1-based attempt
/// number so it can include it in error context.
pub async fn retry_with_backoff<T, E, F, Fut>(
    max_attempts: u64,
    label: &str,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    assert!(
        max_attempts >= 1,
        "retry_with_backoff requires max_attempts >= 1"
    );
    for attempt in 1..=max_attempts {
        match op(attempt).await {
            Ok(value) => return Ok(value),
            Err(e) if attempt < max_attempts => {
                tracing::warn!(label, attempt, error = %e, "operation failed, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(attempt)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("loop body returns on every iteration when max_attempts >= 1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    #[test]
    fn hardened_security_context_pins_all_fields() {
        let sc = hardened_security_context();
        assert_eq!(sc.run_as_non_root, Some(true));
        assert_eq!(sc.run_as_user, Some(1000));
        assert_eq!(sc.read_only_root_filesystem, Some(true));
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        let caps = sc.capabilities.expect("capabilities must be set");
        assert_eq!(caps.drop, Some(vec!["ALL".to_string()]));
    }

    #[tokio::test]
    async fn retry_succeeds_on_first_attempt() {
        tokio::time::pause();
        let calls = AtomicU64::new(0);
        let result: Result<u64, String> = retry_with_backoff(10, "test", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(42) }
        })
        .await;
        assert_eq!(result, Ok(42));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        tokio::time::pause();
        let calls = AtomicU64::new(0);
        let result: Result<u64, String> = retry_with_backoff(10, "test", |attempt| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 3 {
                    Err(format!("fail at {attempt}"))
                } else {
                    Ok(42)
                }
            }
        })
        .await;
        assert_eq!(result, Ok(42));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_returns_err_after_max_attempts() {
        tokio::time::pause();
        let calls = AtomicU64::new(0);
        let result: Result<u64, String> = retry_with_backoff(3, "test", |attempt| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err::<u64, _>(format!("fail at {attempt}")) }
        })
        .await;
        assert_eq!(result, Err("fail at 3".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_invokes_op_with_sequential_attempt_numbers() {
        tokio::time::pause();
        let attempts: Mutex<Vec<u64>> = Mutex::new(Vec::new());
        let result: Result<u64, String> = retry_with_backoff(3, "test", |attempt| {
            attempts.lock().unwrap().push(attempt);
            async { Err("fail".to_string()) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(*attempts.lock().unwrap(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn retry_does_not_sleep_after_final_failure() {
        tokio::time::pause();
        let start = tokio::time::Instant::now();
        let _: Result<u64, String> =
            retry_with_backoff(2, "test", |_| async { Err("fail".to_string()) }).await;
        // Sleeps only between attempts: max_attempts=2 sleeps once (1s), not twice.
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "expected only one 1s sleep, got {elapsed:?}"
        );
    }
}
