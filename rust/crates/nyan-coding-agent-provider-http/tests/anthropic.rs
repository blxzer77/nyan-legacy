//! Loopback tests for the Anthropic Messages adapter.

use std::time::Duration;

use nyan_coding_agent_core::{CancellationToken, Usage};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderStopReason, ProviderStreamEvent, ToolCallRequest,
};
use nyan_coding_agent_provider_http::AnthropicMessagesProvider;
use serde_json::json;
use tokio::runtime::Runtime;

mod common;
use common::{Server, drain, request, serve};

fn event(kind: &str, value: serde_json::Value) -> Vec<u8> {
    let body = value.to_string();
    drop(value);
    format!("event: {kind}\ndata: {body}\n\n").into_bytes()
}

fn provider(runtime: &Runtime, server: &Server) -> AnthropicMessagesProvider {
    AnthropicMessagesProvider::new(
        "anthropic",
        format!("http://{}", server.address),
        Some("anthropic-key".to_owned()),
        runtime.handle().clone(),
    )
    .expect("provider")
    .with_idle_timeout(Duration::from_secs(5))
}

fn text_server(runtime: &Runtime) -> Server {
    serve(
        runtime,
        "200 OK",
        vec![
            (
                0,
                event(
                    "message_start",
                    json!({
                        "type": "message_start",
                        "message": {"usage": {"input_tokens": 10, "output_tokens": 0}}
                    }),
                ),
            ),
            (
                0,
                event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start", "index": 0,
                        "content_block": {"type": "text", "text": ""}
                    }),
                ),
            ),
            (
                0,
                event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta", "index": 0,
                        "delta": {"type": "text_delta", "text": "hello"}
                    }),
                ),
            ),
            (
                0,
                event(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": 0}),
                ),
            ),
            (
                0,
                event(
                    "message_delta",
                    json!({
                        "type": "message_delta", "delta": {"stop_reason": "end_turn"},
                        "usage": {"output_tokens": 2}
                    }),
                ),
            ),
            (0, event("message_stop", json!({"type": "message_stop"}))),
        ],
        false,
    )
}

#[test]
fn request_uses_anthropic_headers_system_prompt_and_content_blocks() {
    let runtime = Runtime::new().expect("runtime");
    let server = text_server(&runtime);
    let mut req = request(CancellationToken::new());
    req.messages.insert(
        0,
        nyan_coding_agent_core::Message::text(
            nyan_coding_agent_core::MessageRole::System,
            "be concise",
        ),
    );
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(req))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert_eq!(
        events,
        [
            ProviderStreamEvent::TextDelta {
                text: "hello".to_owned()
            },
            ProviderStreamEvent::Usage(Usage {
                input: 10,
                output: 2,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 12,
            }),
            ProviderStreamEvent::Finished {
                reason: ProviderStopReason::Stop
            },
        ]
    );
    let captured = server.captured.lock().expect("lock").clone();
    let lower = captured.to_ascii_lowercase();
    assert!(
        captured.starts_with("POST /v1/messages HTTP/1.1"),
        "{captured}"
    );
    assert!(lower.contains("x-api-key: anthropic-key"), "{captured}");
    assert!(
        lower.contains("anthropic-version: 2023-06-01"),
        "{captured}"
    );
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"][0]["text"], "be concise");
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["tools"][0]["name"], "echo");
}

#[test]
fn history_replays_thinking_and_image_blocks_in_order() {
    use nyan_coding_agent_core::{ContentBlock, Message, MessageRole};

    let runtime = Runtime::new().expect("runtime");
    let server = text_server(&runtime);
    let mut assistant = Message::text(MessageRole::Assistant, "");
    assistant.content = vec![
        ContentBlock::Thinking {
            text: "reason".to_owned(),
            signature: Some("sig".to_owned()),
            redacted: false,
        },
        ContentBlock::Image {
            data: "aGVsbG8=".to_owned(),
            mime_type: "image/png".to_owned(),
        },
        ContentBlock::Text {
            text: "answer".to_owned(),
            signature: None,
        },
    ];
    let mut req = request(CancellationToken::new());
    req.messages = vec![Message::text(MessageRole::User, "go"), assistant];
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(req))
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    let content = &body["messages"][1]["content"];
    assert_eq!(
        content[0],
        json!({"type": "thinking", "thinking": "reason", "signature": "sig"})
    );
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
    assert_eq!(content[2], json!({"type": "text", "text": "answer"}));
}

