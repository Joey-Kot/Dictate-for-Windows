//! Remote-audio publishing for workflows that pass an URL or cloud URI.
//!
//! Remote hosting is deliberately outside the workflow document. A workflow
//! only declares whether it needs a public HTTPS URL or a cloud URI; storage
//! credentials remain in the user's Advanced Audio API settings.

mod aliyun_oss;
mod s3;
mod webdav;

use async_trait::async_trait;
use std::fmt;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::advanced_audio::http::PreparedAudio;
use crate::advanced_audio::schema::{AudioDeliveryType, RemoteAudioConfig};

pub use aliyun_oss::AliyunOssPublisher;
pub use s3::S3CompatiblePublisher;
pub use webdav::WebDavPublisher;

/// The remote location made available to a recognition workflow.
#[derive(Clone, PartialEq, Eq)]
pub enum RemoteAudioReference {
    /// A URL an ASR service can fetch directly.
    PublicHttpsUrl(String),
    /// An S3-style cloud URI whose semantics are distinct from HTTPS.
    S3Uri(String),
    /// An Aliyun OSS cloud URI whose semantics are distinct from HTTPS.
    OssUri(String),
}

impl fmt::Debug for RemoteAudioReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::PublicHttpsUrl(_) => "PublicHttpsUrl",
            Self::S3Uri(_) => "S3Uri",
            Self::OssUri(_) => "OssUri",
        };
        formatter.debug_tuple(kind).field(&"<redacted>").finish()
    }
}

impl RemoteAudioReference {
    pub fn public_https_url(&self) -> Option<&str> {
        match self {
            Self::PublicHttpsUrl(url) => Some(url),
            Self::S3Uri(_) | Self::OssUri(_) => None,
        }
    }

    pub fn cloud_uri(&self) -> Option<&str> {
        match self {
            Self::S3Uri(uri) | Self::OssUri(uri) => Some(uri),
            Self::PublicHttpsUrl(_) => None,
        }
    }
}

/// A completed upload plus the opaque cleanup operation associated with it.
#[derive(Clone)]
pub struct PublishedRemoteAudio {
    reference: RemoteAudioReference,
    cleanup: RemoteCleanup,
}

impl fmt::Debug for PublishedRemoteAudio {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedRemoteAudio")
            .field("reference", &self.reference)
            .field("cleanup", &"<redacted>")
            .finish()
    }
}

impl PublishedRemoteAudio {
    pub fn reference(&self) -> &RemoteAudioReference {
        &self.reference
    }

    /// Adds the remote values to the existing local prepared-audio metadata.
    pub fn attach_to(&self, audio: PreparedAudio) -> PreparedAudio {
        audio.with_remote_reference(
            self.reference.public_https_url().map(str::to_owned),
            self.reference.cloud_uri().map(str::to_owned),
        )
    }
}

#[derive(Clone)]
enum RemoteCleanup {
    WebDav(webdav::WebDavObject),
    S3(s3::S3Object),
    AliyunOss(aliyun_oss::AliyunOssObject),
}

