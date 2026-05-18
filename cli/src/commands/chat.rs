use std::io::{self, IsTerminal, Read};

use crate::cli::ChatCmd;
use crate::grpc::grpc_call;
use crate::scope::Scope;

pub(crate) fn run(scope: &Scope, cmd: ChatCmd) -> Result<(), String> {
    if io::stdin().is_terminal() {
        return Err("Message must be provided via stdin.\n  \
             echo \"Hello\" | syco chat <workspace>\n  \
             syco chat <workspace> <<'EOF'\n  \
             Multi-line message\n  \
             EOF"
        .into());
    }

    let mut message = String::new();
    io::stdin()
        .read_to_string(&mut message)
        .map_err(|e| format!("failed to read stdin: {e}"))?;

    let message = message.trim();
    if message.is_empty() {
        return Err("stdin was empty, no message provided".into());
    }

    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    // ChannelRegister carries adapter_hint (free-form, log-only) + workspace.
    // The server mints a channel_id, returned as the first ChannelOutbound
    // frame (ack); subsequent UserMessage frames have their reply_channel
    // stamped server-side using that id. The CLI doesn't echo the id back
    // anywhere — fire-and-forget single-message send.
    let payload = format!(
        "{{\"register\":{{\"adapter_hint\":\"cli:{ws}\",\"workspace\":\"{ws}\"}}}}\n\
         {{\"user_message\":{{\"content\":[{{\"text\":{{\"text\":\"{}\"}}}}],\"sender\":\"cli\"}}}}",
        escaped,
        ws = cmd.workspace,
    );

    let response = grpc_call(
        scope,
        "tightbeam-controller",
        9090,
        "tightbeam.v1.TightbeamController/ChannelStream",
        &payload,
    )?;

    println!("{response}");
    Ok(())
}
