use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

use super::{
    PublishedRemoteAudio, RemoteAudioError, RemoteAudioPublisher, RemoteAudioReference, append_key,
    object_key, published, streaming_body,
};
use crate::advanced_audio::http::PreparedAudio;
use crate::advanced_audio::schema::{AudioDeliveryType, S3RemoteAudioConfig};

const PRESIGN_EXPIRY_SECONDS: i64 = 900;
const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
type HmacSha256 = Hmac<Sha256>;

/// S3-compatible storage. It uses the bounded, standard SigV4 protocol
/// directly rather than pulling a provider SDK into the application.
#[derive(Clone)]
pub struct S3CompatiblePublisher {
    client: reqwest::Client,
    settings: S3RemoteAudioConfig,
}

#[derive(Clone)]
pub(super) struct S3Object {
    pub(super) settings: S3RemoteAudioConfig,
    pub(super) key: String,
}

impl S3CompatiblePublisher {
    pub fn new(client: reqwest::Client, settings: S3RemoteAudioConfig) -> Self {
        Self { client, settings }
    }

    pub(super) async fn publish_for_delivery(
        &self,
        audio: &PreparedAudio,
        delivery: AudioDeliveryType,
        cancellation: &CancellationToken,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError> {
        let key = object_key(&self.settings.prefix, audio.filename());
        let object_url = object_url(&self.settings, &key)?;
        let reference = match delivery {
            AudioDeliveryType::PublicHttpsUrl => {
                let url = if self.settings.presigned {
                    presign_get(&self.settings, &object_url, Utc::now())?
                } else {
                    let base = self.settings.public_url_base.as_deref().ok_or_else(|| {
                        RemoteAudioError::Signing(
                            "S3 public_url_base is required when presigned mode is disabled".into(),
                        )
                    })?;
                    append_key(base, &key)?.to_string()
                };
                let parsed = reqwest::Url::parse(&url).map_err(|_| RemoteAudioError::InvalidUrl)?;
                if !parsed.scheme().eq_ignore_ascii_case("https") {
                    return Err(RemoteAudioError::Signing(
                        "S3 public URL must use HTTPS".into(),
                    ));
                }
                RemoteAudioReference::PublicHttpsUrl(url)
            }
            AudioDeliveryType::CloudUri => {
                RemoteAudioReference::S3Uri(format!("s3://{}/{}", self.settings.bucket, key))
            }
            _ => {
                return Err(RemoteAudioError::Signing(
                    "S3 remote upload is only valid for public_https_url or cloud_uri delivery"
                        .into(),
                ));
            }
        };
        // Resolve and validate the reference before writing an object.  A
        // malformed public base URL or signing configuration must not leave
        // an uploaded object behind merely because reference construction
        // failed afterwards.
        let cleanup = super::RemoteCleanup::S3(S3Object {
            settings: self.settings.clone(),
            key: key.clone(),
        });
        upload(
            &self.client,
            &self.settings,
            &object_url,
            audio,
            cancellation,
            &cleanup,
        )
        .await?;
        Ok(published(reference, cleanup))
    }
}

#[async_trait]
impl RemoteAudioPublisher for S3CompatiblePublisher {
    async fn publish(
        &self,
        audio: &PreparedAudio,
        cancellation: &CancellationToken,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError> {
        self.publish_for_delivery(audio, AudioDeliveryType::CloudUri, cancellation)
            .await
    }
}

async fn upload(
    client: &reqwest::Client,
    settings: &S3RemoteAudioConfig,
    object_url: &reqwest::Url,
    audio: &PreparedAudio,
    cancellation: &CancellationToken,
    cleanup: &super::RemoteCleanup,
) -> Result<(), RemoteAudioError> {
    let body = streaming_body(audio).await?;
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, reqwest::header::CONTENT_TYPE, audio.mime())?;
    insert_header(
        &mut headers,
        reqwest::header::CONTENT_LENGTH,
        &audio.size().to_string(),
    )?;
    sign_headers(
        "PUT",
        object_url,
        &mut headers,
        settings,
        UNSIGNED_PAYLOAD,
        Utc::now(),
    )?;
    let request = client.put(object_url.clone()).headers(headers).body(body);
    let result = tokio::select! {
        _ = cancellation.cancelled() => Err(RemoteAudioError::Canceled),
        response = request.send() => match response {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => Err(RemoteAudioError::UnexpectedStatus {
                operation: "S3 upload",
                status: response.status().as_u16(),
            }),
            Err(_) => Err(RemoteAudioError::Request),
        },
    };
    if result.is_err() {
        super::cleanup_after_upload_failure(client, cleanup).await;
    }
    result
}

pub(super) async fn cleanup(
    client: &reqwest::Client,
    object: &S3Object,
    force: bool,
) -> Result<(), RemoteAudioError> {
    if !force && !object.settings.delete_after_recognition {
        return Ok(());
    }
    let url = object_url(&object.settings, &object.key)?;
    let mut headers = HeaderMap::new();
    sign_headers(
        "DELETE",
        &url,
        &mut headers,
        &object.settings,
        UNSIGNED_PAYLOAD,
        Utc::now(),
    )?;
    let response = client
        .delete(url)
        .headers(headers)
        .send()
        .await
        .map_err(|_| RemoteAudioError::Request)?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(RemoteAudioError::UnexpectedStatus {
            operation: "S3 cleanup",
            status: response.status().as_u16(),
        })
    }
}

