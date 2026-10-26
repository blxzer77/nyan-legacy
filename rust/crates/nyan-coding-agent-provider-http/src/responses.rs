//! `OpenAI` Responses streaming provider (`openai-responses`).
//!
//! This is the wire API the `CLIProxyAPI` catalog registers every model with, at
//! `{root}/backend-api/codex`, and the one `OpenAI` and xAI use. Requests
//! are stateless (`store: false`): the whole context is sent each time.
//!
//! Not implemented yet, and therefore absent from the requests and the replies:
//! reasoning effort and summaries, encrypted reasoning replay, prompt-cache
//! keys, image input, and custom (grammar) tools. A reasoning item in a reply is
//! skipped, so a reasoning model answers without its earlier reasoning in
//! context.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, ContentBlock, Message, MessageRole, ModelCostRates};
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
/// A provider that speaks the `OpenAI` Responses streaming protocol.
#[derive(Clone, Debug)]
pub struct OpenAiResponsesProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    instruction_role: &'static str,
    transport: Transport,
}

impl OpenAiResponsesProvider {
    /// Creates a provider that posts to `{base_url}/responses`.
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
            instruction_role: "developer",
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

    /// Sends the system prompt as a `system` message instead of `developer`,
    /// for servers that predate the `developer` role.
    #[must_use]
    pub fn with_system_role(mut self) -> Self {
        self.instruction_role = "system";
        self
    }
}

impl ModelProvider for OpenAiResponsesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let headers = self
            .transport
            .session_headers(request.session_id.as_deref());
        let post = Post {
            url: format!("{}/responses", self.base_url),
            body: build_request_body(&request, self.instruction_role).to_string(),
            bearer: self.api_key.clone(),
            headers,
        };
        self.transport.start(
            post,
            request.cancellation.clone(),
            ResponsesStream {
                cost_rates: request.options.cost_rates,
                ..ResponsesStream::default()
            },
        )
    }
}

/// Builds the Responses request body.
#[must_use]
pub fn build_request_body(request: &ProviderRequest, instruction_role: &str) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        push_input_items(&mut input, message, instruction_role);
    }
    let mut body = json!({
        "model": request.model.model,
        "input": input,
        "stream": true,
        "store": false,
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(request.tools.iter().map(wire_tool).collect());
    }
    if let Some(effort) = &request.options.reasoning_effort {
        body["reasoning"] = json!({"effort": effort});
    }
    if let Some(retention) = &request.options.prompt_cache_retention {
        body["prompt_cache_retention"] = json!(retention);
    }
    if let Some(key) = &request.options.prompt_cache_key {
        body["prompt_cache_key"] = json!(key);
    }
    if let Some(tier) = &request.options.service_tier {
        body["service_tier"] = json!(tier);
    }
    if let Some(max_output_tokens) = request.options.max_output_tokens {
        body["max_output_tokens"] = json!(max_output_tokens);
    }
    body
}

pub(crate) fn wire_tool(tool: &ToolSpec) -> Value {
    let parameters = serde_json::from_str::<Value>(&tool.parameters_schema)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": parameters,
    })
}

pub(crate) fn push_input_items(input: &mut Vec<Value>, message: &Message, instruction_role: &str) {
    let blocks = message.model_content();
    match message.role {
        MessageRole::System => {
            let text = text_content(&blocks);
            if !text.is_empty() {
                input.push(json!({"role": instruction_role, "content": text}));
            }
        }
        MessageRole::User => {
            let content = blocks
                .iter()
                .filter_map(wire_input_part)
                .collect::<Vec<_>>();
            if !content.is_empty() {
                input.push(json!({"role": "user", "content": content}));
            }
        }
        MessageRole::Assistant => {
            let text = text_content(&blocks);
            if !text.is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": text, "annotations": []}],
                }));
            }
            for call in blocks.iter().filter_map(|block| match block {
                ContentBlock::ToolCall(call) => Some(call),
                _ => None,
            }) {
                input.push(json!({
                    "type": "function_call",
                    "call_id": call.id.as_str(),
                    "name": call.name,
                    "arguments": call.arguments,
                }));
            }
        }
        MessageRole::Tool => input.push(json!({
            "type": "function_call_output",
            "call_id": message.tool_call_id.as_ref().map_or("", |id| id.as_str()),
            "output": message.text_content(),
        })),
    }
}

fn text_content(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn wire_input_part(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text, .. } => Some(json!({"type": "input_text", "text": text})),
        ContentBlock::Image { data, mime_type } => Some(json!({
            "type": "input_image",
            "image_url": format!("data:{mime_type};base64,{data}"),
        })),
        ContentBlock::Thinking { .. } | ContentBlock::ToolCall(_) => None,
    }
}

