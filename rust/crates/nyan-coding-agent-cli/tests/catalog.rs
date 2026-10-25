//! The model catalog end to end: fetching models.dev, the 24-hour cache, stale
//! fallback, `--list-models`, a provider whose API comes from the catalog, and the
//! output limit Anthropic requests carry. A loopback server stands in for models.dev.
#![allow(clippy::unreadable_literal)] // fixture numbers mirror the models.dev data

use nyan_coding_agent_models::cache;
use serde_json::{Map, Value, json};
use tokio::runtime::Runtime;

mod support;
use support::{Home, stub};

fn models_dev() -> Value {
    json!({
        "groq": {"models": {
            "llama-a": {"name": "Llama A", "tool_call": true, "reasoning": false,
                "limit": {"context": 131072, "output": 8192},
                "modalities": {"input": ["text"], "output": ["text"]}},
            "no-tools": {"tool_call": false},
        }},
        "anthropic": {"models": {
            "claude-x": {"name": "Claude X", "tool_call": true, "reasoning": true,
                "limit": {"context": 200000, "output": 64000},
                "modalities": {"input": ["text", "image"], "output": ["text"]}},
        }},
        "fireworks-ai": {"models": {
            "accounts/fireworks/models/glm-5": {"tool_call": true, "limit": {"context": 8000, "output": 2000}},
            "accounts/fireworks/models/other": {"tool_call": true, "limit": {"context": 9000, "output": 3000}},
        }},
    })
}

fn seed_cache(home: &Home, timestamp_ms: u64) {
    let Value::Object(providers) = models_dev() else {
        unreachable!()
    };
    cache::write(&home.agent, &providers, timestamp_ms).expect("seed the cache");
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .try_into()
        .expect("fits")
}

fn serve_models_dev(runtime: &Runtime, home: &Home) -> support::Stub {
    let server = stub(
        runtime,
        "application/json",
        models_dev().to_string().into_bytes(),
    );
    home.serve_models_dev(&format!("http://{}/api.json", server.address));
    server
}

