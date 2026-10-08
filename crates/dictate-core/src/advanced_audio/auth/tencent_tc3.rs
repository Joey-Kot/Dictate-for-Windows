use super::signer::{
    RequestSigner, SignerError, SigningRequest, canonical_request, canonical_selected_headers,
    hex_lower, hmac_sha256, nonempty_configuration, nonempty_credential, set_host_header,
    sha256_hex,
};

/// Tencent Cloud TC3-HMAC-SHA256 header signer.
pub struct TencentTc3Signer {
    service: String,
    secret_id: String,
    secret_key: String,
}

impl TencentTc3Signer {
    pub fn new(
        service: String,
        secret_id: String,
        secret_key: String,
    ) -> Result<Self, SignerError> {
        Ok(Self {
            service: nonempty_configuration("Tencent TC3", "service", service)?,
            secret_id: nonempty_credential("Tencent TC3", "secret ID", secret_id)?,
            secret_key: nonempty_credential("Tencent TC3", "secret key", secret_key)?,
        })
    }

    pub fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
        let timestamp = request.timestamp().timestamp().to_string();
        let date = request.timestamp().format("%Y-%m-%d").to_string();
        set_host_header(request)?;
        request.set_signer_header("x-tc-timestamp", &timestamp)?;

        let header_names = if request.headers().contains_key("content-type") {
            &["content-type", "host"][..]
        } else {
            &["host"][..]
        };
        let canonical_headers = canonical_selected_headers(request.headers(), header_names)?;
        let canonical_request = canonical_request(request, &canonical_headers);
        let scope = format!("{date}/{}/tc3_request", self.service);
        let string_to_sign = format!(
            "TC3-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let signature = hex_lower(&signing_key(
            &self.secret_key,
            &date,
            &self.service,
            &string_to_sign,
        ));
        let authorization = format!(
            "TC3-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
            self.secret_id, canonical_headers.signed_names
        );
        request.set_signer_header("authorization", &authorization)
    }
}

impl RequestSigner for TencentTc3Signer {
    fn sign(&self, request: &mut SigningRequest<'_>) -> Result<(), SignerError> {
        Self::sign(self, request)
    }
}

fn signing_key(secret_key: &str, date: &str, service: &str, string_to_sign: &str) -> [u8; 32] {
    let mut prefixed_secret = Vec::with_capacity(3 + secret_key.len());
    prefixed_secret.extend_from_slice(b"TC3");
    prefixed_secret.extend_from_slice(secret_key.as_bytes());
    let date_key = hmac_sha256(&prefixed_secret, date.as_bytes());
    let service_key = hmac_sha256(&date_key, service.as_bytes());
    let signing_key = hmac_sha256(&service_key, b"tc3_request");
    hmac_sha256(&signing_key, string_to_sign.as_bytes())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use reqwest::header::{HeaderMap, HeaderValue};
    use reqwest::{Method, Url};

    use super::*;
    use crate::advanced_audio::auth::signer::{canonical_request, canonical_selected_headers};

    #[test]
    fn signs_tc3_fixed_vector() {
        let method = Method::POST;
        let url = Url::parse("https://cvm.tencentcloudapi.com/?z=last&dup=b&dup=a").unwrap();
        let body = br#"{"Limit":1}"#;
        let timestamp = Utc.with_ymd_and_hms(2019, 2, 25, 16, 44, 25).unwrap();
        let signer =
            TencentTc3Signer::new("cvm".into(), "AKIDEXAMPLE".into(), "test-secret-key".into())
                .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Type",
            HeaderValue::from_static(" application/json; charset=utf-8 "),
        );
        headers.insert("X-TC-Action", HeaderValue::from_static("DescribeInstances"));
        headers.insert("X-TC-Version", HeaderValue::from_static("2017-03-12"));

        let mut request = SigningRequest::new(&method, &url, &mut headers, body, timestamp);
        signer.sign(&mut request).unwrap();

        let canonical_headers =
            canonical_selected_headers(request.headers(), &["content-type", "host"]).unwrap();
        let canonical = canonical_request(&request, &canonical_headers);
        assert_eq!(
            canonical,
            "POST\n/\ndup=a&dup=b&z=last\ncontent-type:application/json; charset=utf-8\nhost:cvm.tencentcloudapi.com\n\ncontent-type;host\n55522f708dcfebccb7bd3e8d0001a53ecaf2beca9ca801f1e9161e24215faa99"
        );
        let string_to_sign = format!(
            "TC3-HMAC-SHA256\n1551113065\n2019-02-25/cvm/tc3_request\n{}",
            sha256_hex(canonical.as_bytes())
        );
        assert_eq!(
            string_to_sign,
            "TC3-HMAC-SHA256\n1551113065\n2019-02-25/cvm/tc3_request\n20b5343a9ab401ddaad2951035af1608d0333dd8020277cbe89011aa63d58133"
        );
        assert_eq!(headers.get("x-tc-timestamp").unwrap(), "1551113065");
        assert_eq!(
            headers.get("authorization").unwrap(),
            "TC3-HMAC-SHA256 Credential=AKIDEXAMPLE/2019-02-25/cvm/tc3_request, SignedHeaders=content-type;host, Signature=c69f8f90d66a7c61beaaf99c3a84420f03bac07e2d94b5b46f719edcfd005b0a"
        );
    }
}
