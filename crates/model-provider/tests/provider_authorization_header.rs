//! What each provider dialect puts on the wire for authorization.
//!
//! A profile pointing at an unauthenticated destination carries no credential,
//! so the client is handed an empty key. Sending the header with an empty value
//! is not "without an API key": it is a malformed credential that a gateway in
//! front of the destination may reject outright.
//!
//! The request head is read off a real socket rather than a mock, because the
//! header is attached at the call site and nothing below it is observable.

use std::net::SocketAddr;

use model_provider::claude::ClaudeProvider;
use model_provider::openai::OpenAiProvider;
use model_provider::{LlmProvider, ProviderConfig};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Read one request head, answer with an empty 200, hand the head back.
async fn serve_one(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte).await {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    let _ = stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 0\r\n\r\n",
        )
        .await;
    let _ = stream.flush().await;
    String::from_utf8_lossy(&head).to_string()
}

/// Issue one call against a throwaway listener and return the request head the
/// provider actually sent.
async fn request_head_for(
    build: impl FnOnce(String) -> Box<dyn LlmProvider>,
    api_key: &str,
) -> String {
    let listener = TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
        .await
        .expect("bind a throwaway listener");
    let port = listener.local_addr().expect("local addr").port();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept one connection");
        serve_one(&mut stream).await
    });

    let provider = build(format!("http://127.0.0.1:{port}/v1"));
    let config = ProviderConfig {
        model: "liquid/lfm2.5-8b-a1b".to_string(),
        api_key: api_key.to_string(),
    };
    let _ = provider.call(&[], None, &[], None, &config).await;

    server.await.expect("the listener task completes")
}

fn openai(base_url: String) -> Box<dyn LlmProvider> {
    Box::new(OpenAiProvider::new(base_url))
}

fn claude(base_url: String) -> Box<dyn LlmProvider> {
    Box::new(ClaudeProvider::new(base_url))
}

fn header_lines(head: &str, name: &str) -> Vec<String> {
    head.lines()
        .filter(|line| {
            line.split(':')
                .next()
                .is_some_and(|k| k.trim().eq_ignore_ascii_case(name))
        })
        .map(str::to_string)
        .collect()
}

// ---- OpenAI dialect: Authorization: Bearer ----

/// Breaks if the authorization header is attached unconditionally, which is
/// what sends a bearer token with no token in it.
#[tokio::test]
async fn an_empty_api_key_sends_no_authorization_header() {
    let head = request_head_for(openai, "").await;
    let found = header_lines(&head, "authorization");
    assert!(
        found.is_empty(),
        "an empty key means no credential to present, got: {found:?}\n---\n{head}"
    );
    // The discriminator against a request that never left: the call was made.
    assert!(
        head.starts_with("POST /v1/chat/completions "),
        "the completion request must still be issued, got head:\n{head}"
    );
}

/// Breaks if the header is dropped for every request rather than only for the
/// keyless one, which silently unauthenticates every external provider.
#[tokio::test]
async fn a_present_api_key_still_sends_a_bearer_authorization_header() {
    let head = request_head_for(openai, "sk-live-key").await;
    let found = header_lines(&head, "authorization");
    assert_eq!(
        found.len(),
        1,
        "a declared key is presented exactly once, got: {found:?}\n---\n{head}"
    );
    assert!(
        found[0].ends_with("Bearer sk-live-key"),
        "the key is presented as a bearer token, got: {}",
        found[0]
    );
}

// ---- Anthropic dialect: x-api-key ----

/// Breaks if `x-api-key` is attached unconditionally, which sends an empty
/// credential rather than none.
#[tokio::test]
async fn an_empty_api_key_sends_no_x_api_key_header() {
    let head = request_head_for(claude, "").await;
    let found = header_lines(&head, "x-api-key");
    assert!(
        found.is_empty(),
        "an empty key means no credential to present, got: {found:?}\n---\n{head}"
    );
    // The discriminator against a request that never left: the call was made,
    // and the version header the dialect always carries is still on it.
    assert!(
        head.starts_with("POST /v1/messages "),
        "the completion request must still be issued, got head:\n{head}"
    );
    assert_eq!(
        header_lines(&head, "anthropic-version").len(),
        1,
        "dropping the credential must not drop the dialect's other headers"
    );
}

/// Breaks if the header is dropped for every request rather than only for the
/// keyless one, which silently unauthenticates every external provider.
#[tokio::test]
async fn a_present_api_key_still_sends_an_x_api_key_header() {
    let head = request_head_for(claude, "sk-ant-live-key").await;
    let found = header_lines(&head, "x-api-key");
    assert_eq!(
        found.len(),
        1,
        "a declared key is presented exactly once, got: {found:?}\n---\n{head}"
    );
    assert!(
        found[0].ends_with("sk-ant-live-key"),
        "the key is presented verbatim, got: {}",
        found[0]
    );
}