#[derive(Default)]
struct PendingCall {
    call_id: String,
    name: String,
    arguments: String,
    done: bool,
}

struct Terminal {
    status: Option<String>,
    incomplete_reason: Option<String>,
    usage: Option<nyan_coding_agent_core::Usage>,
}

#[derive(Default)]
pub(crate) struct ResponsesStream {
    calls: BTreeMap<u64, PendingCall>,
    by_item_id: HashMap<String, u64>,
    terminal: Option<Terminal>,
    cost_rates: Option<ModelCostRates>,
}

impl ResponsesStream {
    pub(crate) fn with_cost_rates(cost_rates: Option<ModelCostRates>) -> Self {
        Self {
            cost_rates,
            ..Self::default()
        }
    }
}

fn stream_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Stream {
        message: message.into(),
        retryable: false,
    }
}

fn text_of<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

impl ResponsesStream {
    /// Finds the call slot an event refers to: by `output_index`, else by `item_id`.
    fn slot_index(&self, event: &Value) -> Option<u64> {
        event
            .get("output_index")
            .and_then(Value::as_u64)
            .or_else(|| text_of(event, "/item_id").and_then(|id| self.by_item_id.get(id).copied()))
    }

    fn add_call(&mut self, index: u64, item: &Value) {
        if let Some(id) = text_of(item, "/id") {
            self.by_item_id.insert(id.to_owned(), index);
        }
        let call = self.calls.entry(index).or_insert_with(|| PendingCall {
            call_id: String::new(),
            name: String::new(),
            arguments: String::new(),
            done: false,
        });
        if let Some(call_id) = text_of(item, "/call_id") {
            call_id.clone_into(&mut call.call_id);
        }
        if let Some(name) = text_of(item, "/name") {
            name.clone_into(&mut call.name);
        }
        if let Some(arguments) = text_of(item, "/arguments") {
            arguments.clone_into(&mut call.arguments);
        }
    }

    fn apply_event(&mut self, event: &Value) -> Result<Applied, ProviderError> {
        match text_of(event, "/type").unwrap_or_default() {
            "response.output_text.delta" | "response.refusal.delta" => Ok(text_of(event, "/delta")
                .filter(|text| !text.is_empty())
                .map_or(Applied::Nothing, |text| Applied::Text(text.to_owned()))),
            "response.output_item.added" => {
                if let (Some(item), Some(index)) = (
                    event.get("item"),
                    event.get("output_index").and_then(Value::as_u64),
                ) && text_of(item, "/type") == Some("function_call")
                {
                    self.add_call(index, item);
                }
                Ok(Applied::Nothing)
            }
            "response.function_call_arguments.delta" => {
                let delta = text_of(event, "/delta").unwrap_or_default();
                let call = self
                    .slot_index(event)
                    .and_then(|index| self.calls.get_mut(&index))
                    .ok_or_else(|| {
                        stream_error("tool-call arguments arrived for an unknown call")
                    })?;
                call.arguments.push_str(delta);
                Ok(Applied::Nothing)
            }
            "response.function_call_arguments.done" => {
                if let (Some(arguments), Some(index)) =
                    (text_of(event, "/arguments"), self.slot_index(event))
                    && let Some(call) = self.calls.get_mut(&index)
                {
                    arguments.clone_into(&mut call.arguments);
                }
                Ok(Applied::Nothing)
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item")
                    && text_of(item, "/type") == Some("function_call")
                {
                    let index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .or_else(|| {
                            text_of(item, "/id").and_then(|id| self.by_item_id.get(id).copied())
                        })
                        .ok_or_else(|| stream_error("a finished tool call has no output index"))?;
                    self.add_call(index, item);
                    if let Some(call) = self.calls.get_mut(&index) {
                        call.done = true;
                    }
                }
                Ok(Applied::Nothing)
            }
            "response.completed" | "response.incomplete" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                self.terminal = Some(Terminal {
                    status: text_of(response, "/status").map(str::to_owned),
                    incomplete_reason: text_of(response, "/incomplete_details/reason")
                        .map(str::to_owned),
                    usage: response
                        .get("usage")
                        .filter(|usage| usage.is_object())
                        .map(|usage| parse_usage(usage, self.cost_rates)),
                });
                Ok(Applied::Done)
            }
            "response.failed" => {
                let response = event.get("response").unwrap_or(&Value::Null);
                let message = if response.get("error").is_some_and(Value::is_object) {
                    format!(
                        "{}: {}",
                        text_of(response, "/error/code").unwrap_or("unknown"),
                        text_of(response, "/error/message").unwrap_or("no message")
                    )
                } else if let Some(reason) = text_of(response, "/incomplete_details/reason") {
                    format!("incomplete: {reason}")
                } else {
                    "Unknown error (no error details in response)".to_owned()
                };
                Err(stream_error(message))
            }
            "error" => Err(stream_error(format!(
                "Error Code {}: {}",
                text_of(event, "/code").unwrap_or("unknown"),
                text_of(event, "/message").unwrap_or("no message")
            ))),
            _ => Ok(Applied::Nothing),
        }
    }
}

