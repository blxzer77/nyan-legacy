//! Loopback tests for the Bedrock `ConverseStream` adapter.

use std::time::Duration;

use nyan_coding_agent_core::{
    CancellationToken, Message, MessageRole, ToolCall, ToolCallId, Usage,
};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderStopReason, ProviderStreamEvent, ToolCallRequest,
};
use nyan_coding_agent_provider_http::bedrock::{BedrockAuth, BedrockConverseProvider};
use nyan_coding_agent_provider_http::sigv4::{AwsCredentials, SignInput, sign};
use serde_json::json;
use tokio::runtime::Runtime;

mod common;
use common::{Server, converse_event, drain, eventstream_message, request, serve};

const MODEL: &str = "anthropic.claude-3-5-sonnet-20241022-v2:0";
/// 2026-01-01T00:00:00Z
const NOW: u64 = 1_767_225_600;

fn clock() -> u64 {
    NOW
}

fn credentials() -> AwsCredentials {
    AwsCredentials {
        access_key_id: "AKIDEXAMPLE".to_owned(),
        secret_access_key: "secret-key".to_owned(),
        session_token: Some("session-token".to_owned()),
    }
}

fn provider(runtime: &Runtime, server: &Server, auth: BedrockAuth) -> BedrockConverseProvider {
    BedrockConverseProvider::new("bedrock", "us-west-2", auth, runtime.handle().clone())
        .expect("provider")
        .with_endpoint(format!("http://{}", server.address))
        .with_clock(clock)
        .with_idle_timeout(Duration::from_secs(5))
}

fn model_request() -> nyan_coding_agent_provider::ProviderRequest {
    let mut request = request(CancellationToken::new());
    MODEL.clone_into(&mut request.model.model);
    request
}

fn text_reply() -> Vec<(u64, Vec<u8>)> {
    vec![
        (
            0,
            converse_event("messageStart", &json!({"role": "assistant"})),
        ),
        (
            0,
            converse_event(
                "contentBlockDelta",
                &json!({"contentBlockIndex": 0, "delta": {"text": "Hel"}}),
            ),
        ),
        (
            0,
            converse_event(
                "contentBlockDelta",
                &json!({"contentBlockIndex": 0, "delta": {"text": "lo"}}),
            ),
        ),
        (
            0,
            converse_event("contentBlockStop", &json!({"contentBlockIndex": 0})),
        ),
        (
            0,
            converse_event("messageStop", &json!({"stopReason": "end_turn"})),
        ),
        (
            0,
            converse_event(
                "metadata",
                &json!({"usage": {"inputTokens": 7, "outputTokens": 3, "totalTokens": 10,
                                  "cacheReadInputTokens": 2}}),
            ),
        ),
    ]
}

fn header<'a>(captured: &'a str, name: &str) -> Option<&'a str> {
    captured.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

