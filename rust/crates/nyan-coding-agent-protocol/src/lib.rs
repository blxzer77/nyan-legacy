//! Stable names for the Coding Agent's JSONL and RPC protocols.

use std::fmt::Write as _;

use nyan_coding_agent_core::{AgentMode, Message, MessageRole, StopReason, ToolCallId, Usage};
use nyan_coding_agent_runtime::{AgentEvent, AgentOutcome, AssistantEvent};

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

/// Renders tool-call arguments as the JSON object the wire format documents.
///
/// The runtime keeps arguments as the text the provider sent. A valid JSON
/// object is embedded as-is; empty arguments become `{}`; anything else is kept
/// as a string so malformed provider output stays visible instead of vanishing.
fn arguments_json(arguments: &str) -> String {
    if arguments.trim().is_empty() {
        return "{}".to_owned();
    }
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) if value.is_object() => value.to_string(),
        _ => {
            let mut out = String::new();
            push_json_string(&mut out, arguments);
            out
        }
    }
}

fn micros_json(value: u64) -> String {
    format!("{}.{:06}", value / 1_000_000, value % 1_000_000)
}

fn usage_json(usage: Usage) -> String {
    let mut out = format!(
        "{{\"input\":{},\"output\":{},\"reasoning\":{},\"cacheRead\":{},\"cacheWrite\":{},\"totalTokens\":{}}}",
        usage.input,
        usage.output,
        usage.reasoning,
        usage.cache_read,
        usage.cache_write,
        usage.total_tokens
    );
    if let Some(cost) = usage.cost {
        let cost_json = format!(
            "{{\"input\":{},\"output\":{},\"cacheRead\":{},\"cacheWrite\":{},\"total\":{}}}",
            micros_json(cost.input_micros),
            micros_json(cost.output_micros),
            micros_json(cost.cache_read_micros),
            micros_json(cost.cache_write_micros),
            micros_json(cost.total_micros),
        );
        out.pop();
        out.push(',');
        out.push_str("\"cost\":");
        out.push_str(&cost_json);
        out.push('}');
    }
    out
}

const fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
    }
}

/// Encodes a message in the documented wire shape.
///
/// Parity gaps: assistant `usage` omits `cost` (the Rust model registry has no
/// pricing yet), `api` is omitted (the runtime has no API-kind concept), and the
/// JSON message encoder still exposes only the legacy text/tool-call views rather
/// than every ordered image and thinking block.
#[must_use]
pub fn encode_message(message: &Message) -> String {
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
                out.push_str(&arguments_json(&call.arguments));
                out.push('}');
            }
            out.push(']');
            if let Some(provider) = &message.provider {
                out.push_str(",\"provider\":");
                push_json_string(&mut out, provider);
            }
            if let Some(model) = &message.model {
                out.push_str(",\"model\":");
                push_json_string(&mut out, model);
            }
            if let Some(usage) = message.usage {
                out.push_str(",\"usage\":");
                out.push_str(&usage_json(usage));
            }
            if let Some(reason) = message.stop_reason {
                out.push_str(",\"stopReason\":");
                push_json_string(&mut out, stop_reason_name(reason));
            }
            if let Some(error) = &message.error_message {
                out.push_str(",\"errorMessage\":");
                push_json_string(&mut out, error);
            }
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
    let _ = write!(out, ",\"timestamp\":{}}}", message.timestamp_ms);
    out
}

/// Renders Unix milliseconds as an ISO 8601 UTC timestamp with millisecond precision.
#[must_use]
pub fn iso_timestamp(unix_ms: u64) -> String {
    let milliseconds = unix_ms % 1000;
    let seconds = unix_ms / 1000;
    let second_of_day = seconds % 86_400;
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX / 2);
    // Civil-from-days (Howard Hinnant): days since 1970-01-01 to a proleptic Gregorian date.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{milliseconds:03}Z",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

