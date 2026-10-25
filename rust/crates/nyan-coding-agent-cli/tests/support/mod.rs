//! Shared CLI test harness: captured writers, a scripted prompt, and a scratch home.
#![allow(dead_code)] // each test crate uses a different subset

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use nyan_coding_agent_cli::headless::SharedWriter;
use nyan_coding_agent_cli::run::{Environment, run};
use serde_json::Value;

pub struct Capture(pub Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub asked: Vec<String>,
}

pub struct Home {
    pub agent: PathBuf,
    pub cwd: PathBuf,
    /// Provider id to loopback base URL.
    pub base_urls: Mutex<BTreeMap<String, String>>,
    /// Where the model data is fetched from.
    pub models_dev_url: Mutex<String>,
}

impl Home {
    pub fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("nyan-cpa-{label}-{}-{nonce}", std::process::id()));
        let agent = root.join("agent");
        let cwd = root.join("project");
        std::fs::create_dir_all(&agent).expect("agent");
        std::fs::create_dir_all(&cwd).expect("cwd");
        Self {
            agent,
            cwd,
            base_urls: Mutex::new(BTreeMap::new()),
            models_dev_url: Mutex::new("http://127.0.0.1:1/api.json".to_owned()),
        }
    }

    /// Runs the CLI with `answers` queued for its prompts.
    pub fn go(&self, argv: &[&str], env: &[(&str, &str)], answers: &[&str]) -> Outcome {
        let out_bytes = Arc::new(Mutex::new(Vec::new()));
        let err_bytes = Arc::new(Mutex::new(Vec::new()));
        let out: SharedWriter = Arc::new(Mutex::new(Capture(Arc::clone(&out_bytes))));
        let err: SharedWriter = Arc::new(Mutex::new(Capture(Arc::clone(&err_bytes))));
        let queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(
            answers.iter().map(|a| (*a).to_owned()).collect(),
        ));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut variables: BTreeMap<String, String> = env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        variables.insert(
            "NYAN_CODING_AGENT_DIR".to_owned(),
            self.agent.to_string_lossy().into_owned(),
        );
        let (queue_for_prompt, asked_for_prompt) = (Arc::clone(&queue), Arc::clone(&asked));
        let environment = Environment {
            var: Box::new(move |name| variables.get(name).cloned()),
            stdin_is_tty: false,
            stdout_is_tty: false,
            cwd: self.cwd.clone(),
            base_urls: self.base_urls.lock().expect("lock").clone(),
            models_dev_url: self.models_dev_url.lock().expect("lock").clone(),
            read_stdin: Box::new(|| None),
            prompt: Box::new(move |message, _secret| {
                asked_for_prompt
                    .lock()
                    .expect("lock")
                    .push(message.to_owned());
                queue_for_prompt.lock().expect("lock").pop_front()
            }),
        };
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let code = run(&argv, &environment, &out, &err);
        let text = |bytes: &Arc<Mutex<Vec<u8>>>| {
            String::from_utf8(bytes.lock().expect("lock").clone()).expect("utf8")
        };
        let asked = asked.lock().expect("lock").clone();
        Outcome {
            code,
            stdout: text(&out_bytes),
            stderr: text(&err_bytes),
            asked,
        }
    }

    pub fn json(&self, name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.agent.join(name)).expect("read"))
            .expect("json")
    }

    pub fn cleanup(self) {
        if let Some(root) = self.agent.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

impl Home {
    /// Points later runs at a loopback models.dev stand-in.
    pub fn serve_models_dev(&self, url: &str) {
        url.clone_into(&mut self.models_dev_url.lock().expect("lock"));
    }

    /// Routes `provider` to a loopback `base_url` for later runs.
    pub fn route(&self, provider: &str, base_url: &str) {
        self.base_urls
            .lock()
            .expect("lock")
            .insert(provider.to_owned(), base_url.to_owned());
    }
}

/// A loopback server that answers every request with one fixed body.
pub struct Stub {
    pub address: std::net::SocketAddr,
    pub requests: Arc<Mutex<Vec<String>>>,
}

pub fn stub(runtime: &tokio::runtime::Runtime, content_type: &'static str, body: Vec<u8>) -> Stub {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&requests);
    runtime.spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (log, body) = (Arc::clone(&log), body.clone());
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
                log.lock()
                    .expect("lock")
                    .push(String::from_utf8_lossy(&received).into_owned());
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.flush().await;
            });
        }
    });
    Stub { address, requests }
}
