//! Tightbeam-controller's client for dialing per-workspace transponder
//! pods.
//!
//! Tightbeam forwards external `WatchTools` and `CallTool` calls into the
//! workspace's transponder, which owns the unified tool catalog. The pool
//! lazy-connects on first use per workspace and reuses the tonic Channel
//! (HTTP/2 multiplexed) across subsequent calls.
//!
//! Each per-workspace transponder runs as a Service named
//! `transponder-{workspace}` per the chart. The token presented is
//! audience `hangar.transponder.sycophant.md`; transponder pins this
//! on TokenReview to verify the caller.

use shared::auth::{SaTokenInterceptor, HANGAR_TRANSPONDER_TOKEN_PATH};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::codegen::InterceptedService;
use tonic::transport::Channel;
use transponder_proto::transponder_control_client::TransponderControlClient;

type AuthenticatedChannel = InterceptedService<Channel, SaTokenInterceptor>;

/// Per-workspace transponder client. Cloning is cheap (tonic `Channel`
/// is `Arc`-backed); cloned clients share the same HTTP/2 connection.
#[derive(Clone)]
pub struct TransponderClient {
    inner: TransponderControlClient<AuthenticatedChannel>,
}

impl TransponderClient {
    pub fn into_inner(self) -> TransponderControlClient<AuthenticatedChannel> {
        self.inner
    }

    #[allow(clippy::should_implement_trait)]
    pub fn as_mut(&mut self) -> &mut TransponderControlClient<AuthenticatedChannel> {
        &mut self.inner
    }
}

/// Pool of per-workspace transponder clients. Returns a cloned client per
/// call; cloning is cheap and shares the underlying HTTP/2 connection.
pub struct TransponderClientPool {
    /// workspace name → connected client.
    clients: RwLock<HashMap<String, TransponderClient>>,
    /// Service-DNS template; `{workspace}` is substituted at lookup time.
    service_template: String,
}

impl TransponderClientPool {
    /// `namespace` is the controller's namespace; transponder Services live
    /// in the same namespace by chart contract.
    pub fn new(namespace: &str) -> Arc<Self> {
        Self::from_service_template(format!(
            "http://transponder-{{workspace}}.{namespace}.svc.cluster.local:9090"
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
    pub async fn get(&self, workspace: &str) -> Result<TransponderClient, String> {
        {
            let clients = self.clients.read().await;
            if let Some(c) = clients.get(workspace) {
                return Ok(c.clone());
            }
        }
        let addr = self.addr_for(workspace);
        let channel = shared::grpc_client::connect_with_keepalive(&addr, "transponder").await?;
        let inner = TransponderControlClient::with_interceptor(
            channel,
            SaTokenInterceptor::new(HANGAR_TRANSPONDER_TOKEN_PATH),
        );
        let client = TransponderClient { inner };
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
        let pool = TransponderClientPool::new("sycophant");
        assert_eq!(
            pool.addr_for("hello-world"),
            "http://transponder-hello-world.sycophant.svc.cluster.local:9090"
        );
    }

    #[test]
    fn addr_for_distinct_workspaces_yield_distinct_addrs() {
        let pool = TransponderClientPool::new("ns");
        assert_ne!(pool.addr_for("alpha"), pool.addr_for("beta"));
    }
}