#[test]
fn a_signed_request_streams_text_and_reports_usage_that_arrives_after_the_stop() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", text_reply(), false);
    let stream = runtime
        .block_on(
            provider(&runtime, &server, BedrockAuth::Sigv4(credentials())).stream(model_request()),
        )
        .expect("stream");
    assert_eq!(
        drain(&runtime, stream).expect("events"),
        [
            ProviderStreamEvent::TextDelta {
                text: "Hel".to_owned()
            },
            ProviderStreamEvent::TextDelta {
                text: "lo".to_owned()
            },
            ProviderStreamEvent::Usage(Usage {
                input: 7,
                output: 3,
                cache_read: 2,
                cache_write: 0,
                total_tokens: 10,
            }),
            ProviderStreamEvent::Finished {
                reason: ProviderStopReason::Stop
            },
        ]
    );
    let captured = server.captured.lock().expect("lock").clone();
    assert!(
        captured.starts_with(
            "POST /model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream HTTP/1.1"
        ),
        "{captured}"
    );
    assert_eq!(
        header(&captured, "accept"),
        Some("application/vnd.amazon.eventstream")
    );
    assert_eq!(header(&captured, "x-amz-date"), Some("20260101T000000Z"));
    assert_eq!(
        header(&captured, "x-amz-security-token"),
        Some("session-token")
    );
    // The provider must sign exactly the body, host, and path it sends.
    let body = captured.split("\r\n\r\n").nth(1).expect("body");
    let expected = sign(
        &credentials(),
        &SignInput {
            method: "POST",
            region: "us-west-2",
            service: "bedrock",
            host: &server.address.to_string(),
            path: "/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream",
            content_type: Some("application/json"),
            body: body.as_bytes(),
            amz_date: "20260101T000000Z",
        },
    );
    assert_eq!(
        header(&captured, "authorization"),
        Some(expected.authorization.as_str())
    );
    let json: serde_json::Value = serde_json::from_str(body).expect("json");
    assert_eq!(json["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(json["toolConfig"]["tools"][0]["toolSpec"]["name"], "echo");
    assert_eq!(
        json["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["type"],
        "object"
    );
}

#[test]
fn a_bedrock_api_key_is_sent_as_a_bearer_token_without_a_signature() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", text_reply(), false);
    let stream = runtime
        .block_on(
            provider(
                &runtime,
                &server,
                BedrockAuth::Bearer("bedrock-api-key".to_owned()),
            )
            .stream(model_request()),
        )
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("lock").clone();
    assert_eq!(
        header(&captured, "authorization"),
        Some("Bearer bedrock-api-key")
    );
    assert_eq!(header(&captured, "x-amz-date"), None);
}

#[test]
fn tool_use_is_assembled_and_consecutive_results_share_one_user_message() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                converse_event(
                    "contentBlockStart",
                    &json!({"contentBlockIndex": 1,
                            "start": {"toolUse": {"toolUseId": "tooluse_1", "name": "echo"}}}),
                ),
            ),
            (
                0,
                converse_event(
                    "contentBlockDelta",
                    &json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "{\"text\":"}}}),
                ),
            ),
            (
                0,
                converse_event(
                    "contentBlockDelta",
                    &json!({"contentBlockIndex": 1, "delta": {"toolUse": {"input": "\"x\"}"}}}),
                ),
            ),
            (
                0,
                converse_event("contentBlockStop", &json!({"contentBlockIndex": 1})),
            ),
            (
                0,
                converse_event("messageStop", &json!({"stopReason": "tool_use"})),
            ),
        ],
        false,
    );
    let mut assistant = Message::text(MessageRole::Assistant, "");
    for id in ["a", "b"] {
        assistant.tool_calls.push(ToolCall {
            id: ToolCallId::new(id).expect("id"),
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        });
    }
    let mut req = model_request();
    req.messages = vec![
        Message::text(MessageRole::User, "go"),
        assistant,
        Message::tool_result(ToolCallId::new("a").expect("id"), "echo", "one", false),
        Message::tool_result(ToolCallId::new("b").expect("id"), "echo", "two", true),
    ];
    let stream = runtime
        .block_on(provider(&runtime, &server, BedrockAuth::Bearer("k".to_owned())).stream(req))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert_eq!(
        events,
        [
            ProviderStreamEvent::ToolCall(ToolCallRequest {
                id: "tooluse_1".to_owned(),
                name: "echo".to_owned(),
                arguments: "{\"text\":\"x\"}".to_owned(),
            }),
            ProviderStreamEvent::Finished {
                reason: ProviderStopReason::ToolUse
            },
        ]
    );
    let captured = server.captured.lock().expect("lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(
        messages.len(),
        3,
        "user, assistant, ONE user message of results: {messages:?}"
    );
    assert_eq!(messages[1]["content"][0]["toolUse"]["toolUseId"], "a");
    let results = messages[2]["content"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["toolResult"]["status"], "success");
    assert_eq!(results[1]["toolResult"]["toolUseId"], "b");
    assert_eq!(results[1]["toolResult"]["status"], "error");
}

