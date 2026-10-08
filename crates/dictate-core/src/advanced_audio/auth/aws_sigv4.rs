use super::signer::{
    RequestSigner, SignerError, SigningRequest, canonical_all_headers, canonical_request,
    hex_lower, hmac_sha256, nonempty_configuration, nonempty_credential, set_host_header,
    sha256_hex,
};

/// AWS Signature Version 4 header signer.
///
/// This signer owns its credentials so it can be reused for arbitrary Core
/// requests (including remote S3-compatible uploads) without exposing them in
/// public fields or debug output.
pub struct AwsSigV4Signer {
    region: String,
    service: String,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl AwsSigV4Signer {
    pub fn new(
        region: String,
        service: String,
        access_key: String,
        secret_key: String,
        session_token: Option<String>,
    ) -> Result<Self, SignerError> {
        let region = nonempty_configuration("AWS SigV4", "region", region)?;
        let service = nonempty_configuration("AWS SigV4", "service", service)?;
        let access_key = nonempty_credential("AWS SigV4", "access key", access_key)?;
        let secret_key = nonempty_credential("AWS SigV4", "secret key", secret_key)?;
        let session_token = session_token
            .map(|value| nonempty_credential("AWS SigV4", "session token", value))
            .transpose()?;
        Ok(Self {
            region,
            service,
            access_key,
            secret_key,
            session_token,
        })
    }

    pub fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
        let timestamp = request.timestamp().format("%Y%m%dT%H%M%SZ").to_string();
        let date = request.timestamp().format("%Y%m%d").to_string();
        let payload_hash = request.payload_sha256();

        set_host_header(request)?;
        request.set_signer_header("x-amz-date", &timestamp)?;
        request.set_signer_header("x-amz-content-sha256", &payload_hash)?;
        if let Some(session_token) = &self.session_token {
            request.set_signer_header("x-amz-security-token", session_token)?;
        }

        let canonical_headers = canonical_all_headers(request.headers())?;
        let canonical_request = canonical_request(request, &canonical_headers);
        let scope = format!("{date}/{}/{}/aws4_request", self.region, self.service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let signature = hex_lower(&signing_key(
            &self.secret_key,
            &date,
            &self.region,
            &self.service,
            &string_to_sign,
        ));
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
            self.access_key, canonical_headers.signed_names
        );
        request.set_signer_header("authorization", &authorization)
    }
}

impl RequestSigner for AwsSigV4Signer {
    fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
        Self::sign(self, request)
    }
}

fn signing_key(
    secret_key: &str,
    date: &str,
    region: &str,
    service: &str,
    string_to_sign: &str,
) -> [u8; 32] {
    let mut prefixed_secret = Vec::with_capacity(4 + secret_key.len());
    prefixed_secret.extend_from_slice(b"AWS4");
    prefixed_secret.extend_from_slice(secret_key.as_bytes());
    let date_key = hmac_sha256(&prefixed_secret, date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    let signing_key = hmac_sha256(&service_key, b"aws4_request");
    hmac_sha256(&signing_key, string_to_sign.as_bytes())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use reqwest::header::{HeaderMap, HeaderValue};
    use reqwest::{Method, Url};

    use super::*;
    use crate::advanced_audio::auth::signer::{canonical_all_headers, canonical_request};

    #[test]
    fn signs_aws_sigv4_fixed_vector_with_session_token() {
        let method = Method::POST;
        let url = Url::parse(
            "https://example.amazonaws.com:8443/a%20b/%7E?z=last&dup=b&dup=a&space=one+two",
        )
        .unwrap();
        let body = br#"{"message":"hello"}"#;
        let timestamp = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
        let signer = AwsSigV4Signer::new(
            "us-east-1".into(),
            "execute-api".into(),
            "AKIDEXAMPLE".into(),
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            Some("session-token-123".into()),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Type",
            HeaderValue::from_static(" application/json; charset=utf-8 "),
        );
        headers.insert("X-Custom", HeaderValue::from_static("\t alpha   beta \t"));

        let mut request = SigningRequest::new(&method, &url, &mut headers, body, timestamp);
        signer.sign(&mut request).unwrap();

        let canonical_headers = canonical_all_headers(request.headers()).unwrap();
        let canonical = canonical_request(&request, &canonical_headers);
        assert_eq!(
            canonical,
            "POST\n/a%20b/~\ndup=a&dup=b&space=one%2Btwo&z=last\ncontent-type:application/json; charset=utf-8\nhost:example.amazonaws.com:8443\nx-amz-content-sha256:9b2d43affbf49a367028df2e1414f84c0e099ac98c3d54a8a80157fd7771af25\nx-amz-date:20240102T030405Z\nx-amz-security-token:session-token-123\nx-custom:alpha beta\n\ncontent-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-custom\n9b2d43affbf49a367028df2e1414f84c0e099ac98c3d54a8a80157fd7771af25"
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n20240102T030405Z\n20240102/us-east-1/execute-api/aws4_request\n{}",
            sha256_hex(canonical.as_bytes())
        );
        assert_eq!(
            string_to_sign,
            "AWS4-HMAC-SHA256\n20240102T030405Z\n20240102/us-east-1/execute-api/aws4_request\n9cb4b836890bd2698ecbf717e8e803eb6ff061e94bd4b1703dbd416bf4d9b5aa"
        );
        assert_eq!(
            headers.get("x-amz-security-token").unwrap(),
            "session-token-123"
        );
        assert_eq!(
            headers.get("authorization").unwrap(),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240102/us-east-1/execute-api/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-custom, Signature=cd9812200d4fb80c4679363ebc5ccffd4266989077c4bf7b6a43aced489c4fdc"
        );
    }
}
