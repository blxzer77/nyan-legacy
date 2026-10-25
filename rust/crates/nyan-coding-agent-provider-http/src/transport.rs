//! Request admission and streaming shared by every wire protocol.
//!
//! A protocol supplies a [`StreamProtocol`]: how one SSE event changes its
//! state and how its terminal state becomes provider events. This module owns
//! everything else: connecting, HTTP status mapping, cancellation, the idle
//! timeout, and delivery through a channel so the future returned by `stream`
//! can be awaited on any executor.

use std::sync::Arc;
use std::time::Duration;

use nyan_coding_agent_core::{BoxFuture, CancellationToken, Usage};
use nyan_coding_agent_provider::{
    ProviderError, ProviderStopReason, ProviderStream, ProviderStreamEvent, ToolCallRequest,
};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::{Mutex, mpsc};

use crate::sse::{SseDecoder, SseEvent};

const CANCELLATION_POLL: Duration = Duration::from_millis(50);
const MAX_ERROR_BODY_BYTES: usize = 4096;

type EventResult = Result<ProviderStreamEvent, ProviderError>;

/// What one SSE event did to a protocol's state.
pub(crate) enum Applied {
    /// Nothing for the caller to forward.
    Nothing,
    /// A chunk of assistant text.
    Text(String),
    /// The stream is complete; stop reading.
    Done,
}

/// The terminal state of a response, ready to be forwarded.
pub(crate) struct Finished {
    pub usage: Option<Usage>,
    pub calls: Vec<ToolCallRequest>,
    pub reason: ProviderStopReason,
}

/// One wire protocol's incremental stream state.
pub(crate) trait StreamProtocol: Send + 'static {
    /// Applies one decoded SSE event.
    fn apply(&mut self, event: &SseEvent) -> Result<Applied, ProviderError>;

    /// Whether a terminal event was seen, even if the stream then closed without
    /// the protocol's end marker.
    fn is_terminal(&self) -> bool;

    /// Consumes the state once the stream is complete.
    fn finish(self) -> Finished;
}

/// One POST request.
pub(crate) struct Post {
    pub url: String,
    pub body: String,
    pub bearer: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// The HTTP client, runtime handle, and idle timeout one provider uses.
#[derive(Clone, Debug)]
pub(crate) struct Transport {
    client: reqwest::Client,
    handle: Handle,
    idle_timeout: Duration,
    /// A header that carries the conversation id, when the provider wants one.
    session_header: Option<String>,
}

impl Transport {
    pub(crate) fn new(handle: Handle) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("nyan-coding-agent/", env!("CARGO_PKG_VERSION")))
            // A compressed event stream can be buffered by the server or the client
            // until a block fills, which delays text; model requests ask for none.
            .gzip(false)
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| ProviderError::Request {
                message: format!("failed to build the HTTP client: {error}"),
                retryable: false,
            })?;
        Ok(Self {
            client,
            handle,
            idle_timeout: Duration::from_secs(120),
            session_header: None,
        })
    }

    /// Makes every request carry the conversation id in header `name`.
    pub(crate) fn set_session_header(&mut self, name: &str) {
        self.session_header = Some(name.to_owned());
    }

    /// The header to add for `session_id`: none unless a header name was set and the
    /// request has an id.
    pub(crate) fn session_headers(&self, session_id: Option<&str>) -> Vec<(String, String)> {
        match (&self.session_header, session_id) {
            (Some(name), Some(id)) => vec![(name.clone(), id.to_owned())],
            _ => Vec::new(),
        }
    }

    pub(crate) fn set_idle_timeout(&mut self, idle_timeout: Duration) {
        self.idle_timeout = idle_timeout;
    }

    /// Sends `post` and returns a stream driven by `protocol`.
    pub(crate) fn start<P: StreamProtocol>(
        &self,
        post: Post,
        cancellation: CancellationToken,
        protocol: P,
    ) -> BoxFuture<Result<Box<dyn ProviderStream>, ProviderError>> {
        let client = self.client.clone();
        let handle = self.handle.clone();
        let idle_timeout = self.idle_timeout;
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let admission = handle.spawn(async move {
                let mut builder = client
                    .post(post.url)
                    .header("content-type", "application/json")
                    .header("accept", "text/event-stream")
                    .body(post.body);
                for (name, value) in post.headers {
                    builder = builder.header(name, value);
                }
                if let Some(key) = post.bearer {
                    builder = builder.bearer_auth(key);
                }
                let response = builder
                    .send()
                    .await
                    .map_err(|error| transport_error(&error))?;
                let status = response.status();
                if status.is_success() {
                    return Ok(response);
                }
                let text = response.text().await.unwrap_or_default();
                Err(status_error(status.as_u16(), &text))
            });
            let response = admission.await.map_err(|error| ProviderError::Request {
                message: format!("the request task failed: {error}"),
                retryable: false,
            })??;
            let (sender, receiver) = mpsc::channel(64);
            handle.spawn(pump(response, sender, cancellation, idle_timeout, protocol));
            Ok(Box::new(ChannelStream {
                receiver: Arc::new(Mutex::new(receiver)),
            }) as Box<dyn ProviderStream>)
        })
    }
}

struct ChannelStream {
    receiver: Arc<Mutex<mpsc::Receiver<EventResult>>>,
}

