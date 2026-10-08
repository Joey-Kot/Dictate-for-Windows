use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, Url};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{AwsSigV4Signer, TencentTc3Signer};
use crate::advanced_audio::schema::SignerConfig;

type HmacSha256 = Hmac<Sha256>;

/// Signs one fully rendered HTTP request.
///
/// Callers must finalize the URL, request headers, body bytes and content type
/// before invoking a signer.  In particular, a signer cannot correctly sign a
/// streaming body whose exact bytes are unavailable.
pub trait RequestSigner: Send + Sync {
    fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError>;
}

/// Request data used by the built-in signing algorithms.
///
/// The signer only mutates `headers`; it never changes the request URL or
/// payload.  The payload can be supplied as exact bytes or as a previously
/// computed SHA-256 digest when a caller has already materialized it elsewhere.
pub struct SigningRequest<'a> {
    method: &'a Method,
    url: &'a Url,
    headers: &'a mut HeaderMap,
    payload: SigningPayload<'a>,
    timestamp: DateTime<Utc>,
}

enum SigningPayload<'a> {
    Bytes(&'a [u8]),
    Sha256(String),
}

impl<'a> SigningRequest<'a> {
    /// Creates a request whose SHA-256 payload digest will be calculated from
    /// these exact bytes.
    pub fn new(
        method: &'a Method,
        url: &'a Url,
        headers: &'a mut HeaderMap,
        body: &'a [u8],
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            method,
            url,
            headers,
            payload: SigningPayload::Bytes(body),
            timestamp,
        }
    }

    /// Creates a request from a precomputed SHA-256 payload digest.
    ///
    /// This is intended for callers which have already computed the digest
    /// while materializing a large request body.  The digest is normalized to
    /// lower-case hexadecimal before it is placed in signer headers.
    pub fn with_payload_hash(
        method: &'a Method,
        url: &'a Url,
        headers: &'a mut HeaderMap,
        payload_sha256: impl Into<String>,
        timestamp: DateTime<Utc>,
    ) -> Result<Self, SignerError> {
        let payload_sha256 = payload_sha256.into();
        if !is_sha256_hex(&payload_sha256) {
            return Err(SignerError::InvalidPayloadHash);
        }
        Ok(Self {
            method,
            url,
            headers,
            payload: SigningPayload::Sha256(payload_sha256.to_ascii_lowercase()),
            timestamp,
        })
    }

    pub fn method(&self) -> &Method {
        self.method
    }

    pub fn url(&self) -> &Url {
        self.url
    }

    pub fn headers(&self) -> &HeaderMap {
        self.headers
    }

    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        self.headers
    }

    pub fn timestamp(&self) -> &DateTime<Utc> {
        &self.timestamp
    }

    /// Returns the lower-case hexadecimal SHA-256 digest that will be signed.
    pub fn payload_sha256(&self) -> String {
        match &self.payload {
            SigningPayload::Bytes(bytes) => sha256_hex(bytes),
            SigningPayload::Sha256(value) => value.clone(),
        }
    }

    pub(crate) fn set_signer_header(
        &mut self,
        name: &'static str,
        value: &str,
    ) -> Result<(), SignerError> {
        let value = HeaderValue::from_str(value)
            .map_err(|_| SignerError::InvalidGeneratedHeader { name })?;
        self.headers.insert(HeaderName::from_static(name), value);
        Ok(())
    }
}

/// The Core-owned signer selected by a workflow's finite `SignerConfig`.
///
/// Credential fields deliberately stay private and this type does not
/// implement `Debug`, so normal diagnostics cannot accidentally emit them.
pub struct BuiltinRequestSigner {
    inner: BuiltinRequestSignerKind,
}

enum BuiltinRequestSignerKind {
    None,
    AwsSigV4(AwsSigV4Signer),
    TencentTc3(TencentTc3Signer),
}