/// Builds the JSON-mode session header record.
#[must_use]
pub fn session_header_jsonl(session_id: &str, timestamp_ms: u64, cwd: &str) -> String {
    JsonlRecord::new("session")
        .raw("version", "3")
        .string("id", session_id)
        .string("timestamp", &iso_timestamp(timestamp_ms))
        .string("cwd", cwd)
        .finish()
}

fn assistant_event_json(event: &AssistantEvent) -> String {
    let mut out = String::new();
    match event {
        AssistantEvent::TextStart { content_index } => {
            let _ = write!(
                out,
                "{{\"type\":\"text_start\",\"contentIndex\":{content_index}}}"
            );
        }
        AssistantEvent::TextDelta {
            content_index,
            delta,
        } => {
            let _ = write!(
                out,
                "{{\"type\":\"text_delta\",\"contentIndex\":{content_index},\"delta\":"
            );
            push_json_string(&mut out, delta);
            out.push('}');
        }
        AssistantEvent::TextEnd {
            content_index,
            content,
        } => {
            let _ = write!(
                out,
                "{{\"type\":\"text_end\",\"contentIndex\":{content_index},\"content\":"
            );
            push_json_string(&mut out, content);
            out.push('}');
        }
        AssistantEvent::ThinkingStart { content_index } => {
            let _ = write!(
                out,
                "{{\"type\":\"thinking_start\",\"contentIndex\":{content_index}}}"
            );
        }
        AssistantEvent::ThinkingDelta {
            content_index,
            delta,
        } => {
            let _ = write!(
                out,
                "{{\"type\":\"thinking_delta\",\"contentIndex\":{content_index},\"delta\":"
            );
            push_json_string(&mut out, delta);
            out.push('}');
        }
        AssistantEvent::ThinkingEnd {
            content_index,
            content,
        } => {
            let _ = write!(
                out,
                "{{\"type\":\"thinking_end\",\"contentIndex\":{content_index},\"content\":"
            );
            push_json_string(&mut out, content);
            out.push('}');
        }
        AssistantEvent::ToolCallStart {
            content_index,
            id,
            name,
        } => {
            let _ = write!(
                out,
                "{{\"type\":\"toolcall_start\",\"contentIndex\":{content_index},\"id\":"
            );
            push_json_string(&mut out, id.as_str());
            out.push_str(",\"toolName\":");
            push_json_string(&mut out, name);
            out.push('}');
        }
        AssistantEvent::ToolCallEnd {
            content_index,
            tool_call,
        } => {
            let _ = write!(
                out,
                "{{\"type\":\"toolcall_end\",\"contentIndex\":{content_index},\"toolCall\":{{\"type\":\"toolCall\",\"id\":"
            );
            push_json_string(&mut out, tool_call.id.as_str());
            out.push_str(",\"name\":");
            push_json_string(&mut out, &tool_call.name);
            out.push_str(",\"arguments\":");
            out.push_str(&arguments_json(&tool_call.arguments));
            out.push_str("}}");
        }
    }
    out
}

