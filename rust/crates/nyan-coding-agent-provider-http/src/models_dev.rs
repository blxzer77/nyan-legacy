//! Fetching the public models.dev provider map.
//!
//! The endpoint is third-party and read-only. The body must be a JSON object;
//! anything else is a failure, never an empty catalog.
//!
//! The data is about 6 MB uncompressed and 600 KB gzipped, and over a slow link
//! or a proxy the difference decides whether the fetch finishes, so this request
//! (unlike the model requests) asks for gzip. The deadlines are 8 seconds to
//! connect, 8 seconds of silence between reads, and 60 seconds in all: the
//! TypeScript CLI's flat 8 seconds fits a background refresh, but this fetch is
//! one the caller waits for.

use std::time::Duration;

use serde_json::{Map, Value};
use tokio::runtime::Handle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const READ_TIMEOUT: Duration = Duration::from_secs(8);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
/// The data is about 6 MB; this bounds a misbehaving server.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Why the fetch failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchError(pub String);

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FetchError {}

/// Downloads `url` and returns the provider map.
///
/// # Errors
///
/// Returns [`FetchError`] on a transport failure, a non-success status, an
/// oversized body, or a body that is not a JSON object.
pub async fn fetch(handle: &Handle, url: &str) -> Result<Map<String, Value>, FetchError> {
    let url = url.to_owned();
    let task = handle.spawn(async move {
        let client = reqwest::Client::builder()
            .user_agent(concat!("nyan-coding-agent/", env!("CARGO_PKG_VERSION")))
            .gzip(true)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .build()
            .map_err(|error| FetchError(format!("could not build the HTTP client: {error}")))?;
        let response = client
            .get(&url)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|error| FetchError(format!("could not reach {url}: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError(format!(
                "{url} answered HTTP {}",
                status.as_u16()
            )));
        }
        let bytes = response.bytes().await.map_err(|error| {
            FetchError(format!("could not read the response from {url}: {error}"))
        })?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(FetchError(format!(
                "{url} sent more than {MAX_BODY_BYTES} bytes"
            )));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(providers)) => Ok(providers),
            Ok(_) => Err(FetchError(format!("{url} did not return a JSON object"))),
            Err(error) => Err(FetchError(format!("{url} returned invalid JSON: {error}"))),
        }
    });
    task.await
        .map_err(|error| FetchError(format!("the fetch task failed: {error}")))?
}
