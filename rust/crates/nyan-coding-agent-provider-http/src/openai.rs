//! `OpenAI`-compatible chat-completions streaming provider.
//!
//! The provider owns no async runtime. Network work runs on the
//! [`tokio::runtime::Handle`] it is given, and events reach the caller through a
//! channel, so the future returned by `stream` can be awaited on any executor.

use std::collections::BTreeMap;
use std::time::Duration;

use nyan_coding_agent_core::{
    BoxFuture, ContentBlock, Message, MessageRole, ModelCostRates, Usage,
};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::sse::SseEvent;
use crate::transport::{
    Applied, Finished, Post, StreamProtocol, Transport, apply_usage_cost, usage_from_counts,
};

/// A provider that speaks the `OpenAI` chat-completions streaming protocol.
#[derive(Clone, Debug)]
pub struct OpenAiChatProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    transport: Transport,
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
        Ok(Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            transport: Transport::new(handle)?,
        })
    }

    /// Sends the conversation id in header `name` with every request.
    #[must_use]
    pub fn with_session_header(mut self, name: &str) -> Self {
        self.transport.set_session_header(name);
        self
    }

    /// Sets how long a response may stay silent before the stream fails.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.transport.set_idle_timeout(idle_timeout);
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
        let post = Post {
            url: format!("{}/chat/completions", self.base_url),
            body: build_request_body(&request).to_string(),
            bearer: self.api_key.clone(),
            headers: self
                .transport
                .session_headers(request.session_id.as_deref()),
        };
        self.transport.start(
            post,
            request.cancellation.clone(),
            Accumulator {
                cost_rates: request.options.cost_rates,
                ..Accumulator::default()
            },
        )
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
    if let Some(effort) = &request.options.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    if let Some(retention) = &request.options.prompt_cache_retention {
        body["prompt_cache_retention"] = json!(retention);
    }
    if let Some(tier) = &request.options.service_tier {
        body["service_tier"] = json!(tier);
    }
    if let Some(max_output_tokens) = request.options.max_output_tokens {
        body["max_completion_tokens"] = json!(max_output_tokens);
    }
    body
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
        },
    })
}

fn wire_message(message: &Message) -> Value {
    let blocks = message.model_content();
    match message.role {
        MessageRole::System => json!({
            "role": "system",
            "content": wire_prompt_content(&blocks),
        }),
        MessageRole::User => json!({
            "role": "user",
            "content": wire_prompt_content(&blocks),
        }),
        MessageRole::Assistant => {
            let mut wire = json!({
                "role": "assistant",
                "content": wire_text_content(&blocks).map_or(Value::Null, Value::String),
            });
            let calls = blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(call) => Some(call),
                    _ => None,
                })
                .map(|call| {
                    json!({
                        "id": call.id.as_str(),
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.arguments},
                    })
                })
                .collect::<Vec<_>>();
            if !calls.is_empty() {
                wire["tool_calls"] = Value::Array(calls);
            }
            wire
        }
        MessageRole::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
            "content": message.text_content(),
        }),
    }
}

fn wire_text_content(blocks: &[ContentBlock]) -> Option<String> {
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

fn wire_prompt_content(blocks: &[ContentBlock]) -> Value {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => parts.push(json!({"type": "text", "text": text})),
            ContentBlock::Image { data, mime_type } => parts.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime_type};base64,{data}")},
            })),
            ContentBlock::Thinking { .. } | ContentBlock::ToolCall(_) => {}
        }
    }
    if parts.len() == 1 && parts[0].get("type").and_then(Value::as_str) == Some("text") {
        parts[0].get("text").cloned().unwrap_or(Value::Null)
    } else {
        Value::Array(parts)
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
    cost_rates: Option<ModelCostRates>,
}

fn parse_usage(value: &Value, cost_rates: Option<ModelCostRates>) -> Usage {
    let count = |pointer: &str| value.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let mut parsed = usage_from_counts(
        count("/prompt_tokens"),
        count("/completion_tokens"),
        count("/prompt_tokens_details/cached_tokens"),
        count("/prompt_tokens_details/cache_write_tokens"),
    );
    parsed.reasoning = count("/completion_tokens_details/reasoning_tokens");
    apply_usage_cost(&mut parsed, cost_rates);
    parsed
}

impl Accumulator {
    /// Applies one decoded chunk and returns the text it carried, if any.
    fn apply_chunk(&mut self, chunk: &Value) -> Result<Option<String>, ProviderError> {
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
            self.usage = Some(parse_usage(usage, self.cost_rates));
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
}

impl StreamProtocol for Accumulator {
    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError> {
        if event.data == "[DONE]" {
            return Ok(Applied::Done);
        }
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            return Err(ProviderError::Stream {
                message: "the provider sent a chunk that is not valid JSON".to_owned(),
                retryable: false,
            });
        };
        Ok(self
            .apply_chunk(&value)?
            .map_or(Applied::Nothing, Applied::Text))
    }

    fn is_terminal(&self) -> bool {
        self.finish_reason.is_some()
    }

    fn finish(self) -> Finished {
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
            Some("length") => ProviderStopReason::Length,
            _ if has_calls => ProviderStopReason::ToolUse,
            _ => ProviderStopReason::Stop,
        };
        Finished {
            usage,
            calls,
            reason,
        }
    }
}
