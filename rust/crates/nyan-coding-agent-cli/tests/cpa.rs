//! `nyan login`, `nyan logout`, `--list-models`, and a run through the stored login,
//! against a loopback server that behaves like `CLIProxyAPI`: it serves
//! `/v1/models` and `/backend-api/codex/responses` and checks the bearer key.
//! Nothing here uses a real proxy or a real credential.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
mod support;
use support::Home;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

struct Proxy {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

const CATALOG: &str = r#"{"models":[
  {"slug":"gpt-a","display_name":"GPT A","context_window":500000,"max_output_tokens":128000,
   "input_modalities":["text","image"],"supported_reasoning_levels":["low","high"]},
  {"slug":"claude-b","context_window":1000000},
  {"slug":"deepseek-c","context_window":64000}
]}"#;

fn responses_stream() -> String {
    let mut body = String::new();
    for event in [
        json!({"type": "response.output_text.delta", "delta": "Hi"}),
        json!({"type": "response.output_text.delta", "delta": " there"}),
        json!({"type": "response.completed", "response": {"status": "completed",
               "usage": {"input_tokens": 5, "output_tokens": 2}}}),
    ] {
        let _ = write!(
            body,
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().expect("type")
        );
    }
    body
}

/// Serves any number of requests; `valid_key` is the only bearer it accepts.
fn proxy(runtime: &Runtime, valid_key: &'static str) -> Proxy {
    let listener = runtime
        .block_on(TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    runtime.spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut received = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    received.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&received).into_owned();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if received.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&received).into_owned();
                log.lock().expect("lock").push(request.clone());
                let lower = request.to_ascii_lowercase();
                let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
                let authorized = lower.contains(&format!("authorization: bearer {valid_key}\r\n"));
                let (status, kind, body) = if !authorized {
                    ("401 Unauthorized", "text/plain", "invalid api key".to_owned())
                } else if path.starts_with("/v1/models") {
                    ("200 OK", "application/json", CATALOG.to_owned())
                } else if path.starts_with("/backend-api/codex/responses") {
                    ("200 OK", "text/event-stream", responses_stream())
                } else {
                    ("404 Not Found", "text/plain", "no such route".to_owned())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    Proxy { address, requests }
}

fn url_of(proxy: &Proxy) -> String {
    format!("http://{}", proxy.address)
}

fn exists(path: &Path) -> bool {
    path.exists()
}

#[test]
fn login_proves_the_key_stores_it_and_saves_the_chosen_default_model() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "sk-cpa");
    let home = Home::new("login");
    let url = url_of(&proxy);
    let outcome = home.go(&["login", "cliproxyapi"], &[], &[&url, "sk-cpa", "2"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("Registered 3 model(s) from"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("Default model: cliproxyapi/claude-b"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        home.json("auth.json"),
        json!({"cliproxyapi": {"type": "api_key", "key": "sk-cpa",
                               "env": {"CLIPROXYAPI_BASE_URL": url}}})
    );
    let settings = home.json("settings.json");
    assert_eq!(settings["defaultProvider"], "cliproxyapi");
    assert_eq!(settings["defaultModel"], "claude-b");
    let request = proxy.requests.lock().expect("lock")[0].to_ascii_lowercase();
    assert!(
        request.starts_with("get /v1/models?client_version=pi http/1.1"),
        "{request}"
    );
    assert!(
        outcome.asked[0].starts_with("CLIProxyAPI base URL [http://127.0.0.1:8317]: "),
        "{:?}",
        outcome.asked
    );
    home.cleanup();
}

#[test]
fn a_rejected_key_or_a_bad_url_asks_again_and_stores_nothing_until_one_works() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "good");
    let home = Home::new("retry");
    let url = url_of(&proxy);
    let outcome = home.go(
        &["login", "cliproxyapi"],
        &[],
        &["http://", "", &url, "  ", &url, "bad", &url, "good", ""],
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("Invalid base URL:"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("API key cannot be empty."),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains(
            "Login validation failed: the models request failed with HTTP 401: invalid api key"
        ),
        "{}",
        outcome.stdout
    );
    assert_eq!(home.json("auth.json")["cliproxyapi"]["key"], "good");
    assert!(
        outcome.stdout.contains("No default model saved"),
        "a blank choice skips: {}",
        outcome.stdout
    );
    assert!(!exists(&home.agent.join("settings.json")));
    home.cleanup();
}

#[test]
fn a_cancelled_login_stores_nothing_and_exits_one() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "good");
    let home = Home::new("cancel");
    let url = url_of(&proxy);
    let outcome = home.go(&["login", "cliproxyapi"], &[], &[&url, "wrong"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.stderr, "Error: Login cancelled\n");
    assert!(!exists(&home.agent.join("auth.json")));
    let unknown = home.go(&["login", "nowhere"], &[], &[]);
    assert_eq!(unknown.code, 1);
    assert!(
        unknown
            .stderr
            .contains("cannot log in to \"nowhere\" (available: cliproxyapi, azure, "),
        "{}",
        unknown.stderr
    );
    home.cleanup();
}

