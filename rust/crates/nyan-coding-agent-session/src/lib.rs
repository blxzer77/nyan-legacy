//! Session identity and persistence boundaries.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nyan_coding_agent_core::{
    AgentMode, CoreError, Message, MessageRole, SessionId, StopReason, ToolCall, ToolCallId, Usage,
};

const RUST_SESSION_HEADER: &[u8] = b"NYANRS01\n1\n";

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

/// A persisted journal record category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionRecordKind {
    /// Session metadata or configuration.
    Metadata,
    /// User input.
    UserMessage,
    /// Assistant output.
    AssistantMessage,
    /// Tool output.
    ToolResult,
    /// Run admission marker.
    RunStarted,
    /// Run terminal marker.
    RunFinished,
    /// The system prompt the session was started with.
    SystemPrompt,
    /// The session's display name; the last one wins.
    SessionName,
}

impl SessionRecordKind {
    const fn code(self) -> u8 {
        match self {
            Self::Metadata => 1,
            Self::UserMessage => 2,
            Self::AssistantMessage => 3,
            Self::ToolResult => 4,
            Self::RunStarted => 5,
            Self::RunFinished => 6,
            Self::SystemPrompt => 7,
            Self::SessionName => 8,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Metadata,
            2 => Self::UserMessage,
            3 => Self::AssistantMessage,
            4 => Self::ToolResult,
            5 => Self::RunStarted,
            6 => Self::RunFinished,
            7 => Self::SystemPrompt,
            8 => Self::SessionName,
            _ => return None,
        })
    }
}

/// One append-only session journal record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRecord {
    /// Monotonic record sequence within the session.
    pub sequence: u64,
    /// Record category.
    pub kind: SessionRecordKind,
    /// Opaque payload owned by the higher-level session/runtime codec.
    pub payload: Vec<u8>,
}

impl SessionRecord {
    /// Creates a record before it is appended to a journal.
    #[must_use]
    pub fn new(kind: SessionRecordKind, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            sequence: 0,
            kind,
            payload: payload.into(),
        }
    }
}

/// Errors raised while encoding or decoding a provider-neutral message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionMessageError {
    /// The byte stream ended before a complete value was read.
    Truncated,
    /// The byte stream uses an unsupported codec version.
    UnsupportedVersion {
        /// Version discriminant found in the payload.
        version: u8,
    },
    /// A string was not valid UTF-8.
    InvalidUtf8,
    /// A role discriminant was not recognized.
    InvalidRole {
        /// Role discriminant found in the payload.
        value: u8,
    },
    /// An optional boolean field contained an invalid value.
    InvalidFlag {
        /// Flag value found in the payload.
        value: u8,
    },
    /// A tool-call identifier was rejected by the core identity type.
    InvalidToolCallId,
    /// A length would exceed the supported codec limit.
    LengthOverflow,
}

impl std::fmt::Display for SessionMessageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("session message payload is truncated"),
            Self::UnsupportedVersion { version } => {
                write!(
                    formatter,
                    "session message codec version is unsupported: {version}"
                )
            }
            Self::InvalidUtf8 => formatter.write_str("session message contains invalid UTF-8"),
            Self::InvalidRole { value } => {
                write!(formatter, "invalid session message role: {value}")
            }
            Self::InvalidFlag { value } => {
                write!(formatter, "invalid session message flag: {value}")
            }
            Self::InvalidToolCallId => formatter.write_str("invalid session message tool-call id"),
            Self::LengthOverflow => formatter.write_str("session message field is too large"),
        }
    }
}

impl std::error::Error for SessionMessageError {}

const MESSAGE_CODEC_VERSION: u8 = 2;

