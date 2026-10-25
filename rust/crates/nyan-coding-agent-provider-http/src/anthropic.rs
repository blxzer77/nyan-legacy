//! Anthropic Messages streaming provider.
//!
//! This adapter keeps Anthropic's message/content-block protocol separate from
//! OpenAI-shaped requests. It supports text, tool use, tool results, streaming
//! JSON arguments, usage, and the terminal stop reasons. Thinking blocks are
//! accepted and ignored at the provider boundary because the current neutral
//! `ProviderStreamEvent` has no thinking event.

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
use crate::transport::{Applied, Finished, Post, StreamProtocol, Transport};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A provider that speaks the Anthropic Messages streaming protocol.
#[derive(Clone, Debug)]
pub struct AnthropicMessagesProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    transport: Transport,
}

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
            body: build_request_body(&request).to_string(),
            bearer: None,
            headers: vec![
                (
                    "x-api-key".to_owned(),
                    self.api_key.clone().unwrap_or_default(),
                ),
                ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
            ],
            accept: None,
        };
        self.transport.start(
            post,
            request.cancellation.clone(),
            AnthropicStream::default(),
        )
    }
}

/// Builds an Anthropic Messages request.
#[must_use]
pub fn build_request_body(request: &ProviderRequest) -> Value {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        match message.role {
            MessageRole::System => {
                if !message.text.is_empty() {
                    system.push(json!({"type": "text", "text": message.text}));
                }
            }
            MessageRole::User => messages.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": message.text}],
            })),
            MessageRole::Assistant => {
                let mut content = Vec::new();
                if !message.text.is_empty() {
                    content.push(json!({"type": "text", "text": message.text}));
                }
                for call in &message.tool_calls {
                    let input = serde_json::from_str::<Value>(&call.arguments)
                        .unwrap_or_else(|_| json!({}));
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id.as_str(),
                        "name": call.name,
                        "input": input,
                    }));
                }
                if !content.is_empty() {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
            MessageRole::Tool => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
                    "content": message.text,
                    "is_error": message.is_error,
                }],
            })),
        }
    }
    let mut body = json!({
        "model": request.model.model,
        "max_tokens": 16_384,
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
struct AnthropicStream {
    tools: BTreeMap<u64, PendingTool>,
    terminal: Option<ProviderStopReason>,
    usage: Option<Usage>,
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
            self.usage = Some(Usage {
                input,
                output,
                cache_read,
                cache_write,
                total_tokens: input
                    .saturating_add(output)
                    .saturating_add(cache_read)
                    .saturating_add(cache_write),
            });
        }
    }

    fn apply_block_start(&mut self, event: &Value) {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        let block = event.get("content_block").unwrap_or(&Value::Null);
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            return;
        }
        let tool = self.tools.entry(index).or_default();
        if let Some(id) = block.get("id").and_then(Value::as_str) {
            id.clone_into(&mut tool.id);
        }
        if let Some(name) = block.get("name").and_then(Value::as_str) {
            name.clone_into(&mut tool.name);
        }
        if let Some(input) = block.get("input")
            && (!input.is_object() || input.as_object().is_some_and(|object| !object.is_empty()))
        {
            tool.arguments = input.to_string();
        }
    }

    fn apply_block_delta(&mut self, event: &Value) -> Result<Applied, ProviderError> {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        let delta = event.get("delta").unwrap_or(&Value::Null);
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => Ok(delta
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map_or(Applied::Nothing, |text| Applied::Text(text.to_owned()))),
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
            "content_block_start" => {
                self.apply_block_start(event);
                Ok(Applied::Nothing)
            }
            "content_block_delta" => self.apply_block_delta(event),
            "content_block_stop" => {
                if let Some(tool) = self
                    .tools
                    .get_mut(&event.get("index").and_then(Value::as_u64).unwrap_or(0))
                {
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
