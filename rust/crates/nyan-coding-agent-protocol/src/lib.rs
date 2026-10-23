//! Stable names for the Coding Agent's JSONL and RPC protocols.

use std::fmt::Write as _;

use nyan_coding_agent_core::{AgentMode, Message, MessageRole, ToolCallId};
use nyan_coding_agent_runtime::{AgentEvent, AgentOutcome};

/// The JSONL-facing runtime mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolMode {
    /// Human-oriented terminal mode is not a wire protocol.
    Text,
    /// One-shot event stream.
    Json,
    /// Long-lived command/event stream.
    Rpc,
}

impl From<ProtocolMode> for AgentMode {
    fn from(mode: ProtocolMode) -> Self {
        match mode {
            ProtocolMode::Text => Self::Print,
            ProtocolMode::Json => Self::Json,
            ProtocolMode::Rpc => Self::Rpc,
        }
    }
}

/// RPC command names accepted by the current TypeScript CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RpcCommandName {
    /// Submit a prompt.
    Prompt,
    /// Submit steering input.
    Steer,
    /// Submit follow-up input.
    FollowUp,
    /// Abort active work.
    Abort,
    /// Clear queued inputs.
    ClearQueue,
    /// Start a session.
    NewSession,
    /// Inspect state.
    GetState,
    /// Select a model.
    SetModel,
    /// Cycle models.
    CycleModel,
    /// List models.
    GetAvailableModels,
    /// Set thinking level.
    SetThinkingLevel,
    /// Cycle thinking levels.
    CycleThinkingLevel,
    /// List thinking levels.
    GetAvailableThinkingLevels,
    /// Set steering queue mode.
    SetSteeringMode,
    /// Set follow-up queue mode.
    SetFollowUpMode,
    /// Compact context.
    Compact,
    /// Enable or disable auto-compaction.
    SetAutoCompaction,
    /// Enable or disable auto-retry.
    SetAutoRetry,
    /// Stop a retry delay.
    AbortRetry,
    /// Run bash.
    Bash,
    /// Stop bash.
    AbortBash,
    /// Read session statistics.
    GetSessionStats,
    /// Export HTML.
    ExportHtml,
    /// Switch session.
    SwitchSession,
    /// Fork a conversation.
    Fork,
    /// Clone a conversation.
    Clone,
    /// List forkable messages.
    GetForkMessages,
    /// Read session entries.
    GetEntries,
    /// Read the active tree.
    GetTree,
    /// Read the last assistant text.
    GetLastAssistantText,
    /// Set the session name.
    SetSessionName,
    /// List commands.
    GetCommands,
    /// Read messages.
    GetMessages,
}

impl RpcCommandName {
    /// Returns the exact wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
            Self::Abort => "abort",
            Self::ClearQueue => "clear_queue",
            Self::NewSession => "new_session",
            Self::GetState => "get_state",
            Self::SetModel => "set_model",
            Self::CycleModel => "cycle_model",
            Self::GetAvailableModels => "get_available_models",
            Self::SetThinkingLevel => "set_thinking_level",
            Self::CycleThinkingLevel => "cycle_thinking_level",
            Self::GetAvailableThinkingLevels => "get_available_thinking_levels",
            Self::SetSteeringMode => "set_steering_mode",
            Self::SetFollowUpMode => "set_follow_up_mode",
            Self::Compact => "compact",
            Self::SetAutoCompaction => "set_auto_compaction",
            Self::SetAutoRetry => "set_auto_retry",
            Self::AbortRetry => "abort_retry",
            Self::Bash => "bash",
            Self::AbortBash => "abort_bash",
            Self::GetSessionStats => "get_session_stats",
            Self::ExportHtml => "export_html",
            Self::SwitchSession => "switch_session",
            Self::Fork => "fork",
            Self::Clone => "clone",
            Self::GetForkMessages => "get_fork_messages",
            Self::GetEntries => "get_entries",
            Self::GetTree => "get_tree",
            Self::GetLastAssistantText => "get_last_assistant_text",
            Self::SetSessionName => "set_session_name",
            Self::GetCommands => "get_commands",
            Self::GetMessages => "get_messages",
        }
    }

    /// Returns every command in the documented order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Prompt,
            Self::Steer,
            Self::FollowUp,
            Self::Abort,
            Self::ClearQueue,
            Self::NewSession,
            Self::GetState,
            Self::SetModel,
            Self::CycleModel,
            Self::GetAvailableModels,
            Self::SetThinkingLevel,
            Self::CycleThinkingLevel,
            Self::GetAvailableThinkingLevels,
            Self::SetSteeringMode,
            Self::SetFollowUpMode,
            Self::Compact,
            Self::SetAutoCompaction,
            Self::SetAutoRetry,
            Self::AbortRetry,
            Self::Bash,
            Self::AbortBash,
            Self::GetSessionStats,
            Self::ExportHtml,
            Self::SwitchSession,
            Self::Fork,
            Self::Clone,
            Self::GetForkMessages,
            Self::GetEntries,
            Self::GetTree,
            Self::GetLastAssistantText,
            Self::SetSessionName,
            Self::GetCommands,
            Self::GetMessages,
        ]
    }
}

