//! Loopback tests for the Mistral Conversations adapter.

use std::time::Duration;

use nyan_coding_agent_core::{
    CancellationToken, Message, MessageRole, ToolCall, ToolCallId, Usage,
};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderStopReason, ProviderStreamEvent, ToolCallRequest,
};
use nyan_coding_agent_provider_http::MistralConversationsProvider;
use serde_json::json;
use tokio::runtime::Runtime;

mod common;
use common::{Server, drain, request, serve};

fn sse(value: serde_json::Value) -> Vec<u8> {
    let body = value.to_string();
    drop(value);
    format!("data: {body}\n\n").into_bytes()
}

fn provider(runtime: &Runtime, server: &Server) -> MistralConversationsProvider {
    MistralConversationsProvider::new(
        "mistral",
        format!("http://{}", server.address),
        Some("mistral-key".to_owned()),
        runtime.handle().clone(),
    )
    .expect("provider")
    .with_idle_timeout(Duration::from_secs(5))
}

#[test]
fn request_uses_mistral_path_snake_case_and_streams_text_usage() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (0, sse(json!({"choices":[{"delta":{"content":"hi"}}]}))),
            (
                0,
                sse(json!({"choices":[{"delta":{"content":"!"},"finish_reason":"stop"}]})),
            ),
            (
                0,
                sse(
                    json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,
                "prompt_tokens_details":{"cached_tokens":4}}}),
                ),
            ),
            (0, sse(json!("[DONE]"))),
        ],
        false,
    );
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(request(CancellationToken::new())))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert_eq!(
        events,
        [
            ProviderStreamEvent::TextDelta {
                text: "hi".to_owned()
            },
            ProviderStreamEvent::TextDelta {
                text: "!".to_owned()
            },
            ProviderStreamEvent::Usage(Usage {
                input: 8,
                output: 3,
                cache_read: 4,
                cache_write: 0,
                total_tokens: 15,
            }),
            ProviderStreamEvent::Finished {
                reason: ProviderStopReason::Stop
            },
        ]
    );
    let captured = server.captured.lock().expect("lock").clone();
    let lower = captured.to_ascii_lowercase();
    assert!(
        captured.starts_with("POST /v1/chat/completions HTTP/1.1"),
        "{captured}"
    );
    assert!(
        lower.contains("authorization: bearer mistral-key"),
        "{captured}"
    );
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
}

#[test]
fn tool_calls_are_assembled_and_ids_are_normalized_for_replay() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                sse(json!({"choices":[{"delta":{"tool_calls":[
                    {"index":0,"id":"foreign/id","function":{"name":"echo","arguments":"{\"x"}}
                ]}}]})),
            ),
            (
                0,
                sse(json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"\":1}"}}
            ]},"finish_reason":"tool_calls"}]})),
            ),
            (0, sse(json!("[DONE]"))),
        ],
        false,
    );
    let mut assistant = Message::text(MessageRole::Assistant, "");
    assistant.tool_calls.push(ToolCall {
        id: ToolCallId::new("foreign/id").expect("id"),
        name: "echo".to_owned(),
        arguments: "{\"x\":1}".to_owned(),
    });
    let mut replay_request = request(CancellationToken::new());
    replay_request.messages = vec![assistant];
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(replay_request))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert!(
        events.contains(&ProviderStreamEvent::ToolCall(ToolCallRequest {
            id: "foreign/id".to_owned(),
            name: "echo".to_owned(),
            arguments: "{\"x\":1}".to_owned(),
        }))
    );
    assert_eq!(
        events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::ToolUse
        })
    );
    let captured = server.captured.lock().expect("lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    let id = body["messages"][0]["tool_calls"][0]["id"]
        .as_str()
        .expect("id");
    assert_eq!(id.len(), 9);
    assert!(
        id.chars()
            .all(|character| character.is_ascii_alphanumeric())
    );
}

#[test]
fn wrapped_events_and_http_errors_are_supported() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(
            0,
            sse(json!({"data":{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}})),
        )],
        false,
    );
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(request(CancellationToken::new())))
        .expect("stream");
    assert!(drain(&runtime, stream).is_ok());

    let denied = serve(
        &runtime,
        "403 Forbidden",
        vec![(0, b"denied".to_vec())],
        false,
    );
    let Err(error) =
        runtime.block_on(provider(&runtime, &denied).stream(request(CancellationToken::new())))
    else {
        panic!("error expected");
    };
    assert_eq!(
        error,
        ProviderError::Authentication {
            message: "HTTP 403: denied".to_owned()
        }
    );
}
