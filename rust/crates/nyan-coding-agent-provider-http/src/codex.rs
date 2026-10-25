//! The `OpenAI` Codex Responses provider (`openai-codex-responses`), which talks to the
//! `ChatGPT` backend with a subscription login instead of an API key.
//!
//! The wire protocol is the Responses stream of [`crate::responses`]; what differs
//! is the request. The system prompt travels as `instructions`, not as an input
//! item; every request names the `ChatGPT` account and carries the Codex beta
//! header; and the credential is asked for on each request, because an access
//! token expires while a session lives.
//!
//! Not implemented: the WebSocket transport, zstd request compression,
//! previous-response continuation, reasoning effort and summaries, service tiers,
//! and images. The request omits `reasoning`, so the model uses its own default.

use std::sync::Arc;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, MessageRole};
use nyan_coding_agent_provider::{ModelProvider, ProviderError, ProviderRequest, ProviderStream};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::responses::{ResponsesStream, push_input_items, wire_tool};
use crate::transport::{Post, Transport};

/// Where the backend lives.
pub const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const DEFAULT_INSTRUCTIONS: &str = "You are a helpful assistant.";

/// What one Codex request is authorized with.
#[derive(Clone, Eq, PartialEq)]
pub struct CodexCredentials {
    /// The OAuth access token.
    pub access_token: String,
    /// The `ChatGPT` account the token belongs to.
    pub account_id: String,
}

impl std::fmt::Debug for CodexCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexCredentials")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

/// Supplies fresh credentials for each request, refreshing them when they expire.
pub trait CodexAuthSource: Send + Sync {
    /// Returns credentials good for the request about to be sent.
    fn credentials(&self) -> BoxFuture<Result<CodexCredentials, ProviderError>>;
}

/// A provider for the Codex backend.
pub struct OpenAiCodexProvider {
    id: String,
    base_url: String,
    auth: Arc<dyn CodexAuthSource>,
    originator: String,
    transport: Transport,
}

impl std::fmt::Debug for OpenAiCodexProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCodexProvider")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl OpenAiCodexProvider {
    /// Creates a provider that posts to `{base_url}/codex/responses`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the HTTP client cannot be built.
    pub fn new(
        id: impl Into<String>,
        base_url: &str,
        auth: Arc<dyn CodexAuthSource>,
        handle: Handle,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            id: id.into(),
            base_url: resolve_url(base_url),
            auth,
            originator: "nyan".to_owned(),
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

/// The request URL for a base URL, accepting a bare backend, `/codex`, or the full path.
#[must_use]
pub fn resolve_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let base = if trimmed.is_empty() {
        DEFAULT_BASE_URL
    } else {
        trimmed
    };
    if base.ends_with("/codex/responses") {
        base.to_owned()
    } else if base.ends_with("/codex") {
        format!("{base}/responses")
    } else {
        format!("{base}/codex/responses")
    }
}

impl ModelProvider for OpenAiCodexProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let auth = Arc::clone(&self.auth);
        let url = self.base_url.clone();
        let originator = self.originator.clone();
        let transport = self.transport.clone();
        Box::pin(async move {
            if request.cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let credentials = auth.credentials().await?;
            let mut headers = vec![
                ("chatgpt-account-id".to_owned(), credentials.account_id),
                ("originator".to_owned(), originator),
                (
                    "openai-beta".to_owned(),
                    "responses=experimental".to_owned(),
                ),
            ];
            if let Some(session) = &request.session_id {
                headers.push(("session-id".to_owned(), session.clone()));
                headers.push(("x-client-request-id".to_owned(), session.clone()));
            }
            let post = Post {
                url,
                body: build_request_body(&request).to_string(),
                bearer: Some(credentials.access_token),
                headers,
                accept: None,
            };
            transport
                .start(
                    post,
                    request.cancellation.clone(),
                    ResponsesStream::default(),
                )
                .await
        })
    }
}

/// Builds the Codex request body.
#[must_use]
pub fn build_request_body(request: &ProviderRequest) -> Value {
    let instructions = request
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::System && !message.text.is_empty())
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut input = Vec::new();
    for message in &request.messages {
        if message.role != MessageRole::System {
            push_input_items(&mut input, message, "developer");
        }
    }
    let mut body = json!({
        "model": request.model.model,
        "store": false,
        "stream": true,
        "instructions": if instructions.is_empty() { DEFAULT_INSTRUCTIONS } else { &instructions },
        "input": input,
        "text": {"verbosity": "low"},
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
    });
    if let Some(session) = &request.session_id {
        body["prompt_cache_key"] = json!(session);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    let mut wire = wire_tool(tool);
                    // The Codex backend takes the schema as given: `strict` is present and null.
                    wire["strict"] = Value::Null;
                    wire
                })
                .collect(),
        );
    }
    body
}