/// Encodes one provider-neutral message into a stable Rust-native payload.
///
/// # Errors
///
/// Returns [`SessionMessageError::LengthOverflow`] when a field exceeds the
/// codec's 32-bit length limit.
pub fn encode_message(message: &Message) -> Result<Vec<u8>, SessionMessageError> {
    let mut bytes = Vec::new();
    bytes.push(MESSAGE_CODEC_VERSION);
    bytes.push(match message.role {
        MessageRole::System => 0,
        MessageRole::User => 1,
        MessageRole::Assistant => 2,
        MessageRole::Tool => 3,
    });
    bytes.push(u8::from(message.is_error));
    put_string(&mut bytes, &message.text)?;
    put_optional_string(
        &mut bytes,
        message.tool_call_id.as_ref().map(ToolCallId::as_str),
    )?;
    put_optional_string(&mut bytes, message.tool_name.as_deref())?;
    let count =
        u32::try_from(message.tool_calls.len()).map_err(|_| SessionMessageError::LengthOverflow)?;
    bytes.extend_from_slice(&count.to_le_bytes());
    for call in &message.tool_calls {
        put_string(&mut bytes, call.id.as_str())?;
        put_string(&mut bytes, &call.name)?;
        put_string(&mut bytes, &call.arguments)?;
    }
    bytes.extend_from_slice(&message.timestamp_ms.to_le_bytes());
    put_optional_string(&mut bytes, message.provider.as_deref())?;
    put_optional_string(&mut bytes, message.model.as_deref())?;
    match message.usage {
        Some(usage) => {
            bytes.push(1);
            for value in [
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write,
                usage.total_tokens,
            ] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        None => bytes.push(0),
    }
    bytes.push(match message.stop_reason {
        None => 0,
        Some(StopReason::Stop) => 1,
        Some(StopReason::Length) => 2,
        Some(StopReason::ToolUse) => 3,
        Some(StopReason::Error) => 4,
        Some(StopReason::Aborted) => 5,
    });
    put_optional_string(&mut bytes, message.error_message.as_deref())?;
    Ok(bytes)
}

/// Decodes a Rust-native message payload.
///
/// # Errors
///
/// Returns a precise codec error when the payload is truncated or invalid.
pub fn decode_message(bytes: &[u8]) -> Result<Message, SessionMessageError> {
    let mut cursor = Cursor { bytes, position: 0 };
    let version = cursor.u8()?;
    if version != MESSAGE_CODEC_VERSION {
        return Err(SessionMessageError::UnsupportedVersion { version });
    }
    let role = match cursor.u8()? {
        0 => MessageRole::System,
        1 => MessageRole::User,
        2 => MessageRole::Assistant,
        3 => MessageRole::Tool,
        value => return Err(SessionMessageError::InvalidRole { value }),
    };
    let is_error = match cursor.u8()? {
        0 => false,
        1 => true,
        value => return Err(SessionMessageError::InvalidFlag { value }),
    };
    let text = cursor.string()?;
    let tool_call_id = cursor
        .optional_string()?
        .map(|value| ToolCallId::new(value).map_err(|_| SessionMessageError::InvalidToolCallId))
        .transpose()?;
    let tool_name = cursor.optional_string()?;
    let count = cursor.u32()? as usize;
    let mut tool_calls = Vec::with_capacity(count);
    for _ in 0..count {
        let id = ToolCallId::new(cursor.string()?)
            .map_err(|_| SessionMessageError::InvalidToolCallId)?;
        tool_calls.push(ToolCall {
            id,
            name: cursor.string()?,
            arguments: cursor.string()?,
        });
    }
    let timestamp_ms = cursor.u64()?;
    let provider = cursor.optional_string()?;
    let model = cursor.optional_string()?;
    let usage = match cursor.u8()? {
        0 => None,
        1 => Some(Usage {
            input: cursor.u64()?,
            output: cursor.u64()?,
            cache_read: cursor.u64()?,
            cache_write: cursor.u64()?,
            total_tokens: cursor.u64()?,
        }),
        value => return Err(SessionMessageError::InvalidFlag { value }),
    };
    let stop_reason = match cursor.u8()? {
        0 => None,
        1 => Some(StopReason::Stop),
        2 => Some(StopReason::Length),
        3 => Some(StopReason::ToolUse),
        4 => Some(StopReason::Error),
        5 => Some(StopReason::Aborted),
        value => return Err(SessionMessageError::InvalidFlag { value }),
    };
    let error_message = cursor.optional_string()?;
    Ok(Message {
        role,
        text,
        tool_call_id,
        tool_name,
        is_error,
        tool_calls,
        timestamp_ms,
        provider,
        model,
        usage,
        stop_reason,
        error_message,
    })
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), SessionMessageError> {
    let length = u32::try_from(value.len()).map_err(|_| SessionMessageError::LengthOverflow)?;
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_optional_string(
    bytes: &mut Vec<u8>,
    value: Option<&str>,
) -> Result<(), SessionMessageError> {
    match value {
        Some(value) => {
            bytes.push(1);
            put_string(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Cursor<'_> {
    fn u8(&mut self) -> Result<u8, SessionMessageError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SessionMessageError::Truncated)?;
        self.position += 1;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, SessionMessageError> {
        let end = self
            .position
            .checked_add(4)
            .ok_or(SessionMessageError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(SessionMessageError::Truncated)?;
        self.position = end;
        Ok(u32::from_le_bytes(
            bytes.try_into().expect("four-byte slice"),
        ))
    }

    fn u64(&mut self) -> Result<u64, SessionMessageError> {
        let end = self
            .position
            .checked_add(8)
            .ok_or(SessionMessageError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(SessionMessageError::Truncated)?;
        self.position = end;
        Ok(u64::from_le_bytes(
            bytes.try_into().expect("eight-byte slice"),
        ))
    }

    fn string(&mut self) -> Result<String, SessionMessageError> {
        let length = self.u32()? as usize;
        let end = self
            .position
            .checked_add(length)
            .ok_or(SessionMessageError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(SessionMessageError::Truncated)?;
        self.position = end;
        String::from_utf8(bytes.to_vec()).map_err(|_| SessionMessageError::InvalidUtf8)
    }

    fn optional_string(&mut self) -> Result<Option<String>, SessionMessageError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.string().map(Some),
            value => Err(SessionMessageError::InvalidFlag { value }),
        }
    }
}

/// Errors produced by a native session store.
#[derive(Debug)]
pub enum SessionStoreError {
    /// The persistent store needs a root directory.
    StorageRootRequired,
    /// Another owner currently holds the session lock.
    Locked {
        /// Lock file path.
        path: PathBuf,
    },
    /// The existing file does not contain the expected Rust session header.
    InvalidHeader {
        /// Session file path.
        path: PathBuf,
    },
    /// The existing file uses a version this runtime cannot read.
    UnsupportedVersion {
        /// Session file path.
        path: PathBuf,
    },
    /// The journal contains a truncated or invalid record.
    CorruptRecord {
        /// Session file path.
        path: PathBuf,
    },
    /// The session's previous run never finished, so its outcome is unknown.
    IncompleteRun,
    /// A journal message payload could not be decoded.
    MessageDecode {
        /// Codec diagnostic.
        message: String,
    },
    /// A filesystem operation failed.
    Io {
        /// Logical operation being attempted.
        operation: String,
        /// Underlying diagnostic.
        message: String,
    },
}

impl SessionStoreError {
    /// Returns a stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StorageRootRequired => "session_directory_required",
            Self::Locked { .. } => "session_locked",
            Self::InvalidHeader { .. } => "session_invalid_header",
            Self::UnsupportedVersion { .. } => "session_unsupported_version",
            Self::CorruptRecord { .. } => "session_corrupt_record",
            Self::IncompleteRun => "session_incomplete_run",
            Self::MessageDecode { .. } => "session_message_decode",
            Self::Io { .. } => "session_io",
        }
    }
}

impl std::fmt::Display for SessionStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StorageRootRequired => formatter.write_str("session storage root is required"),
            Self::Locked { path } => write!(formatter, "session is locked: {}", path.display()),
            Self::InvalidHeader { path } => {
                write!(
                    formatter,
                    "session file has an invalid header: {}",
                    path.display()
                )
            }
            Self::UnsupportedVersion { path } => {
                write!(
                    formatter,
                    "session file version is unsupported: {}",
                    path.display()
                )
            }
            Self::CorruptRecord { path } => {
                write!(formatter, "session journal is corrupt: {}", path.display())
            }
            Self::IncompleteRun => formatter
                .write_str("the session's previous run did not finish, so its outcome is unknown"),
            Self::MessageDecode { message } => {
                write!(formatter, "session message decode failed: {message}")
            }
            Self::Io { operation, message } => {
                write!(formatter, "session {operation} failed: {message}")
            }
        }
    }
}

