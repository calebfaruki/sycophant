use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pkm_controller::grpc::PkmServiceImpl;
use pkm_controller::state::PkmState;
use pkm_proto::pkm_service_client::PkmServiceClient;
use pkm_proto::pkm_service_server::PkmServiceServer;
use pkm_proto::{
    pkm_event, transponder_event, PkmEvent, ReportSystemTurn, RunSystemTurn, TransponderEvent,
    UserMessage,
};
use tightbeam_proto::{content_block, ContentBlock, TextBlock};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Server;

fn seed_prompts(pkm_dir: &Path) {
    let agents_dir = pkm_dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    for name in &["router", "research", "writer"] {
        let sub = agents_dir.join(name);
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("prompt.md"), format!("{name} prompt")).unwrap();
    }
}

async fn start_server() -> (String, tempfile::TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());

    let tmp = tempfile::TempDir::new().unwrap();
    seed_prompts(tmp.path());

    let state = Arc::new(PkmState::new(tmp.path()).await.unwrap());
    let service = PkmServiceImpl::new(state, None);

    tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        Server::builder()
            .add_service(PkmServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (url, tmp)
}

fn user_message(text: &str) -> TransponderEvent {
    TransponderEvent {
        event: Some(transponder_event::Event::UserMessage(UserMessage {
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
            }],
            sender: "alice".into(),
        })),
    }
}

fn report(response_json: &str) -> TransponderEvent {
    report_with(response_json, None)
}

fn report_with(response_json: &str, structured_json: Option<String>) -> TransponderEvent {
    TransponderEvent {
        event: Some(transponder_event::Event::ReportSystemTurn(
            ReportSystemTurn {
                response_json: response_json.into(),
                structured_json,
            },
        )),
    }
}

/// Drive a single ResolveTurn invocation and collect all server events.
async fn drive_resolve(
    url: &str,
    events: Vec<TransponderEvent>,
) -> Result<Vec<PkmEvent>, tonic::Status> {
    let mut client = PkmServiceClient::connect(url.to_string()).await.unwrap();

    let (tx, rx) = mpsc::channel(8);
    for evt in events {
        tx.send(evt).await.unwrap();
    }
    drop(tx);

    let response = client.resolve_turn(ReceiverStream::new(rx)).await?;
    let mut stream = response.into_inner();

    let mut out = Vec::new();
    while let Some(msg) = stream.message().await? {
        out.push(msg);
    }
    Ok(out)
}

fn run_system_turn(evt: &PkmEvent) -> Option<&RunSystemTurn> {
    match &evt.event {
        Some(pkm_event::Event::RunSystemTurn(rs)) => Some(rs),
        _ => None,
    }
}

fn run_agent_turn_name(evt: &PkmEvent) -> Option<&str> {
    match &evt.event {
        Some(pkm_event::Event::RunAgentTurn(ra)) => Some(&ra.agent_name),
        _ => None,
    }
}

fn resolve_error_code(evt: &PkmEvent) -> Option<i32> {
    match &evt.event {
        Some(pkm_event::Event::ResolveError(re)) => Some(re.code),
        _ => None,
    }
}

#[tokio::test]
async fn resolves_known_agent() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hello"), report("research")])
        .await
        .unwrap();

    assert_eq!(events.len(), 2);
    let rs = run_system_turn(&events[0]).expect("first event should be RunSystemTurn");
    assert_eq!(rs.system_prompt, "router prompt");
    assert_eq!(run_agent_turn_name(&events[1]), Some("research"));
}

#[tokio::test]
async fn falls_back_to_active_agent_on_unknown() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hello"), report("nonexistent")])
        .await
        .unwrap();

    // Default fallback is the alphabetically-first non-router agent
    assert_eq!(run_agent_turn_name(&events[1]), Some("research"));
}

