//! Which provider a run talks to, and with which credential.
//!
//! Resolution per provider, strongest first:
//! - `cliproxyapi`: `--api-key`, then `CLIPROXYAPI_API_KEY`, then the key stored
//!   by `nyan login`; the base URL from `CLIPROXYAPI_BASE_URL`, then the one
//!   stored with the key, then `http://127.0.0.1:8317`.
//! - `openai`: `--api-key`, then `OPENAI_API_KEY`, then the stored key.
//!
//! There is no default provider or model: a model exists because a login or
//! `--model` named it.

use std::path::Path;

use nyan_coding_agent_auth::{AuthStore, Credential};
use nyan_coding_agent_core::CoreError;
use nyan_coding_agent_provider::{ModelProvider, ProviderError, ProviderModel, ProviderRegistry};
use nyan_coding_agent_provider_http::cpa::{Endpoints, resolve_endpoints};
use nyan_coding_agent_provider_http::{OpenAiChatProvider, OpenAiResponsesProvider};
use tokio::runtime::Handle;

use crate::args::Args;
use crate::run::Environment;
use crate::settings::read_default_model;

/// The provider id of `CLIProxyAPI`.
pub const CPA_PROVIDER: &str = "cliproxyapi";
/// The credential setting that holds the proxy's base URL.
pub const CPA_BASE_URL_SETTING: &str = "CLIPROXYAPI_BASE_URL";
/// Where a local `CLIProxyAPI` listens by default.
pub const DEFAULT_CPA_BASE_URL: &str = "http://127.0.0.1:8317";
/// The providers this build can call.
pub const SUPPORTED_PROVIDERS: [&str; 2] = ["openai", CPA_PROVIDER];

/// A provider ready to be built: its endpoint and credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderSetup {
    /// `OpenAI` chat completions.
    OpenAiChat {
        /// Endpoint root, for example `https://api.openai.com/v1`.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// `CLIProxyAPI` over the Responses API.
    Cpa {
        /// The `/backend-api/codex` base URL.
        inference_base_url: String,
        /// API key.
        api_key: String,
    },
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

fn stored_api_key(
    store: &AuthStore,
    provider: &str,
) -> Result<Option<nyan_coding_agent_auth::ApiKeyCredential>, String> {
    match store
        .credential(provider)
        .map_err(|error| error.to_string())?
    {
        Some(Credential::ApiKey(credential)) => Ok(Some(credential)),
        Some(Credential::OAuth) => Err(format!(
            "the stored login for \"{provider}\" is an OAuth login made by another CLI, which this build cannot use yet; run `nyan login {provider}` to store an API key"
        )),
        None => Ok(None),
    }
}

/// The key and endpoints of the `CLIProxyAPI` connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpaConnection {
    /// The key to authenticate with.
    pub api_key: String,
    /// Where the catalog and the models live.
    pub endpoints: Endpoints,
}

/// Resolves the `CLIProxyAPI` key and base URL from the flag, the environment, and the stored login.
///
/// # Errors
///
/// Returns a message when there is no credential or the base URL is invalid.
pub fn cpa_connection(
    flag_key: Option<&str>,
    env: &Environment,
    agent_dir: &Path,
) -> Result<CpaConnection, String> {
    let stored = stored_api_key(&AuthStore::new(agent_dir), CPA_PROVIDER)?;
    let api_key = non_empty(flag_key.map(str::to_owned))
        .or_else(|| non_empty((env.var)("CLIPROXYAPI_API_KEY")))
        .or_else(|| {
            stored
                .as_ref()
                .and_then(|credential| credential.key.clone())
        })
        .ok_or_else(|| {
            "no credential for provider \"cliproxyapi\": run `nyan login cliproxyapi`".to_owned()
        })?;
    let base_url = non_empty((env.var)(CPA_BASE_URL_SETTING))
        .or_else(|| {
            stored
                .as_ref()
                .and_then(|credential| credential.env.get(CPA_BASE_URL_SETTING).cloned())
        })
        .unwrap_or_else(|| DEFAULT_CPA_BASE_URL.to_owned());
    let endpoints = resolve_endpoints(&base_url)
        .map_err(|message| format!("the CLIProxyAPI base URL is not valid ({message})"))?;
    Ok(CpaConnection { api_key, endpoints })
}

