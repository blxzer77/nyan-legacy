//! Anthropic Messages streaming provider.
//!
//! This adapter keeps Anthropic's message/content-block protocol separate from
//! OpenAI-shaped requests. It supports text, thinking, tool use, tool results,
//! streaming JSON arguments, usage, and the terminal stop reasons.

use std::collections::BTreeMap;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, ContentBlock, MessageRole, ModelCostRates, Usage};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ProviderStreamEvent, ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::sse::SseEvent;
use crate::transport::{Applied, Finished, Post, StreamProtocol, Transport, apply_usage_cost};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A provider that speaks the Anthropic Messages streaming protocol.
#[derive(Clone, Debug)]
pub struct AnthropicMessagesProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    max_tokens: u64,
    transport: Transport,
}

/// The output limit sent when the caller does not know the model's own.
pub const DEFAULT_MAX_TOKENS: u64 = 16_384;

impl AnthropicMessagesProvider {
    /// Creates a provider that posts to `{base_url}/v1/messages`.
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
            max_tokens: DEFAULT_MAX_TOKENS,
            transport: Transport::new(handle)?,
        })
    }

    /// Sets `max_tokens`, which the Messages API requires on every request;
    /// callers pass the model's output limit.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
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

impl ModelProvider for AnthropicMessagesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let post = Post {
            url: format!("{}/v1/messages", self.base_url),
            body: build_request_body(
                &request,
                request.options.max_output_tokens.unwrap_or(self.max_tokens),
            )
            .to_string(),
            bearer: None,
            headers: [
                (
                    "x-api-key".to_owned(),
                    self.api_key.clone().unwrap_or_default(),
                ),
                ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
            ]
            .into_iter()
            .chain(
                self.transport
                    .session_headers(request.session_id.as_deref()),
            )
            .collect(),
        };
        self.transport.start(
            post,
            request.cancellation.clone(),
            AnthropicStream {
                cost_rates: request.options.cost_rates,
                ..AnthropicStream::default()
            },
        )
    }
}

/// Builds an Anthropic Messages request.
#[must_use]
pub fn build_request_body(request: &ProviderRequest, max_tokens: u64) -> Value {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        let blocks = message.model_content();
        match message.role {
            MessageRole::System => {
                for block in blocks {
                    if let ContentBlock::Text { text, .. } = block {
                        system.push(json!({"type": "text", "text": text}));
                    }
                }
            }
            MessageRole::User => {
                let content = blocks
                    .iter()
                    .filter_map(wire_input_block)
                    .collect::<Vec<_>>();
                if !content.is_empty() {
                    messages.push(json!({"role": "user", "content": content}));
                }
            }
            MessageRole::Assistant => {
                let content = blocks
                    .iter()
                    .filter_map(wire_assistant_block)
                    .collect::<Vec<_>>();
                if !content.is_empty() {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
            MessageRole::Tool => {
                let result_content = blocks
                    .iter()
                    .filter_map(wire_input_block)
                    .collect::<Vec<_>>();
                let content = if result_content.len() == 1
                    && result_content[0].get("type").and_then(Value::as_str) == Some("text")
                {
                    result_content[0]
                        .get("text")
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    Value::Array(result_content)
                };
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
                        "content": content,
                        "is_error": message.is_error,
                    }],
                }));
            }
        }
    }
    let mut body = json!({
        "model": request.model.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(wire_tool).collect());
    }
    body
}

fn wire_input_block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text, .. } => Some(json!({"type": "text", "text": text})),
        ContentBlock::Image { data, mime_type } => Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": mime_type, "data": data},
        })),
        ContentBlock::Thinking { .. } | ContentBlock::ToolCall(_) => None,
    }
}

fn wire_assistant_block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text, .. } => Some(json!({"type": "text", "text": text})),
        ContentBlock::Thinking {
            text: _,
            signature: Some(data),
            redacted: true,
        } => Some(json!({"type": "redacted_thinking", "data": data})),
        ContentBlock::Thinking { redacted: true, .. } => None,
        ContentBlock::Thinking {
            text,
            signature,
            redacted: false,
        } => {
            let mut value = json!({"type": "thinking", "thinking": text});
            if let Some(signature) = signature {
                value["signature"] = json!(signature);
            }
            Some(value)
        }
        ContentBlock::Image { data, mime_type } => Some(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": mime_type, "data": data},
        })),
        ContentBlock::ToolCall(call) => {
            let input =
                serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({}));
            Some(json!({
                "type": "tool_use",
                "id": call.id.as_str(),
                "name": call.name,
                "input": input,
            }))
        }
    }
}

