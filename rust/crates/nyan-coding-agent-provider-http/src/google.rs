//! Google Generative AI and Vertex AI streaming providers.
//!
//! Both APIs use the Gemini `generateContent` schema. The public providers keep
//! their transport/authentication differences explicit: Generative AI uses an
//! API key query parameter, while Vertex uses a Bearer token and a caller-
//! supplied endpoint prefix.

use std::collections::BTreeMap;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, ContentBlock, MessageRole, Usage};
use nyan_coding_agent_provider::{
    ModelProvider, ProviderError, ProviderRequest, ProviderStopReason, ProviderStream,
    ToolCallRequest, ToolSpec,
};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::sse::SseEvent;
use crate::transport::{Applied, Finished, Post, StreamProtocol, Transport, usage_from_counts};

/// A Google Generative AI provider using an API key.
#[derive(Clone, Debug)]
pub struct GoogleGenerativeAiProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    transport: Transport,
}

/// How a Vertex request is authenticated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VertexAuth {
    /// An API key (Vertex express mode), sent in `x-goog-api-key`.
    ApiKey(String),
    /// An OAuth access token, sent as a bearer token.
    Bearer(String),
}

/// A Google Vertex AI provider.
#[derive(Clone, Debug)]
pub struct GoogleVertexProvider {
    id: String,
    base_url: String,
    auth: Option<VertexAuth>,
    transport: Transport,
}

impl GoogleGenerativeAiProvider {
    /// Creates a provider for a Generative AI base URL that already carries the
    /// API version, for example `https://generativelanguage.googleapis.com/v1beta`.
    ///
    /// The request path becomes `models/{model}:streamGenerateContent`. The key
    /// travels in `x-goog-api-key`, not in the URL.
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

impl GoogleVertexProvider {
    /// Creates a Vertex provider. `base_url` is everything before `/models/`:
    /// `https://aiplatform.googleapis.com/v1/publishers/google` for an API key, or
    /// the project and location path for an access token.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the HTTP client cannot be built.
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        auth: Option<VertexAuth>,
        handle: Handle,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            auth,
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

impl ModelProvider for GoogleGenerativeAiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url,
            url_encode(&request.model.model),
        );
        let post = Post {
            url,
            body: build_request_body(&request).to_string(),
            bearer: None,
            headers: self
                .api_key
                .iter()
                .map(|key| ("x-goog-api-key".to_owned(), key.clone()))
                .chain(
                    self.transport
                        .session_headers(request.session_id.as_deref()),
                )
                .collect(),
        };
        self.transport
            .start(post, request.cancellation.clone(), GoogleStream::default())
    }
}

impl ModelProvider for GoogleVertexProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let (bearer, headers) = match &self.auth {
            Some(VertexAuth::ApiKey(key)) => {
                (None, vec![("x-goog-api-key".to_owned(), key.clone())])
            }
            Some(VertexAuth::Bearer(token)) => (Some(token.clone()), Vec::new()),
            None => (None, Vec::new()),
        };
        let post = Post {
            url: format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                self.base_url,
                url_encode(&request.model.model)
            ),
            body: build_request_body(&request).to_string(),
            bearer,
            headers,
        };
        self.transport
            .start(post, request.cancellation.clone(), GoogleStream::default())
    }
}

/// Builds the shared Gemini request body.
#[must_use]
pub fn build_request_body(request: &ProviderRequest) -> Value {
    let mut system = String::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        let blocks = message.model_content();
        match message.role {
            MessageRole::System => {
                for block in blocks {
                    if let ContentBlock::Text { text, .. } = block {
                        if !system.is_empty() {
                            system.push('\n');
                        }
                        system.push_str(&text);
                    }
                }
            }
            MessageRole::User => {
                let parts = blocks
                    .iter()
                    .filter_map(wire_input_part)
                    .collect::<Vec<_>>();
                if !parts.is_empty() {
                    contents.push(json!({"role": "user", "parts": parts}));
                }
            }
            MessageRole::Assistant => {
                let parts = blocks
                    .iter()
                    .filter_map(wire_model_part)
                    .collect::<Vec<_>>();
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            MessageRole::Tool => {
                let text = message.text_content();
                let args = serde_json::from_str::<Value>(&text)
                    .unwrap_or_else(|_| json!({"output": text}));
                contents.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": {
                        "name": message.tool_name.as_deref().unwrap_or("tool"),
                        "response": args,
                    }}],
                }));
            }
        }
    }
    let mut body = json!({"contents": contents});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system}]});
    }
    if !request.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": request.tools.iter().map(wire_tool).collect::<Vec<_>>()
        }]);
    }
    body
}

