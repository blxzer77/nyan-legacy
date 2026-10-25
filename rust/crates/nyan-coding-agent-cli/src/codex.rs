//! `openai-codex` in the CLI: the stored OAuth login, its refresh, and `nyan login openai-codex`.
//!
//! The login lives in `auth.json` as `{"type": "oauth", "access", "refresh",
//! "expires", "accountId"}`, the shape the TypeScript CLI writes. Every request
//! asks [`StoredCodexAuth`] for its credentials; when the access token is within a
//! minute of expiring it is refreshed under the credential file's lock, so two
//! processes cannot spend the same single-use refresh token.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use nyan_coding_agent_auth::{AuthError, AuthStore, OAuthCredential};
use nyan_coding_agent_core::{BoxFuture, CancellationToken, now_ms};
use nyan_coding_agent_provider::ProviderError;
use nyan_coding_agent_provider_http::codex_auth::{
    self, DEFAULT_AUTH_BASE, DeviceCode, PollPacing, REDIRECT_URI, Tokens,
};
use nyan_coding_agent_provider_http::{CodexAuthSource, CodexCredentials};
use tokio::runtime::Handle;

use crate::headless::SharedWriter;
use crate::run::Environment;

/// The provider id.
pub const CODEX_PROVIDER: &str = "openai-codex";
/// The key in [`Environment::base_urls`] that replaces the sign-in server (a test hook).
pub const AUTH_BASE_KEY: &str = "openai-codex:auth";
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_MS: u64 = 60_000;
/// The program name sent as `originator` in the authorize request.
const ORIGINATOR: &str = "nyan";
/// How long the browser callback is awaited.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The sign-in server for this environment.
#[must_use]
pub fn auth_base(env: &Environment) -> String {
    env.base_urls
        .get(AUTH_BASE_KEY)
        .cloned()
        .unwrap_or_else(|| DEFAULT_AUTH_BASE.to_owned())
}

/// Credentials read from `auth.json` and refreshed when they run out.
pub struct StoredCodexAuth {
    agent_dir: PathBuf,
    auth_base: String,
    handle: Handle,
}

impl std::fmt::Debug for StoredCodexAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCodexAuth")
            .field("agent_dir", &self.agent_dir)
            .finish_non_exhaustive()
    }
}

impl StoredCodexAuth {
    /// Reads and refreshes the login stored in `agent_dir`.
    #[must_use]
    pub fn new(agent_dir: &Path, auth_base: String, handle: Handle) -> Self {
        Self {
            agent_dir: agent_dir.to_path_buf(),
            auth_base,
            handle,
        }
    }
}

fn credential_from(tokens: &Tokens, previous: &OAuthCredential) -> Result<OAuthCredential, String> {
    let account = codex_auth::account_id(&tokens.access)
        .or_else(|| previous.extra_str("accountId").map(str::to_owned))
        .ok_or_else(|| "the new access token names no ChatGPT account".to_owned())?;
    let mut extra = previous.extra.clone();
    extra.insert("accountId".to_owned(), serde_json::Value::String(account));
    Ok(OAuthCredential {
        access: tokens.access.clone(),
        refresh: tokens.refresh.clone(),
        expires_ms: tokens.expires_ms,
        extra,
    })
}

fn relogin_hint(message: &str) -> String {
    format!("{message}; run `nyan login {CODEX_PROVIDER}` to sign in again")
}

fn provider_error(error: AuthError) -> ProviderError {
    match error {
        AuthError::NotLoggedIn { .. } => ProviderError::Authentication {
            message: format!(
                "not logged in to {CODEX_PROVIDER}: run `nyan login {CODEX_PROVIDER}`"
            ),
        },
        // A transport failure may pass; a rejected refresh token never will.
        AuthError::RefreshFailed { message } if message.contains(" error: ") => {
            ProviderError::Request {
                message,
                retryable: true,
            }
        }
        AuthError::RefreshFailed { message } => ProviderError::Authentication {
            message: relogin_hint(&message),
        },
        other => ProviderError::Request {
            message: other.to_string(),
            retryable: false,
        },
    }
}

