//! Credential storage for the Rust Coding Agent: `auth.json` in the agent directory.
//!
//! The file is a JSON object with one credential per provider id. Rust writes
//! the plain `{"type": "api_key", "key": ..., "env": {...}}` shape and the
//! `{"type": "oauth", "access", "refresh", "expires", ...}` shape, which the
//! TypeScript CLI also accepts, and leaves every other entry as it found it. The file is validated as a whole before it is changed: an unreadable or
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
/// A refresh makes a network call while holding the lock, so waiters wait longer.
const REFRESH_LOCK_WAIT: Duration = Duration::from_secs(45);

/// A stored API-key credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiKeyCredential {
    /// The key itself.
    pub key: Option<String>,
    /// Provider-scoped settings such as a base URL, stored beside the key.
    pub env: BTreeMap<String, String>,
}

/// A stored OAuth login.
#[derive(Clone, Eq, PartialEq)]
pub struct OAuthCredential {
    /// The access token.
    pub access: String,
    /// The refresh token.
    pub refresh: String,
    /// When the access token expires, in Unix milliseconds.
    pub expires_ms: u64,
    /// Provider-specific fields stored beside the tokens, such as `accountId`.
    pub extra: BTreeMap<String, Value>,
}

impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthCredential")
            .field("expires_ms", &self.expires_ms)
            .finish_non_exhaustive()
    }
}

impl OAuthCredential {
    /// Whether the access token is still good for at least `margin_ms` at `now_ms`.
    #[must_use]
    pub fn is_valid_for(&self, now_ms: u64, margin_ms: u64) -> bool {
        self.expires_ms > now_ms.saturating_add(margin_ms)
    }

    /// A string field stored beside the tokens.
    #[must_use]
    pub fn extra_str(&self, name: &str) -> Option<&str> {
        self.extra.get(name).and_then(Value::as_str)
    }
}

/// A stored credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Credential {
    /// A plain API key.
    ApiKey(ApiKeyCredential),
    /// An OAuth login.
    OAuth(OAuthCredential),
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
    /// There is no stored OAuth login to refresh.
    NotLoggedIn {
        /// The provider.
        provider: String,
    },
    /// The refresh itself failed; the stored login was left as it was.
    RefreshFailed {
        /// What the refresher reported.
        message: String,
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
            Self::NotLoggedIn { .. } => "auth_not_logged_in",
            Self::RefreshFailed { .. } => "auth_refresh_failed",
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
            Self::NotLoggedIn { provider } => {
                write!(
                    formatter,
                    "there is no stored OAuth login for \"{provider}\""
                )
            }
            Self::RefreshFailed { message } => {
                write!(formatter, "refreshing the login failed: {message}")
            }
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

    /// Stores an OAuth login for `provider`, replacing any earlier credential.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is invalid, locked, or cannot be written.
    pub fn save_oauth(
        &self,
        provider: &str,
        credential: &OAuthCredential,
    ) -> Result<(), AuthError> {
        self.modify(|entries| {
            entries.insert(provider.to_owned(), oauth_json(credential));
            true
        })
        .map(|_| ())
    }

    /// Returns the OAuth login for `provider` with an access token good for at
    /// least `margin_ms`, refreshing it first if it is not.
    ///
    /// The refresh runs under the write lock, and the stored login is read again
    /// once the lock is held: a process that refreshed while this one waited has
    /// already replaced the (single-use) refresh token, so using the old one
    /// would fail. `refresher` gets the stored login and returns the new one,
    /// which is written before the lock is released. If it fails, nothing is
    /// written.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NotLoggedIn`] when the provider has no OAuth login,
    /// [`AuthError::RefreshFailed`] when `refresher` fails, and the usual file
    /// errors otherwise.
    pub fn refresh_oauth(
        &self,
        provider: &str,
        now_ms: u64,
        margin_ms: u64,
        refresher: impl FnOnce(&OAuthCredential) -> Result<OAuthCredential, String>,
    ) -> Result<OAuthCredential, AuthError> {
        if let Some(Credential::OAuth(current)) = self.credential(provider)?
            && current.is_valid_for(now_ms, margin_ms)
        {
            return Ok(current);
        }
        if let Some(directory) = self.path.parent() {
            fs::create_dir_all(directory)
                .map_err(|error| io_error("create the agent directory", &error))?;
        }
        let _lock = self.lock(REFRESH_LOCK_WAIT.max(self.lock_wait))?;
        let mut entries = self.read_entries()?;
        let Some(Credential::OAuth(current)) = entries.get(provider).and_then(parse_credential)
        else {
            return Err(AuthError::NotLoggedIn {
                provider: provider.to_owned(),
            });
        };
        if current.is_valid_for(now_ms, margin_ms) {
            return Ok(current);
        }
        let renewed =
            refresher(&current).map_err(|message| AuthError::RefreshFailed { message })?;
        entries.insert(provider.to_owned(), oauth_json(&renewed));
        self.write(&entries)?;
        Ok(renewed)
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
        let _lock = self.lock(self.lock_wait)?;
        let mut entries = self.read_entries()?;
        let changed = change(&mut entries);
        if changed {
            self.write(&entries)?;
        }
        Ok(changed)
    }

    fn lock(&self, wait: Duration) -> Result<WriteLock, AuthError> {
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
                    if started.elapsed() >= wait {
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

fn oauth_json(credential: &OAuthCredential) -> Value {
    let mut object = Map::new();
    object.insert("type".to_owned(), json!("oauth"));
    object.insert("access".to_owned(), json!(credential.access));
    object.insert("refresh".to_owned(), json!(credential.refresh));
    object.insert("expires".to_owned(), json!(credential.expires_ms));
    for (name, value) in &credential.extra {
        object.insert(name.clone(), value.clone());
    }
    Value::Object(object)
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
            let text = |name: &str| object.get(name)?.as_str().map(str::to_owned);
            let expires = object.get("expires")?.as_f64()?;
            if !expires.is_finite() || expires < 0.0 {
                return None;
            }
            let extra = object
                .iter()
                .filter(|(name, _)| {
                    !matches!(name.as_str(), "type" | "access" | "refresh" | "expires")
                })
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            Some(Credential::OAuth(OAuthCredential {
                access: text("access")?,
                refresh: text("refresh")?,
                // Whole milliseconds; the file stores a JSON number.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                expires_ms: expires as u64,
                extra,
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
