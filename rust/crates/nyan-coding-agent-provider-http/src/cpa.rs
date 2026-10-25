//! `CLIProxyAPI` endpoints and model catalog.
//!
//! The proxy serves its catalog at `{root}/v1/models?client_version=pi` and the
//! models themselves over the Responses API at `{root}/backend-api/codex`. A
//! user may type any of those URLs; [`resolve_endpoints`] reduces them to the
//! server root. Everything here follows `extensions/cliproxyapi/{config,catalog}.ts`.
//!
//! Not ported yet: per-model overrides (`pin`, `hidden`, context and cost
//! overrides), pricing, thinking-level maps, the disk cache, and the catalog
//! fields that only `/cpa-models` displayed.

use std::time::Duration;

use serde_json::Value;
use tokio::runtime::Handle;

/// The query value that makes the proxy return its extended metadata.
const CLIENT_VERSION: &str = "pi";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_CHARS: usize = 200;
/// Fallbacks when the catalog omits a usable value.
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;
const DEFAULT_MAX_TOKENS: u64 = 16_384;

/// The URLs derived from one base URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Endpoints {
    /// The server root, without a trailing slash.
    pub root: String,
    /// The Responses base URL the models are called through.
    pub inference_base_url: String,
    /// The catalog URL.
    pub models_url: String,
}

/// Reduces a typed base URL to the proxy's endpoints.
///
/// A missing scheme means `http`, and a pasted catalog or inference URL is cut
/// back to the root.
///
/// # Errors
///
/// Returns a message when the input is empty or not a URL.
pub fn resolve_endpoints(input: &str) -> Result<Endpoints, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("baseUrl is empty".to_owned());
    }
    let lower = raw.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let url = reqwest::Url::parse(&with_scheme).map_err(|error| format!("{raw}: {error}"))?;
    if url.host_str().is_none() {
        return Err(format!("{raw}: the URL has no host"));
    }
    let mut path = url.path().trim_end_matches('/').to_owned();
    for suffix in ["/v1/models", "/v1"] {
        if let Some(stripped) = path.strip_suffix(suffix) {
            path = stripped.to_owned();
            break;
        }
    }
    for suffix in [
        "/backend-api/codex/responses",
        "/backend-api/codex",
        "/backend-api",
    ] {
        if let Some(stripped) = path.strip_suffix(suffix) {
            path = stripped.to_owned();
            break;
        }
    }
    let origin = url.origin().ascii_serialization();
    let root = format!("{origin}{path}");
    let mut models_path = format!("{path}/v1/models");
    while models_path.contains("//") {
        models_path = models_path.replace("//", "/");
    }
    Ok(Endpoints {
        inference_base_url: format!("{root}/backend-api/codex"),
        models_url: format!("{origin}{models_path}?client_version={CLIENT_VERSION}"),
        root,
    })
}

/// One model the proxy offers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogModel {
    /// The id requests use.
    pub id: String,
    /// The display name.
    pub name: String,
    /// Context window in tokens.
    pub context_window: u64,
    /// Output limit in tokens.
    pub max_tokens: u64,
    /// Whether the model reasons (it lists a level other than `none`).
    pub reasoning: bool,
    /// Reasoning levels the model supports, lowercase, in catalog order.
    pub reasoning_levels: Vec<String>,
    /// Whether the model accepts image input.
    pub image_input: bool,
}

/// Why a catalog request failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    /// The proxy answered with a non-success status.
    Http {
        /// HTTP status.
        status: u16,
        /// The start of the response body.
        body: String,
    },
    /// The request did not complete.
    Transport {
        /// The diagnostic.
        message: String,
    },
}

impl CatalogError {
    /// Whether the proxy rejected the credential.
    #[must_use]
    pub const fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: 401 | 403,
                ..
            }
        )
    }
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { status, body } if body.is_empty() => {
                write!(formatter, "the models request failed with HTTP {status}")
            }
            Self::Http { status, body } => {
                write!(
                    formatter,
                    "the models request failed with HTTP {status}: {body}"
                )
            }
            Self::Transport { message } => {
                write!(formatter, "the models request failed: {message}")
            }
        }
    }
}

impl std::error::Error for CatalogError {}

/// Fetches and maps the catalog. A 200 is success even with an empty or
/// non-JSON body, which yields no models.
///
/// # Errors
///
/// Returns [`CatalogError`] on a non-success status or a transport failure.
pub async fn fetch_catalog(
    handle: &Handle,
    models_url: &str,
    api_key: &str,
) -> Result<Vec<CatalogModel>, CatalogError> {
    let url = models_url.to_owned();
    let key = api_key.to_owned();
    let task = handle.spawn(async move {
        let client = reqwest::Client::builder()
            .user_agent(concat!("nyan-coding-agent/", env!("CARGO_PKG_VERSION")))
            .gzip(false)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| CatalogError::Transport {
                message: error.to_string(),
            })?;
        let response = client
            .get(url)
            .bearer_auth(key)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|error| CatalogError::Transport {
                message: error.to_string(),
            })?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| CatalogError::Transport {
                message: error.to_string(),
            })?;
        if !status.is_success() {
            return Err(CatalogError::Http {
                status: status.as_u16(),
                body: text.chars().take(MAX_ERROR_BODY_CHARS).collect(),
            });
        }
        Ok(text)
    });
    let text = task.await.map_err(|error| CatalogError::Transport {
        message: format!("the request task failed: {error}"),
    })??;
    Ok(parse_catalog(&text))
}

/// Maps a catalog response body to usable models.
#[must_use]
pub fn parse_catalog(body: &str) -> Vec<CatalogModel> {
    let Ok(payload) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let entries = match &payload {
        Value::Array(entries) => Some(entries),
        Value::Object(object) => object
            .get("models")
            .and_then(Value::as_array)
            .or_else(|| object.get("data").and_then(Value::as_array)),
        _ => None,
    };
    entries
        .into_iter()
        .flatten()
        .filter_map(map_model)
        .collect()
}

/// The first listed field that holds a whole number of at least 1.
fn positive(entry: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| entry.get(*name)?.as_u64().filter(|number| *number >= 1))
}

fn map_model(entry: &Value) -> Option<CatalogModel> {
    let text = |name: &str| entry.get(name).and_then(Value::as_str).map(str::trim);
    let id = text("slug")
        .filter(|id| !id.is_empty())
        .or_else(|| text("id"))?;
    if id.is_empty() {
        return None;
    }
    if text("visibility").is_some_and(|visibility| visibility.eq_ignore_ascii_case("hide")) {
        return None;
    }
    let name = text("display_name")
        .filter(|name| !name.is_empty())
        .or_else(|| text("name").filter(|name| !name.is_empty()))
        .unwrap_or(id);
    let mut reasoning_levels: Vec<String> = Vec::new();
    for level in entry
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let effort = level
            .as_str()
            .or_else(|| level.get("effort").and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !effort.is_empty() && !reasoning_levels.contains(&effort) {
            reasoning_levels.push(effort);
        }
    }
    let image_input = entry
        .get("input_modalities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|modality| modality.trim().eq_ignore_ascii_case("image"));
    Some(CatalogModel {
        id: id.to_owned(),
        name: name.to_owned(),
        context_window: positive(entry, &["context_window", "max_context_window"])
            .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        max_tokens: positive(
            entry,
            &["max_tokens", "max_output_tokens", "max_completion_tokens"],
        )
        .unwrap_or(DEFAULT_MAX_TOKENS),
        reasoning: reasoning_levels.iter().any(|level| level != "none"),
        reasoning_levels,
        image_input,
    })
}
