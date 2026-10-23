//! `OpenAI`-compatible chat-completions streaming provider.
//!
//! The provider owns no async runtime. Network work runs on the
//! [`tokio::runtime::Handle`] it is given, and events reach the caller through a
//! channel, so the future returned by `stream` can be awaited on any executor.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, CancellationToken, Message, MessageRole, Usage};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ProviderStreamEvent, ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::{Mutex, mpsc};

use crate::sse::SseDecoder;

const CANCELLATION_POLL: Duration = Duration::from_millis(50);
const MAX_ERROR_BODY_BYTES: usize = 4096;

type EventResult = Result<ProviderStreamEvent, ProviderError>;

/// A provider that speaks the `OpenAI` chat-completions streaming protocol.
#[derive(Clone, Debug)]
pub struct OpenAiChatProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    handle: Handle,
    idle_timeout: Duration,
}

impl OpenAiChatProvider {
    /// Creates a provider for `base_url` (for example `https://api.openai.com/v1`).
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the HTTP client cannot be built.
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
        handle: Handle,
    ) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| ProviderError::Request {
                message: format!("failed to build the HTTP client: {error}"),
                retryable: false,
            })?;
        Ok(Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            client,
            handle,
            idle_timeout: Duration::from_secs(120),
        })
    }

    /// Sets how long a response may stay silent before the stream fails.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }
}

impl ModelProvider for OpenAiChatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let client = self.client.clone();
        let url = format!("{}/chat/completions", self.base_url);
        let api_key = self.api_key.clone();
        let handle = self.handle.clone();
        let idle_timeout = self.idle_timeout;
        let cancellation = request.cancellation.clone();
        let body = build_request_body(&request).to_string();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let admission = handle.spawn(async move {
                let mut builder = client
                    .post(url)
                    .header("content-type", "application/json")
                    .header("accept", "text/event-stream")
                    .body(body);
                if let Some(key) = api_key {
                    builder = builder.bearer_auth(key);
                }
                let response = builder
                    .send()
                    .await
                    .map_err(|error| transport_error(&error))?;
                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }
                let text = response.text().await.unwrap_or_default();
                Err(status_error(status.as_u16(), &text))
            });
            let response = admission.await.map_err(|error| ProviderError::Request {
                message: format!("the request task failed: {error}"),
                retryable: false,
            })??;
            let (sender, receiver) = mpsc::channel(64);
            handle.spawn(pump(response, sender, cancellation, idle_timeout));
            Ok(Box::new(ChannelStream {
                receiver: Arc::new(Mutex::new(receiver)),
            }) as Box<dyn ProviderStream>)
        })
    }
}

struct ChannelStream {
    receiver: Arc<Mutex<mpsc::Receiver<EventResult>>>,
}

impl ProviderStream for ChannelStream {
    fn next(&mut self) -> BoxFuture<Result<Option<ProviderStreamEvent>, ProviderError>> {
        let receiver = Arc::clone(&self.receiver);
        Box::pin(async move {
            match receiver.lock().await.recv().await {
                Some(Ok(event)) => Ok(Some(event)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        })
    }
}

fn transport_error(error: &reqwest::Error) -> ProviderError {
    ProviderError::Request {
        message: format!("request failed: {error}"),
        retryable: error.is_connect() || error.is_timeout() || error.is_request(),
    }
}

fn status_error(status: u16, body: &str) -> ProviderError {
    let mut detail: String = body.chars().take(MAX_ERROR_BODY_BYTES).collect();
    if let Ok(value) = serde_json::from_str::<Value>(body)
        && let Some(message) = value.pointer("/error/message").and_then(Value::as_str)
    {
        message.clone_into(&mut detail);
    }
    let message = format!("HTTP {status}: {detail}");
    match status {
        401 | 403 => ProviderError::Authentication { message },
        404 => ProviderError::ModelUnavailable { message },
        408 | 409 | 425 | 429 | 500..=599 => ProviderError::Request {
            message,
            retryable: true,
        },
        _ => ProviderError::Request {
            message,
            retryable: false,
        },
    }
}

/// Builds the chat-completions request body.
#[must_use]
pub fn build_request_body(request: &ProviderRequest) -> Value {
    let mut body = json!({
        "model": request.model.model,
        "messages": request.messages.iter().map(wire_message).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(wire_tool).collect());
    }
    body
}

fn wire_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": {"type": "object", "properties": {}},
        },
    })
}

fn wire_message(message: &Message) -> Value {
    match message.role {
        MessageRole::System => json!({"role": "system", "content": message.text}),
        MessageRole::User => json!({"role": "user", "content": message.text}),
        MessageRole::Assistant => {
            let mut wire = json!({
                "role": "assistant",
                "content": if message.text.is_empty() { Value::Null } else { json!(message.text) },
            });
            if !message.tool_calls.is_empty() {
                wire["tool_calls"] = Value::Array(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id.as_str(),
                                "type": "function",
                                "function": {"name": call.name, "arguments": call.arguments},
                            })
                        })
                        .collect(),
                );
            }
            wire
        }
        MessageRole::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
            "content": message.text,
        }),
    }
}