fn wire_input_part(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text, .. } => Some(json!({"text": text})),
        ContentBlock::Image { data, mime_type } => Some(json!({
            "inlineData": {"mimeType": mime_type, "data": data},
        })),
        ContentBlock::Thinking { .. } | ContentBlock::ToolCall(_) => None,
    }
}

fn wire_model_part(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text, .. } => Some(json!({"text": text})),
        ContentBlock::ToolCall(call) => {
            let args = serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({}));
            Some(json!({
                "functionCall": {"name": call.name, "args": args},
            }))
        }
        ContentBlock::Thinking { .. } | ContentBlock::Image { .. } => None,
    }
}

fn wire_tool(tool: &ToolSpec) -> Value {
    let parameters = serde_json::from_str::<Value>(&tool.parameters_schema)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({"name": tool.name, "description": tool.description, "parametersJsonSchema": parameters})
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct GoogleStream {
    calls: BTreeMap<u64, PendingCall>,
    next_call: u64,
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

impl GoogleStream {
    fn apply_value(&mut self, value: &Value) -> Result<Applied, ProviderError> {
        if let Some(error) = value.get("error") {
            return Err(stream_error(
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Google reported an error"),
            ));
        }
        if let Some(usage) = value.get("usageMetadata") {
            let input = usage
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache_read = usage
                .get("cachedContentTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(input);
            let output = usage
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_add(
                    usage
                        .get("thoughtsTokenCount")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
            let mut parsed = usage_from_counts(input, output, cache_read, 0);
            if let Some(total) = usage.get("totalTokenCount").and_then(Value::as_u64) {
                parsed.total_tokens = total;
            }
            self.usage = Some(parsed);
        }
        let candidate = value.pointer("/candidates/0").unwrap_or(&Value::Null);
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_owned());
            self.terminal = true;
        }
        let mut text = String::new();
        if let Some(parts) = candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(part_text) = part.get("text").and_then(Value::as_str)
                    && part.get("thought").and_then(Value::as_bool) != Some(true)
                {
                    text.push_str(part_text);
                }
                if let Some(call) = part.get("functionCall") {
                    let index = self.next_call;
                    self.next_call = self.next_call.saturating_add(1);
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let arguments = call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string();
                    self.calls.insert(
                        index,
                        PendingCall {
                            id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .map_or_else(|| format!("{name}_{index}"), str::to_owned),
                            name,
                            arguments,
                        },
                    );
                }
            }
        }
        if text.is_empty() {
            Ok(Applied::Nothing)
        } else {
            Ok(Applied::Text(text))
        }
    }
}

impl StreamProtocol for GoogleStream {
    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError> {
        if event.data == "[DONE]" {
            self.terminal = true;
            return Ok(Applied::Done);
        }
        let value = serde_json::from_str::<Value>(&event.data)
            .map_err(|_| stream_error("the Google event is not valid JSON"))?;
        self.apply_value(&value)
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn finish(self) -> Finished {
        let calls: Vec<ToolCallRequest> = self
            .calls
            .into_values()
            .map(|call| ToolCallRequest {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        let reason = match self.finish_reason.as_deref() {
            Some("MAX_TOKENS") => ProviderStopReason::Length,
            Some("SAFETY" | "RECITATION" | "OTHER") => ProviderStopReason::Error {
                message: format!(
                    "Google stopped with: {}",
                    self.finish_reason.unwrap_or_default()
                ),
            },
            _ if !calls.is_empty() => ProviderStopReason::ToolUse,
            Some("STOP") | None => ProviderStopReason::Stop,
            Some(other) => ProviderStopReason::Error {
                message: format!("Google stopped with: {other}"),
            },
        };
        Finished {
            usage: self.usage,
            calls,
            reason,
        }
    }
}