/// A bounded Core-owned publisher abstraction. It intentionally exposes no
/// provider-specific workflow surface.
#[async_trait]
pub trait RemoteAudioPublisher: Send + Sync {
    async fn publish(
        &self,
        audio: &PreparedAudio,
        cancellation: &CancellationToken,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError>;
}

/// Uploads a prepared recording according to the persisted remote-audio
/// configuration. The caller owns the later best-effort cleanup lifecycle.
pub async fn publish_remote_audio(
    client: &reqwest::Client,
    config: &RemoteAudioConfig,
    delivery: AudioDeliveryType,
    audio: &PreparedAudio,
    cancellation: &CancellationToken,
) -> Result<PublishedRemoteAudio, RemoteAudioError> {
    match config {
        RemoteAudioConfig::None => Err(RemoteAudioError::NotConfigured),
        RemoteAudioConfig::Webdav(settings) => {
            WebDavPublisher::new(client.clone(), settings.clone())
                .publish_for_delivery(audio, delivery, cancellation)
                .await
        }
        RemoteAudioConfig::S3Compatible(settings) => {
            S3CompatiblePublisher::new(client.clone(), settings.clone())
                .publish_for_delivery(audio, delivery, cancellation)
                .await
        }
        RemoteAudioConfig::AliyunOss(settings) => {
            AliyunOssPublisher::new(client.clone(), settings.clone())
                .publish_for_delivery(audio, delivery, cancellation)
                .await
        }
    }
}

/// Cleanup is intentionally best-effort. It is invoked after the entire
/// recognition workflow finishes (including polling/result fetch), not after
/// upload or submit. A failed or canceled recognition forces cleanup even when
/// the user elected to retain objects after successful recognition. Failure to
/// delete must not replace a transcription or a cancellation outcome.
pub async fn cleanup_remote_audio(
    client: &reqwest::Client,
    published: &PublishedRemoteAudio,
    force: bool,
) {
    cleanup_best_effort(
        client,
        &published.cleanup,
        force,
        "[remote-audio] cleanup failed",
    )
    .await;
}

/// A failed or canceled upload may still have created the object remotely.
/// The caller retains its original upload error regardless of this cleanup.
async fn cleanup_after_upload_failure(client: &reqwest::Client, cleanup: &RemoteCleanup) {
    // `delete_after_recognition` governs retention after a completed
    // recognition workflow. An upload that failed or was canceled never
    // reached recognition, so always try to remove its possibly-created
    // object.
    cleanup_best_effort(
        client,
        cleanup,
        true,
        "[remote-audio] upload cleanup failed",
    )
    .await;
}

async fn cleanup_best_effort(
    client: &reqwest::Client,
    cleanup: &RemoteCleanup,
    force: bool,
    failure_message: &'static str,
) {
    let result = match cleanup {
        RemoteCleanup::WebDav(object) => webdav::cleanup(client, object, force).await,
        RemoteCleanup::S3(object) => s3::cleanup(client, object, force).await,
        RemoteCleanup::AliyunOss(object) => aliyun_oss::cleanup(client, object, force).await,
    };
    if result.is_err() {
        // `reqwest::Error` may include the endpoint URL, including a
        // presigned query string.  Do not format the error here: this helper
        // has no Config available for central redaction, and cleanup is
        // intentionally best-effort anyway.
        crate::debug_log::write(
            crate::debug_log::Category::Upload,
            format_args!("{failure_message}"),
        );
    }
}

fn published(reference: RemoteAudioReference, cleanup: RemoteCleanup) -> PublishedRemoteAudio {
    PublishedRemoteAudio { reference, cleanup }
}

/// Creates a privacy-preserving remote object key. The original local
/// filename is not uploaded; only a harmless extension is retained.
pub(crate) fn object_key(prefix: &str, filename: &str) -> String {
    let prefix = prefix
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .map(encode_path_segment)
        .collect::<Vec<_>>();
    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 16
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_default();
    let filename = format!("{}{}", uuid::Uuid::new_v4().simple(), extension);
    prefix
        .into_iter()
        .chain(std::iter::once(filename))
        .collect::<Vec<_>>()
        .join("/")
}

/// Percent-encodes one URL path segment. Slashes are handled by callers as
/// separators, never as user-controlled filename bytes.
pub(crate) fn encode_path_segment(value: &str) -> String {
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

pub(crate) fn append_key(base: &str, key: &str) -> Result<reqwest::Url, RemoteAudioError> {
    let mut url = reqwest::Url::parse(base).map_err(|_| RemoteAudioError::InvalidUrl)?;
    let existing = url.path().trim_end_matches('/');
    let path = if existing.is_empty() || existing == "/" {
        format!("/{key}")
    } else {
        format!("{existing}/{key}")
    };
    url.set_path(&path);
    Ok(url)
}

pub(crate) async fn streaming_body(
    audio: &PreparedAudio,
) -> Result<reqwest::Body, RemoteAudioError> {
    let file = tokio::fs::File::open(audio.path())
        .await
        .map_err(RemoteAudioError::OpenAudio)?;
    Ok(reqwest::Body::wrap_stream(
        tokio_util::io::ReaderStream::new(file),
    ))
}

#[derive(Debug, Error)]
pub enum RemoteAudioError {
    #[error("remote audio hosting is not configured")]
    NotConfigured,
    // URLs can be presigned and `reqwest::Error` retains the full URL.  Keep
    // this variant opaque so neither ordinary display nor an error-source
    // chain can disclose a query credential.
    #[error("invalid remote audio URL")]
    InvalidUrl,
    #[error("failed to open audio for remote upload: {0}")]
    OpenAudio(#[source] std::io::Error),
    #[error("remote audio request failed")]
    Request,
    #[error("remote audio {operation} returned HTTP {status}")]
    UnexpectedStatus {
        operation: &'static str,
        status: u16,
    },
    #[error("remote audio request was canceled")]
    Canceled,
    #[error("remote audio signing failed: {0}")]
    Signing(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        use tokio::io::AsyncReadExt;

        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let read = stream.read(&mut buffer).await.unwrap();
            assert!(
                read > 0,
                "fake server connection closed before request completed"
            );
            request.extend_from_slice(&buffer[..read]);
            let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            if request.len() >= header_end + 4 + content_length {
                return request;
            }
        }
    }

    fn request_parts(request: &[u8]) -> (String, String, Vec<u8>) {
        let header_end = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
        let target = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap()
            .to_owned();
        (target, headers, request[header_end + 4..].to_vec())
    }

    async fn start_put_then_delete_server(
        put_response: &'static [u8],
        delete_response: &'static [u8],
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<(Vec<u8>, Vec<u8>)>,
    ) {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut upload, _) = listener.accept().await.unwrap();
            let upload_request = read_request(&mut upload).await;
            upload.write_all(put_response).await.unwrap();

            let (mut delete, _) = listener.accept().await.unwrap();
            let delete_request = read_request(&mut delete).await;
            delete.write_all(delete_response).await.unwrap();
            (upload_request, delete_request)
        });
        (address, server)
    }

    async fn temporary_audio() -> (tempfile::TempDir, PreparedAudio) {
        let directory = tempfile::tempdir().unwrap();
        let local_file = directory.path().join("private meeting with Alice.wav");
        std::fs::write(&local_file, b"recording bytes").unwrap();
        let audio = PreparedAudio::from_path(&local_file, Some("audio/wav"))
            .await
            .unwrap();
        (directory, audio)
    }

    fn assert_upload_then_delete(upload_request: &[u8], delete_request: &[u8]) {
        let (upload_target, upload_headers, upload_body) = request_parts(upload_request);
        let (delete_target, delete_headers, delete_body) = request_parts(delete_request);

        assert!(upload_headers.starts_with("PUT "));
        assert_eq!(delete_headers.split_whitespace().next(), Some("DELETE"));
        assert_eq!(upload_target, delete_target);
        assert_eq!(upload_body, b"recording bytes");
        assert!(delete_body.is_empty());
    }

    #[test]
    fn object_keys_are_path_safe_and_do_not_leak_local_names() {
        let key = object_key("recordings/../today", "John Doe.wav");
        assert!(key.starts_with("recordings/today/"));
        assert!(key.ends_with(".wav"));
        assert!(!key.contains("John"));
        assert!(!key.contains(".."));
    }

    #[test]
    fn path_join_keeps_base_path_and_encodes_only_key_segments() {
        let url = append_key("https://storage.example/root", "a/b%20c.wav").unwrap();
        assert_eq!(url.as_str(), "https://storage.example/root/a/b%20c.wav");
    }

    #[test]
    fn invalid_remote_url_error_does_not_echo_embedded_credentials() {
        let error = append_key("https://upload-user:secret-token@[", "audio.wav").unwrap_err();
        assert_eq!(error.to_string(), "invalid remote audio URL");
        assert!(!error.to_string().contains("secret-token"));
    }

    #[tokio::test]
    async fn webdav_fake_server_uploads_anonymous_key_and_cleans_up_after_recognition() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut upload, _) = listener.accept().await.unwrap();
            let upload_request = read_request(&mut upload).await;
            upload
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();

            let (mut delete, _) = listener.accept().await.unwrap();
            let delete_request = read_request(&mut delete).await;
            delete
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            (upload_request, delete_request)
        });

        let directory = tempfile::tempdir().unwrap();
        let local_file = directory.path().join("private meeting with Alice.wav");
        std::fs::write(&local_file, b"recording bytes").unwrap();
        let audio = PreparedAudio::from_path(&local_file, Some("audio/wav"))
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let settings = crate::advanced_audio::schema::WebDavRemoteAudioConfig {
            upload_base_url: format!("http://{address}/private-dav"),
            username: "upload-user".into(),
            password: "upload-password".into(),
            remote_path_prefix: "dictate/2026".into(),
            // This deliberately differs from the DAV endpoint: it is the
            // externally reachable location handed to the ASR workflow.
            public_download_base_url: "https://downloads.example.test/recognition".into(),
            delete_after_recognition: true,
        };
        let published = publish_remote_audio(
            &client,
            &crate::advanced_audio::schema::RemoteAudioConfig::Webdav(settings),
            AudioDeliveryType::PublicHttpsUrl,
            &audio,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let public_url = published.reference().public_https_url().unwrap().to_owned();
        cleanup_remote_audio(&client, &published, false).await;

        let (upload_request, delete_request) = server.await.unwrap();
        let (upload_target, upload_headers, upload_body) = request_parts(&upload_request);
        let (delete_target, delete_headers, delete_body) = request_parts(&delete_request);

        assert!(upload_headers.starts_with("PUT "));
        assert!(
            upload_headers
                .to_ascii_lowercase()
                .contains("authorization: basic ")
        );
        assert_eq!(upload_body, b"recording bytes");
        assert_eq!(delete_headers.split_whitespace().next(), Some("DELETE"));
        assert!(
            delete_headers
                .to_ascii_lowercase()
                .contains("authorization: basic ")
        );
        assert!(delete_body.is_empty());
        assert_eq!(upload_target, delete_target);
        assert!(upload_target.starts_with("/private-dav/dictate/2026/"));
        assert!(upload_target.ends_with(".wav"));
        let remote_name = upload_target.rsplit('/').next().unwrap();
        let uuid = remote_name.strip_suffix(".wav").unwrap();
        assert_eq!(uuid.len(), 32);
        assert!(uuid.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!remote_name.contains("private"));
        assert!(!remote_name.contains("Alice"));
        let remote_key_path = upload_target.strip_prefix("/private-dav").unwrap();
        assert_eq!(
            public_url,
            format!("https://downloads.example.test/recognition{remote_key_path}")
        );
    }

    #[tokio::test]
    async fn webdav_upload_failure_forces_delete_and_preserves_the_upload_error() {
        const SERVER_ERROR: &[u8] =
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (address, server) = start_put_then_delete_server(SERVER_ERROR, SERVER_ERROR).await;
        let (_directory, audio) = temporary_audio().await;
        let client = reqwest::Client::new();
        let settings = crate::advanced_audio::schema::WebDavRemoteAudioConfig {
            upload_base_url: format!("http://{address}/private-dav"),
            username: "upload-user".into(),
            password: "upload-password".into(),
            remote_path_prefix: "dictate/2026".into(),
            public_download_base_url: "https://downloads.example.test/recognition".into(),
            // Failed uploads must be deleted even when successful recordings
            // would normally be retained.
            delete_after_recognition: false,
        };

        let error = publish_remote_audio(
            &client,
            &RemoteAudioConfig::Webdav(settings),
            AudioDeliveryType::PublicHttpsUrl,
            &audio,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RemoteAudioError::UnexpectedStatus {
                operation: "WebDAV upload",
                status: 500,
            }
        ));
        let (upload_request, delete_request) = server.await.unwrap();
        assert_upload_then_delete(&upload_request, &delete_request);
    }

    #[tokio::test]
    async fn webdav_upload_cancellation_after_put_forces_delete() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (put_received, put_received_waiter) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut upload, _) = listener.accept().await.unwrap();
            let upload_request = read_request(&mut upload).await;
            put_received.send(()).unwrap();

            let (mut delete, _) = listener.accept().await.unwrap();
            let delete_request = read_request(&mut delete).await;
            delete
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            (upload_request, delete_request)
        });
        let (_directory, audio) = temporary_audio().await;
        let client = reqwest::Client::new();
        let settings = crate::advanced_audio::schema::WebDavRemoteAudioConfig {
            upload_base_url: format!("http://{address}/private-dav"),
            username: "upload-user".into(),
            password: "upload-password".into(),
            remote_path_prefix: "dictate/2026".into(),
            public_download_base_url: "https://downloads.example.test/recognition".into(),
            delete_after_recognition: false,
        };
        let cancellation = CancellationToken::new();
        let publish_cancellation = cancellation.clone();
        let publish = tokio::spawn(async move {
            publish_remote_audio(
                &client,
                &RemoteAudioConfig::Webdav(settings),
                AudioDeliveryType::PublicHttpsUrl,
                &audio,
                &publish_cancellation,
            )
            .await
        });

        put_received_waiter.await.unwrap();
        cancellation.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(2), publish)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();

        assert!(matches!(error, RemoteAudioError::Canceled));
        let (upload_request, delete_request) =
            tokio::time::timeout(std::time::Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap();
        assert_upload_then_delete(&upload_request, &delete_request);
    }

    #[tokio::test]
    async fn s3_upload_failure_forces_delete() {
        const SERVER_ERROR: &[u8] =
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        const NO_CONTENT: &[u8] =
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (address, server) = start_put_then_delete_server(SERVER_ERROR, NO_CONTENT).await;
        let (_directory, audio) = temporary_audio().await;
        let client = reqwest::Client::new();
        let settings = crate::advanced_audio::schema::S3RemoteAudioConfig {
            endpoint: format!("http://{address}/storage"),
            region: "us-east-1".into(),
            bucket: "audio".into(),
            access_key: "s3-access-key".into(),
            secret_key: "s3-secret-key".into(),
            prefix: "dictate/2026".into(),
            delete_after_recognition: false,
            ..Default::default()
        };

        let error = publish_remote_audio(
            &client,
            &RemoteAudioConfig::S3Compatible(settings),
            AudioDeliveryType::CloudUri,
            &audio,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RemoteAudioError::UnexpectedStatus {
                operation: "S3 upload",
                status: 500,
            }
        ));
        let (upload_request, delete_request) = server.await.unwrap();
        assert_upload_then_delete(&upload_request, &delete_request);
    }

    #[tokio::test]
    async fn oss_upload_failure_forces_delete() {
        const SERVER_ERROR: &[u8] =
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        const NO_CONTENT: &[u8] =
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (address, server) = start_put_then_delete_server(SERVER_ERROR, NO_CONTENT).await;
        let (_directory, audio) = temporary_audio().await;
        let client = reqwest::Client::new();
        let settings = crate::advanced_audio::schema::AliyunOssRemoteAudioConfig {
            endpoint: format!("http://{address}/oss"),
            bucket: "audio".into(),
            access_key: "oss-access-key".into(),
            secret_key: "oss-secret-key".into(),
            prefix: "dictate/2026".into(),
            delete_after_recognition: false,
            ..Default::default()
        };

        let error = publish_remote_audio(
            &client,
            &RemoteAudioConfig::AliyunOss(settings),
            AudioDeliveryType::CloudUri,
            &audio,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RemoteAudioError::UnexpectedStatus {
                operation: "OSS upload",
                status: 500,
            }
        ));
        let (upload_request, delete_request) = server.await.unwrap();
        assert_upload_then_delete(&upload_request, &delete_request);
    }

    #[test]
    fn remote_audio_debug_output_redacts_urls_and_credentials() {
        let reference = RemoteAudioReference::PublicHttpsUrl(
            "https://downloads.example.test/audio?X-Amz-Signature=presigned-token".into(),
        );
        let published = published(
            reference.clone(),
            RemoteCleanup::WebDav(webdav::WebDavObject {
                upload_url: reqwest::Url::parse(
                    "https://upload-user:upload-password@storage.example.test/audio?token=upload-token",
                )
                .unwrap(),
                username: "upload-user".into(),
                password: "upload-password".into(),
                delete_after_recognition: true,
            }),
        );
        let reference_debug = format!("{reference:?}");
        let published_debug = format!("{published:?}");

        for secret in [
            "presigned-token",
            "upload-user",
            "upload-password",
            "upload-token",
        ] {
            assert!(!reference_debug.contains(secret));
            assert!(!published_debug.contains(secret));
        }
    }
}
