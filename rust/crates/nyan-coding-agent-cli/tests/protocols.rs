//! Every wire protocol, end to end through `nyan -p`: provider resolution, the
//! credential from the environment, the request the stub receives, and the
//! answer printed. Nothing here uses a real provider or credential.

use std::fmt::Write as _;

use nyan_coding_agent_provider_http::eventstream::crc32;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

mod support;
use support::{Home, stub};

fn sse(events: &[Value]) -> Vec<u8> {
    let mut text = String::new();
    for event in events {
        let _ = write!(text, "data: {event}\n\n");
    }
    text.into_bytes()
}

fn named_sse(events: &[Value]) -> Vec<u8> {
    let mut text = String::new();
    for event in events {
        let _ = write!(
            text,
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().expect("type")
        );
    }
    text.into_bytes()
}

fn frame(kind: &str, payload: &Value) -> Vec<u8> {
    let headers = [(":message-type", "event"), (":event-type", kind)];
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(u8::try_from(name.len()).expect("short"));
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(&u16::try_from(value.len()).expect("short").to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let payload = payload.to_string();
    let total = 12 + header_bytes.len() + payload.len() + 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(total).expect("size").to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(header_bytes.len())
            .expect("size")
            .to_be_bytes(),
    );
    let prelude = crc32(&bytes);
    bytes.extend_from_slice(&prelude.to_be_bytes());
    bytes.extend_from_slice(&header_bytes);
    bytes.extend_from_slice(payload.as_bytes());
    let message = crc32(&bytes);
    bytes.extend_from_slice(&message.to_be_bytes());
    bytes
}

