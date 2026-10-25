//! The `OpenAI` Codex (`ChatGPT`) OAuth login, ported from
//! `packages/ai/src/auth/oauth/openai-codex.ts`.
//!
//! Two ways to sign in, both ending in the same token exchange:
//! - browser: open the authorize URL, receive the redirect on
//!   `http://localhost:1455/auth/callback` (or take a pasted URL when the port is
//!   taken), then exchange the code;
//! - device code: show a code to enter at `https://auth.openai.com/codex/device`
//!   and poll until it is approved. Nothing listens locally.
//!
//! Tokens last a limited time; [`refresh`] trades the refresh token for new ones.
//! The access token is a JWT whose `chatgpt_account_id` claim every Codex
//! request must carry as a header.
//!
//! This code has been exercised against a loopback server only, never against the
//! real `auth.openai.com`.

use std::fmt::Write as _;
use std::time::Duration;

use nyan_coding_agent_core::CancellationToken;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Handle;

/// The public client id the Codex CLI and this CLI sign in with.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Where sign-in happens.
pub const DEFAULT_AUTH_BASE: &str = "https://auth.openai.com";
/// The redirect the authorize request names; the Codex CLI uses the same port.
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// The port the callback server listens on.
pub const CALLBACK_PORT: u16 = 1455;
/// The path of the callback.
pub const CALLBACK_PATH: &str = "/auth/callback";
/// Where the user enters a device code.
pub const DEVICE_VERIFICATION_PATH: &str = "/codex/device";
const DEVICE_REDIRECT_PATH: &str = "/deviceauth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM: &str = "https://api.openai.com/auth";
/// How long a device code stays valid.
pub const DEVICE_CODE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A sign-in or refresh failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OAuthError(pub String);

impl std::fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for OAuthError {}

fn fail<T>(message: impl Into<String>) -> Result<T, OAuthError> {
    Err(OAuthError(message.into()))
}

/// Access and refresh tokens and when the access token expires.
#[derive(Clone, Eq, PartialEq)]
pub struct Tokens {
    /// The access token (a JWT).
    pub access: String,
    /// The refresh token.
    pub refresh: String,
    /// Expiry in Unix milliseconds.
    pub expires_ms: u64,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tokens")
            .field("expires_ms", &self.expires_ms)
            .finish_non_exhaustive()
    }
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding.
#[must_use]
pub fn base64url_encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0_u32, |acc, (index, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * index))
        });
        let symbols = chunk.len() + 1;
        for index in 0..symbols {
            text.push(char::from(
                BASE64URL[((value >> (18 - 6 * index)) & 63) as usize],
            ));
        }
    }
    text
}

/// Decodes base64url or standard base64, with or without padding.
#[must_use]
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for character in text.trim_end_matches('=').chars() {
        let value = match character {
            'A'..='Z' => character as u32 - 'A' as u32,
            'a'..='z' => character as u32 - 'a' as u32 + 26,
            '0'..='9' => character as u32 - '0' as u32 + 52,
            '-' | '+' => 62,
            '_' | '/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(u8::try_from((buffer >> bits) & 0xFF).ok()?);
        }
    }
    Some(bytes)
}

/// A PKCE verifier and its S256 challenge.
#[derive(Clone, Eq, PartialEq)]
pub struct Pkce {
    /// The secret kept until the code exchange.
    pub verifier: String,
    /// Its SHA-256, sent with the authorize request.
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Pkce").finish_non_exhaustive()
    }
}

/// The S256 challenge for `verifier`.
#[must_use]
pub fn challenge_for(verifier: &str) -> String {
    base64url_encode(&Sha256::digest(verifier.as_bytes()))
}

/// Generates a PKCE pair from 32 random bytes.
///
/// # Errors
///
/// Returns an error when the operating system has no random source.
pub fn generate_pkce() -> Result<Pkce, OAuthError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| OAuthError(format!("no random source: {error}")))?;
    let verifier = base64url_encode(&bytes);
    Ok(Pkce {
        challenge: challenge_for(&verifier),
        verifier,
    })
}

/// A random `state` value: 16 bytes in hex.
///
/// # Errors
///
/// Returns an error when the operating system has no random source.
pub fn random_state() -> Result<String, OAuthError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| OAuthError(format!("no random source: {error}")))?;
    Ok(bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    }))
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://form.invalid/").expect("a constant URL parses");
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().unwrap_or_default().to_owned()
}