impl std::error::Error for SessionStoreError {}

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

/// A native Rust session store with a versioned file header and cross-platform lock file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSessionStore {
    root: PathBuf,
}

impl FileSessionStore {
    /// Creates a store rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn paths(&self, request: &SessionOpenRequest) -> Result<(PathBuf, PathBuf), SessionStoreError> {
        let root = request
            .storage_root
            .as_deref()
            .unwrap_or(self.root.as_path());
        if root.as_os_str().is_empty() {
            return Err(SessionStoreError::StorageRootRequired);
        }
        let safe_id = encode_session_id(&request.descriptor.id);
        Ok((
            root.join(format!("{safe_id}.session")),
            root.join(format!("{safe_id}.lock")),
        ))
    }
}

impl FileSessionStore {
    /// Lists sessions newest first by last modification, then by id.
    ///
    /// # Errors
    ///
    /// Returns a filesystem error when the root cannot be read.
    pub fn sessions_by_recency(
        &self,
        storage_root: Option<&Path>,
    ) -> Result<Vec<SessionId>, SessionStoreError> {
        let root = storage_root.unwrap_or(self.root.as_path());
        let mut entries: Vec<(std::time::SystemTime, SessionId)> = self
            .session_ids(Some(root))?
            .into_iter()
            .map(|id| {
                let modified =
                    fs::metadata(root.join(format!("{}.session", encode_session_id(&id))))
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                (modified, id)
            })
            .collect();
        entries.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        Ok(entries.into_iter().map(|(_, id)| id).collect())
    }

    /// Lists Rust-native session ids in a storage root, sorted by id.
    ///
    /// Missing roots are treated as an empty session directory.
    ///
    /// # Errors
    ///
    /// Returns a filesystem error when the root cannot be read.
    pub fn session_ids(
        &self,
        storage_root: Option<&Path>,
    ) -> Result<Vec<SessionId>, SessionStoreError> {
        let root = storage_root.unwrap_or(self.root.as_path());
        if root.as_os_str().is_empty() {
            return Err(SessionStoreError::StorageRootRequired);
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(SessionStoreError::Io {
                    operation: "list sessions".to_owned(),
                    message: error.to_string(),
                });
            }
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| SessionStoreError::Io {
                operation: "list sessions".to_owned(),
                message: error.to_string(),
            })?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("session") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if let Some(id) = decode_session_id(stem) {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }
}

fn encode_session_id(id: &SessionId) -> String {
    id.as_str()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut encoded, byte| {
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
            encoded
        })
}

fn decode_session_id(encoded: &str) -> Option<SessionId> {
    if encoded.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    SessionId::new(String::from_utf8(bytes).ok()?).ok()
}

impl SessionStore for FileSessionStore {
    type Error = SessionStoreError;

    fn open(&self, request: SessionOpenRequest) -> Result<OpenedSession, Self::Error> {
        request
            .descriptor
            .validate()
            .map_err(|error| SessionStoreError::Io {
                operation: "validate".to_owned(),
                message: error.to_string(),
            })?;
        if !request.descriptor.persistent {
            return Ok(OpenedSession {
                descriptor: request.descriptor,
                format: SessionFormat::RustNative,
                path: None,
                lease: None,
            });
        }

        let (session_path, lock_path) = self.paths(&request)?;
        let root = session_path.parent().ok_or_else(|| SessionStoreError::Io {
            operation: "resolve root".to_owned(),
            message: session_path.display().to_string(),
        })?;
        fs::create_dir_all(root).map_err(|error| SessionStoreError::Io {
            operation: "create directory".to_owned(),
            message: error.to_string(),
        })?;
        let lock_file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(SessionStoreError::Locked { path: lock_path });
            }
            Err(error) => {
                return Err(SessionStoreError::Io {
                    operation: "create lock".to_owned(),
                    message: error.to_string(),
                });
            }
        };
        let lease = Arc::new(SessionLease {
            lock_path,
            _file: lock_file,
        });

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&session_path)
            .map_err(|error| SessionStoreError::Io {
                operation: "open session".to_owned(),
                message: error.to_string(),
            })?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .map_err(|error| SessionStoreError::Io {
                operation: "read session header".to_owned(),
                message: error.to_string(),
            })?;
        if contents.is_empty() {
            file.write_all(RUST_SESSION_HEADER)
                .and_then(|()| file.sync_all())
                .map_err(|error| SessionStoreError::Io {
                    operation: "initialize session".to_owned(),
                    message: error.to_string(),
                })?;
        } else if !contents.starts_with(RUST_SESSION_HEADER) {
            let _ = fs::remove_file(&lease.lock_path);
            return Err(if contents.starts_with(b"NYANRS01\n") {
                SessionStoreError::UnsupportedVersion { path: session_path }
            } else {
                SessionStoreError::InvalidHeader { path: session_path }
            });
        }
        file.seek(SeekFrom::End(0))
            .map_err(|error| SessionStoreError::Io {
                operation: "position session".to_owned(),
                message: error.to_string(),
            })?;

        Ok(OpenedSession {
            descriptor: request.descriptor,
            format: SessionFormat::RustNative,
            path: Some(session_path),
            lease: Some(lease),
        })
    }
}

