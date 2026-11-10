//! Relay-controller's client for dialing per-agent harness
//! pods.
//!
//! Relay forwards external client calls (the tool catalog, tool calls and
//! the agent's conversation) into the agent's harness. The pool
//! lazy-connects on first use per agent and reuses the tonic Channel
//! (HTTP/2 multiplexed) across subsequent calls.
//!
//! Each per-agent harness runs as a Service named
//! `ag-{agent}` per the chart. NetworkPolicy admits relay-ctrl alone to
//! the harness's port; the harness checks no token.

use harness_proto::harness_control_client::HarnessControlClient;
use shared::auth::{SaTokenInterceptor, AGENT_PREFIX, RELAY_HARNESS_TOKEN_PATH};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;

type AuthenticatedChannel = InterceptedService<Channel, SaTokenInterceptor>;

/// Per-agent harness client. Cloning is cheap (tonic `Channel`
/// is `Arc`-backed); cloned clients share the same HTTP/2 connection.
#[derive(Clone)]
pub struct HarnessClient {
    inner: HarnessControlClient<AuthenticatedChannel>,
}

impl HarnessClient {
    pub fn into_inner(self) -> HarnessControlClient<AuthenticatedChannel> {
        self.inner
    }

    #[allow(clippy::should_implement_trait)]
    pub fn as_mut(&mut self) -> &mut HarnessControlClient<AuthenticatedChannel> {
        &mut self.inner
    }
}

/// Total budget for one pool dial. The forward path is synchronous with the
/// caller's RPC: an agent whose harness is down (or whose Service DNS
/// does not resolve) must answer `Unavailable` promptly, not hold the caller
/// through the connect retry loop's full backoff.
const DIAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// Pool of per-agent harness clients. Returns a cloned client per
/// call; cloning is cheap and shares the underlying HTTP/2 connection.
pub struct HarnessClientPool {
    /// agent name → connected client.
    clients: RwLock<HashMap<String, HarnessClient>>,
    /// Service-DNS template; `{agent}` is substituted at lookup time.
    service_template: String,
}

impl HarnessClientPool {
    /// `namespace` is the controller's namespace; harness Services live
    /// in the same namespace by chart contract.
    pub fn new(namespace: &str) -> Arc<Self> {
        // Port 9091 is the harness's HarnessControl port. Keep in step with
        // `RELAY_FORWARD_PORT` in the harness and the harness Service port.
        Self::from_service_template(format!(
            "http://{}{{agent}}.{namespace}.svc.cluster.local:9091",
            AGENT_PREFIX
        ))
    }

    /// Build a pool from a ready service-DNS template. `{agent}` is
    /// substituted at lookup time.
    pub(crate) fn from_service_template(template: String) -> Arc<Self> {
        Arc::new(Self {
            clients: RwLock::new(HashMap::new()),
            service_template: template,
        })
    }

    fn addr_for(&self, agent: &str) -> String {
        self.service_template.replace("{agent}", agent)
    }

    /// Get-or-connect. Returns a cloned client. The connect rides the retry
    /// loop but the whole dial is bounded by [`DIAL_BUDGET`]: a caller whose
    /// agent harness is down gets `Unavailable` within the budget.
    pub async fn get(&self, agent: &str) -> Result<HarnessClient, String> {
        {
            let clients = self.clients.read().await;
            if let Some(c) = clients.get(agent) {
                return Ok(c.clone());
            }
        }
        let addr = self.addr_for(agent);
        let connect = shared::grpc_client::connect_with_keepalive(&addr, "harness");
        let channel = match tokio::time::timeout(DIAL_BUDGET, connect).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(format!(
                    "timed out dialing harness at {addr} within the dial budget"
                ))
            }
        };
        let inner = HarnessControlClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(RELAY_HARNESS_TOKEN_PATH),
        );
        let client = HarnessClient { inner };
        let mut clients = self.clients.write().await;
        // Another waiter may have raced us — re-check before insert.
        if let Some(existing) = clients.get(agent) {
            return Ok(existing.clone());
        }
        clients.insert(agent.to_string(), client.clone());
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_for_substitutes_agent_into_template() {
        let pool = HarnessClientPool::new("sycophant");
        assert_eq!(
            pool.addr_for("hello-world"),
            "http://ag-hello-world.sycophant.svc.cluster.local:9091"
        );
    }
}