/// Event names shared by the JSON and RPC session event streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEventName {
    /// Agent started.
    AgentStart,
    /// Low-level agent run ended.
    AgentEnd,
    /// Automatic work settled.
    AgentSettled,
    /// Assistant turn started.
    TurnStart,
    /// Assistant turn ended.
    TurnEnd,
    /// Message started.
    MessageStart,
    /// Streaming update.
    MessageUpdate,
    /// Message ended.
    MessageEnd,
    /// Tool started.
    ToolExecutionStart,
    /// Tool partial update.
    ToolExecutionUpdate,
    /// Tool ended.
    ToolExecutionEnd,
    /// Queues changed.
    QueueUpdate,
    /// Custom entry appended.
    EntryAppended,
    /// Session metadata changed.
    SessionInfoChanged,
    /// Thinking level changed.
    ThinkingLevelChanged,
    /// Compaction started.
    CompactionStart,
    /// Compaction ended.
    CompactionEnd,
    /// Model retry started.
    AutoRetryStart,
    /// Model retry ended.
    AutoRetryEnd,
    /// Summary retry scheduled.
    SummarizationRetryScheduled,
    /// Summary retry attempt started.
    SummarizationRetryAttemptStart,
    /// Summary retry finished.
    SummarizationRetryFinished,
    /// Direct bash output chunk.
    BashExecutionUpdate,
}

impl SessionEventName {
    /// Returns the exact wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentStart => "agent_start",
            Self::AgentEnd => "agent_end",
            Self::AgentSettled => "agent_settled",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::MessageStart => "message_start",
            Self::MessageUpdate => "message_update",
            Self::MessageEnd => "message_end",
            Self::ToolExecutionStart => "tool_execution_start",
            Self::ToolExecutionUpdate => "tool_execution_update",
            Self::ToolExecutionEnd => "tool_execution_end",
            Self::QueueUpdate => "queue_update",
            Self::EntryAppended => "entry_appended",
            Self::SessionInfoChanged => "session_info_changed",
            Self::ThinkingLevelChanged => "thinking_level_changed",
            Self::CompactionStart => "compaction_start",
            Self::CompactionEnd => "compaction_end",
            Self::AutoRetryStart => "auto_retry_start",
            Self::AutoRetryEnd => "auto_retry_end",
            Self::SummarizationRetryScheduled => "summarization_retry_scheduled",
            Self::SummarizationRetryAttemptStart => "summarization_retry_attempt_start",
            Self::SummarizationRetryFinished => "summarization_retry_finished",
            Self::BashExecutionUpdate => "bash_execution_update",
        }
    }

    /// Returns every documented session event name.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::AgentStart,
            Self::AgentEnd,
            Self::AgentSettled,
            Self::TurnStart,
            Self::TurnEnd,
            Self::MessageStart,
            Self::MessageUpdate,
            Self::MessageEnd,
            Self::ToolExecutionStart,
            Self::ToolExecutionUpdate,
            Self::ToolExecutionEnd,
            Self::QueueUpdate,
            Self::EntryAppended,
            Self::SessionInfoChanged,
            Self::ThinkingLevelChanged,
            Self::CompactionStart,
            Self::CompactionEnd,
            Self::AutoRetryStart,
            Self::AutoRetryEnd,
            Self::SummarizationRetryScheduled,
            Self::SummarizationRetryAttemptStart,
            Self::SummarizationRetryFinished,
            Self::BashExecutionUpdate,
        ]
    }
}

