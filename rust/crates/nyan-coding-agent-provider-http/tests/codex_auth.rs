//! The Codex OAuth exchange, refresh, device-code poll, and callback server,
//! against loopback servers. The real `auth.openai.com` is never contacted.

use std::time::Duration;

use nyan_coding_agent_core::CancellationToken;
use nyan_coding_agent_provider_http::codex_auth::{
    CALLBACK_PATH, CLIENT_ID, OAuthError, PollPacing, REDIRECT_URI, bind_callback, exchange_code,
    poll_device, refresh, serve_callback, start_device,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;

mod common;
use common::serve_sequence;

fn tokens_body() -> String {
    json!({"access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600})
        .to_string()
}

fn form(request: &str) -> std::collections::BTreeMap<String, String> {
    let body = request.split("\r\n\r\n").nth(1).expect("body");
    reqwest::Url::parse(&format!("http://form.invalid/?{body}"))
        .expect("form body")
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[test]
fn a_code_exchange_posts_the_pkce_form_and_computes_the_expiry_from_the_reply() {
    let runtime = Runtime::new().expect("runtime");
    let (address, log) = serve_sequence(&runtime, vec![("200 OK", tokens_body())]);
    let tokens = runtime
        .block_on(exchange_code(
            runtime.handle(),
            &format!("http://{address}"),
            "the-code",
            "the-verifier",
            REDIRECT_URI,
            1_000_000,
        ))
        .expect("tokens");
    assert_eq!(
        (
            tokens.access.as_str(),
            tokens.refresh.as_str(),
            tokens.expires_ms
        ),
        ("new-access", "new-refresh", 1_000_000 + 3_600_000)
    );
    let request = log.lock().expect("lock")[0].clone();
    assert!(
        request.starts_with("POST /oauth/token HTTP/1.1"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "{request}"
    );
    let fields = form(&request);
    for (name, value) in [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", "the-code"),
        ("code_verifier", "the-verifier"),
        ("redirect_uri", REDIRECT_URI),
    ] {
        assert_eq!(fields.get(name).map(String::as_str), Some(value), "{name}");
    }
}

#[test]
fn a_refresh_posts_the_refresh_token_and_never_the_code_fields() {
    let runtime = Runtime::new().expect("runtime");
    let (address, log) = serve_sequence(&runtime, vec![("200 OK", tokens_body())]);
    runtime
        .block_on(refresh(
            runtime.handle(),
            &format!("http://{address}"),
            "old-refresh",
            0,
        ))
        .expect("tokens");
    let fields = form(&log.lock().expect("lock")[0]);
    assert_eq!(
        fields.get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        fields.get("refresh_token").map(String::as_str),
        Some("old-refresh")
    );
    assert_eq!(fields.get("client_id").map(String::as_str), Some(CLIENT_ID));
    assert!(!fields.contains_key("code") && !fields.contains_key("code_verifier"));
}

#[test]
fn token_failures_say_which_operation_failed_and_what_the_server_said() {
    let runtime = Runtime::new().expect("runtime");
    let run = |status: &'static str, body: &str, exchange: bool| -> OAuthError {
        let (address, _) = serve_sequence(&runtime, vec![(status, body.to_owned())]);
        let base = format!("http://{address}");
        runtime
            .block_on(async {
                if exchange {
                    exchange_code(runtime.handle(), &base, "c", "v", REDIRECT_URI, 0).await
                } else {
                    refresh(runtime.handle(), &base, "r", 0).await
                }
            })
            .expect_err("must fail")
    };
    assert_eq!(
        run("400 Bad Request", r#"{"error":"invalid_grant"}"#, false).0,
        r#"OpenAI Codex token refresh failed (400): {"error":"invalid_grant"}"#
    );
    assert_eq!(
        run("401 Unauthorized", "denied", true).0,
        "OpenAI Codex token exchange failed (401): denied"
    );
    for body in [
        r#"{"access_token":"a"}"#,
        r#"{"access_token":"a","refresh_token":"r","expires_in":"soon"}"#,
        "not json",
    ] {
        assert!(
            run("200 OK", body, true).0.contains("exchange response"),
            "{body}"
        );
    }
}

fn fast() -> PollPacing {
    PollPacing {
        minimum_interval: Duration::from_millis(10),
        slow_down_step: Duration::from_millis(10),
        timeout: Duration::from_secs(10),
    }
}

#[test]
fn a_device_code_is_requested_with_the_client_id_and_its_interval_is_read_as_a_number_or_a_string()
{
    let runtime = Runtime::new().expect("runtime");
    for interval in [json!(7), json!(" 7 ")] {
        let (address, log) = serve_sequence(
            &runtime,
            vec![(
                "200 OK",
                json!({"device_auth_id": "dev-1", "user_code": "ABCD-1234", "interval": interval})
                    .to_string(),
            )],
        );
        let device = runtime
            .block_on(start_device(runtime.handle(), &format!("http://{address}")))
            .expect("device code");
        assert_eq!(
            (
                device.device_auth_id.as_str(),
                device.user_code.as_str(),
                device.interval_seconds
            ),
            ("dev-1", "ABCD-1234", 7)
        );
        let request = log.lock().expect("lock")[0].clone();
        assert!(
            request.starts_with("POST /api/accounts/deviceauth/usercode HTTP/1.1"),
            "{request}"
        );
        assert!(
            request.contains(&format!("\"client_id\":\"{CLIENT_ID}\"")),
            "{request}"
        );
    }
    let (address, _) = serve_sequence(&runtime, vec![("404 Not Found", String::new())]);
    let error = runtime
        .block_on(start_device(runtime.handle(), &format!("http://{address}")))
        .expect_err("not enabled");
    assert!(
        error
            .0
            .contains("device code login is not enabled for this server"),
        "{error}"
    );
    let (address, _) = serve_sequence(
        &runtime,
        vec![("200 OK", r#"{"user_code":"X"}"#.to_owned())],
    );
    assert!(
        runtime
            .block_on(start_device(runtime.handle(), &format!("http://{address}")))
            .is_err()
    );
}

fn device() -> nyan_coding_agent_provider_http::codex_auth::DeviceCode {
    nyan_coding_agent_provider_http::codex_auth::DeviceCode {
        device_auth_id: "dev-1".to_owned(),
        user_code: "ABCD-1234".to_owned(),
        interval_seconds: 0,
    }
}

#[test]
fn polling_waits_through_pending_answers_and_returns_the_approval() {
    let runtime = Runtime::new().expect("runtime");
    let (address, log) = serve_sequence(
        &runtime,
        vec![
            ("403 Forbidden", String::new()),
            ("404 Not Found", String::new()),
            (
                "400 Bad Request",
                r#"{"error":{"code":"deviceauth_authorization_pending"}}"#.to_owned(),
            ),
            (
                "200 OK",
                json!({"authorization_code": "auth-code", "code_verifier": "server-verifier"})
                    .to_string(),
            ),
        ],
    );
    let approval = runtime
        .block_on(poll_device(
            runtime.handle(),
            &format!("http://{address}"),
            &device(),
            fast(),
            &CancellationToken::new(),
        ))
        .expect("approved");
    assert_eq!(
        (
            approval.authorization_code.as_str(),
            approval.code_verifier.as_str()
        ),
        ("auth-code", "server-verifier")
    );
    let requests = log.lock().expect("lock");
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("POST /api/accounts/deviceauth/token HTTP/1.1"));
    assert!(
        requests[0].contains("\"device_auth_id\":\"dev-1\"")
            && requests[0].contains("\"user_code\":\"ABCD-1234\"")
    );
}

#[test]
fn slow_down_lengthens_the_wait_and_other_errors_end_the_login() {
    let runtime = Runtime::new().expect("runtime");
    let pacing = PollPacing {
        minimum_interval: Duration::from_millis(10),
        slow_down_step: Duration::from_millis(150),
        timeout: Duration::from_secs(10),
    };
    let (address, _) = serve_sequence(
        &runtime,
        vec![
            ("400 Bad Request", r#"{"error":"slow_down"}"#.to_owned()),
            (
                "200 OK",
                json!({"authorization_code": "c", "code_verifier": "v"}).to_string(),
            ),
        ],
    );
    let started = std::time::Instant::now();
    runtime
        .block_on(poll_device(
            runtime.handle(),
            &format!("http://{address}"),
            &device(),
            pacing,
            &CancellationToken::new(),
        ))
        .expect("approved");
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "the step must be waited: {:?}",
        started.elapsed()
    );

    let (address, _) = serve_sequence(
        &runtime,
        vec![("400 Bad Request", r#"{"error":"access_denied"}"#.to_owned())],
    );
    let error = runtime
        .block_on(poll_device(
            runtime.handle(),
            &format!("http://{address}"),
            &device(),
            fast(),
            &CancellationToken::new(),
        ))
        .expect_err("denied");
    assert!(
        error
            .0
            .starts_with("OpenAI Codex device auth failed with status 400"),
        "{error}"
    );
}

#[test]
fn polling_ends_on_cancellation_and_on_timeout() {
    let runtime = Runtime::new().expect("runtime");
    let (address, _) = serve_sequence(&runtime, vec![("403 Forbidden", String::new())]);
    let base = format!("http://{address}");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = runtime
        .block_on(poll_device(
            runtime.handle(),
            &base,
            &device(),
            fast(),
            &cancellation,
        ))
        .expect_err("cancelled");
    assert_eq!(error.0, "Login cancelled");

    let short = PollPacing {
        timeout: Duration::from_millis(100),
        ..fast()
    };
    let error = runtime
        .block_on(poll_device(
            runtime.handle(),
            &base,
            &device(),
            short,
            &CancellationToken::new(),
        ))
        .expect_err("timed out");
    assert_eq!(error.0, "Device flow timed out");
}

async fn browser_hits(port: u16, target: &str) -> String {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    socket
        .write_all(format!("GET {target} HTTP/1.1\r\nhost: localhost\r\n\r\n").as_bytes())
        .await
        .expect("write");
    let mut reply = String::new();
    socket.read_to_string(&mut reply).await.expect("read");
    reply
}

#[test]
fn the_callback_server_returns_the_code_for_the_right_state_and_ignores_other_paths() {
    let runtime = Runtime::new().expect("runtime");
    let listener = runtime
        .block_on(bind_callback("127.0.0.1", 0))
        .expect("bind");
    let port = listener.local_addr().expect("address").port();
    let waiter = runtime.spawn(async move {
        serve_callback(
            &listener,
            CALLBACK_PATH,
            "state-1",
            Duration::from_secs(10),
            &CancellationToken::new(),
        )
        .await
    });
    let stray = runtime.block_on(browser_hits(port, "/favicon.ico"));
    assert!(stray.starts_with("HTTP/1.1 404"), "{stray}");
    let page = runtime.block_on(browser_hits(
        port,
        &format!("{CALLBACK_PATH}?code=the-code&state=state-1"),
    ));
    assert!(
        page.starts_with("HTTP/1.1 200") && page.contains("Signed in"),
        "{page}"
    );
    assert_eq!(
        runtime.block_on(waiter).expect("task"),
        Ok("the-code".to_owned())
    );
}

#[test]
fn a_wrong_state_a_provider_error_and_a_missing_code_end_the_wait_with_a_failure_page() {
    let runtime = Runtime::new().expect("runtime");
    for (query, expected) in [
        ("code=c&state=other", "State mismatch"),
        (
            "error=access_denied&error_description=No+thanks&state=state-1",
            "OpenAI sign-in failed: No thanks",
        ),
        ("state=state-1", "Missing authorization code"),
    ] {
        let listener = runtime
            .block_on(bind_callback("127.0.0.1", 0))
            .expect("bind");
        let port = listener.local_addr().expect("address").port();
        let waiter = runtime.spawn(async move {
            serve_callback(
                &listener,
                CALLBACK_PATH,
                "state-1",
                Duration::from_secs(10),
                &CancellationToken::new(),
            )
            .await
        });
        let page = runtime.block_on(browser_hits(port, &format!("{CALLBACK_PATH}?{query}")));
        assert!(
            page.starts_with("HTTP/1.1 400") && page.contains("Sign-in failed"),
            "{query}: {page}"
        );
        assert_eq!(
            runtime.block_on(waiter).expect("task"),
            Err(OAuthError(expected.to_owned())),
            "{query}"
        );
    }
}

#[test]
fn the_callback_wait_times_out_and_can_be_cancelled_and_a_taken_port_is_reported() {
    let runtime = Runtime::new().expect("runtime");
    let listener = runtime
        .block_on(bind_callback("127.0.0.1", 0))
        .expect("bind");
    let port = listener.local_addr().expect("address").port();
    let error = runtime
        .block_on(serve_callback(
            &listener,
            CALLBACK_PATH,
            "s",
            Duration::from_millis(120),
            &CancellationToken::new(),
        ))
        .expect_err("timeout");
    assert_eq!(error.0, "Timed out waiting for the browser sign-in");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = runtime
        .block_on(serve_callback(
            &listener,
            CALLBACK_PATH,
            "s",
            Duration::from_secs(10),
            &cancellation,
        ))
        .expect_err("cancelled");
    assert_eq!(error.0, "Login cancelled");
    let taken = runtime
        .block_on(bind_callback("127.0.0.1", port))
        .expect_err("port is in use");
    assert!(
        taken
            .0
            .starts_with(&format!("could not listen on 127.0.0.1:{port}")),
        "{taken}"
    );
}
