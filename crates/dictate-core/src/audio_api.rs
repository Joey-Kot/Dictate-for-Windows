//! Unified runtime entry point for Legacy and Advanced Audio API workflows.

use std::future::Future;
use std::path::{Path, PathBuf};

use futures_util::stream::{FuturesUnordered, StreamExt};
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

    /// Runs one complete non-realtime transcription workflow for every prepared
    /// segment. The completed texts are concatenated in source-segment order
    /// without adding separators.
    ///
    /// Successful per-segment raw responses are deliberately discarded before
    /// the batch result is collected. This avoids retaining an unbounded set of
    /// response bodies for a long recording; callers receive the merged text
    /// with an empty `raw_response`.
    pub async fn transcribe_segments(
        &self,
        cancellation: &CancellationToken,
        file_paths: &[PathBuf],
        max_concurrency: usize,
    ) -> Result<Transcription, AudioApiError> {
        self.ensure_segmented_upload_available()?;
        let texts = run_segment_batch(
            file_paths,
            cancellation,
            max_concurrency,
            |_, file_path, segment_cancellation| async move {
                self.transcribe(&segment_cancellation, &file_path)
                    .await
                    .map(|transcription| transcription.text)
            },
        )
        .await
        .map_err(AudioApiError::from_segment_batch_error)?;

        Ok(Transcription {
            text: texts.concat(),
            raw_response: Vec::new(),
        })
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

    /// Runs the formal Legacy or Advanced test workflow for every prepared
    /// segment, using the same cancellation and bounded-concurrency semantics
    /// as [`Self::transcribe_segments`].
    pub async fn test_connection_segments_cancellable(
        &self,
        cancellation: &CancellationToken,
        file_paths: &[PathBuf],
        max_concurrency: usize,
    ) -> Result<(), AudioApiError> {
        self.ensure_segmented_upload_available()?;
        run_segment_batch(
            file_paths,
            cancellation,
            max_concurrency,
            |_, file_path, segment_cancellation| async move {
                self.test_connection_cancellable(&segment_cancellation, &file_path)
                    .await
            },
        )
        .await
        .map_err(AudioApiError::from_segment_batch_error)?;
        Ok(())
    }

    fn ensure_segmented_upload_available(&self) -> Result<(), AudioApiError> {
        if self.is_realtime_workflow() {
            Err(AudioApiError::SegmentedRealtimeUnavailable)
        } else {
            Ok(())
        }
    }
}

/// Executes independent segment operations while retaining every started
/// future until it finishes. Keeping the futures alive after an error or
/// cancellation is required for Advanced workflows to run their existing
/// remote-object cleanup paths.
async fn run_segment_batch<T, E, F, Fut>(
    file_paths: &[PathBuf],
    cancellation: &CancellationToken,
    max_concurrency: usize,
    mut operation: F,
) -> Result<Vec<T>, SegmentBatchError<E>>
where
    F: FnMut(usize, PathBuf, CancellationToken) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    if file_paths.is_empty() {
        return Err(SegmentBatchError::Empty);
    }
    if max_concurrency == 0 {
        return Err(SegmentBatchError::InvalidConcurrency);
    }

    let segment_cancellation = cancellation.child_token();
    let mut next_index = 0;
    let mut stopped = cancellation.is_cancelled();
    if stopped {
        segment_cancellation.cancel();
    }

    let mut in_flight = FuturesUnordered::new();
    let mut completed = Vec::with_capacity(file_paths.len());
    let mut first_error = None;

    loop {
        while !stopped && in_flight.len() < max_concurrency && next_index < file_paths.len() {
            if cancellation.is_cancelled() {
                stopped = true;
                segment_cancellation.cancel();
                break;
            }

            let index = next_index;
            next_index += 1;
            let future = operation(
                index,
                file_paths[index].clone(),
                segment_cancellation.clone(),
            );
            in_flight.push(async move { (index, future.await) });
        }

        if in_flight.is_empty() {
            break;
        }

        let completed_operation = if stopped {
            // The child token has already been canceled. Drain every started
            // operation instead of dropping its future, so its cleanup guards
            // can finish before this batch returns.
            in_flight.next().await
        } else {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    stopped = true;
                    segment_cancellation.cancel();
                    continue;
                }
                result = in_flight.next() => result,
            }
        };

        let Some((index, result)) = completed_operation else {
            break;
        };
        match result {
            Ok(value) => completed.push((index, value)),
            Err(source) => {
                if first_error.is_none() {
                    first_error = Some(SegmentBatchError::Segment { index, source });
                }
                stopped = true;
                segment_cancellation.cancel();
            }
        }
    }

    // A user cancellation wins even when an earlier segment had already
    // failed. It is safe to inspect this only after draining because the
    // parent token remains canceled permanently.
    if cancellation.is_cancelled() {
        return Err(SegmentBatchError::Canceled);
    }
    if let Some(error) = first_error {
        return Err(error);
    }

    completed.sort_unstable_by_key(|(index, _)| *index);
    Ok(completed.into_iter().map(|(_, value)| value).collect())
}