/// Appends `value` to `out` as a JSON string literal.
///
/// Only the characters JSON requires are escaped. U+2028 and U+2029 are left
/// as-is: the Coding Agent's JSONL framing splits records on LF only, and
/// those separators are valid inside strings.
pub fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if u32::from(control) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Builds one strict JSONL record: a single JSON object terminated by LF.
#[derive(Debug)]
pub struct JsonlRecord {
    buffer: String,
    fields: usize,
}

impl JsonlRecord {
    /// Starts a record whose first field is `"type"`.
    #[must_use]
    pub fn new(record_type: &str) -> Self {
        let mut buffer = String::from("{\"type\":");
        push_json_string(&mut buffer, record_type);
        Self { buffer, fields: 1 }
    }

    /// Adds a string field.
    #[must_use]
    pub fn string(mut self, name: &str, value: &str) -> Self {
        self.key(name);
        push_json_string(&mut self.buffer, value);
        self
    }

    /// Adds a boolean field.
    #[must_use]
    pub fn boolean(mut self, name: &str, value: bool) -> Self {
        self.key(name);
        self.buffer.push_str(if value { "true" } else { "false" });
        self
    }

    /// Adds a field whose value is already valid JSON.
    ///
    /// The caller owns the validity of `json`; use it only for values built by
    /// this module's own encoders.
    #[must_use]
    pub fn raw(mut self, name: &str, json: &str) -> Self {
        self.key(name);
        self.buffer.push_str(json);
        self
    }

    fn key(&mut self, name: &str) {
        debug_assert!(self.fields > 0);
        self.buffer.push(',');
        push_json_string(&mut self.buffer, name);
        self.buffer.push(':');
        self.fields += 1;
    }

    /// Finishes the record, returning it with its terminating LF.
    #[must_use]
    pub fn finish(mut self) -> String {
        self.buffer.push_str("}\n");
        self.buffer
    }
}

/// Encodes a message with the fields the Rust runtime currently carries.
///
/// Parity gap: the documented wire message also carries `usage`, `api`,
/// `provider`, `model`, and `stopReason` on assistant messages. The Rust
/// `Message` does not hold those yet, so they are omitted rather than invented.
/// `timestamp` is supplied by the caller in Unix milliseconds.
#[must_use]
pub fn encode_message(message: &Message, timestamp_ms: u64) -> String {
    let mut out = String::from("{\"role\":");
    match message.role {
        MessageRole::System => {
            push_json_string(&mut out, "system");
            out.push_str(",\"content\":");
            push_json_string(&mut out, &message.text);
        }
        MessageRole::User => {
            push_json_string(&mut out, "user");
            out.push_str(",\"content\":");
            push_json_string(&mut out, &message.text);
        }
        MessageRole::Assistant => {
            push_json_string(&mut out, "assistant");
            out.push_str(",\"content\":[");
            let mut first = true;
            if !message.text.is_empty() {
                out.push_str("{\"type\":\"text\",\"text\":");
                push_json_string(&mut out, &message.text);
                out.push('}');
                first = false;
            }
            for call in &message.tool_calls {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str("{\"type\":\"toolCall\",\"id\":");
                push_json_string(&mut out, call.id.as_str());
                out.push_str(",\"name\":");
                push_json_string(&mut out, &call.name);
                out.push_str(",\"arguments\":");
                push_json_string(&mut out, &call.arguments);
                out.push('}');
            }
            out.push(']');
        }
        MessageRole::Tool => {
            push_json_string(&mut out, "toolResult");
            out.push_str(",\"toolCallId\":");
            push_json_string(
                &mut out,
                message.tool_call_id.as_ref().map_or("", ToolCallId::as_str),
            );
            out.push_str(",\"toolName\":");
            push_json_string(&mut out, message.tool_name.as_deref().unwrap_or(""));
            out.push_str(",\"content\":[{\"type\":\"text\",\"text\":");
            push_json_string(&mut out, &message.text);
            out.push_str("}],\"isError\":");
            out.push_str(if message.is_error { "true" } else { "false" });
        }
    }
    let _ = write!(out, ",\"timestamp\":{timestamp_ms}}}");
    out
}

