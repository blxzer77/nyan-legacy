//! Drives the same `run` function the executable calls, against a loopback
//! server standing in for the `OpenAI` endpoint. Nothing here uses a real provider
//! or a real credential.

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_cli::headless::SharedWriter;
use nyan_coding_agent_cli::run::{Environment, run};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("capture lock")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

fn execute(argv: &[&str], base_url: &str, key: Option<&str>, tty: bool) -> Outcome {
    execute_in(argv, base_url, key, tty, PathBuf::from("/tmp/project"))
}

fn execute_in(
    argv: &[&str],
    base_url: &str,
    key: Option<&str>,
    tty: bool,
    cwd: PathBuf,
) -> Outcome {
    let out_bytes = Arc::new(Mutex::new(Vec::new()));
    let err_bytes = Arc::new(Mutex::new(Vec::new()));
    let out: SharedWriter = Arc::new(Mutex::new(Capture(Arc::clone(&out_bytes))));
    let err: SharedWriter = Arc::new(Mutex::new(Capture(Arc::clone(&err_bytes))));
    let key = key.map(str::to_owned);
    let environment = Environment {
        var: Box::new(move |name| (name == "OPENAI_API_KEY").then(|| key.clone()).flatten()),
        stdin_is_tty: tty,
        stdout_is_tty: tty,
        cwd,
        openai_base_url: base_url.to_owned(),
    };
    let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
    let code = run(&argv, &environment, &out, &err);
    let text = |bytes: &Arc<Mutex<Vec<u8>>>| {
        String::from_utf8(bytes.lock().expect("capture lock").clone()).expect("utf8")
    };
    Outcome {
        code,
        stdout: text(&out_bytes),
        stderr: text(&err_bytes),
    }
}

/// Starts a server that answers one request and returns its address and the
/// request it received.
fn serve(
    runtime: &Runtime,
    status: &'static str,
    body: String,
) -> (SocketAddr, Arc<Mutex<String>>) {
    let (address, mut captured) = serve_many(runtime, vec![(status, body)]);
    (address, captured.remove(0))
}

