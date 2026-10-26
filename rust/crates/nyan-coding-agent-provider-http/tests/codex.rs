//! The Codex provider through `ModelProvider`, against a loopback server that
//! speaks the Responses stream. No real account or token is involved.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nyan_coding_agent_core::{
    BoxFuture, CancellationToken, Message, MessageRole, ToolCall, ToolCallId,
};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderRequestOptions, ProviderStopReason,
    ProviderStreamEvent,
};
use nyan_coding_agent_provider_http::codex::{build_request_body, resolve_url};
use nyan_coding_agent_provider_http::{CodexAuthSource, CodexCredentials, OpenAiCodexProvider};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

mod common;
use common::{drain, request, serve};

/// Hands out a numbered token on every call, or fails.
struct Source {
    calls: AtomicUsize,
    failure: Mutex<Option<ProviderError>>,
}

impl Source {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            failure: Mutex::new(None),
        })
    }

    fn failing(error: ProviderError) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            failure: Mutex::new(Some(error)),
        })
    }
}

impl CodexAuthSource for Source {
    fn credentials(&self) -> BoxFuture<Result<CodexCredentials, ProviderError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let failure = self.failure.lock().expect("lock").clone();
        Box::pin(async move {
            if let Some(error) = failure {
                return Err(error);
            }
            Ok(CodexCredentials {
                access_token: format!("access-{call}"),
                account_id: "acct-77".to_owned(),
            })
        })
    }
}

fn frame(value: &Value) -> Vec<u8> {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().expect("type")
    )
    .into_bytes()
}

fn done() -> Vec<(u64, Vec<u8>)> {
    vec![
        (
            0,
            frame(&json!({"type": "response.output_text.delta", "delta": "Hi"})),
        ),
        (
            0,
            frame(
                &json!({"type": "response.completed", "response": {"status": "completed",
            "usage": {"input_tokens": 3, "output_tokens": 1}}}),
            ),
        ),
    ]
}

fn provider(
    runtime: &Runtime,
    address: std::net::SocketAddr,
    source: Arc<Source>,
) -> OpenAiCodexProvider {
    OpenAiCodexProvider::new(
        "openai-codex",
        &format!("http://{address}/backend-api"),
        source,
        runtime.handle().clone(),
    )
    .expect("provider")
    .with_idle_timeout(Duration::from_secs(5))
}

fn body_of(captured: &str) -> Value {
    serde_json::from_str(captured.split("\r\n\r\n").nth(1).expect("body")).expect("json")
}

fn with_session(session: Option<&str>) -> ProviderRequest {
    ProviderRequest {
        session_id: session.map(str::to_owned),
        ..request(CancellationToken::new())
    }
}

#[test]
fn request_options_are_translated_to_codex_controls() {
    let mut request = with_session(Some("session"));
    request.options = ProviderRequestOptions {
        reasoning_effort: Some("high".to_owned()),
        prompt_cache_retention: Some("24h".to_owned()),
        prompt_cache_key: Some("explicit-key".to_owned()),
        service_tier: Some("priority".to_owned()),
        max_output_tokens: Some(4096),
        cost_rates: None,
    };
    let body = build_request_body(&request);
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["prompt_cache_retention"], "24h");
    assert_eq!(body["prompt_cache_key"], "explicit-key");
    assert_eq!(body["service_tier"], "priority");
    assert_eq!(body["max_output_tokens"], 4096);
}

#[test]
fn urls_accept_a_bare_backend_the_codex_path_or_the_full_endpoint() {
    for (input, expected) in [
        (
            "https://chatgpt.com/backend-api",
            "https://chatgpt.com/backend-api/codex/responses",
        ),
        (
            "https://chatgpt.com/backend-api/",
            "https://chatgpt.com/backend-api/codex/responses",
        ),
        (
            "https://chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex/responses",
        ),
        (
            "https://chatgpt.com/backend-api/codex/responses",
            "https://chatgpt.com/backend-api/codex/responses",
        ),
        ("", "https://chatgpt.com/backend-api/codex/responses"),
    ] {
        assert_eq!(resolve_url(input), expected, "{input:?}");
    }
}

#[test]
fn a_request_carries_the_account_the_beta_header_and_the_session_and_streams_the_reply() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", done(), false);
    let source = Source::ok();
    let stream = runtime
        .block_on(provider(&runtime, server.address, source).stream(with_session(Some("sess-5"))))
        .expect("stream");
    let events = drain(&runtime, stream).expect("events");
    assert_eq!(
        events[0],
        ProviderStreamEvent::TextDelta {
            text: "Hi".to_owned()
        }
    );
    assert_eq!(
        events.last(),
        Some(&ProviderStreamEvent::Finished {
            reason: ProviderStopReason::Stop
        })
    );

    let captured = server.captured.lock().expect("lock").clone();
    let lower = captured.to_ascii_lowercase();
    assert!(
        captured.starts_with("POST /backend-api/codex/responses HTTP/1.1"),
        "{captured}"
    );
    for header in [
        "authorization: bearer access-1",
        "chatgpt-account-id: acct-77",
        "originator: nyan",
        "openai-beta: responses=experimental",
        "accept: text/event-stream",
        "session-id: sess-5",
        "x-client-request-id: sess-5",
    ] {
        assert!(lower.contains(header), "missing `{header}` in\n{captured}");
    }
    let body = body_of(&captured);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["prompt_cache_key"], "sess-5");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["text"], json!({"verbosity": "low"}));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert!(
        body.get("reasoning").is_none(),
        "no reasoning setting is sent"
    );
}

