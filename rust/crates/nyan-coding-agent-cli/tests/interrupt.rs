//! A signal ends a run cleanly: the model request stops, a running tool loses its
//! process group, the run is recorded as aborted, and the exit code names the
//! signal. The signal is a real `SIGINT`/`SIGTERM` sent to this test process, so
//! the scenarios run one after another in one test: a signal is process-wide.
//!
//! POSIX only: the signals are sent with `kill`. Windows console events cannot be
//! raised from a test the same way.
#![cfg(unix)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

mod support;
use support::{Home, Outcome};

/// Answers every request with `body` after `delay`, or never when `body` is `None`.
struct Server {
    address: SocketAddr,
    requests: Arc<Mutex<usize>>,
}

fn serve(runtime: &Runtime, bodies: Vec<Option<String>>) -> Server {
    let listener = runtime
        .block_on(TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let requests = Arc::new(Mutex::new(0_usize));
    let count = Arc::clone(&requests);
    runtime.spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let index = {
                let mut count = count.lock().expect("lock");
                *count += 1;
                *count - 1
            };
            let body = bodies.get(index).cloned().flatten();
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
                let Some(body) = body else {
                    // Hold the connection open: a model that never answers.
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    return;
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    Server { address, requests }
}

fn tool_call_reply(command: &str) -> String {
    let arguments = json!({ "command": command }).to_string();
    let chunk = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1",
        "function":{"name":"bash","arguments":arguments}}]},"finish_reason":"tool_calls"}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn send(signal: &str) {
    let status = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(std::process::id().to_string())
        .status()
        .expect("kill runs");
    assert!(status.success(), "kill -{signal} failed");
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs the CLI on a thread, calls `during` once it is underway, and returns the outcome.
fn run_and_interrupt(
    home: &Arc<Home>,
    argv: &'static [&'static str],
    during: impl FnOnce(),
) -> (Outcome, Duration) {
    let worker = {
        let home = Arc::clone(home);
        std::thread::spawn(move || home.go(argv, &[("GROQ_API_KEY", "k")], &[]))
    };
    during();
    let signalled = Instant::now();
    let outcome = worker.join().expect("run thread");
    (outcome, signalled.elapsed())
}

fn events(outcome: &Outcome) -> Vec<Value> {
    outcome
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is JSON"))
        .collect()
}

fn settled_aborted(events: &[Value]) -> Option<bool> {
    events
        .iter()
        .find(|event| event["type"] == "agent_settled")
        .and_then(|event| event["aborted"].as_bool())
}

#[test]
fn signals_stop_a_hung_request_and_a_running_tool_and_name_themselves_in_the_exit_code() {
    let runtime = Runtime::new().expect("runtime");

    // A model that never answers, then SIGINT.
    let hung = serve(&runtime, vec![None]);
    let home = Arc::new(Home::new("interrupt-request"));
    home.route("groq", &format!("http://{}/v1", hung.address));
    let argv: &'static [&'static str] = &[
        "--mode",
        "json",
        "--session-id",
        "interrupted",
        "--model",
        "groq/anything",
        "hi",
    ];
    let (outcome, took) = run_and_interrupt(&home, argv, || {
        wait_for("the request to arrive", || {
            *hung.requests.lock().expect("lock") > 0
        });
        send("INT");
    });
    assert_eq!(outcome.code, 130, "{}", outcome.stderr);
    assert!(took < Duration::from_secs(5), "took {took:?}");
    let records = events(&outcome);
    assert_eq!(settled_aborted(&records), Some(true), "{}", outcome.stdout);
    // The aborted run is a complete record: the session continues instead of being refused.
    let again = serve(
        &runtime,
        vec![Some(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                .to_owned(),
        )],
    );
    home.route("groq", &format!("http://{}/v1", again.address));
    let resumed = home.go(
        &[
            "-p",
            "--session-id",
            "interrupted",
            "--model",
            "groq/anything",
            "again",
        ],
        &[("GROQ_API_KEY", "k")],
        &[],
    );
    assert_eq!(resumed.code, 0, "{}", resumed.stderr);
    assert_eq!(resumed.stdout, "Hi\n");

    // A tool that is running when SIGTERM arrives.
    let home = Arc::new(Home::new("interrupt-tool"));
    let pid_file = home.cwd.join("tool.pid");
    let command = format!("echo $$ > '{}'; sleep 60", pid_file.display());
    let tool_server = serve(&runtime, vec![Some(tool_call_reply(&command))]);
    home.route("groq", &format!("http://{}/v1", tool_server.address));
    let argv: &'static [&'static str] = &[
        "--mode",
        "json",
        "--no-session",
        "--model",
        "groq/anything",
        "go",
    ];
    let seen_pid = Arc::new(Mutex::new(None::<i32>));
    let recorded = Arc::clone(&seen_pid);
    let watched: &Path = &pid_file;
    let (outcome, took) = run_and_interrupt(&home, argv, || {
        wait_for("the tool to start", || {
            std::fs::read_to_string(watched)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .inspect(|pid| *recorded.lock().expect("lock") = Some(*pid))
                .is_some()
        });
        send("TERM");
    });
    assert_eq!(outcome.code, 143, "{}", outcome.stderr);
    assert!(took < Duration::from_secs(10), "took {took:?}");
    let records = events(&outcome);
    assert_eq!(settled_aborted(&records), Some(true), "{}", outcome.stdout);
    let pid = seen_pid
        .lock()
        .expect("lock")
        .expect("the tool wrote its pid");
    wait_for("the tool process to be gone", || !alive(pid));
}