#[test]
fn list_models_fetches_when_there_is_no_cache_then_serves_the_fresh_cache_offline() {
    let runtime = Runtime::new().expect("runtime");
    let home = Home::new("list-fetch");
    let server = serve_models_dev(&runtime, &home);
    let env = [("GROQ_API_KEY", "k"), ("ANTHROPIC_API_KEY", "k")];

    let first = home.go(&["--list-models"], &env, &[]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert_eq!(first.stderr, "");
    let lines: Vec<&str> = first.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{}", first.stdout);
    assert!(
        lines[1].starts_with("anthropic               claude-x"),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains("200K")
            && lines[1].contains("64K")
            && lines[1].ends_with("yes       yes"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("groq                    llama-a"),
        "{}",
        lines[2]
    );
    assert_eq!(server.requests.lock().expect("lock").len(), 1);
    let saved = cache::read(&home.agent).expect("the fetch was cached");
    assert!(saved.is_fresh(now_ms()));

    home.serve_models_dev("http://127.0.0.1:1/api.json");
    let second = home.go(&["--list-models", "llama"], &env, &[]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert_eq!(
        second.stderr, "",
        "a fresh cache is used silently, not as a fallback"
    );
    assert_eq!(second.stdout.lines().count(), 2, "{}", second.stdout);
    assert_eq!(
        server.requests.lock().expect("lock").len(),
        1,
        "a fresh cache needs no request"
    );
    home.cleanup();
}

#[test]
fn a_stale_cache_is_refreshed_and_when_that_fails_it_is_used_with_a_warning() {
    let runtime = Runtime::new().expect("runtime");
    let home = Home::new("stale");
    let env = [("GROQ_API_KEY", "k")];
    seed_cache(&home, now_ms() - 3 * 24 * 3_600_000);

    let offline = home.go(&["--list-models"], &env, &[]);
    assert_eq!(offline.code, 0, "{}", offline.stderr);
    assert!(offline.stdout.contains("llama-a"), "{}", offline.stdout);
    assert!(
        offline.stderr.starts_with(
            "Warning: could not refresh the model data (could not reach http://127.0.0.1:1/api.json"
        ),
        "{}",
        offline.stderr
    );
    assert!(
        offline.stderr.contains("using data that is 3 days old"),
        "{}",
        offline.stderr
    );

    let server = serve_models_dev(&runtime, &home);
    let online = home.go(&["--list-models"], &env, &[]);
    assert_eq!(online.stderr, "");
    assert_eq!(server.requests.lock().expect("lock").len(), 1);
    assert!(
        cache::read(&home.agent).expect("cache").is_fresh(now_ms()),
        "the cache was renewed"
    );
    home.cleanup();
}

#[test]
fn without_any_model_data_the_command_that_needs_it_says_so() {
    let home = Home::new("no-data");
    let outcome = home.go(&["--list-models"], &[("GROQ_API_KEY", "k")], &[]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome
            .stderr
            .starts_with("Error: the model data is unavailable: could not reach"),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("connect to the network and try again"),
        "{}",
        outcome.stderr
    );
    // Nothing is configured, so nothing needs the data: that is not an error.
    let nothing = home.go(&["--list-models"], &[], &[]);
    assert_eq!(nothing.code, 0);
    assert_eq!(
        nothing.stdout,
        "No models available. Run `nyan login` to add a provider.\n"
    );
    home.cleanup();
}

#[test]
fn a_single_api_provider_never_waits_for_the_network_to_send_a_request() {
    let runtime = Runtime::new().expect("runtime");
    let model_server = stub(
        &runtime,
        "text/event-stream",
        b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec(),
    );
    let home = Home::new("no-wait");
    home.route("groq", &format!("http://{}/v1", model_server.address));
    let outcome = home.go(
        &["-p", "--no-session", "--model", "groq/anything", "hi"],
        &[("GROQ_API_KEY", "k")],
        &[],
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(
        outcome.stderr, "",
        "no catalog warning for a run that does not need it"
    );
    assert!(
        !cache::path(&home.agent).exists(),
        "a run does not write the cache"
    );
    home.cleanup();
}

#[test]
fn anthropic_sends_the_models_output_limit_from_the_cache_and_a_default_without_one() {
    let runtime = Runtime::new().expect("runtime");
    let server = stub(
        &runtime,
        "text/event-stream",
        b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n".to_vec(),
    );
    let home = Home::new("max-tokens");
    home.route("anthropic", &format!("http://{}", server.address));
    // A models.dev stand-in that must never be asked: the limit is read from the cache only.
    let models_dev_server = serve_models_dev(&runtime, &home);
    let env = [("ANTHROPIC_API_KEY", "k")];
    let sent = || -> Value {
        let requests = server.requests.lock().expect("lock");
        let request = requests.last().expect("a request");
        serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json")
    };
    let run = |model: &str| {
        let outcome = home.go(&["-p", "--no-session", "--model", model, "hi"], &env, &[]);
        assert_eq!(outcome.code, 0, "{}", outcome.stderr);
        assert_eq!(
            outcome.stderr, "",
            "no catalog warning for a run that does not need it"
        );
        assert_eq!(
            models_dev_server.requests.lock().expect("lock").len(),
            0,
            "a request must not wait for or trigger a models.dev fetch"
        );
    };
    run("anthropic/claude-x");
    assert_eq!(sent()["max_tokens"], 16_384, "no cache: the default");
    seed_cache(&home, 1);
    run("anthropic/claude-x");
    assert_eq!(
        sent()["max_tokens"],
        64_000,
        "a stale cache still tells the output limit"
    );
    run("anthropic/not-in-the-catalog");
    assert_eq!(
        sent()["max_tokens"],
        16_384,
        "an unknown model gets the default"
    );
    home.cleanup();
}

#[test]
fn a_provider_that_mixes_wire_apis_takes_each_models_api_from_the_catalog() {
    let runtime = Runtime::new().expect("runtime");
    let chat = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec();
    let server = stub(&runtime, "text/event-stream", chat);
    let home = Home::new("mixed");
    home.route("fireworks", &format!("http://{}", server.address));
    seed_cache(&home, now_ms());
    let env = [("FIREWORKS_API_KEY", "fw-key")];

    let glm = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "fireworks/accounts/fireworks/models/glm-5",
            "hi",
        ],
        &env,
        &[],
    );
    assert_eq!(glm.code, 0, "{}", glm.stderr);
    assert_eq!(glm.stdout, "Hi\n");
    let request = server.requests.lock().expect("lock")[0].clone();
    assert!(
        request.starts_with("POST /chat/completions HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer fw-key"),
        "{request}"
    );

    let other = home.go(
        &[
            "-p",
            "--no-session",
            "--model",
            "fireworks/accounts/fireworks/models/other",
            "hi",
        ],
        &env,
        &[],
    );
    assert_eq!(
        other.code, 1,
        "this one speaks Anthropic Messages, which the stub does not"
    );
    let request = server.requests.lock().expect("lock")[1].clone();
    assert!(
        request.starts_with("POST /v1/messages HTTP/1.1"),
        "{request}"
    );
    assert!(
        request.to_ascii_lowercase().contains("x-api-key: fw-key"),
        "{request}"
    );

    let unknown = home.go(
        &["-p", "--no-session", "--model", "fireworks/nope", "hi"],
        &env,
        &[],
    );
    assert_eq!(unknown.code, 1);
    assert_eq!(
        unknown.stderr,
        "Error: model \"nope\" is not in the catalog for provider \"fireworks\"; `nyan --list-models` shows what is\n"
    );
    home.cleanup();
}

#[test]
fn login_offers_the_providers_models_from_the_catalog_and_saves_the_choice() {
    let runtime = Runtime::new().expect("runtime");
    let home = Home::new("login-catalog");
    let _server = serve_models_dev(&runtime, &home);
    let outcome = home.go(&["login", "anthropic"], &[], &["sk-ant-typed", "1"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("1) claude-x"), "{}", outcome.stdout);
    assert!(
        outcome.stdout.contains("Default model: anthropic/claude-x"),
        "{}",
        outcome.stdout
    );
    assert_eq!(home.json("settings.json")["defaultModel"], "claude-x");
    assert_eq!(
        outcome.asked.last().map(String::as_str),
        Some("Default model (number or id, blank to skip): ")
    );

    // A provider the catalog does not list falls back to typing an id.
    let typed = home.go(&["login", "deepseek"], &[], &["ds-key", "deepseek-v4"]);
    assert_eq!(typed.code, 0, "{}", typed.stderr);
    assert!(
        typed.stdout.contains("Default model: deepseek/deepseek-v4"),
        "{}",
        typed.stdout
    );
    home.cleanup();
}

#[test]
fn the_cache_is_the_typescript_file_so_a_map_it_wrote_is_usable_here() {
    let home = Home::new("shared-cache");
    let Value::Object(providers): Value = models_dev() else {
        unreachable!()
    };
    let mut typescript_shape = Map::new();
    typescript_shape.insert("timestamp".to_owned(), Value::from(now_ms()));
    typescript_shape.insert("providers".to_owned(), Value::Object(providers));
    std::fs::write(
        cache::path(&home.agent),
        Value::Object(typescript_shape).to_string(),
    )
    .expect("seed");
    let outcome = home.go(&["--list-models"], &[("GROQ_API_KEY", "k")], &[]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("llama-a"), "{}", outcome.stdout);
    home.cleanup();
}
