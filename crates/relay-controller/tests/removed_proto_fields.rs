//! The message-number, catch-up and event-log-replay protocol is gone from
//! the proto sources: every field it used is reserved by number (and by name
//! where one is listed), so none can be reused, and nothing defines one of
//! its names again.

use std::path::PathBuf;

/// (proto source relative to `crates/`, message, field number, reserved field
/// name if the message reserves one)
const REMOVED_FIELDS: &[(&str, &str, u32, Option<&str>)] = &[
    (COMMON, "ChannelStreamOutbound", 6, None),
    (
        COMMON,
        "ChannelStreamOutbound",
        8,
        Some("catch_up_complete"),
    ),
    (
        COMMON,
        "ChannelStreamOutbound",
        9,
        Some("replay_impossible"),
    ),
    (COMMON, "Heartbeat", 1, None),
    (COMMON, "StreamItem", 1, None),
    (
        COMMON,
        "ChannelStreamReceiveRequest",
        4,
        Some("resume_after_seq"),
    ),
    (COMMON, "SendServerNotificationRequest", 5, None),
    (COMMON, "SendServerRequestAndAwaitRequest", 7, None),
    (RELAY, "DeliverStreamItemRequest", 3, None),
    (RELAY, "DeliverReplyRequest", 3, None),
    (RELAY, "DeliverTurnStateRequest", 3, None),
];

/// Names no field, message or RPC may carry in any proto source.
const REMOVED_NAMES: &[&str] = &[
    "resume_after_seq",
    "CatchUpComplete",
    "ReplayImpossible",
    "ReplayEventLog",
];

const COMMON: &str = "proto-common/proto/sycophant/common/v1/common.proto";
const RELAY: &str = "relay-proto/proto/relay/v1/relay.proto";
const HARNESS: &str = "harness-proto/proto/harness/v1/harness.proto";

fn read_proto(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(rel);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    strip_comments(&raw)
}

/// Drops `//` line comments and `/* */` block comments, which may mention a
/// removed name without defining it.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Body of `message <name> { ... }`, braces balanced, nested blocks included.
fn message_body<'a>(src: &'a str, name: &str) -> &'a str {
    let is_header_at = |i: usize| {
        let before_ok = src[..i]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == '{' || c == '}');
        let rest = src[i + "message".len()..].trim_start();
        let after_ok = rest
            .strip_prefix(name)
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_whitespace() || c == '{');
        before_ok && after_ok
    };
    let start = src
        .match_indices("message")
        .map(|(i, _)| i)
        .find(|&i| is_header_at(i))
        .unwrap_or_else(|| panic!("message {name} not found"));
    let open = start + src[start..].find('{').expect("message has a body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open + 1..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("message {name} has unbalanced braces");
}

/// The numbers and names a message body reserves, from every `reserved`
/// statement (`reserved 1, 4 to 6;` and `reserved "a", "b";`).
fn reserved_in(body: &str) -> (Vec<u32>, Vec<String>) {
    let mut numbers = Vec::new();
    let mut names = Vec::new();
    for stmt in body.split([';', '{', '}']) {
        let stmt = stmt.trim();
        let Some(list) = stmt.strip_prefix("reserved") else {
            continue;
        };
        for item in list.split(',') {
            let item = item.trim();
            if let Some(name) = item.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                names.push(name.to_string());
            } else if let Some((lo, hi)) = item.split_once(" to ") {
                let lo: u32 = lo.trim().parse().unwrap();
                let hi: u32 = hi.trim().parse().unwrap();
                numbers.extend(lo..=hi);
            } else if let Ok(n) = item.parse() {
                numbers.push(n);
            }
        }
    }
    (numbers, names)
}