#[derive(Debug)]
enum SegmentBatchError<E> {
    Empty,
    InvalidConcurrency,
    Canceled,
    Segment { index: usize, source: E },
}

#[derive(Debug, Error)]
pub enum AudioApiError {
    #[error("{0}")]
    Legacy(#[from] AsrError),
    #[error("{0}")]
    Advanced(#[from] AdvancedAudioError),
    #[error("live realtime transcription requires an Advanced realtime workflow")]
    LiveRealtimeUnavailable,
    #[error("segmented upload requires at least one prepared audio segment")]
    EmptySegmentBatch,
    #[error("segmented upload concurrency must be greater than zero")]
    InvalidSegmentConcurrency,
    #[error("segmented upload is unavailable for realtime workflows")]
    SegmentedRealtimeUnavailable,
    // Do not include the nested error in the display string. It can contain
    // transport details; callers still retain the error chain for diagnostics.
    #[error("segmented upload failed for segment {index}")]
    Segment {
        index: usize,
        #[source]
        source: Box<AudioApiError>,
    },
    #[error("request canceled")]
    Canceled,
}

impl AudioApiError {
    fn from_segment_batch_error(error: SegmentBatchError<Self>) -> Self {
        match error {
            SegmentBatchError::Empty => Self::EmptySegmentBatch,
            SegmentBatchError::InvalidConcurrency => Self::InvalidSegmentConcurrency,
            SegmentBatchError::Canceled => Self::Canceled,
            SegmentBatchError::Segment { index, source } => Self::Segment {
                index,
                source: Box::new(source),
            },
        }
    }

    pub fn is_retry_exhausted(&self) -> bool {
        match self {
            Self::Legacy(error) => error.is_retry_exhausted(),
            Self::Segment { source, .. } => source.is_retry_exhausted(),
            _ => false,
        }
    }

    pub fn last_response(&self) -> &[u8] {
        match self {
            Self::Legacy(error) => error.last_response(),
            Self::Advanced(error) => error.last_response(),
            Self::Segment { source, .. } => source.last_response(),
            Self::LiveRealtimeUnavailable
            | Self::EmptySegmentBatch
            | Self::InvalidSegmentConcurrency
            | Self::SegmentedRealtimeUnavailable
            | Self::Canceled => &[],
        }
    }

    pub fn is_canceled(&self) -> bool {
        matches!(self, Self::Canceled | Self::Legacy(AsrError::Canceled))
            || matches!(self, Self::Advanced(error) if error.is_canceled())
            || matches!(self, Self::Segment { source, .. } if source.is_canceled())
    }

    pub fn is_text_extraction_error(&self) -> bool {
        matches!(self, Self::Legacy(AsrError::TextExtraction { .. }))
            || matches!(self, Self::Segment { source, .. } if source.is_text_extraction_error())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};
    use tokio::time::timeout;

    use super::*;
    use crate::Config;
    use crate::advanced_audio::CURRENT_SCHEMA_VERSION;
    use crate::advanced_audio::schema::{
        AudioSpec, PauseBehavior, RealtimeAudioMessage, RealtimeAudioStream, RealtimeCompletion,
        RealtimeConnect, RealtimePacing, RealtimeTransport, RealtimeWorkflow, SignerConfig,
        StreamAction, StreamRule,
    };
    use crate::advanced_audio::{
        AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery,
        WorkflowSchemaVersion,
    };

    #[derive(Debug, PartialEq, Eq)]
    enum TestError {
        Failed(usize),
        Canceled,
    }

    fn paths(count: usize) -> Vec<PathBuf> {
        (0..count)
            .map(|index| PathBuf::from(format!("segment-{index}.wav")))
            .collect()
    }

    fn realtime_client() -> AudioApiClient {
        let workflow = AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            name: "realtime batch rejection test".into(),
            parameters: vec![],
            secrets: vec![],
            audio: AudioSpec {
                delivery: AudioDelivery::RealtimeChunks,
                mime: None,
            },
            recognition: AdvancedRecognition::RealtimeSession {
                realtime: Box::new(RealtimeWorkflow {
                    transport: RealtimeTransport::WebSocket,
                    connect: RealtimeConnect {
                        url: "ws://127.0.0.1:1/realtime".into(),
                        query: BTreeMap::new(),
                        headers: BTreeMap::new(),
                        signer: SignerConfig::None,
                        subprotocol: None,
                    },
                    initial_messages: vec![],
                    audio_stream: RealtimeAudioStream {
                        codec: "pcm_s16le".into(),
                        sample_rate: 16_000,
                        channels: 1,
                        chunk_duration_ms: 10,
                        pacing: RealtimePacing::Realtime,
                    },
                    audio_message: RealtimeAudioMessage::Binary,
                    receive_rules: vec![StreamRule {
                        event: Some("done".into()),
                        path: None,
                        action: StreamAction::Complete,
                        equals: None,
                    }],
                    finish_messages: vec![],
                    completion: RealtimeCompletion {
                        event: None,
                        path: None,
                        equals: None,
                    },
                    pause_behavior: PauseBehavior::RestartSession,
                    finalization_timeout_ms: 1_000,
                }),
            },
        };
        let mut config = Config::default();
        config.advanced_audio_api = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(workflow),
            ..AdvancedAudioConfig::default()
        };
        AudioApiClient::new(config).expect("test realtime workflow must validate")
    }