#[test]
fn exceptions_corruption_and_early_close_are_distinct_failures() {
    let runtime = Runtime::new().expect("runtime");
    let run = |chunks: Vec<(u64, Vec<u8>)>| {
        let server = serve(&runtime, "200 OK", chunks, false);
        runtime
            .block_on(
                provider(&runtime, &server, BedrockAuth::Bearer("k".to_owned()))
                    .stream(model_request()),
            )
            .and_then(|stream| drain(&runtime, stream))
    };

    let throttled = run(vec![(
        0,
        eventstream_message(
            &[
                (":message-type", "exception"),
                (":exception-type", "throttlingException"),
            ],
            br#"{"message":"Too many requests"}"#,
        ),
    )]);
    assert_eq!(
        throttled.expect_err("throttled"),
        ProviderError::Stream {
            message: "throttlingException: Too many requests".to_owned(),
            retryable: true
        }
    );
    let validation = run(vec![(
        0,
        eventstream_message(
            &[
                (":message-type", "exception"),
                (":exception-type", "validationException"),
            ],
            br#"{"message":"bad input"}"#,
        ),
    )]);
    assert_eq!(
        validation.expect_err("validation"),
        ProviderError::Stream {
            message: "validationException: bad input".to_owned(),
            retryable: false
        }
    );

    let mut corrupt = converse_event("messageStop", &json!({"stopReason": "end_turn"}));
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xFF;
    let error = run(vec![(0, corrupt)]).expect_err("corrupt frame");
    assert!(
        error.to_string().contains("event stream is corrupt"),
        "{error}"
    );

    let early = run(vec![(
        0,
        converse_event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 0, "delta": {"text": "partial"}}),
        ),
    )]);
    assert_eq!(
        early.expect_err("early close"),
        ProviderError::Stream {
            message: "the response ended before a terminal event".to_owned(),
            retryable: true
        }
    );
}

#[test]
fn stop_reasons_map_to_distinct_outcomes_and_http_errors_are_stable() {
    let runtime = Runtime::new().expect("runtime");
    let finish = |stop: &str| {
        let server = serve(
            &runtime,
            "200 OK",
            vec![(
                0,
                converse_event("messageStop", &json!({"stopReason": stop})),
            )],
            false,
        );
        let stream = runtime
            .block_on(
                provider(&runtime, &server, BedrockAuth::Bearer("k".to_owned()))
                    .stream(model_request()),
            )
            .expect("stream");
        drain(&runtime, stream)
            .expect("events")
            .pop()
            .expect("terminal")
    };
    assert_eq!(
        finish("max_tokens"),
        ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Length
        }
    );
    assert_eq!(
        finish("guardrail_intervened"),
        ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Error {
                message: "Provider stopped with: guardrail_intervened".to_owned()
            }
        }
    );

    let denied = serve(
        &runtime,
        "403 Forbidden",
        vec![(0, br#"{"message":"no access"}"#.to_vec())],
        false,
    );
    let Err(error) = runtime.block_on(
        provider(&runtime, &denied, BedrockAuth::Bearer("k".to_owned())).stream(model_request()),
    ) else {
        panic!("error expected");
    };
    assert_eq!(
        error,
        ProviderError::Authentication {
            message: "HTTP 403: {\"message\":\"no access\"}".to_owned()
        }
    );
}

#[test]
fn a_tool_call_that_never_stopped_is_an_error_not_a_tool_use() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(
        &runtime,
        "200 OK",
        vec![
            (
                0,
                converse_event(
                    "contentBlockStart",
                    &json!({"contentBlockIndex": 0,
                            "start": {"toolUse": {"toolUseId": "tooluse_x", "name": "echo"}}}),
                ),
            ),
            (
                0,
                converse_event("messageStop", &json!({"stopReason": "tool_use"})),
            ),
        ],
        false,
    );
    let stream = runtime
        .block_on(
            provider(&runtime, &server, BedrockAuth::Bearer("k".to_owned()))
                .stream(model_request()),
        )
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert_eq!(
        events,
        [ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Error {
                message: "the response ended with an unfinished tool call: echo (tooluse_x)"
                    .to_owned()
            }
        }],
        "a truncated call must not reach the agent"
    );
}