fn responses_reply() -> Vec<u8> {
    named_sse(&[
        json!({"type": "response.output_text.delta", "delta": "Hi"}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ])
}

fn anthropic_reply() -> Vec<u8> {
    named_sse(&[
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ])
}

fn gemini_reply() -> Vec<u8> {
    sse(&[
        json!({"candidates": [{"finishReason": "STOP", "content": {"parts": [{"text": "Hi"}]}}]}),
    ])
}

fn chat_reply() -> Vec<u8> {
    sse(&[json!({"choices": [{"delta": {"content": "Hi"}, "finish_reason": "stop"}]})])
        .into_iter()
        .chain(b"data: [DONE]\n\n".iter().copied())
        .collect()
}

fn bedrock_reply() -> Vec<u8> {
    let mut bytes = frame(
        "contentBlockDelta",
        &json!({"contentBlockIndex": 0, "delta": {"text": "Hi"}}),
    );
    bytes.extend(frame("messageStop", &json!({"stopReason": "end_turn"})));
    bytes
}

struct Case {
    name: &'static str,
    model: &'static str,
    env: Vec<(&'static str, &'static str)>,
    /// The id the base URL is routed under (None: configured through the environment).
    route: Option<&'static str>,
    content_type: &'static str,
    reply: Vec<u8>,
    path_prefix: &'static str,
    /// A lowercase header line that must appear in the request.
    header: &'static str,
}

fn run_case(case: &Case) -> Value {
    let runtime = Runtime::new().expect("runtime");
    let server = stub(&runtime, case.content_type, case.reply.clone());
    let home = Home::new(case.name);
    let url = format!("http://{}", server.address);
    let mut env = case.env.clone();
    let bedrock_url;
    if let Some(route) = case.route {
        let base = match route {
            "google" => format!("{url}/v1beta"),
            "google-vertex" => format!("{url}/v1/publishers/google"),
            "azure" => format!("{url}/openai/v1"),
            "amazon-bedrock" | "anthropic" | "mistral" => url.clone(),
            _ => format!("{url}/v1"),
        };
        home.route(route, &base);
    }
    if case.name == "azure" {
        bedrock_url = format!("{url}/openai/v1");
        env.push(("AZURE_OPENAI_BASE_URL", &bedrock_url));
    }
    let outcome = home.go(
        &["-p", "--no-session", "--model", case.model, "say hi"],
        &env,
        &[],
    );
    assert_eq!(outcome.code, 0, "{}: {}", case.name, outcome.stderr);
    assert_eq!(outcome.stdout, "Hi\n", "{}", case.name);
    let request = server.requests.lock().expect("lock")[0].clone();
    let first_line = request.lines().next().expect("request line").to_owned();
    assert!(
        first_line.contains(case.path_prefix),
        "{}: expected {} in {first_line}",
        case.name,
        case.path_prefix
    );
    assert!(
        request.to_ascii_lowercase().contains(case.header),
        "{}: missing `{}` in\n{request}",
        case.name,
        case.header
    );
    home.cleanup();
    serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json body")
}

#[test]
fn every_protocol_reaches_its_endpoint_with_its_own_auth_and_request_shape() {
    let openai = run_case(&Case {
        name: "openai",
        model: "openai/gpt-x",
        env: vec![("OPENAI_API_KEY", "sk-openai")],
        route: Some("openai"),
        content_type: "text/event-stream",
        reply: responses_reply(),
        path_prefix: "POST /v1/responses",
        header: "authorization: bearer sk-openai",
    });
    assert_eq!(openai["store"], false);
    assert_eq!(openai["input"][0]["role"], "developer");

    let anthropic = run_case(&Case {
        name: "anthropic",
        model: "anthropic/claude-x",
        env: vec![("ANTHROPIC_API_KEY", "sk-ant-api")],
        route: Some("anthropic"),
        content_type: "text/event-stream",
        reply: anthropic_reply(),
        path_prefix: "POST /v1/messages",
        header: "x-api-key: sk-ant-api",
    });
    assert_eq!(anthropic["system"][0]["type"], "text");
    assert!(anthropic["messages"][0]["content"][0]["text"].is_string());

    let google = run_case(&Case {
        name: "google",
        model: "google/gemini-x",
        env: vec![("GEMINI_API_KEY", "gem-key")],
        route: Some("google"),
        content_type: "text/event-stream",
        reply: gemini_reply(),
        path_prefix: "POST /v1beta/models/gemini-x:streamGenerateContent?alt=sse",
        header: "x-goog-api-key: gem-key",
    });
    assert_eq!(google["contents"][0]["role"], "user");
    assert!(google["systemInstruction"]["parts"][0]["text"].is_string());

    let mistral = run_case(&Case {
        name: "mistral",
        model: "mistral/mistral-x",
        env: vec![("MISTRAL_API_KEY", "mis-key")],
        route: Some("mistral"),
        content_type: "text/event-stream",
        reply: chat_reply(),
        path_prefix: "POST /v1/chat/completions",
        header: "authorization: bearer mis-key",
    });
    assert_eq!(mistral["messages"][0]["role"], "system");

    let groq = run_case(&Case {
        name: "groq",
        model: "groq/llama-x",
        env: vec![("GROQ_API_KEY", "gsk-key")],
        route: Some("groq"),
        content_type: "text/event-stream",
        reply: chat_reply(),
        path_prefix: "POST /v1/chat/completions",
        header: "authorization: bearer gsk-key",
    });
    assert_eq!(groq["stream"], true);
}

#[test]
fn azure_vertex_and_bedrock_reach_their_endpoints_with_their_own_auth() {
    let azure = run_case(&Case {
        name: "azure",
        model: "azure/gpt-x",
        env: vec![
            ("AZURE_OPENAI_API_KEY", "az-key"),
            (
                "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
                "gpt-x=prod-gpt, other=o",
            ),
            ("AZURE_OPENAI_API_VERSION", "2025-04-01-preview"),
        ],
        route: None,
        content_type: "text/event-stream",
        reply: responses_reply(),
        path_prefix: "POST /openai/v1/responses?api-version=2025-04-01-preview",
        header: "api-key: az-key",
    });
    assert_eq!(azure["model"], "prod-gpt");

    let vertex = run_case(&Case {
        name: "google-vertex",
        model: "google-vertex/gemini-x",
        env: vec![("GOOGLE_CLOUD_API_KEY", "vx-key")],
        route: Some("google-vertex"),
        content_type: "text/event-stream",
        reply: gemini_reply(),
        path_prefix: "POST /v1/publishers/google/models/gemini-x:streamGenerateContent?alt=sse",
        header: "x-goog-api-key: vx-key",
    });
    assert_eq!(vertex["contents"][0]["role"], "user");

    let bedrock = run_case(&Case {
        name: "amazon-bedrock",
        model: "amazon-bedrock/anthropic.claude-x:0",
        env: vec![
            ("AWS_REGION", "us-west-2"),
            ("AWS_BEARER_TOKEN_BEDROCK", "bk-token"),
        ],
        route: Some("amazon-bedrock"),
        content_type: "application/vnd.amazon.eventstream",
        reply: bedrock_reply(),
        path_prefix: "POST /model/anthropic.claude-x%3A0/converse-stream",
        header: "authorization: bearer bk-token",
    });
    assert_eq!(bedrock["messages"][0]["role"], "user");
    assert!(bedrock["system"][0]["text"].is_string());
}

#[test]
fn bedrock_with_iam_keys_signs_the_request_and_without_credentials_says_what_is_missing() {
    let runtime = Runtime::new().expect("runtime");
    let server = stub(
        &runtime,
        "application/vnd.amazon.eventstream",
        bedrock_reply(),
    );
    let home = Home::new("bedrock-iam");
    home.route("amazon-bedrock", &format!("http://{}", server.address));
    let env = [
        ("AWS_REGION", "eu-central-1"),
        ("AWS_ACCESS_KEY_ID", "AKIDEXAMPLE"),
        ("AWS_SECRET_ACCESS_KEY", "secret"),
        ("AWS_SESSION_TOKEN", "tok"),
    ];
    let outcome = home.go(
        &["-p", "--no-session", "--model", "amazon-bedrock/m", "hi"],
        &env,
        &[],
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let request = server.requests.lock().expect("lock")[0].to_ascii_lowercase();
    assert!(
        request.contains("authorization: aws4-hmac-sha256 credential=akidexample/"),
        "{request}"
    );
    assert!(
        request.contains("/eu-central-1/bedrock/aws4_request"),
        "{request}"
    );
    assert!(request.contains("x-amz-security-token: tok"), "{request}");

    let no_region = home.go(
        &["-p", "--no-session", "--model", "amazon-bedrock/m", "hi"],
        &[],
        &[],
    );
    assert_eq!(
        no_region.stderr,
        "Error: no AWS region for amazon-bedrock: set AWS_REGION (or AWS_DEFAULT_REGION)\n"
    );
    let no_creds = home.go(
        &["-p", "--no-session", "--model", "amazon-bedrock/m", "hi"],
        &[("AWS_REGION", "us-east-1")],
        &[],
    );
    assert!(
        no_creds.stderr.contains(
            "set AWS_BEARER_TOKEN_BEDROCK, or AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
        ),
        "{}",
        no_creds.stderr
    );
    home.cleanup();
}

#[test]
fn providers_that_cannot_be_called_correctly_are_refused_with_the_reason() {
    let home = Home::new("refusals");
    let ask = |model: &str, env: &[(&str, &str)]| {
        let outcome = home.go(&["-p", "--no-session", "--model", model, "hi"], env, &[]);
        assert_eq!(outcome.code, 1, "{model}");
        assert_eq!(outcome.stdout, "");
        outcome.stderr
    };
    let mixed = ask(
        "openrouter/anthropic/claude-x",
        &[("OPENROUTER_API_KEY", "k")],
    );
    assert_eq!(
        mixed,
        "Error: provider \"openrouter\" is not supported yet: its model list comes from a live listing this build does not read yet\n"
    );
    let routed = ask("opencode/m", &[("OPENCODE_API_KEY", "k")]);
    assert!(
        routed.contains("requires a per-conversation routing header"),
        "{routed}"
    );
    let copilot = ask("github-copilot/m", &[]);
    assert!(copilot.contains("needs a GitHub OAuth login"), "{copilot}");
    let codex = ask("openai-codex/gpt-x", &[]);
    assert!(
        codex
            .contains("uses the openai-codex-responses API, which this build does not support yet"),
        "{codex}"
    );
    let unknown = ask("nowhere/m", &[]);
    assert!(
        unknown
            .contains("provider \"nowhere\" is not supported by the Rust preview yet (supported: "),
        "{unknown}"
    );
    assert!(
        unknown.contains("anthropic")
            && unknown.contains("cliproxyapi")
            && unknown.contains("amazon-bedrock"),
        "{unknown}"
    );
    let oauth = ask(
        "anthropic/claude-x",
        &[("ANTHROPIC_API_KEY", "sk-ant-oat01-token")],
    );
    assert!(
        oauth.contains("is an OAuth token, which this build does not support yet"),
        "{oauth}"
    );
    let no_key = ask("mistral/m", &[]);
    assert_eq!(
        no_key,
        "Error: no API key for provider \"mistral\": set MISTRAL_API_KEY, pass --api-key, or run `nyan login mistral`\n"
    );
    let vertex = ask("google-vertex/m", &[]);
    assert!(
        vertex.contains("Application Default Credentials are not supported yet"),
        "{vertex}"
    );
    let azure = ask("azure/m", &[("AZURE_OPENAI_API_KEY", "k")]);
    assert!(
        azure.contains("set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME"),
        "{azure}"
    );
    home.cleanup();
}

#[test]
fn the_key_comes_from_the_flag_then_the_environment_then_the_stored_login() {
    let runtime = Runtime::new().expect("runtime");
    let server = stub(&runtime, "text/event-stream", chat_reply());
    let home = Home::new("key-order");
    home.route("groq", &format!("http://{}/v1", server.address));
    nyan_coding_agent_auth::AuthStore::new(&home.agent)
        .save_api_key("groq", "stored-key", &std::collections::BTreeMap::new())
        .expect("store");
    let run = |argv: &[&str], env: &[(&str, &str)]| {
        let outcome = home.go(argv, env, &[]);
        assert_eq!(outcome.code, 0, "{}", outcome.stderr);
        let requests = server.requests.lock().expect("lock");
        requests.last().expect("a request").to_ascii_lowercase()
    };
    let stored = run(&["-p", "--no-session", "--model", "groq/m", "x"], &[]);
    assert!(stored.contains("bearer stored-key"), "{stored}");
    let from_env = run(
        &["-p", "--no-session", "--model", "groq/m", "x"],
        &[("GROQ_API_KEY", "env-key")],
    );
    assert!(from_env.contains("bearer env-key"), "{from_env}");
    let from_flag = run(
        &[
            "-p",
            "--no-session",
            "--model",
            "groq/m",
            "--api-key",
            "flag-key",
            "x",
        ],
        &[("GROQ_API_KEY", "env-key")],
    );
    assert!(from_flag.contains("bearer flag-key"), "{from_flag}");
    home.cleanup();
}

#[test]
fn azure_defaults_to_api_version_v1() {
    let runtime = Runtime::new().expect("runtime");
    let server = stub(&runtime, "text/event-stream", responses_reply());
    let home = Home::new("azure-default");
    let base = format!("http://{}/openai/v1", server.address);
    let outcome = home.go(
        &["-p", "--no-session", "--model", "azure/gpt-x", "hi"],
        &[
            ("AZURE_OPENAI_API_KEY", "k"),
            ("AZURE_OPENAI_BASE_URL", &base),
        ],
        &[],
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let request = server.requests.lock().expect("lock")[0].clone();
    assert!(
        request.starts_with("POST /openai/v1/responses?api-version=v1 HTTP/1.1"),
        "{request}"
    );

    home.cleanup();
}

#[test]
fn login_stores_an_api_key_for_any_key_provider_and_the_next_run_uses_it() {
    let runtime = Runtime::new().expect("runtime");
    let server = stub(&runtime, "text/event-stream", chat_reply());
    let home = Home::new("generic-login");
    home.route("groq", &format!("http://{}/v1", server.address));

    let outcome = home.go(&["login", "groq"], &[], &["gsk-typed", "llama-x"]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert!(
        outcome
            .stdout
            .contains("It has not been checked against the provider."),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("Default model: groq/llama-x"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        outcome.asked,
        [
            "Groq API key: ",
            "Default model id for groq (blank to skip): "
        ]
    );
    assert_eq!(
        home.json("auth.json"),
        serde_json::json!({"groq": {"type": "api_key", "key": "gsk-typed", "env": {}}})
    );
    assert_eq!(home.json("settings.json")["defaultModel"], "llama-x");

    // No flag, no environment: the stored key and the saved default are all it has.
    let run = home.go(&["-p", "--no-session", "say hi"], &[], &[]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "Hi\n");
    let request = server.requests.lock().expect("lock")[0].to_ascii_lowercase();
    assert!(
        request.contains("authorization: bearer gsk-typed"),
        "{request}"
    );
    assert!(request.contains("\"model\":\"llama-x\""), "{request}");

    let empty = home.go(&["login", "mistral"], &[], &["   "]);
    assert_eq!(empty.code, 1);
    assert_eq!(empty.stderr, "Error: the API key cannot be empty\n");
    assert!(home.json("auth.json").get("mistral").is_none());
    let menu = home.go(&["login"], &[], &["groq", "gsk-2", ""]);
    assert_eq!(menu.code, 0, "{}", menu.stderr);
    assert!(
        menu.asked[0].starts_with("Choose a provider:\n"),
        "{:?}",
        menu.asked
    );
    home.cleanup();
}
