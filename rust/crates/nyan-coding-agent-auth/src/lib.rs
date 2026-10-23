//! Credential storage for the Rust Coding Agent: `auth.json` in the agent directory.
//!
//! The file is a JSON object with one credential per provider id. Rust writes
//! only the plain `{"type": "api_key", "key": ..., "env": {...}}` shape, which
//! the TypeScript CLI also accepts, and leaves every other entry as it found
//! it. The file is validated as a whole before it is changed: an unreadable or
//! malformed file is reported and never overwritten, because only its owner can
//! say whether the content is recoverable.
//!
//! Writes are read-modify-write under a lock file, then a temporary file in the
//! same directory is renamed over `auth.json`, so a crash leaves either the old
//! file or the new one. On Unix the file is created with mode 0600; on Windows
//! it inherits the access rules of the user's profile directory.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

const FILE_NAME: &str = "auth.json";
const LOCK_WAIT: Duration = Duration::from_secs(5);
const LOCK_POLL: Duration = Duration::from_millis(25);
const LOCK_STALE: Duration = Duration::from_secs(30);

/// A stored API-key credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiKeyCredential {
    /// The key itself.
    pub key: Option<String>,
    /// Provider-scoped settings such as a base URL, stored beside the key.
    pub env: BTreeMap<String, String>,
}

/// A stored credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Credential {
    /// A plain API key.
    ApiKey(ApiKeyCredential),
    /// An OAuth login made by another CLI; it is kept but not usable here yet.
    OAuth,
}

/// Why a credential operation failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthError {
    /// A filesystem operation failed.
    Io {
        /// What was being done.
        operation: String,
        /// The OS diagnostic.
        message: String,
    },
    /// `auth.json` exists but is not a valid credential file.
    Invalid {
        /// File path.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
    /// Another process held the write lock for the whole wait.
    Locked {
        /// Lock file path.
        path: PathBuf,
    },
}

impl AuthError {
    /// A stable code for dispatch and reports.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "auth_io",
            Self::Invalid { .. } => "auth_invalid",
            Self::Locked { .. } => "auth_locked",
        }
    }
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { operation, message } => write!(formatter, "{operation} failed: {message}"),
            Self::Invalid { path, message } => write!(
                formatter,
                "{} is not a valid credential file ({message}); fix or remove it, nothing was changed",
                path.display()
            ),
            Self::Locked { path } => write!(
                formatter,
                "another process is writing credentials ({} is locked)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AuthError {}

fn io_error(operation: &str, error: &std::io::Error) -> AuthError {
    AuthError::Io {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

/// The credential file of one agent directory.
#[derive(Clone, Debug)]
pub struct AuthStore {
    path: PathBuf,
    lock_wait: Duration,
}

struct WriteLock {
    path: PathBuf,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl AuthStore {
    /// The store for `agent_dir/auth.json`.
    #[must_use]
    pub fn new(agent_dir: &Path) -> Self {
        Self {
            path: agent_dir.join(FILE_NAME),
            lock_wait: LOCK_WAIT,
        }
    }

    /// Sets how long a write waits for another writer's lock.
    #[must_use]
    pub fn with_lock_wait(mut self, lock_wait: Duration) -> Self {
        self.lock_wait = lock_wait;
        self
    }

    /// The credential file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_entries(&self) -> Result<Map<String, Value>, AuthError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(error) => return Err(io_error("read the credential file", &error)),
        };
        let invalid = |message: String| AuthError::Invalid {
            path: self.path.clone(),
            message,
        };
        let value: Value =
            serde_json::from_str(&text).map_err(|error| invalid(error.to_string()))?;
        let Value::Object(entries) = value else {
            return Err(invalid("expected a JSON object".to_owned()));
        };
        for (provider, entry) in &entries {
            if parse_credential(entry).is_none() {
                return Err(invalid(format!(
                    "the entry for \"{provider}\" is not a credential"
                )));
            }
        }
        Ok(entries)
    }

    /// Returns the credential for `provider`, if one is stored.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or is not valid.
    pub fn credential(&self, provider: &str) -> Result<Option<Credential>, AuthError> {
        Ok(self
            .read_entries()?
            .get(provider)
            .and_then(parse_credential))
    }

    /// Lists the providers that have a stored credential, sorted.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or is not valid.
    pub fn providers(&self) -> Result<Vec<String>, AuthError> {
        Ok(self.read_entries()?.keys().cloned().collect())
    }

    /// Stores an API-key credential for `provider`, replacing any earlier one.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is invalid, locked, or cannot be written.
    pub fn save_api_key(
        &self,
        provider: &str,
        key: &str,
        env: &BTreeMap<String, String>,
    ) -> Result<(), AuthError> {
        self.modify(|entries| {
            entries.insert(
                provider.to_owned(),
                json!({"type": "api_key", "key": key, "env": env}),
            );
            true
        })
        .map(|_| ())
    }

    /// Removes the credential for `provider` and reports whether there was one.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is invalid, locked, or cannot be written.
    pub fn remove(&self, provider: &str) -> Result<bool, AuthError> {
        self.modify(|entries| entries.remove(provider).is_some())
    }

    /// Runs `change` on the current entries under the lock and writes the result
    /// when `change` returns `true`.
    fn modify(
        &self,
        change: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<bool, AuthError> {
        if let Some(directory) = self.path.parent() {
            fs::create_dir_all(directory)
                .map_err(|error| io_error("create the agent directory", &error))?;
        }
        let _lock = self.lock()?;
        let mut entries = self.read_entries()?;
        let changed = change(&mut entries);
        if changed {
            self.write(&entries)?;
        }
        Ok(changed)
    }

    fn lock(&self) -> Result<WriteLock, AuthError> {
        let path = self.path.with_extension("json.lock");
        let started = std::time::Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(WriteLock { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                        .is_some_and(|age| age > LOCK_STALE);
                    if stale {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() >= self.lock_wait {
                        return Err(AuthError::Locked { path });
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(error) => return Err(io_error("take the credential write lock", &error)),
            }
        }
    }

    fn write(&self, entries: &Map<String, Value>) -> Result<(), AuthError> {
        let temporary = self
            .path
            .with_extension(format!("json.tmp-{}", std::process::id()));
        let mut text =
            serde_json::to_string_pretty(&Value::Object(entries.clone())).map_err(|error| {
                AuthError::Io {
                    operation: "encode the credential file".to_owned(),
                    message: error.to_string(),
                }
            })?;
        text.push('\n');
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = options
            .open(&temporary)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temporary, &self.path));
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(io_error("write the credential file", &error));
        }
        Ok(())
    }
}

/// Parses one entry; `None` means it is not a credential.
fn parse_credential(entry: &Value) -> Option<Credential> {
    let object = entry.as_object()?;
    match object.get("type")?.as_str()? {
        "api_key" => {
            let key = match object.get("key") {
                None | Some(Value::Null) => None,
                Some(Value::String(key)) => Some(key.clone()),
                Some(_) => return None,
            };
            let mut env = BTreeMap::new();
            match object.get("env") {
                None | Some(Value::Null) => {}
                Some(Value::Object(values)) => {
                    for (name, value) in values {
                        env.insert(name.clone(), value.as_str()?.to_owned());
                    }
                }
                Some(_) => return None,
            }
            Some(Credential::ApiKey(ApiKeyCredential { key, env }))
        }
        "oauth" => {
            let text = |name: &str| object.get(name).is_some_and(Value::is_string);
            let expires = object.get("expires").is_some_and(Value::is_number);
            (text("access") && text("refresh") && expires).then_some(Credential::OAuth)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
