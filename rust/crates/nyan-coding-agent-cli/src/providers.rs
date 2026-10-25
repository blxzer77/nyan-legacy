//! Which provider a run talks to, and with which credential.
//!
//! A provider is one row of the generated table in [`crate::provider_table`]
//! (one wire API for all its models), `cliproxyapi`, or one of the providers
//! that need their own settings: `google-vertex`.
//!
//! The API key comes from `--api-key`, then the provider's environment
//! variables, then the key stored by `nyan login`. `cliproxyapi` also stores a
//! base URL beside its key. There is no default provider or model: a model
//! exists because a login or `--model` named it.
//!
//! Providers whose models mix wire APIs (`openrouter`, `opencode`, ...) are
//! refused rather than guessed: picking the API needs the per-model catalog,
//! which this build does not have.

use std::path::Path;

use nyan_coding_agent_auth::{ApiKeyCredential, AuthStore, Credential};
use nyan_coding_agent_core::CoreError;
use nyan_coding_agent_provider::{ModelProvider, ProviderError, ProviderModel, ProviderRegistry};
use nyan_coding_agent_provider_http::cpa::{Endpoints, resolve_endpoints};
use nyan_coding_agent_provider_http::{
    AnthropicMessagesProvider, GoogleGenerativeAiProvider, GoogleVertexProvider,
    OpenAiChatProvider, OpenAiCodexProvider, OpenAiResponsesProvider, VertexAuth,
};
use tokio::runtime::Handle;

use nyan_coding_agent_models::table::{
    MIXED_API_PROVIDERS, MixedDef, PROVIDERS, ProviderDef, UNSUPPORTED_API_PROVIDERS,
};

use crate::args::Args;
use crate::catalog_access::{Access, load};
use crate::run::Environment;
use crate::settings::read_default_model;

/// The provider id of `CLIProxyAPI`.
pub const CPA_PROVIDER: &str = "cliproxyapi";
/// The credential setting that holds the proxy's base URL.
pub const CPA_BASE_URL_SETTING: &str = "CLIPROXYAPI_BASE_URL";
/// Where a local `CLIProxyAPI` listens by default.
pub const DEFAULT_CPA_BASE_URL: &str = "http://127.0.0.1:8317";

pub use nyan_coding_agent_models::WireApi as Api;

