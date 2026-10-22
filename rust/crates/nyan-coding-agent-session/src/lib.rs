//! Session identity and persistence boundaries.

use std::path::{Path, PathBuf};

use nyan_coding_agent_core::{AgentMode, CoreError, SessionId};

/// Metadata needed to open a Coding Agent session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDescriptor {
    /// Stable session id.
    pub id: SessionId,
    /// Canonical working directory.
    pub cwd: PathBuf,
    /// Whether the session is persisted.
    pub persistent: bool,
}

impl SessionDescriptor {
    /// Creates a persistent descriptor.
    pub fn persistent(id: SessionId, cwd: impl Into<PathBuf>) -> Self {
        Self {
            id,
            cwd: cwd.into(),
            persistent: true,
        }
    }

    /// Creates an in-memory descriptor.
    pub fn ephemeral(id: SessionId, cwd: impl Into<PathBuf>) -> Self {
        Self {
            id,
            cwd: cwd.into(),
            persistent: false,
        }
    }

    /// Validates the descriptor against the process working directory.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidState`] when the working directory is empty.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.cwd.as_os_str().is_empty() {
            return Err(CoreError::InvalidState {
                message: "session working directory is empty".to_owned(),
            });
        }
        Ok(())
    }
}

/// Lifecycle state owned by a session runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    /// The session has been selected but no runtime is attached.
    Selected,
    /// The session accepts work.
    Open,
    /// The owner is cancelling work and flushing state.
    Closing,
    /// The session no longer accepts work.
    Closed,
}

/// The persisted session format selected by the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionFormat {
    /// The Rust-native versioned format.
    RustNative,
    /// The current durable SQLite format, read during migration.
    DurableSqlite,
    /// The historical JSONL format, read-only during migration.
    LegacyJsonl,
}

/// Inputs needed by a session store implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionOpenRequest {
    /// Session descriptor to open.
    pub descriptor: SessionDescriptor,
    /// Mode that requested the open.
    pub mode: AgentMode,
    /// Optional explicit storage root.
    pub storage_root: Option<PathBuf>,
}

/// A storage-independent session store boundary.
pub trait SessionStore: Send + Sync {
    /// The implementation's error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Opens and locks one session.
    ///
    /// # Errors
    ///
    /// Returns the store's error when the session cannot be opened or locked.
    fn open(&self, request: SessionOpenRequest) -> Result<OpenedSession, Self::Error>;
}

/// An opened session and its ownership metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenedSession {
    /// Descriptor selected by the caller.
    pub descriptor: SessionDescriptor,
    /// Format used by the storage implementation.
    pub format: SessionFormat,
    /// Database or file path when persisted.
    pub path: Option<PathBuf>,
}

impl OpenedSession {
    /// Returns the persisted path if one exists.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionDescriptor, SessionFormat};
    use nyan_coding_agent_core::SessionId;

    #[test]
    fn persistent_descriptor_validates() {
        let descriptor =
            SessionDescriptor::persistent(SessionId::new("session-1").unwrap(), "/tmp/project");
        assert!(descriptor.validate().is_ok());
    }

    #[test]
    fn format_is_explicit() {
        assert_eq!(SessionFormat::LegacyJsonl, SessionFormat::LegacyJsonl);
    }
}
