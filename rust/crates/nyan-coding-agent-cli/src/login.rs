//! `nyan login`, `nyan logout`, and `--list-models`.
//!
//! Login follows the TypeScript `CLIProxyAPI` flow: ask for the base URL and the
//! key, prove them by fetching the model catalog, and only then store them.
//! What differs is the next step. There is no built-in default model, so once the
//! catalog is known the user chooses a default from it, and that choice is saved
//! in `settings.json`.
//!
//! The key is stored as a plain `api_key` credential with the base URL beside it
//! (`env.CLIPROXYAPI_BASE_URL`), not as the `oauth`-shaped entry with a
//! hundred-year expiry the TypeScript extension used.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use nyan_coding_agent_auth::AuthStore;
use nyan_coding_agent_models::catalog::catalog_providers;
use nyan_coding_agent_provider_http::cpa::{fetch_catalog, resolve_endpoints};
use tokio::runtime::Handle;

use crate::catalog_access::{Access, load};
use crate::headless::SharedWriter;
use crate::providers::{
    CPA_BASE_URL_SETTING, CPA_PROVIDER, DEFAULT_CPA_BASE_URL, cpa_connection, credential_available,
};
use crate::run::Environment;
use crate::settings::{DefaultModel, read_default_model, write_default_model};
use nyan_coding_agent_models::table::PROVIDERS;

/// The providers `nyan login` can set up: id and display name.
///
/// `cliproxyapi` has its own flow (base URL, key, catalog check). Every other
/// entry stores an API key; Amazon Bedrock is absent because it takes AWS
/// credentials from the environment, not a key.
#[must_use]
pub fn login_providers() -> Vec<(&'static str, &'static str)> {
    let mut providers = vec![(CPA_PROVIDER, "CLIProxyAPI"), ("azure", "Azure OpenAI")];
    providers.extend(
        PROVIDERS
            .iter()
            .filter(|def| def.id != "amazon-bedrock")
            .map(|def| (def.id, def.name)),
    );
    providers[2..].sort_by_key(|(id, _)| *id);
    providers
}

/// A model as the login menu and `--list-models` show it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Listed {
    id: String,
    context_window: u64,
    max_tokens: u64,
    reasoning: bool,
    image_input: bool,
}

impl From<&nyan_coding_agent_provider_http::cpa::CatalogModel> for Listed {
    fn from(model: &nyan_coding_agent_provider_http::cpa::CatalogModel) -> Self {
        Self {
            id: model.id.clone(),
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            reasoning: model.reasoning,
            image_input: model.image_input,
        }
    }
}

impl From<&nyan_coding_agent_models::catalog::CatalogModel> for Listed {
    fn from(model: &nyan_coding_agent_models::catalog::CatalogModel) -> Self {
        Self {
            id: model.id.clone(),
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            reasoning: model.reasoning,
            image_input: model.image_input,
        }
    }
}

fn say(writer: &SharedWriter, text: &str) {
    if let Ok(mut writer) = writer.lock() {
        let _ = writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush());
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the async runtime: {error}"))
}

fn ask(env: &Environment, message: &str, secret: bool) -> Result<String, String> {
    (env.prompt)(message, secret).ok_or_else(|| "Login cancelled".to_owned())
}

/// Picks the provider named in `rest`, or asks when none was named.
fn choose_provider(rest: &[String], env: &Environment) -> Result<&'static str, String> {
    let providers = login_providers();
    if let Some(name) = rest.first() {
        return providers
            .iter()
            .find(|(id, display)| {
                id.eq_ignore_ascii_case(name) || display.eq_ignore_ascii_case(name)
            })
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                format!(
                    "cannot log in to \"{name}\" (available: {})",
                    providers
                        .iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
    }
    let mut menu = String::new();
    for (index, (_, name)) in providers.iter().enumerate() {
        let _ = writeln!(menu, "  {:>2}) {name}", index + 1);
    }
    let answer = ask(env, &format!("Choose a provider:\n{menu}> "), false)?;
    let answer = answer.trim();
    answer
        .parse::<usize>()
        .ok()
        .and_then(|number| providers.get(number.wrapping_sub(1)))
        .or_else(|| {
            providers
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(answer))
        })
        .map(|(id, _)| *id)
        .ok_or_else(|| "Login cancelled".to_owned())
}

fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 && count % 1_000_000 == 0 {
        format!("{}M", count / 1_000_000)
    } else if count >= 1_000 {
        format!("{}K", count / 1_000)
    } else {
        count.to_string()
    }
}

/// Runs `nyan login [provider]` and returns the exit code.
#[must_use]
pub fn run_login(
    rest: &[String],
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    err: &SharedWriter,
) -> i32 {
    let outcome = choose_provider(rest, env).and_then(|provider| {
        if provider == CPA_PROVIDER {
            let runtime = runtime()?;
            login_cpa(env, agent_dir, out, runtime.handle())
        } else {
            login_api_key(provider, env, agent_dir, out)
        }
    });
    match outcome {
        Ok(()) => 0,
        Err(message) => {
            say(err, &format!("Error: {message}\n"));
            1
        }
    }
}

