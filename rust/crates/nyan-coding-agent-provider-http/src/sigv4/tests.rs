use super::{AwsCredentials, SignInput, amz_date, sign, uri_encode};

fn credentials(token: Option<&str>) -> AwsCredentials {
    AwsCredentials {
        access_key_id: "AKIDEXAMPLE".to_owned(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_owned(),
        session_token: token.map(str::to_owned),
    }
}

#[test]
fn the_aws_get_vanilla_test_vector_signs_to_the_published_signature() {
    let signed = sign(
        &credentials(None),
        &SignInput {
            method: "GET",
            region: "us-east-1",
            service: "service",
            host: "example.amazonaws.com",
            path: "/",
            content_type: None,
            body: b"",
            amz_date: "20150830T123600Z",
        },
    );
    assert_eq!(
        signed.authorization,
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
    );
}

#[test]
fn a_bedrock_shaped_post_matches_an_independent_python_implementation() {
    // Expected values come from /tmp/sigv4_reference.py, which uses hashlib and
    // hmac only, for the same inputs (see the test's inputs below).
    let signed = sign(
        &credentials(Some("session-token-value")),
        &SignInput {
            method: "POST",
            region: "us-west-2",
            service: "bedrock",
            host: "bedrock-runtime.us-west-2.amazonaws.com",
            path: "/model/anthropic.claude-3-5-sonnet-20241022-v2%3A0/converse-stream",
            content_type: Some("application/json"),
            body: br#"{"messages":[]}"#,
            amz_date: "20260101T000000Z",
        },
    );
    assert_eq!(signed.amz_date, "20260101T000000Z");
    assert_eq!(
        signed.authorization,
        include_str!("bedrock_expected.txt").trim()
    );
}

#[test]
fn unix_seconds_format_as_amz_dates_including_leap_days() {
    for (seconds, expected) in [
        (0, "19700101T000000Z"),
        (1_440_938_160, "20150830T123600Z"),
        (951_782_400, "20000229T000000Z"),
        (1_709_164_799, "20240228T235959Z"),
        (1_709_164_800, "20240229T000000Z"),
        (1_767_225_600, "20260101T000000Z"),
    ] {
        assert_eq!(amz_date(seconds), expected, "{seconds}");
    }
}

#[test]
fn uri_encoding_keeps_only_unreserved_characters() {
    assert_eq!(uri_encode("a-b_c.d~e"), "a-b_c.d~e");
    assert_eq!(uri_encode("a:b/c d"), "a%3Ab%2Fc%20d");
    assert_eq!(uri_encode("%3A"), "%253A");
}