/// Maps one runtime event to its JSONL record, or `None` when the event has no
/// documented wire form yet.
///
/// `RetryStart` and `RetryEnd` map to `auto_retry_start` and `auto_retry_end`.
/// `AgentEnd` maps to `agent_end` followed by `agent_settled`, because the Rust
/// runtime has no automatic work that outlives a run.
#[must_use]
pub fn event_to_jsonl(event: &AgentEvent, timestamp_ms: u64) -> Option<String> {
    Some(match event {
        AgentEvent::AgentStart => JsonlRecord::new("agent_start").finish(),
        AgentEvent::TurnStart { .. } => JsonlRecord::new("turn_start").finish(),
        AgentEvent::MessageStart { message } => JsonlRecord::new("message_start")
            .raw("message", &encode_message(message, timestamp_ms))
            .finish(),
        AgentEvent::MessageEnd { message } => JsonlRecord::new("message_end")
            .raw("message", &encode_message(message, timestamp_ms))
            .finish(),
        AgentEvent::MessageUpdate { .. } => return None,
        AgentEvent::RetryStart { attempt } => JsonlRecord::new("auto_retry_start")
            .raw("attempt", &attempt.to_string())
            .finish(),
        AgentEvent::RetryEnd { attempt, succeeded } => JsonlRecord::new("auto_retry_end")
            .boolean("success", *succeeded)
            .raw("attempt", &attempt.to_string())
            .finish(),
        AgentEvent::ToolExecutionStart {
            call_id,
            tool_name,
            arguments,
        } => JsonlRecord::new("tool_execution_start")
            .string("toolCallId", call_id.as_str())
            .string("toolName", tool_name)
            .string("args", arguments)
            .finish(),
        AgentEvent::ToolExecutionUpdate {
            call_id,
            tool_name,
            partial_result,
        } => JsonlRecord::new("tool_execution_update")
            .string("toolCallId", call_id.as_str())
            .string("toolName", tool_name)
            .raw("partialResult", &encode_tool_result(&partial_result.text))
            .finish(),
        AgentEvent::ToolExecutionEnd {
            call_id,
            tool_name,
            result,
        } => JsonlRecord::new("tool_execution_end")
            .string("toolCallId", call_id.as_str())
            .string("toolName", tool_name)
            .raw("result", &encode_tool_result(&result.text))
            .boolean("isError", result.is_error)
            .finish(),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => {
            let results = tool_results
                .iter()
                .map(|result| encode_message(result, timestamp_ms))
                .collect::<Vec<_>>()
                .join(",");
            JsonlRecord::new("turn_end")
                .raw("message", &encode_message(message, timestamp_ms))
                .raw("toolResults", &format!("[{results}]"))
                .finish()
        }
        AgentEvent::AgentEnd { outcome } => {
            let aborted = matches!(outcome, AgentOutcome::Aborted);
            let mut records = JsonlRecord::new("agent_end")
                .raw("messages", "[]")
                .boolean("willRetry", false)
                .finish();
            records.push_str(
                &JsonlRecord::new("agent_settled")
                    .boolean("aborted", aborted)
                    .finish(),
            );
            records
        }
    })
}

/// Encodes a tool result in the documented `{content, details}` wire shape.
#[must_use]
pub fn encode_tool_result(text: &str) -> String {
    let mut out = String::from("{\"content\":[{\"type\":\"text\",\"text\":");
    push_json_string(&mut out, text);
    out.push_str("}],\"details\":{}}");
    out
}

#[cfg(test)]
mod tests {
    use super::{
        JsonlRecord, RpcCommandName, SessionEventName, encode_message, encode_tool_result,
        event_to_jsonl, push_json_string,
    };