fn wire_tool(tool: &ToolSpec) -> Value {
    let input_schema = serde_json::from_str::<Value>(&tool.parameters_schema)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": input_schema,
    })
}

#[derive(Default)]
struct PendingTool {
    id: String,
    name: String,
    arguments: String,
    done: bool,
}

#[derive(Default)]
struct PendingThinking {
    text: String,
    signature: String,
    redacted: bool,
    done: bool,
}

#[derive(Default)]
struct AnthropicStream {
    tools: BTreeMap<u64, PendingTool>,
    thinking: BTreeMap<u64, PendingThinking>,
    terminal: Option<ProviderStopReason>,
    usage: Option<Usage>,
    cost_rates: Option<ModelCostRates>,
}

fn stream_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Stream {
        message: message.into(),
        retryable: false,
    }
}

fn count(value: &Value, pointer: &str) -> u64 {
    value.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

impl AnthropicStream {
    fn apply_message_start(&mut self, event: &Value) {
        if let Some(usage) = event.pointer("/message/usage") {
            let input = count(usage, "/input_tokens");
            let output = count(usage, "/output_tokens");
            let cache_read = count(usage, "/cache_read_input_tokens");
            let cache_write = count(usage, "/cache_creation_input_tokens");
            let reasoning = count(usage, "/output_tokens_details/thinking_tokens");
            let mut parsed = Usage {
                input,
                output,
                reasoning,
                cache_read,
                cache_write,
                total_tokens: input
                    .saturating_add(output)
                    .saturating_add(cache_read)
                    .saturating_add(cache_write),
                cost: None,
            };
            apply_usage_cost(&mut parsed, self.cost_rates);
            self.usage = Some(parsed);
        }
    }

    fn apply_block_start(&mut self, event: &Value) -> Result<Applied, ProviderError> {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        let content_index = usize::try_from(index)
            .map_err(|_| stream_error("Anthropic content-block index is too large"))?;
        let block = event.get("content_block").unwrap_or(&Value::Null);
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                let text = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let signature = block
                    .get("signature")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.thinking.insert(
                    index,
                    PendingThinking {
                        text: text.clone(),
                        signature: signature.clone(),
                        redacted: false,
                        done: false,
                    },
                );
                Ok(Applied::Event(ProviderStreamEvent::ThinkingStart {
                    content_index,
                    text,
                    signature: (!signature.is_empty()).then_some(signature),
                    redacted: false,
                }))
            }
            Some("redacted_thinking") => {
                let signature = block
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.thinking.insert(
                    index,
                    PendingThinking {
                        text: String::new(),
                        signature: signature.clone(),
                        redacted: true,
                        done: false,
                    },
                );
                Ok(Applied::Event(ProviderStreamEvent::ThinkingStart {
                    content_index,
                    text: String::new(),
                    signature: (!signature.is_empty()).then_some(signature),
                    redacted: true,
                }))
            }
            Some("tool_use") => {
                let tool = self.tools.entry(index).or_default();
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    id.clone_into(&mut tool.id);
                }
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    name.clone_into(&mut tool.name);
                }
                if let Some(input) = block.get("input")
                    && (!input.is_object()
                        || input.as_object().is_some_and(|object| !object.is_empty()))
                {
                    tool.arguments = input.to_string();
                }
                Ok(Applied::Nothing)
            }
            _ => Ok(Applied::Nothing),
        }
    }

    fn apply_block_delta(&mut self, event: &Value) -> Result<Applied, ProviderError> {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        let content_index = usize::try_from(index)
            .map_err(|_| stream_error("Anthropic content-block index is too large"))?;
        let delta = event.get("delta").unwrap_or(&Value::Null);
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => Ok(delta
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map_or(Applied::Nothing, |text| Applied::Text(text.to_owned()))),
            Some("thinking_delta") => {
                let text = delta
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let thinking = self
                    .thinking
                    .get_mut(&index)
                    .ok_or_else(|| stream_error("thinking text arrived for an unknown block"))?;
                thinking.text.push_str(text);
                Ok(Applied::Event(ProviderStreamEvent::ThinkingDelta {
                    content_index,
                    delta: text.to_owned(),
                }))
            }
            Some("signature_delta") => {
                let signature = delta
                    .get("signature")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let thinking = self.thinking.get_mut(&index).ok_or_else(|| {
                    stream_error("thinking signature arrived for an unknown block")
                })?;
                thinking.signature.push_str(signature);
                Ok(Applied::Nothing)
            }
            Some("input_json_delta") => {
                let partial = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let tool = self
                    .tools
                    .get_mut(&index)
                    .ok_or_else(|| stream_error("tool arguments arrived for an unknown tool"))?;
                tool.arguments.push_str(partial);
                Ok(Applied::Nothing)
            }
            _ => Ok(Applied::Nothing),
        }
    }

    fn apply_message_delta(&mut self, event: &Value) -> Applied {
        if let Some(stop_reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
            self.terminal = Some(match stop_reason {
                "tool_use" => ProviderStopReason::ToolUse,
                "max_tokens" => ProviderStopReason::Length,
                "end_turn" | "stop_sequence" => ProviderStopReason::Stop,
                other => ProviderStopReason::Error {
                    message: format!("Anthropic stopped with: {other}"),
                },
            });
        }
        if let Some(usage) = event.get("usage") {
            let current = self.usage.get_or_insert_with(Usage::default);
            current.input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(current.input);
            current.output = usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(current.output);
            if let Some(reasoning) = usage
                .pointer("/output_tokens_details/thinking_tokens")
                .and_then(Value::as_u64)
            {
                current.reasoning = reasoning;
            }
            current.cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(current.cache_read);
            current.cache_write = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(current.cache_write);
            current.total_tokens = current
                .input
                .saturating_add(current.output)
                .saturating_add(current.cache_read)
                .saturating_add(current.cache_write);
            apply_usage_cost(current, self.cost_rates);
        }
        if self.terminal.is_some() {
            Applied::Done
        } else {
            Applied::Nothing
        }
    }

    fn apply_event(&mut self, event: &Value) -> Result<Applied, ProviderError> {
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "message_start" => {
                self.apply_message_start(event);
                Ok(Applied::Nothing)
            }
            "content_block_start" => self.apply_block_start(event),
            "content_block_delta" => self.apply_block_delta(event),
            "content_block_stop" => {
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(thinking) = self.thinking.get_mut(&index) {
                    thinking.done = true;
                    return Ok(Applied::Event(ProviderStreamEvent::ThinkingEnd {
                        content_index: usize::try_from(index).map_err(|_| {
                            stream_error("Anthropic content-block index is too large")
                        })?,
                        content: thinking.text.clone(),
                        signature: (!thinking.signature.is_empty())
                            .then_some(thinking.signature.clone()),
                        redacted: thinking.redacted,
                    }));
                }
                if let Some(tool) = self.tools.get_mut(&index) {
                    tool.done = true;
                }
                Ok(Applied::Nothing)
            }
            "message_delta" => Ok(self.apply_message_delta(event)),
            "message_stop" => Ok(if self.terminal.is_some() {
                Applied::Done
            } else {
                Applied::Nothing
            }),
            "error" => Err(stream_error(
                event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic reported an error"),
            )),
            _ => Ok(Applied::Nothing),
        }
    }
}
impl StreamProtocol for AnthropicStream {
    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError> {
        if event.data == "[DONE]" {
            return Ok(Applied::Done);
        }
        let value = serde_json::from_str::<Value>(&event.data)
            .map_err(|_| stream_error("the Anthropic event is not valid JSON"))?;
        self.apply_event(&value)
    }

    fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    fn finish(self) -> Finished {
        let unfinished_thinking = self.thinking.values().find(|thinking| !thinking.done);
        if unfinished_thinking.is_some() {
            return Finished {
                usage: self.usage,
                calls: Vec::new(),
                reason: ProviderStopReason::Error {
                    message: "the Anthropic response ended with an unfinished thinking block"
                        .to_owned(),
                },
            };
        }
        let unfinished = self.tools.values().find(|tool| !tool.done);
        if let Some(tool) = unfinished {
            return Finished {
                usage: self.usage,
                calls: Vec::new(),
                reason: ProviderStopReason::Error {
                    message: format!(
                        "the Anthropic response ended with an unfinished tool call: {} ({})",
                        tool.name, tool.id
                    ),
                },
            };
        }
        let calls = self
            .tools
            .into_iter()
            .map(|(index, tool)| ToolCallRequest {
                id: if tool.id.is_empty() {
                    format!("tool_{index}")
                } else {
                    tool.id
                },
                name: tool.name,
                arguments: if tool.arguments.is_empty() {
                    "{}".to_owned()
                } else {
                    tool.arguments
                },
            })
            .collect();
        Finished {
            usage: self.usage,
            calls,
            reason: self.terminal.unwrap_or(ProviderStopReason::Error {
                message: "Anthropic stream ended without a stop reason".to_owned(),
            }),
        }
    }
}
