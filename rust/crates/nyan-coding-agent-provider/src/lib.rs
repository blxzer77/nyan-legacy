//! Model-provider boundary for the Rust Coding Agent.

use nyan_coding_agent_core::{BoxFuture, CancellationToken, CoreError, Message, ModelRef, Usage};

/// A model selected for one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderModel {
    /// Provider registry id.
    pub provider: String,
    /// Provider-specific model id.
    pub model: String,
}

impl From<ProviderModel> for ModelRef {
    fn from(model: ProviderModel) -> Self {
        ModelRef {
            provider: model.provider,
            model: model.model,
        }
    }
}

/// A tool the model may call.
///
/// Parity gap: only the name and description are declared. The JSON schema of
/// the arguments is not carried yet, which providers treat as an object with no
/// declared properties.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSpec {
    /// Stable tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
}

/// A provider-neutral request.
#[derive(Clone, Debug)]
pub struct ProviderRequest {
    /// Selected model.
    pub model: ProviderModel,
    /// Context messages in order.
    pub messages: Vec<Message>,
    /// Tools the model may call in this request.
    pub tools: Vec<ToolSpec>,
    /// Cancellation owned by the current agent run.
    pub cancellation: CancellationToken,
}

/// A provider-requested stream update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderStreamEvent {
    /// A chunk of assistant text.
    TextDelta {
        /// Text content in this update.
        text: String,
    },
    /// Token accounting for the response, which may arrive before the terminal event.
    Usage(Usage),
    /// A complete provider tool call.
    ToolCall(ToolCallRequest),
    /// The provider has reached a terminal response state.
    Finished {
        /// Terminal reason.
        reason: ProviderStopReason,
    },
}

/// The provider's terminal response reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderStopReason {
    /// The assistant completed without requesting tools.
    Stop,
    /// The assistant requested one or more tools.
    ToolUse,
    /// The response was cut off by the output token limit.
    Length,
    /// The provider failed after producing a response.
    Error {
        /// Provider diagnostic.
        message: String,
    },
    /// The provider observed cancellation.
    Aborted,
}

/// A provider-requested tool call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCallRequest {
    /// Provider call id.
    pub id: String,
    /// Registered tool name.
    pub name: String,
    /// Opaque JSON payload until the protocol crate owns serialization.
    pub arguments: String,
}

/// Errors that cross the provider boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    /// Authentication or credential resolution failed.
    Authentication {
        /// Provider diagnostic.
        message: String,
    },
    /// The selected model does not exist or cannot be used.
    ModelUnavailable {
        /// Provider diagnostic.
        message: String,
    },
    /// The provider returned an error.
    Request {
        /// Provider diagnostic.
        message: String,
        /// Whether a fresh request may be attempted safely.
        retryable: bool,
    },
    /// The provider stream ended unexpectedly.
    Stream {
        /// Provider diagnostic.
        message: String,
        /// Whether a fresh request may be attempted safely.
        retryable: bool,
    },
    /// The caller cancelled the request.
    Cancelled,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authentication { message }
            | Self::ModelUnavailable { message }
            | Self::Request { message, .. }
            | Self::Stream { message, .. } => formatter.write_str(message),
            Self::Cancelled => formatter.write_str("provider request cancelled"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl ProviderError {
    /// Reports whether the request can be retried without changing the outcome contract.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Request { retryable, .. } | Self::Stream { retryable, .. } => *retryable,
            Self::Authentication { .. } | Self::ModelUnavailable { .. } | Self::Cancelled => false,
        }
    }
}

/// A provider stream owned by one model request.
pub trait ProviderStream: Send {
    /// Returns the next update, or `None` if the stream ended without a terminal event.
    fn next(&mut self) -> BoxFuture<Result<Option<ProviderStreamEvent>, ProviderError>>;
}

/// A model provider that does not choose an async runtime.
pub trait ModelProvider: Send + Sync {
    /// Stable provider registry id.
    fn id(&self) -> &str;

    /// Starts one request and returns a stream future owned by the caller.
    fn stream(
        &self,
        request: ProviderRequest,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>>;
}

/// Resolves credentials and models for a provider.
pub trait ProviderRegistry: Send + Sync {
    /// Resolves a provider by id.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Unsupported`] when no provider is registered for `id`.
    fn provider(&self, id: &str) -> Result<&dyn ModelProvider, CoreError>;
}

#[cfg(test)]
mod tests {
    use super::{ProviderModel, ProviderRequest};
    use nyan_coding_agent_core::{Message, MessageRole};

    #[test]
    fn request_keeps_message_order() {
        let request = ProviderRequest {
            model: ProviderModel {
                provider: "test".to_owned(),
                model: "model".to_owned(),
            },
            messages: vec![Message::text(MessageRole::User, "hello")],
            tools: Vec::new(),
            cancellation: nyan_coding_agent_core::CancellationToken::new(),
        };
        assert_eq!(request.messages[0].text, "hello");
        assert!(!request.cancellation.is_cancelled());
    }

    #[test]
    fn request_errors_expose_retryability() {
        assert!(
            super::ProviderError::Request {
                message: "temporary".to_owned(),
                retryable: true,
            }
            .is_retryable()
        );
        assert!(
            !super::ProviderError::Authentication {
                message: "missing credential".to_owned(),
            }
            .is_retryable()
        );
    }
}
