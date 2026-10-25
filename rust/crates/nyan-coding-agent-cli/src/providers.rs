//! Which provider a run talks to, and with which credential.
//!
//! A provider is one row of the generated table in [`crate::provider_table`]
//! (one wire API for all its models), `cliproxyapi`, or one of the providers
//! that need their own settings: `azure`, `amazon-bedrock`, `google-vertex`.
//!
//! The API key comes from `--api-key`, then the provider's environment
//! variables, then the key stored by `nyan login`. `cliproxyapi` also stores a
//! base URL beside its key. There is no default provider or model: a model
//! exists because a login or `--model` named it.
//!
//! Providers whose models mix wire APIs (`openrouter`, `opencode`, ...) are
//! refused rather than guessed: picking the API needs the per-model catalog,
//! which this build does not have.

use std::collections::BTreeMap;
use std::path::Path;

use nyan_coding_agent_auth::{ApiKeyCredential, AuthStore, Credential};
use nyan_coding_agent_core::CoreError;
use nyan_coding_agent_provider::{ModelProvider, ProviderError, ProviderModel, ProviderRegistry};
use nyan_coding_agent_provider_http::bedrock::{BedrockAuth, BedrockConverseProvider};
use nyan_coding_agent_provider_http::cpa::{Endpoints, resolve_endpoints};
use nyan_coding_agent_provider_http::sigv4::AwsCredentials;
use nyan_coding_agent_provider_http::{
    AnthropicMessagesProvider, GoogleGenerativeAiProvider, GoogleVertexProvider,
    MistralConversationsProvider, OpenAiChatProvider, OpenAiResponsesProvider, VertexAuth,
};
use tokio::runtime::Handle;

use crate::args::Args;
use crate::provider_table::{
    MIXED_API_PROVIDERS, PROVIDERS, ProviderDef, UNSUPPORTED_API_PROVIDERS,
};
use crate::run::Environment;
use crate::settings::read_default_model;

/// The provider id of `CLIProxyAPI`.
pub const CPA_PROVIDER: &str = "cliproxyapi";
/// The credential setting that holds the proxy's base URL.
pub const CPA_BASE_URL_SETTING: &str = "CLIPROXYAPI_BASE_URL";
/// Where a local `CLIProxyAPI` listens by default.
pub const DEFAULT_CPA_BASE_URL: &str = "http://127.0.0.1:8317";