#[derive(Debug)]
struct SessionLease {
    lock_path: PathBuf,
    _file: File,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

/// An opened session and its ownership metadata.
#[derive(Clone, Debug)]
pub struct OpenedSession {
    /// Descriptor selected by the caller.
    pub descriptor: SessionDescriptor,
    /// Format used by the storage implementation.
    pub format: SessionFormat,
    /// Database or file path when persisted.
    pub path: Option<PathBuf>,
    lease: Option<Arc<SessionLease>>,
}

impl OpenedSession {
    /// Returns the persisted path if one exists.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Opens the append-only journal while retaining this session's lock lease.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::StorageRootRequired`] for an ephemeral
    /// session or a filesystem/corruption error for a persistent session.
    pub fn journal(&self) -> Result<SessionJournal, SessionStoreError> {
        SessionJournal::open(self)
    }
}

const MAX_RECORD_BYTES: u32 = 64 * 1024 * 1024;

/// An append-only journal owned by one opened session lease.
#[derive(Debug)]
pub struct SessionJournal {
    file: File,
    path: PathBuf,
    _lease: Arc<SessionLease>,
    next_sequence: u64,
}

impl SessionJournal {
    fn open(session: &OpenedSession) -> Result<Self, SessionStoreError> {
        let path = session
            .path
            .clone()
            .ok_or(SessionStoreError::StorageRootRequired)?;
        let lease = session
            .lease
            .clone()
            .ok_or(SessionStoreError::StorageRootRequired)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| SessionStoreError::Io {
                operation: "open journal".to_owned(),
                message: error.to_string(),
            })?;
        let records = read_records(&mut file, &path)?;
        let next_sequence = records
            .last()
            .map_or(0, |record| record.sequence.saturating_add(1));
        file.seek(SeekFrom::End(0))
            .map_err(|error| SessionStoreError::Io {
                operation: "position journal".to_owned(),
                message: error.to_string(),
            })?;
        Ok(Self {
            file,
            path,
            _lease: lease,
            next_sequence,
        })
    }

    /// Appends one record and synchronizes it before returning its sequence.
    ///
    /// # Errors
    ///
    /// Returns a filesystem error when the frame cannot be written or synced.
    pub fn append(&mut self, mut record: SessionRecord) -> Result<u64, SessionStoreError> {
        let payload_length =
            record
                .payload
                .len()
                .checked_add(9)
                .ok_or_else(|| SessionStoreError::Io {
                    operation: "append journal record".to_owned(),
                    message: "record length overflow".to_owned(),
                })?;
        let frame_length = u32::try_from(payload_length).map_err(|_| SessionStoreError::Io {
            operation: "append journal record".to_owned(),
            message: "record exceeds the maximum frame size".to_owned(),
        })?;
        if frame_length > MAX_RECORD_BYTES {
            return Err(SessionStoreError::Io {
                operation: "append journal record".to_owned(),
                message: "record exceeds the maximum frame size".to_owned(),
            });
        }
        let sequence = self.next_sequence;
        record.sequence = sequence;
        self.file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(&frame_length.to_le_bytes()))
            .and_then(|()| self.file.write_all(&sequence.to_le_bytes()))
            .and_then(|()| self.file.write_all(&[record.kind.code()]))
            .and_then(|()| self.file.write_all(&record.payload))
            .and_then(|()| self.file.sync_all())
            .map_err(|error| SessionStoreError::Io {
                operation: "append journal record".to_owned(),
                message: error.to_string(),
            })?;
        self.next_sequence = sequence.saturating_add(1);
        Ok(sequence)
    }

    /// Encodes and appends one message record.
    ///
    /// # Errors
    ///
    /// Returns a codec or filesystem error when the message cannot be encoded
    /// or the record cannot be committed.
    pub fn append_message(
        &mut self,
        kind: SessionRecordKind,
        message: &Message,
    ) -> Result<u64, SessionStoreError> {
        let payload = encode_message(message).map_err(|error| SessionStoreError::Io {
            operation: "encode session message".to_owned(),
            message: error.to_string(),
        })?;
        self.append(SessionRecord::new(kind, payload))
    }

    /// Replays all records currently committed in the journal.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::CorruptRecord`] if a frame is truncated or
    /// has an unknown record kind.
    pub fn records(&mut self) -> Result<Vec<SessionRecord>, SessionStoreError> {
        read_records(&mut self.file, &self.path)
    }

    /// Decodes a message record returned by [`Self::records`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionMessageError`] when the record payload is not a valid
    /// Rust-native message.
    pub fn decode_message_record(record: &SessionRecord) -> Result<Message, SessionMessageError> {
        decode_message(&record.payload)
    }

    /// Replays all user, assistant, and tool-result messages in journal order.
    ///
    /// Metadata and run-marker records remain available through [`Self::records`]
    /// but are omitted from the provider transcript.
    ///
    /// # Errors
    ///
    /// Returns a store error when the journal or a message payload is invalid.
    pub fn replay_messages(&mut self) -> Result<Vec<Message>, SessionStoreError> {
        self.records()?
            .into_iter()
            .filter(|record| {
                matches!(
                    record.kind,
                    SessionRecordKind::SystemPrompt
                        | SessionRecordKind::UserMessage
                        | SessionRecordKind::AssistantMessage
                        | SessionRecordKind::ToolResult
                )
            })
            .map(|record| {
                Self::decode_message_record(&record).map_err(|error| {
                    SessionStoreError::MessageDecode {
                        message: error.to_string(),
                    }
                })
            })
            .collect()
    }
}

