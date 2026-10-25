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
    serve_with_headers(runtime, status, "", chunks, hold_open)
}

/// Like [`serve`], with extra response header lines (each ending in `\r\n`).
pub fn serve_with_headers(
    runtime: &Runtime,
    status: &'static str,
    extra_headers: &'static str,
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
            "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\n{extra_headers}connection: close\r\n\r\n"
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
        session_id: None,
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

/// Builds one AWS event-stream message with string headers.
pub fn eventstream_message(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    use nyan_coding_agent_provider_http::eventstream::crc32;
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(u8::try_from(name.len()).expect("short name"));
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(
            &u16::try_from(value.len())
                .expect("short value")
                .to_be_bytes(),
        );
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let total = 12 + header_bytes.len() + payload.len() + 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(total).expect("size").to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(header_bytes.len())
            .expect("size")
            .to_be_bytes(),
    );
    let prelude_crc = crc32(&bytes);
    bytes.extend_from_slice(&prelude_crc.to_be_bytes());
    bytes.extend_from_slice(&header_bytes);
    bytes.extend_from_slice(payload);
    let message_crc = crc32(&bytes);
    bytes.extend_from_slice(&message_crc.to_be_bytes());
    bytes
}

/// One Converse event frame.
pub fn converse_event(kind: &str, payload: &serde_json::Value) -> Vec<u8> {
    eventstream_message(
        &[(":message-type", "event"), (":event-type", kind)],
        payload.to_string().as_bytes(),
    )
}

/// A server that answers the n-th request with the n-th `(status, body)` and repeats the
/// last one after that. Returns its address and every request it received.
pub fn serve_sequence(
    runtime: &Runtime,
    responses: Vec<(&'static str, String)>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = runtime
        .block_on(TcpListener::bind("127.0.0.1:0"))
        .expect("bind loopback");
    let address = listener.local_addr().expect("address");
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    runtime.spawn(async move {
        let mut index = 0_usize;
        loop {
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
            sink.lock()
                .expect("lock")
                .push(String::from_utf8_lossy(&received).into_owned());
            let (status, body) = responses
                .get(index)
                .or_else(|| responses.last())
                .cloned()
                .unwrap_or(("500 Internal Server Error", String::new()));
            index += 1;
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    (address, log)
}