    fn update_peak(peak: &AtomicUsize, current: usize) {
        let mut observed = peak.load(Ordering::Relaxed);
        while current > observed {
            match peak.compare_exchange_weak(
                observed,
                current,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => observed = actual,
            }
        }
    }

    async fn receive_index(receiver: &mut mpsc::UnboundedReceiver<usize>) -> usize {
        timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("timed out waiting for a segment operation to start")
            .expect("segment operation start sender was dropped")
    }

    #[tokio::test]
    async fn segment_batch_limits_concurrency_and_restores_source_order() {
        let paths = paths(4);
        let parent = CancellationToken::new();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (started_sender, mut started_receiver) = mpsc::unbounded_channel();
        let mut release_senders = Vec::new();
        let mut release_receivers = Vec::new();
        for _ in 0..paths.len() {
            let (sender, receiver) = oneshot::channel();
            release_senders.push(Some(sender));
            release_receivers.push(Some(receiver));
        }
        let release_receivers = Arc::new(tokio::sync::Mutex::new(release_receivers));

        let batch_paths = paths.clone();
        let batch_parent = parent.clone();
        let batch_active = active.clone();
        let batch_peak = peak.clone();
        let batch_receivers = release_receivers.clone();
        let batch = tokio::spawn(async move {
            run_segment_batch(&batch_paths, &batch_parent, 2, move |index, _, _| {
                let active = batch_active.clone();
                let peak = batch_peak.clone();
                let receivers = batch_receivers.clone();
                let started_sender = started_sender.clone();
                async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    update_peak(&peak, current);
                    started_sender
                        .send(index)
                        .expect("test receiver must remain available");
                    let receiver = receivers
                        .lock()
                        .await
                        .get_mut(index)
                        .and_then(Option::take)
                        .expect("each segment must have exactly one gate");
                    receiver
                        .await
                        .expect("test gate sender must remain available");
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<usize, TestError>(index)
                }
            })
            .await
        });

