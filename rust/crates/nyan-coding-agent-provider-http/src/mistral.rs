//! Mistral Conversations streaming provider.
//!
//! Mistral resembles Chat Completions but uses its own `snake_case` wire schema,
//! supports structured content chunks for thinking, and has provider-specific
//! tool-call id constraints. The neutral Rust boundary currently exposes text,
//! tools, usage, and stop reasons; thinking chunks are consumed but not emitted.

use std::collections::BTreeMap;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, MessageRole, Usage};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::sse::SseEvent;
use crate::transport::{Applied, Finished, Post, StreamProtocol, Transport, usage_from_counts};

/// A provider that speaks Mistral's Conversations streaming endpoint.
#[derive(Clone, Debug)]
pub struct MistralConversationsProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    transport: Transport,
}

impl MistralConversationsProvider {
    /// Creates a provider for a Mistral server root or `/v1` base URL.
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
        Ok(Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            transport: Transport::new(handle)?,
        })
    }

    /// Sets how long a response may stay silent before the stream fails.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.transport.set_idle_timeout(idle_timeout);
        self
    }
}

impl ModelProvider for MistralConversationsProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let base = self.base_url.trim_end_matches("/v1").trim_end_matches('/');
        let post = Post {
            url: format!("{base}/v1/chat/completions"),
            body: build_request_body(&request).to_string(),
            bearer: self.api_key.clone(),
            headers: Vec::new(),
            accept: None,
        };
        self.transport
            .start(post, request.cancellation.clone(), MistralStream::default())
    }
}

/// Builds the Mistral wire request.
#[must_use]
pub fn build_request_body(request: &ProviderRequest) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| match message.role {
            MessageRole::System => json!({"role": "system", "content": message.text}),
            MessageRole::User => json!({"role": "user", "content": message.text}),
            MessageRole::Assistant => {
                let mut value = json!({"role": "assistant", "content": message.text});
                if !message.tool_calls.is_empty() {
                    value["tool_calls"] = Value::Array(
                        message
                            .tool_calls
                            .iter()
                            .enumerate()
                            .map(|(index, call)| {
                                json!({
                                    "id": normalize_tool_id(call.id.as_str(), index),
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        "arguments": call.arguments,
                                    },
                                    "index": index,
                                })
                            })
                            .collect(),
                    );
                }
                value
            }
            MessageRole::Tool => json!({
                "role": "tool",
                "tool_call_id": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
                "content": message.text,
            }),
        })
        .collect();
    let mut body = json!({
        "model": request.model.model,
        "stream": true,
        "messages": messages,
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(wire_tool).collect());
    }
    body
}

fn normalize_tool_id(id: &str, index: usize) -> String {
    let normalized: String = id.chars().filter(char::is_ascii_alphanumeric).collect();
    if normalized.len() == 9 {
        normalized
    } else {
        let prefix: String = normalized.chars().take(8).collect();
        format!("{prefix}{:01x}", index % 16)
    }
}

fn wire_tool(tool: &ToolSpec) -> Value {
    let parameters = serde_json::from_str::<Value>(&tool.parameters_schema)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
            "strict": false,
        },
    })
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct MistralStream {
    calls: BTreeMap<u64, PendingCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    terminal: bool,
}

fn stream_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Stream {
        message: message.into(),
        retryable: false,
    }
}

fn as_u64(value: &Value, names: &[&str]) -> u64 {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_u64))
        .unwrap_or(0)
}

impl MistralStream {
    fn apply_chunk(&mut self, chunk: &Value) -> Result<Applied, ProviderError> {
        if chunk.get("error").is_some() {
            return Err(stream_error(
                chunk
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Mistral reported an error"),
            ));
        }
        if let Some(usage) = chunk.get("usage") {
            let prompt = as_u64(usage, &["prompt_tokens", "promptTokens"]);
            let output = as_u64(usage, &["completion_tokens", "completionTokens"]);
            let cache_read = usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .or_else(|| {
                    usage
                        .pointer("/promptTokensDetails/cachedTokens")
                        .and_then(Value::as_u64)
                })
                .or_else(|| usage.get("num_cached_tokens").and_then(Value::as_u64))
                .unwrap_or(0)
                .min(prompt);
            let mut parsed = usage_from_counts(prompt, output, cache_read, 0);
            let total = as_u64(usage, &["total_tokens", "totalTokens"]);
            if total > 0 {
                parsed.total_tokens = total;
            }
            self.usage = Some(parsed);
        }
        let choice = chunk.pointer("/choices/0").unwrap_or(&Value::Null);
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_owned());
            self.terminal = true;
        }
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let pending = self.calls.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    id.clone_into(&mut pending.id);
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    pending.name.push_str(name);
                }
                if let Some(arguments) = call.pointer("/function/arguments") {
                    if let Some(text) = arguments.as_str() {
                        pending.arguments.push_str(text);
                    } else if arguments.is_object() {
                        pending.arguments.push_str(&arguments.to_string());
                    }
                }
            }
        }
        Ok(delta
            .get("content")
            .and_then(|content| {
                content.as_str().map(str::to_owned).or_else(|| {
                    content.as_array().and_then(|chunks| {
                        let text = chunks
                            .iter()
                            .filter_map(|chunk| {
                                chunk.get("text").and_then(Value::as_str).or_else(|| {
                                    chunk.pointer("/thinking/0/text").and_then(Value::as_str)
                                })
                            })
                            .collect::<String>();
                        (!text.is_empty()).then_some(text)
                    })
                })
            })
            .filter(|text| !text.is_empty())
            .map_or(Applied::Nothing, Applied::Text))
    }
}

impl StreamProtocol for MistralStream {
    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError> {
        if event.data == "[DONE]" {
            self.terminal = true;
            return Ok(Applied::Done);
        }
        let mut value = serde_json::from_str::<Value>(&event.data)
            .map_err(|_| stream_error("the Mistral event is not valid JSON"))?;
        if value.get("data").is_some_and(Value::is_object) {
            value = value["data"].clone();
        }
        self.apply_chunk(&value)
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn finish(self) -> Finished {
        let calls = self
            .calls
            .into_iter()
            .map(|(index, call)| ToolCallRequest {
                id: if call.id.is_empty() {
                    format!("call_{index}")
                } else {
                    call.id
                },
                name: call.name,
                arguments: if call.arguments.is_empty() {
                    "{}".to_owned()
                } else {
                    call.arguments
                },
            })
            .collect::<Vec<_>>();
        let reason = match self.finish_reason.as_deref() {
            Some("length" | "max_tokens") => ProviderStopReason::Length,
            Some("tool_calls" | "tool_use") if !calls.is_empty() => ProviderStopReason::ToolUse,
            Some("stop" | "end_turn") => {
                if calls.is_empty() {
                    ProviderStopReason::Stop
                } else {
                    ProviderStopReason::ToolUse
                }
            }
            None if calls.is_empty() => ProviderStopReason::Stop,
            None => ProviderStopReason::ToolUse,
            Some(other) => ProviderStopReason::Error {
                message: format!("Mistral stopped with: {other}"),
            },
        };
        Finished {
            usage: self.usage,
            calls,
            reason,
        }
    }
}
