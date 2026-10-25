//! Loopback tests for Gemini Generative AI and Vertex wire adapters.

use std::time::Duration;

use nyan_coding_agent_core::{CancellationToken, Usage};
use nyan_coding_agent_provider::{ModelProvider, ProviderStopReason, ProviderStreamEvent};
use nyan_coding_agent_provider_http::{
    GoogleGenerativeAiProvider, GoogleVertexProvider, VertexAuth,
};
use serde_json::json;
use tokio::runtime::Runtime;

mod common;
use common::{Server, drain, request, serve};

fn sse(value: serde_json::Value) -> Vec<u8> {
    let text = value.to_string();
    drop(value);
    format!("data: {text}\n\n").into_bytes()
}

fn gemini_provider(runtime: &Runtime, server: &Server) -> GoogleGenerativeAiProvider {
    GoogleGenerativeAiProvider::new(
        "google",
        format!("http://{}/v1beta", server.address),
        Some("google-key".to_owned()),
        runtime.handle().clone(),
    )
    .expect("provider")
    .with_idle_timeout(Duration::from_secs(5))
}

#[test]
fn generative_ai_uses_key_query_contents_tools_and_usage() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                sse(json!({
                    "candidates": [{"content": {"parts": [{"text": "hello"}]}}]
                })),
            ),
            (
                0,
                sse(json!({
                    "candidates": [{"finishReason": "STOP", "content": {"parts": [{
                        "functionCall": {"name": "echo", "args": {"text": "x"}}
                    }]}}],
                    "usageMetadata": {"promptTokenCount": 12, "cachedContentTokenCount": 4,
                        "candidatesTokenCount": 3, "thoughtsTokenCount": 1, "totalTokenCount": 16}
                })),
            ),
        ],
        false,
    );
    let stream = runtime
        .block_on(gemini_provider(&runtime, &server).stream(request(CancellationToken::new())))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert!(events.contains(&ProviderStreamEvent::TextDelta {
        text: "hello".to_owned()
    }));
    assert_eq!(
        events.iter().find_map(|event| match event {
            ProviderStreamEvent::Usage(usage) => Some(*usage),
            _ => None,
        }),
        Some(Usage {
            input: 8,
            output: 4,
            cache_read: 4,
            cache_write: 0,
            total_tokens: 16
        })
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::ToolCall(_)))
    );
    assert_eq!(
        events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::ToolUse
        })
    );
    let captured = server.captured.lock().expect("lock").clone();
    assert!(
        captured
            .starts_with("POST /v1beta/models/test-model:streamGenerateContent?alt=sse HTTP/1.1"),
        "{captured}"
    );
    assert!(
        captured
            .to_ascii_lowercase()
            .contains("x-goog-api-key: google-key"),
        "{captured}"
    );
    assert!(
        !captured.lines().next().unwrap_or("").contains("google-key"),
        "the key must not appear in the URL"
    );
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "echo");
}

#[test]
fn vertex_uses_bearer_auth_and_publisher_path() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(
            0,
            sse(json!({
                "candidates": [{"finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
            })),
        )],
        false,
    );
    let provider = GoogleVertexProvider::new(
        "google-vertex",
        format!(
            "http://{}/v1/projects/p/locations/l/publishers/google",
            server.address
        ),
        Some(VertexAuth::Bearer("vertex-token".to_owned())),
        runtime.handle().clone(),
    )
    .expect("provider");
    let stream = runtime
        .block_on(provider.stream(request(CancellationToken::new())))
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("lock").clone();
    assert!(captured.starts_with("POST /v1/projects/p/locations/l/publishers/google/models/test-model:streamGenerateContent?alt=sse HTTP/1.1"), "{captured}");
    assert!(
        captured
            .to_ascii_lowercase()
            .contains("authorization: bearer vertex-token")
    );
}

#[test]
fn thought_parts_are_not_shown_and_history_replays_as_function_parts() {
    use nyan_coding_agent_core::{ContentBlock, Message, MessageRole, ToolCall, ToolCallId};
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(
            0,
            sse(
                json!({"candidates": [{"finishReason": "STOP", "content": {"parts": [
                    {"text": "private reasoning", "thought": true},
                    {"text": "answer"}
                ]}}]}),
            ),
        )],
        false,
    );
    let mut assistant = Message::text(MessageRole::Assistant, "calling");
    assistant.tool_calls.push(ToolCall {
        id: ToolCallId::new("c1").expect("id"),
        name: "echo".to_owned(),
        arguments: r#"{"text":"x"}"#.to_owned(),
    });
    let mut result = Message::tool_result(ToolCallId::new("c1").expect("id"), "echo", "x", false);
    result.text = r#"{"output":"x"}"#.to_owned();
    let mut user = Message::text(MessageRole::User, "go");
    user.content.push(ContentBlock::Image {
        data: "aGVsbG8=".to_owned(),
        mime_type: "image/png".to_owned(),
    });
    let mut req = request(CancellationToken::new());
    req.messages = vec![
        Message::text(MessageRole::System, "rules"),
        user,
        assistant,
        result,
    ];
    let stream = runtime
        .block_on(gemini_provider(&runtime, &server).stream(req))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert!(events.contains(&ProviderStreamEvent::TextDelta {
        text: "answer".to_owned()
    }));
    assert!(
        !events.iter().any(|event| matches!(event, ProviderStreamEvent::TextDelta { text } if text.contains("private"))),
        "thought text must not reach the user: {events:?}"
    );
    let captured = server.captured.lock().expect("lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "rules");
    assert_eq!(
        body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(body["contents"][1]["role"], "model");
    assert_eq!(
        body["contents"][1]["parts"][1]["functionCall"]["name"],
        "echo"
    );
    assert_eq!(
        body["contents"][1]["parts"][1]["functionCall"]["args"]["text"],
        "x"
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "echo"
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["response"]["output"],
        "x"
    );
}

#[test]
fn vertex_express_mode_sends_the_api_key_header_and_no_bearer() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(0, sse(json!({"candidates": [{"finishReason": "STOP"}]})))],
        false,
    );
    let provider = GoogleVertexProvider::new(
        "google-vertex",
        format!("http://{}/v1/publishers/google", server.address),
        Some(VertexAuth::ApiKey("express-key".to_owned())),
        runtime.handle().clone(),
    )
    .expect("provider");
    let stream = runtime
        .block_on(provider.stream(request(CancellationToken::new())))
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("lock").clone();
    let lower = captured.to_ascii_lowercase();
    assert!(
        captured.starts_with(
            "POST /v1/publishers/google/models/test-model:streamGenerateContent?alt=sse HTTP/1.1"
        ),
        "{captured}"
    );
    assert!(lower.contains("x-goog-api-key: express-key"), "{captured}");
    assert!(!lower.contains("authorization:"), "{captured}");
}