/// A provider ready to be built: its endpoint and credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderSetup {
    /// Chat completions.
    OpenAiCompletions {
        /// Endpoint root.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// Responses (`OpenAI`, xAI, and `CLIProxyAPI`).
    OpenAiResponses {
        /// Endpoint root.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// Anthropic Messages.
    Anthropic {
        /// Endpoint root.
        base_url: String,
        /// API key.
        api_key: String,
        /// The model's output limit, when the catalog knows it.
        max_tokens: Option<u64>,
    },
    /// Google Generative AI.
    Google {
        /// Base URL including the API version.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// `ChatGPT` Codex, signed in with OAuth.
    Codex {
        /// The backend's base URL.
        base_url: String,
        /// The sign-in server, used to refresh the login.
        auth_base: String,
        /// Where the stored login lives.
        agent_dir: std::path::PathBuf,
    },
    /// Google Vertex AI.
    Vertex {
        /// Everything before `/models/`.
        base_url: String,
        /// Authentication.
        auth: VertexAuth,
    },
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

fn env_value(env: &Environment, name: &str) -> Option<String> {
    non_empty((env.var)(name))
}

fn stored_api_key(store: &AuthStore, provider: &str) -> Result<Option<ApiKeyCredential>, String> {
    match store
        .credential(provider)
        .map_err(|error| error.to_string())?
    {
        Some(Credential::ApiKey(credential)) => Ok(Some(credential)),
        Some(Credential::OAuth(_)) => Err(format!(
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
        .or_else(|| env_value(env, "CLIPROXYAPI_API_KEY"))
        .or_else(|| {
            stored
                .as_ref()
                .and_then(|credential| credential.key.clone())
        })
        .ok_or_else(|| {
            "no credential for provider \"cliproxyapi\": run `nyan login cliproxyapi`".to_owned()
        })?;
    let base_url = env_value(env, CPA_BASE_URL_SETTING)
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

/// Every provider id this build can call.
#[must_use]
pub fn supported_providers() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = PROVIDERS.iter().map(|def| def.id).collect();
    ids.push(CPA_PROVIDER);
    ids.push(crate::codex::CODEX_PROVIDER);
    ids.sort_unstable();
    ids
}

/// The table row for `id`.
#[must_use]
pub fn provider_def(id: &str) -> Option<&'static ProviderDef> {
    PROVIDERS.iter().find(|def| def.id == id)
}

/// The API key from the flag, the provider's variables, or the stored login.
fn api_key_for(
    provider: &str,
    env_keys: &[&str],
    args: &Args,
    env: &Environment,
    store: &AuthStore,
) -> Result<String, String> {
    non_empty(args.api_key.clone())
        .or_else(|| env_keys.iter().find_map(|name| env_value(env, name)))
        .or_else(|| {
            stored_api_key(store, provider)
                .ok()
                .flatten()
                .and_then(|credential| credential.key)
        })
        .ok_or_else(|| {
            let hint = if env_keys.is_empty() {
                String::new()
            } else {
                format!("set {}, ", env_keys.join(" or "))
            };
            format!("no API key for provider \"{provider}\": {hint}pass --api-key, or run `nyan login {provider}`")
        })
}

/// Fills `{NAME}` placeholders in a base URL from the environment or the stored
/// credential's settings.
fn expand_template(
    template: &str,
    provider: &str,
    env: &Environment,
    store: &AuthStore,
) -> Result<String, String> {
    let stored = stored_api_key(store, provider).ok().flatten();
    let mut url = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|offset| start + offset)
            .ok_or_else(|| format!("malformed base URL template for \"{provider}\""))?;
        let name = &rest[start + 1..end];
        let value = env_value(env, name)
            .or_else(|| {
                stored
                    .as_ref()
                    .and_then(|credential| credential.env.get(name).cloned())
            })
            .ok_or_else(|| {
                format!("provider \"{provider}\" needs {name}: set it in the environment")
            })?;
        url.push_str(&rest[..start]);
        url.push_str(&value);
        rest = &rest[end + 1..];
    }
    url.push_str(rest);
    Ok(url)
}

/// The provider's base URL: the injected override, else the table's with placeholders filled.
fn base_url_for(def: &ProviderDef, env: &Environment, store: &AuthStore) -> Result<String, String> {
    if let Some(url) = env.base_urls.get(def.id) {
        return Ok(url.clone());
    }
    let template = def
        .base_url
        .ok_or_else(|| format!("provider \"{}\" has no base URL configured", def.id))?;
    expand_template(template, def.id, env, store)
}

fn vertex_setup(
    args: &Args,
    env: &Environment,
    store: &AuthStore,
) -> Result<ProviderSetup, String> {
    let Ok(api_key) = api_key_for("google-vertex", &["GOOGLE_CLOUD_API_KEY"], args, env, store)
    else {
        return Err("no API key for provider \"google-vertex\": set GOOGLE_CLOUD_API_KEY or pass --api-key (Application Default Credentials are not supported yet)".to_owned());
    };
    Ok(ProviderSetup::Vertex {
        base_url: env
            .base_urls
            .get("google-vertex")
            .cloned()
            .unwrap_or_else(|| "https://aiplatform.googleapis.com/v1/publishers/google".to_owned()),
        auth: VertexAuth::ApiKey(api_key),
    })
}

/// A resolved provider and anything to tell the user about how.
#[derive(Debug)]
pub struct Resolved {
    /// The endpoint and credential.
    pub setup: ProviderSetup,
    /// Lines for stderr.
    pub notes: Vec<String>,
}

fn resolved(setup: ProviderSetup) -> Resolved {
    Resolved {
        setup,
        notes: Vec::new(),
    }
}

/// Mixed-API providers this build can call, because the catalog entry for the
/// model says which wire API and URL it uses.
const CATALOG_DRIVEN: &[&str] = &["fireworks", "opencode-go"];

/// Why a mixed-API provider cannot be called yet.
fn mixed_refusal(def: &MixedDef) -> String {
    let reason = match def.id {
        "openrouter" => "its model list comes from a live listing this build does not read yet",
        "github-copilot" => "it needs a GitHub OAuth login, which this build does not have yet",
        "opencode" => "it is not in the supported provider list",
        "cloudflare-ai-gateway" => "its gateway authentication is not implemented yet",
        _ => "its models need the catalog to choose a wire API",
    };
    format!("provider \"{}\" is not supported yet: {reason}", def.id)
}

/// `openai-codex` needs a stored OAuth login; there is no key to pass or set.
fn codex_setup(
    env: &Environment,
    store: &AuthStore,
    agent_dir: &Path,
) -> Result<ProviderSetup, String> {
    match store
        .credential(crate::codex::CODEX_PROVIDER)
        .map_err(|error| error.to_string())?
    {
        Some(Credential::OAuth(_)) => Ok(ProviderSetup::Codex {
            base_url: env
                .base_urls
                .get(crate::codex::CODEX_PROVIDER)
                .cloned()
                .unwrap_or_else(|| nyan_coding_agent_provider_http::codex::DEFAULT_BASE_URL.to_owned()),
            auth_base: crate::codex::auth_base(env),
            agent_dir: agent_dir.to_path_buf(),
        }),
        Some(Credential::ApiKey(_)) => Err(
            "the stored login for \"openai-codex\" is an API key, but Codex needs a ChatGPT sign-in: run `nyan login openai-codex`".to_owned(),
        ),
        None => Err("not logged in to openai-codex: run `nyan login openai-codex`".to_owned()),
    }
}

/// Resolves the endpoint and credential for `provider` serving `model`.
///
/// # Errors
///
/// Returns a message when the provider is unknown, unsupported, has no
/// credential, or its settings are unusable.
pub fn resolve_provider(
    provider: &str,
    model: &str,
    args: &Args,
    env: &Environment,
    agent_dir: &Path,
) -> Result<Resolved, String> {
    let store = AuthStore::new(agent_dir);
    match provider {
        CPA_PROVIDER => {
            let connection = cpa_connection(args.api_key.as_deref(), env, agent_dir)?;
            Ok(resolved(ProviderSetup::OpenAiResponses {
                base_url: connection.endpoints.inference_base_url,
                api_key: connection.api_key,
            }))
        }
        "google-vertex" => vertex_setup(args, env, &store).map(resolved),
        crate::codex::CODEX_PROVIDER => codex_setup(env, &store, agent_dir).map(resolved),
        _ => {
            if let Some(def) = provider_def(provider) {
                return simple_setup(def, model, args, env, &store, agent_dir);
            }
            if let Some(def) = MIXED_API_PROVIDERS.iter().find(|def| def.id == provider) {
                if CATALOG_DRIVEN.contains(&provider) {
                    return catalog_setup(def, model, args, env, &store, agent_dir);
                }
                return Err(mixed_refusal(def));
            }
            if let Some((_, api)) = UNSUPPORTED_API_PROVIDERS
                .iter()
                .find(|(id, _)| *id == provider)
            {
                return Err(format!(
                    "provider \"{provider}\" uses the {api} API, which this build does not support yet"
                ));
            }
            Err(format!(
                "provider \"{provider}\" is not supported by the Rust preview yet (supported: {})",
                supported_providers().join(", ")
            ))
        }
    }
}

/// A mixed-API provider: the catalog entry for the model picks the wire API and URL.
fn catalog_setup(
    def: &MixedDef,
    model: &str,
    args: &Args,
    env: &Environment,
    store: &AuthStore,
    agent_dir: &Path,
) -> Result<Resolved, String> {
    let api_key = api_key_for(def.id, def.env_keys, args, env, store)?;
    let loaded = load(env, agent_dir, Access::RefreshIfStale)?;
    let entry = loaded.catalog.find(def.id, model).ok_or_else(|| {
        format!(
            "model \"{model}\" is not in the catalog for provider \"{}\"; `nyan --list-models` shows what is",
            def.id
        )
    })?;
    let base_url = match env.base_urls.get(def.id) {
        Some(url) => url.clone(),
        None => expand_template(&entry.base_url, def.id, env, store)?,
    };
    Ok(Resolved {
        setup: setup_for_api(entry.api, base_url, api_key, Some(entry.max_tokens))?,
        notes: loaded.notes,
    })
}

fn setup_for_api(
    api: Api,
    base_url: String,
    api_key: String,
    max_tokens: Option<u64>,
) -> Result<ProviderSetup, String> {
    Ok(match api {
        Api::OpenAiCompletions => ProviderSetup::OpenAiCompletions { base_url, api_key },
        Api::OpenAiResponses => ProviderSetup::OpenAiResponses { base_url, api_key },
        Api::AnthropicMessages => ProviderSetup::Anthropic {
            base_url,
            api_key,
            max_tokens,
        },
        Api::GoogleGenerativeAi => ProviderSetup::Google { base_url, api_key },
        Api::GoogleVertex | Api::OpenAiCodexResponses => {
            return Err("this provider needs its own settings".to_owned());
        }
    })
}

/// A single-API provider. The model's output limit, which Anthropic requires on
/// every request, is read from the cache when there is one; a request never
/// waits for the network just to learn it.
fn simple_setup(
    def: &ProviderDef,
    model: &str,
    args: &Args,
    env: &Environment,
    store: &AuthStore,
    agent_dir: &Path,
) -> Result<Resolved, String> {
    let env_keys: &[&str] = if def.id == "anthropic" {
        &["ANTHROPIC_API_KEY"]
    } else {
        def.env_keys
    };
    let api_key = api_key_for(def.id, env_keys, args, env, store)?;
    if def.id == "anthropic" && api_key.contains("sk-ant-oat") {
        return Err("this Anthropic credential is an OAuth token, which this build does not support yet; use an API key".to_owned());
    }
    let base_url = base_url_for(def, env, store)?;
    let max_tokens = if def.api == Api::AnthropicMessages {
        load(env, agent_dir, Access::CacheOnly)
            .ok()
            .and_then(|loaded| {
                loaded
                    .catalog
                    .find(def.id, model)
                    .map(|entry| entry.max_tokens)
            })
    } else {
        None
    };
    Ok(resolved(setup_for_api(
        def.api, base_url, api_key, max_tokens,
    )?))
}

/// Whether `provider` has a credential this build could use, without making a request.
#[must_use]
pub fn credential_available(provider: &str, env: &Environment, agent_dir: &Path) -> bool {
    let store = AuthStore::new(agent_dir);
    let no_flags = crate::args::parse_args(&[]);
    match provider {
        CPA_PROVIDER => cpa_connection(None, env, agent_dir).is_ok(),
        "google-vertex" => vertex_setup(&no_flags, env, &store).is_ok(),
        crate::codex::CODEX_PROVIDER => matches!(
            store.credential(crate::codex::CODEX_PROVIDER),
            Ok(Some(Credential::OAuth(_)))
        ),
        other => {
            let env_keys: &[&str] = if other == "anthropic" {
                &["ANTHROPIC_API_KEY"]
            } else if let Some(def) = provider_def(other) {
                def.env_keys
            } else if let Some(def) = MIXED_API_PROVIDERS.iter().find(|def| def.id == other) {
                def.env_keys
            } else {
                return false;
            };
            api_key_for(other, env_keys, &no_flags, env, &store).is_ok()
        }
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
    let session = session_header(id);
    let provider: Box<dyn ModelProvider> = match setup {
        ProviderSetup::OpenAiCompletions { base_url, api_key } => {
            let provider = OpenAiChatProvider::new(id, base_url, Some(api_key), handle)?;
            Box::new(match session {
                Some(name) => provider.with_session_header(name),
                None => provider,
            })
        }
        ProviderSetup::OpenAiResponses { base_url, api_key } => {
            let provider = OpenAiResponsesProvider::new(id, base_url, Some(api_key), handle)?;
            Box::new(match session {
                Some(name) => provider.with_session_header(name),
                None => provider,
            })
        }
        ProviderSetup::Anthropic {
            base_url,
            api_key,
            max_tokens,
        } => {
            let mut provider = AnthropicMessagesProvider::new(id, base_url, Some(api_key), handle)?;
            if let Some(limit) = max_tokens {
                provider = provider.with_max_tokens(limit);
            }
            if let Some(name) = session {
                provider = provider.with_session_header(name);
            }
            Box::new(provider)
        }
        ProviderSetup::Google { base_url, api_key } => {
            let provider = GoogleGenerativeAiProvider::new(id, base_url, Some(api_key), handle)?;
            Box::new(match session {
                Some(name) => provider.with_session_header(name),
                None => provider,
            })
        }
        ProviderSetup::Vertex { base_url, auth } => {
            Box::new(GoogleVertexProvider::new(id, base_url, Some(auth), handle)?)
        }
        ProviderSetup::Codex {
            base_url,
            auth_base,
            agent_dir,
        } => {
            let auth = std::sync::Arc::new(crate::codex::StoredCodexAuth::new(
                &agent_dir,
                auth_base,
                handle.clone(),
            ));
            Box::new(OpenAiCodexProvider::new(id, &base_url, auth, handle)?)
        }
    };
    Ok(SingleProvider(provider))
}

/// The header a provider wants the conversation id in, if any.
///
/// `OpenCode` routes by conversation and requires `x-opencode-session` on every
/// request, whichever wire API the model uses.
#[must_use]
pub const fn session_header(provider: &str) -> Option<&'static str> {
    // `str` equality is not const, so compare bytes.
    let name = provider.as_bytes();
    if matches!(name, b"opencode-go" | b"opencode") {
        Some("x-opencode-session")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::{ProviderSetup, resolve_provider};
    use crate::args::parse_args;
    use crate::run::Environment;

    fn environment(variables: &[(&str, &str)]) -> Environment {
        let variables: BTreeMap<String, String> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Environment {
            var: Box::new(move |name| variables.get(name).cloned()),
            stdin_is_tty: false,
            stdout_is_tty: false,
            cwd: Path::new("/tmp").to_path_buf(),
            base_urls: BTreeMap::new(),
            models_dev_url: "http://127.0.0.1:1/api.json".to_owned(),
            open_url: Box::new(|_| {}),
            oauth_callback_port: 0,
            read_stdin: Box::new(|| None),
            prompt: std::sync::Arc::new(|_, _| None),
        }
    }

    fn resolve(provider: &str, variables: &[(&str, &str)]) -> Result<ProviderSetup, String> {
        resolve_provider(
            provider,
            "some-model",
            &parse_args(&[]),
            &environment(variables),
            Path::new("/nonexistent-agent-dir"),
        )
        .map(|resolved| resolved.setup)
    }

    #[test]
    fn every_generated_provider_resolves_with_its_documented_environment_key() {
        for def in nyan_coding_agent_models::table::PROVIDERS {
            if def.id == "google-vertex" {
                continue;
            }
            let key = if def.id == "anthropic" {
                "ANTHROPIC_API_KEY"
            } else {
                def.env_keys[0]
            };
            let setup = resolve(
                def.id,
                &[(key, "the-key"), ("CLOUDFLARE_ACCOUNT_ID", "acct")],
            )
            .unwrap_or_else(|error| panic!("{}: {error}", def.id));
            let (ProviderSetup::OpenAiCompletions { base_url, api_key }
            | ProviderSetup::OpenAiResponses { base_url, api_key }
            | ProviderSetup::Anthropic {
                base_url, api_key, ..
            }
            | ProviderSetup::Google { base_url, api_key }) = setup
            else {
                panic!("{}: unexpected setup", def.id);
            };
            assert_eq!(api_key, "the-key", "{}", def.id);
            let expected = def
                .base_url
                .map(|url| url.replace("{CLOUDFLARE_ACCOUNT_ID}", "acct"));
            assert_eq!(Some(base_url), expected, "{}", def.id);
        }
    }
}

#[cfg(test)]
mod template_tests {
    use super::{ProviderSetup, Resolved, resolve_provider};
    use crate::args::parse_args;
    use crate::run::Environment;
    use std::collections::BTreeMap;
    use std::path::Path;

    fn env(variables: &[(&str, &str)]) -> Environment {
        let variables: BTreeMap<String, String> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Environment {
            var: Box::new(move |name| variables.get(name).cloned()),
            stdin_is_tty: false,
            stdout_is_tty: false,
            cwd: Path::new("/tmp").to_path_buf(),
            base_urls: BTreeMap::new(),
            models_dev_url: "http://127.0.0.1:1/api.json".to_owned(),
            open_url: Box::new(|_| {}),
            oauth_callback_port: 0,
            read_stdin: Box::new(|| None),
            prompt: std::sync::Arc::new(|_, _| None),
        }
    }

    #[test]
    fn a_url_template_is_filled_from_the_environment_or_refused_by_name() {
        let args = parse_args(&[]);
        let dir = Path::new("/nonexistent-agent-dir");
        let Ok(Resolved {
            setup: ProviderSetup::OpenAiCompletions { base_url, .. },
            ..
        }) = resolve_provider(
            "cloudflare-workers-ai",
            "m",
            &args,
            &env(&[
                ("CLOUDFLARE_API_KEY", "k"),
                ("CLOUDFLARE_ACCOUNT_ID", "acct123"),
            ]),
            dir,
        )
        else {
            panic!("must resolve");
        };
        assert_eq!(
            base_url,
            "https://api.cloudflare.com/client/v4/accounts/acct123/ai/v1"
        );
        let missing = resolve_provider(
            "cloudflare-workers-ai",
            "m",
            &args,
            &env(&[("CLOUDFLARE_API_KEY", "k")]),
            dir,
        )
        .expect_err("account id is required");
        assert_eq!(
            missing,
            "provider \"cloudflare-workers-ai\" needs CLOUDFLARE_ACCOUNT_ID: set it in the environment"
        );
    }
}