#[test]
fn tool_use_arguments_are_assembled_and_tool_use_stop_is_reported() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start", "index": 1,
                        "content_block": {"type": "tool_use", "id": "toolu_1", "name": "echo", "input": {}}
                    }),
                ),
            ),
            (
                0,
                event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta", "index": 1,
                        "delta": {"type": "input_json_delta", "partial_json": "{\"text\":\"hi\"}"}
                    }),
                ),
            ),
            (
                0,
                event(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": 1}),
                ),
            ),
            (
                0,
                event(
                    "message_delta",
                    json!({
                        "type": "message_delta", "delta": {"stop_reason": "tool_use"}
                    }),
                ),
            ),
        ],
        false,
    );
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(request(CancellationToken::new())))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert!(
        events.contains(&ProviderStreamEvent::ToolCall(ToolCallRequest {
            id: "toolu_1".to_owned(),
            name: "echo".to_owned(),
            arguments: "{\"text\":\"hi\"}".to_owned(),
        }))
    );
    assert_eq!(
        events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::ToolUse
        })
    );
}

#[test]
fn anthropic_stop_reasons_and_unfinished_tool_calls_are_safe() {
    let runtime = Runtime::new().expect("runtime");
    let max = serve(
        &runtime,
        "200 OK",
        vec![(
            0,
            event(
                "message_delta",
                json!({
                    "type": "message_delta", "delta": {"stop_reason": "max_tokens"}
                }),
            ),
        )],
        false,
    );
    let max_events = drain(
        &runtime,
        runtime
            .block_on(provider(&runtime, &max).stream(request(CancellationToken::new())))
            .expect("stream"),
    )
    .expect("events");
    assert_eq!(
        max_events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Length
        })
    );

    let unfinished = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start", "index": 0,
                        "content_block": {"type": "tool_use", "id": "toolu_bad", "name": "echo", "input": {}}
                    }),
                ),
            ),
            (
                0,
                event(
                    "message_delta",
                    json!({
                        "type": "message_delta", "delta": {"stop_reason": "tool_use"}
                    }),
                ),
            ),
        ],
        false,
    );
    let events = drain(
        &runtime,
        runtime
            .block_on(provider(&runtime, &unfinished).stream(request(CancellationToken::new())))
            .expect("stream"),
    )
    .expect("events");
    assert!(matches!(
        events.last(),
        Some(ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Error { .. }
        })
    ));
}

#[test]
fn anthropic_http_auth_error_is_stable() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "401 Unauthorized",
        vec![(0, b"bad key".to_vec())],
        false,
    );
    let Err(error) =
        runtime.block_on(provider(&runtime, &server).stream(request(CancellationToken::new())))
    else {
        panic!("auth error expected");
    };
    assert_eq!(
        error,
        ProviderError::Authentication {
            message: "HTTP 401: bad key".to_owned()
        }
    );
}

#[test]
fn history_replays_tool_use_and_error_results_as_anthropic_blocks() {
    use nyan_coding_agent_core::{Message, MessageRole, ToolCall, ToolCallId};
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(
            0,
            event(
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            ),
        )],
        false,
    );
    let mut assistant = Message::text(MessageRole::Assistant, "running");
    assistant.tool_calls.push(ToolCall {
        id: ToolCallId::new("toolu_9").expect("id"),
        name: "echo".to_owned(),
        arguments: r#"{"text":"x"}"#.to_owned(),
    });
    let failed = Message::tool_result(
        ToolCallId::new("toolu_9").expect("id"),
        "echo",
        "boom",
        true,
    );
    let mut req = request(CancellationToken::new());
    req.messages = vec![Message::text(MessageRole::User, "go"), assistant, failed];
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(req))
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["messages"][1]["content"][0]["text"], "running");
    assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
    assert_eq!(body["messages"][1]["content"][1]["input"]["text"], "x");
    assert_eq!(body["messages"][2]["role"], "user");
    assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "toolu_9");
    assert_eq!(body["messages"][2]["content"][0]["is_error"], true);
}

#[test]
fn max_tokens_is_the_default_unless_the_caller_sets_the_models_limit() {
    let runtime = Runtime::new().expect("runtime");
    let sent = |limit: Option<u64>| {
        let server = serve(
            &runtime,
            "200 OK",
            vec![(
                0,
                event(
                    "message_delta",
                    json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
                ),
            )],
            false,
        );
        let mut provider = provider(&runtime, &server);
        if let Some(limit) = limit {
            provider = provider.with_max_tokens(limit);
        }
        let stream = runtime
            .block_on(provider.stream(request(CancellationToken::new())))
            .expect("stream");
        drain(&runtime, stream).expect("events");
        let captured = server.captured.lock().expect("lock").clone();
        let body: serde_json::Value =
            serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
        body["max_tokens"].clone()
    };
    assert_eq!(sent(None), 16_384);
    assert_eq!(sent(Some(64_000)), 64_000);
}
