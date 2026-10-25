//! Amazon Bedrock `ConverseStream` provider.
//!
//! Requests are signed with `SigV4` from static credentials, or authenticated
//! with a Bedrock API key (`AWS_BEARER_TOKEN_BEDROCK`) as a bearer token.
//! Responses use the binary event-stream framing.
//!
//! Not implemented: credential discovery beyond the values the caller passes
//! (named profiles, ECS and IRSA roles, SSO), prompt-cache points, reasoning
//! content, images, and `additionalModelRequestFields`.

use std::collections::BTreeMap;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, MessageRole, Usage};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::eventstream::EventStreamDecoder;
use crate::sigv4::{AwsCredentials, SignInput, amz_date, sign, uri_encode};
use crate::sse::SseEvent;
use crate::transport::{Applied, Finished, Post, StreamProtocol, Transport};

/// How a Bedrock request is authenticated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BedrockAuth {
    /// A Bedrock API key sent as a bearer token.
    Bearer(String),
    /// IAM credentials, signed with `SigV4`.
    Sigv4(AwsCredentials),
}

/// The current time in Unix seconds; replaceable so signing is testable.
pub type Clock = fn() -> u64;

fn system_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A provider that speaks Bedrock `ConverseStream`.
#[derive(Clone, Debug)]
pub struct BedrockConverseProvider {
    id: String,
    /// Scheme and authority, for example `https://bedrock-runtime.us-east-1.amazonaws.com`.
    endpoint: String,
    region: String,
    auth: BedrockAuth,
    clock: Clock,
    transport: Transport,
}

impl BedrockConverseProvider {
    /// Creates a provider for the regional runtime endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the HTTP client cannot be built.
    pub fn new(
        id: impl Into<String>,
        region: impl Into<String>,
        auth: BedrockAuth,
        handle: Handle,
    ) -> Result<Self, ProviderError> {
        let region = region.into();
        Ok(Self {
            id: id.into(),
            endpoint: format!("https://bedrock-runtime.{region}.amazonaws.com"),
            region,
            auth,
            clock: system_clock,
            transport: Transport::new(handle)?,
        })
    }

    /// Sends requests to `endpoint` (scheme and authority) instead of the regional one.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        endpoint
            .into()
            .trim_end_matches('/')
            .clone_into(&mut self.endpoint);
        self
    }

    /// Replaces the clock used for the signature timestamp.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Sets how long a response may stay silent before the stream fails.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.transport.set_idle_timeout(idle_timeout);
        self
    }
}

impl ModelProvider for BedrockConverseProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let body = build_request_body(&request).to_string();
        let path = format!(
            "/model/{}/converse-stream",
            uri_encode(&request.model.model)
        );
        let host = self
            .endpoint
            .split_once("://")
            .map_or(self.endpoint.as_str(), |(_, authority)| authority)
            .to_owned();
        let (bearer, headers) = match &self.auth {
            BedrockAuth::Bearer(token) => (Some(token.clone()), Vec::new()),
            BedrockAuth::Sigv4(credentials) => {
                let signed = sign(
                    credentials,
                    &SignInput {
                        method: "POST",
                        region: &self.region,
                        service: "bedrock",
                        host: &host,
                        path: &path,
                        content_type: Some("application/json"),
                        body: body.as_bytes(),
                        amz_date: &amz_date((self.clock)()),
                    },
                );
                let mut headers = vec![
                    ("x-amz-date".to_owned(), signed.amz_date),
                    ("authorization".to_owned(), signed.authorization),
                ];
                if let Some(token) = &credentials.session_token {
                    headers.push(("x-amz-security-token".to_owned(), token.clone()));
                }
                (None, headers)
            }
        };
        let post = Post {
            url: format!("{}{path}", self.endpoint),
            body,
            bearer,
            headers,
            accept: Some("application/vnd.amazon.eventstream"),
        };
        self.transport.start_framed(
            post,
            request.cancellation.clone(),
            ConverseStream::default(),
            EventStreamDecoder::new(),
        )
    }
}

/// Builds the `ConverseStream` request body.
#[must_use]
pub fn build_request_body(request: &ProviderRequest) -> Value {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if !message.text.trim().is_empty() {
                    system.push(json!({"text": message.text}));
                }
            }
            MessageRole::User => messages.push(json!({
                "role": "user",
                "content": [{"text": non_blank(&message.text)}],
            })),
            MessageRole::Assistant => {
                let mut content = Vec::new();
                if !message.text.trim().is_empty() {
                    content.push(json!({"text": message.text}));
                }
                for call in &message.tool_calls {
                    content.push(json!({"toolUse": {
                        "toolUseId": call.id.as_str(),
                        "name": call.name,
                        "input": serde_json::from_str::<Value>(&call.arguments)
                            .unwrap_or_else(|_| json!({})),
                    }}));
                }
                if !content.is_empty() {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
            MessageRole::Tool => {
                let result = json!({"toolResult": {
                    "toolUseId": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
                    "content": [{"text": non_blank(&message.text)}],
                    "status": if message.is_error { "error" } else { "success" },
                }});
                // Bedrock wants every result of one turn in a single user message.
                match messages.last_mut() {
                    Some(last) if is_tool_result_message(last) => {
                        if let Some(content) = last["content"].as_array_mut() {
                            content.push(result);
                        }
                    }
                    _ => messages.push(json!({"role": "user", "content": [result]})),
                }
            }
        }
    }
    let mut body = json!({"messages": messages});
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if !request.tools.is_empty() {
        body["toolConfig"] =
            json!({"tools": request.tools.iter().map(wire_tool).collect::<Vec<_>>()});
    }
    body
}