fn parse_usage(usage: &Value, cost_rates: Option<ModelCostRates>) -> nyan_coding_agent_core::Usage {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let mut parsed = usage_from_counts(
        count("/input_tokens"),
        count("/output_tokens"),
        count("/input_tokens_details/cached_tokens"),
        count("/input_tokens_details/cache_write_tokens"),
    );
    let reported = count("/total_tokens");
    if reported > 0 {
        parsed.total_tokens = reported;
    }
    parsed.reasoning = count("/output_tokens_details/reasoning_tokens");
    apply_usage_cost(&mut parsed, cost_rates);
    parsed
}
impl ResponsesStream {
    fn apply_non_streaming(&mut self, value: &Value) -> Applied {
        let usage = value
            .get("usage")
            .filter(|usage| usage.is_object())
            .map(|usage| parse_usage(usage, self.cost_rates));
        let status = text_of(value, "/status").map(str::to_owned);
        let incomplete_reason = text_of(value, "/incomplete_details/reason").map(str::to_owned);
        let mut text = String::new();
        if let Some(output) = value.get("output").and_then(Value::as_array) {
            for (index, item) in output.iter().enumerate() {
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        if let Some(content) = item.get("content").and_then(Value::as_array) {
                            for part in content {
                                if let Some(value) = part.get("text").and_then(Value::as_str) {
                                    text.push_str(value);
                                }
                            }
                        }
                    }
                    Some("function_call") => {
                        let call = self.calls.entry(index as u64).or_default();
                        item.get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .clone_into(&mut call.call_id);
                        item.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .clone_into(&mut call.name);
                        item.get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .clone_into(&mut call.arguments);
                        call.done = true;
                    }
                    _ => {}
                }
            }
        }
        self.terminal = Some(Terminal {
            status,
            incomplete_reason,
            usage,
        });
        if text.is_empty() {
            Applied::Nothing
        } else {
            Applied::Text(text)
        }
    }
}

impl StreamProtocol for ResponsesStream {
    fn apply_json(&mut self, body: &str) -> Result<Applied, ProviderError> {
        let value = serde_json::from_str::<Value>(body)
            .map_err(|_| stream_error("the provider returned non-stream JSON that is not valid"))?;
        Ok(self.apply_non_streaming(&value))
    }

    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError> {
        if event.data == "[DONE]" {
            return Ok(Applied::Nothing);
        }
        let value = serde_json::from_str::<Value>(&event.data)
            .map_err(|_| stream_error("the provider sent an event that is not valid JSON"))?;
        self.apply_event(&value)
    }

    fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    fn finish(self) -> Finished {
        let terminal = self.terminal.unwrap_or(Terminal {
            status: None,
            incomplete_reason: None,
            usage: None,
        });
        if let Some(call) = self.calls.values().find(|call| !call.done) {
            return Finished {
                usage: terminal.usage,
                calls: Vec::new(),
                reason: ProviderStopReason::Error {
                    message: format!(
                        "the response ended with an unfinished tool call: {} ({})",
                        call.name, call.call_id
                    ),
                },
            };
        }
        let has_calls = !self.calls.is_empty();
        let calls: Vec<ToolCallRequest> = self
            .calls
            .into_iter()
            .map(|(index, call)| ToolCallRequest {
                id: if call.call_id.is_empty() {
                    format!("call_{index}")
                } else {
                    call.call_id
                },
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        let reason = match terminal.status.as_deref() {
            Some("incomplete") => match terminal.incomplete_reason.as_deref() {
                Some("max_output_tokens") if has_calls => ProviderStopReason::Error {
                    message: "the response hit the output token limit, so its tool-call arguments may be truncated".to_owned(),
                },
                Some("max_output_tokens") => ProviderStopReason::Length,
                Some(other) => ProviderStopReason::Error {
                    message: format!("Response incomplete: {other}"),
                },
                None => ProviderStopReason::Error {
                    message: "Response incomplete without a provider reason".to_owned(),
                },
            },
            Some(status @ ("failed" | "cancelled")) => ProviderStopReason::Error {
                message: format!("the response ended with status {status}"),
            },
            _ if has_calls => ProviderStopReason::ToolUse,
            _ => ProviderStopReason::Stop,
        };
        Finished {
            usage: terminal.usage,
            calls,
            reason,
        }
    }
}