impl ProviderStream for ChannelStream {
    fn next(&mut self) -> BoxFuture<Result<Option<ProviderStreamEvent>, ProviderError>> {
        let receiver = Arc::clone(&self.receiver);
        Box::pin(async move {
            match receiver.lock().await.recv().await {
                Some(Ok(event)) => Ok(Some(event)),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        })
    }
}

fn transport_error(error: &reqwest::Error) -> ProviderError {
    ProviderError::Request {
        message: format!("request failed: {error}"),
        retryable: error.is_connect() || error.is_timeout() || error.is_request(),
    }
}

/// Maps a non-success HTTP status and body to a stable error kind.
pub(crate) fn status_error(status: u16, body: &str) -> ProviderError {
    let mut detail: String = body.chars().take(MAX_ERROR_BODY_BYTES).collect();
    if let Ok(value) = serde_json::from_str::<Value>(body)
        && let Some(message) = value.pointer("/error/message").and_then(Value::as_str)
    {
        message.clone_into(&mut detail);
    }
    let message = format!("HTTP {status}: {detail}");
    // A spent quota does not come back by trying again.
    let spent = status == 429 && is_spent_quota(&format!("{body} {detail}"));
    match status {
        429 if spent => ProviderError::Request {
            message,
            retryable: false,
        },
        401 | 403 => ProviderError::Authentication { message },
        404 => ProviderError::ModelUnavailable { message },
        408 | 409 | 425 | 429 | 500..=599 => ProviderError::Request {
            message,
            retryable: true,
        },
        _ => ProviderError::Request {
            message,
            retryable: false,
        },
    }
}

/// Whether a 429 body says the account is out of quota or budget, not just busy.
fn is_spent_quota(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "gousagelimiterror",
        "freeusagelimiterror",
        "monthly usage limit reached",
        "usage limit",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Token accounting shared by the `OpenAI` wire protocols.
///
/// `prompt` includes the cached tokens, so the reported input excludes them,
/// matching how the TypeScript providers count; the total is the sum of the
/// four parts.
pub(crate) fn usage_from_counts(
    prompt: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> Usage {
    let input = prompt
        .saturating_sub(cache_read)
        .saturating_sub(cache_write);
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input
            .saturating_add(output)
            .saturating_add(cache_read)
            .saturating_add(cache_write),
    }
}

async fn send_error(sender: &mpsc::Sender<EventResult>, message: &str, retryable: bool) {
    let _ = sender
        .send(Err(ProviderError::Stream {
            message: message.to_owned(),
            retryable,
        }))
        .await;
}

async fn pump<P: StreamProtocol>(
    mut response: reqwest::Response,
    sender: mpsc::Sender<EventResult>,
    cancellation: CancellationToken,
    idle_timeout: Duration,
    mut protocol: P,
) {
    let mut decoder = SseDecoder::new();
    let mut idle = Duration::ZERO;
    let mut done = false;
    'read: loop {
        if cancellation.is_cancelled() {
            let _ = sender.send(Err(ProviderError::Cancelled)).await;
            return;
        }
        let chunk = match tokio::time::timeout(CANCELLATION_POLL, response.chunk()).await {
            Err(_) => {
                idle += CANCELLATION_POLL;
                if idle >= idle_timeout {
                    send_error(&sender, "the response stream went silent", true).await;
                    return;
                }
                continue;
            }
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(error)) => {
                send_error(
                    &sender,
                    &format!("the response stream failed: {error}"),
                    true,
                )
                .await;
                return;
            }
        };
        idle = Duration::ZERO;
        for event in decoder.push(&chunk) {
            match protocol.apply(&event) {
                Ok(Applied::Nothing) => {}
                Ok(Applied::Text(text)) => {
                    if sender
                        .send(Ok(ProviderStreamEvent::TextDelta { text }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(Applied::Done) => {
                    done = true;
                    break 'read;
                }
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            }
        }
    }
    if !done && !protocol.is_terminal() {
        send_error(&sender, "the response ended before a terminal event", true).await;
        return;
    }
    let Finished {
        usage,
        calls,
        reason,
    } = protocol.finish();
    if let Some(usage) = usage
        && sender
            .send(Ok(ProviderStreamEvent::Usage(usage)))
            .await
            .is_err()
    {
        return;
    }
    for call in calls {
        if sender
            .send(Ok(ProviderStreamEvent::ToolCall(call)))
            .await
            .is_err()
        {
            return;
        }
    }
    let _ = sender
        .send(Ok(ProviderStreamEvent::Finished { reason }))
        .await;
}

#[cfg(test)]
mod tests {
    use super::{status_error, usage_from_counts};
    use nyan_coding_agent_provider::ProviderError;

    #[test]
    fn cached_tokens_are_not_counted_twice_in_the_input() {
        let usage = usage_from_counts(12, 3, 4, 2);
        assert_eq!(
            (
                usage.input,
                usage.output,
                usage.cache_read,
                usage.cache_write
            ),
            (6, 3, 4, 2)
        );
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn a_cache_count_larger_than_the_prompt_does_not_underflow() {
        let usage = usage_from_counts(3, 1, 10, 0);
        assert_eq!(usage.input, 0);
    }

    #[test]
    fn an_error_body_message_is_preferred_over_the_raw_body() {
        let error = status_error(429, r#"{"error":{"message":"slow down"}}"#);
        assert_eq!(
            error,
            ProviderError::Request {
                message: "HTTP 429: slow down".to_owned(),
                retryable: true
            }
        );
    }
}