/// Bedrock rejects blank text blocks.
fn non_blank(text: &str) -> &str {
    if text.trim().is_empty() {
        "(empty)"
    } else {
        text
    }
}

fn is_tool_result_message(message: &Value) -> bool {
    message["role"] == "user"
        && message["content"].as_array().is_some_and(|content| {
            content
                .iter()
                .all(|block| block.get("toolResult").is_some())
        })
}

fn wire_tool(tool: &ToolSpec) -> Value {
    let schema = serde_json::from_str::<Value>(&tool.parameters_schema)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({"toolSpec": {
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {"json": schema},
    }})
}

#[derive(Default)]
struct PendingTool {
    id: String,
    name: String,
    arguments: String,
    done: bool,
}

#[derive(Default)]
struct ConverseStream {
    tools: BTreeMap<u64, PendingTool>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
}

fn stream_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Stream {
        message: message.into(),
        retryable: false,
    }
}

fn count(value: &Value, name: &str) -> u64 {
    value.get(name).and_then(Value::as_u64).unwrap_or(0)
}

impl ConverseStream {
    fn apply_payload(&mut self, kind: &str, payload: &Value) -> Result<Applied, ProviderError> {
        let index = payload
            .get("contentBlockIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        match kind {
            "contentBlockStart" => {
                if let Some(tool) = payload.pointer("/start/toolUse") {
                    let pending = self.tools.entry(index).or_default();
                    if let Some(id) = tool.get("toolUseId").and_then(Value::as_str) {
                        id.clone_into(&mut pending.id);
                    }
                    if let Some(name) = tool.get("name").and_then(Value::as_str) {
                        name.clone_into(&mut pending.name);
                    }
                }
                Ok(Applied::Nothing)
            }
            "contentBlockDelta" => {
                if let Some(text) = payload.pointer("/delta/text").and_then(Value::as_str) {
                    return Ok(if text.is_empty() {
                        Applied::Nothing
                    } else {
                        Applied::Text(text.to_owned())
                    });
                }
                if let Some(input) = payload
                    .pointer("/delta/toolUse/input")
                    .and_then(Value::as_str)
                {
                    self.tools
                        .get_mut(&index)
                        .ok_or_else(|| stream_error("tool input arrived for an unknown tool"))?
                        .arguments
                        .push_str(input);
                }
                Ok(Applied::Nothing)
            }
            "contentBlockStop" => {
                if let Some(tool) = self.tools.get_mut(&index) {
                    tool.done = true;
                }
                Ok(Applied::Nothing)
            }
            "messageStop" => {
                self.stop_reason = payload
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Ok(Applied::Nothing)
            }
            "metadata" => {
                if let Some(usage) = payload.get("usage") {
                    let input = count(usage, "inputTokens");
                    let output = count(usage, "outputTokens");
                    let cache_read = count(usage, "cacheReadInputTokens");
                    let cache_write = count(usage, "cacheWriteInputTokens");
                    let reported = count(usage, "totalTokens");
                    self.usage = Some(Usage {
                        input,
                        output,
                        cache_read,
                        cache_write,
                        total_tokens: if reported > 0 {
                            reported
                        } else {
                            input.saturating_add(output)
                        },
                    });
                }
                Ok(Applied::Nothing)
            }
            _ => Ok(Applied::Nothing),
        }
    }
}

impl StreamProtocol for ConverseStream {
    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError> {
        let kind = event.event.as_deref().unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&event.data).unwrap_or(Value::Null);
        if let Some(exception) = kind.strip_prefix("exception:") {
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no message");
            let retryable = matches!(
                exception,
                "throttlingException" | "serviceUnavailableException" | "modelTimeoutException"
            );
            return Err(ProviderError::Stream {
                message: format!("{exception}: {message}"),
                retryable,
            });
        }
        if let Some(code) = kind.strip_prefix("error:") {
            return Err(stream_error(format!("Bedrock error {code}")));
        }
        self.apply_payload(kind, &payload)
    }

    /// Usage arrives in a `metadata` event after `messageStop`, so the response is
    /// complete only when the connection closes after a stop reason.
    fn is_terminal(&self) -> bool {
        self.stop_reason.is_some()
    }

    fn finish(self) -> Finished {
        if let Some(tool) = self.tools.values().find(|tool| !tool.done) {
            return Finished {
                usage: self.usage,
                calls: Vec::new(),
                reason: ProviderStopReason::Error {
                    message: format!(
                        "the response ended with an unfinished tool call: {} ({})",
                        tool.name, tool.id
                    ),
                },
            };
        }
        let calls: Vec<ToolCallRequest> = self
            .tools
            .into_values()
            .map(|tool| ToolCallRequest {
                id: tool.id,
                name: tool.name,
                arguments: if tool.arguments.is_empty() {
                    "{}".to_owned()
                } else {
                    tool.arguments
                },
            })
            .collect();
        let reason = match self.stop_reason.as_deref() {
            Some("end_turn" | "stop_sequence") => ProviderStopReason::Stop,
            Some("tool_use") => ProviderStopReason::ToolUse,
            Some("max_tokens" | "model_context_window_exceeded") => ProviderStopReason::Length,
            Some(other) => ProviderStopReason::Error {
                message: format!("Provider stopped with: {other}"),
            },
            None => ProviderStopReason::Error {
                message: "Bedrock stream ended without a stop reason".to_owned(),
            },
        };
        Finished {
            usage: self.usage,
            calls,
            reason,
        }
    }
}