impl BuiltinRequestSigner {
    /// Resolves the workflow's declared secret IDs against the runtime secret
    /// map.  Errors identify only the credential role, never its value.
    pub fn from_config(
        config: &SignerConfig,
        secrets: &BTreeMap<String, String>,
    ) -> Result<Self, SignerError> {
        let inner = match config {
            SignerConfig::None => BuiltinRequestSignerKind::None,
            SignerConfig::AwsSigv4 {
                region,
                service,
                access_key_secret,
                secret_key_secret,
                session_token_secret,
            } => {
                let access_key =
                    configured_secret(secrets, "AWS SigV4", "access key", access_key_secret)?;
                let secret_key =
                    configured_secret(secrets, "AWS SigV4", "secret key", secret_key_secret)?;
                let session_token = session_token_secret
                    .as_deref()
                    .map(|secret_id| {
                        configured_secret(secrets, "AWS SigV4", "session token", secret_id)
                    })
                    .transpose()?;
                BuiltinRequestSignerKind::AwsSigV4(AwsSigV4Signer::new(
                    region.clone(),
                    service.clone(),
                    access_key,
                    secret_key,
                    session_token,
                )?)
            }
            SignerConfig::TencentTc3 {
                service,
                secret_id_secret,
                secret_key_secret,
            } => {
                let secret_id =
                    configured_secret(secrets, "Tencent TC3", "secret ID", secret_id_secret)?;
                let secret_key =
                    configured_secret(secrets, "Tencent TC3", "secret key", secret_key_secret)?;
                BuiltinRequestSignerKind::TencentTc3(TencentTc3Signer::new(
                    service.clone(),
                    secret_id,
                    secret_key,
                )?)
            }
        };
        Ok(Self { inner })
    }

    pub fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
        match &self.inner {
            BuiltinRequestSignerKind::None => Ok(()),
            BuiltinRequestSignerKind::AwsSigV4(signer) => signer.sign(request),
            BuiltinRequestSignerKind::TencentTc3(signer) => signer.sign(request),
        }
    }
}

impl RequestSigner for BuiltinRequestSigner {
    fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
        Self::sign(self, request)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SignerError {
    #[error("{signer} {credential} credential is missing")]
    MissingCredential {
        signer: &'static str,
        credential: &'static str,
    },
    #[error("{signer} {credential} credential is empty")]
    EmptyCredential {
        signer: &'static str,
        credential: &'static str,
    },
    #[error("{signer} signer configuration field '{field}' is empty")]
    EmptyConfiguration {
        signer: &'static str,
        field: &'static str,
    },
    #[error("request URL does not contain a host")]
    MissingHost,
    #[error("request contains a header value that is not valid text")]
    NonTextHeaderValue,
    #[error("could not set generated signer header '{name}'")]
    InvalidGeneratedHeader { name: &'static str },
    #[error("payload digest must be a 64-character SHA-256 hexadecimal value")]
    InvalidPayloadHash,
}

pub(crate) struct CanonicalHeaders {
    pub(crate) value: String,
    pub(crate) signed_names: String,
}

pub(crate) fn canonical_request(
    request: &SigningRequest<'_>,
    canonical_headers: &CanonicalHeaders,
) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method().as_str(),
        canonical_uri(request.url()),
        canonical_query(request.url()),
        canonical_headers.value,
        canonical_headers.signed_names,
        request.payload_sha256(),
    )
}

pub(crate) fn canonical_all_headers(headers: &HeaderMap) -> Result<CanonicalHeaders, SignerError> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for (name, value) in headers {
        if name.as_str().eq_ignore_ascii_case("authorization") {
            continue;
        }
        let value = value
            .to_str()
            .map_err(|_| SignerError::NonTextHeaderValue)?;
        grouped
            .entry(name.as_str().to_ascii_lowercase())
            .or_default()
            .push(normalize_header_value(value));
    }
    canonicalize_headers(grouped)
}

pub(crate) fn canonical_selected_headers(
    headers: &HeaderMap,
    names: &[&str],
) -> Result<CanonicalHeaders, SignerError> {
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for name in names {
        let values = headers.get_all(*name);
        if values.iter().next().is_none() {
            continue;
        }
        let values = values
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(normalize_header_value)
                    .map_err(|_| SignerError::NonTextHeaderValue)
            })
            .collect::<Result<Vec<_>, _>>()?;
        grouped.insert((*name).to_ascii_lowercase(), values);
    }
    canonicalize_headers(grouped)
}