/// Stores an API key for a provider that has no catalog endpoint to check it against.
///
/// The key is stored as typed: nothing proves it works until a request is made.
/// The default model is typed too, since this build has no per-provider model list.
fn login_api_key(
    provider: &str,
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
) -> Result<(), String> {
    let name = login_providers()
        .into_iter()
        .find(|(id, _)| *id == provider)
        .map_or(provider, |(_, name)| name);
    let key = ask(env, &format!("{name} API key: "), true)?;
    let key = key.trim();
    if key.is_empty() {
        return Err("the API key cannot be empty".to_owned());
    }
    AuthStore::new(agent_dir)
        .save_api_key(provider, key, &BTreeMap::new())
        .map_err(|error| error.to_string())?;
    say(
        out,
        &format!("Stored the {name} API key. It has not been checked against the provider.\n"),
    );
    // The provider's own model list, when the catalog has one; otherwise ask for an id.
    match load(env, agent_dir, Access::RefreshIfStale) {
        Ok(loaded) => {
            for note in &loaded.notes {
                say(out, &format!("{note}\n"));
            }
            let listed: Vec<Listed> = loaded
                .catalog
                .for_provider(provider)
                .map(Listed::from)
                .collect();
            if !listed.is_empty() {
                return choose_default(env, agent_dir, out, provider, &listed);
            }
        }
        Err(message) => say(out, &format!("Could not load the model list: {message}\n")),
    }
    let model = (env.prompt)(
        &format!("Default model id for {provider} (blank to skip): "),
        false,
    )
    .unwrap_or_default();
    let model = model.trim();
    if model.is_empty() {
        say(
            out,
            &format!(
                "No default model saved; pass --model {provider}/<id> to choose one per run.\n"
            ),
        );
        return Ok(());
    }
    write_default_model(
        agent_dir,
        &DefaultModel {
            provider: provider.to_owned(),
            model: model.to_owned(),
        },
    )?;
    say(out, &format!("Default model: {provider}/{model}\n"));
    Ok(())
}

fn login_cpa(
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    handle: &Handle,
) -> Result<(), String> {
    say(
        out,
        "Configure CLIProxyAPI. Preferred base URL form: host:port (for example http://127.0.0.1:8317).\n",
    );
    let mut default_url = DEFAULT_CPA_BASE_URL.to_owned();
    loop {
        let typed = ask(
            env,
            &format!("CLIProxyAPI base URL [{default_url}]: "),
            false,
        )?;
        let entered = if typed.trim().is_empty() {
            default_url.clone()
        } else {
            typed.trim().to_owned()
        };
        let endpoints = match resolve_endpoints(&entered) {
            Ok(endpoints) => endpoints,
            Err(message) => {
                say(out, &format!("Invalid base URL: {message}\n"));
                default_url = entered;
                continue;
            }
        };
        let key = ask(env, "CLIProxyAPI API key: ", true)?;
        let key = key.trim();
        if key.is_empty() {
            say(out, "API key cannot be empty.\n");
            default_url = entered;
            continue;
        }
        say(out, "Validating credentials via the models endpoint...\n");
        let fetched = handle.block_on(fetch_catalog(handle, &endpoints.models_url, key));
        match fetched {
            Ok(models) => {
                let settings = BTreeMap::from([(CPA_BASE_URL_SETTING.to_owned(), entered)]);
                AuthStore::new(agent_dir)
                    .save_api_key(CPA_PROVIDER, key, &settings)
                    .map_err(|error| error.to_string())?;
                say(
                    out,
                    &format!(
                        "Registered {} model(s) from {}.\n",
                        models.len(),
                        endpoints.models_url
                    ),
                );
                let listed: Vec<Listed> = models.iter().map(Listed::from).collect();
                return choose_default(env, agent_dir, out, CPA_PROVIDER, &listed);
            }
            Err(error) => {
                say(
                    out,
                    &format!(
                        "Login validation failed: {error}\nPlease re-enter the base URL and API key.\n"
                    ),
                );
                default_url = entered;
            }
        }
    }
}

