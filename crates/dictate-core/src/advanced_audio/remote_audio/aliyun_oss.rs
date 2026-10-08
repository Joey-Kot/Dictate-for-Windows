use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use sha1::Sha1;
use tokio_util::sync::CancellationToken;

use super::{
    PublishedRemoteAudio, RemoteAudioError, RemoteAudioPublisher, RemoteAudioReference, append_key,
    object_key, published, streaming_body,
};
use crate::advanced_audio::http::PreparedAudio;
use crate::advanced_audio::schema::{AliyunOssRemoteAudioConfig, AudioDeliveryType};

const PRESIGN_EXPIRY_SECONDS: i64 = 900;
type HmacSha1 = Hmac<Sha1>;

/// Aliyun OSS has its own signing protocol and intentionally does not reuse
/// the S3-compatible implementation.
#[derive(Clone)]
pub struct AliyunOssPublisher {
    client: reqwest::Client,
    settings: AliyunOssRemoteAudioConfig,
}

#[derive(Clone)]
pub(super) struct AliyunOssObject {
    pub(super) settings: AliyunOssRemoteAudioConfig,
    pub(super) key: String,
}

impl AliyunOssPublisher {
    pub fn new(client: reqwest::Client, settings: AliyunOssRemoteAudioConfig) -> Self {
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
                    presign_get(&self.settings, &object_url, &key, Utc::now())?
                } else {
                    let base = self.settings.public_url_base.as_deref().ok_or_else(|| {
                        RemoteAudioError::Signing(
                            "OSS public_url_base is required when presigned mode is disabled"
                                .into(),
                        )
                    })?;
                    append_key(base, &key)?.to_string()
                };
                let parsed = reqwest::Url::parse(&url).map_err(|_| RemoteAudioError::InvalidUrl)?;
                if !parsed.scheme().eq_ignore_ascii_case("https") {
                    return Err(RemoteAudioError::Signing(
                        "OSS public URL must use HTTPS".into(),
                    ));
                }
                RemoteAudioReference::PublicHttpsUrl(url)
            }
            AudioDeliveryType::CloudUri => {
                RemoteAudioReference::OssUri(format!("oss://{}/{}", self.settings.bucket, key))
            }
            _ => {
                return Err(RemoteAudioError::Signing(
                    "OSS remote upload is only valid for public_https_url or cloud_uri delivery"
                        .into(),
                ));
            }
        };
        // Generate the externally visible reference before the PUT.  This
        // prevents an invalid public URL or signature configuration from
        // orphaning a remote object after a successful upload.
        let cleanup = super::RemoteCleanup::AliyunOss(AliyunOssObject {
            settings: self.settings.clone(),
            key: key.clone(),
        });
        upload(
            &self.client,
            &self.settings,
            &object_url,
            &key,
            audio,
            cancellation,
            &cleanup,
        )
        .await?;
        Ok(published(reference, cleanup))
    }
}

