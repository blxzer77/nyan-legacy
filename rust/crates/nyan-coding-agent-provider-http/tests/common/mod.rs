//! Loopback server and request helpers shared by the provider tests.
#![allow(dead_code)] // each test crate uses a different subset

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nyan_coding_agent_core::{CancellationToken, Message, MessageRole};
use nyan_coding_agent_provider::{
    ProviderError, ProviderModel, ProviderRequest, ProviderStream, ProviderStreamEvent, ToolSpec,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

pub struct Server {
    pub address: SocketAddr,
    pub captured: Arc<Mutex<String>>,
}

/// Serves one response: the status line and headers, then each `(delay, bytes)`
/// chunk, then either closes or (when `hold_open`) stays silent.
pub fn serve(
    runtime: &Runtime,
    status: &'static str,
    chunks: Vec<(u64, Vec<u8>)>,
    hold_open: bool,
) -> Server {
    let listener = runtime
        .block_on(TcpListener::bind("127.0.0.1:0"))
        .expect("bind loopback");
    let address = listener.local_addr().expect("local address");
    let captured = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&captured);
    runtime.spawn(async move {
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
            if let Some(header_end) = text.find("\r\n\r\n") {
                let length = text
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if received.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        *sink.lock().expect("captured lock") = String::from_utf8_lossy(&received).into_owned();
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
        );
        let _ = socket.write_all(head.as_bytes()).await;
        for (delay, bytes) in chunks {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let _ = socket.write_all(&bytes).await;
            let _ = socket.flush().await;
        }
        if hold_open {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });
    Server { address, captured }
}

pub fn request(cancellation: CancellationToken) -> ProviderRequest {
    ProviderRequest {
        model: ProviderModel {
            provider: "loopback".to_owned(),
            model: "test-model".to_owned(),
        },
        messages: vec![Message::text(MessageRole::User, "hello")],
        tools: vec![ToolSpec {
            name: "echo".to_owned(),
            description: "Echo input".to_owned(),
            parameters_schema:
                r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#
                    .to_owned(),
        }],
        cancellation,
    }
}

pub fn sse(data: &str) -> Vec<u8> {
    format!("data: {data}\n\n").into_bytes()
}

pub fn drain(
    runtime: &Runtime,
    mut stream: Box<dyn ProviderStream>,
) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
    let mut events = Vec::new();
    loop {
        match runtime.block_on(stream.next())? {
            Some(event) => events.push(event),
            None => return Ok(events),
        }
    }
}