/// The URL the user opens to sign in.
///
/// # Errors
///
/// Returns an error when `auth_base` is not a URL.
pub fn authorize_url(
    auth_base: &str,
    challenge: &str,
    state: &str,
    originator: &str,
) -> Result<String, OAuthError> {
    let mut url = reqwest::Url::parse(&format!(
        "{}/oauth/authorize",
        auth_base.trim_end_matches('/')
    ))
    .map_err(|error| OAuthError(format!("{auth_base} is not a valid sign-in URL: {error}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", originator);
    Ok(url.to_string())
}

/// Reads a pasted redirect URL, `code#state`, `code=..&state=..`, or a bare code.
#[must_use]
pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    let from_query = |url: &reqwest::Url| {
        let find = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        (find("code"), find("state"))
    };
    if let Ok(url) = reqwest::Url::parse(value) {
        return from_query(&url);
    }
    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_owned()), Some(state.to_owned()));
    }
    if value.contains("code=") {
        if let Ok(url) = reqwest::Url::parse(&format!("http://pasted.invalid/?{value}")) {
            return from_query(&url);
        }
    }
    (Some(value.to_owned()), None)
}

/// The `ChatGPT` account id in the access token's claims.
#[must_use]
pub fn account_id(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&base64_decode(payload)?).ok()?;
    claims
        .get(JWT_CLAIM)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .user_agent(concat!("nyan-coding-agent/", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| OAuthError(format!("could not build the HTTP client: {error}")))
}

async fn spawned<T: Send + 'static>(
    handle: &Handle,
    task: impl std::future::Future<Output = Result<T, OAuthError>> + Send + 'static,
) -> Result<T, OAuthError> {
    handle
        .spawn(task)
        .await
        .map_err(|error| OAuthError(format!("the request task failed: {error}")))?
}

async fn token_request(
    auth_base: &str,
    operation: &'static str,
    body: String,
    now_ms: u64,
) -> Result<Tokens, OAuthError> {
    let response = client()?
        .post(format!("{}/oauth/token", auth_base.trim_end_matches('/')))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|error| OAuthError(format!("OpenAI Codex token {operation} error: {error}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return fail(format!(
            "OpenAI Codex token {operation} failed ({}): {}",
            status.as_u16(),
            if text.is_empty() {
                status.as_str()
            } else {
                &text
            }
        ));
    }
    let json: Value = serde_json::from_str(&text).map_err(|_| {
        OAuthError(format!(
            "OpenAI Codex token {operation} response is not JSON"
        ))
    })?;
    let field = |name: &str| json.get(name).and_then(Value::as_str).map(str::to_owned);
    let (Some(access), Some(refresh), Some(expires_in)) = (
        field("access_token"),
        field("refresh_token"),
        json.get("expires_in").and_then(Value::as_u64),
    ) else {
        return fail(format!(
            "OpenAI Codex token {operation} response is missing fields"
        ));
    };
    Ok(Tokens {
        access,
        refresh,
        expires_ms: now_ms.saturating_add(expires_in.saturating_mul(1000)),
    })
}

/// Trades an authorization code for tokens.
///
/// # Errors
///
/// Returns an error on a transport failure, a rejected code, or a malformed reply.
pub async fn exchange_code(
    handle: &Handle,
    auth_base: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    now_ms: u64,
) -> Result<Tokens, OAuthError> {
    let body = form_body(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ]);
    let auth_base = auth_base.to_owned();
    spawned(handle, async move {
        token_request(&auth_base, "exchange", body, now_ms).await
    })
    .await
}

/// Trades a refresh token for new tokens.
///
/// # Errors
///
/// Returns an error on a transport failure, a rejected token, or a malformed reply.
pub async fn refresh(
    handle: &Handle,
    auth_base: &str,
    refresh_token: &str,
    now_ms: u64,
) -> Result<Tokens, OAuthError> {
    let body = form_body(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ]);
    let auth_base = auth_base.to_owned();
    spawned(handle, async move {
        token_request(&auth_base, "refresh", body, now_ms).await
    })
    .await
}

/// A pending device-code sign-in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceCode {
    /// Identifies the sign-in to the server.
    pub device_auth_id: String,
    /// The code the user types.
    pub user_code: String,
    /// How often to poll, in seconds.
    pub interval_seconds: u64,
}

impl DeviceCode {
    /// The page where the code is entered.
    #[must_use]
    pub fn verification_url(auth_base: &str) -> String {
        format!(
            "{}{DEVICE_VERIFICATION_PATH}",
            auth_base.trim_end_matches('/')
        )
    }
}

