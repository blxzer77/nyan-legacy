//! `OpenAI` Responses streaming provider (`openai-responses`).
//!
//! This is the wire API the `CLIProxyAPI` catalog registers every model with, at
//! `{root}/backend-api/codex`, and the one `OpenAI`, xAI, and Meta use. Requests
//! are stateless (`store: false`): the whole context is sent each time.
//!
//! Not implemented yet, and therefore absent from the requests and the replies:
//! reasoning effort and summaries, encrypted reasoning replay, prompt-cache
//! keys, image input, and custom (grammar) tools. A reasoning item in a reply is
//! skipped, so a reasoning model answers without its earlier reasoning in
//! context.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, Message, MessageRole};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::sse::SseEvent;
use crate::transport::{Applied, Finished, Post, StreamProtocol, Transport, usage_from_counts};

/// A provider that speaks the `OpenAI` Responses streaming protocol.
#[derive(Clone, Debug)]
pub struct OpenAiResponsesProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    instruction_role: &'static str,
    /// Sends the key in this header instead of as a bearer token (Azure: `api-key`).
    key_header: Option<&'static str>,
    /// A query string appended to the URL, without the leading `?`.
    query: Option<String>,
    /// Maps a model id to the deployment name the server expects.
    deployments: BTreeMap<String, String>,
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
            key_header: None,
            query: None,
            deployments: BTreeMap::new(),
            transport: Transport::new(handle)?,
        })
    }

    /// Creates an Azure `OpenAI` provider: the key travels in `api-key`, the URL
    /// carries `api-version`, and `base_url` is the resource's `/openai/v1` root.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the HTTP client cannot be built.
    pub fn azure(
        id: impl Into<String>,
        base_url: &str,
        api_key: Option<String>,
        api_version: &str,
        handle: Handle,
    ) -> Result<Self, ProviderError> {
        let mut provider = Self::new(id, normalize_azure_base_url(base_url), api_key, handle)?;
        provider.key_header = Some("api-key");
        provider.query = Some(format!("api-version={api_version}"));
        Ok(provider)
    }

    /// Maps model ids to deployment names; an unmapped id is sent as is.
    #[must_use]
    pub fn with_deployments(mut self, deployments: BTreeMap<String, String>) -> Self {
        self.deployments = deployments;
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
        let mut request = request;
        if let Some(deployment) = self.deployments.get(&request.model.model) {
            request.model.model.clone_from(deployment);
        }
        let mut url = format!("{}/responses", self.base_url);
        if let Some(query) = &self.query {
            url.push('?');
            url.push_str(query);
        }
        let (bearer, mut headers) = match (self.key_header, &self.api_key) {
            (Some(name), Some(key)) => (None, vec![(name.to_owned(), key.clone())]),
            (None, key) => (key.clone(), Vec::new()),
            (Some(_), None) => (None, Vec::new()),
        };
        headers.extend(
            self.transport
                .session_headers(request.session_id.as_deref()),
        );
        let post = Post {
            url,
            body: build_request_body(&request, self.instruction_role).to_string(),
            bearer,
            headers,
            accept: None,
        };
        self.transport.start(
            post,
            request.cancellation.clone(),
            ResponsesStream::default(),
        )
    }
}

/// Reduces an Azure resource URL to its `/openai/v1` root.
///
/// A bare Azure host, `/openai`, and a pasted `/openai/v1/responses` URL all
/// become `{origin}/openai/v1`; any other URL is only stripped of trailing
/// slashes, so a custom gateway keeps its own path.
#[must_use]
pub fn normalize_azure_base_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    let Ok(url) = reqwest::Url::parse(trimmed) else {
        return trimmed.to_owned();
    };
    let host = url.host_str().unwrap_or_default();
    let azure_host = [
        ".openai.azure.com",
        ".cognitiveservices.azure.com",
        ".ai.azure.com",
    ]
    .iter()
    .any(|suffix| host.ends_with(suffix));
    let path = url.path().trim_end_matches('/');
    if azure_host && matches!(path, "" | "/openai" | "/openai/v1/responses") {
        return format!("{}/openai/v1", url.origin().ascii_serialization());
    }
    trimmed.to_owned()
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
    match message.role {
        MessageRole::System => {
            if !message.text.is_empty() {
                input.push(json!({"role": instruction_role, "content": message.text}));
            }
        }
        MessageRole::User => input.push(json!({
            "role": "user",
            "content": [{"type": "input_text", "text": message.text}],
        })),
        MessageRole::Assistant => {
            if !message.text.is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": message.text, "annotations": []}],
                }));
            }
            for call in &message.tool_calls {
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
            "output": message.text,
        })),
    }
}

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
                        .map(parse_usage),
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

fn parse_usage(usage: &Value) -> nyan_coding_agent_core::Usage {
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
    parsed
}

impl StreamProtocol for ResponsesStream {
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
