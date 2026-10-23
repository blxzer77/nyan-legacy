//! Runtime-neutral domain contracts for the Rust Coding Agent.

#![forbid(unsafe_code)]

use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// The workspace version of the first native Rust scaffold.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A boxed, sendable future used at async boundaries without choosing an executor.
pub type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

/// Stable identity for one persisted conversation session.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(String);

impl SessionId {
    /// Creates an id after rejecting an empty value.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidIdentifier`] when the value is empty or whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CoreError::InvalidIdentifier {
                kind: "session",
                value,
            });
        }
        Ok(Self(value))
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for SessionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stable identity for a tool invocation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Creates a tool-call id after rejecting an empty value.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidIdentifier`] when the value is empty or whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CoreError::InvalidIdentifier {
                kind: "tool_call",
                value,
            });
        }
        Ok(Self(value))
    }

    /// Returns the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ToolCallId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A selected model reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRef {
    /// Provider registry id.
    pub provider: String,
    /// Provider-specific model id.
    pub model: String,
}

/// Message roles exposed at the agent boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageRole {
    /// Provider instructions.
    System,
    /// User input.
    User,
    /// Model output.
    Assistant,
    /// Tool output.
    Tool,
}

/// A provider-neutral tool call requested by an assistant message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCall {
    /// Stable provider call id.
    pub id: ToolCallId,
    /// Registered tool name.
    pub name: String,
    /// Opaque JSON arguments.
    pub arguments: String,
}

/// Token accounting reported by a provider for one assistant response.
///
/// Cost is not carried: it needs per-model pricing, which the Rust model
/// registry does not have yet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens served from the provider cache.
    pub cache_read: u64,
    /// Input tokens written to the provider cache.
    pub cache_write: u64,
    /// Total tokens as reported, or the sum when the provider reports none.
    pub total_tokens: u64,
}

/// Why an assistant response ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// The model finished its answer.
    Stop,
    /// The response hit the output token limit.
    Length,
    /// The model requested tool calls.
    ToolUse,
    /// The response ended with an error.
    Error,
    /// The run was cancelled.
    Aborted,
}

/// Milliseconds since the Unix epoch, or 0 if the clock is before it.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// A provider-neutral conversation message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    /// The message role.
    pub role: MessageRole,
    /// The text payload.
    pub text: String,
    /// The provider tool-call id for a tool result, when applicable.
    pub tool_call_id: Option<ToolCallId>,
    /// The provider tool name for a tool result, when applicable.
    pub tool_name: Option<String>,
    /// Whether a tool result represents a failed execution.
    pub is_error: bool,
    /// Tool calls requested by an assistant message.
    pub tool_calls: Vec<ToolCall>,
    /// Creation time in Unix milliseconds, stamped by the constructors.
    pub timestamp_ms: u64,
    /// Provider id that produced an assistant message.
    pub provider: Option<String>,
    /// Model id that produced an assistant message.
    pub model: Option<String>,
    /// Token accounting for an assistant message, when the provider reported it.
    pub usage: Option<Usage>,
    /// Why an assistant message ended.
    pub stop_reason: Option<StopReason>,
    /// Diagnostic for an assistant message that ended in error.
    pub error_message: Option<String>,
}

impl Message {
    /// Creates a text message.
    #[must_use]
    pub fn text(role: MessageRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            tool_call_id: None,
            tool_name: None,
            is_error: false,
            tool_calls: Vec::new(),
            timestamp_ms: now_ms(),
            provider: None,
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
        }
    }

    /// Creates a model-visible tool result.
    #[must_use]
    pub fn tool_result(
        call_id: ToolCallId,
        tool_name: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: MessageRole::Tool,
            text: text.into(),
            tool_call_id: Some(call_id),
            tool_name: Some(tool_name.into()),
            is_error,
            ..Self::text(MessageRole::Tool, String::new())
        }
    }
}

/// The process-facing execution mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentMode {
    /// Human-oriented interactive terminal mode.
    Interactive,
    /// One-shot text output mode.
    Print,
    /// One-shot JSONL event mode.
    Json,
    /// Long-lived JSONL command mode.
    Rpc,
}

/// Cancellation shared by an agent run and its owned operations.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Creates a non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Returns an error if cancellation was requested.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Cancelled`] after cancellation has been requested.
    pub fn check(&self) -> Result<(), CoreError> {
        if self.is_cancelled() {
            Err(CoreError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Stable errors shared across crate boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreError {
    /// An identifier was empty or otherwise invalid.
    InvalidIdentifier {
        /// The identifier kind.
        kind: &'static str,
        /// The rejected value.
        value: String,
    },
    /// An operation was cancelled by its owner.
    Cancelled,
    /// A requested operation is not available in the current mode.
    Unsupported {
        /// The unavailable operation.
        operation: String,
    },
    /// A required state transition was not valid.
    InvalidState {
        /// Description of the invalid transition.
        message: String,
    },
}

impl Display for CoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIdentifier { kind, value } => {
                write!(formatter, "invalid {kind} identifier: {value}")
            }
            Self::Cancelled => formatter.write_str("operation cancelled"),
            Self::Unsupported { operation } => {
                write!(formatter, "unsupported operation: {operation}")
            }
            Self::InvalidState { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CoreError {}

/// A tool execution result before presentation-specific rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolResult {
    /// Model-facing text.
    pub text: String,
    /// Whether the tool failed from the model's perspective.
    pub is_error: bool,
}

/// A callback for tool-produced partial results.
pub type ToolUpdateSink = Arc<dyn Fn(ToolResult) + Send + Sync + 'static>;

/// A minimal description of the current working environment.
#[derive(Clone)]
pub struct ExecutionContext {
    /// The project working directory.
    pub cwd: PathBuf,
    /// The owning run's cancellation token.
    pub cancellation: CancellationToken,
    updates: Option<ToolUpdateSink>,
}

impl std::fmt::Debug for ExecutionContext {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionContext")
            .field("cwd", &self.cwd)
            .field("cancelled", &self.cancellation.is_cancelled())
            .field("has_update_sink", &self.updates.is_some())
            .finish()
    }
}

impl ExecutionContext {
    /// Creates a tool context without a partial-result callback.
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, cancellation: CancellationToken) -> Self {
        Self {
            cwd: cwd.into(),
            cancellation,
            updates: None,
        }
    }

    /// Returns a copy that forwards partial results to `sink`.
    #[must_use]
    pub fn with_update_sink(mut self, sink: ToolUpdateSink) -> Self {
        self.updates = Some(sink);
        self
    }

    /// Reports a partial result while the tool is executing.
    pub fn report_update(&self, result: ToolResult) {
        if let Some(sink) = &self.updates {
            sink(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CancellationToken, CoreError, Message, MessageRole, SessionId};

    #[test]
    fn identifiers_reject_empty_values() {
        assert_eq!(
            SessionId::new(" "),
            Err(CoreError::InvalidIdentifier {
                kind: "session",
                value: " ".to_owned()
            })
        );
    }

    #[test]
    fn cancellation_is_shared_by_clones() {
        let first = CancellationToken::new();
        let second = first.clone();
        first.cancel();
        assert!(second.is_cancelled());
        assert_eq!(second.check(), Err(CoreError::Cancelled));
    }

    #[test]
    fn messages_keep_their_role_and_text() {
        assert_eq!(
            Message::text(MessageRole::User, "hello").role,
            MessageRole::User
        );
    }
}
