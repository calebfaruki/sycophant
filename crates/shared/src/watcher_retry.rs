//! Resilient wrapper for kube-rs watchers.
//!
//! The low-level `kube::runtime::watcher::watcher()` surfaces stream errors
//! (HTTP 429 "storage is (re)initializing", watch desync, network blips) to
//! the caller and expects them to manage reconnection. Without that, a single
//! transient hiccup permanently kills the watcher task — the controller Pod
//! stays `Available` but its in-memory state goes stale forever.
//!
//! `run_watcher_forever` is the canonical fix: loop the watcher closure,
//! restart on any error or clean return, with exponential backoff capped at
//! 30s so we don't tight-spin on persistent failures.

use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use tracing::{error, warn};

/// Loop a watcher closure forever, restarting on any error or clean return.
/// Backoff doubles each iteration (1s → 2s → 4s → ... cap 30s) and resets
/// only on the next successful Ok return (since stable watchers should never
/// return Ok in practice, the cap protects against persistent-failure spam).
pub async fn run_watcher_forever<F, Fut, E>(name: &'static str, mut f: F) -> !
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(30);
    loop {
        match f().await {
            Ok(()) => warn!(watcher = %name, "returned Ok unexpectedly; restarting"),
            Err(e) => error!(
                watcher = %name,
                error = %e,
                delay_secs = delay.as_secs(),
                "errored; restarting after backoff",
            ),
        }
        sleep(delay).await;
        delay = (delay * 2).min(max_delay);
    }
}

/// Spawn `run_watcher_forever` as a tokio task. Caller owns the JoinHandle
/// (use it for abort on shutdown). The closure typically captures a cloned
/// `kube::Client` plus per-task state.
pub fn spawn_watcher_task<F, Fut, E>(name: &'static str, f: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), E>> + Send,
    E: std::fmt::Display + Send,
{
    tokio::spawn(async move {
        run_watcher_forever(name, f).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::time::{timeout, Duration as TokioDuration};

    /// The loop must keep running across errors. Under paused time, an
    /// in-test `sleep` auto-advances the virtual clock, firing the helper's
    /// pending backoff sleeps in order so the spawned task processes
    /// multiple iterations.
    #[tokio::test(start_paused = true)]
    async fn run_watcher_forever_restarts_on_error() {
        let calls = Arc::new(AtomicU64::new(0));
        let calls_inner = calls.clone();

        let task = tokio::spawn(async move {
            run_watcher_forever("test", move || {
                let calls = calls_inner.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>("synthetic error")
                }
            })
            .await
        });

        // 30s of virtual time covers 1 + 2 + 4 + 8 + 16 = 31s of backoff,
        // enough for 5+ restarts (each iteration: f() then sleep).
        tokio::time::sleep(TokioDuration::from_secs(30)).await;

        task.abort();
        assert!(
            calls.load(Ordering::SeqCst) >= 4,
            "expected at least 4 restarts within 30s, got {}",
            calls.load(Ordering::SeqCst),
        );
    }

    /// Clean Ok returns should ALSO restart — watchers aren't expected to
    /// return Ok, but if they do (e.g., stream EOF), we want to reconnect.
    #[tokio::test(start_paused = true)]
    async fn run_watcher_forever_restarts_on_ok() {
        let calls = Arc::new(AtomicU64::new(0));
        let calls_inner = calls.clone();

        let task = tokio::spawn(async move {
            run_watcher_forever("test", move || {
                let calls = calls_inner.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), &'static str>(())
                }
            })
            .await
        });

        tokio::time::sleep(TokioDuration::from_secs(10)).await;

        task.abort();
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "Ok returns must still trigger restart, got {} calls",
            calls.load(Ordering::SeqCst),
        );
    }

    /// The function signature is `-> !`, so it must never return.
    /// `tokio::time::timeout` is the only way to bound it.
    #[tokio::test(start_paused = true)]
    async fn run_watcher_forever_never_returns() {
        let result = timeout(
            TokioDuration::from_secs(60),
            run_watcher_forever("test", || async { Err::<(), _>("always") }),
        )
        .await;
        assert!(result.is_err(), "run_watcher_forever must never return");
    }

    /// The `spawn_watcher_task` wrapper must drive the underlying loop —
    /// catches a regression where the helper accidentally calls f() once
    /// and exits.
    #[tokio::test(start_paused = true)]
    async fn spawn_watcher_task_drives_loop() {
        let calls = Arc::new(AtomicU64::new(0));
        let calls_inner = calls.clone();

        let handle = spawn_watcher_task("test", move || {
            let calls = calls_inner.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>("synthetic error")
            }
        });

        tokio::time::sleep(TokioDuration::from_secs(30)).await;
        handle.abort();
        assert!(
            calls.load(Ordering::SeqCst) >= 4,
            "expected at least 4 restarts within 30s, got {}",
            calls.load(Ordering::SeqCst),
        );
    }
}