fn object_url(settings: &S3RemoteAudioConfig, key: &str) -> Result<reqwest::Url, RemoteAudioError> {
    let base = append_key(&settings.endpoint, &encode_bucket(&settings.bucket))?;
    append_key(base.as_str(), key)
}

fn encode_bucket(bucket: &str) -> String {
    // Bucket names are constrained by config validation, but retain an
    // explicit percent-encoding boundary when constructing a path-style URL.
    super::encode_path_segment(bucket)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: HeaderName,
    value: &str,
) -> Result<(), RemoteAudioError> {
    let value = HeaderValue::from_str(value)
        .map_err(|error| RemoteAudioError::Signing(format!("invalid header value: {error}")))?;
    headers.insert(name, value);
    Ok(())
}

fn sign_headers(
    method: &str,
    url: &reqwest::Url,
    headers: &mut HeaderMap,
    settings: &S3RemoteAudioConfig,
    payload_hash: &str,
    now: DateTime<Utc>,
) -> Result<(), RemoteAudioError> {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    insert_header(headers, HeaderName::from_static("host"), &host_header(url))?;
    insert_header(headers, HeaderName::from_static("x-amz-date"), &amz_date)?;
    insert_header(
        headers,
        HeaderName::from_static("x-amz-content-sha256"),
        payload_hash,
    )?;

    let (canonical_headers, signed_headers) = canonical_headers(headers)?;
    let canonical_request = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        canonical_uri(url),
        canonical_query(url),
    );
    let scope = format!("{date}/{}/s3/aws4_request", settings.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let signing_key = signing_key(&settings.secret_key, &date, &settings.region, "s3")?;
    let signature = hex_hmac(&signing_key, string_to_sign.as_bytes())?;
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        settings.access_key
    );
    insert_header(
        headers,
        HeaderName::from_static("authorization"),
        &authorization,
    )
}