/// An opened Rust-native session with its replayed provider transcript.
#[derive(Debug)]
pub struct NativeSession {
    opened: OpenedSession,
    journal: Option<SessionJournal>,
    messages: Vec<Message>,
    incomplete_run: bool,
    name: Option<String>,
}

impl NativeSession {
    /// Opens a session and replays its committed message records.
    ///
    /// # Errors
    ///
    /// Returns the store or codec error when selection, locking, or replay fails.
    pub fn open(
        store: &FileSessionStore,
        request: SessionOpenRequest,
    ) -> Result<Self, SessionStoreError> {
        let opened = store.open(request)?;
        let mut journal = opened.path().map(|_| opened.journal()).transpose()?;
        let (messages, incomplete_run, name) = if let Some(journal) = &mut journal {
            let records = journal.records()?;
            let messages = records
                .iter()
                .filter(|record| {
                    matches!(
                        record.kind,
                        SessionRecordKind::SystemPrompt
                            | SessionRecordKind::UserMessage
                            | SessionRecordKind::AssistantMessage
                            | SessionRecordKind::ToolResult
                    )
                })
                .map(SessionJournal::decode_message_record)
                .map(|result| {
                    result.map_err(|error| SessionStoreError::MessageDecode {
                        message: error.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let incomplete_run =
                records
                    .iter()
                    .fold(false, |incomplete, record| match record.kind {
                        SessionRecordKind::RunStarted => true,
                        SessionRecordKind::RunFinished => false,
                        _ => incomplete,
                    });
            let mut messages = messages;
            normalize_system_messages(&mut messages);
            let name = records
                .iter()
                .rev()
                .find(|record| record.kind == SessionRecordKind::SessionName)
                .and_then(|record| String::from_utf8(record.payload.clone()).ok());
            (messages, incomplete_run, name)
        } else {
            (Vec::new(), false, None)
        };
        Ok(Self {
            opened,
            journal,
            messages,
            incomplete_run,
            name,
        })
    }

    /// Returns the selected session metadata.
    #[must_use]
    pub fn session(&self) -> &OpenedSession {
        &self.opened
    }

    /// Returns the replayed and newly appended provider messages.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns the display name, if one was set.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Sets the display name and records it in the journal.
    ///
    /// # Errors
    ///
    /// Returns a store error when the record cannot be committed.
    pub fn set_name(&mut self, name: &str) -> Result<(), SessionStoreError> {
        if let Some(journal) = &mut self.journal {
            journal.append(SessionRecord::new(
                SessionRecordKind::SessionName,
                name.as_bytes().to_vec(),
            ))?;
        }
        self.name = Some(name.to_owned());
        Ok(())
    }

    /// Returns whether replay found a run start without a later run finish.
    #[must_use]
    pub fn has_incomplete_run(&self) -> bool {
        self.incomplete_run
    }

    /// Commits a run-start marker before external work begins.
    ///
    /// # Errors
    ///
    /// Returns an error if another run is already marked incomplete or the
    /// marker cannot be persisted.
    pub fn begin_run(&mut self, payload: impl Into<Vec<u8>>) -> Result<(), SessionStoreError> {
        if self.incomplete_run {
            return Err(SessionStoreError::IncompleteRun);
        }
        if let Some(journal) = &mut self.journal {
            journal.append(SessionRecord::new(SessionRecordKind::RunStarted, payload))?;
        }
        self.incomplete_run = true;
        Ok(())
    }

    /// Commits a run-finished marker after the run outcome is known.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker cannot be persisted.
    pub fn finish_run(&mut self, payload: impl Into<Vec<u8>>) -> Result<(), SessionStoreError> {
        if let Some(journal) = &mut self.journal {
            journal.append(SessionRecord::new(SessionRecordKind::RunFinished, payload))?;
        }
        self.incomplete_run = false;
        Ok(())
    }

    /// Appends one message and updates the in-memory transcript only after the write succeeds.
    ///
    /// # Errors
    ///
    /// Returns a store or codec error when the message cannot be committed.
    pub fn append_message(
        &mut self,
        kind: SessionRecordKind,
        message: Message,
    ) -> Result<u64, SessionStoreError> {
        let sequence = if let Some(journal) = &mut self.journal {
            journal.append_message(kind, &message)?
        } else {
            self.messages.len() as u64
        };
        self.messages.push(message);
        normalize_system_messages(&mut self.messages);
        Ok(sequence)
    }

    /// Creates a new session containing the first `message_count` messages.
    ///
    /// The target request supplies the new session id and storage selection.
    /// The source session remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns a selection, lock, codec, or filesystem error when the fork
    /// cannot be completed.
    pub fn fork_into(
        &self,
        store: &FileSessionStore,
        request: SessionOpenRequest,
        message_count: usize,
    ) -> Result<Self, SessionStoreError> {
        if message_count > self.messages.len() {
            return Err(SessionStoreError::Io {
                operation: "fork session".to_owned(),
                message: format!(
                    "requested {message_count} messages but session contains {}",
                    self.messages.len()
                ),
            });
        }
        let mut fork = Self::open(store, request)?;
        for message in self.messages.iter().take(message_count) {
            fork.append_message(record_kind_for_message(message), message.clone())?;
        }
        Ok(fork)
    }

    /// Creates a full copy of this session under the target request.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::fork_into`].
    pub fn clone_into(
        &self,
        store: &FileSessionStore,
        request: SessionOpenRequest,
    ) -> Result<Self, SessionStoreError> {
        self.fork_into(store, request, self.messages.len())
    }
}

/// Keeps only the latest system message and moves it to the front.
fn normalize_system_messages(messages: &mut Vec<Message>) {
    let Some(latest) = messages
        .iter()
        .rposition(|message| message.role == MessageRole::System)
    else {
        return;
    };
    let system = messages[latest].clone();
    messages.retain(|message| message.role != MessageRole::System);
    messages.insert(0, system);
}

fn record_kind_for_message(message: &Message) -> SessionRecordKind {
    match message.role {
        MessageRole::System => SessionRecordKind::SystemPrompt,
        MessageRole::User => SessionRecordKind::UserMessage,
        MessageRole::Assistant => SessionRecordKind::AssistantMessage,
        MessageRole::Tool => SessionRecordKind::ToolResult,
    }
}

fn read_records(file: &mut File, path: &Path) -> Result<Vec<SessionRecord>, SessionStoreError> {
    file.seek(SeekFrom::Start(RUST_SESSION_HEADER.len() as u64))
        .map_err(|error| SessionStoreError::Io {
            operation: "seek journal".to_owned(),
            message: error.to_string(),
        })?;
    let mut records = Vec::new();
    loop {
        let mut length_bytes = [0_u8; 4];
        let read = file
            .read(&mut length_bytes)
            .map_err(|error| SessionStoreError::Io {
                operation: "read journal frame".to_owned(),
                message: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        if read != length_bytes.len() {
            return Err(SessionStoreError::CorruptRecord {
                path: path.to_owned(),
            });
        }
        let frame_length = u32::from_le_bytes(length_bytes);
        if !(9..=MAX_RECORD_BYTES).contains(&frame_length) {
            return Err(SessionStoreError::CorruptRecord {
                path: path.to_owned(),
            });
        }
        let mut frame = vec![0_u8; frame_length as usize];
        file.read_exact(&mut frame)
            .map_err(|_| SessionStoreError::CorruptRecord {
                path: path.to_owned(),
            })?;
        let sequence = u64::from_le_bytes(
            frame[0..8]
                .try_into()
                .expect("validated journal frame has a sequence"),
        );
        let kind = SessionRecordKind::from_code(frame[8]).ok_or_else(|| {
            SessionStoreError::CorruptRecord {
                path: path.to_owned(),
            }
        })?;
        records.push(SessionRecord {
            sequence,
            kind,
            payload: frame[9..].to_vec(),
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        FileSessionStore, NativeSession, SessionDescriptor, SessionFormat, SessionJournal,
        SessionOpenRequest, SessionRecord, SessionRecordKind, SessionStore, SessionStoreError,
        decode_message, encode_message,
    };
    use nyan_coding_agent_core::{
        AgentMode, Message, MessageRole, SessionId, StopReason, ToolCall, ToolCallId, Usage,
    };

    fn test_root(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "nyan-session-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn request(root: &std::path::Path) -> SessionOpenRequest {
        request_for(root, "session-1")
    }

    fn request_for(root: &std::path::Path, id: &str) -> SessionOpenRequest {
        SessionOpenRequest {
            descriptor: SessionDescriptor::persistent(
                SessionId::new(id).expect("session id"),
                "/tmp/project",
            ),
            mode: AgentMode::Print,
            storage_root: Some(root.to_owned()),
        }
    }

    #[test]
    fn persistent_descriptor_validates() {
        let descriptor = SessionDescriptor::persistent(
            SessionId::new("session-1").expect("session id"),
            "/tmp/project",
        );
        assert!(descriptor.validate().is_ok());
    }

    #[test]
    fn format_is_explicit() {
        assert_eq!(SessionFormat::LegacyJsonl, SessionFormat::LegacyJsonl);
    }

    #[test]
    fn native_store_writes_a_versioned_file_and_releases_its_lock() {
        let root = test_root("versioned");
        let store = FileSessionStore::new(&root);
        let opened = store.open(request(&root)).expect("open session");
        let path = opened.path().expect("session path").to_owned();
        assert_eq!(opened.format, SessionFormat::RustNative);
        assert_eq!(fs::read(&path).expect("read session"), b"NYANRS01\n1\n");
        assert!(matches!(
            store.open(request(&root)),
            Err(SessionStoreError::Locked { .. })
        ));
        drop(opened);
        let reopened = store.open(request(&root)).expect("reopen session");
        assert_eq!(reopened.path(), Some(path.as_path()));
        drop(reopened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn journal_appends_and_replays_in_sequence_order() {
        let root = test_root("journal");
        let store = FileSessionStore::new(&root);
        let opened = store.open(request(&root)).expect("open session");
        let mut journal = opened.journal().expect("open journal");
        assert_eq!(
            journal
                .append(SessionRecord::new(
                    SessionRecordKind::UserMessage,
                    b"hello".to_vec(),
                ))
                .expect("append user"),
            0
        );
        assert_eq!(
            journal
                .append(SessionRecord::new(
                    SessionRecordKind::AssistantMessage,
                    b"world".to_vec(),
                ))
                .expect("append assistant"),
            1
        );
        let records = journal.records().expect("replay journal");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, SessionRecordKind::UserMessage);
        assert_eq!(records[0].payload, b"hello");
        assert_eq!(records[1].sequence, 1);
        drop(journal);
        drop(opened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn message_codec_roundtrips_tool_calls_and_results() {
        let message = Message {
            role: MessageRole::Assistant,
            text: "use tool".to_owned(),
            tool_call_id: Some(ToolCallId::new("result-1").expect("tool result id")),
            tool_name: Some("echo".to_owned()),
            is_error: true,
            tool_calls: vec![ToolCall {
                id: ToolCallId::new("call-1").expect("tool call id"),
                name: "echo".to_owned(),
                arguments: "hello".to_owned(),
            }],
            timestamp_ms: 1_733_234_401_000,
            provider: Some("openai".to_owned()),
            model: Some("gpt-test".to_owned()),
            usage: Some(Usage {
                input: 100,
                output: 20,
                cache_read: 5,
                cache_write: 3,
                total_tokens: 120,
            }),
            stop_reason: Some(StopReason::Length),
            error_message: Some("cut off".to_owned()),
        };
        let encoded = encode_message(&message).expect("encode message");
        assert_eq!(decode_message(&encoded).expect("decode message"), message);
    }

    #[test]
    fn message_codec_roundtrips_every_stop_reason_and_rejects_the_old_version() {
        for reason in [
            None,
            Some(StopReason::Stop),
            Some(StopReason::Length),
            Some(StopReason::ToolUse),
            Some(StopReason::Error),
            Some(StopReason::Aborted),
        ] {
            let mut message = Message::text(MessageRole::Assistant, "x");
            message.stop_reason = reason;
            let encoded = encode_message(&message).expect("encode");
            assert_eq!(
                decode_message(&encoded).expect("decode").stop_reason,
                reason
            );
        }
        let mut old = encode_message(&Message::text(MessageRole::User, "x")).expect("encode");
        old[0] = 1;
        assert_eq!(
            decode_message(&old),
            Err(super::SessionMessageError::UnsupportedVersion { version: 1 })
        );
        let truncated = encode_message(&Message::text(MessageRole::User, "x")).expect("encode");
        assert_eq!(
            decode_message(&truncated[..truncated.len() - 1]),
            Err(super::SessionMessageError::Truncated)
        );
    }

    #[test]
    fn journal_message_helper_uses_the_codec() {
        let root = test_root("journal-message");
        let store = FileSessionStore::new(&root);
        let opened = store.open(request(&root)).expect("open session");
        let mut journal = opened.journal().expect("open journal");
        let message = Message::text(MessageRole::User, "hello");
        assert_eq!(
            journal
                .append_message(SessionRecordKind::UserMessage, &message)
                .expect("append message"),
            0
        );
        let records = journal.records().expect("read records");
        assert_eq!(
            SessionJournal::decode_message_record(&records[0]).expect("decode record"),
            message
        );
        drop(journal);
        drop(opened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn native_session_reopens_its_provider_transcript() {
        let root = test_root("native-session");
        let store = FileSessionStore::new(&root);
        let mut session = NativeSession::open(&store, request(&root)).expect("open native session");
        session
            .append_message(
                SessionRecordKind::UserMessage,
                Message::text(MessageRole::User, "hello"),
            )
            .expect("append user message");
        session
            .append_message(
                SessionRecordKind::AssistantMessage,
                Message::text(MessageRole::Assistant, "world"),
            )
            .expect("append assistant message");
        drop(session);

        let reopened = NativeSession::open(&store, request(&root)).expect("reopen native session");
        assert_eq!(reopened.messages().len(), 2);
        assert_eq!(reopened.messages()[0].text, "hello");
        assert_eq!(reopened.messages()[1].text, "world");
        drop(reopened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn native_session_recovers_incomplete_run_marker() {
        let root = test_root("recovery");
        let store = FileSessionStore::new(&root);
        let mut session = NativeSession::open(&store, request_for(&root, "recoverable"))
            .expect("open recoverable session");
        assert!(!session.has_incomplete_run());
        session.begin_run(b"run-1".to_vec()).expect("begin run");
        assert!(session.has_incomplete_run());
        drop(session);

        let mut reopened = NativeSession::open(&store, request_for(&root, "recoverable"))
            .expect("reopen recoverable session");
        assert!(reopened.has_incomplete_run());
        assert!(reopened.begin_run(b"run-2".to_vec()).is_err());
        reopened
            .finish_run(b"aborted".to_vec())
            .expect("finish recovery");
        assert!(!reopened.has_incomplete_run());
        drop(reopened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn the_latest_system_prompt_replaces_earlier_ones_and_stays_first() {
        let root = test_root("system-prompt");
        let store = FileSessionStore::new(&root);
        let mut session =
            NativeSession::open(&store, request_for(&root, "prompted")).expect("open");
        let append = |session: &mut NativeSession, role, text: &str| {
            let kind = match role {
                MessageRole::System => SessionRecordKind::SystemPrompt,
                MessageRole::User => SessionRecordKind::UserMessage,
                _ => SessionRecordKind::AssistantMessage,
            };
            session
                .append_message(kind, Message::text(role, text))
                .expect("append");
        };
        append(&mut session, MessageRole::System, "prompt one");
        append(&mut session, MessageRole::User, "hello");
        append(&mut session, MessageRole::System, "prompt two");
        let texts = |session: &NativeSession| {
            session
                .messages()
                .iter()
                .map(|message| message.text.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&session), ["prompt two", "hello"]);
        drop(session);
        let reopened = NativeSession::open(&store, request_for(&root, "prompted")).expect("reopen");
        assert_eq!(
            texts(&reopened),
            ["prompt two", "hello"],
            "replay normalizes the same way"
        );
        drop(reopened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn the_session_name_persists_and_the_last_one_wins() {
        let root = test_root("name");
        let store = FileSessionStore::new(&root);
        let mut session = NativeSession::open(&store, request_for(&root, "named")).expect("open");
        assert_eq!(session.name(), None);
        session.set_name("first").expect("name");
        session.set_name("second").expect("rename");
        drop(session);
        let reopened = NativeSession::open(&store, request_for(&root, "named")).expect("reopen");
        assert_eq!(reopened.name(), Some("second"));
        drop(reopened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn sessions_list_newest_first() {
        let root = test_root("recency");
        let store = FileSessionStore::new(&root);
        for id in ["old", "mid", "new"] {
            let session = NativeSession::open(&store, request_for(&root, id)).expect("open");
            drop(session);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let ids: Vec<String> = store
            .sessions_by_recency(Some(&root))
            .expect("list")
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["new", "mid", "old"]);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn an_unfinished_run_has_its_own_error_code() {
        let root = test_root("incomplete-code");
        let store = FileSessionStore::new(&root);
        let mut session = NativeSession::open(&store, request_for(&root, "crashed")).expect("open");
        session.begin_run(Vec::new()).expect("begin");
        drop(session);
        let mut reopened =
            NativeSession::open(&store, request_for(&root, "crashed")).expect("reopen");
        assert_eq!(
            reopened
                .begin_run(Vec::new())
                .expect_err("must refuse")
                .code(),
            "session_incomplete_run"
        );
        drop(reopened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn session_selection_lists_native_ids() {
        let root = test_root("selection");
        let store = FileSessionStore::new(&root);
        let first =
            NativeSession::open(&store, request_for(&root, "first")).expect("first session");
        drop(first);
        let second =
            NativeSession::open(&store, request_for(&root, "second")).expect("second session");
        drop(second);
        assert_eq!(
            store.session_ids(Some(&root)).expect("list sessions"),
            vec![
                SessionId::new("first").expect("first id"),
                SessionId::new("second").expect("second id")
            ]
        );
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn native_session_forks_and_clones_message_prefixes() {
        let root = test_root("fork");
        let store = FileSessionStore::new(&root);
        let mut source = NativeSession::open(&store, request_for(&root, "source")).expect("source");
        source
            .append_message(
                SessionRecordKind::UserMessage,
                Message::text(MessageRole::User, "one"),
            )
            .expect("append one");
        source
            .append_message(
                SessionRecordKind::AssistantMessage,
                Message::text(MessageRole::Assistant, "two"),
            )
            .expect("append two");
        let fork = source
            .fork_into(&store, request_for(&root, "forked"), 1)
            .expect("fork session");
        assert_eq!(fork.messages().len(), 1);
        assert_eq!(fork.messages()[0].text, "one");
        drop(fork);
        let clone = source
            .clone_into(&store, request_for(&root, "cloned"))
            .expect("clone session");
        assert_eq!(clone.messages().len(), 2);
        drop(clone);
        drop(source);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn corrupt_journal_frame_is_reported() {
        let root = test_root("corrupt-journal");
        let store = FileSessionStore::new(&root);
        let opened = store.open(request(&root)).expect("open session");
        let path = opened.path().expect("session path").to_owned();
        drop(opened);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open session file");
        file.write_all(&[20, 0]).expect("write partial frame");
        drop(file);
        let opened = store.open(request(&root)).expect("reopen session");
        let error = opened.journal().expect_err("corrupt journal");
        assert_eq!(error.code(), "session_corrupt_record");
        drop(opened);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn invalid_and_unsupported_headers_are_rejected() {
        let root = test_root("headers");
        let store = FileSessionStore::new(&root);
        let opened = store.open(request(&root)).expect("create session");
        let path = opened.path().expect("session path").to_owned();
        drop(opened);

        fs::write(&path, b"not a nyan session").expect("write invalid header");
        let error = store.open(request(&root)).expect_err("invalid header");
        assert_eq!(error.code(), "session_invalid_header");
        fs::write(&path, b"NYANRS01\n2\n").expect("write unsupported header");
        let error = store.open(request(&root)).expect_err("unsupported version");
        assert_eq!(error.code(), "session_unsupported_version");
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn ephemeral_sessions_do_not_require_storage() {
        let store = FileSessionStore::new("");
        let request = SessionOpenRequest {
            descriptor: SessionDescriptor::ephemeral(
                SessionId::new("memory").expect("session id"),
                "/tmp/project",
            ),
            mode: AgentMode::Json,
            storage_root: None,
        };
        let opened = store.open(request).expect("open ephemeral session");
        assert_eq!(opened.path(), None);
        assert_eq!(opened.format, SessionFormat::RustNative);
    }
}
