//! Drives the provider through its public `ModelProvider` entry point against a
//! loopback HTTP server. Nothing here talks to a real provider.

use std::time::Duration;

use nyan_coding_agent_core::{CancellationToken, ContentBlock};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderStopReason, ProviderStream, ProviderStreamEvent,
};
use nyan_coding_agent_provider_http::OpenAiChatProvider;
use serde_json::json;

mod common;
use common::{Server, drain, request, serve, sse};
use tokio::runtime::Runtime;

fn provider(runtime: &Runtime, server: &Server) -> OpenAiChatProvider {
    OpenAiChatProvider::new(
        "loopback",
        format!("http://{}/v1", server.address),
        Some("secret-key".to_owned()),
        runtime.handle().clone(),
    )
    .expect("build provider")
    .with_idle_timeout(Duration::from_secs(5))
}

fn open(
    runtime: &Runtime,
    provider: &OpenAiChatProvider,
) -> Result<Box<dyn ProviderStream>, ProviderError> {
    runtime.block_on(provider.stream(request(CancellationToken::new())))
}

#[test]
fn text_arrives_incrementally_and_the_request_is_well_formed() {
    let runtime = Runtime::new().expect("runtime");
    let first = sse(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#);
    let (head, tail) = first.split_at(10);
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (0, head.to_vec()),
            (20, tail.to_vec()),
            (0, sse(r#"{"choices":[{"delta":{"content":"lo"}}]}"#)),
            (
                0,
                sse(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            ),
            (0, sse("[DONE]")),
        ],
        false,
    );
    let stream = open(&runtime, &provider(&runtime, &server)).expect("open stream");
    assert_eq!(
        drain(&runtime, stream).expect("drain"),
        [
            ProviderStreamEvent::TextDelta {
                text: "Hel".to_owned()
            },
            ProviderStreamEvent::TextDelta {
                text: "lo".to_owned()
            },
            ProviderStreamEvent::Finished {
                reason: ProviderStopReason::Stop
            },
        ]
    );
    let captured = server.captured.lock().expect("captured lock").clone();
    assert!(captured.starts_with("POST /v1/chat/completions HTTP/1.1"));
    assert!(
        captured
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-key")
    );
    let body = captured.split("\r\n\r\n").nth(1).expect("request body");
    let body: serde_json::Value = serde_json::from_str(body).expect("json body");
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hello");
    assert_eq!(body["tools"][0]["function"]["name"], "echo");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["required"][0], "text",
        "the declared argument schema reaches the provider"
    );
}

#[test]
fn image_history_uses_openai_multimodal_parts() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                sse(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            ),
            (0, sse("[DONE]")),
        ],
        false,
    );
    let mut req = request(CancellationToken::new());
    req.messages[0].content.push(ContentBlock::Image {
        data: "aGVsbG8=".to_owned(),
        mime_type: "image/png".to_owned(),
    });
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(req))
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("captured lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({"type": "text", "text": "hello"})
    );
    assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
    assert_eq!(
        body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,aGVsbG8="
    );
}

#[test]
fn a_length_finish_is_not_reported_as_a_clean_stop() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                sse(r#"{"choices":[{"delta":{"content":"cut"},"finish_reason":"length"}]}"#),
            ),
            (0, sse("[DONE]")),
        ],
        false,
    );
    let stream = open(&runtime, &provider(&runtime, &server)).expect("open stream");
    let events = drain(&runtime, stream).expect("drain");
    assert_eq!(
        events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Length
        })
    );
}

#[test]
fn usage_is_reported_before_the_terminal_event() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                sse(r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#),
            ),
            (
                0,
                sse(
                    r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":4}}}"#,
                ),
            ),
            (0, sse("[DONE]")),
        ],
        false,
    );
    let stream = open(&runtime, &provider(&runtime, &server)).expect("open stream");
    let events = drain(&runtime, stream).expect("drain");
    assert_eq!(
        events,
        [
            ProviderStreamEvent::TextDelta {
                text: "ok".to_owned()
            },
            ProviderStreamEvent::Usage(nyan_coding_agent_core::Usage {
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
    let captured = server.captured.lock().expect("captured lock").clone();
    assert!(captured.contains("\"include_usage\":true"), "{captured}");
}

#[test]
fn tool_calls_are_assembled_across_deltas() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                sse(
                    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":"{\"te"}}]}}]}"#,
                ),
            ),
            (
                0,
                sse(
                    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"xt\":\"hi\"}"}}]}}]}"#,
                ),
            ),
            (
                0,
                sse(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
            ),
            (0, sse("[DONE]")),
        ],
        false,
    );
    let stream = open(&runtime, &provider(&runtime, &server)).expect("open stream");
    assert_eq!(
        drain(&runtime, stream).expect("drain"),
        [
            ProviderStreamEvent::ToolCall(nyan_coding_agent_provider::ToolCallRequest {
                id: "call_1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{\"text\":\"hi\"}".to_owned(),
            }),
            ProviderStreamEvent::Finished {
                reason: ProviderStopReason::ToolUse
            },
        ]
    );
}