impl CodexAuthSource for StoredCodexAuth {
    fn credentials(&self) -> BoxFuture<Result<CodexCredentials, ProviderError>> {
        let (agent_dir, base, handle) = (
            self.agent_dir.clone(),
            self.auth_base.clone(),
            self.handle.clone(),
        );
        Box::pin(async move {
            let blocking = handle.clone();
            let outcome = handle
                .spawn_blocking(move || {
                    let now = now_ms();
                    AuthStore::new(&agent_dir).refresh_oauth(
                        CODEX_PROVIDER,
                        now,
                        REFRESH_MARGIN_MS,
                        |current| {
                            let tokens = blocking
                                .block_on(codex_auth::refresh(
                                    &blocking,
                                    &base,
                                    &current.refresh,
                                    now,
                                ))
                                .map_err(|error| error.0)?;
                            credential_from(&tokens, current)
                        },
                    )
                })
                .await
                .map_err(|error| ProviderError::Request {
                    message: format!("the credential task failed: {error}"),
                    retryable: false,
                })?
                .map_err(provider_error)?;
            let account_id = outcome
                .extra_str("accountId")
                .map(str::to_owned)
                .or_else(|| codex_auth::account_id(&outcome.access))
                .ok_or_else(|| ProviderError::Authentication {
                    message: relogin_hint("the stored login names no ChatGPT account"),
                })?;
            Ok(CodexCredentials {
                access_token: outcome.access,
                account_id,
            })
        })
    }
}

fn say(writer: &SharedWriter, text: &str) {
    if let Ok(mut writer) = writer.lock() {
        let _ = std::io::Write::write_all(&mut *writer, text.as_bytes())
            .and_then(|()| std::io::Write::flush(&mut *writer));
    }
}

fn ask(env: &Environment, message: &str) -> Result<String, String> {
    (env.prompt)(message, false).ok_or_else(|| "Login cancelled".to_owned())
}

fn save(agent_dir: &Path, tokens: &Tokens) -> Result<(), String> {
    let account = codex_auth::account_id(&tokens.access)
        .ok_or_else(|| "Failed to extract accountId from token".to_owned())?;
    let credential = OAuthCredential {
        access: tokens.access.clone(),
        refresh: tokens.refresh.clone(),
        expires_ms: tokens.expires_ms,
        extra: [("accountId".to_owned(), serde_json::Value::String(account))].into(),
    };
    AuthStore::new(agent_dir)
        .save_oauth(CODEX_PROVIDER, &credential)
        .map_err(|error| error.to_string())
}

/// Signs in with a device code and stores the login.
fn login_device(
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    handle: &Handle,
) -> Result<(), String> {
    let base = auth_base(env);
    let device = handle
        .block_on(codex_auth::start_device(handle, &base))
        .map_err(|error| error.0)?;
    say(
        out,
        &format!(
            "Open {} and enter the code {}\nWaiting for approval (up to 15 minutes)...\n",
            DeviceCode::verification_url(&base),
            device.user_code
        ),
    );
    let cancellation = CancellationToken::new();
    let approval = handle
        .block_on(codex_auth::poll_device(
            handle,
            &base,
            &device,
            PollPacing::default(),
            &cancellation,
        ))
        .map_err(|error| error.0)?;
    let tokens = handle
        .block_on(codex_auth::exchange_code(
            handle,
            &base,
            &approval.authorization_code,
            &approval.code_verifier,
            &codex_auth::device_redirect_uri(&base),
            now_ms(),
        ))
        .map_err(|error| error.0)?;
    save(agent_dir, &tokens)
}

enum Arrival {
    Callback(Result<String, String>),
    Pasted(Option<String>),
}

/// Signs in through the browser, taking the redirect on a local port or a pasted URL.
fn login_browser(
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    handle: &Handle,
) -> Result<(), String> {
    let base = auth_base(env);
    let pkce = codex_auth::generate_pkce().map_err(|error| error.0)?;
    let state = codex_auth::random_state().map_err(|error| error.0)?;
    let url = codex_auth::authorize_url(&base, &pkce.challenge, &state, ORIGINATOR)
        .map_err(|error| error.0)?;

    let host = (env.var)("NYAN_OAUTH_CALLBACK_HOST")
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    // A taken port (the Codex CLI uses the same one) means only pasting works.
    let listener = handle
        .block_on(codex_auth::bind_callback(&host, env.oauth_callback_port))
        .ok();
    say(out, &format!("Open this URL to sign in:\n{url}\n"));
    if listener.is_none() {
        say(
            out,
            &format!(
                "Port {} is busy, so paste the redirect URL when asked.\n",
                env.oauth_callback_port
            ),
        );
    }
    (env.open_url)(&url);

    let (sender, receiver) = mpsc::channel::<Arrival>();
    let cancellation = CancellationToken::new();
    if let Some(listener) = listener {
        let (sender, cancellation, expected) =
            (sender.clone(), cancellation.clone(), state.clone());
        handle.spawn(async move {
            let result = codex_auth::serve_callback(
                &listener,
                codex_auth::CALLBACK_PATH,
                &expected,
                BROWSER_TIMEOUT,
                &cancellation,
            )
            .await
            .map_err(|error| error.0);
            let _ = sender.send(Arrival::Callback(result));
        });
    }
    // The prompt cannot be interrupted, so it gets a thread of its own; if the
    // callback wins, this thread ends with the process.
    let prompt = std::sync::Arc::clone(&env.prompt);
    let prompt_sender = sender;
    std::thread::spawn(move || {
        let answer = prompt(
            "Complete the login in your browser, or paste the authorization code / redirect URL here: ",
            false,
        );
        let _ = prompt_sender.send(Arrival::Pasted(answer));
    });

    let code = match receiver.recv() {
        Ok(Arrival::Callback(result)) => result?,
        Ok(Arrival::Pasted(Some(input))) => {
            let (code, pasted_state) = codex_auth::parse_authorization_input(&input);
            if pasted_state.is_some_and(|pasted| pasted != state) {
                cancellation.cancel();
                return Err("State mismatch".to_owned());
            }
            code.ok_or_else(|| "Missing authorization code".to_owned())?
        }
        Ok(Arrival::Pasted(None)) | Err(_) => return Err("Login cancelled".to_owned()),
    };
    cancellation.cancel();
    let tokens = handle
        .block_on(codex_auth::exchange_code(
            handle,
            &base,
            &code,
            &pkce.verifier,
            REDIRECT_URI,
            now_ms(),
        ))
        .map_err(|error| error.0)?;
    save(agent_dir, &tokens)
}

