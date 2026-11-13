//! Puts a model tool call to the user through the relay before it runs.
//!
//! The call runs only on an explicit `{"approved": true}`. Every other answer,
//! and a missing relay, refuses it.

use proto_common::{text_block, CallToolResponse};
use tokio_util::sync::CancellationToken;

use crate::clients::{RelayRpc, ServerRequestOutcome};
use crate::runtime_tools::DispatchAbort;

pub(crate) const METHOD: &str = "RequestToolApproval";

const TIMEOUT_SECONDS: u32 = 300;

const DENIED: &str = "The user denied this tool call. Do not retry it. Wait for the user.";

const NO_ANSWER: &str = "The user did not answer in time. This tool call was denied. \
                         Do not retry it. Wait for the user.";

/// The user's answer to one approval request.
pub(crate) enum Approval {
    Approved,
    /// The call must not run; this is its tool result.
    Refused(CallToolResponse),
}

/// Ask the user to approve calling `tool` with `input_json`, waiting until the
/// user answers, the relay gives up, or `cancel` fires.
pub(crate) async fn ask(
    relay: Option<&mut dyn RelayRpc>,
    tool: &str,
    input_json: &str,
    cancel: &CancellationToken,
) -> Result<Approval, DispatchAbort> {
    let Some(relay) = relay else {
        return Ok(refused(unadvertised()));
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let params_json = serde_json::json!({ "tool": tool, "input": input_json }).to_string();

    let outcome = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(DispatchAbort::Cancelled),
        outcome = relay.send_server_request_and_await(
            &request_id,
            METHOD,
            &params_json,
            TIMEOUT_SECONDS,
        ) => outcome.map_err(DispatchAbort::Error)?,
    };

    Ok(match outcome {
        ServerRequestOutcome::Result(json) if is_approval(&json) => Approval::Approved,
        ServerRequestOutcome::Result(_) | ServerRequestOutcome::Error { .. } => {
            refused(DENIED.to_string())
        }
        ServerRequestOutcome::TimedOut => refused(NO_ANSWER.to_string()),
        ServerRequestOutcome::UnsupportedMethod | ServerRequestOutcome::UnknownChannelStream => {
            refused(unadvertised())
        }
    })
}

/// Only a JSON object whose `approved` is the boolean `true` approves.
fn is_approval(result_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(result_json)
        .ok()
        .and_then(|v| v.get("approved").and_then(serde_json::Value::as_bool))
        == Some(true)
}

fn unadvertised() -> String {
    format!("client did not advertise `{METHOD}`")
}

fn refused(text: String) -> Approval {
    Approval::Refused(CallToolResponse {
        content: vec![text_block(text)],
        is_error: true,
    })
}