#[test]
fn without_a_session_the_session_headers_and_cache_key_are_absent() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", done(), false);
    let stream = runtime
        .block_on(provider(&runtime, server.address, Source::ok()).stream(with_session(None)))
        .expect("stream");
    drain(&runtime, stream).expect("events");
    let captured = server.captured.lock().expect("lock").clone();
    let lower = captured.to_ascii_lowercase();
    assert!(
        !lower.contains("session-id") && !lower.contains("x-client-request-id"),
        "{captured}"
    );
    assert!(body_of(&captured).get("prompt_cache_key").is_none());
}

#[test]
fn the_system_prompt_becomes_instructions_and_never_an_input_item() {
    let mut with_system = with_session(None);
    with_system.messages = vec![
        Message::text(MessageRole::System, "be brief"),
        Message::text(MessageRole::User, "hello"),
    ];
    let body = build_request_body(&with_system);
    assert_eq!(body["instructions"], "be brief");
    assert_eq!(
        body["input"],
        json!([{"role": "user", "content": [{"type": "input_text", "text": "hello"}]}])
    );

    let mut without = with_session(None);
    without.messages = vec![Message::text(MessageRole::User, "hello")];
    assert_eq!(
        build_request_body(&without)["instructions"],
        "You are a helpful assistant."
    );
}

#[test]
fn history_and_tools_use_the_responses_item_shapes_with_a_null_strict() {
    let mut assistant = Message::text(MessageRole::Assistant, "");
    assistant.tool_calls.push(ToolCall {
        id: ToolCallId::new("call_1").expect("id"),
        name: "echo".to_owned(),
        arguments: r#"{"text":"x"}"#.to_owned(),
    });
    let mut req = with_session(None);
    req.messages = vec![
        Message::text(MessageRole::User, "go"),
        assistant,
        Message::tool_result(
            ToolCallId::new("call_1").expect("id"),
            "echo",
            "done",
            false,
        ),
    ];
    let body = build_request_body(&req);
    assert_eq!(body["input"][1]["type"], "function_call");
    assert_eq!(body["input"][1]["call_id"], "call_1");
    assert_eq!(
        body["input"][2],
        json!({"type": "function_call_output", "call_id": "call_1", "output": "done"})
    );
    assert_eq!(body["tools"][0]["name"], "echo");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["strict"], Value::Null);
    assert!(
        body["tools"][0]
            .as_object()
            .expect("object")
            .contains_key("strict")
    );
}

#[test]
fn credentials_are_asked_for_on_every_request_so_a_refreshed_token_is_used() {
    let runtime = Runtime::new().expect("runtime");
    let source = Source::ok();
    let seen = ["access-1", "access-2"].map(|expected| {
        let server = serve(&runtime, "200 OK", done(), false);
        let stream = runtime
            .block_on(
                provider(&runtime, server.address, Arc::clone(&source)).stream(with_session(None)),
            )
            .expect("stream");
        drain(&runtime, stream).expect("events");
        let captured = server.captured.lock().expect("lock").to_ascii_lowercase();
        assert!(
            captured.contains(&format!("authorization: bearer {expected}")),
            "{captured}"
        );
        expected
    });
    assert_eq!(seen, ["access-1", "access-2"]);
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_credential_failure_stops_the_request_before_any_network_use() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", done(), false);
    let error = ProviderError::Authentication {
        message: "log in again".to_owned(),
    };
    let Err(got) = runtime.block_on(
        provider(&runtime, server.address, Source::failing(error.clone()))
            .stream(with_session(None)),
    ) else {
        panic!("the credential error must surface");
    };
    assert_eq!(got, error);
    assert_eq!(
        server.captured.lock().expect("lock").as_str(),
        "",
        "nothing was sent"
    );
}

#[test]
fn a_cancelled_request_never_asks_for_credentials() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, "200 OK", done(), false);
    let source = Source::ok();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let req = ProviderRequest {
        cancellation,
        ..request(CancellationToken::new())
    };
    let Err(error) =
        runtime.block_on(provider(&runtime, server.address, Arc::clone(&source)).stream(req))
    else {
        panic!("cancelled");
    };
    assert_eq!(error, ProviderError::Cancelled);
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_spent_quota_is_not_retried_but_a_busy_server_is() {
    let runtime = Runtime::new().expect("runtime");
    let run = |body: &str| {
        let server = serve(
            &runtime,
            "429 Too Many Requests",
            vec![(0, body.as_bytes().to_vec())],
            false,
        );
        match runtime
            .block_on(provider(&runtime, server.address, Source::ok()).stream(with_session(None)))
        {
            Err(error) => error,
            Ok(_) => panic!("429 must fail"),
        }
    };
    let busy = run(r#"{"error":{"message":"Too many requests, slow down"}}"#);
    assert!(busy.is_retryable(), "{busy:?}");
    for spent in [
        r#"{"error":{"message":"You have hit your ChatGPT usage limit (plus plan)"}}"#,
        r#"{"error":{"code":"insufficient_quota","message":"x"}}"#,
        "Monthly usage limit reached",
        "FreeUsageLimitError",
    ] {
        assert!(!run(spent).is_retryable(), "{spent}");
    }
}