/// Resolves the endpoint and credential for `provider`.
///
/// # Errors
///
/// Returns a message when the provider is unsupported, has no credential, or its
/// stored settings are unusable.
pub fn resolve_provider(
    provider: &str,
    args: &Args,
    env: &Environment,
    agent_dir: &Path,
) -> Result<ProviderSetup, String> {
    let store = AuthStore::new(agent_dir);
    match provider {
        "openai" => {
            let api_key = non_empty(args.api_key.clone())
                .or_else(|| non_empty((env.var)("OPENAI_API_KEY")))
                .or_else(|| {
                    stored_api_key(&store, "openai")
                        .ok()
                        .flatten()
                        .and_then(|credential| credential.key)
                })
                .ok_or_else(|| {
                    "no API key for provider \"openai\": set OPENAI_API_KEY, pass --api-key, or run `nyan login openai`".to_owned()
                })?;
            Ok(ProviderSetup::OpenAiChat {
                base_url: env.openai_base_url.clone(),
                api_key,
            })
        }
        CPA_PROVIDER => {
            let connection = cpa_connection(args.api_key.as_deref(), env, agent_dir)?;
            Ok(ProviderSetup::Cpa {
                inference_base_url: connection.endpoints.inference_base_url,
                api_key: connection.api_key,
            })
        }
        other => Err(format!(
            "provider \"{other}\" is not supported by the Rust preview yet (supported: {})",
            SUPPORTED_PROVIDERS.join(", ")
        )),
    }
}

/// Chooses the model: `--model`, else the saved default.
///
/// # Errors
///
/// Returns a message that says what to do next when no model can be chosen.
pub fn resolve_model(args: &Args, agent_dir: &Path) -> Result<ProviderModel, String> {
    if let Some(model) = args.model.as_deref() {
        return match args.provider.as_deref() {
            Some(provider) => Ok(ProviderModel {
                provider: provider.to_owned(),
                model: model.to_owned(),
            }),
            None => model
                .split_once('/')
                .filter(|(provider, id)| !provider.is_empty() && !id.is_empty())
                .map(|(provider, id)| ProviderModel {
                    provider: provider.to_owned(),
                    model: id.to_owned(),
                })
                .ok_or_else(|| {
                    "--model needs a provider: pass --provider or use \"provider/model\"".to_owned()
                }),
        };
    }
    if let Some(provider) = args.provider.as_deref() {
        return Err(format!(
            "--provider {provider} needs --model: the model is chosen from the provider's own list"
        ));
    }
    if let Some(default) = read_default_model(agent_dir)? {
        return Ok(ProviderModel {
            provider: default.provider,
            model: default.model,
        });
    }
    let logged_in = AuthStore::new(agent_dir)
        .providers()
        .map_err(|error| error.to_string())?;
    Err(if logged_in.is_empty() {
        "no model selected and nothing is logged in: run `nyan login`, then choose a default model"
            .to_owned()
    } else {
        "no model selected: pass --model <provider>/<id>, or run `nyan login <provider>` to choose a default from its model list".to_owned()
    })
}

/// A registry holding one provider.
pub struct SingleProvider(Box<dyn ModelProvider>);

impl std::fmt::Debug for SingleProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SingleProvider")
            .field("id", &self.0.id())
            .finish()
    }
}

impl ProviderRegistry for SingleProvider {
    fn provider(&self, id: &str) -> Result<&dyn ModelProvider, CoreError> {
        if id == self.0.id() {
            Ok(self.0.as_ref())
        } else {
            Err(CoreError::Unsupported {
                operation: format!("provider {id}"),
            })
        }
    }
}

/// Builds the provider for `setup`, registered under `id`.
///
/// # Errors
///
/// Returns [`ProviderError`] when the HTTP client cannot be built.
pub fn build_provider(
    id: &str,
    setup: ProviderSetup,
    handle: Handle,
) -> Result<SingleProvider, ProviderError> {
    let provider: Box<dyn ModelProvider> = match setup {
        ProviderSetup::OpenAiChat { base_url, api_key } => Box::new(OpenAiChatProvider::new(
            id,
            base_url,
            Some(api_key),
            handle,
        )?),
        ProviderSetup::Cpa {
            inference_base_url,
            api_key,
        } => Box::new(OpenAiResponsesProvider::new(
            id,
            inference_base_url,
            Some(api_key),
            handle,
        )?),
    };
    Ok(SingleProvider(provider))
}
