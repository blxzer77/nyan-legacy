//! The on-disk copy of the models.dev data: `<agent dir>/models-dev-cache.json`.
//!
//! The file is the one the TypeScript CLI writes, `{"timestamp": <ms>,
//! "providers": {...}}`, with the same 24-hour freshness window, so either CLI
//! can use what the other fetched. A missing, unreadable, or malformed file is
//! simply "no cache": it is replaced by the next successful fetch and never
//! reported as an error.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// How long fetched data counts as fresh.
pub const TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// The public endpoint the data comes from.
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// A cached copy of the models.dev provider map.
#[derive(Clone, Debug, PartialEq)]
pub struct Cached {
    /// When it was fetched, in Unix milliseconds.
    pub timestamp_ms: u64,
    /// The provider map: provider key to `{models: {...}, ...}`.
    pub providers: Map<String, Value>,
}

impl Cached {
    /// Whether the copy is younger than [`TTL_MS`] at `now_ms`.
    #[must_use]
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.timestamp_ms) < TTL_MS
    }
}

/// Where the cache lives.
#[must_use]
pub fn path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("models-dev-cache.json")
}

/// Reads the cache, if there is a valid one.
#[must_use]
pub fn read(agent_dir: &Path) -> Option<Cached> {
    let text = fs::read_to_string(path(agent_dir)).ok()?;
    let Value::Object(mut root) = serde_json::from_str::<Value>(&text).ok()? else {
        return None;
    };
    let timestamp_ms = root.get("timestamp")?.as_u64()?;
    let Value::Object(providers) = root.remove("providers")? else {
        return None;
    };
    Some(Cached {
        timestamp_ms,
        providers,
    })
}

/// Replaces the cache atomically: a temporary file in the same directory is
/// renamed over the old one, so a crash leaves either the old file or the new.
///
/// # Errors
///
/// Returns a message when the directory or file cannot be written.
pub fn write(
    agent_dir: &Path,
    providers: &Map<String, Value>,
    timestamp_ms: u64,
) -> Result<(), String> {
    fs::create_dir_all(agent_dir)
        .map_err(|error| format!("could not create {}: {error}", agent_dir.display()))?;
    let destination = path(agent_dir);
    let temporary = destination.with_extension(format!("json.tmp-{}", std::process::id()));
    let mut root = Map::new();
    root.insert("timestamp".to_owned(), Value::from(timestamp_ms));
    root.insert("providers".to_owned(), Value::Object(providers.clone()));
    let text = serde_json::to_string(&Value::Object(root)).map_err(|error| error.to_string())?;
    let result = fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, &destination));
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "could not write {}: {error}",
            destination.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