/// Maps one runtime event to its JSONL record(s).
///
/// `RetryStart` and `RetryEnd` map to `auto_retry_start` and `auto_retry_end`.
/// `AgentEnd` maps to `agent_end` followed by `agent_settled`, because the Rust
/// runtime has no automatic work that outlives a run.
///
/// Parity gap: `tool_execution_end.durationMs` is not reported because the
/// runtime does not time tool execution yet.
#[must_use]
pub fn event_to_jsonl(event: &AgentEvent) -> String {
    match event {
        AgentEvent::AgentStart => JsonlRecord::new("agent_start").finish(),
        AgentEvent::TurnStart { .. } => JsonlRecord::new("turn_start").finish(),
        AgentEvent::MessageStart { message } => JsonlRecord::new("message_start")
            .raw("message", &encode_message(message))
            .finish(),
        AgentEvent::MessageEnd { message } => JsonlRecord::new("message_end")
            .raw("message", &encode_message(message))
            .finish(),
        AgentEvent::MessageUpdate { message, event } => {
            let usage = message.usage.map_or_else(
                || {
                    "{\"input\":0,\"output\":0,\"reasoning\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":0}"
                        .to_owned()
                },
                usage_json,
            );
            JsonlRecord::new("message_update")
                .raw("usage", &usage)
                .raw("assistantMessageEvent", &assistant_event_json(event))
                .finish()
        }
        AgentEvent::RetryStart { .. } | AgentEvent::RetryEnd { .. } => retry_event_jsonl(event),
        AgentEvent::ThinkingLevelChanged { level } => JsonlRecord::new("thinking_level_changed")
            .string("thinkingLevel", level)
            .finish(),
        AgentEvent::QueueUpdate {
            steering,
            follow_up,
        } => {
            let steering = steering
                .iter()
                .map(encode_message)
                .collect::<Vec<_>>()
                .join(",");
            let follow_up = follow_up
                .iter()
                .map(encode_message)
                .collect::<Vec<_>>()
                .join(",");
            JsonlRecord::new("queue_update")
                .raw("steering", &format!("[{steering}]"))
                .raw("followUp", &format!("[{follow_up}]"))
                .finish()
        }
        AgentEvent::ToolExecutionStart { .. }
        | AgentEvent::ToolExecutionUpdate { .. }
        | AgentEvent::ToolExecutionEnd { .. } => tool_event_jsonl(event),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => {
            let results = tool_results
                .iter()
                .map(encode_message)
                .collect::<Vec<_>>()
                .join(",");
            JsonlRecord::new("turn_end")
                .raw("message", &encode_message(message))
                .raw("toolResults", &format!("[{results}]"))
                .finish()
        }
        AgentEvent::AgentEnd { outcome, messages } => {
            let aborted = matches!(outcome, AgentOutcome::Aborted);
            let generated = messages
                .iter()
                .map(encode_message)
                .collect::<Vec<_>>()
                .join(",");
            let mut records = JsonlRecord::new("agent_end")
                .raw("messages", &format!("[{generated}]"))
                .boolean("willRetry", false)
                .finish();
            records.push_str(
                &JsonlRecord::new("agent_settled")
                    .boolean("aborted", aborted)
                    .finish(),
            );
            records
        }
    }
}

fn retry_event_jsonl(event: &AgentEvent) -> String {
    match event {
        AgentEvent::RetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => JsonlRecord::new("auto_retry_start")
            .raw("attempt", &attempt.to_string())
            .raw("maxAttempts", &max_attempts.to_string())
            .raw("delayMs", &delay_ms.to_string())
            .string("errorMessage", error_message)
            .finish(),
        AgentEvent::RetryEnd {
            attempt,
            succeeded,
            final_error,
        } => {
            let mut record = JsonlRecord::new("auto_retry_end")
                .boolean("success", *succeeded)
                .raw("attempt", &attempt.to_string());
            if let Some(error) = final_error {
                record = record.string("finalError", error);
            }
            record.finish()
        }
        _ => unreachable!("retry_event_jsonl is only called for its own events"),
    }
}

