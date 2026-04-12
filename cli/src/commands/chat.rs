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
    let payload = format!(
        "{{\"register\":{{\"channel_type\":\"cli\",\"channel_name\":\"{}\"}}}}\n\
         {{\"user_message\":{{\"content\":[{{\"text\":{{\"text\":\"{}\"}}}}],\"sender\":\"cli\"}}}}",
        cmd.workspace, escaped,
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
