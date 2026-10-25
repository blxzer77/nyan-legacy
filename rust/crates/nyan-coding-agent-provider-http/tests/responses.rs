//! Drives the Responses provider through `ModelProvider` against a loopback
//! server that speaks the `openai-responses` SSE wire format.

use std::time::Duration;

use nyan_coding_agent_core::{
    CancellationToken, Message, MessageRole, ToolCall, ToolCallId, Usage,
};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderModel, ProviderRequest, ProviderStopReason,
    ProviderStream, ProviderStreamEvent, ToolCallRequest, ToolSpec,
};
use nyan_coding_agent_provider_http::OpenAiResponsesProvider;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

mod common;
use common::{Server, drain, request, serve};

/// One SSE frame in the Responses format: an `event:` line and a JSON body whose
/// `type` repeats it.
fn frame(value: &Value) -> Vec<u8> {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().expect("type")
    )
    .into_bytes()
}

fn frames(values: &[Value]) -> Vec<(u64, Vec<u8>)> {
    values.iter().map(|value| (0, frame(value))).collect()
}

fn provider(runtime: &Runtime, server: &Server) -> OpenAiResponsesProvider {
    OpenAiResponsesProvider::new(
        "loopback",
        format!("http://{}/backend-api/codex", server.address),
        Some("secret-key".to_owned()),
        runtime.handle().clone(),
    )
    .expect("build provider")
    .with_idle_timeout(Duration::from_secs(5))
}

fn run(
    runtime: &Runtime,
    status: &'static str,
    events: &[Value],
) -> (Server, Result<Vec<ProviderStreamEvent>, ProviderError>) {
    let server = serve(runtime, status, frames(events), false);
    let provider = provider(runtime, &server);
    let result = runtime
        .block_on(provider.stream(request(CancellationToken::new())))
        .and_then(|stream| drain(runtime, stream));
    (server, result)
}

fn completed(usage: &Value) -> Value {
    json!({"type": "response.completed", "response": {"status": "completed", "usage": usage}})
}

fn body_of(server: &Server) -> Value {
    let captured = server.captured.lock().expect("lock").clone();
    serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json body")
}

#[test]
fn text_streams_and_the_request_is_a_stateless_responses_call() {
    let runtime = Runtime::new().expect("runtime");
    let (server, events) = run(
        &runtime,
        "200 OK",
        &[
            json!({"type": "response.created", "response": {"status": "in_progress"}}),
            json!({"type": "response.output_text.delta", "delta": "Hel"}),
            json!({"type": "response.output_text.delta", "delta": "lo"}),
            completed(
                &json!({"input_tokens": 12, "output_tokens": 3, "total_tokens": 15,
                "input_tokens_details": {"cached_tokens": 4}}),
            ),
        ],
    );
    assert_eq!(
        events.expect("stream"),
        [
            ProviderStreamEvent::TextDelta {
                text: "Hel".to_owned()
            },
            ProviderStreamEvent::TextDelta {
                text: "lo".to_owned()
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
    assert!(
        captured.starts_with("POST /backend-api/codex/responses HTTP/1.1"),
        "{captured}"
    );
    assert!(
        captured
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-key")
    );
    assert!(
        captured
            .to_ascii_lowercase()
            .contains("accept: text/event-stream")
    );
    assert!(
        captured
            .to_ascii_lowercase()
            .contains("user-agent: nyan-coding-agent/")
    );
    assert!(
        !captured.to_ascii_lowercase().contains("accept-encoding"),
        "an event stream is not requested compressed: {captured}"
    );
    let body = body_of(&server);
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(
        body["input"],
        json!([{"role": "user", "content": [{"type": "input_text", "text": "hello"}]}])
    );
    assert_eq!(
        body["tools"],
        json!([{
            "type": "function",
            "name": "echo",
            "description": "Echo input",
            "parameters": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
        }])
    );
}

#[test]
fn history_is_sent_as_responses_items_with_the_system_prompt_first() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", frames(&[completed(&json!({}))]), false);
    let mut assistant = Message::text(MessageRole::Assistant, "checking");
    assistant.tool_calls.push(ToolCall {
        id: ToolCallId::new("call_9").expect("id"),
        name: "echo".to_owned(),
        arguments: r#"{"text":"x"}"#.to_owned(),
    });
    let mut result = Message::text(MessageRole::Tool, "x");
    result.tool_call_id = Some(ToolCallId::new("call_9").expect("id"));
    let request = ProviderRequest {
        model: ProviderModel {
            provider: "loopback".to_owned(),
            model: "m".to_owned(),
        },
        messages: vec![
            Message::text(MessageRole::System, "be brief"),
            Message::text(MessageRole::User, "run echo"),
            assistant,
            result,
        ],
        tools: Vec::<ToolSpec>::new(),
        cancellation: CancellationToken::new(),
        session_id: None,
    };
    let stream = runtime
        .block_on(provider(&runtime, &server).stream(request))
        .expect("stream");
    drain(&runtime, stream).expect("drain");
    let body = body_of(&server);
    assert_eq!(
        body["input"],
        json!([
            {"role": "developer", "content": "be brief"},
            {"role": "user", "content": [{"type": "input_text", "text": "run echo"}]},
            {"type": "message", "role": "assistant", "status": "completed",
             "content": [{"type": "output_text", "text": "checking", "annotations": []}]},
            {"type": "function_call", "call_id": "call_9", "name": "echo", "arguments": "{\"text\":\"x\"}"},
            {"type": "function_call_output", "call_id": "call_9", "output": "x"},
        ])
    );
    assert!(body.get("tools").is_none(), "no tools, no tools field");
}

#[test]
fn a_function_call_is_assembled_from_argument_deltas_and_ends_in_tool_use() {
    let runtime = Runtime::new().expect("runtime");
    let (_, events) = run(
        &runtime,
        "200 OK",
        &[
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "reasoning", "id": "rs_1"}}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "echo", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_1", "delta": "{\"te"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "item_id": "fc_1", "delta": "xt\":\"hi\"}"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "echo", "arguments": "{\"text\":\"hi\"}"}}),
            completed(&json!({"input_tokens": 5, "output_tokens": 2})),
        ],
    );
    assert_eq!(
        events.expect("stream"),
        [
            ProviderStreamEvent::Usage(Usage {
                input: 5,
                output: 2,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 7,
            }),
            ProviderStreamEvent::ToolCall(ToolCallRequest {
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
fn argument_deltas_find_their_call_by_item_id_when_the_server_omits_the_output_index() {
    let runtime = Runtime::new().expect("runtime");
    let (_, events) = run(
        &runtime,
        "200 OK",
        &[
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_7", "call_id": "call_7", "name": "echo"}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_7", "delta": "{}"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_7", "call_id": "call_7", "name": "echo", "arguments": "{}"}}),
            completed(&json!({})),
        ],
    );
    let events = events.expect("stream");
    assert!(
        events.contains(&ProviderStreamEvent::ToolCall(ToolCallRequest {
            id: "call_7".to_owned(),
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        }))
    );
}

#[test]
fn a_call_that_never_finished_is_an_error_not_a_tool_use() {
    let runtime = Runtime::new().expect("runtime");
    let (_, events) = run(
        &runtime,
        "200 OK",
        &[
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "echo"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": "{\"te"}),
            completed(&json!({})),
        ],
    );
    let events = events.expect("stream");
    assert_eq!(
        events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Error {
                message: "the response ended with an unfinished tool call: echo (call_1)"
                    .to_owned()
            }
        })
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::ToolCall(_))),
        "a truncated call must not be handed to the agent"
    );
}

