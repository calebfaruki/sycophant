//! Relay-controller's client for dialing per-workspace harness
//! pods.
//!
//! Relay forwards external `WatchTools` and `CallTool` calls into the
//! workspace's harness, which owns the unified tool catalog. The pool
//! lazy-connects on first use per workspace and reuses the tonic Channel
//! (HTTP/2 multiplexed) across subsequent calls.
//!
//! Each per-workspace harness runs as a Service named
//! `harness-{workspace}` per the chart. The token presented is
//! audience `relay.harness.sycophant.md`; harness pins this
//! on TokenReview to verify the caller.

use harness_proto::harness_control_client::HarnessControlClient;
use shared::auth::{SaTokenInterceptor, RELAY_HARNESS_TOKEN_PATH};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;

type AuthenticatedChannel = InterceptedService<Channel, SaTokenInterceptor>;

/// Per-workspace harness client. Cloning is cheap (tonic `Channel`
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

/// Pool of per-workspace harness clients. Returns a cloned client per
/// call; cloning is cheap and shares the underlying HTTP/2 connection.
pub struct HarnessClientPool {
    /// workspace name → connected client.
    clients: RwLock<HashMap<String, HarnessClient>>,
    /// Service-DNS template; `{workspace}` is substituted at lookup time.
    service_template: String,
}

impl HarnessClientPool {
    /// `namespace` is the controller's namespace; harness Services live
    /// in the same namespace by chart contract.
    pub fn new(namespace: &str) -> Arc<Self> {
        Self::from_service_template(format!(
            "http://harness-{{workspace}}.{namespace}.svc.cluster.local:9090"
        ))
    }

    /// Build a pool from a ready service-DNS template. `{workspace}` is
    /// substituted at lookup time.
    pub(crate) fn from_service_template(template: String) -> Arc<Self> {
        Arc::new(Self {
            clients: RwLock::new(HashMap::new()),
            service_template: template,
        })
    }

    fn addr_for(&self, workspace: &str) -> String {
        self.service_template.replace("{workspace}", workspace)
    }

    /// Get-or-connect. Returns a cloned client.
    pub async fn get(&self, workspace: &str) -> Result<HarnessClient, String> {
        {
            let clients = self.clients.read().await;
            if let Some(c) = clients.get(workspace) {
                return Ok(c.clone());
            }
        }
        let addr = self.addr_for(workspace);
        let channel = shared::grpc_client::connect_with_keepalive(&addr, "harness").await?;
        let inner = HarnessControlClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(RELAY_HARNESS_TOKEN_PATH),
        );
        let client = HarnessClient { inner };
        let mut clients = self.clients.write().await;
        // Another waiter may have raced us — re-check before insert.
        if let Some(existing) = clients.get(workspace) {
            return Ok(existing.clone());
        }
        clients.insert(workspace.to_string(), client.clone());
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_for_substitutes_workspace_into_template() {
        let pool = HarnessClientPool::new("sycophant");
        assert_eq!(
            pool.addr_for("hello-world"),
            "http://harness-hello-world.sycophant.svc.cluster.local:9090"
        );
    }

    #[test]
    fn addr_for_distinct_workspaces_yield_distinct_addrs() {
        let pool = HarnessClientPool::new("ns");
        assert_ne!(pool.addr_for("alpha"), pool.addr_for("beta"));
    }
}
