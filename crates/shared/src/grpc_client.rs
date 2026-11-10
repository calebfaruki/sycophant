//! Shared tonic Channel construction with HTTP/2 + TCP keepalive.
//!
//! Without keepalive, a long-lived gRPC stream sits half-open if the peer
//! disappears without sending a FIN/RST — `stream.next().await` blocks
//! indefinitely instead of erroring, so any reconnect logic in the caller
//! never fires. Every outbound channel in the workspace SHOULD go through
//! this helper.

use std::time::Duration;
use tonic::transport::{Channel, Endpoint};

pub const H2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
pub const H2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);
/// Cap on a single connect attempt. Without one a dial to an address that
/// never answers (unresolvable Service DNS while the harness pod is down)
/// hangs until the OS gives up — far longer than any caller's patience.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

pub fn keepalive_endpoint(addr: &str) -> Result<Endpoint, String> {
    Endpoint::from_shared(addr.to_string())
        .map_err(|e| format!("invalid endpoint {addr}: {e}"))
        .map(|e| {
            e.http2_keep_alive_interval(H2_KEEPALIVE_INTERVAL)
                .keep_alive_timeout(H2_KEEPALIVE_TIMEOUT)
                .keep_alive_while_idle(true)
                .tcp_keepalive(Some(TCP_KEEPALIVE))
                .connect_timeout(CONNECT_TIMEOUT)
        })
}

pub async fn connect_with_keepalive(addr: &str, label: &'static str) -> Result<Channel, String> {
    let addr = addr.to_string();
    crate::retry_with_backoff(10, label, |_| {
        let addr = addr.clone();
        async move {
            keepalive_endpoint(&addr)?
                .connect()
                .await
                .map_err(|e| format!("failed to connect to {label} at {addr}: {e}"))
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_with_keepalive_returns_err_on_invalid_uri() {
        tokio::time::pause();
        let result = connect_with_keepalive("not a uri", "test").await;
        let err = result.unwrap_err();
        assert!(err.contains("invalid endpoint"), "got: {err}");
    }
}