/// The wire APIs this build speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Api {
    /// `OpenAI` chat completions and its many compatibles.
    OpenAiCompletions,
    /// `OpenAI` Responses.
    OpenAiResponses,
    /// Azure `OpenAI` Responses.
    AzureOpenAiResponses,
    /// Anthropic Messages.
    AnthropicMessages,
    /// Google Generative AI.
    GoogleGenerativeAi,
    /// Google Vertex AI.
    GoogleVertex,
    /// Mistral Conversations.
    MistralConversations,
    /// Amazon Bedrock `ConverseStream`.
    BedrockConverse,
}

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
    /// Azure `OpenAI` Responses.
    Azure {
        /// The resource's base URL.
        base_url: String,
        /// API key.
        api_key: String,
        /// The `api-version` query value.
        api_version: String,
        /// Model id to deployment name.
        deployments: BTreeMap<String, String>,
    },
    /// Anthropic Messages.
    Anthropic {
        /// Endpoint root.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// Google Generative AI.
    Google {
        /// Base URL including the API version.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// Google Vertex AI.
    Vertex {
        /// Everything before `/models/`.
        base_url: String,
        /// Authentication.
        auth: VertexAuth,
    },
    /// Mistral Conversations.
    Mistral {
        /// Endpoint root.
        base_url: String,
        /// API key.
        api_key: String,
    },
    /// Amazon Bedrock.
    Bedrock {
        /// AWS region.
        region: String,
        /// Authentication.
        auth: BedrockAuth,
        /// A replacement endpoint (scheme and authority), for tests and gateways.
        endpoint: Option<String>,
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
    ids.extend([CPA_PROVIDER, "azure"]);
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

/// The provider's base URL, with `{NAME}` placeholders filled from the
/// environment or the stored credential's settings.
fn base_url_for(def: &ProviderDef, env: &Environment, store: &AuthStore) -> Result<String, String> {
    if let Some(url) = env.base_urls.get(def.id) {
        return Ok(url.clone());
    }
    let template = def
        .base_url
        .ok_or_else(|| format!("provider \"{}\" has no base URL configured", def.id))?;
    let stored = stored_api_key(store, def.id).ok().flatten();
    let mut url = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|offset| start + offset)
            .ok_or_else(|| format!("malformed base URL template for \"{}\"", def.id))?;
        let name = &rest[start + 1..end];
        let value = env_value(env, name)
            .or_else(|| {
                stored
                    .as_ref()
                    .and_then(|credential| credential.env.get(name).cloned())
            })
            .ok_or_else(|| {
                format!(
                    "provider \"{}\" needs {name}: set it in the environment",
                    def.id
                )
            })?;
        url.push_str(&rest[..start]);
        url.push_str(&value);
        rest = &rest[end + 1..];
    }
    url.push_str(rest);
    Ok(url)
}

fn azure_setup(args: &Args, env: &Environment, store: &AuthStore) -> Result<ProviderSetup, String> {
    let api_key = api_key_for("azure", &["AZURE_OPENAI_API_KEY"], args, env, store)?;
    let base_url = env
        .base_urls
        .get("azure")
        .cloned()
        .or_else(|| env_value(env, "AZURE_OPENAI_BASE_URL"))
        .or_else(|| {
            env_value(env, "AZURE_OPENAI_RESOURCE_NAME")
                .map(|name| format!("https://{name}.openai.azure.com/openai/v1"))
        })
        .ok_or_else(|| {
            "Azure OpenAI base URL is required: set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME".to_owned()
        })?;
    let deployments = env_value(env, "AZURE_OPENAI_DEPLOYMENT_NAME_MAP")
        .map(|map| {
            map.split(',')
                .filter_map(|entry| entry.trim().split_once('='))
                .filter(|(model, deployment)| !model.is_empty() && !deployment.is_empty())
                .map(|(model, deployment)| (model.trim().to_owned(), deployment.trim().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    Ok(ProviderSetup::Azure {
        base_url,
        api_key,
        api_version: env_value(env, "AZURE_OPENAI_API_VERSION").unwrap_or_else(|| "v1".to_owned()),
        deployments,
    })
}

fn bedrock_setup(env: &Environment) -> Result<ProviderSetup, String> {
    let region = env_value(env, "AWS_REGION")
        .or_else(|| env_value(env, "AWS_DEFAULT_REGION"))
        .ok_or_else(|| {
            "no AWS region for amazon-bedrock: set AWS_REGION (or AWS_DEFAULT_REGION)".to_owned()
        })?;
    let auth = if let Some(token) = env_value(env, "AWS_BEARER_TOKEN_BEDROCK") {
        BedrockAuth::Bearer(token)
    } else if let (Some(access_key_id), Some(secret_access_key)) = (
        env_value(env, "AWS_ACCESS_KEY_ID"),
        env_value(env, "AWS_SECRET_ACCESS_KEY"),
    ) {
        BedrockAuth::Sigv4(AwsCredentials {
            access_key_id,
            secret_access_key,
            session_token: env_value(env, "AWS_SESSION_TOKEN"),
        })
    } else {
        return Err("no credentials for amazon-bedrock: set AWS_BEARER_TOKEN_BEDROCK, or AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY (named profiles, ECS and IRSA roles, and SSO are not supported yet)".to_owned());
    };
    Ok(ProviderSetup::Bedrock {
        region,
        auth,
        endpoint: env.base_urls.get("amazon-bedrock").cloned(),
    })
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

/// Resolves the endpoint and credential for `provider`.
///
/// # Errors
///
/// Returns a message when the provider is unknown, unsupported, has no
/// credential, or its settings are unusable.
pub fn resolve_provider(
    provider: &str,
    args: &Args,
    env: &Environment,
    agent_dir: &Path,
) -> Result<ProviderSetup, String> {
    let store = AuthStore::new(agent_dir);
    match provider {
        CPA_PROVIDER => {
            let connection = cpa_connection(args.api_key.as_deref(), env, agent_dir)?;
            Ok(ProviderSetup::OpenAiResponses {
                base_url: connection.endpoints.inference_base_url,
                api_key: connection.api_key,
            })
        }
        "azure" => azure_setup(args, env, &store),
        "amazon-bedrock" => bedrock_setup(env),
        "google-vertex" => vertex_setup(args, env, &store),
        _ => {
            if let Some(def) = provider_def(provider) {
                return simple_setup(def, args, env, &store);
            }
            if MIXED_API_PROVIDERS.contains(&provider) {
                return Err(format!(
                    "provider \"{provider}\" serves models over different wire APIs, and choosing the right one needs the model catalog, which this build does not have yet"
                ));
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

fn simple_setup(
    def: &ProviderDef,
    args: &Args,
    env: &Environment,
    store: &AuthStore,
) -> Result<ProviderSetup, String> {
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
    Ok(match def.api {
        Api::OpenAiCompletions => ProviderSetup::OpenAiCompletions { base_url, api_key },
        Api::OpenAiResponses => ProviderSetup::OpenAiResponses { base_url, api_key },
        Api::AnthropicMessages => ProviderSetup::Anthropic { base_url, api_key },
        Api::GoogleGenerativeAi => ProviderSetup::Google { base_url, api_key },
        Api::MistralConversations => ProviderSetup::Mistral { base_url, api_key },
        Api::AzureOpenAiResponses | Api::GoogleVertex | Api::BedrockConverse => {
            return Err(format!("provider \"{}\" needs its own settings", def.id));
        }
    })
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
    let provider: Box<dyn ModelProvider> =
        match setup {
            ProviderSetup::OpenAiCompletions { base_url, api_key } => Box::new(
                OpenAiChatProvider::new(id, base_url, Some(api_key), handle)?,
            ),
            ProviderSetup::OpenAiResponses { base_url, api_key } => Box::new(
                OpenAiResponsesProvider::new(id, base_url, Some(api_key), handle)?,
            ),
            ProviderSetup::Azure {
                base_url,
                api_key,
                api_version,
                deployments,
            } => Box::new(
                OpenAiResponsesProvider::azure(id, &base_url, Some(api_key), &api_version, handle)?
                    .with_deployments(deployments),
            ),
            ProviderSetup::Anthropic { base_url, api_key } => Box::new(
                AnthropicMessagesProvider::new(id, base_url, Some(api_key), handle)?,
            ),
            ProviderSetup::Google { base_url, api_key } => Box::new(
                GoogleGenerativeAiProvider::new(id, base_url, Some(api_key), handle)?,
            ),
            ProviderSetup::Vertex { base_url, auth } => {
                Box::new(GoogleVertexProvider::new(id, base_url, Some(auth), handle)?)
            }
            ProviderSetup::Mistral { base_url, api_key } => Box::new(
                MistralConversationsProvider::new(id, base_url, Some(api_key), handle)?,
            ),
            ProviderSetup::Bedrock {
                region,
                auth,
                endpoint,
            } => {
                let provider = BedrockConverseProvider::new(id, region, auth, handle)?;
                Box::new(match endpoint {
                    Some(endpoint) => provider.with_endpoint(endpoint),
                    None => provider,
                })
            }
        };
    Ok(SingleProvider(provider))
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
            read_stdin: Box::new(|| None),
            prompt: Box::new(|_, _| None),
        }
    }

    fn resolve(provider: &str, variables: &[(&str, &str)]) -> Result<ProviderSetup, String> {
        resolve_provider(
            provider,
            &parse_args(&[]),
            &environment(variables),
            Path::new("/nonexistent-agent-dir"),
        )
    }

    #[test]
    fn an_azure_resource_name_expands_to_its_public_openai_v1_endpoint() {
        let Ok(ProviderSetup::Azure {
            base_url,
            api_version,
            ..
        }) = resolve(
            "azure",
            &[
                ("AZURE_OPENAI_API_KEY", "k"),
                ("AZURE_OPENAI_RESOURCE_NAME", "myres"),
            ],
        )
        else {
            panic!("azure must resolve");
        };
        assert_eq!(base_url, "https://myres.openai.azure.com/openai/v1");
        assert_eq!(api_version, "v1");
    }

    #[test]
    fn an_explicit_azure_base_url_beats_the_resource_name() {
        let Ok(ProviderSetup::Azure { base_url, .. }) = resolve(
            "azure",
            &[
                ("AZURE_OPENAI_API_KEY", "k"),
                ("AZURE_OPENAI_RESOURCE_NAME", "myres"),
                ("AZURE_OPENAI_BASE_URL", "https://gateway.example.com/azure"),
            ],
        ) else {
            panic!("azure must resolve");
        };
        assert_eq!(base_url, "https://gateway.example.com/azure");
    }

    #[test]
    fn every_generated_provider_resolves_with_its_documented_environment_key() {
        for def in crate::provider_table::PROVIDERS {
            if matches!(def.id, "amazon-bedrock" | "google-vertex") {
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
            | ProviderSetup::Anthropic { base_url, api_key }
            | ProviderSetup::Google { base_url, api_key }
            | ProviderSetup::Mistral { base_url, api_key }) = setup
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
    use super::{ProviderSetup, resolve_provider};
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
            read_stdin: Box::new(|| None),
            prompt: Box::new(|_, _| None),
        }
    }

    #[test]
    fn a_url_template_is_filled_from_the_environment_or_refused_by_name() {
        let args = parse_args(&[]);
        let dir = Path::new("/nonexistent-agent-dir");
        let Ok(ProviderSetup::OpenAiCompletions { base_url, .. }) = resolve_provider(
            "cloudflare-workers-ai",
            &args,
            &env(&[
                ("CLOUDFLARE_API_KEY", "k"),
                ("CLOUDFLARE_ACCOUNT_ID", "acct123"),
            ]),
            dir,
        ) else {
            panic!("must resolve");
        };
        assert_eq!(
            base_url,
            "https://api.cloudflare.com/client/v4/accounts/acct123/ai/v1"
        );
        let missing = resolve_provider(
            "cloudflare-workers-ai",
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
