//! Incremental Server-Sent Events decoding.
//!
//! The decoder accepts arbitrary byte chunks: network reads can split a line, a
//! CRLF pair, or a multi-byte UTF-8 character anywhere.

/// One dispatched SSE event.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SseEvent {
    /// The `event:` field, when present.
    pub event: Option<String>,
    /// The `data:` lines joined with LF.
    pub data: String,
}

/// Stateful SSE decoder.
#[derive(Debug, Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    skip_lf: bool,
}

impl SseDecoder {
    /// Creates an empty decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk and returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        for &byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' => self.finish_line(&mut events),
                b'\r' => {
                    self.skip_lf = true;
                    self.finish_line(&mut events);
                }
                other => self.pending.push(other),
            }
        }
        events
    }

    fn finish_line(&mut self, events: &mut Vec<SseEvent>) {
        let line = String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned();
        if line.is_empty() {
            if !self.data.is_empty() || self.event.is_some() {
                events.push(SseEvent {
                    event: self.event.take(),
                    data: self.data.join("\n"),
                });
                self.data.clear();
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SseDecoder, SseEvent};

    fn event(name: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: name.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn decodes_events_with_either_line_ending() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b"data: one\n\ndata: two\r\n\r\nevent: done\rdata: [DONE]\r\r");
        assert_eq!(
            events,
            [
                event(None, "one"),
                event(None, "two"),
                event(Some("done"), "[DONE]")
            ]
        );
    }

    #[test]
    fn joins_multiline_data_and_ignores_comments_and_unknown_fields() {
        let mut decoder = SseDecoder::new();
        let events = decoder.push(b": keepalive\nid: 7\ndata: a\ndata:b\ndata\n\n");
        assert_eq!(events, [event(None, "a\nb\n")]);
    }

    #[test]
    fn a_chunk_boundary_may_fall_anywhere() {
        let whole = "data: h\u{e9}llo \u{4e16}\u{754c}\r\n\r\n".as_bytes();
        for split in 0..=whole.len() {
            let mut decoder = SseDecoder::new();
            let mut events = decoder.push(&whole[..split]);
            events.extend(decoder.push(&whole[split..]));
            assert_eq!(
                events,
                [event(None, "h\u{e9}llo \u{4e16}\u{754c}")],
                "split {split}"
            );
        }
    }

    #[test]
    fn an_unterminated_event_is_not_dispatched() {
        let mut decoder = SseDecoder::new();
        assert_eq!(decoder.push(b"data: partial\n"), []);
        assert_eq!(decoder.push(b"\n"), [event(None, "partial")]);
    }
}
