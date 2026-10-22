//! Stable names for the Coding Agent's JSONL and RPC protocols.

use nyan_coding_agent_core::AgentMode;

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

#[cfg(test)]
mod tests {
    use super::{RpcCommandName, SessionEventName};

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