/// Answers one request per scripted response, in order, and returns each
/// request it received.
fn serve_many(
    runtime: &Runtime,
    responses: Vec<(&'static str, String)>,
) -> (SocketAddr, Vec<Arc<Mutex<String>>>) {
    let listener = runtime
        .block_on(TcpListener::bind("127.0.0.1:0"))
        .expect("bind loopback");
    let address = listener.local_addr().expect("address");
    let captured: Vec<Arc<Mutex<String>>> = responses
        .iter()
        .map(|_| Arc::new(Mutex::new(String::new())))
        .collect();
    let sinks = captured.clone();
    runtime.spawn(async move {
        for ((status, body), sink) in responses.into_iter().zip(sinks) {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut received = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
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
            *sink.lock().expect("captured lock") = String::from_utf8_lossy(&received).into_owned();
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    (address, captured)
}

fn answer_body() -> String {
    let mut body = String::new();
    for data in [
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#,
        "[DONE]",
    ] {
        body.push_str("data: ");
        body.push_str(data);
        body.push_str("\n\n");
    }
    body
}

#[test]
fn print_mode_prints_the_answer_and_sends_the_credential_and_model() {
    let runtime = Runtime::new().expect("runtime");
    let (address, captured) = serve(&runtime, "200 OK", answer_body());
    let outcome = execute(
        &["-p", "--model", "openai/gpt-test", "say hi"],
        &format!("http://{address}/v1"),
        Some("env-key"),
        false,
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "Hello\n");
    assert_eq!(outcome.stderr, "");
    let request = captured.lock().expect("captured lock").clone();
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer env-key")
    );
    let body: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["messages"][0]["content"], "say hi");
}

#[test]
fn an_explicit_api_key_flag_overrides_the_environment() {
    let runtime = Runtime::new().expect("runtime");
    let (address, captured) = serve(&runtime, "200 OK", answer_body());
    let outcome = execute(
        &[
            "-p",
            "--provider",
            "openai",
            "--model",
            "m",
            "--api-key",
            "flag-key",
            "hi",
        ],
        &format!("http://{address}/v1"),
        Some("env-key"),
        false,
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert!(
        captured
            .lock()
            .expect("captured lock")
            .to_ascii_lowercase()
            .contains("authorization: bearer flag-key")
    );
}

#[test]
fn json_mode_streams_the_session_header_and_documented_events() {
    let runtime = Runtime::new().expect("runtime");
    let (address, _) = serve(&runtime, "200 OK", answer_body());
    let outcome = execute(
        &["--mode", "json", "--model", "openai/m", "hi"],
        &format!("http://{address}/v1"),
        Some("k"),
        true,
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stderr, "");
    let records: Vec<serde_json::Value> = outcome
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is JSON"))
        .collect();
    assert_eq!(records[0]["type"], "session");
    assert_eq!(records[0]["cwd"], "/tmp/project");
    assert_eq!(
        records.last().expect("last record")["type"],
        "agent_settled"
    );
    let end = records
        .iter()
        .find(|record| record["type"] == "agent_end")
        .expect("agent_end");
    let assistant = &end["messages"][1];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"][0]["text"], "Hello");
    assert_eq!(assistant["provider"], "openai");
    assert_eq!(assistant["model"], "m");
    assert_eq!(assistant["stopReason"], "stop");
    assert_eq!(assistant["usage"]["input"], 5);
    assert_eq!(assistant["usage"]["totalTokens"], 7);
}

#[test]
fn a_rejected_credential_exits_one_with_the_http_reason_on_stderr() {
    let runtime = Runtime::new().expect("runtime");
    let (address, _) = serve(
        &runtime,
        "401 Unauthorized",
        r#"{"error":{"message":"bad key"}}"#.to_owned(),
    );
    let outcome = execute(
        &["-p", "--model", "openai/m", "hi"],
        &format!("http://{address}/v1"),
        Some("wrong"),
        false,
    );
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.stdout, "");
    assert!(
        outcome.stderr.contains("HTTP 401: bad key"),
        "{}",
        outcome.stderr
    );
}

#[test]
fn configuration_errors_are_reported_before_any_network_use() {
    let unreachable = "http://127.0.0.1:1/v1";
    let cases: [(&[&str], Option<&str>, i32, &str); 6] = [
        (
            &["-p", "--model", "openai/m", "hi"],
            None,
            1,
            "no API key for provider \"openai\"",
        ),
        (&["-p", "hi"], Some("k"), 1, "--model is required"),
        (
            &["-p", "--model", "gpt", "hi"],
            Some("k"),
            1,
            "--model needs a provider",
        ),
        (
            &["-p", "--model", "other/m", "hi"],
            Some("k"),
            1,
            "provider \"other\" is not supported",
        ),
        (
            &["-p", "--model", "openai/m", "--plan", "hi"],
            Some("k"),
            1,
            "unknown option --plan",
        ),
        (
            &["--mode", "rpc"],
            Some("k"),
            2,
            "not available in the Rust preview",
        ),
    ];
    for (argv, key, code, message) in cases {
        let outcome = execute(argv, unreachable, key, false);
        assert_eq!(outcome.code, code, "{argv:?}: {}", outcome.stderr);
        assert!(
            outcome.stderr.contains(message),
            "{argv:?}: {}",
            outcome.stderr
        );
        assert_eq!(outcome.stdout, "", "{argv:?}");
    }
}

#[test]
fn version_help_and_parse_errors_do_not_need_a_provider() {
    let version = execute(&["--version"], "", None, false);
    assert_eq!((version.code, version.stdout.as_str()), (0, "0.1.0\n"));
    let help = execute(&["--help"], "", None, false);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("33 RPC commands, 23 session events"));
    let bare = execute(&[], "", None, true);
    assert_eq!(bare.code, 0);
    assert!(bare.stdout.contains("Rust Coding Agent preview"));
    let piped = execute(&[], "", None, false);
    assert_eq!(piped.code, 1);
    assert!(
        piped.stderr.contains("--model is required"),
        "{}",
        piped.stderr
    );
    assert_eq!(piped.stdout, "");
    let bad = execute(&["--mode", "bogus"], "", None, false);
    assert_eq!(bad.code, 1);
    assert_eq!(
        bad.stderr,
        "Error: Invalid mode \"bogus\". Valid values: text, json, rpc\n"
    );
    let warned = execute(&["--thinking", "ultra", "--version"], "", None, false);
    assert_eq!(warned.code, 0);
    assert!(
        warned
            .stderr
            .starts_with("Warning: Invalid thinking level \"ultra\"")
    );
}

fn sse_body(chunks: &[&str]) -> String {
    let mut body = String::new();
    for data in chunks {
        body.push_str("data: ");
        body.push_str(data);
        body.push_str("\n\n");
    }
    body
}

#[test]
fn tool_flags_decide_which_tools_the_model_is_offered() {
    let cases: [(&[&str], &[&str]); 4] = [
        (
            &["-p", "--model", "openai/m", "hi"],
            &["read", "bash", "edit", "write"],
        ),
        (
            &["-p", "--model", "openai/m", "--tools", "ls,grep,find", "hi"],
            &["grep", "find", "ls"],
        ),
        (
            &["-p", "--model", "openai/m", "--tools", "+ls,-bash", "hi"],
            &["read", "edit", "write", "ls"],
        ),
        (
            &[
                "-p",
                "--model",
                "openai/m",
                "--exclude-tools",
                "bash,write",
                "hi",
            ],
            &["read", "edit"],
        ),
    ];
    for (argv, expected) in cases {
        let runtime = Runtime::new().expect("runtime");
        let (address, captured) = serve(&runtime, "200 OK", answer_body());
        let outcome = execute(argv, &format!("http://{address}/v1"), Some("k"), false);
        assert_eq!(outcome.code, 0, "{argv:?}: {}", outcome.stderr);
        let request = captured.lock().expect("lock").clone();
        let body: serde_json::Value =
            serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("body")).expect("json");
        let offered: Vec<&str> = body["tools"]
            .as_array()
            .map(|tools| {
                tools
                    .iter()
                    .map(|tool| tool["function"]["name"].as_str().expect("name"))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(offered, expected, "{argv:?}");
    }
    let runtime = Runtime::new().expect("runtime");
    let (address, captured) = serve(&runtime, "200 OK", answer_body());
    let outcome = execute(
        &["-p", "--model", "openai/m", "--no-tools", "hi"],
        &format!("http://{address}/v1"),
        Some("k"),
        false,
    );
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let request = captured.lock().expect("lock").clone();
    assert!(
        !request.contains("\"tools\""),
        "no tools field when none are active"
    );
}

#[test]
fn an_unknown_tool_name_is_rejected_before_any_network_use() {
    let outcome = execute(
        &["-p", "--model", "openai/m", "--tools", "teleport", "hi"],
        "http://127.0.0.1:1/v1",
        Some("k"),
        false,
    );
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.stderr.contains("unknown tool \"teleport\""),
        "{}",
        outcome.stderr
    );
    assert_eq!(outcome.stdout, "");
}

#[test]
fn a_tool_call_changes_the_workspace_and_its_result_reaches_the_next_request() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let workspace = std::env::temp_dir().join(format!("nyan-e2e-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&workspace).expect("workspace");

    let runtime = Runtime::new().expect("runtime");
    let tool_turn = sse_body(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_w","function":{"name":"write","arguments":"{\"path\":\"out/hello.txt\",\"content\":\"hi there\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);
    let (address, requests) = serve_many(
        &runtime,
        vec![("200 OK", tool_turn), ("200 OK", answer_body())],
    );
    let outcome = execute_in(
        &["--mode", "json", "--model", "openai/m", "create the file"],
        &format!("http://{address}/v1"),
        Some("k"),
        true,
        workspace.clone(),
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(
        std::fs::read_to_string(workspace.join("out/hello.txt")).expect("tool wrote the file"),
        "hi there"
    );

    let first = requests[0].lock().expect("lock").clone();
    let first_body: serde_json::Value =
        serde_json::from_str(first.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    let declared: Vec<&str> = first_body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("name"))
        .collect();
    assert_eq!(declared, ["read", "bash", "edit", "write"]);
    assert_eq!(
        first_body["tools"][3]["function"]["parameters"]["required"][1],
        "content"
    );

    let second = requests[1].lock().expect("lock").clone();
    let second_body: serde_json::Value =
        serde_json::from_str(second.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    let messages = second_body["messages"].as_array().expect("messages");
    let assistant = &messages[1];
    assert_eq!(assistant["tool_calls"][0]["id"], "call_w");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "write");
    let result = &messages[2];
    assert_eq!(result["role"], "tool");
    assert_eq!(result["tool_call_id"], "call_w");
    assert_eq!(result["content"], "Successfully wrote to out/hello.txt");

    let types: Vec<String> = outcome
        .stdout
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("jsonl");
            value["type"].as_str().expect("type").to_owned()
        })
        .collect();
    assert!(types.iter().any(|kind| kind == "tool_execution_start"));
    assert!(types.iter().any(|kind| kind == "tool_execution_end"));
    let _ = std::fs::remove_dir_all(workspace);
}

#[cfg(unix)]
#[test]
fn a_bash_tool_call_runs_in_the_workspace_and_a_failure_is_reported_back_to_the_model() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let workspace =
        std::env::temp_dir().join(format!("nyan-e2e-bash-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&workspace).expect("workspace");

    let runtime = Runtime::new().expect("runtime");
    let tool_turn = sse_body(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_b","function":{"name":"bash","arguments":"{\"command\":\"echo made > made.txt; cat missing.txt\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "[DONE]",
    ]);
    let (address, requests) = serve_many(
        &runtime,
        vec![("200 OK", tool_turn), ("200 OK", answer_body())],
    );
    let outcome = execute_in(
        &["-p", "--model", "openai/m", "run it"],
        &format!("http://{address}/v1"),
        Some("k"),
        false,
        workspace.clone(),
    );
    assert_eq!(outcome.code, 0, "stderr: {}", outcome.stderr);
    assert_eq!(outcome.stdout, "Hello\n");
    assert_eq!(
        std::fs::read_to_string(workspace.join("made.txt")).expect("command wrote the file"),
        "made\n"
    );
    let second = requests[1].lock().expect("lock").clone();
    let body: serde_json::Value =
        serde_json::from_str(second.split("\r\n\r\n").nth(1).expect("body")).expect("json");
    let result = body["messages"][2]["content"]
        .as_str()
        .expect("tool result text");
    assert!(result.contains("missing.txt"), "{result}");
    assert!(result.ends_with("Command exited with code 1"), "{result}");
    let _ = std::fs::remove_dir_all(workspace);
}