#[derive(Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct Accumulator {
    tool_calls: BTreeMap<u64, PendingToolCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

fn parse_usage(value: &Value) -> Usage {
    let count = |pointer: &str| value.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let input = count("/prompt_tokens");
    let output = count("/completion_tokens");
    let reported_total = count("/total_tokens");
    Usage {
        input,
        output,
        cache_read: count("/prompt_tokens_details/cached_tokens"),
        cache_write: 0,
        total_tokens: if reported_total > 0 {
            reported_total
        } else {
            input.saturating_add(output)
        },
    }
}

impl Accumulator {
    /// Applies one decoded chunk and returns the text it carried, if any.
    fn apply(&mut self, chunk: &Value) -> Result<Option<String>, ProviderError> {
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the provider reported an error in the stream");
            return Err(ProviderError::Stream {
                message: message.to_owned(),
                retryable: false,
            });
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            self.usage = Some(parse_usage(usage));
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(None);
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_owned());
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(None);
        };
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let pending = self.tool_calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    id.clone_into(&mut pending.id);
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    pending.name.push_str(name);
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    pending.arguments.push_str(arguments);
                }
            }
        }
        Ok(delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned))
    }

    /// Returns the usage, tool-call events, and terminal reason once the stream is complete.
    fn finish(self) -> (Option<Usage>, Vec<ToolCallRequest>, ProviderStopReason) {
        let usage = self.usage;
        let has_calls = !self.tool_calls.is_empty();
        let calls = self
            .tool_calls
            .into_iter()
            .map(|(index, call)| ToolCallRequest {
                id: if call.id.is_empty() {
                    format!("call_{index}")
                } else {
                    call.id
                },
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        let reason = match self.finish_reason.as_deref() {
            Some("length") if has_calls => ProviderStopReason::Error {
                message: "the response hit the output token limit, so its tool-call arguments may be truncated".to_owned(),
            },
            Some("content_filter") => ProviderStopReason::Error {
                message: "the response was stopped by the provider's content filter".to_owned(),
            },
            _ if has_calls => ProviderStopReason::ToolUse,
            _ => ProviderStopReason::Stop,
        };
        (usage, calls, reason)
    }
}

async fn pump(
    mut response: reqwest::Response,
    sender: mpsc::Sender<EventResult>,
    cancellation: CancellationToken,
    idle_timeout: Duration,
) {
    let mut decoder = SseDecoder::new();
    let mut accumulator = Accumulator::default();
    let mut idle = Duration::ZERO;
    let mut done = false;
    'read: loop {
        if cancellation.is_cancelled() {
            let _ = sender.send(Err(ProviderError::Cancelled)).await;
            return;
        }
        let chunk = match tokio::time::timeout(CANCELLATION_POLL, response.chunk()).await {
            Err(_) => {
                idle += CANCELLATION_POLL;
                if idle >= idle_timeout {
                    let _ = sender
                        .send(Err(ProviderError::Stream {
                            message: "the response stream went silent".to_owned(),
                            retryable: true,
                        }))
                        .await;
                    return;
                }
                continue;
            }
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                let _ = sender
                    .send(Err(ProviderError::Stream {
                        message: format!("the response stream failed: {error}"),
                        retryable: true,
                    }))
                    .await;
                return;
            }
        };
        idle = Duration::ZERO;
        for event in decoder.push(&chunk) {
            if event.data == "[DONE]" {
                done = true;
                break 'read;
            }
            let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                let _ = sender
                    .send(Err(ProviderError::Stream {
                        message: "the provider sent a chunk that is not valid JSON".to_owned(),
                        retryable: false,
                    }))
                    .await;
                return;
            };
            match accumulator.apply(&value) {
                Ok(Some(text)) => {
                    if sender
                        .send(Ok(ProviderStreamEvent::TextDelta { text }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            }
        }
    }
    if !done && accumulator.finish_reason.is_none() {
        let _ = sender
            .send(Err(ProviderError::Stream {
                message: "the response ended before a terminal event".to_owned(),
                retryable: true,
            }))
            .await;
        return;
    }
    let (usage, calls, reason) = accumulator.finish();
    if let Some(usage) = usage
        && sender
            .send(Ok(ProviderStreamEvent::Usage(usage)))
            .await
            .is_err()
    {
        return;
    }
    for call in calls {
        if sender
            .send(Ok(ProviderStreamEvent::ToolCall(call)))
            .await
            .is_err()
        {
            return;
        }
    }
    let _ = sender
        .send(Ok(ProviderStreamEvent::Finished { reason }))
        .await;
}
