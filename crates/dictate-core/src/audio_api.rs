//! Unified runtime entry point for Legacy and Advanced Audio API workflows.

use std::future::Future;
use std::path::Path;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::advanced_audio::LiveChunkSource;
use crate::advanced_audio::schema::RealtimeAudioStream;
use crate::advanced_audio::{AdvancedAudioClient, AdvancedAudioError};
use crate::asr::{AsrClient, AsrError, Transcription};

/// The runtime chooses exactly one client from config.  Frontends do not need
/// to know whether a transcription uses multipart, polling, SSE, or another
/// advanced protocol primitive.
#[derive(Clone)]
pub enum AudioApiClient {
    Legacy(Box<AsrClient>),
    Advanced(Box<AdvancedAudioClient>),
}

impl AudioApiClient {
    pub fn new(config: Config) -> Result<Self, AudioApiError> {
        if config.advanced_audio_api.enabled {
            Ok(Self::Advanced(Box::new(AdvancedAudioClient::new(config)?)))
        } else {
            Ok(Self::Legacy(Box::new(AsrClient::new(config)?)))
        }
    }

    pub fn is_advanced(&self) -> bool {
        matches!(self, Self::Advanced(_))
    }

    pub fn is_realtime_workflow(&self) -> bool {
        matches!(self, Self::Advanced(client) if client.is_realtime_workflow())
    }

    pub fn realtime_audio_stream(&self) -> Option<RealtimeAudioStream> {
        match self {
            Self::Advanced(client) => client.realtime_audio_stream(),
            Self::Legacy(_) => None,
        }
    }

    /// Runs a live realtime Advanced workflow from the recorder packet tee.
    /// Legacy ASR intentionally has no equivalent path.
    pub async fn transcribe_live(
        &self,
        cancellation: &CancellationToken,
        source: &mut LiveChunkSource,
    ) -> Result<Transcription, AudioApiError> {
        match self {
            Self::Advanced(client) => client
                .transcribe_live(cancellation, source)
                .await
                .map_err(AudioApiError::Advanced),
            Self::Legacy(_) => Err(AudioApiError::LiveRealtimeUnavailable),
        }
    }

    pub async fn transcribe(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<Transcription, AudioApiError> {
        match self {
            Self::Legacy(client) => client
                .transcribe(cancellation, file_path)
                .await
                .map_err(AudioApiError::Legacy),
            Self::Advanced(client) => client
                .transcribe(cancellation, file_path)
                .await
                .map_err(AudioApiError::Advanced),
        }
    }

    /// Preserves Legacy retry/reconversion semantics. Advanced workflows own
    /// their safe retry policy in Core: submit is not replayed, while request,
    /// poll, and read-only stages can retry independently.
    pub async fn transcribe_with_retry_prepare<F, Fut>(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
        prepare: F,
    ) -> Result<Transcription, AudioApiError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<(), AsrError>>,
    {
        match self {
            Self::Legacy(client) => client
                .transcribe_with_retry_prepare(cancellation, file_path, prepare)
                .await
                .map_err(AudioApiError::Legacy),
            Self::Advanced(client) => client
                .transcribe(cancellation, file_path)
                .await
                .map_err(AudioApiError::Advanced),
        }
    }

    pub async fn test_connection_cancellable(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<(), AudioApiError> {
        match self {
            Self::Legacy(client) => client
                .test_connection_cancellable(cancellation, file_path)
                .await
                .map_err(AudioApiError::Legacy),
            Self::Advanced(client) => client
                .test_workflow(cancellation, file_path)
                .await
                .map_err(AudioApiError::Advanced),
        }
    }
}

#[derive(Debug, Error)]
pub enum AudioApiError {
    #[error("{0}")]
    Legacy(#[from] AsrError),
    #[error("{0}")]
    Advanced(#[from] AdvancedAudioError),
    #[error("live realtime transcription requires an Advanced realtime workflow")]
    LiveRealtimeUnavailable,
}

impl AudioApiError {
    pub fn is_retry_exhausted(&self) -> bool {
        matches!(self, Self::Legacy(error) if error.is_retry_exhausted())
    }

    pub fn last_response(&self) -> &[u8] {
        match self {
            Self::Legacy(error) => error.last_response(),
            Self::Advanced(error) => error.last_response(),
            Self::LiveRealtimeUnavailable => &[],
        }
    }

    pub fn is_canceled(&self) -> bool {
        matches!(self, Self::Legacy(AsrError::Canceled))
            || matches!(self, Self::Advanced(error) if error.is_canceled())
    }

    pub fn is_text_extraction_error(&self) -> bool {
        matches!(self, Self::Legacy(AsrError::TextExtraction { .. }))
    }
}