#[test]
fn http_statuses_map_to_stable_error_kinds() {
    let cases: [(&'static str, &str); 4] = [
        ("401 Unauthorized", "authentication"),
        ("404 Not Found", "model"),
        ("429 Too Many Requests", "retryable"),
        ("400 Bad Request", "fatal"),
    ];
    for (status, expected) in cases {
        let runtime = Runtime::new().expect("runtime");
        let body = br#"{"error":{"message":"nope"}}"#.to_vec();
        let server = serve(&runtime, status, vec![(0, body)], false);
        let error = open(&runtime, &provider(&runtime, &server))
            .err()
            .unwrap_or_else(|| panic!("{status} must be refused"));
        match (expected, &error) {
            ("authentication", ProviderError::Authentication { message })
            | ("model", ProviderError::ModelUnavailable { message }) => {
                assert!(message.contains("nope"), "{status}: {message}");
            }
            (
                "retryable",
                ProviderError::RateLimited {
                    retryable: true, ..
                },
            )
            | (
                "fatal",
                ProviderError::Request {
                    retryable: false, ..
                },
            ) => {}
            _ => panic!("{status} mapped to {error:?}"),
        }
    }
}

#[test]
fn a_stream_that_ends_without_a_terminal_event_is_a_retryable_failure() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(0, sse(r#"{"choices":[{"delta":{"content":"par"}}]}"#))],
        false,
    );
    let stream = open(&runtime, &provider(&runtime, &server)).expect("open stream");
    let error = drain(&runtime, stream).expect_err("truncated stream");
    assert!(matches!(
        error,
        ProviderError::Stream {
            retryable: true,
            ..
        }
    ));
}

#[test]
fn an_error_object_in_the_stream_is_reported() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(0, sse(r#"{"error":{"message":"overloaded"}}"#))],
        false,
    );
    let stream = open(&runtime, &provider(&runtime, &server)).expect("open stream");
    let error = drain(&runtime, stream).expect_err("stream error");
    assert!(matches!(&error, ProviderError::Stream { message, .. } if message == "overloaded"));
}

#[test]
fn cancellation_ends_a_silent_stream() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![(0, sse(r#"{"choices":[{"delta":{"content":"a"}}]}"#))],
        true,
    );
    let cancellation = CancellationToken::new();
    let provider = provider(&runtime, &server);
    let mut stream = runtime
        .block_on(provider.stream(request(cancellation.clone())))
        .expect("open stream");
    assert_eq!(
        runtime.block_on(stream.next()).expect("first event"),
        Some(ProviderStreamEvent::TextDelta {
            text: "a".to_owned()
        })
    );
    cancellation.cancel();
    let error = runtime
        .block_on(stream.next())
        .expect_err("cancelled stream");
    assert!(matches!(error, ProviderError::Cancelled));
}

#[test]
fn a_request_cancelled_before_it_starts_never_connects() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", vec![], false);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let result = runtime.block_on(provider(&runtime, &server).stream(request(cancellation)));
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    assert_eq!(*server.captured.lock().expect("captured lock"), "");
}

#[test]
fn cancellation_ends_a_request_the_server_never_answers() {
    let runtime = Runtime::new().expect("runtime");
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let address = listener.local_addr().expect("address");
    // Accepts the connection and then says nothing, not even response headers.
    runtime.spawn(async move {
        let _held = listener.accept().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let provider = OpenAiChatProvider::new(
        "loopback",
        format!("http://{address}/v1"),
        Some("secret-key".to_owned()),
        runtime.handle().clone(),
    )
    .expect("build provider");
    let cancellation = CancellationToken::new();
    let trigger = cancellation.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    let result = runtime.block_on(provider.stream(request(cancellation)));
    canceller.join().expect("canceller");
    assert!(matches!(result, Err(ProviderError::Cancelled)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}