/// Identifiers outside string literals, so a reserved `"resume_after_seq"`
/// does not count as a definition.
fn identifiers(src: &str) -> Vec<&str> {
    src.split('"')
        .step_by(2)
        .flat_map(|outside| outside.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')))
        .filter(|t| !t.is_empty())
        .collect()
}

#[test]
fn proto_sources_reserve_every_removed_field_number_and_listed_name_and_define_no_removed_name() {
    for (file, message, number, name) in REMOVED_FIELDS {
        let src = read_proto(file);
        let (numbers, names) = reserved_in(message_body(&src, message));
        assert!(
            numbers.contains(number),
            "{file}: {message} does not reserve field number {number}"
        );
        if let Some(name) = name {
            assert!(
                names.iter().any(|n| n == name),
                "{file}: {message} does not reserve field name \"{name}\""
            );
        }
    }

    for file in [COMMON, RELAY, HARNESS] {
        let src = read_proto(file);
        let idents = identifiers(&src);
        for removed in REMOVED_NAMES {
            assert!(
                !idents.contains(removed),
                "{file} still defines or references `{removed}` outside a reserved name"
            );
        }
    }
}

const TOOLSET: &str = "toolset-proto/proto/toolset/v1/toolset.proto";

/// (proto source, message, field type, field name, field number): the channel
/// stream wire names, which keep the field numbers their old names held.
const CHANNEL_STREAM_FIELDS: &[(&str, &str, &str, &str, &str)] = &[
    (
        RELAY,
        "DeliverStreamItemRequest",
        "string",
        "channel_stream_id",
        "1",
    ),
    (
        RELAY,
        "DeliverTurnStateRequest",
        "string",
        "channel_stream_id",
        "1",
    ),
    (
        RELAY,
        "DeliverOutboundRequest",
        "string",
        "channel_stream_id",
        "1",
    ),
    (
        COMMON,
        "ChannelStreamAck",
        "string",
        "channel_stream_id",
        "1",
    ),
    (
        COMMON,
        "ChannelStreamIngestRequest",
        "string",
        "channel_stream_id",
        "1",
    ),
    (
        COMMON,
        "ChannelStreamIngestAck",
        "string",
        "channel_stream_id",
        "1",
    ),
    (
        COMMON,
        "ChannelStreamOutbound",
        "ChannelStreamAck",
        "ack",
        "1",
    ),
    (
        COMMON,
        "ChannelStreamOutbound",
        "ChannelStreamSend",
        "send_message",
        "2",
    ),
    (COMMON, "UserMessage", "string", "reply_channel_stream", "3"),
    (
        COMMON,
        "SendServerRequestAndAwaitResponse",
        "bool",
        "unknown_channel_stream",
        "4",
    ),
    (
        TOOLSET,
        "TurnRequest",
        "string",
        "reply_channel_stream",
        "6",
    ),
];

/// The relay RPCs, written without the `sycophant.common.v1.` package prefix.
const CHANNEL_STREAM_RPCS: &[&str] = &[
    "rpc ChannelStreamIngest (ChannelStreamIngestRequest) returns (ChannelStreamIngestAck)",
    "rpc ChannelStreamReceive (ChannelStreamReceiveRequest) returns (stream ChannelStreamOutbound)",
];

/// Whether `body` declares `<ty> <name> = <number>` as one of its fields
/// (an `optional`/`repeated` label and field options are allowed).
fn declares_field(body: &str, ty: &str, name: &str, number: &str) -> bool {
    body.split([';', '{', '}']).any(|stmt| {
        let stmt = stmt.split('[').next().unwrap_or("");
        let tokens: Vec<&str> = stmt.split_whitespace().collect();
        let n = tokens.len();
        n >= 4
            && tokens[n - 4] == ty
            && tokens[n - 3] == name
            && tokens[n - 2] == "="
            && tokens[n - 1] == number
    })
}

#[test]
fn proto_sources_declare_the_channel_stream_wire_names_with_their_field_numbers() {
    for (file, message, ty, name, number) in CHANNEL_STREAM_FIELDS {
        let src = read_proto(file);
        assert!(
            declares_field(message_body(&src, message), ty, name, number),
            "{file}: {message} does not declare `{ty} {name} = {number}`"
        );
    }

    let relay = read_proto(RELAY)
        .replace("sycophant.common.v1.", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for rpc in CHANNEL_STREAM_RPCS {
        assert!(relay.contains(rpc), "{RELAY} does not declare `{rpc}`");
    }
}
