//! Channel-source tool catalog. These are tools the LLM advertises and
//! invokes, but their execution lives on the client side (Flutter app
//! today, future SPAs / adapters). When the tool_router sees one of
//! these names, it dispatches through the relay gateway's
//! `SendServerNotification` / `SendServerRequestAndAwait` RPCs instead
//! of an in-cluster controller. The client receives a
//! `ChannelOutbound::ServerRequest` frame and acts accordingly.

use proto_common::{text_block, CallToolResponse, ToolInfo};

use crate::clients::RelayRpc;

pub(crate) const REVEAL_PATH: &str = "RevealPath";
pub(crate) const REQUEST_USER_INPUT: &str = "RequestUserInput";
pub(crate) const REQUEST_USER_AUTH: &str = "RequestUserAuth";

/// Whether a tool waits for a client response (Request) or fires and
/// forgets (Notification).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Notification,
    Request,
}

pub(crate) fn kind_of(name: &str) -> Option<Kind> {
    match name {
        REVEAL_PATH => Some(Kind::Notification),
        REQUEST_USER_INPUT | REQUEST_USER_AUTH => Some(Kind::Request),
        _ => None,
    }
}

/// Definitions advertised to the LLM. Same shape as airlock/mainframe
/// tool catalogs, so the existing fan-in in `tool_router.rs` can
/// promote a channel snapshot to `Source::Channel` with no schema
/// translation.
pub(crate) fn tool_definitions() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
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
            name: REQUEST_USER_INPUT.to_string(),
            description: "Ask the user a question with a closed set of options (approval, choice, confirmation, input, clarification). Blocks until the user responds.".to_string(),
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
            name: REQUEST_USER_AUTH.to_string(),
            description: "Ask the user to complete an out-of-band auth flow at the given URL. Blocks until the callback fires or the user cancels.".to_string(),
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

/// Dispatch a channel-source tool call. The channel must be present on
/// the originating user message (`reply_channel`); without one we
/// surface a sane LLM-visible error rather than panic.
pub(crate) async fn dispatch(
    name: &str,
    input_json: &str,
    gateway: &mut dyn RelayRpc,
    reply_channel: Option<&str>,
    tool_call_id: &str,
) -> Result<CallToolResponse, String> {
    let Some(channel_id) = reply_channel else {
        return Ok(CallToolResponse {
            content: vec![text_block(format!(
                "channel tool `{name}` requires a connected client; this turn has no reply_channel"
            ))],
            is_error: true,
        });
    };
    let Some(kind) = kind_of(name) else {
        return Err(format!("unknown channel tool: {name}"));
    };
    match kind {
        Kind::Notification => {
            let delivered = gateway
                .send_server_notification(channel_id, name, input_json)
                .await?;
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
                .send_server_request_and_await(channel_id, tool_call_id, name, input_json, 30)
                .await?;
            Ok(outcome_to_tool_response(name, outcome))
        }
    }
}

/// Map a `SendServerRequestAndAwait` outcome to a `CallToolResponse` the
/// agent loop can fold into its next continuation.
pub(crate) fn outcome_to_tool_response(
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
            content: vec![text_block(format!(
                r#"{{"code":{code},"message":{message:?}}}"#
            ))],
            is_error: true,
        },
        ServerRequestOutcome::TimedOut => CallToolResponse {
            content: vec![text_block(format!(
                "client did not respond to `{method}` within the server-side timeout"
            ))],
            is_error: true,
        },
        ServerRequestOutcome::UnknownChannel => CallToolResponse {
            content: vec![text_block(
                "client channel is no longer registered".to_string(),
            )],
            is_error: true,
        },
        ServerRequestOutcome::UnsupportedMethod => CallToolResponse {
            content: vec![text_block(format!("client did not advertise `{method}`"))],
            is_error: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // channel_tools.rs:37 `tool_definitions` advertises exactly the three
    // channel tools the LLM can invoke (consts at channel_tools.rs:13-15).
    // Materiality: drop any one of the three `ToolInfo` entries from the
    // returned vec and its name goes missing -> the matching `contains` reds.
    #[test]
    fn tool_definitions_advertises_the_three_channel_tools() {
        let names: Vec<String> = tool_definitions().into_iter().map(|t| t.name).collect();
        assert!(
            names.contains(&REVEAL_PATH.to_string()),
            "RevealPath must be advertised, got {names:?}"
        );
        assert!(
            names.contains(&REQUEST_USER_INPUT.to_string()),
            "RequestUserInput must be advertised, got {names:?}"
        );
        assert!(
            names.contains(&REQUEST_USER_AUTH.to_string()),
            "RequestUserAuth must be advertised, got {names:?}"
        );
    }
}