fn canonicalize_headers(
    grouped: BTreeMap<String, Vec<String>>,
) -> Result<CanonicalHeaders, SignerError> {
    if grouped.is_empty() {
        return Err(SignerError::MissingHost);
    }
    let mut value = String::new();
    let mut names = Vec::with_capacity(grouped.len());
    for (name, values) in grouped {
        value.push_str(&name);
        value.push(':');
        value.push_str(&values.join(","));
        value.push('\n');
        names.push(name);
    }
    Ok(CanonicalHeaders {
        value,
        signed_names: names.join(";"),
    })
}

pub(crate) fn set_host_header(request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
    let host = request.url().host_str().ok_or(SignerError::MissingHost)?;
    let value = match request.url().port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    request.set_signer_header("host", &value)
}

pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac =
        HmacSha256::new_from_slice(key).expect("HMAC-SHA256 accepts keys of every length");
    mac.update(data);
    let output = mac.finalize().into_bytes();
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&output);
    bytes
}

pub(crate) fn sha256_hex(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    hex_lower(&digest)
}

pub(crate) fn hex_lower(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(input.len() * 2);
    for byte in input {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn canonical_uri(url: &Url) -> String {
    let path = percent_decode(url.path().as_bytes());
    if path.is_empty() {
        "/".into()
    } else {
        uri_encode(&path, true)
    }
}

pub(crate) fn canonical_query(url: &Url) -> String {
    let Some(query) = url.query() else {
        return String::new();
    };
    let mut pairs = query
        .split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                uri_encode(&percent_decode(key.as_bytes()), false),
                uri_encode(&percent_decode(value.as_bytes()), false),
            )
        })
        .collect::<Vec<_>>();
    pairs.sort_unstable();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn configured_secret(
    secrets: &BTreeMap<String, String>,
    signer: &'static str,
    credential: &'static str,
    secret_id: &str,
) -> Result<String, SignerError> {
    let Some(value) = secrets.get(secret_id) else {
        return Err(SignerError::MissingCredential { signer, credential });
    };
    if value.trim().is_empty() {
        return Err(SignerError::EmptyCredential { signer, credential });
    }
    Ok(value.clone())
}

pub(crate) fn nonempty_configuration(
    signer: &'static str,
    field: &'static str,
    value: String,
) -> Result<String, SignerError> {
    if value.trim().is_empty() {
        return Err(SignerError::EmptyConfiguration { signer, field });
    }
    Ok(value)
}

pub(crate) fn nonempty_credential(
    signer: &'static str,
    credential: &'static str,
    value: String,
) -> Result<String, SignerError> {
    if value.trim().is_empty() {
        return Err(SignerError::EmptyCredential { signer, credential });
    }
    Ok(value)
}