    fn encoded(value: &str) -> String {
        let mut out = String::new();
        push_json_string(&mut out, value);
        out
    }

    #[test]
    fn json_strings_escape_exactly_what_json_requires() {
        assert_eq!(encoded("plain"), "\"plain\"");
        assert_eq!(encoded("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(encoded("line\nbreak\r\ttab"), "\"line\\nbreak\\r\\ttab\"");
        assert_eq!(encoded("\u{01}\u{1f}"), "\"\\u0001\\u001f\"");
        assert_eq!(encoded("\u{08}\u{0c}"), "\"\\b\\f\"");
    }

    #[test]
    fn unicode_line_separators_stay_inside_the_record() {
        let record = JsonlRecord::new("message_end")
            .string("text", "a\u{2028}b\u{2029}c")
            .finish();
        assert_eq!(record.matches('\n').count(), 1);
        assert!(record.ends_with("}\n"));
        assert!(record.contains("a\u{2028}b\u{2029}c"));
    }

    #[test]
    fn records_put_type_first_and_end_with_one_lf() {
        let record = JsonlRecord::new("tool_execution_end")
            .string("toolCallId", "call-1")
            .boolean("isError", false)
            .raw("result", &encode_tool_result("out\nput"))
            .finish();
        assert_eq!(
            record,
            concat!(
                "{\"type\":\"tool_execution_end\",\"toolCallId\":\"call-1\",",
                "\"isError\":false,\"result\":{\"content\":[{\"type\":\"text\",",
                "\"text\":\"out\\nput\"}],\"details\":{}}}\n"
            )
        );
    }

    #[test]
    fn run_events_map_to_documented_records() {
        use nyan_coding_agent_core::{Message, MessageRole, ToolCallId};
        use nyan_coding_agent_runtime::{AgentEvent, AgentOutcome};

        assert_eq!(
            event_to_jsonl(&AgentEvent::AgentStart, 1).as_deref(),
            Some("{\"type\":\"agent_start\"}\n")
        );
        assert_eq!(
            event_to_jsonl(
                &AgentEvent::MessageEnd {
                    message: Message::text(MessageRole::User, "hi\n")
                },
                1_733_234_401_000
            )
            .as_deref(),
            Some(concat!(
                "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",",
                "\"content\":\"hi\\n\",\"timestamp\":1733234401000}}\n"
            ))
        );
        let tool_result = Message::tool_result(
            ToolCallId::new("call-1").expect("tool call id"),
            "echo",
            "out",
            true,
        );
        let encoded = encode_message(&tool_result, 7);
        assert_eq!(
            encoded,
            concat!(
                "{\"role\":\"toolResult\",\"toolCallId\":\"call-1\",\"toolName\":\"echo\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"out\"}],\"isError\":true,",
                "\"timestamp\":7}"
            )
        );
        let end = event_to_jsonl(
            &AgentEvent::AgentEnd {
                outcome: AgentOutcome::Aborted,
            },
            1,
        )
        .expect("agent end");
        assert_eq!(end.lines().count(), 2);
        assert!(
            end.lines()
                .nth(1)
                .expect("settled line")
                .contains("\"aborted\":true")
        );
        assert!(
            event_to_jsonl(
                &AgentEvent::MessageUpdate {
                    message: Message::text(MessageRole::Assistant, "x")
                },
                1
            )
            .is_none()
        );
    }

    #[test]
    fn field_free_events_are_a_bare_type_object() {
        assert_eq!(
            JsonlRecord::new("agent_start").finish(),
            "{\"type\":\"agent_start\"}\n"
        );
    }

    #[test]
    fn rpc_names_match_the_frozen_matrix() {
        assert_eq!(RpcCommandName::all().len(), 33);
        assert_eq!(RpcCommandName::GetMessages.as_str(), "get_messages");
    }

    #[test]
    fn session_event_names_match_the_frozen_matrix() {
        assert_eq!(SessionEventName::all().len(), 23);
        assert_eq!(SessionEventName::MessageUpdate.as_str(), "message_update");
    }
}
