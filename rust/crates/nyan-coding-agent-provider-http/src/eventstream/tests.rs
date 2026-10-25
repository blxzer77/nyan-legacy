use super::{EventStreamDecoder, crc32};
use crate::transport::FrameDecoder;

/// Builds one message the way the service does: string headers only.
pub(crate) fn message(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
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

#[test]
fn crc32_matches_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn an_event_and_an_exception_decode_to_named_frames() {
    let mut decoder = EventStreamDecoder::new();
    let mut bytes = message(
        &[
            (":message-type", "event"),
            (":event-type", "contentBlockDelta"),
        ],
        br#"{"delta":{"text":"hi"}}"#,
    );
    bytes.extend(message(
        &[
            (":message-type", "exception"),
            (":exception-type", "throttlingException"),
        ],
        br#"{"message":"slow down"}"#,
    ));
    let events = decoder.push(&bytes).expect("decode");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event.as_deref(), Some("contentBlockDelta"));
    assert_eq!(events[0].data, r#"{"delta":{"text":"hi"}}"#);
    assert_eq!(
        events[1].event.as_deref(),
        Some("exception:throttlingException")
    );
    assert!(!decoder.has_partial_message());
}

#[test]
fn messages_split_at_every_byte_boundary_decode_the_same() {
    let bytes = message(
        &[(":message-type", "event"), (":event-type", "messageStop")],
        br#"{"stopReason":"end_turn"}"#,
    );
    for split in 1..bytes.len() {
        let mut decoder = EventStreamDecoder::new();
        let mut events = decoder.push(&bytes[..split]).expect("first half");
        assert!(events.is_empty(), "split at {split} produced a frame early");
        assert!(decoder.has_partial_message());
        events.extend(decoder.push(&bytes[split..]).expect("second half"));
        assert_eq!(events.len(), 1, "split at {split}");
        assert_eq!(events[0].event.as_deref(), Some("messageStop"));
    }
}

#[test]
fn a_flipped_bit_in_the_prelude_or_the_body_is_an_error_not_a_guess() {
    let good = message(&[(":event-type", "x")], b"{}");
    for position in [0, 5, 9, 14, good.len() - 6, good.len() - 1] {
        let mut bad = good.clone();
        bad[position] ^= 0x01;
        let error = EventStreamDecoder::new()
            .push(&bad)
            .expect_err("corruption must be detected");
        assert!(
            error.to_string().contains("event stream is corrupt"),
            "{position}: {error}"
        );
        assert!(error.is_retryable());
    }
}

#[test]
fn an_absurd_length_is_rejected_before_buffering_it() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::MAX.to_be_bytes());
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    let prelude_crc = crc32(&bytes);
    bytes.extend_from_slice(&prelude_crc.to_be_bytes());
    let error = EventStreamDecoder::new()
        .push(&bytes)
        .expect_err("too large");
    assert!(
        error.to_string().contains("lengths are inconsistent"),
        "{error}"
    );
}

#[test]
fn non_string_headers_are_skipped_without_losing_the_string_ones() {
    // A timestamp header (type 8) and an int header (type 4) before the event type.
    let mut headers = Vec::new();
    headers.extend_from_slice(&[2, b't', b's', 8, 0, 0, 0, 0, 0, 0, 0, 1]);
    headers.extend_from_slice(&[1, b'n', 4, 0, 0, 0, 9]);
    headers.extend_from_slice(&[
        11, b':', b'e', b'v', b'e', b'n', b't', b'-', b't', b'y', b'p', b'e', 7, 0, 1, b'm',
    ]);
    let payload = b"{}";
    let total = 12 + headers.len() + payload.len() + 4;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(total).expect("size").to_be_bytes());
    bytes.extend_from_slice(&u32::try_from(headers.len()).expect("size").to_be_bytes());
    let prelude_crc = crc32(&bytes);
    bytes.extend_from_slice(&prelude_crc.to_be_bytes());
    bytes.extend_from_slice(&headers);
    bytes.extend_from_slice(payload);
    let message_crc = crc32(&bytes);
    bytes.extend_from_slice(&message_crc.to_be_bytes());
    let events = EventStreamDecoder::new().push(&bytes).expect("decode");
    assert_eq!(events[0].event.as_deref(), Some("m"));
}

#[test]
fn a_corrupt_length_that_looks_plausible_fails_at_the_prelude_instead_of_waiting() {
    let mut bytes = message(&[(":event-type", "x")], b"{}");
    // Grow the length field: still within limits, so only the prelude CRC can catch it.
    let total = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) + 500;
    bytes[0..4].copy_from_slice(&total.to_be_bytes());
    let error = EventStreamDecoder::new()
        .push(&bytes)
        .expect_err("the prelude checksum must reject it");
    assert!(error.to_string().contains("prelude checksum"), "{error}");
}