/// Lets the user choose the default model from the catalog just fetched.
fn choose_default(
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    provider: &str,
    models: &[Listed],
) -> Result<(), String> {
    if models.is_empty() {
        say(
            out,
            "No models were listed, so no default model was saved.\n",
        );
        return Ok(());
    }
    if let Some(current) = read_default_model(agent_dir)?
        && current.provider == provider
        && models.iter().any(|model| model.id == current.model)
    {
        say(
            out,
            &format!(
                "Default model unchanged: {}/{}\n",
                current.provider, current.model
            ),
        );
        return Ok(());
    }
    let mut menu = String::from("Models:\n");
    for (index, model) in models.iter().enumerate() {
        let _ = writeln!(
            menu,
            "  {:>2}) {}  ({} context{})",
            index + 1,
            model.id,
            format_tokens(model.context_window),
            if model.reasoning { ", reasoning" } else { "" }
        );
    }
    say(out, &menu);
    loop {
        let Some(answer) = (env.prompt)("Default model (number or id, blank to skip): ", false)
        else {
            say(out, "No default model saved.\n");
            return Ok(());
        };
        let answer = answer.trim();
        if answer.is_empty() {
            say(
                out,
                &format!(
                    "No default model saved; pass --model {provider}/<id> to choose one per run.\n"
                ),
            );
            return Ok(());
        }
        let chosen = answer
            .parse::<usize>()
            .ok()
            .and_then(|number| models.get(number.wrapping_sub(1)))
            .or_else(|| models.iter().find(|model| model.id == answer));
        if let Some(model) = chosen {
            write_default_model(
                agent_dir,
                &DefaultModel {
                    provider: provider.to_owned(),
                    model: model.id.clone(),
                },
            )?;
            say(out, &format!("Default model: {provider}/{}\n", model.id));
            return Ok(());
        }
        say(out, &format!("\"{answer}\" is not in the list.\n"));
    }
}

/// Runs `nyan logout [provider]` and returns the exit code.
#[must_use]
pub fn run_logout(
    rest: &[String],
    agent_dir: &Path,
    out: &SharedWriter,
    err: &SharedWriter,
) -> i32 {
    let store = AuthStore::new(agent_dir);
    let outcome = (|| {
        let provider = if let Some(name) = rest.first() {
            name.clone()
        } else {
            let stored = store.providers().map_err(|error| error.to_string())?;
            match stored.as_slice() {
                [only] => only.clone(),
                [] => return Err("nothing is logged in".to_owned()),
                many => {
                    return Err(format!(
                        "name the provider to log out of ({})",
                        many.join(", ")
                    ));
                }
            }
        };
        if store.remove(&provider).map_err(|error| error.to_string())? {
            Ok(format!("Logged out of {provider}.\n"))
        } else {
            Err(format!("no stored login for \"{provider}\""))
        }
    })();
    match outcome {
        Ok(message) => {
            say(out, &message);
            0
        }
        Err(message) => {
            say(err, &format!("Error: {message}\n"));
            1
        }
    }
}

/// Runs `--list-models [search]` over the providers that are logged in.
#[must_use]
pub fn run_list_models(
    search: Option<&str>,
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    err: &SharedWriter,
) -> i32 {
    let mut rows: Vec<(String, Listed)> = Vec::new();
    let mut failure: Option<String> = None;

    if credential_available(CPA_PROVIDER, env, agent_dir) {
        let fetched = runtime().and_then(|runtime| {
            let connection = cpa_connection(None, env, agent_dir)?;
            runtime
                .block_on(fetch_catalog(
                    runtime.handle(),
                    &connection.endpoints.models_url,
                    &connection.api_key,
                ))
                .map_err(|error| format!("{CPA_PROVIDER}: {error}"))
        });
        match fetched {
            Ok(models) => rows.extend(
                models
                    .iter()
                    .map(|model| (CPA_PROVIDER.to_owned(), Listed::from(model))),
            ),
            Err(message) => failure = Some(message),
        }
    }

    let wanted: Vec<&str> = catalog_providers()
        .into_iter()
        .filter(|provider| credential_available(provider, env, agent_dir))
        .collect();
    if !wanted.is_empty() {
        match load(env, agent_dir, Access::RefreshIfStale) {
            Ok(loaded) => {
                for note in &loaded.notes {
                    say(err, &format!("{note}\n"));
                }
                for provider in wanted {
                    rows.extend(
                        loaded
                            .catalog
                            .for_provider(provider)
                            .map(|model| (provider.to_owned(), Listed::from(model))),
                    );
                }
            }
            Err(message) => {
                failure =
                    Some(failure.map_or(message.clone(), |first| format!("{first}\n{message}")));
            }
        }
    }

    if let Some(message) = &failure {
        if rows.is_empty() {
            say(err, &format!("Error: {message}\n"));
            return 1;
        }
        say(err, &format!("Warning: {message}\n"));
    }
    let needle = search.map(str::to_lowercase);
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|(provider, model)| {
            needle.as_ref().is_none_or(|needle| {
                format!("{provider}/{}", model.id)
                    .to_lowercase()
                    .contains(needle)
            })
        })
        .collect();
    if rows.is_empty() {
        say(
            out,
            match search {
                Some(_) => "No models match the search.\n",
                None => "No models available. Run `nyan login` to add a provider.\n",
            },
        );
        return 0;
    }
    let mut table = String::from(
        "provider                model                          context  max-out  thinking  images\n",
    );
    for (provider, model) in rows {
        let _ = writeln!(
            table,
            "{provider:<23} {:<30} {:<8} {:<8} {:<9} {}",
            model.id,
            format_tokens(model.context_window),
            format_tokens(model.max_tokens),
            if model.reasoning { "yes" } else { "no" },
            if model.image_input { "yes" } else { "no" },
        );
    }
    say(out, &table);
    0
}
