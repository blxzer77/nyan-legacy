use super::{
    CLIENT_ID, DeviceCode, REDIRECT_URI, account_id, authorize_url, base64_decode,
    base64url_encode, challenge_for, device_redirect_uri, generate_pkce, parse_authorization_input,
    random_state,
};

#[test]
fn base64url_matches_the_rfc_4648_vectors_without_padding() {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg"),
        ("fo", "Zm8"),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg"),
        ("fooba", "Zm9vYmE"),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64url_encode(plain.as_bytes()), encoded, "{plain}");
        assert_eq!(
            base64_decode(encoded).as_deref(),
            Some(plain.as_bytes()),
            "{encoded}"
        );
    }
    assert_eq!(
        base64url_encode(&[0xfb, 0xff, 0xfe]),
        "-__-",
        "url alphabet, not + and /"
    );
    assert_eq!(
        base64_decode("+//+").as_deref(),
        Some(&[0xfb_u8, 0xff, 0xfe][..]),
        "both alphabets decode"
    );
    assert_eq!(
        base64_decode("Zm9v====").as_deref(),
        Some(&b"foo"[..]),
        "padding is tolerated"
    );
    assert_eq!(base64_decode("Zm9v!"), None);
}

#[test]
fn the_pkce_challenge_matches_the_rfc_7636_appendix_b_vector() {
    assert_eq!(
        challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn generated_secrets_have_the_documented_shape_and_differ_every_time() {
    let first = generate_pkce().expect("pkce");
    let second = generate_pkce().expect("pkce");
    assert_eq!(first.verifier.len(), 43, "32 bytes in base64url");
    assert!(
        first
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
    assert_eq!(first.challenge, challenge_for(&first.verifier));
    assert_ne!(first.verifier, second.verifier);
    let state = random_state().expect("state");
    assert_eq!(state.len(), 32);
    assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(state, random_state().expect("state"));
}

#[test]
fn the_authorize_url_carries_every_parameter_the_codex_flow_needs() {
    let url = reqwest::Url::parse(
        &authorize_url("https://auth.openai.com/", "CHAL", "STATE", "nyan").expect("valid base"),
    )
    .expect("a URL");
    assert_eq!(
        url.origin().ascii_serialization(),
        "https://auth.openai.com"
    );
    assert_eq!(url.path(), "/oauth/authorize");
    let params: std::collections::BTreeMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    for (name, value) in [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", "CHAL"),
        ("code_challenge_method", "S256"),
        ("state", "STATE"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "nyan"),
    ] {
        assert_eq!(params.get(name).map(String::as_str), Some(value), "{name}");
    }
    assert_eq!(params.len(), 10);
}

#[test]
fn pasted_input_is_understood_in_every_form_the_typescript_cli_accepts() {
    let some = |code: &str, state: Option<&str>| (Some(code.to_owned()), state.map(str::to_owned));
    for (input, expected) in [
        (
            "http://localhost:1455/auth/callback?code=abc&state=xyz",
            some("abc", Some("xyz")),
        ),
        (
            "  http://localhost:1455/auth/callback?code=abc  ",
            some("abc", None),
        ),
        ("abc#xyz", some("abc", Some("xyz"))),
        ("code=abc&state=xyz", some("abc", Some("xyz"))),
        ("justacode", some("justacode", None)),
        ("", (None, None)),
        ("   ", (None, None)),
    ] {
        assert_eq!(parse_authorization_input(input), expected, "{input:?}");
    }
}

fn jwt(claims: &serde_json::Value) -> String {
    format!(
        "{}.{}.signature",
        base64url_encode(br#"{"alg":"none"}"#),
        base64url_encode(claims.to_string().as_bytes())
    )
}

#[test]
fn the_account_id_comes_from_the_documented_claim_and_nowhere_else() {
    let token =
        jwt(&serde_json::json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct-9"}}));
    assert_eq!(account_id(&token).as_deref(), Some("acct-9"));
    for bad in [
        jwt(&serde_json::json!({"https://api.openai.com/auth": {}})),
        jwt(&serde_json::json!({"https://api.openai.com/auth": {"chatgpt_account_id": ""}})),
        jwt(&serde_json::json!({"chatgpt_account_id": "acct-9"})),
        "not-a-jwt".to_owned(),
        "a.b".to_owned(),
        "a.!!!.c".to_owned(),
    ] {
        assert_eq!(account_id(&bad), None, "{bad}");
    }
}

#[test]
fn device_urls_derive_from_the_auth_base() {
    assert_eq!(
        DeviceCode::verification_url("https://auth.openai.com/"),
        "https://auth.openai.com/codex/device"
    );
    assert_eq!(
        device_redirect_uri("https://auth.openai.com"),
        "https://auth.openai.com/deviceauth/callback"
    );
}

#[test]
fn an_invalid_sign_in_base_is_an_error_not_a_panic() {
    assert!(authorize_url("not a url", "c", "s", "nyan").is_err());
}