        assert_eq!(receive_index(&mut started_receiver).await, 0);
        assert_eq!(receive_index(&mut started_receiver).await, 1);
        release_senders[1]
            .take()
            .expect("segment 1 gate must exist")
            .send(())
            .expect("segment 1 must still be running");
        assert_eq!(receive_index(&mut started_receiver).await, 2);
        release_senders[2]
            .take()
            .expect("segment 2 gate must exist")
            .send(())
            .expect("segment 2 must still be running");
        assert_eq!(receive_index(&mut started_receiver).await, 3);
        release_senders[0]
            .take()
            .expect("segment 0 gate must exist")
            .send(())
            .expect("segment 0 must still be running");
        release_senders[3]
            .take()
            .expect("segment 3 gate must exist")
            .send(())
            .expect("segment 3 must still be running");

        let result = timeout(Duration::from_secs(1), batch)
            .await
            .expect("batch must finish")
            .expect("batch task must not panic");
        assert_eq!(result.expect("batch must succeed"), vec![0, 1, 2, 3]);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn segment_batch_first_error_stops_feeding_and_drains_started_work() {
        let paths = paths(3);
        let parent = CancellationToken::new();
        let started = Arc::new(Mutex::new(Vec::new()));
        let (cleanup_sender, cleanup_receiver) = oneshot::channel();
        let cleanup_sender = Arc::new(Mutex::new(Some(cleanup_sender)));

        let result = timeout(
            Duration::from_secs(1),
            run_segment_batch(&paths, &parent, 2, {
                let started = started.clone();
                move |index, _, segment_cancellation| {
                    started
                        .lock()
                        .expect("test lock must not poison")
                        .push(index);
                    let cleanup_sender = cleanup_sender.clone();
                    async move {
                        if index == 0 {
                            return Err::<usize, TestError>(TestError::Failed(index));
                        }
                        segment_cancellation.cancelled().await;
                        cleanup_sender
                            .lock()
                            .expect("test lock must not poison")
                            .take()
                            .expect("only the started sibling should clean up")
                            .send(())
                            .expect("test receiver must remain available");
                        Err::<usize, TestError>(TestError::Canceled)
                    }
                }
            }),
        )
        .await
        .expect("batch must drain started work");

        assert!(matches!(
            result,
            Err(SegmentBatchError::Segment {
                index: 0,
                source: TestError::Failed(0),
            })
        ));
        assert_eq!(
            *started.lock().expect("test lock must not poison"),
            vec![0, 1]
        );
        assert!(cleanup_receiver.await.is_ok());
        assert!(!parent.is_cancelled());
    }

