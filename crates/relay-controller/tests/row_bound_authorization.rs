//! Every request is bound to the grant row that signed it, checked against
//! the live grants table, and a conversation belongs to exactly one row.
//!
//! `ListWorkspaces` reads the row-bound store.
//!
//! The contract these tests pin:
//!
//! ```ignore
//! // relay_controller::signature_layer — one extension replaces the two
//! pub struct VerifiedRow(pub String);   // the grant row key, == the signing kid
//!
//! // relay_controller::gateway — the pure conversation-ownership decision
//! pub enum RowAccess { Allow, Deny, ResolveWithHarness }
//! pub fn conversation_access(cached_owner: Option<&str>, verified_row: &str) -> RowAccess;
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use tonic::Request;

use proto_common::{CallToolRequest, GetTurnStateRequest, ListWorkspacesRequest};
use relay_controller::gateway::{conversation_access, GatewayService, RowAccess};
use relay_controller::grants::{apply_delivery, RelayGrants};
use relay_controller::signature_layer::VerifiedRow;
use relay_controller::state::GatewayState;
use relay_proto::relay_gateway_server::RelayGateway;
use shared::client_signature::ClientSignatureVerifier;

const NAMESPACE: &str = "tenant";

fn grants_table(rows: &[(&str, &str, &str, &str)]) -> RelayGrants {
    let data = rows
        .iter()
        .map(|(key, channel, identity, workspace)| {
            (
                (*key).to_string(),
                format!("channel: {channel}\nidentity: {identity}\nworkspace: {workspace}\n"),
            )
        })
        .collect::<BTreeMap<String, String>>();
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some("relay-grants".into()),
            namespace: Some(NAMESPACE.into()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };
    let (table, errors) = apply_delivery(&cm);
    assert!(errors.is_empty(), "fixture grants must all parse");
    table
}

async fn service_with(table: RelayGrants) -> GatewayService {
    let state = Arc::new(GatewayState::new(
        Arc::new(ClientSignatureVerifier::new(Duration::from_secs(300))),
        None,
        NAMESPACE.into(),
    ));
    *state.grants().write().await = table;
    GatewayService::new(state)
}

fn signed_by<T>(message: T, row: &str) -> Request<T> {
    let mut req = Request::new(message);
    req.extensions_mut().insert(VerifiedRow(row.to_string()));
    req
}

/// Two rows, same channel, same workspace — the shape the household actually
/// has. `caleb-phone` and `caleb-laptop` both name `family`.
fn family_grants() -> RelayGrants {
    grants_table(&[
        ("caleb-phone", "app", "kJ8f2QwXnR4tYv6b", "family"),
        ("caleb-laptop", "app", "pQ3z7NmBc1dLe5wR", "family"),
    ])
}

// --- Step 24: ListWorkspaces on the row-bound store ------------------------

/// Step 24. `ListWorkspaces` is the RPC the Flutter client fires the instant
/// redemption succeeds (`client/lib/main.dart:471-475`). A grant row names
/// exactly one workspace, so the answer is a one-element list.
///
/// Materiality: step 21 replaces the `kid`-keyed registration store and step 28
/// deletes `get_workspaces_for_kid`'s owner, so this lookup is rewired blind.
/// Get it wrong and enrollment appears to succeed while the client lands on an
/// empty workspace picker — every other stage-D test stays green, and the three
/// existing allow-list tests at `signature_layer.rs:501-523` prove only that the
/// method is classified workspace-free, never that its lookup still resolves.
#[tokio::test]
async fn list_workspaces_returns_the_signing_rows_workspace() {
    let service = service_with(family_grants()).await;
    let resp = service
        .list_workspaces(signed_by(ListWorkspacesRequest {}, "caleb-phone"))
        .await
        .expect("a live row must resolve its workspace")
        .into_inner();
    assert_eq!(resp.workspaces, vec!["family".to_string()]);
}

/// Revocation on the RPC the client hits first after enrolling. The operator
/// deleted the row; the device still holds a perfectly valid key.
///
/// Answer from the registered-key store alone and revocation stops working the
/// moment a key is already registered, which is every case that matters. Row
/// removal is the only revocation mechanism there is.
#[tokio::test]
async fn list_workspaces_refuses_a_row_absent_from_the_live_grants_table() {
    let service = service_with(family_grants()).await;
    let err = service
        .list_workspaces(signed_by(ListWorkspacesRequest {}, "dad-telegram"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// --- Revocation on the conversational surface ------------------------------

/// Revocation on a conversational RPC. `GetTurnState` answers `IDLE` for any
/// conversation it has not heard of, so without a row check a revoked row gets
/// a well-formed `Ok` here.
///
/// Check the row only where a workspace was previously checked and this path
/// keeps answering `Ok(IDLE)` after revocation, a poll loop that
/// never notices it has been cut off. The per-request check has to run on every
/// RPC on the surface, not only the ones that forward.
#[tokio::test]
async fn a_conversational_rpc_from_a_removed_row_is_refused() {
    let service = service_with(family_grants()).await;
    let err = service
        .get_turn_state(signed_by(
            GetTurnStateRequest {
                conversation_id: "family.conv-1".into(),
            },
            "dad-telegram",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

/// Revocation on the tool-dispatch path. The row check runs before the
/// forward, not after it.
///
/// Leave the row check out of this path and a revoked row can still dispatch
/// `Shell` into its old workspace's tool Job for as long as the harness
/// answers. The timeout is the assertion that no dial happened.
#[tokio::test]
async fn dispatch_tool_from_a_removed_row_is_refused_without_dialing_the_harness() {
    let service = service_with(family_grants()).await;
    let call = service.dispatch_tool(signed_by(
        CallToolRequest {
            name: "Shell".into(),
            input_json: "{}".into(),
            conversation_id: "family.conv-1".into(),
        },
        "dad-telegram",
    ));
    let err = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .expect("a removed row must be refused before the harness is dialed")
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

// --- Conversations bind to the row, not the workspace ----------------------

/// Stated at the seam that decides it. The two rows here are the same channel
/// and the same workspace, the case that a workspace comparison cannot tell
/// apart.
///
/// The decision function does not take a workspace at all. That absence is the
/// property: a workspace parameter is what would let two callers in one
/// workspace reach each other's conversations.
///
/// Compare workspaces instead of rows and this reds. Return `Allow` for a
/// mismatched owner and it reds.
#[test]
fn a_conversation_owned_by_another_row_is_denied() {
    assert_eq!(
        conversation_access(Some("caleb-phone"), "caleb-phone"),
        RowAccess::Allow
    );
    assert_eq!(
        conversation_access(Some("caleb-phone"), "caleb-laptop"),
        RowAccess::Deny
    );
}

/// The relay's conversation-to-row map is a *cache*,
/// rebuilt from the harness on restart. A miss must send the relay to the
/// harness for an answer; it must never be read as "no owner recorded,
/// therefore fine".
///
/// `Option::unwrap_or(verified_row)`, or any `is_none() => allow`
/// shortcut, makes the cold cache permissive, and every warm-path test above
/// still passes. A relay restart then briefly opens every conversation in the
/// tenant to every row. This is the test that earns the caching decision.
#[test]
fn a_cold_cache_never_allows() {
    assert_eq!(
        conversation_access(None, "caleb-laptop"),
        RowAccess::ResolveWithHarness
    );
    assert_ne!(conversation_access(None, "caleb-laptop"), RowAccess::Allow);
}