#[test]
fn a_run_uses_the_stored_login_and_the_saved_default_model() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "sk-cpa");
    let home = Home::new("run");
    let url = url_of(&proxy);
    assert_eq!(
        home.go(&["login", "cliproxyapi"], &[], &[&url, "sk-cpa", "1"])
            .code,
        0
    );
    proxy.requests.lock().expect("lock").clear();

    let outcome = home.go(&["-p", "--no-session", "say hi"], &[], &[]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(outcome.stdout, "Hi there\n");
    let request = proxy.requests.lock().expect("lock")[0].clone();
    assert!(
        request.starts_with("POST /backend-api/codex/responses HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-cpa")
    );
    let body: Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["model"], "gpt-a");
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][0]["role"], "developer");
    assert_eq!(body["input"][1]["content"][0]["text"], "say hi");

    let other = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "cliproxyapi/claude-b",
            "again",
        ],
        &[],
        &[],
    );
    assert_eq!(other.code, 0, "{}", other.stderr);
    let second = proxy.requests.lock().expect("lock")[1].clone();
    let second_body: Value =
        serde_json::from_str(second.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(
        second_body["model"], "claude-b",
        "--model beats the saved default"
    );
    home.cleanup();
}

#[test]
fn the_environment_beats_the_stored_key_and_the_flag_beats_the_environment() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "from-flag");
    let home = Home::new("precedence");
    let url = url_of(&proxy);
    let stored = nyan_coding_agent_auth::AuthStore::new(&home.agent);
    stored
        .save_api_key(
            "cliproxyapi",
            "stored",
            &BTreeMap::from([("CLIPROXYAPI_BASE_URL".to_owned(), url)]),
        )
        .expect("store");
    let env = [("CLIPROXYAPI_API_KEY", "from-env")];
    let by_env = home.go(
        &["-p", "--no-session", "--model", "cliproxyapi/m", "x"],
        &env,
        &[],
    );
    assert_eq!(by_env.code, 1, "the proxy only accepts the flag key");
    assert!(
        proxy.requests.lock().expect("lock")[0]
            .to_ascii_lowercase()
            .contains("bearer from-env")
    );
    let by_flag = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "cliproxyapi/m",
            "--api-key",
            "from-flag",
            "x",
        ],
        &env,
        &[],
    );
    assert_eq!(by_flag.code, 0, "{}", by_flag.stderr);
    home.cleanup();
}

#[test]
fn without_a_login_or_a_default_the_error_says_what_to_do() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "k");
    let home = Home::new("guidance");
    let nothing = home.go(&["-p", "hi"], &[], &[]);
    assert_eq!(nothing.code, 1);
    assert_eq!(
        nothing.stderr,
        "Error: no model selected and nothing is logged in: run `nyan login`, then choose a default model\n"
    );
    let missing = home.go(&["-p", "--model", "cliproxyapi/x", "hi"], &[], &[]);
    assert_eq!(missing.code, 1);
    assert_eq!(
        missing.stderr,
        "Error: no credential for provider \"cliproxyapi\": run `nyan login cliproxyapi`\n"
    );

    let url = url_of(&proxy);
    assert_eq!(
        home.go(&["login", "cliproxyapi"], &[], &[&url, "k", ""])
            .code,
        0
    );
    let undecided = home.go(&["-p", "hi"], &[], &[]);
    assert_eq!(undecided.code, 1);
    assert!(
        undecided
            .stderr
            .contains("run `nyan login <provider>` to choose a default from its model list"),
        "{}",
        undecided.stderr
    );
    home.cleanup();
}

#[test]
fn logout_removes_the_login_and_reports_when_there_is_none() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "k");
    let home = Home::new("logout");
    let url = url_of(&proxy);
    assert_eq!(
        home.go(&["login", "cliproxyapi"], &[], &[&url, "k", ""])
            .code,
        0
    );
    let out = home.go(&["logout"], &[], &[]);
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (0, "Logged out of cliproxyapi.\n")
    );
    assert_eq!(home.json("auth.json"), json!({}));
    let again = home.go(&["logout", "cliproxyapi"], &[], &[]);
    assert_eq!(again.code, 1);
    assert_eq!(again.stderr, "Error: no stored login for \"cliproxyapi\"\n");
    assert_eq!(
        home.go(&["logout"], &[], &[]).stderr,
        "Error: nothing is logged in\n"
    );
    home.cleanup();
}

#[test]
fn list_models_shows_the_logged_in_catalog_and_filters_by_search() {
    let runtime = Runtime::new().expect("runtime");
    let proxy = proxy(&runtime, "k");
    let home = Home::new("list");
    let none = home.go(&["--list-models"], &[], &[]);
    assert_eq!(
        (none.code, none.stdout.as_str()),
        (
            0,
            "No models available. Run `nyan login` to add a provider.\n"
        )
    );

    let url = url_of(&proxy);
    assert_eq!(
        home.go(&["login", "cliproxyapi"], &[], &[&url, "k", ""])
            .code,
        0
    );
    let all = home.go(&["--list-models"], &[], &[]);
    assert_eq!(all.code, 0, "{}", all.stderr);
    let lines: Vec<&str> = all.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{}", all.stdout);
    assert!(
        lines[1].starts_with("cliproxyapi             gpt-a"),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains("500K")
            && lines[1].contains("128K")
            && lines[1].ends_with("yes       yes"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains("claude-b") && lines[2].contains("1M"),
        "{}",
        lines[2]
    );

    let filtered = home.go(&["--list-models", "deepseek"], &[], &[]);
    assert_eq!(filtered.stdout.lines().count(), 2, "{}", filtered.stdout);
    let nothing = home.go(&["--list-models", "zzz"], &[], &[]);
    assert_eq!(nothing.stdout, "No models match the search.\n");
    home.cleanup();
}
