//! Channel-stream tool catalog. These are tools the LLM advertises and
//! invokes, but their execution lives on the client side. When the
//! tool_router sees one of these names, it dispatches through the relay
//! gateway's `SendServerNotification` / `SendServerRequestAndAwait` RPCs.
//! The client receives a
//! `ChannelStreamOutbound::ServerRequest` frame and acts accordingly.

use proto_common::{text_block, CallToolResponse, ToolInfo};

use crate::clients::RelayRpc;

const REVEAL_PATH: &str = "RevealPath";
const REQUEST_USER_INPUT: &str = "RequestUserInput";
const REQUEST_USER_AUTH: &str = "RequestUserAuth";
const REQUEST_TIMEOUT_SECONDS: u32 = 30;

/// Whether a tool waits for a client response (Request) or fires and
/// forgets (Notification).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Notification,
    Request,
}

fn kind_of(name: &str) -> Option<Kind> {
    match name {
        REVEAL_PATH => Some(Kind::Notification),
        REQUEST_USER_INPUT | REQUEST_USER_AUTH => Some(Kind::Request),
        _ => None,
    }
}

/// Definitions advertised to the LLM.
pub(crate) fn tool_definitions() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            toolset: String::new(),
            name: REVEAL_PATH.to_string(),
            description: "Open the workspace browser pane at the given path on the user's device. Fire-and-forget — does not wait for the user.".to_string(),
            parameters_json: r#"{
  "type": "object",
  "required": ["path"],
  "properties": {
    "path": { "type": "string", "description": "Absolute path within the workspace (e.g., \"/target-files/clients/acme\")." }
  }
}"#.to_string(),
        },
        ToolInfo {
            toolset: String::new(),
            name: REQUEST_USER_INPUT.to_string(),
            description: format!("Ask the user a question with a closed set of options (approval, choice, confirmation, input, clarification). Waits up to {REQUEST_TIMEOUT_SECONDS} seconds for the user's answer."),
            parameters_json: r#"{
  "type": "object",
  "required": ["prompt", "actions"],
  "properties": {
    "prompt": { "type": "string" },
    "actions": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["id", "label"],
        "properties": {
          "id": { "type": "string" },
          "label": { "type": "string" }
        }
      }
    }
  }
}"#.to_string(),
        },
        ToolInfo {
            toolset: String::new(),
            name: REQUEST_USER_AUTH.to_string(),
            description: format!("Ask the user to complete an out-of-band auth flow at the given URL. Waits up to {REQUEST_TIMEOUT_SECONDS} seconds for the callback or the user's cancel."),
            parameters_json: r#"{
  "type": "object",
  "required": ["prompt", "url"],
  "properties": {
    "prompt": { "type": "string" },
    "url": { "type": "string" },
    "expires_at": { "type": "string", "description": "Optional RFC 3339 expiry." }
  }
}"#.to_string(),
        },
    ]
}

/// Dispatch a channel-stream tool call. With no connected client the call
/// returns an error result to the model.
pub(crate) async fn dispatch(
    name: &str,
    input_json: &str,
    gateway: &mut dyn RelayRpc,
    has_client: bool,
    tool_call_id: &str,
) -> Result<CallToolResponse, String> {
    if !has_client {
        return Ok(CallToolResponse {
            content: vec![text_block(format!(
                "channel-stream tool `{name}` requires a connected client; none is connected"
            ))],
            is_error: true,
        });
    }
    let Some(kind) = kind_of(name) else {
        return Err(format!("unknown channel-stream tool: {name}"));
    };
    match kind {
        Kind::Notification => {
            let delivered = gateway.send_server_notification(name, input_json).await?;
            if delivered {
                Ok(CallToolResponse {
                    content: vec![text_block(format!(
                        r#"{{"dispatched":true,"method":"{name}"}}"#
                    ))],
                    is_error: false,
                })
            } else {
                Ok(CallToolResponse {
                    content: vec![text_block(format!(
                        "client did not advertise `{name}`; the notification was not delivered"
                    ))],
                    is_error: true,
                })
            }
        }
        Kind::Request => {
            let outcome = gateway
                .send_server_request_and_await(
                    tool_call_id,
                    name,
                    input_json,
                    REQUEST_TIMEOUT_SECONDS,
                )
                .await?;
            Ok(outcome_to_tool_response(name, outcome))
        }
    }
}

/// Map a `SendServerRequestAndAwait` outcome to a `CallToolResponse` the
/// agent loop can fold into its next continuation.
fn outcome_to_tool_response(
    method: &str,
    outcome: crate::clients::ServerRequestOutcome,
) -> CallToolResponse {
    use crate::clients::ServerRequestOutcome;
    match outcome {
        ServerRequestOutcome::Result(json) => CallToolResponse {
            content: vec![text_block(json)],
            is_error: false,
        },
        ServerRequestOutcome::Error { code, message } => CallToolResponse {
            content: vec![text_block(
                serde_json::json!({"code": code, "message": message}).to_string(),
            )],
            is_error: true,
        },
        ServerRequestOutcome::TimedOut => CallToolResponse {
            content: vec![text_block(format!(
                "client did not respond to `{method}` within the server-side timeout"
            ))],
            is_error: true,
        },
        ServerRequestOutcome::UnknownChannelStream => CallToolResponse {
            content: vec![text_block(
                "client channel stream is no longer registered".to_string(),
            )],
            is_error: true,
        },
        ServerRequestOutcome::UnsupportedMethod => CallToolResponse {
            content: vec![text_block(format!("client did not advertise `{method}`"))],
            is_error: true,
        },
    }
}