/// Runs the whole Codex login: choose a method, sign in, store the login.
///
/// # Errors
///
/// Returns a message when the login is cancelled, refused, or cannot be stored.
pub fn login(
    env: &Environment,
    agent_dir: &Path,
    out: &SharedWriter,
    handle: &Handle,
) -> Result<(), String> {
    let choice = ask(
        env,
        "Select OpenAI Codex login method:\n  1) Browser login (default)\n  2) Device code login (headless)\n> ",
    )?;
    match choice.trim().to_ascii_lowercase().as_str() {
        "" | "1" | "browser" => login_browser(env, agent_dir, out, handle)?,
        "2" | "device" | "device_code" => login_device(env, agent_dir, out, handle)?,
        other => return Err(format!("Unknown OpenAI Codex login method: {other}")),
    }
    say(out, "Logged in to OpenAI Codex.\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use nyan_coding_agent_auth::{AuthError, OAuthCredential};
    use nyan_coding_agent_provider::ProviderError;
    use nyan_coding_agent_provider_http::codex_auth::{Tokens, base64url_encode};
    use serde_json::json;

    use super::{credential_from, provider_error};

    fn token(claims: &serde_json::Value) -> String {
        format!("h.{}.s", base64url_encode(claims.to_string().as_bytes()))
    }

    fn previous() -> OAuthCredential {
        OAuthCredential {
            access: "old".to_owned(),
            refresh: "old-refresh".to_owned(),
            expires_ms: 1,
            extra: [
                ("accountId".to_owned(), json!("acct-stored")),
                ("note".to_owned(), json!("kept")),
            ]
            .into(),
        }
    }

    #[test]
    fn a_refreshed_login_takes_the_account_from_the_new_token_or_keeps_the_stored_one() {
        let named = Tokens {
            access: token(
                &json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct-new"}}),
            ),
            refresh: "r2".to_owned(),
            expires_ms: 9,
        };
        let renewed = credential_from(&named, &previous()).expect("renewed");
        assert_eq!(renewed.extra_str("accountId"), Some("acct-new"));
        assert_eq!((renewed.refresh.as_str(), renewed.expires_ms), ("r2", 9));
        assert_eq!(
            renewed.extra_str("note"),
            Some("kept"),
            "other stored fields survive"
        );

        let unnamed = Tokens {
            access: token(&json!({})),
            ..named
        };
        assert_eq!(
            credential_from(&unnamed, &previous())
                .expect("renewed")
                .extra_str("accountId"),
            Some("acct-stored")
        );
        let mut without_account = previous();
        without_account.extra.clear();
        assert!(
            credential_from(&unnamed, &without_account).is_err(),
            "no account anywhere is an error"
        );
    }

    #[test]
    fn refresh_failures_split_into_worth_retrying_and_sign_in_again() {
        let transport = provider_error(AuthError::RefreshFailed {
            message: "OpenAI Codex token refresh error: connection refused".to_owned(),
        });
        assert!(transport.is_retryable(), "{transport:?}");

        let rejected = provider_error(AuthError::RefreshFailed {
            message: "OpenAI Codex token refresh failed (400): invalid_grant".to_owned(),
        });
        assert_eq!(
            rejected,
            ProviderError::Authentication {
                message: "OpenAI Codex token refresh failed (400): invalid_grant; run `nyan login openai-codex` to sign in again".to_owned()
            }
        );
        assert!(matches!(
            provider_error(AuthError::NotLoggedIn {
                provider: "openai-codex".to_owned()
            }),
            ProviderError::Authentication { .. }
        ));
        assert!(!provider_error(AuthError::Locked { path: "x".into() }).is_retryable());
    }
}