    #[tokio::test]
    async fn segment_batch_parent_cancellation_stops_feeding_and_drains_children() {
        let paths = paths(3);
        let parent = CancellationToken::new();
        let batch_parent = parent.clone();
        let (started_sender, mut started_receiver) = mpsc::unbounded_channel();
        let (cleanup_sender, mut cleanup_receiver) = mpsc::unbounded_channel();

        let batch = tokio::spawn(async move {
            run_segment_batch(
                &paths,
                &batch_parent,
                2,
                move |index, _, segment_cancellation| {
                    started_sender
                        .send(index)
                        .expect("test receiver must remain available");
                    let cleanup_sender = cleanup_sender.clone();
                    async move {
                        segment_cancellation.cancelled().await;
                        cleanup_sender
                            .send(index)
                            .expect("test receiver must remain available");
                        Err::<usize, TestError>(TestError::Canceled)
                    }
                },
            )
            .await
        });

        assert_eq!(receive_index(&mut started_receiver).await, 0);
        assert_eq!(receive_index(&mut started_receiver).await, 1);
        parent.cancel();

        let result = timeout(Duration::from_secs(1), batch)
            .await
            .expect("batch must finish after child cleanup")
            .expect("batch task must not panic");
        assert!(matches!(result, Err(SegmentBatchError::Canceled)));
        let mut cleaned = vec![
            receive_index(&mut cleanup_receiver).await,
            receive_index(&mut cleanup_receiver).await,
        ];
        cleaned.sort_unstable();
        assert_eq!(cleaned, vec![0, 1]);
        assert!(started_receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn segment_batch_already_canceled_parent_starts_nothing() {
        let paths = paths(1);
        let parent = CancellationToken::new();
        parent.cancel();
        let starts = AtomicUsize::new(0);

        let result = run_segment_batch(&paths, &parent, 1, |_, _, _| {
            starts.fetch_add(1, Ordering::SeqCst);
            async { Ok::<usize, TestError>(0) }
        })
        .await;

        assert!(matches!(result, Err(SegmentBatchError::Canceled)));
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn segment_batch_parent_cancellation_wins_over_a_prior_segment_error() {
        let paths = paths(2);
        let parent = CancellationToken::new();
        let cancel_parent = parent.clone();

        let result = timeout(
            Duration::from_secs(1),
            run_segment_batch(&paths, &parent, 2, move |index, _, segment_cancellation| {
                let cancel_parent = cancel_parent.clone();
                async move {
                    if index == 0 {
                        return Err::<usize, TestError>(TestError::Failed(index));
                    }
                    segment_cancellation.cancelled().await;
                    cancel_parent.cancel();
                    Err::<usize, TestError>(TestError::Canceled)
                }
            }),
        )
        .await
        .expect("batch must drain started work");

        assert!(matches!(result, Err(SegmentBatchError::Canceled)));
    }

    #[tokio::test]
    async fn segment_batch_validates_empty_input_and_zero_concurrency() {
        let parent = CancellationToken::new();
        let empty_paths = Vec::new();
        let empty = run_segment_batch(&empty_paths, &parent, 1, |_, _, _| async {
            Ok::<usize, TestError>(0)
        })
        .await;
        assert!(matches!(empty, Err(SegmentBatchError::Empty)));

        let nonempty_paths = paths(1);
        let zero = run_segment_batch(&nonempty_paths, &parent, 0, |_, _, _| async {
            Ok::<usize, TestError>(0)
        })
        .await;
        assert!(matches!(zero, Err(SegmentBatchError::InvalidConcurrency)));
    }

    #[tokio::test]
    async fn segmented_methods_reject_realtime_workflows_before_using_paths() {
        let client = realtime_client();
        let parent = CancellationToken::new();
        let paths = paths(1);

        let transcription = client.transcribe_segments(&parent, &paths, 1).await;
        assert!(matches!(
            transcription,
            Err(AudioApiError::SegmentedRealtimeUnavailable)
        ));

        let test_workflow = client
            .test_connection_segments_cancellable(&parent, &paths, 1)
            .await;
        assert!(matches!(
            test_workflow,
            Err(AudioApiError::SegmentedRealtimeUnavailable)
        ));
    }

    #[test]
    fn segment_error_hides_nested_display_but_preserves_error_helpers() {
        let error = AudioApiError::from_segment_batch_error(SegmentBatchError::Segment {
            index: 2,
            source: AudioApiError::Legacy(AsrError::RetryExhausted {
                max_retry: 1,
                attempts: 1,
                last_response: b"response body that must not be displayed".to_vec(),
            }),
        });

        assert_eq!(error.to_string(), "segmented upload failed for segment 2");
        assert!(error.is_retry_exhausted());
        assert_eq!(
            error.last_response(),
            b"response body that must not be displayed"
        );
        assert!(!error.to_string().contains("response body"));

        let canceled = AudioApiError::Segment {
            index: 0,
            source: Box::new(AudioApiError::Legacy(AsrError::Canceled)),
        };
        assert!(canceled.is_canceled());

        let extraction = AudioApiError::Segment {
            index: 1,
            source: Box::new(AudioApiError::Legacy(AsrError::TextExtraction {
                source: crate::jsonpath::TextExtractionError::NoMatch,
                last_response: Vec::new(),
            })),
        };
        assert!(extraction.is_text_extraction_error());
    }
}