fn normalize_header_value(value: &str) -> String {
    value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%'
            && index + 2 < input.len()
            && let (Some(high), Some(low)) =
                (hex_value(input[index + 1]), hex_value(input[index + 2]))
        {
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    output
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn uri_encode(input: &[u8], preserve_slash: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(input.len());
    for byte in input {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else if preserve_slash && *byte == b'/' {
            output.push('/');
        } else {
            output.push('%');
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use reqwest::header::{HeaderMap, HeaderValue};
    use reqwest::{Method, Url};

    use super::*;
    use crate::advanced_audio::schema::SignerConfig;

    #[test]
    fn canonical_query_sorts_encodes_and_keeps_duplicates() {
        let url = Url::parse(
            "https://example.test/a%20b?z=last&dup=b&dup=a&space=one+two&slash=%2F&unicode=%E2%98%83",
        )
        .unwrap();

        assert_eq!(canonical_uri(&url), "/a%20b");
        assert_eq!(
            canonical_query(&url),
            "dup=a&dup=b&slash=%2F&space=one%2Btwo&unicode=%E2%98%83&z=last"
        );
    }

    #[test]
    fn canonical_headers_lowercase_normalize_and_exclude_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Test", HeaderValue::from_static("\t alpha   beta \t"));
        headers.append("x-test", HeaderValue::from_static("second"));
        headers.insert("Authorization", HeaderValue::from_static("do-not-sign"));
        headers.insert("Host", HeaderValue::from_static("example.test"));

        let canonical = canonical_all_headers(&headers).unwrap();
        assert_eq!(
            canonical.value,
            "host:example.test\nx-test:alpha beta,second\n"
        );
        assert_eq!(canonical.signed_names, "host;x-test");
    }

    #[test]
    fn predefined_payload_hash_is_validated_and_normalized() {
        let method = Method::PUT;
        let url = Url::parse("https://example.test/").unwrap();
        let mut headers = HeaderMap::new();
        let digest = "A".repeat(64);
        let request = SigningRequest::with_payload_hash(
            &method,
            &url,
            &mut headers,
            digest,
            Utc.timestamp_opt(0, 0).single().unwrap(),
        )
        .unwrap();
        assert_eq!(request.payload_sha256(), "a".repeat(64));
        let error = SigningRequest::with_payload_hash(
            &method,
            &url,
            &mut HeaderMap::new(),
            "not-a-digest",
            Utc.timestamp_opt(0, 0).single().unwrap(),
        )
        .err()
        .unwrap();
        assert_eq!(
            error.to_string(),
            "payload digest must be a 64-character SHA-256 hexadecimal value"
        );
    }

    #[test]
    fn config_secret_lookup_is_safe_and_none_does_not_mutate() {
        let mut headers = HeaderMap::new();
        headers.insert("x-original", HeaderValue::from_static("unchanged"));
        let original = headers.clone();
        let method = Method::GET;
        let url = Url::parse("https://example.test/").unwrap();
        let mut request = SigningRequest::new(
            &method,
            &url,
            &mut headers,
            b"",
            Utc.timestamp_opt(0, 0).single().unwrap(),
        );
        BuiltinRequestSigner::from_config(&SignerConfig::None, &BTreeMap::new())
            .unwrap()
            .sign(&mut request)
            .unwrap();
        assert_eq!(request.headers(), &original);

        let config = SignerConfig::AwsSigv4 {
            region: "us-east-1".into(),
            service: "execute-api".into(),
            access_key_secret: "access-key-id".into(),
            secret_key_secret: "secret-key-id".into(),
            session_token_secret: Some("session-token-id".into()),
        };
        let missing = BuiltinRequestSigner::from_config(&config, &BTreeMap::new())
            .err()
            .unwrap()
            .to_string();
        assert!(missing.contains("AWS SigV4 access key credential is missing"));
        assert!(!missing.contains("access-key-id"));

        let mut secrets = BTreeMap::new();
        secrets.insert("access-key-id".into(), "actual-access-key-value".into());
        secrets.insert("secret-key-id".into(), "   ".into());
        let empty = BuiltinRequestSigner::from_config(&config, &secrets)
            .err()
            .unwrap()
            .to_string();
        assert!(empty.contains("AWS SigV4 secret key credential is empty"));
        assert!(!empty.contains("actual-access-key-value"));
        assert!(!empty.contains("secret-key-id"));
    }

    #[test]
    fn signing_errors_do_not_expose_credentials_body_or_header_values() {
        let signer = AwsSigV4Signer::new(
            "us-east-1".into(),
            "execute-api".into(),
            "access-secret-value".into(),
            "key-secret-value".into(),
            Some("token-secret-value".into()),
        )
        .unwrap();
        let method = Method::POST;
        let url = Url::parse("https://example.test/").unwrap();
        let body = b"body-secret-value";
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-opaque",
            HeaderValue::from_bytes(b"header-secret-value\xff").unwrap(),
        );
        let mut request = SigningRequest::new(
            &method,
            &url,
            &mut headers,
            body,
            Utc.timestamp_opt(0, 0).single().unwrap(),
        );

        let error = signer.sign(&mut request).err().unwrap().to_string();
        assert_eq!(
            error,
            "request contains a header value that is not valid text"
        );
        for secret in [
            "access-secret-value",
            "key-secret-value",
            "token-secret-value",
            "body-secret-value",
            "header-secret-value",
        ] {
            assert!(!error.contains(secret));
        }
    }
}