#[async_trait]
impl RemoteAudioPublisher for AliyunOssPublisher {
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
    settings: &AliyunOssRemoteAudioConfig,
    object_url: &reqwest::Url,
    key: &str,
    audio: &PreparedAudio,
    cancellation: &CancellationToken,
    cleanup: &super::RemoteCleanup,
) -> Result<(), RemoteAudioError> {
    let body = streaming_body(audio).await?;
    let now = Utc::now();
    let mut headers = HeaderMap::new();
    insert_header(&mut headers, reqwest::header::CONTENT_TYPE, audio.mime())?;
    insert_header(
        &mut headers,
        reqwest::header::CONTENT_LENGTH,
        &audio.size().to_string(),
    )?;
    sign_headers("PUT", &mut headers, settings, key, audio.mime(), now)?;
    let request = client.put(object_url.clone()).headers(headers).body(body);
    let result = tokio::select! {
        _ = cancellation.cancelled() => Err(RemoteAudioError::Canceled),
        response = request.send() => match response {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => Err(RemoteAudioError::UnexpectedStatus {
                operation: "OSS upload",
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
    object: &AliyunOssObject,
    force: bool,
) -> Result<(), RemoteAudioError> {
    if !force && !object.settings.delete_after_recognition {
        return Ok(());
    }
    let url = object_url(&object.settings, &object.key)?;
    let mut headers = HeaderMap::new();
    sign_headers(
        "DELETE",
        &mut headers,
        &object.settings,
        &object.key,
        "",
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
            operation: "OSS cleanup",
            status: response.status().as_u16(),
        })
    }
}

fn object_url(
    settings: &AliyunOssRemoteAudioConfig,
    key: &str,
) -> Result<reqwest::Url, RemoteAudioError> {
    let mut endpoint =
        reqwest::Url::parse(&settings.endpoint).map_err(|_| RemoteAudioError::InvalidUrl)?;
    let localhost_or_ip = endpoint.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok()
    });
    if localhost_or_ip {
        let bucket = super::encode_path_segment(&settings.bucket);
        let base = append_key(endpoint.as_str(), &bucket)?;
        return append_key(base.as_str(), key);
    }
    let host = endpoint
        .host_str()
        .ok_or_else(|| RemoteAudioError::Signing("OSS endpoint is missing a host".into()))?;
    if !host.eq_ignore_ascii_case(&settings.bucket)
        && !host
            .to_ascii_lowercase()
            .starts_with(&format!("{}.", settings.bucket.to_ascii_lowercase()))
    {
        endpoint
            .set_host(Some(&format!("{}.{}", settings.bucket, host)))
            .map_err(|_| RemoteAudioError::Signing("invalid OSS bucket host".into()))?;
    }
    append_key(endpoint.as_str(), key)
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
    headers: &mut HeaderMap,
    settings: &AliyunOssRemoteAudioConfig,
    key: &str,
    content_type: &str,
    now: DateTime<Utc>,
) -> Result<(), RemoteAudioError> {
    let date = now.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    insert_header(headers, reqwest::header::DATE, &date)?;
    let string_to_sign = format!(
        "{method}\n\n{content_type}\n{date}\n{}",
        canonical_resource(settings, key)
    );
    let signature = hmac_sha1_base64(&settings.secret_key, &string_to_sign)?;
    insert_header(
        headers,
        reqwest::header::AUTHORIZATION,
        &format!("OSS {}:{signature}", settings.access_key),
    )
}

fn presign_get(
    settings: &AliyunOssRemoteAudioConfig,
    object_url: &reqwest::Url,
    key: &str,
    now: DateTime<Utc>,
) -> Result<String, RemoteAudioError> {
    let expires = now.timestamp() + PRESIGN_EXPIRY_SECONDS;
    let string_to_sign = format!("GET\n\n\n{expires}\n{}", canonical_resource(settings, key));
    let signature = hmac_sha1_base64(&settings.secret_key, &string_to_sign)?;
    let mut url = object_url.clone();
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("OSSAccessKeyId", &settings.access_key);
        query.append_pair("Expires", &expires.to_string());
        query.append_pair("Signature", &signature);
    }
    Ok(url.to_string())
}

fn canonical_resource(settings: &AliyunOssRemoteAudioConfig, key: &str) -> String {
    format!("/{}/{}", settings.bucket, key)
}

fn hmac_sha1_base64(secret: &str, value: &str) -> Result<String, RemoteAudioError> {
    let mut mac = HmacSha1::new_from_slice(secret.as_bytes())
        .map_err(|error| RemoteAudioError::Signing(error.to_string()))?;
    mac.update(value.as_bytes());
    Ok(STANDARD.encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_oss_endpoint_uses_path_style_for_test_and_lan_servers() {
        let settings = AliyunOssRemoteAudioConfig {
            endpoint: "http://127.0.0.1:9000/oss".into(),
            bucket: "audio".into(),
            ..Default::default()
        };
        assert_eq!(
            object_url(&settings, "day/a.wav").unwrap().as_str(),
            "http://127.0.0.1:9000/oss/audio/day/a.wav"
        );
    }

    #[test]
    fn oss_canonical_resource_never_confuses_cloud_uri_with_https() {
        let settings = AliyunOssRemoteAudioConfig {
            bucket: "audio".into(),
            ..Default::default()
        };
        assert_eq!(
            canonical_resource(&settings, "day/a.wav"),
            "/audio/day/a.wav"
        );
    }
}
