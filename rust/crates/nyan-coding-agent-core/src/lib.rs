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

/// One block of message content, in the order the model produced it.
///
/// An assistant message is a list of blocks, not a text plus a list of calls: a
/// response can think, speak, call a tool, and speak again, and a provider needs the
/// blocks back in that order (a thinking block with its signature must stay in front
/// of the tool call it led to).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text.
        text: String,
        /// Opaque provider metadata that must come back with the text.
        signature: Option<String>,
    },
    /// The model's reasoning.
    Thinking {
        /// Visible reasoning text; empty for a redacted block.
        text: String,
        /// Opaque provider replay data (a signature, or an encrypted payload).
        signature: Option<String>,
        /// Whether the provider withheld the reasoning and sent only the payload.
        redacted: bool,
    },
    /// An image.
    Image {
        /// Base64-encoded image bytes.
        data: String,
        /// Media type such as `image/png`.
        mime_type: String,
    },
    /// A tool call requested by the model.
    ToolCall(ToolCall),
}

/// Fixed-point token cost in micro-dollars.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UsageCost {
    /// Cost of uncached input tokens.
    pub input_micros: u64,
    /// Cost of output tokens.
    pub output_micros: u64,
    /// Cost of cached input tokens.
    pub cache_read_micros: u64,
    /// Cost of cache writes.
    pub cache_write_micros: u64,
    /// Sum of the four components.
    pub total_micros: u64,
}

/// Per-million-token prices represented as micro-dollars.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModelCostRates {
    /// Input price in dollars per million tokens, converted to micro-dollars.
    pub input_micros_per_million: u64,
    /// Output price in dollars per million tokens, converted to micro-dollars.
    pub output_micros_per_million: u64,
    /// Cached input price in dollars per million tokens, converted to micro-dollars.
    pub cache_read_micros_per_million: u64,
    /// Cache-write price in dollars per million tokens, converted to micro-dollars.
    pub cache_write_micros_per_million: u64,
}

impl ModelCostRates {
    /// Converts catalog prices expressed as dollars per million tokens.
    #[must_use]
    pub fn from_dollars_per_million(
        input: f64,
        output: f64,
        cache_read: f64,
        cache_write: f64,
    ) -> Option<Self> {
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        fn convert(value: f64) -> Option<u64> {
            if !value.is_finite() || value < 0.0 {
                return None;
            }
            let micros = value * 1_000_000.0;
            if !micros.is_finite() || micros > u64::MAX as f64 {
                return None;
            }
            Some(micros.round() as u64)
        }
        Some(Self {
            input_micros_per_million: convert(input)?,
            output_micros_per_million: convert(output)?,
            cache_read_micros_per_million: convert(cache_read)?,
            cache_write_micros_per_million: convert(cache_write)?,
        })
    }

    /// Calculates a fixed-point cost from token counts.
    #[must_use]
    pub fn calculate(self, usage: &Usage) -> UsageCost {
        fn cost(rate: u64, tokens: u64) -> u64 {
            rate.saturating_mul(tokens) / 1_000_000
        }
        let input_micros = cost(self.input_micros_per_million, usage.input);
        let output_micros = cost(self.output_micros_per_million, usage.output);
        let cache_read_micros = cost(self.cache_read_micros_per_million, usage.cache_read);
        let cache_write_micros = cost(self.cache_write_micros_per_million, usage.cache_write);
        UsageCost {
            input_micros,
            output_micros,
            cache_read_micros,
            cache_write_micros,
            total_micros: input_micros
                .saturating_add(output_micros)
                .saturating_add(cache_read_micros)
                .saturating_add(cache_write_micros),
        }
    }
}

