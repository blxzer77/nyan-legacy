//! `openai-codex` end to end: the device-code and browser sign-ins, the stored
//! login, its refresh, and a run through the Codex endpoint. A loopback server
//! stands in for both `auth.openai.com` and `chatgpt.com`; nothing here reaches
//! the real services or uses a real account.
#![allow(clippy::unreadable_literal)]

use nyan_coding_agent_auth::{AuthStore, Credential, OAuthCredential};
use nyan_coding_agent_provider_http::codex_auth::base64url_encode;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

mod support;
use support::{Home, stub, stub_sequence};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .try_into()
        .expect("fits")
}

/// An access token whose claims name `account`.
fn access_token(account: &str, marker: &str) -> String {
    let claims =
        json!({"https://api.openai.com/auth": {"chatgpt_account_id": account}, "marker": marker});
    format!(
        "{}.{}.signature",
        base64url_encode(br#"{"alg":"none"}"#),
        base64url_encode(claims.to_string().as_bytes())
    )
}

fn token_reply(account: &str, marker: &str, refresh: &str) -> String {
    json!({"access_token": access_token(account, marker), "refresh_token": refresh, "expires_in": 3600}).to_string()
}

/// A query parameter of `url`; the values this test reads (hex, base64url) need no decoding.
fn query_param(url: &str, name: &str) -> Option<String> {
    url.split_once('?')?
        .1
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
        .map(str::to_owned)
}

fn stored(home: &Home) -> OAuthCredential {
    match AuthStore::new(&home.agent).credential("openai-codex") {
        Ok(Some(Credential::OAuth(login))) => login,
        other => panic!("expected a stored OAuth login, got {other:?}"),
    }
}

fn seed_login(home: &Home, marker: &str, refresh: &str, expires_ms: u64) {
    let login = OAuthCredential {
        access: access_token("acct-seed", marker),
        refresh: refresh.to_owned(),
        expires_ms,
        extra: [("accountId".to_owned(), json!("acct-seed"))].into(),
    };
    AuthStore::new(&home.agent)
        .save_oauth("openai-codex", &login)
        .expect("seed");
}

fn responses_reply() -> Vec<u8> {
    b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n".to_vec()
}

#[test]
fn a_device_code_login_shows_the_code_stores_the_login_and_offers_the_builtin_models() {
    let runtime = Runtime::new().expect("runtime");
    let auth = stub_sequence(
        &runtime,
        vec![
            (
                "200 OK",
                json!({"device_auth_id": "dev-1", "user_code": "WXYZ-9999", "interval": 0})
                    .to_string(),
            ),
            ("403 Forbidden", String::new()),
            (
                "200 OK",
                json!({"authorization_code": "approved-code", "code_verifier": "server-verifier"})
                    .to_string(),
            ),
            (
                "200 OK",
                token_reply("acct-device", "device", "refresh-device"),
            ),
        ],
    );
    let home = Home::new("codex-device");
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    let outcome = home.go(&["login", "openai-codex"], &[], &["2", "1"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(
        outcome
            .stdout
            .contains("/codex/device and enter the code WXYZ-9999"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("Logged in to OpenAI Codex."),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("Default model: openai-codex/"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        outcome.opened,
        Vec::<String>::new(),
        "the device flow opens no browser"
    );

    let login = stored(&home);
    assert_eq!(login.refresh, "refresh-device");
    assert_eq!(login.extra_str("accountId"), Some("acct-device"));
    assert!(
        login.expires_ms > now_ms() + 3_000_000,
        "{}",
        login.expires_ms
    );
    assert_eq!(
        home.json("settings.json")["defaultProvider"],
        "openai-codex"
    );

    let requests = auth.requests.lock().expect("lock");
    let exchange = requests.last().expect("the exchange");
    assert!(
        exchange.starts_with("POST /oauth/token HTTP/1.1"),
        "{exchange}"
    );
    assert!(
        exchange.contains("code=approved-code")
            && exchange.contains("code_verifier=server-verifier"),
        "{exchange}"
    );
    assert!(
        exchange.contains("redirect_uri=http%3A%2F%2F"),
        "{exchange}"
    );
    assert!(
        exchange.contains("%2Fdeviceauth%2Fcallback"),
        "the device redirect, not the browser one: {exchange}"
    );
    drop(requests);
    home.cleanup();
}

#[test]
fn a_browser_login_opens_the_authorize_url_and_accepts_a_pasted_code() {
    let runtime = Runtime::new().expect("runtime");
    let auth = stub_sequence(
        &runtime,
        vec![(
            "200 OK",
            token_reply("acct-browser", "browser", "refresh-browser"),
        )],
    );
    let home = Home::new("codex-browser-paste");
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    let outcome = home.go(&["login", "openai-codex"], &[], &["", "just-a-code", ""]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let url = outcome.opened[0].clone();
    assert!(
        url.starts_with(&format!("http://{}/oauth/authorize?", auth.address)),
        "{url}"
    );
    let param = |name: &str| query_param(&url, name);
    assert_eq!(param("code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(param("originator").as_deref(), Some("nyan"));
    assert_eq!(param("state").map(|state| state.len()), Some(32));
    assert!(
        outcome.stdout.contains(&outcome.opened[0]),
        "the URL is printed too: {}",
        outcome.stdout
    );
    assert_eq!(stored(&home).extra_str("accountId"), Some("acct-browser"));
    let exchange = auth.requests.lock().expect("lock")[0].clone();
    assert!(exchange.contains("code=just-a-code"), "{exchange}");
    assert!(
        exchange.contains("grant_type=authorization_code"),
        "{exchange}"
    );
    // The verifier sent now must hash to the challenge the URL carried.
    let verifier = exchange
        .split('&')
        .find_map(|pair| pair.strip_prefix("code_verifier="))
        .expect("a verifier");
    assert_eq!(
        nyan_coding_agent_provider_http::codex_auth::challenge_for(verifier),
        param("code_challenge").expect("a challenge")
    );
    home.cleanup();
}

#[test]
fn a_pasted_redirect_with_the_wrong_state_is_refused_and_stores_nothing() {
    let runtime = Runtime::new().expect("runtime");
    let auth = stub_sequence(&runtime, vec![("200 OK", token_reply("a", "m", "r"))]);
    let home = Home::new("codex-wrong-state");
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    let outcome = home.go(
        &["login", "openai-codex"],
        &[],
        &[
            "1",
            "http://localhost:1455/auth/callback?code=c&state=not-the-state",
        ],
    );
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.stderr, "Error: State mismatch\n");
    assert!(
        auth.requests.lock().expect("lock").is_empty(),
        "no code was exchanged"
    );
    assert!(
        AuthStore::new(&home.agent)
            .credential("openai-codex")
            .expect("read")
            .is_none()
    );
    home.cleanup();
}

#[test]
fn the_browser_redirect_on_the_local_port_completes_the_login_without_pasting() {
    let runtime = Runtime::new().expect("runtime");
    let auth = stub_sequence(
        &runtime,
        vec![(
            "200 OK",
            token_reply("acct-callback", "cb", "refresh-callback"),
        )],
    );
    let home = Home::new("codex-browser-callback");
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("addr")
        .port();
    home.listen_on(port);
    // The "browser": follows the authorize URL by calling back with its state.
    home.when_opened(move |url| {
        let state = query_param(url, "state").expect("state");
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for _ in 0..100 {
                if let Ok(mut socket) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                    let _ = socket.write_all(
                        format!("GET /auth/callback?code=browser-code&state={state} HTTP/1.1\r\nhost: localhost\r\n\r\n").as_bytes(),
                    );
                    let mut reply = String::new();
                    let _ = socket.read_to_string(&mut reply);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    });
    // The terminal never answers: only the redirect can finish the login.
    let outcome = home.go(&["login", "openai-codex"], &[], &["1", "<block>", ""]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(stored(&home).extra_str("accountId"), Some("acct-callback"));
    assert!(auth.requests.lock().expect("lock")[0].contains("code=browser-code"));
    home.cleanup();
}

#[test]
fn a_run_uses_the_stored_login_without_refreshing_while_it_is_valid() {
    let runtime = Runtime::new().expect("runtime");
    let codex = stub(&runtime, "text/event-stream", responses_reply());
    let auth = stub_sequence(
        &runtime,
        vec![("500 Internal Server Error", "must not be called".to_owned())],
    );
    let home = Home::new("codex-run");
    home.route(
        "openai-codex",
        &format!("http://{}/backend-api", codex.address),
    );
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    seed_login(&home, "valid", "refresh-0", now_ms() + 3_600_000);
    let outcome = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "openai-codex/gpt-6-sol",
            "say hi",
        ],
        &[],
        &[],
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(outcome.stdout, "Hi\n");
    let request = codex.requests.lock().expect("lock")[0].clone();
    let lower = request.to_ascii_lowercase();
    assert!(
        request.starts_with("POST /backend-api/codex/responses HTTP/1.1"),
        "{request}"
    );
    assert!(
        lower.contains(&format!(
            "authorization: bearer {}",
            access_token("acct-seed", "valid").to_ascii_lowercase()
        )),
        "{request}"
    );
    assert!(lower.contains("chatgpt-account-id: acct-seed"), "{request}");
    let body: Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["model"], "gpt-6-sol");
    assert!(
        body["instructions"]
            .as_str()
            .expect("instructions")
            .starts_with("You are an expert coding assistant")
    );
    assert!(
        auth.requests.lock().expect("lock").is_empty(),
        "a valid login is not refreshed"
    );
    home.cleanup();
}

#[test]
fn an_expiring_login_is_refreshed_before_the_request_and_the_new_tokens_are_saved() {
    let runtime = Runtime::new().expect("runtime");
    let codex = stub(&runtime, "text/event-stream", responses_reply());
    let auth = stub_sequence(
        &runtime,
        vec![("200 OK", token_reply("acct-seed", "renewed", "refresh-1"))],
    );
    let home = Home::new("codex-refresh");
    home.route(
        "openai-codex",
        &format!("http://{}/backend-api", codex.address),
    );
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    seed_login(&home, "stale", "refresh-0", now_ms() + 10_000);
    let outcome = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "openai-codex/gpt-6-sol",
            "hi",
        ],
        &[],
        &[],
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);

    let refresh = auth.requests.lock().expect("lock")[0].clone();
    assert!(
        refresh.contains("grant_type=refresh_token") && refresh.contains("refresh_token=refresh-0"),
        "{refresh}"
    );
    let request = codex.requests.lock().expect("lock")[0].to_ascii_lowercase();
    assert!(
        request.contains(&access_token("acct-seed", "renewed").to_ascii_lowercase()),
        "the new token is used"
    );
    let login = stored(&home);
    assert_eq!(
        login.refresh, "refresh-1",
        "the rotated refresh token is saved"
    );
    assert!(login.access.contains('.') && login.expires_ms > now_ms() + 3_000_000);
    home.cleanup();
}

#[test]
fn a_refused_refresh_fails_the_run_with_a_pointer_to_login_and_keeps_the_stored_login() {
    let runtime = Runtime::new().expect("runtime");
    let codex = stub(&runtime, "text/event-stream", responses_reply());
    let auth = stub_sequence(
        &runtime,
        vec![("400 Bad Request", r#"{"error":"invalid_grant"}"#.to_owned())],
    );
    let home = Home::new("codex-refused");
    home.route(
        "openai-codex",
        &format!("http://{}/backend-api", codex.address),
    );
    home.route("openai-codex:auth", &format!("http://{}", auth.address));
    seed_login(&home, "stale", "refresh-0", 1);
    let before = std::fs::read_to_string(home.agent.join("auth.json")).expect("read");
    let outcome = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "openai-codex/gpt-6-sol",
            "hi",
        ],
        &[],
        &[],
    );
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.stdout, "");
    assert!(
        outcome
            .stderr
            .contains("OpenAI Codex token refresh failed (400)"),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("run `nyan login openai-codex` to sign in again"),
        "{}",
        outcome.stderr
    );
    assert!(
        codex.requests.lock().expect("lock").is_empty(),
        "no request went out"
    );
    assert_eq!(
        std::fs::read_to_string(home.agent.join("auth.json")).expect("read"),
        before
    );
    home.cleanup();
}

#[test]
fn without_a_codex_login_the_error_says_how_to_get_one() {
    let home = Home::new("codex-none");
    let none = home.go(&["-p", "--model", "openai-codex/gpt-6-sol", "hi"], &[], &[]);
    assert_eq!(
        none.stderr,
        "Error: not logged in to openai-codex: run `nyan login openai-codex`\n"
    );
    AuthStore::new(&home.agent)
        .save_api_key(
            "openai-codex",
            "sk-wrong-kind",
            &std::collections::BTreeMap::new(),
        )
        .expect("store");
    let wrong = home.go(&["-p", "--model", "openai-codex/gpt-6-sol", "hi"], &[], &[]);
    assert!(
        wrong
            .stderr
            .contains("is an API key, but Codex needs a ChatGPT sign-in"),
        "{}",
        wrong.stderr
    );
    home.cleanup();
}

#[test]
fn list_models_shows_the_codex_models_with_only_a_login_and_no_network() {
    let home = Home::new("codex-list");
    seed_login(&home, "valid", "r", now_ms() + 3_600_000);
    let outcome = home.go(&["--list-models", "codex"], &[], &[]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(
        outcome.stderr, "",
        "the built-in list needs no models.dev fetch"
    );
    let lines: Vec<&str> = outcome.stdout.lines().collect();
    assert!(lines.len() >= 2, "{}", outcome.stdout);
    assert!(lines[1].starts_with("openai-codex"), "{}", lines[1]);
    assert!(
        outcome.stdout.contains("gpt-5.3-codex-spark") && outcome.stdout.contains("128K"),
        "{}",
        outcome.stdout
    );
    // Logged out, it is not offered.
    let bare = Home::new("codex-list-none");
    assert_eq!(
        bare.go(&["--list-models"], &[], &[]).stdout,
        "No models available. Run `nyan login` to add a provider.\n"
    );
    home.cleanup();
    bare.cleanup();
}
