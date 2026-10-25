//! The models.dev fetch, against a loopback server.

use nyan_coding_agent_provider_http::models_dev::{FetchError, fetch};
use tokio::runtime::Runtime;

mod common;
use common::{serve, serve_with_headers};

fn fetch_from(
    status: &'static str,
    body: &[u8],
) -> (
    Result<serde_json::Map<String, serde_json::Value>, FetchError>,
    String,
) {
    let runtime = Runtime::new().expect("runtime");
    let server = serve(&runtime, status, vec![(0, body.to_vec())], false);
    let url = format!("http://{}/api.json", server.address);
    let result = runtime.block_on(fetch(runtime.handle(), &url));
    let request = server.captured.lock().expect("lock").clone();
    (result, request)
}

#[test]
fn a_json_object_is_returned_as_the_provider_map() {
    let (result, request) = fetch_from("200 OK", br#"{"openai":{"models":{"m":{}}}}"#);
    let providers = result.expect("fetched");
    assert!(providers["openai"]["models"]["m"].is_object());
    assert!(request.starts_with("GET /api.json HTTP/1.1"), "{request}");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("accept: application/json"),
        "{request}"
    );
}

#[test]
fn bad_statuses_and_bodies_are_failures_not_an_empty_catalog() {
    let (status, _) = fetch_from("503 Service Unavailable", b"down");
    assert!(status.expect_err("status").0.ends_with("answered HTTP 503"));
    for (body, expected) in [
        (&b"[]"[..], "did not return a JSON object"),
        (b"\"text\"", "did not return a JSON object"),
        (b"not json", "returned invalid JSON"),
        (b"", "returned invalid JSON"),
    ] {
        let (result, _) = fetch_from("200 OK", body);
        assert!(
            result.expect_err("bad body").0.contains(expected),
            "{body:?}"
        );
    }
}

#[test]
fn an_unreachable_server_is_a_failure_that_names_the_url() {
    let runtime = Runtime::new().expect("runtime");
    let error = runtime
        .block_on(fetch(runtime.handle(), "http://127.0.0.1:1/api.json"))
        .expect_err("nothing listens on port 1");
    assert!(
        error
            .0
            .starts_with("could not reach http://127.0.0.1:1/api.json"),
        "{error}"
    );
}

/// `{"groq":{"models":{"m":{"tool_call":true}}}}`, gzip-compressed.
const GZIPPED: [u8; 58] = [
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 86, 74, 47, 202, 47, 84, 178, 170, 86, 202, 205, 79,
    73, 205, 41, 6, 179, 64, 68, 73, 126, 126, 78, 124, 114, 98, 78, 142, 146, 85, 73, 81, 105,
    106, 109, 109, 109, 45, 0, 80, 162, 55, 113, 44, 0, 0, 0,
];

#[test]
fn a_gzipped_body_is_requested_and_decoded() {
    let runtime = Runtime::new().expect("runtime");
    let server = serve_with_headers(
        &runtime,
        "200 OK",
        "content-encoding: gzip\r\n",
        vec![(0, GZIPPED.to_vec())],
        false,
    );
    let url = format!("http://{}/api.json", server.address);
    let providers = runtime
        .block_on(fetch(runtime.handle(), &url))
        .expect("a compressed body must be decoded");
    assert_eq!(providers["groq"]["models"]["m"]["tool_call"], true);
    let request = server.captured.lock().expect("lock").to_ascii_lowercase();
    assert!(
        request.contains("accept-encoding: gzip"),
        "the 600 KB form is requested: {request}"
    );
}