/// Token accounting reported by a provider for one assistant response.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens, including reasoning tokens when the provider reports them.
    pub output: u64,
    /// Reasoning tokens included in the output count.
    pub reasoning: u64,
    /// Input tokens served from the provider cache.
    pub cache_read: u64,
    /// Input tokens written to the provider cache.
    pub cache_write: u64,
    /// Total tokens as reported, or the sum when the provider reports none.
    pub total_tokens: u64,
    /// Cost at the selected model's catalog price, when rates were available.
    pub cost: Option<UsageCost>,
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
    /// The content blocks, in order.
    pub content: Vec<ContentBlock>,
    /// Legacy flattened text view retained while callers migrate to `content`.
    pub text: String,
    /// Legacy flattened tool-call view retained while callers migrate to `content`.
    pub tool_calls: Vec<ToolCall>,
    /// The provider tool-call id for a tool result, when applicable.
    pub tool_call_id: Option<ToolCallId>,
    /// The provider tool name for a tool result, when applicable.
    pub tool_name: Option<String>,
    /// Whether a tool result represents a failed execution.
    pub is_error: bool,
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
        let text = text.into();
        Self {
            role,
            content: if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentBlock::Text {
                    text: text.clone(),
                    signature: None,
                }]
            },
            text,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_name: None,
            is_error: false,
            timestamp_ms: now_ms(),
            provider: None,
            model: None,
            usage: None,
            stop_reason: None,
            error_message: None,
        }
    }

    /// Returns ordered content, falling back to the compatibility fields for
    /// messages created by code that has not migrated yet.
    #[must_use]
    pub fn model_content(&self) -> Vec<ContentBlock> {
        let mut content = if self.content.is_empty() {
            let mut content = Vec::new();
            if !self.text.is_empty() {
                content.push(ContentBlock::Text {
                    text: self.text.clone(),
                    signature: None,
                });
            }
            content
        } else {
            self.content.clone()
        };
        for call in &self.tool_calls {
            let already_present = content.iter().any(
                |block| matches!(block, ContentBlock::ToolCall(existing) if existing.id == call.id),
            );
            if !already_present {
                content.push(ContentBlock::ToolCall(call.clone()));
            }
        }
        content
    }

    /// The text of the message: every text block, joined without a separator.
    #[must_use]
    pub fn text_content(&self) -> String {
        if !self.content.is_empty() {
            return self
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
        }
        self.text.clone()
    }

    /// Whether the message requests at least one tool call.
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }

    /// Appends streamed text while keeping both the ordered and legacy views in sync.
    pub fn append_text(&mut self, delta: &str) {
        let _ = self.append_text_block(delta);
    }

    /// Appends streamed text and returns the ordered content-block index it changed.
    pub fn append_text_block(&mut self, delta: &str) -> usize {
        self.text.push_str(delta);
        if matches!(self.content.last(), Some(ContentBlock::Text { .. })) {
            let index = self.content.len() - 1;
            if let ContentBlock::Text { text, .. } = &mut self.content[index] {
                text.push_str(delta);
            }
            index
        } else {
            let index = self.content.len();
            self.content.push(ContentBlock::Text {
                text: delta.to_owned(),
                signature: None,
            });
            index
        }
    }

    /// Starts a streamed thinking block and returns its ordered content-block index.
    pub fn start_thinking(
        &mut self,
        text: impl Into<String>,
        signature: Option<String>,
        redacted: bool,
    ) -> usize {
        let index = self.content.len();
        self.content.push(ContentBlock::Thinking {
            text: text.into(),
            signature,
            redacted,
        });
        index
    }

    /// Appends a delta to an existing thinking block.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidState`] when `index` does not identify a thinking block.
    pub fn append_thinking(&mut self, index: usize, delta: &str) -> Result<(), CoreError> {
        match self.content.get_mut(index) {
            Some(ContentBlock::Thinking { text, .. }) => {
                text.push_str(delta);
                Ok(())
            }
            _ => Err(CoreError::InvalidState {
                message: format!("content block {index} is not a thinking block"),
            }),
        }
    }

    /// Stores the authoritative final state of an existing thinking block.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidState`] when `index` does not identify a thinking block.
    pub fn finish_thinking(
        &mut self,
        index: usize,
        content: impl Into<String>,
        signature: Option<String>,
        redacted: bool,
    ) -> Result<(), CoreError> {
        match self.content.get_mut(index) {
            Some(ContentBlock::Thinking {
                text,
                signature: block_signature,
                redacted: block_redacted,
            }) => {
                *text = content.into();
                *block_signature = signature;
                *block_redacted = redacted;
                Ok(())
            }
            _ => Err(CoreError::InvalidState {
                message: format!("content block {index} is not a thinking block"),
            }),
        }
    }

    /// Appends a streamed tool call while keeping both the ordered and legacy views in sync.
    pub fn append_tool_call(&mut self, call: ToolCall) {
        self.tool_calls.push(call.clone());
        self.content.push(ContentBlock::ToolCall(call));
    }

    /// Creates a model-visible tool result.
    #[must_use]
    pub fn tool_result(
        call_id: ToolCallId,
        tool_name: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        let mut message = Self::text(MessageRole::Tool, text);
        message.tool_call_id = Some(call_id);
        message.tool_name = Some(tool_name.into());
        message.is_error = is_error;
        message
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
    use super::{
        CancellationToken, ContentBlock, CoreError, Message, MessageRole, ModelCostRates,
        SessionId, ToolCall, ToolCallId, Usage,
    };

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

    #[test]
    fn streamed_content_keeps_text_and_tool_calls_in_order() {
        let mut message = Message::text(MessageRole::Assistant, "before");
        message.append_text(" after");
        message.append_tool_call(ToolCall {
            id: ToolCallId::new("call-1").expect("id"),
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        });
        message.append_text(" tail");
        assert_eq!(message.text, "before after tail");
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(
            message.content,
            vec![
                ContentBlock::Text {
                    text: "before after".to_owned(),
                    signature: None,
                },
                ContentBlock::ToolCall(message.tool_calls[0].clone()),
                ContentBlock::Text {
                    text: " tail".to_owned(),
                    signature: None,
                },
            ]
        );
    }

    #[test]
    fn model_cost_rates_calculate_fixed_point_usage_cost() {
        let rates =
            ModelCostRates::from_dollars_per_million(2.0, 4.0, 0.5, 1.0).expect("valid rates");
        let usage = Usage {
            input: 8,
            output: 3,
            reasoning: 1,
            cache_read: 4,
            cache_write: 0,
            total_tokens: 15,
            cost: None,
        };
        assert_eq!(
            rates.calculate(&usage),
            super::UsageCost {
                input_micros: 16,
                output_micros: 12,
                cache_read_micros: 2,
                cache_write_micros: 0,
                total_micros: 30,
            }
        );
    }

    #[test]
    fn streamed_thinking_can_be_updated_and_finalized_in_order() {
        let mut message = Message::text(MessageRole::Assistant, "");
        let thinking_index = message.start_thinking("", None, false);
        message
            .append_thinking(thinking_index, "reason")
            .expect("thinking block");
        message
            .finish_thinking(
                thinking_index,
                "reason",
                Some("signature".to_owned()),
                false,
            )
            .expect("thinking block");
        let text_index = message.append_text_block("answer");

        assert_eq!(thinking_index, 0);
        assert_eq!(text_index, 1);
        assert_eq!(
            message.content,
            vec![
                ContentBlock::Thinking {
                    text: "reason".to_owned(),
                    signature: Some("signature".to_owned()),
                    redacted: false,
                },
                ContentBlock::Text {
                    text: "answer".to_owned(),
                    signature: None,
                },
            ]
        );
    }
}