#[test]
fn incomplete_and_failed_responses_map_to_distinct_outcomes() {
    let runtime = Runtime::new().expect("runtime");
    let incomplete = |reason: Option<&str>| {
        let details = reason.map_or(Value::Null, |reason| json!({"reason": reason}));
        json!({"type": "response.incomplete",
               "response": {"status": "incomplete", "incomplete_details": details}})
    };
    let last = |event: Value| {
        let (_, events) = run(&runtime, "200 OK", &[event]);
        events.expect("stream").pop().expect("a terminal event")
    };
    assert_eq!(
        last(incomplete(Some("max_output_tokens"))),
        ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Length
        }
    );
    assert_eq!(
        last(incomplete(Some("content_filter"))),
        ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Error {
                message: "Response incomplete: content_filter".to_owned()
            }
        }
    );
    assert_eq!(
        last(incomplete(None)),
        ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Error {
                message: "Response incomplete without a provider reason".to_owned()
            }
        }
    );
    for (event, message) in [
        (
            json!({"type": "response.failed",
                   "response": {"error": {"code": "server_error", "message": "boom"}}}),
            "server_error: boom",
        ),
        (
            json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"}),
            "Error Code rate_limit_exceeded: slow down",
        ),
    ] {
        let (_, result) = run(&runtime, "200 OK", &[event]);
        assert_eq!(
            result.expect_err("stream error"),
            ProviderError::Stream {
                message: message.to_owned(),
                retryable: false
            }
        );
    }
}

#[test]
fn a_stream_that_closes_without_a_terminal_event_is_a_retryable_failure() {
    let runtime = Runtime::new().expect("runtime");
    let (_, result) = run(
        &runtime,
        "200 OK",
        &[json!({"type": "response.output_text.delta", "delta": "partial"})],
    );
    assert_eq!(
        result.expect_err("truncated"),
        ProviderError::Stream {
            message: "the response ended before a terminal event".to_owned(),
            retryable: true
        }
    );
}

#[test]
fn http_statuses_map_to_the_same_error_kinds_as_the_chat_provider() {
    let runtime = Runtime::new().expect("runtime");
    for (status, expect_auth, retryable) in [
        ("401 Unauthorized", true, false),
        ("429 Too Many Requests", false, true),
        ("400 Bad Request", false, false),
    ] {
        let server = serve(
            &runtime,
            status,
            vec![(0, br#"{"error":{"message":"nope"}}"#.to_vec())],
            false,
        );
        let error = runtime
            .block_on(provider(&runtime, &server).stream(request(CancellationToken::new())))
            .err()
            .expect("status error");
        match error {
            ProviderError::Authentication { message } if expect_auth => {
                assert!(message.ends_with(": nope"), "{message}");
            }
            ProviderError::Request {
                message,
                retryable: got,
                ..
            } if !expect_auth => {
                assert_eq!(got, retryable, "{status}");
                assert!(message.ends_with(": nope"), "{message}");
            }
            other => panic!("{status}: unexpected {other:?}"),
        }
    }
}

#[test]
fn cancellation_ends_a_silent_stream() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", Vec::new(), true);
    let cancellation = CancellationToken::new();
    let mut stream: Box<dyn ProviderStream> = runtime
        .block_on(provider(&runtime, &server).stream(request(cancellation.clone())))
        .expect("stream");
    cancellation.cancel();
    let started = std::time::Instant::now();
    let error = runtime.block_on(stream.next()).expect_err("cancelled");
    assert_eq!(error, ProviderError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}
