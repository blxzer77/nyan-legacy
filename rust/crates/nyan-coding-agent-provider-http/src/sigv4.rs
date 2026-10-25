//! AWS Signature Version 4 request signing for a JSON POST.
//!
//! Only what Bedrock needs: a request without a query string, signed over
//! `host`, `x-amz-date`, an optional `content-type`, and (with temporary
//! credentials) `x-amz-security-token`. The path is signed double-encoded, as
//! every AWS service except S3 expects.

use std::fmt::Write as _;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Long-lived or temporary AWS credentials.
#[derive(Clone, Eq, PartialEq)]
pub struct AwsCredentials {
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Session token of temporary credentials.
    pub session_token: Option<String>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AwsCredentials")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Percent-encodes everything except the RFC 3986 unreserved characters.
#[must_use]
pub fn uri_encode(value: &str) -> String {
    use std::fmt::Write as _;
    value.bytes().fold(String::new(), |mut text, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            text.push(char::from(byte));
        } else {
            let _ = write!(text, "%{byte:02X}");
        }
        text
    })
}

/// Formats Unix seconds as the `YYYYMMDDTHHMMSSZ` form `SigV4` uses.
#[must_use]
pub fn amz_date(unix_seconds: u64) -> String {
    let days = i64::try_from(unix_seconds / 86_400).unwrap_or(0);
    let seconds = unix_seconds % 86_400;
    // Civil-from-days (Howard Hinnant), valid for the whole proleptic Gregorian calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

/// The headers a signed request must carry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signed {
    /// `x-amz-date`.
    pub amz_date: String,
    /// `authorization`.
    pub authorization: String,
}

/// What to sign.
#[derive(Clone, Debug)]
pub struct SignInput<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// Region, for example `us-east-1`.
    pub region: &'a str,
    /// Service signing name, for example `bedrock`.
    pub service: &'a str,
    /// The `host` header value.
    pub host: &'a str,
    /// The already single-encoded request path.
    pub path: &'a str,
    /// A signed `content-type`, when the request has one.
    pub content_type: Option<&'a str>,
    /// The request body.
    pub body: &'a [u8],
    /// `YYYYMMDDTHHMMSSZ`.
    pub amz_date: &'a str,
}

/// Signs a request without a query string.
#[must_use]
pub fn sign(credentials: &AwsCredentials, input: &SignInput<'_>) -> Signed {
    let amz_date = input.amz_date;
    let date = &amz_date[..8];
    let canonical_path = input
        .path
        .split('/')
        .map(uri_encode)
        .collect::<Vec<_>>()
        .join("/");
    let mut headers = vec![
        ("host", input.host.to_owned()),
        ("x-amz-date", amz_date.to_owned()),
    ];
    if let Some(content_type) = input.content_type {
        headers.push(("content-type", content_type.to_owned()));
    }
    if let Some(token) = &credentials.session_token {
        headers.push(("x-amz-security-token", token.clone()));
    }
    headers.sort_by(|left, right| left.0.cmp(right.0));
    let canonical_headers = headers
        .iter()
        .fold(String::new(), |mut text, (name, value)| {
            let _ = writeln!(text, "{name}:{}", value.trim());
            text
        });
    let signed_headers = headers
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let payload_hash = hex(&Sha256::digest(input.body));
    let canonical_request = format!(
        "{}\n{canonical_path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        input.method
    );
    let scope = format!("{date}/{}/{}/aws4_request", input.region, input.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical_request.as_bytes()))
    );
    let mut key = hmac(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    for part in [input.region, input.service, "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));
    Signed {
        amz_date: amz_date.to_owned(),
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key_id
        ),
    }
}

#[cfg(test)]
mod tests;