#[tokio::test]
async fn falls_back_on_router_keyword() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hello"), report("router")])
        .await
        .unwrap();

    assert_eq!(run_agent_turn_name(&events[1]), Some("research"));
}

#[tokio::test]
async fn case_insensitive_match() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hello"), report("  Writer  ")])
        .await
        .unwrap();

    assert_eq!(run_agent_turn_name(&events[1]), Some("writer"));
}

#[tokio::test]
async fn invalid_first_event() {
    let (url, _tmp) = start_server().await;

    let result = drive_resolve(&url, vec![report("research")]).await;

    let err = result.expect_err("expected Status::invalid_argument");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn wrong_event_after_run_system_turn() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hello"), user_message("oops")])
        .await
        .unwrap();

    assert_eq!(events.len(), 2);
    assert!(run_system_turn(&events[0]).is_some());
    assert_eq!(resolve_error_code(&events[1]), Some(2));
}

#[tokio::test]
async fn active_agent_persists_across_invocations() {
    let (url, _tmp) = start_server().await;

    // First call picks "writer"
    let events = drive_resolve(&url, vec![user_message("hi"), report("writer")])
        .await
        .unwrap();
    assert_eq!(run_agent_turn_name(&events[1]), Some("writer"));

    // Second call gets unknown response → falls back to "writer" (active), not "research" (default)
    let events = drive_resolve(&url, vec![user_message("hi again"), report("zzznotreal")])
        .await
        .unwrap();
    assert_eq!(run_agent_turn_name(&events[1]), Some("writer"));
}

#[tokio::test]
async fn resolve_turn_emits_select_agent_schema() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hi"), report("research")])
        .await
        .unwrap();

    let rs = run_system_turn(&events[0]).expect("first event should be RunSystemTurn");
    let schema_str = rs
        .response_schema_json
        .as_deref()
        .expect("response_schema_json must be set");
    let schema: serde_json::Value = serde_json::from_str(schema_str).unwrap();
    assert_eq!(schema["type"], serde_json::json!("object"));
    assert_eq!(schema["additionalProperties"], serde_json::json!(false));
    let enum_values: Vec<&str> = schema["properties"]["agent_name"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(enum_values.contains(&"research"));
    assert!(enum_values.contains(&"writer"));
    assert!(!enum_values.contains(&"router"));
}

#[tokio::test]
async fn resolve_turn_uses_structured_json_when_set() {
    let (url, _tmp) = start_server().await;

    // structured_json carries "writer"; response_json says "research" (would-be conflict).
    // Structured wins.
    let events = drive_resolve(
        &url,
        vec![
            user_message("hi"),
            report_with("research", Some(r#"{"agent_name":"writer"}"#.into())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(run_agent_turn_name(&events[1]), Some("writer"));
}

#[tokio::test]
async fn resolve_turn_falls_back_to_free_text_when_structured_json_absent() {
    let (url, _tmp) = start_server().await;

    let events = drive_resolve(&url, vec![user_message("hi"), report_with("writer", None)])
        .await
        .unwrap();
    assert_eq!(run_agent_turn_name(&events[1]), Some("writer"));
}

#[tokio::test]
async fn resolve_turn_falls_back_when_structured_json_invalid() {
    let (url, _tmp) = start_server().await;

    // Malformed structured_json; response_json says "writer" — fallback uses it.
    let events = drive_resolve(
        &url,
        vec![
            user_message("hi"),
            report_with("writer", Some("not json{".into())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(run_agent_turn_name(&events[1]), Some("writer"));
}

#[tokio::test]
async fn resolve_turn_falls_back_when_structured_agent_unknown() {
    let (url, _tmp) = start_server().await;

    // structured_json names a non-existent agent; fallback to active (default = research).
    let events = drive_resolve(
        &url,
        vec![
            user_message("hi"),
            report_with("nobody", Some(r#"{"agent_name":"nobody"}"#.into())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(run_agent_turn_name(&events[1]), Some("research"));
}
