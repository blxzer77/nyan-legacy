//! Getting the model catalog: from the cache, or from models.dev when the cache
//! is missing or older than 24 hours.
//!
//! Two modes, by what the caller needs:
//! - [`Access::CacheOnly`] never touches the network. It serves a run whose
//!   provider needs nothing from the catalog, such as the output limit of a
//!   model; with no cache it returns an empty catalog.
//! - [`Access::RefreshIfStale`] serves a caller that cannot work without the
//!   catalog: a model list, or a provider whose models mix wire APIs. It
//!   fetches when the cache is not fresh, and falls back to a stale cache with a
//!   note when the fetch fails.

use std::path::Path;

use nyan_coding_agent_core::now_ms;
use nyan_coding_agent_models::cache::{self, Cached};
use nyan_coding_agent_models::catalog::Catalog;
use nyan_coding_agent_provider_http::models_dev;

use crate::run::Environment;

/// How much the caller needs the network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    /// Use whatever is cached; never fetch.
    CacheOnly,
    /// Fetch when the cache is missing or stale.
    RefreshIfStale,
}

/// A catalog and anything the caller should tell the user about it.
#[derive(Debug)]
pub struct Loaded {
    /// The catalog; without a cache in [`Access::CacheOnly`] it holds only the built-in lists.
    pub catalog: Catalog,
    /// Lines for stderr, such as "using data from 3 days ago".
    pub notes: Vec<String>,
}

fn describe_age(timestamp_ms: u64, now: u64) -> String {
    let hours = now.saturating_sub(timestamp_ms) / 3_600_000;
    if hours >= 48 {
        format!("{} days old", hours / 24)
    } else {
        format!("{hours} hours old")
    }
}

/// Loads the catalog.
///
/// # Errors
///
/// Returns a message only for [`Access::RefreshIfStale`] when there is no usable
/// cache and the fetch failed.
pub fn load(env: &Environment, agent_dir: &Path, access: Access) -> Result<Loaded, String> {
    let now = now_ms();
    let cached = cache::read(agent_dir);
    let from = |cached: &Cached, notes: Vec<String>| Loaded {
        catalog: Catalog::from_models_dev(&cached.providers),
        notes,
    };
    match (&cached, access) {
        (Some(cached), Access::CacheOnly) => return Ok(from(cached, Vec::new())),
        (None, Access::CacheOnly) => {
            // Not empty: the hand-written Codex list needs no models.dev data.
            return Ok(Loaded {
                catalog: Catalog::from_models_dev(&serde_json::Map::new()),
                notes: Vec::new(),
            });
        }
        (Some(cached), Access::RefreshIfStale) if cached.is_fresh(now) => {
            return Ok(from(cached, Vec::new()));
        }
        _ => {}
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the async runtime: {error}"))?;
    match runtime.block_on(models_dev::fetch(runtime.handle(), &env.models_dev_url)) {
        Ok(providers) => {
            let mut notes = Vec::new();
            if let Err(message) = cache::write(agent_dir, &providers, now) {
                notes.push(format!("Warning: could not save the model data: {message}"));
            }
            Ok(Loaded {
                catalog: Catalog::from_models_dev(&providers),
                notes,
            })
        }
        Err(error) => match cached {
            Some(cached) => Ok(from(
                &cached,
                vec![format!(
                    "Warning: could not refresh the model data ({error}); using data that is {}",
                    describe_age(cached.timestamp_ms, now)
                )],
            )),
            None => Err(format!(
                "the model data is unavailable: {error}. It is needed for this command; connect to the network and try again"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::describe_age;

    #[test]
    fn ages_read_as_hours_then_days() {
        assert_eq!(describe_age(0, 5 * 3_600_000), "5 hours old");
        assert_eq!(describe_age(0, 47 * 3_600_000), "47 hours old");
        assert_eq!(describe_age(0, 72 * 3_600_000), "3 days old");
        assert_eq!(describe_age(10, 5), "0 hours old");
    }
}