/// Asks for a device code.
///
/// # Errors
///
/// Returns an error when the server refuses or answers with something unusable.
pub async fn start_device(handle: &Handle, auth_base: &str) -> Result<DeviceCode, OAuthError> {
    let url = format!(
        "{}/api/accounts/deviceauth/usercode",
        auth_base.trim_end_matches('/')
    );
    spawned(handle, async move {
        let response = client()?
            .post(url)
            .header("content-type", "application/json")
            .body(json!({"client_id": CLIENT_ID}).to_string())
            .send()
            .await
            .map_err(|error| {
                OAuthError(format!("OpenAI Codex device code request failed: {error}"))
            })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.as_u16() == 404 {
            return fail(
                "OpenAI Codex device code login is not enabled for this server. Use browser login.",
            );
        }
        if !status.is_success() {
            return fail(format!(
                "OpenAI Codex device code request failed with status {}{}",
                status.as_u16(),
                if text.is_empty() {
                    String::new()
                } else {
                    format!(": {text}")
                }
            ));
        }
        let json: Value = serde_json::from_str(&text)
            .map_err(|_| OAuthError("OpenAI Codex device code response is not JSON".to_owned()))?;
        let interval = match json.get("interval") {
            Some(Value::Number(number)) => number.as_u64(),
            Some(Value::String(text)) => text.trim().parse().ok(),
            _ => None,
        };
        let (Some(id), Some(code), Some(interval_seconds)) = (
            json.get("device_auth_id").and_then(Value::as_str),
            json.get("user_code").and_then(Value::as_str),
            interval,
        ) else {
            return fail("Invalid OpenAI Codex device code response");
        };
        Ok(DeviceCode {
            device_auth_id: id.to_owned(),
            user_code: code.to_owned(),
            interval_seconds,
        })
    })
    .await
}

/// How a device-code poll is paced.
#[derive(Clone, Copy, Debug)]
pub struct PollPacing {
    /// The shortest wait between polls.
    pub minimum_interval: Duration,
    /// Added to the wait after each `slow_down`.
    pub slow_down_step: Duration,
    /// Give up after this long.
    pub timeout: Duration,
}

impl Default for PollPacing {
    fn default() -> Self {
        Self {
            minimum_interval: Duration::from_secs(1),
            slow_down_step: Duration::from_secs(5),
            timeout: DEVICE_CODE_TIMEOUT,
        }
    }
}

/// What an approved device code yields: the pieces of the code exchange.
#[derive(Clone, Eq, PartialEq)]
pub struct DeviceApproval {
    /// The authorization code.
    pub authorization_code: String,
    /// The PKCE verifier the server chose.
    pub code_verifier: String,
}

impl std::fmt::Debug for DeviceApproval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceApproval")
            .finish_non_exhaustive()
    }
}

/// The redirect URI to use when exchanging a device approval.
#[must_use]
pub fn device_redirect_uri(auth_base: &str) -> String {
    format!("{}{DEVICE_REDIRECT_PATH}", auth_base.trim_end_matches('/'))
}

enum Poll {
    Pending,
    SlowDown,
    Approved(DeviceApproval),
}

async fn poll_once(url: &str, device: &DeviceCode) -> Result<Poll, OAuthError> {
    let response = client()?
        .post(url)
        .header("content-type", "application/json")
        .body(
            json!({"device_auth_id": device.device_auth_id, "user_code": device.user_code})
                .to_string(),
        )
        .send()
        .await
        .map_err(|error| OAuthError(format!("OpenAI Codex device auth failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        return match (
            json.get("authorization_code").and_then(Value::as_str),
            json.get("code_verifier").and_then(Value::as_str),
        ) {
            (Some(code), Some(verifier)) => Ok(Poll::Approved(DeviceApproval {
                authorization_code: code.to_owned(),
                code_verifier: verifier.to_owned(),
            })),
            _ => fail("Invalid OpenAI Codex device auth token response"),
        };
    }
    if matches!(status.as_u16(), 403 | 404) {
        return Ok(Poll::Pending);
    }
    let error_code = serde_json::from_str::<Value>(&text).ok().and_then(|json| {
        let error = json.get("error")?;
        error
            .as_str()
            .or_else(|| error.get("code").and_then(Value::as_str))
            .map(str::to_owned)
    });
    match error_code.as_deref() {
        Some("deviceauth_authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        _ => fail(format!(
            "OpenAI Codex device auth failed with status {}{}",
            status.as_u16(),
            if text.is_empty() {
                String::new()
            } else {
                format!(": {text}")
            }
        )),
    }
}

