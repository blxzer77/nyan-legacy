//! Session identity and persistence boundaries.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nyan_coding_agent_core::{AgentMode, CoreError, SessionId};

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
        let safe_id = request.descriptor.id.as_str().as_bytes().iter().fold(
            String::new(),
            |mut safe_id, byte| {
                write!(&mut safe_id, "{byte:02x}").expect("writing to a String cannot fail");
                safe_id
            },
        );
        Ok((
            root.join(format!("{safe_id}.session")),
            root.join(format!("{safe_id}.lock")),
        ))
    }
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

    /// Replays all records currently committed in the journal.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::CorruptRecord`] if a frame is truncated or
    /// has an unknown record kind.
    pub fn records(&mut self) -> Result<Vec<SessionRecord>, SessionStoreError> {
        read_records(&mut self.file, &self.path)
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
        FileSessionStore, SessionDescriptor, SessionFormat, SessionOpenRequest, SessionRecord,
        SessionRecordKind, SessionStore, SessionStoreError,
    };
    use nyan_coding_agent_core::{AgentMode, SessionId};

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
        SessionOpenRequest {
            descriptor: SessionDescriptor::persistent(
                SessionId::new("session-1").expect("session id"),
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