fn presign_get(
    settings: &S3RemoteAudioConfig,
    object_url: &reqwest::Url,
    now: DateTime<Utc>,
) -> Result<String, RemoteAudioError> {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let scope = format!("{date}/{}/s3/aws4_request", settings.region);
    let mut values = BTreeMap::new();
    for (name, value) in object_url.query_pairs() {
        values.insert(name.into_owned(), value.into_owned());
    }
    values.insert("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into());
    values.insert(
        "X-Amz-Credential".into(),
        format!("{}/{scope}", settings.access_key),
    );
    values.insert("X-Amz-Date".into(), amz_date.clone());
    values.insert("X-Amz-Expires".into(), PRESIGN_EXPIRY_SECONDS.to_string());
    values.insert("X-Amz-SignedHeaders".into(), "host".into());
    let canonical_query = canonical_pairs(&values);
    let canonical_request = format!(
        "GET\n{}\n{canonical_query}\nhost:{}\n\nhost\n{UNSIGNED_PAYLOAD}",
        canonical_uri(object_url),
        host_header(object_url),
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let signing_key = signing_key(&settings.secret_key, &date, &settings.region, "s3")?;
    values.insert(
        "X-Amz-Signature".into(),
        hex_hmac(&signing_key, string_to_sign.as_bytes())?,
    );
    let mut signed = object_url.clone();
    signed.set_query(Some(&canonical_pairs(&values)));
    Ok(signed.to_string())
}

fn canonical_headers(headers: &HeaderMap) -> Result<(String, String), RemoteAudioError> {
    let mut values = BTreeMap::<String, String>::new();
    for (name, value) in headers {
        if name == reqwest::header::AUTHORIZATION {
            continue;
        }
        let value = value
            .to_str()
            .map_err(|error| RemoteAudioError::Signing(format!("non-text header: {error}")))?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        values.insert(name.as_str().to_ascii_lowercase(), value);
    }
    let canonical = values
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let signed = values.keys().cloned().collect::<Vec<_>>().join(";");
    Ok((canonical, signed))
}

fn canonical_uri(url: &reqwest::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".into()
    } else {
        path.split('/')
            .map(aws_encode_path_segment)
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// `Url::path()` is already percent-encoded.  Preserve its escape triplets
/// while normalizing their hex case so a key such as `voice%20note.wav` is
/// signed as that path, not as `voice%2520note.wav`.
fn aws_encode_path_segment(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut encoded = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
            index += 1;
        } else if byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            encoded.push('%');
            encoded.push((bytes[index + 1] as char).to_ascii_uppercase());
            encoded.push((bytes[index + 2] as char).to_ascii_uppercase());
            index += 3;
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
            index += 1;
        }
    }
    encoded
}

fn canonical_query(url: &reqwest::Url) -> String {
    let values = url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect::<BTreeMap<_, _>>();
    canonical_pairs(&values)
}

fn canonical_pairs(values: &BTreeMap<String, String>) -> String {
    values
        .iter()
        .map(|(name, value)| format!("{}={}", aws_encode(name), aws_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn aws_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn host_header(url: &reqwest::Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().into(),
    }
}

fn signing_key(
    secret: &str,
    date: &str,
    region: &str,
    service: &str,
) -> Result<Vec<u8>, RemoteAudioError> {
    let date_key = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes())?;
    let region_key = hmac(&date_key, region.as_bytes())?;
    let service_key = hmac(&region_key, service.as_bytes())?;
    hmac(&service_key, b"aws4_request")
}

fn hmac(key: &[u8], message: &[u8]) -> Result<Vec<u8>, RemoteAudioError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| RemoteAudioError::Signing(error.to_string()))?;
    mac.update(message);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_hmac(key: &[u8], message: &[u8]) -> Result<String, RemoteAudioError> {
    Ok(hex(&hmac(key, message)?))
}

fn hex_sha256(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_canonical_encoding_is_rfc3986_not_form_encoding() {
        assert_eq!(aws_encode("a b/+"), "a%20b%2F%2B");
    }

    #[test]
    fn object_url_uses_path_style_for_compatible_endpoints() {
        let settings = S3RemoteAudioConfig {
            endpoint: "https://s3.example.test/root".into(),
            bucket: "audio".into(),
            ..Default::default()
        };
        assert_eq!(
            object_url(&settings, "a/b.wav").unwrap().as_str(),
            "https://s3.example.test/root/audio/a/b.wav"
        );
    }

    #[test]
    fn canonical_uri_preserves_existing_path_escapes_once() {
        let url = reqwest::Url::parse("https://s3.example.test/audio/voice%20note/%E4%B8%AD.wav")
            .unwrap();
        assert_eq!(canonical_uri(&url), "/audio/voice%20note/%E4%B8%AD.wav");
    }
}