/// Polls until the user approves the device code, the code expires, or `cancellation` fires.
///
/// # Errors
///
/// Returns an error on cancellation, timeout, or a failure the server reports.
pub async fn poll_device(
    handle: &Handle,
    auth_base: &str,
    device: &DeviceCode,
    pacing: PollPacing,
    cancellation: &CancellationToken,
) -> Result<DeviceApproval, OAuthError> {
    let url = format!(
        "{}/api/accounts/deviceauth/token",
        auth_base.trim_end_matches('/')
    );
    let started = tokio::time::Instant::now();
    let mut interval = Duration::from_secs(device.interval_seconds).max(pacing.minimum_interval);
    let mut slowed = false;
    loop {
        if cancellation.is_cancelled() {
            return fail("Login cancelled");
        }
        if started.elapsed() >= pacing.timeout {
            return fail(if slowed {
                "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again."
            } else {
                "Device flow timed out"
            });
        }
        let (poll_url, poll_device) = (url.clone(), device.clone());
        match spawned(
            handle,
            async move { poll_once(&poll_url, &poll_device).await },
        )
        .await?
        {
            Poll::Approved(approval) => return Ok(approval),
            Poll::Pending => {}
            Poll::SlowDown => {
                slowed = true;
                interval += pacing.slow_down_step;
            }
        }
        // Wake often enough to notice cancellation.
        let wake = tokio::time::Instant::now() + interval;
        while tokio::time::Instant::now() < wake {
            if cancellation.is_cancelled() {
                return fail("Login cancelled");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

const SUCCESS_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Signed in</title><body style=\"font-family:sans-serif\"><h1>Signed in</h1><p>You can close this tab and return to the terminal.</p></body>";

fn error_page(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>Sign-in failed</title><body style=\"font-family:sans-serif\"><h1>Sign-in failed</h1><p>{escaped}</p></body>"
    )
}

async fn respond(socket: &mut tokio::net::TcpStream, status: &str, html: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        html.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(html.as_bytes()).await;
    let _ = socket.flush().await;
}

/// Binds the callback listener on `host:port`.
///
/// # Errors
///
/// Returns an error when the address is taken, which is the cue to fall back to a
/// pasted redirect URL.
pub async fn bind_callback(host: &str, port: u16) -> Result<TcpListener, OAuthError> {
    TcpListener::bind((host, port))
        .await
        .map_err(|error| OAuthError(format!("could not listen on {host}:{port}: {error}")))
}

/// Waits on `listener` for the browser's redirect and returns the authorization code.
///
/// Requests for other paths are answered 404 and ignored. A redirect with the wrong
/// `state`, or carrying an `error`, ends the wait with an error and the matching page.
///
/// # Errors
///
/// Returns an error on a state mismatch, a provider error, timeout, or cancellation.
pub async fn serve_callback(
    listener: &TcpListener,
    path: &str,
    expected_state: &str,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<String, OAuthError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cancellation.is_cancelled() {
            return fail("Login cancelled");
        }
        if tokio::time::Instant::now() >= deadline {
            return fail("Timed out waiting for the browser sign-in");
        }
        let accepted = tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
        let Ok(Ok((mut socket, _))) = accepted else {
            continue;
        };
        let mut received = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") && received.len() < 16_384 {
            let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            if read == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8_lossy(&received).into_owned();
        let target = request
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or_default();
        let Ok(url) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
            respond(
                &mut socket,
                "400 Bad Request",
                &error_page("Malformed request"),
            )
            .await;
            continue;
        };
        if url.path() != path {
            respond(&mut socket, "404 Not Found", &error_page("Not found")).await;
            continue;
        }
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        if let Some(error) = param("error") {
            let message = param("error_description").unwrap_or(error);
            respond(&mut socket, "400 Bad Request", &error_page(&message)).await;
            return fail(format!("OpenAI sign-in failed: {message}"));
        }
        if param("state").as_deref() != Some(expected_state) {
            respond(
                &mut socket,
                "400 Bad Request",
                &error_page("State mismatch"),
            )
            .await;
            return fail("State mismatch");
        }
        let Some(code) = param("code").filter(|code| !code.is_empty()) else {
            respond(
                &mut socket,
                "400 Bad Request",
                &error_page("Missing authorization code"),
            )
            .await;
            return fail("Missing authorization code");
        };
        respond(&mut socket, "200 OK", SUCCESS_PAGE).await;
        return Ok(code);
    }
}

#[cfg(test)]
mod tests;