fn tool_event_jsonl(event: &AgentEvent) -> String {
    match event {
        AgentEvent::ToolExecutionStart {
            call_id,
            tool_name,
            arguments,
        } => JsonlRecord::new("tool_execution_start")
            .string("toolCallId", call_id.as_str())
            .string("toolName", tool_name)
            .raw("args", &arguments_json(arguments))
            .finish(),
        AgentEvent::ToolExecutionUpdate {
            call_id,
            tool_name,
            arguments,
            partial_result,
        } => JsonlRecord::new("tool_execution_update")
            .string("toolCallId", call_id.as_str())
            .string("toolName", tool_name)
            .raw("args", &arguments_json(arguments))
            .raw("partialResult", &encode_tool_result(&partial_result.text))
            .finish(),
        AgentEvent::ToolExecutionEnd {
            call_id,
            tool_name,
            result,
            duration_ms,
        } => {
            let mut record = JsonlRecord::new("tool_execution_end")
                .string("toolCallId", call_id.as_str())
                .string("toolName", tool_name)
                .raw("result", &encode_tool_result(&result.text))
                .boolean("isError", result.is_error);
            if let Some(duration) = duration_ms {
                record = record.raw("durationMs", &duration.to_string());
            }
            record.finish()
        }
        _ => unreachable!("tool_event_jsonl is only called for its own events"),
    }
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
        event_to_jsonl, iso_timestamp, push_json_string, session_header_jsonl,
    };
    use nyan_coding_agent_core::Message;

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

    fn stamped(mut message: Message, timestamp_ms: u64) -> Message {
        message.timestamp_ms = timestamp_ms;
        message
    }

    #[test]
    fn user_and_tool_messages_use_the_documented_shape() {
        use nyan_coding_agent_core::{MessageRole, ToolCallId};
        use nyan_coding_agent_runtime::AgentEvent;

        assert_eq!(
            event_to_jsonl(&AgentEvent::AgentStart),
            "{\"type\":\"agent_start\"}\n"
        );
        let user = stamped(Message::text(MessageRole::User, "hi\n"), 1_733_234_401_000);
        assert_eq!(
            event_to_jsonl(&AgentEvent::MessageEnd { message: user }),
            concat!(
                "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",",
                "\"content\":\"hi\\n\",\"timestamp\":1733234401000}}\n"
            )
        );
        let tool_result = stamped(
            Message::tool_result(
                ToolCallId::new("call-1").expect("tool call id"),
                "echo",
                "out",
                true,
            ),
            7,
        );
        assert_eq!(
            encode_message(&tool_result),
            concat!(
                "{\"role\":\"toolResult\",\"toolCallId\":\"call-1\",\"toolName\":\"echo\",",
                "\"content\":[{\"type\":\"text\",\"text\":\"out\"}],\"isError\":true,",
                "\"timestamp\":7}"
            )
        );
    }

    #[test]
    fn assistant_messages_carry_provider_usage_and_stop_reason() {
        use nyan_coding_agent_core::{
            MessageRole, StopReason, ToolCall, ToolCallId, Usage, UsageCost,
        };

        let mut message = Message::text(MessageRole::Assistant, "done");
        message.timestamp_ms = 9;
        message.provider = Some("openai".to_owned());
        message.model = Some("gpt-test".to_owned());
        message.usage = Some(Usage {
            input: 10,
            output: 2,
            reasoning: 1,
            cache_read: 1,
            cost: Some(UsageCost {
                input_micros: 16,
                output_micros: 12,
                cache_read_micros: 2,
                cache_write_micros: 0,
                total_micros: 30,
            }),
            cache_write: 0,
            total_tokens: 12,
        });
        message.stop_reason = Some(StopReason::ToolUse);
        message.tool_calls = vec![ToolCall {
            id: ToolCallId::new("c1").expect("id"),
            name: "bash".to_owned(),
            arguments: "{\"command\":\"ls\"}".to_owned(),
        }];
        assert_eq!(
            encode_message(&message),
            concat!(
                "{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"},",
                "{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"bash\",\"arguments\":{\"command\":\"ls\"}}],",
                "\"provider\":\"openai\",\"model\":\"gpt-test\",",
                "\"usage\":{\"input\":10,\"output\":2,\"reasoning\":1,\"cacheRead\":1,\"cacheWrite\":0,\"totalTokens\":12,\"cost\":{\"input\":0.000016,\"output\":0.000012,\"cacheRead\":0.000002,\"cacheWrite\":0.000000,\"total\":0.000030}},",
                "\"stopReason\":\"toolUse\",\"timestamp\":9}"
            )
        );
    }

    #[test]
    fn tool_arguments_that_are_not_an_object_stay_visible() {
        use nyan_coding_agent_core::{MessageRole, ToolCall, ToolCallId};

        let mut message = Message::text(MessageRole::Assistant, "");
        message.timestamp_ms = 1;
        message.tool_calls = ["", "not json", "[1]"]
            .iter()
            .enumerate()
            .map(|(index, arguments)| ToolCall {
                id: ToolCallId::new(format!("c{index}")).expect("id"),
                name: "t".to_owned(),
                arguments: (*arguments).to_owned(),
            })
            .collect();
        let encoded = encode_message(&message);
        assert!(encoded.contains("\"arguments\":{}"), "{encoded}");
        assert!(encoded.contains("\"arguments\":\"not json\""), "{encoded}");
        assert!(encoded.contains("\"arguments\":\"[1]\""), "{encoded}");
    }

    #[test]
    fn streaming_updates_are_delta_only() {
        use nyan_coding_agent_core::{MessageRole, ToolCall, ToolCallId};
        use nyan_coding_agent_runtime::{AgentEvent, AssistantEvent};

        let message = Message::text(MessageRole::Assistant, "Hello");
        let update = |event| {
            event_to_jsonl(&AgentEvent::MessageUpdate {
                message: message.clone(),
                event,
            })
        };
        let zero_usage = "\"usage\":{\"input\":0,\"output\":0,\"reasoning\":0,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":0}";
        assert_eq!(
            update(AssistantEvent::TextDelta {
                content_index: 0,
                delta: "Hel\"lo".to_owned()
            }),
            format!(
                "{{\"type\":\"message_update\",{zero_usage},\"assistantMessageEvent\":{{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Hel\\\"lo\"}}}}\n"
            )
        );
        let thinking = update(AssistantEvent::ThinkingEnd {
            content_index: 0,
            content: "reason".to_owned(),
        });
        assert!(thinking.contains(
            "\"assistantMessageEvent\":{\"type\":\"thinking_end\",\"contentIndex\":0,\"content\":\"reason\"}"
        ));
        let ended = update(AssistantEvent::ToolCallEnd {
            content_index: 1,
            tool_call: ToolCall {
                id: ToolCallId::new("c1").expect("id"),
                name: "bash".to_owned(),
                arguments: "{\"a\":1}".to_owned(),
            },
        });
        assert!(ended.contains(
            "\"assistantMessageEvent\":{\"type\":\"toolcall_end\",\"contentIndex\":1,\"toolCall\":{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"bash\",\"arguments\":{\"a\":1}}}"
        ));
        assert!(
            !ended.contains("\"message\""),
            "wire updates omit the cumulative message"
        );
    }

    #[test]
    fn thinking_level_changes_encode_the_selected_level() {
        use nyan_coding_agent_runtime::AgentEvent;

        assert_eq!(
            event_to_jsonl(&AgentEvent::ThinkingLevelChanged {
                level: "high".to_owned(),
            }),
            "{\"type\":\"thinking_level_changed\",\"thinkingLevel\":\"high\"}\n"
        );
    }

    #[test]
    fn queue_updates_encode_pending_messages() {
        use nyan_coding_agent_core::MessageRole;
        use nyan_coding_agent_runtime::AgentEvent;

        let record = event_to_jsonl(&AgentEvent::QueueUpdate {
            steering: vec![Message::text(MessageRole::User, "steer")],
            follow_up: vec![Message::text(MessageRole::User, "later")],
        });
        assert!(record.starts_with("{\"type\":\"queue_update\",\"steering\":[{"));
        assert!(record.contains("\"content\":\"steer\""));
        assert!(record.contains("\"followUp\":[{"));
        assert!(record.contains("\"content\":\"later\""));
    }

    #[test]
    fn agent_end_lists_generated_messages_and_settles() {
        use nyan_coding_agent_core::MessageRole;
        use nyan_coding_agent_runtime::{AgentEvent, AgentOutcome};

        let generated = stamped(Message::text(MessageRole::User, "go"), 5);
        let end = event_to_jsonl(&AgentEvent::AgentEnd {
            outcome: AgentOutcome::Aborted,
            messages: vec![generated],
        });
        let mut lines = end.lines();
        assert_eq!(
            lines.next(),
            Some(concat!(
                "{\"type\":\"agent_end\",\"messages\":[{\"role\":\"user\",\"content\":\"go\",\"timestamp\":5}],",
                "\"willRetry\":false}"
            ))
        );
        assert_eq!(
            lines.next(),
            Some("{\"type\":\"agent_settled\",\"aborted\":true}")
        );
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn retry_events_carry_the_documented_fields() {
        use nyan_coding_agent_runtime::AgentEvent;

        assert_eq!(
            event_to_jsonl(&AgentEvent::RetryStart {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 2000,
                error_message: "529 \"overloaded\"".to_owned(),
            }),
            concat!(
                "{\"type\":\"auto_retry_start\",\"attempt\":1,\"maxAttempts\":3,",
                "\"delayMs\":2000,\"errorMessage\":\"529 \\\"overloaded\\\"\"}\n"
            )
        );
        assert_eq!(
            event_to_jsonl(&AgentEvent::RetryEnd {
                attempt: 2,
                succeeded: true,
                final_error: None,
            }),
            "{\"type\":\"auto_retry_end\",\"success\":true,\"attempt\":2}\n"
        );
        assert_eq!(
            event_to_jsonl(&AgentEvent::RetryEnd {
                attempt: 3,
                succeeded: false,
                final_error: Some("gave up".to_owned()),
            }),
            concat!(
                "{\"type\":\"auto_retry_end\",\"success\":false,\"attempt\":3,",
                "\"finalError\":\"gave up\"}\n"
            )
        );
    }

    #[test]
    fn tool_events_carry_args_and_the_duration_only_when_the_tool_ran() {
        use nyan_coding_agent_core::{ToolCallId, ToolResult};
        use nyan_coding_agent_runtime::AgentEvent;

        let call_id = ToolCallId::new("c1").expect("id");
        let update = event_to_jsonl(&AgentEvent::ToolExecutionUpdate {
            call_id: call_id.clone(),
            tool_name: "bash".to_owned(),
            arguments: "{\"command\":\"ls\"}".to_owned(),
            partial_result: ToolResult {
                text: "part".to_owned(),
                is_error: false,
            },
        });
        assert_eq!(
            update,
            concat!(
                "{\"type\":\"tool_execution_update\",\"toolCallId\":\"c1\",\"toolName\":\"bash\",",
                "\"args\":{\"command\":\"ls\"},",
                "\"partialResult\":{\"content\":[{\"type\":\"text\",\"text\":\"part\"}],\"details\":{}}}\n"
            )
        );
        let result = Message::tool_result(call_id, "bash", "done", false);
        for (duration_ms, tail) in [(Some(42), ",\"durationMs\":42}\n"), (None, "}\n")] {
            let end = event_to_jsonl(&AgentEvent::ToolExecutionEnd {
                call_id: ToolCallId::new("c1").expect("id"),
                tool_name: "bash".to_owned(),
                result: result.clone(),
                duration_ms,
            });
            let expected = format!(
                "{{\"type\":\"tool_execution_end\",\"toolCallId\":\"c1\",\"toolName\":\"bash\",\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}],\"details\":{{}}}},\"isError\":false{tail}"
            );
            assert_eq!(end, expected);
        }
    }

    #[test]
    fn timestamps_render_as_iso_8601_utc() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_timestamp(1_733_234_401_000), "2024-12-03T14:00:01.000Z");
        assert_eq!(iso_timestamp(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(iso_timestamp(4_102_444_799_999), "2099-12-31T23:59:59.999Z");
    }

    #[test]
    fn session_header_matches_the_documented_record() {
        assert_eq!(
            session_header_jsonl("uuid", 1_733_234_401_000, "/path"),
            "{\"type\":\"session\",\"version\":3,\"id\":\"uuid\",\"timestamp\":\"2024-12-03T14:00:01.000Z\",\"cwd\":\"/path\"}\n"
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
