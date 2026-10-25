//! Request admission and streaming shared by every wire protocol.
//!
//! A protocol supplies a [`StreamProtocol`]: how one SSE event changes its
//! state and how its terminal state becomes provider events. This module owns
//! everything else: connecting, HTTP status mapping, cancellation, the idle
//! timeout, and delivery through a channel so the future returned by `stream`
//! can be awaited on any executor.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

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
                retry_after: None,
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
            let mut admission = handle.spawn(async move {
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
                let retry_after = retry_after(response.headers());
                let text = response.text().await.unwrap_or_default();
                Err(status_error_with_retry_after(
                    status.as_u16(),
                    &text,
                    retry_after,
                ))
            });
            // Waiting for the response headers can take as long as the server likes, so
            // the wait itself watches for cancellation.
            let admitted = loop {
                match tokio::time::timeout(CANCELLATION_POLL, &mut admission).await {
                    Ok(joined) => break joined,
                    Err(_) if cancellation.is_cancelled() => {
                        admission.abort();
                        return Err(ProviderError::Cancelled);
                    }
                    Err(_) => {}
                }
            };
            let response = admitted.map_err(|error| ProviderError::Request {
                message: format!("the request task failed: {error}"),
                retryable: false,
                retry_after: None,
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
        retry_after: None,
    }
}

fn duration_from_secs(value: f64) -> Option<Duration> {
    if value.is_finite() && value >= 0.0 && value <= Duration::MAX.as_secs_f64() {
        Some(Duration::from_secs_f64(value))
    } else {
        None
    }
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .and_then(|value| duration_from_secs(value / 1000.0))
    {
        return Some(value);
    }
    let value = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)?;
    if let Ok(seconds) = value.parse::<f64>()
        && let Some(delay) = duration_from_secs(seconds)
    {
        return Some(delay);
    }
    let timestamp = parse_http_date(value)?;
    Some(
        timestamp
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    let fields: Vec<&str> = value.split_ascii_whitespace().collect();
    if fields.len() != 6 || fields[5] != "GMT" {
        return None;
    }
    let day = fields[1].parse::<u32>().ok()?;
    let month = match fields[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = fields[3].parse::<i64>().ok()?;
    let mut clock = fields[4].split(':');
    let hour = clock.next()?.parse::<u64>().ok()?;
    let minute = clock.next()?.parse::<u64>().ok()?;
    let second = clock.next()?.parse::<u64>().ok()?;
    if clock.next().is_some() || hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::try_from(hour * 3_600 + minute * 60 + second).ok()?)?;
    if seconds < 0 {
        return Some(SystemTime::UNIX_EPOCH);
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).ok()?))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 || !(1..=9999).contains(&year) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let month_prime = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// Maps a non-success HTTP status and body to a stable error kind.
#[cfg(test)]
pub(crate) fn status_error(status: u16, body: &str) -> ProviderError {
    status_error_with_retry_after(status, body, None)
}

fn status_error_with_retry_after(
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
) -> ProviderError {
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
            retry_after,
        },
        401 | 403 => ProviderError::Authentication { message },
        404 => ProviderError::ModelUnavailable { message },
        408 | 409 | 425 | 429 | 500..=599 => ProviderError::Request {
            message,
            retryable: true,
            retry_after,
        },
        _ => ProviderError::Request {
            message,
            retryable: false,
            retry_after,
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
    use std::time::Duration;

    use super::{retry_after, status_error, status_error_with_retry_after, usage_from_counts};
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
                retryable: true,
                retry_after: None,
            }
        );
    }

    #[test]
    fn retry_after_accepts_milliseconds_seconds_and_http_dates() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "retry-after-ms",
            reqwest::header::HeaderValue::from_static("1250"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(1250)));

        headers.clear();
        headers.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_static("2.5"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::from_millis(2500)));

        headers.clear();
        headers.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::ZERO));

        assert_eq!(
            status_error_with_retry_after(503, "busy", Some(Duration::from_secs(7))),
            ProviderError::Request {
                message: "HTTP 503: busy".to_owned(),
                retryable: true,
                retry_after: Some(Duration::from_secs(7)),
            }
        );
    }
}
