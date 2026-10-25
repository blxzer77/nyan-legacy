//! The AWS `application/vnd.amazon.eventstream` binary framing.
//!
//! Each message is `total_length`, `headers_length`, a CRC of those eight bytes,
//! the headers, the payload, and a CRC of everything before it (all integers
//! big-endian). Both CRCs are checked: a corrupt frame is an error, never a
//! guess. Frames come out as [`SseEvent`]s so a protocol sees one shape: `event`
//! is `:event-type` for an event, `exception:<type>` for an exception, and
//! `error:<code>` for an error message; `data` is the payload as UTF-8 text.

use nyan_coding_agent_provider::ProviderError;

use crate::sse::SseEvent;
use crate::transport::FrameDecoder;

const PRELUDE: usize = 12;
const MESSAGE_CRC: usize = 4;
/// The service limit on one event is far below this; it bounds a corrupt length.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;

fn crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = u32::try_from(index).unwrap_or(0);
        for _ in 0..8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *entry = value;
    }
    table
}

/// CRC-32 (IEEE 802.3), as used by the event stream.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let table = crc_table();
    !data.iter().fold(!0_u32, |crc, byte| {
        table[usize::from(crc.to_le_bytes()[0] ^ byte)] ^ (crc >> 8)
    })
}

fn corrupt(message: &str) -> ProviderError {
    ProviderError::Stream {
        message: format!("the event stream is corrupt: {message}"),
        retryable: true,
    }
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Incremental decoder: chunks may split a message anywhere.
#[derive(Debug, Default)]
pub struct EventStreamDecoder {
    buffer: Vec<u8>,
}

impl EventStreamDecoder {
    /// Creates an empty decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a partial message is still buffered.
    #[must_use]
    pub fn has_partial_message(&self) -> bool {
        !self.buffer.is_empty()
    }
}

/// String headers by name; other header types are skipped over correctly.
fn parse_headers(mut headers: &[u8]) -> Result<Vec<(String, String)>, ProviderError> {
    let mut parsed = Vec::new();
    while !headers.is_empty() {
        let name_length = usize::from(headers[0]);
        let rest = headers
            .get(1..)
            .ok_or_else(|| corrupt("truncated header"))?;
        let name = rest
            .get(..name_length)
            .ok_or_else(|| corrupt("truncated header name"))?;
        let rest = &rest[name_length..];
        let (&kind, rest) = rest
            .split_first()
            .ok_or_else(|| corrupt("missing header type"))?;
        let (value, remaining): (Option<String>, &[u8]) = match kind {
            0 | 1 => (None, rest),
            2 => (None, rest.get(1..).ok_or_else(|| corrupt("short header"))?),
            3 => (None, rest.get(2..).ok_or_else(|| corrupt("short header"))?),
            4 => (None, rest.get(4..).ok_or_else(|| corrupt("short header"))?),
            5 | 8 => (None, rest.get(8..).ok_or_else(|| corrupt("short header"))?),
            9 => (None, rest.get(16..).ok_or_else(|| corrupt("short header"))?),
            6 | 7 => {
                let length = rest.get(..2).ok_or_else(|| corrupt("short header"))?;
                let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
                let value = rest
                    .get(2..2 + length)
                    .ok_or_else(|| corrupt("truncated header value"))?;
                let text = (kind == 7).then(|| String::from_utf8_lossy(value).into_owned());
                (text, &rest[2 + length..])
            }
            other => return Err(corrupt(&format!("unknown header type {other}"))),
        };
        if let Some(value) = value {
            parsed.push((String::from_utf8_lossy(name).into_owned(), value));
        }
        headers = remaining;
    }
    Ok(parsed)
}

impl FrameDecoder for EventStreamDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, ProviderError> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            if self.buffer.len() < PRELUDE {
                break;
            }
            let total = be32(&self.buffer[0..4]) as usize;
            let headers_length = be32(&self.buffer[4..8]) as usize;
            if be32(&self.buffer[8..12]) != crc32(&self.buffer[0..8]) {
                return Err(corrupt("the prelude checksum does not match"));
            }
            if !(PRELUDE + MESSAGE_CRC..=MAX_MESSAGE).contains(&total)
                || headers_length > total - PRELUDE - MESSAGE_CRC
            {
                return Err(corrupt("the message lengths are inconsistent"));
            }
            if self.buffer.len() < total {
                break;
            }
            let message: Vec<u8> = self.buffer.drain(..total).collect();
            if be32(&message[total - MESSAGE_CRC..]) != crc32(&message[..total - MESSAGE_CRC]) {
                return Err(corrupt("the message checksum does not match"));
            }
            let headers = parse_headers(&message[PRELUDE..PRELUDE + headers_length])?;
            let header = |name: &str| {
                headers
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .map(|(_, value)| value.as_str())
            };
            let event = match header(":message-type") {
                Some("exception") => format!(
                    "exception:{}",
                    header(":exception-type").unwrap_or("unknown")
                ),
                Some("error") => format!("error:{}", header(":error-code").unwrap_or("unknown")),
                _ => header(":event-type").unwrap_or_default().to_owned(),
            };
            let payload = &message[PRELUDE + headers_length..total - MESSAGE_CRC];
            events.push(SseEvent {
                event: Some(event),
                data: String::from_utf8_lossy(payload).into_owned(),
            });
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests;
