use async_trait::async_trait;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    PublishedRemoteAudio, RemoteAudioError, RemoteAudioPublisher, RemoteAudioReference, append_key,
    object_key, published, streaming_body,
};
use crate::advanced_audio::http::PreparedAudio;
use crate::advanced_audio::schema::AudioDeliveryType;
use crate::advanced_audio::schema::WebDavRemoteAudioConfig;

/// WebDAV's upload endpoint and independently configured public download URL
/// are deliberately kept separate. A private DAV endpoint is not assumed to
/// be reachable by an ASR service.
#[derive(Clone)]
pub struct WebDavPublisher {
    client: reqwest::Client,
    settings: WebDavRemoteAudioConfig,
}

#[derive(Clone)]
pub(super) struct WebDavObject {
    pub(super) upload_url: reqwest::Url,
    pub(super) username: String,
    pub(super) password: String,
    pub(super) delete_after_recognition: bool,
}

impl WebDavPublisher {
    pub fn new(client: reqwest::Client, settings: WebDavRemoteAudioConfig) -> Self {
        Self { client, settings }
    }

    pub(super) async fn publish_for_delivery(
        &self,
        audio: &PreparedAudio,
        delivery: AudioDeliveryType,
        cancellation: &CancellationToken,
        upload_debug: bool,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError> {
        if delivery != AudioDeliveryType::PublicHttpsUrl {
            return Err(RemoteAudioError::Signing(
                "WebDAV can publish only a public HTTPS URL".into(),
            ));
        }
        self.publish_with_debug(audio, cancellation, upload_debug)
            .await
    }

    async fn publish_with_debug(
        &self,
        audio: &PreparedAudio,
        cancellation: &CancellationToken,
        upload_debug: bool,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError> {
        let started = Instant::now();
        super::debug(
            upload_debug,
            format_args!(
                "[upload] remote-audio provider=webdav event=upload-start method=PUT bytes={}",
                audio.size()
            ),
        );
        let result = self.publish_inner(audio, cancellation, upload_debug).await;
        match &result {
            Ok(_) => super::debug(
                upload_debug,
                format_args!(
                    "[upload] remote-audio provider=webdav event=upload-complete elapsed_ms={}",
                    started.elapsed().as_millis()
                ),
            ),
            Err(error) => super::debug(
                upload_debug,
                format_args!(
                    "[upload] remote-audio provider=webdav event=upload-failed kind={} elapsed_ms={}",
                    remote_error_kind(error),
                    started.elapsed().as_millis()
                ),
            ),
        }
        result
    }

    async fn publish_inner(
        &self,
        audio: &PreparedAudio,
        cancellation: &CancellationToken,
        upload_debug: bool,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError> {
        let key = object_key(&self.settings.remote_path_prefix, audio.filename());
        let upload_url = append_key(&self.settings.upload_base_url, &key)?;
        let public_url = append_key(&self.settings.public_download_base_url, &key)?;
        if !public_url.scheme().eq_ignore_ascii_case("https") {
            return Err(RemoteAudioError::Signing(
                "WebDAV public download URL must use HTTPS".into(),
            ));
        }
        let body = streaming_body(audio).await?;
        let cleanup = super::RemoteCleanup::WebDav(WebDavObject {
            upload_url: upload_url.clone(),
            username: self.settings.username.clone(),
            password: self.settings.password.clone(),
            delete_after_recognition: self.settings.delete_after_recognition,
        });
        let request = self
            .client
            .put(upload_url)
            .basic_auth(&self.settings.username, Some(&self.settings.password))
            .header(reqwest::header::CONTENT_TYPE, audio.mime())
            .header(reqwest::header::CONTENT_LENGTH, audio.size())
            .body(body);
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(RemoteAudioError::Canceled),
            response = request.send() => match response {
                Ok(response) => {
                    let status = response.status();
                    super::debug(
                        upload_debug,
                        format_args!(
                            "[upload] remote-audio provider=webdav event=response-headers status={}",
                            status.as_u16()
                        ),
                    );
                    if status.is_success() {
                        Ok(())
                    } else {
                        Err(RemoteAudioError::UnexpectedStatus {
                            operation: "WebDAV upload",
                            status: status.as_u16(),
                        })
                    }
                }
                Err(_) => Err(RemoteAudioError::Request),
            },
        };
        match result {
            Ok(()) => Ok(published(
                RemoteAudioReference::PublicHttpsUrl(public_url.into()),
                cleanup,
            )),
            Err(error) => {
                super::cleanup_after_upload_failure(&self.client, &cleanup, upload_debug).await;
                Err(error)
            }
        }
    }
}

#[async_trait]
impl RemoteAudioPublisher for WebDavPublisher {
    async fn publish(
        &self,
        audio: &PreparedAudio,
        cancellation: &CancellationToken,
    ) -> Result<PublishedRemoteAudio, RemoteAudioError> {
        self.publish_inner(audio, cancellation, false).await
    }
}

fn remote_error_kind(error: &RemoteAudioError) -> &'static str {
    match error {
        RemoteAudioError::Canceled => "canceled",
        RemoteAudioError::UnexpectedStatus { .. } => "http",
        RemoteAudioError::Request => "network",
        _ => "configuration",
    }
}

pub(super) async fn cleanup(
    client: &reqwest::Client,
    object: &WebDavObject,
    force: bool,
) -> Result<(), RemoteAudioError> {
    if !force && !object.delete_after_recognition {
        return Ok(());
    }
    let response = client
        .delete(object.upload_url.clone())
        .basic_auth(&object.username, Some(&object.password))
        .send()
        .await
        .map_err(|_| RemoteAudioError::Request)?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(RemoteAudioError::UnexpectedStatus {
            operation: "WebDAV cleanup",
            status: response.status().as_u16(),
        })
    }
}
