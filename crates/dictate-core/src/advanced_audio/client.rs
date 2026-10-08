//! Runtime executor for validated Advanced Audio API workflows.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::accumulator::{TranscriptAccumulator, TranscriptAccumulatorError, TranscriptAction};
use super::extractor::{self, ResponseData, ResponseExtractionError};
use super::http::{
    BodyError, HttpEngine, HttpEngineError, HttpResponseData, PreparedAudio, ResponseReadError,
    StageContext, read_response_limited,
};
use super::realtime::{LiveChunkSource, run_realtime_session, run_recorded_replay};
use super::remote_audio::{PublishedRemoteAudio, cleanup_remote_audio, publish_remote_audio};
use super::request_stream::{JsonChunksDecoder, NdjsonDecoder, SseDecoder, StreamFrameError};
use super::schema::*;
use super::template::RuntimeTemplateValues;
use super::validation::{
    ValidationErrors, capture_only_http_url_template_id, validate_advanced_audio_config,
};

use crate::asr::Transcription;

/// Core-owned executor selected when `ADVANCED_AUDIO_API.enabled` is true.
/// It contains no provider identity and no LLM/generator dependency.
#[derive(Clone)]
pub struct AdvancedAudioClient {
    config: crate::Config,
    workflow: AdvancedAudioWorkflow,
    values: BTreeMap<String, String>,
    secrets: BTreeMap<String, String>,
    sensitive_capture_ids: std::collections::BTreeSet<String>,
    network_client: reqwest::Client,
    http: HttpEngine,
}

impl AdvancedAudioClient {
    pub fn new(config: crate::Config) -> Result<Self, AdvancedAudioError> {
        if !config.advanced_audio_api.enabled {
            return Err(AdvancedAudioError::Disabled);
        }
        validate_advanced_audio_config(&config.advanced_audio_api)
            .map_err(AdvancedAudioError::InvalidConfig)?;
        let workflow = config
            .advanced_audio_api
            .workflow
            .clone()
            .ok_or(AdvancedAudioError::MissingWorkflow)?;
        let sensitive_capture_ids = workflow_sensitive_capture_ids(&workflow);
        let client = crate::network::client(&config).map_err(AdvancedAudioError::Client)?;
        let mut values = config.advanced_audio_api.values.clone();
        for definition in &workflow.parameters {
            if !values.contains_key(&definition.id)
                && let Some(default) = &definition.default
            {
                values.insert(definition.id.clone(), default.clone());
            }
        }
        Ok(Self {
            secrets: config.advanced_audio_api.secrets.clone(),
            config,
            workflow,
            values,
            network_client: client.clone(),
            http: HttpEngine::new(client),
            sensitive_capture_ids,
        })
    }

    pub fn workflow(&self) -> &AdvancedAudioWorkflow {
        &self.workflow
    }

    pub fn is_realtime_workflow(&self) -> bool {
        matches!(
            self.workflow.recognition,
            AdvancedRecognition::RealtimeSession { .. }
        )
    }

    pub fn realtime_audio_stream(&self) -> Option<RealtimeAudioStream> {
        match &self.workflow.recognition {
            AdvancedRecognition::RealtimeSession { realtime } => {
                Some(realtime.audio_stream.clone())
            }
            _ => None,
        }
    }

    /// Advanced workflow diagnostics intentionally identify only the stable
    /// protocol phase.  Rendered URLs, headers, bodies, captures, audio, and
    /// transcripts must never enter the upload debug stream.
    fn debug_stage(&self, label: &'static str) {
        if self.config.upload_debug {
            crate::debug_log::write(crate::debug_log::Category::Upload, format_args!("{label}"));
        }
    }

    fn redact_sensitive_capture_response(
        &self,
        response: &[u8],
        captures: &BTreeMap<String, String>,
    ) -> Vec<u8> {
        let sensitive_values = captures
            .iter()
            .filter(|(id, value)| self.sensitive_capture_ids.contains(*id) && !value.is_empty())
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        if sensitive_values.is_empty() {
            return response.to_vec();
        }
        let response_text = String::from_utf8_lossy(response);
        let redacted = crate::debug_log::request_message_with_sensitive_captures(
            &response_text,
            &self.config,
            &sensitive_values,
        );
        // Responses are normally text/JSON, but raw cache data is bytes.
        // Do not turn unrelated non-UTF-8 data into replacement characters
        // merely because another capture in this workflow is sensitive.
        if redacted == response_text {
            response.to_vec()
        } else {
            redacted.into_bytes()
        }
    }

    /// Runs the live half of a realtime workflow. The recorder owns the WAV;
    /// this source is only its optional non-blocking packet tee.
    pub async fn transcribe_live(
        &self,
        cancellation: &CancellationToken,
        source: &mut LiveChunkSource,
    ) -> Result<Transcription, AdvancedAudioError> {
        let AdvancedRecognition::RealtimeSession { realtime } = &self.workflow.recognition else {
            return Err(AdvancedAudioError::NotRealtimeWorkflow);
        };
        self.debug_stage("[asr-stream] realtime");
        let runtime = runtime_values();
        run_realtime_session(
            realtime,
            &self.values,
            &self.secrets,
            &runtime,
            source,
            cancellation,
        )
        .await
        .map_err(AdvancedAudioError::Realtime)
    }

    /// Runs the workflow for a prepared audio file and returns one final
    /// transcript. Streaming partials never escape this Core client.
    pub async fn transcribe(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<Transcription, AdvancedAudioError> {
        if cancellation.is_cancelled() {
            return Err(AdvancedAudioError::Canceled);
        }
        let runtime = runtime_values();
        if let AdvancedRecognition::RealtimeSession { realtime } = &self.workflow.recognition {
            return self
                .execute_realtime_replay(realtime, &runtime, file_path, cancellation)
                .await;
        }
        let audio =
            PreparedAudio::from_path(file_path, self.workflow.audio.mime.as_deref()).await?;
        if matches!(
            &self.workflow.audio.delivery,
            AudioDelivery::PublicHttpsUrl | AudioDelivery::CloudUri
        ) {
            self.debug_stage("[remote-audio] upload");
        }
        let (audio, published) = self.prepare_audio_delivery(audio, cancellation).await?;
        // Keep all fallible workflow work inside a nested result.  A `?` in
        // this scope must not bypass the remote object's end-of-workflow
        // cleanup: recognition errors and cancellation are both terminal
        // lifecycle states for a successful upload.
        let result = async {
            match &self.workflow.recognition {
                AdvancedRecognition::Request {
                    request,
                    final_text,
                } => {
                    self.debug_stage("[advanced-audio] request");
                    let response = self
                        .execute_retryable_stage(request, &audio, &runtime, cancellation)
                        .await?;
                    let text = extract_final(&response, final_text)?;
                    Ok(Transcription {
                        text,
                        raw_response: response.body,
                    })
                }
                AdvancedRecognition::RequestStream { request, stream } => {
                    self.debug_stage("[asr-stream] stream");
                    self.execute_request_stream(request, stream, &audio, &runtime, cancellation)
                        .await
                }
                AdvancedRecognition::AsyncPoll {
                    prepare,
                    submit,
                    poll,
                    result_steps,
                    final_text,
                } => {
                    self.execute_async_poll(
                        prepare.as_deref(),
                        submit,
                        poll.as_deref(),
                        result_steps,
                        final_text,
                        &audio,
                        &runtime,
                        cancellation,
                    )
                    .await
                }
                AdvancedRecognition::RealtimeSession { .. } => {
                    unreachable!("handled before audio preparation")
                }
            }
        }
        .await;
        if let Some(published) = published {
            // `delete_after_recognition` is a retention preference for a
            // completed workflow. An error or cancellation after a successful
            // upload still requires best-effort cleanup so a failed ASR task
            // cannot leave an orphaned remote recording behind.
            cleanup_remote_audio(&self.network_client, &published, result.is_err()).await;
        }
        result
    }

    /// A replay failure always starts a completely new session at the
    /// beginning of the recorded file. `run_recorded_replay` owns both the
    /// source and transcript accumulator for one attempt, so no partial text
    /// can survive into a retry.
    async fn execute_realtime_replay(
        &self,
        realtime: &RealtimeWorkflow,
        runtime: &RuntimeTemplateValues,
        file_path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Transcription, AdvancedAudioError> {
        let max_attempts = self.config.max_retry.max(1) as usize;
        let mut delay = self.config.retry_base_delay.max(0.0);
        for attempt in 0..max_attempts {
            self.debug_stage("[asr-stream] replay");
            match run_recorded_replay(
                realtime,
                &self.values,
                &self.secrets,
                runtime,
                file_path,
                cancellation,
            )
            .await
            {
                Ok(transcription) => return Ok(transcription),
                Err(_) if cancellation.is_cancelled() => return Err(AdvancedAudioError::Canceled),
                Err(_error) if attempt + 1 < max_attempts => {
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                        _ = tokio::time::sleep(Duration::from_secs_f64(delay)) => {}
                    }
                    delay = (delay * 2.0).min(60.0);
                    // The discarded error belongs to an attempt-local source
                    // and accumulator; no partial recognition state is kept
                    // before the next session starts.
                }
                Err(error) => return Err(AdvancedAudioError::Realtime(error)),
            }
        }
        unreachable!("at least one realtime replay attempt is always made")
    }

    /// Settings test uses the exact runtime workflow, not a separate request
    /// implementation. The caller supplies the fixed short test audio.
    pub async fn test_workflow(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<(), AdvancedAudioError> {
        self.transcribe(cancellation, file_path).await.map(|_| ())
    }

    async fn prepare_audio_delivery(
        &self,
        audio: PreparedAudio,
        cancellation: &CancellationToken,
    ) -> Result<(PreparedAudio, Option<PublishedRemoteAudio>), AdvancedAudioError> {
        match &self.workflow.audio.delivery {
            AudioDelivery::PublicHttpsUrl | AudioDelivery::CloudUri => {
                let published = publish_remote_audio(
                    &self.network_client,
                    &self.config.advanced_audio_api.remote_audio,
                    self.workflow.audio.delivery.kind(),
                    &audio,
                    cancellation,
                )
                .await
                .map_err(AdvancedAudioError::RemoteAudio)?;
                Ok((published.attach_to(audio), Some(published)))
            }
            AudioDelivery::RealtimeChunks => Err(AdvancedAudioError::RealtimeUnavailable),
            AudioDelivery::MultipartFile
            | AudioDelivery::RawAudio
            | AudioDelivery::Base64
            | AudioDelivery::DataUri
            | AudioDelivery::ProviderUpload => Ok((audio, None)),
        }
    }

    async fn execute_retryable_stage(
        &self,
        stage: &HttpStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
    ) -> Result<HttpResponseData, AdvancedAudioError> {
        let max_attempts = self.config.max_retry.max(1) as usize;
        let mut delay = self.config.retry_base_delay.max(0.0);
        let mut last_error = None;
        for attempt in 0..max_attempts {
            match self
                .execute_stage(stage, audio, runtime, cancellation, &mut BTreeMap::new())
                .await
            {
                Ok(response) => return Ok(response),
                Err(_error) if cancellation.is_cancelled() => {
                    return Err(AdvancedAudioError::Canceled);
                }
                Err(error) if is_retryable(&error) && attempt + 1 < max_attempts => {
                    last_error = Some(error);
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                        _ = tokio::time::sleep(Duration::from_secs_f64(delay)) => {}
                    }
                    delay = (delay * 2.0).min(60.0);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| AdvancedAudioError::Execution("request retry exhausted".into())))
    }

    async fn execute_stage(
        &self,
        stage: &HttpStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
        captures: &mut BTreeMap<String, String>,
    ) -> Result<HttpResponseData, AdvancedAudioError> {
        let response = self
            .execute_stage_response(stage, audio, runtime, cancellation, captures)
            .await?;
        self.store_captures(stage, &response, captures)?;
        Ok(response)
    }

    /// Executes a stage through status validation without mutating captures.
    /// Poll responses need this separation: captures declared on a poll stage
    /// represent terminal result fields, which are absent while a task is
    /// pending and would otherwise be inserted repeatedly on every poll.
    async fn execute_stage_response(
        &self,
        stage: &HttpStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
        captures: &BTreeMap<String, String>,
    ) -> Result<HttpResponseData, AdvancedAudioError> {
        let response = self
            .send_stage(stage, audio, runtime, cancellation, captures)
            .await?;
        let response = read_response_limited(response, cancellation)
            .await
            .map_err(AdvancedAudioError::ReadResponse)?;
        if !response.is_accepted(&stage.accepted_statuses) {
            return Err(AdvancedAudioError::UnexpectedStatus {
                status: response.status.as_u16(),
                response: self.redact_sensitive_capture_response(&response.body, captures),
            });
        }
        Ok(response)
    }

    async fn send_stage(
        &self,
        stage: &HttpStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
        captures: &BTreeMap<String, String>,
    ) -> Result<reqwest::Response, AdvancedAudioError> {
        let context = StageContext::new(
            &self.values,
            &self.secrets,
            captures,
            audio,
            runtime,
            matches!(
                &self.workflow.audio.delivery,
                AudioDelivery::Base64 | AudioDelivery::DataUri
            ),
        )
        .await?;
        self.http
            .send(stage, &context, cancellation)
            .await
            .map_err(AdvancedAudioError::Http)
    }

    fn store_captures(
        &self,
        stage: &HttpStage,
        response: &HttpResponseData,
        captures: &mut BTreeMap<String, String>,
    ) -> Result<(), AdvancedAudioError> {
        let view = ResponseData::new(response.status.as_u16(), &response.headers, &response.body);
        let extracted = extractor::extract_captures(&view, &stage.captures)
            .map_err(AdvancedAudioError::Extraction)?;
        for (name, value) in extracted {
            if captures.insert(name.clone(), value).is_some() {
                return Err(AdvancedAudioError::Execution(format!(
                    "capture '{name}' would overwrite an existing capture"
                )));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_async_poll(
        &self,
        prepare: Option<&HttpStage>,
        submit: &HttpStage,
        poll: Option<&PollStage>,
        result_steps: &[HttpStage],
        final_text: &ResponseExtractor,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
    ) -> Result<Transcription, AdvancedAudioError> {
        let mut captures = BTreeMap::new();
        if let Some(prepare) = prepare {
            self.debug_stage("[advanced-audio] prepare");
            self.execute_stage(prepare, audio, runtime, cancellation, &mut captures)
                .await?;
        }
        // Never route a failed submit through the request retry loop. The
        // server may have created a paid task even when its response was lost.
        self.debug_stage("[advanced-audio] submit");
        let mut response = self
            .execute_stage(submit, audio, runtime, cancellation, &mut captures)
            .await?;
        if let Some(poll) = poll {
            response = self
                .poll_until_terminal(poll, audio, runtime, cancellation, &mut captures)
                .await?;
        }
        for stage in result_steps {
            self.debug_stage("[advanced-audio] result");
            if stage.method.is_read_only() {
                response = self
                    .execute_retryable_stage_with_captures(
                        stage,
                        audio,
                        runtime,
                        cancellation,
                        &mut captures,
                    )
                    .await?;
            } else {
                response = self
                    .execute_stage(stage, audio, runtime, cancellation, &mut captures)
                    .await?;
            }
        }
        let text = extract_final(&response, final_text)?;
        Ok(Transcription {
            text,
            // The final response is retained by the cache layer. It can echo
            // a temporary result URL captured earlier in this workflow, so
            // apply the same execution-local redaction used for errors.
            raw_response: self.redact_sensitive_capture_response(&response.body, &captures),
        })
    }

    async fn execute_retryable_stage_with_captures(
        &self,
        stage: &HttpStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
        captures: &mut BTreeMap<String, String>,
    ) -> Result<HttpResponseData, AdvancedAudioError> {
        let max_attempts = self.config.max_retry.max(1) as usize;
        let mut delay = self.config.retry_base_delay.max(0.0);
        for attempt in 0..max_attempts {
            match self
                .execute_stage(stage, audio, runtime, cancellation, captures)
                .await
            {
                Ok(response) => return Ok(response),
                Err(_error) if cancellation.is_cancelled() => {
                    return Err(AdvancedAudioError::Canceled);
                }
                Err(error) if is_retryable(&error) && attempt + 1 < max_attempts => {
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                        _ = tokio::time::sleep(Duration::from_secs_f64(delay)) => {}
                    }
                    delay = (delay * 2.0).min(60.0);
                }
                Err(error) => return Err(error),
            }
        }
        Err(AdvancedAudioError::Execution(
            "read-only request retry exhausted".into(),
        ))
    }

    /// Retries a read-only stage without recording its captures. This is used
    /// for polling, where captures become available only on a successful
    /// terminal response.
    async fn execute_retryable_stage_without_captures(
        &self,
        stage: &HttpStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
        captures: &BTreeMap<String, String>,
    ) -> Result<HttpResponseData, AdvancedAudioError> {
        let max_attempts = self.config.max_retry.max(1) as usize;
        let mut delay = self.config.retry_base_delay.max(0.0);
        for attempt in 0..max_attempts {
            match self
                .execute_stage_response(stage, audio, runtime, cancellation, captures)
                .await
            {
                Ok(response) => return Ok(response),
                Err(_error) if cancellation.is_cancelled() => {
                    return Err(AdvancedAudioError::Canceled);
                }
                Err(error) if is_poll_retryable(&error) && attempt + 1 < max_attempts => {
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                        _ = tokio::time::sleep(Duration::from_secs_f64(delay)) => {}
                    }
                    delay = (delay * 2.0).min(60.0);
                }
                Err(error) => return Err(error),
            }
        }
        Err(AdvancedAudioError::Execution(
            "poll request retry exhausted".into(),
        ))
    }

    async fn poll_until_terminal(
        &self,
        poll: &PollStage,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
        captures: &mut BTreeMap<String, String>,
    ) -> Result<HttpResponseData, AdvancedAudioError> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(poll.timeout_ms);
        loop {
            if cancellation.is_cancelled() {
                return Err(AdvancedAudioError::Canceled);
            }
            self.debug_stage("[advanced-audio] poll");
            let request = self.execute_retryable_stage_without_captures(
                &poll.request,
                audio,
                runtime,
                cancellation,
                captures,
            );
            let response = tokio::select! {
                _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                result = tokio::time::timeout_at(deadline, request) => {
                    result.map_err(|_| AdvancedAudioError::PollTimeout {
                        timeout_ms: poll.timeout_ms,
                    })??
                }
            };
            match poll_state(poll, &response)? {
                PollState::Success => {
                    // A poll capture is a terminal value by construction. Do
                    // not attempt to extract it from pending responses (where
                    // it often does not exist), and do not let it overwrite
                    // itself after a repeated poll.
                    self.store_captures(&poll.request, &response, captures)?;
                    return Ok(response);
                }
                PollState::Failure => return Err(AdvancedAudioError::PollFailure),
                PollState::Pending => {}
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                _ = tokio::time::sleep_until(deadline) => return Err(AdvancedAudioError::PollTimeout {
                    timeout_ms: poll.timeout_ms,
                }),
                _ = tokio::time::sleep(Duration::from_millis(poll.interval_ms)) => {}
            }
        }
    }

    async fn execute_request_stream(
        &self,
        request: &HttpStage,
        stream_schema: &StreamResponse,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
    ) -> Result<Transcription, AdvancedAudioError> {
        let max_attempts = self.config.max_retry.max(1) as usize;
        let mut delay = self.config.retry_base_delay.max(0.0);
        let mut last_error = None;
        for attempt in 0..max_attempts {
            match self
                .execute_request_stream_once(request, stream_schema, audio, runtime, cancellation)
                .await
            {
                Ok(transcription) => return Ok(transcription),
                Err(_error) if cancellation.is_cancelled() => {
                    return Err(AdvancedAudioError::Canceled);
                }
                Err(error) if is_stream_retryable(&error) && attempt + 1 < max_attempts => {
                    last_error = Some(error);
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(AdvancedAudioError::Canceled),
                        _ = tokio::time::sleep(Duration::from_secs_f64(delay)) => {}
                    }
                    delay = (delay * 2.0).min(60.0);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            AdvancedAudioError::Execution("stream request retry exhausted".into())
        }))
    }

    async fn execute_request_stream_once(
        &self,
        request: &HttpStage,
        stream_schema: &StreamResponse,
        audio: &PreparedAudio,
        runtime: &RuntimeTemplateValues,
        cancellation: &CancellationToken,
    ) -> Result<Transcription, AdvancedAudioError> {
        let mut captures = BTreeMap::new();
        let response = self
            .send_stage(request, audio, runtime, cancellation, &captures)
            .await?;
        if !request
            .accepted_statuses
            .iter()
            .any(|status| *status == response.status().as_u16())
        {
            let response = read_response_limited(response, cancellation)
                .await
                .map_err(AdvancedAudioError::ReadResponse)?;
            return Err(AdvancedAudioError::UnexpectedStatus {
                status: response.status.as_u16(),
                response: self.redact_sensitive_capture_response(&response.body, &captures),
            });
        }
        // Header-only captures are available before streamed body events. A
        // JSON/body capture deliberately cannot be requested from a stream.
        let headers = response.headers().clone();
        let header_view = ResponseData::new(response.status().as_u16(), &headers, &[]);
        for capture in &request.captures {
            if !matches!(
                capture.from,
                ResponseExtractor::Header { .. } | ResponseExtractor::Status
            ) {
                return Err(AdvancedAudioError::Execution(
                    "request_stream captures may only read response headers or status".into(),
                ));
            }
        }
        captures.extend(
            extractor::extract_captures(&header_view, &request.captures)
                .map_err(AdvancedAudioError::Extraction)?,
        );

        let mut raw = Vec::new();
        let mut accumulator = TranscriptAccumulator::new();
        let mut bytes = response.bytes_stream();
        match stream_schema.format {
            StreamFormat::Sse => {
                let mut decoder = SseDecoder::new();
                while let Some(chunk) = next_stream_chunk(&mut bytes, cancellation).await? {
                    append_raw(&mut raw, &chunk)?;
                    for event in decoder
                        .push(&chunk)
                        .map_err(AdvancedAudioError::StreamFrame)?
                    {
                        apply_stream_rules(
                            stream_schema,
                            event.event.as_deref(),
                            &event.data,
                            &mut accumulator,
                        )?;
                    }
                }
                for event in decoder.finish().map_err(AdvancedAudioError::StreamFrame)? {
                    apply_stream_rules(
                        stream_schema,
                        event.event.as_deref(),
                        &event.data,
                        &mut accumulator,
                    )?;
                }
            }
            StreamFormat::Ndjson => {
                let mut decoder = NdjsonDecoder::new();
                while let Some(chunk) = next_stream_chunk(&mut bytes, cancellation).await? {
                    append_raw(&mut raw, &chunk)?;
                    for frame in decoder
                        .push(&chunk)
                        .map_err(AdvancedAudioError::StreamFrame)?
                    {
                        apply_stream_rules(stream_schema, None, &frame.data, &mut accumulator)?;
                    }
                }
                for frame in decoder.finish().map_err(AdvancedAudioError::StreamFrame)? {
                    apply_stream_rules(stream_schema, None, &frame.data, &mut accumulator)?;
                }
            }
            StreamFormat::JsonChunks => {
                let mut decoder = JsonChunksDecoder::new();
                while let Some(chunk) = next_stream_chunk(&mut bytes, cancellation).await? {
                    append_raw(&mut raw, &chunk)?;
                    for frame in decoder
                        .push(&chunk)
                        .map_err(AdvancedAudioError::StreamFrame)?
                    {
                        apply_stream_rules(stream_schema, None, &frame, &mut accumulator)?;
                    }
                }
                for frame in decoder.finish().map_err(AdvancedAudioError::StreamFrame)? {
                    apply_stream_rules(stream_schema, None, &frame, &mut accumulator)?;
                }
            }
        }
        let text = accumulator
            .into_final_text()
            .map_err(AdvancedAudioError::Accumulator)?;
        Ok(Transcription {
            text,
            raw_response: raw,
        })
    }
}

fn workflow_sensitive_capture_ids(
    workflow: &AdvancedAudioWorkflow,
) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    let mut collect = |stage: &HttpStage| {
        ids.extend(
            stage
                .captures
                .iter()
                .filter(|capture| capture.sensitive)
                .map(|capture| capture.id.clone()),
        );
        if let Some(id) = capture_only_http_url_template_id(&stage.url) {
            // A response-provided full URL may be presigned. Even when the
            // workflow author did not set `Capture.sensitive`, keeping it out
            // of later errors and cached raw responses is the safe default.
            ids.insert(id.into());
        }
    };
    match &workflow.recognition {
        AdvancedRecognition::Request { request, .. }
        | AdvancedRecognition::RequestStream { request, .. } => collect(request),
        AdvancedRecognition::AsyncPoll {
            prepare,
            submit,
            poll,
            result_steps,
            ..
        } => {
            if let Some(prepare) = prepare {
                collect(prepare);
            }
            collect(submit);
            if let Some(poll) = poll {
                collect(&poll.request);
            }
            for stage in result_steps {
                collect(stage);
            }
        }
        AdvancedRecognition::RealtimeSession { .. } => {}
    }
    ids
}

fn runtime_values() -> RuntimeTemplateValues {
    let now = Utc::now();
    RuntimeTemplateValues {
        uuid: Some(Uuid::new_v4().to_string()),
        unix_seconds: Some(now.timestamp().to_string()),
        unix_millis: Some(now.timestamp_millis().to_string()),
    }
}

fn extract_final(
    response: &HttpResponseData,
    extractor: &ResponseExtractor,
) -> Result<String, AdvancedAudioError> {
    let view = ResponseData::new(response.status.as_u16(), &response.headers, &response.body);
    extractor::extract(&view, extractor).map_err(AdvancedAudioError::Extraction)
}

fn is_retryable(error: &AdvancedAudioError) -> bool {
    match error {
        AdvancedAudioError::Http(HttpEngineError::Request(_)) => true,
        AdvancedAudioError::UnexpectedStatus { status, .. } => {
            *status == 408 || *status == 429 || (500..=599).contains(status)
        }
        _ => false,
    }
}

/// Polling is the only workflow phase whose response-body failure may be
/// retried independently of submission. It repeats the configured status
/// check without ever re-running `submit`, and validation prohibits a poll
/// request from carrying or referring to audio.
///
/// Keep this deliberately narrower than a generic response-read retry:
/// cancellation and bounded-read failures describe local terminal states and
/// must return immediately.
fn is_poll_retryable(error: &AdvancedAudioError) -> bool {
    is_retryable(error)
        || matches!(
            error,
            AdvancedAudioError::ReadResponse(ResponseReadError::Body(_))
        )
}

fn is_stream_retryable(error: &AdvancedAudioError) -> bool {
    is_retryable(error) || matches!(error, AdvancedAudioError::StreamResponse(_))
}

enum PollState {
    Pending,
    Success,
    Failure,
}

fn poll_state(
    poll: &PollStage,
    response: &HttpResponseData,
) -> Result<PollState, AdvancedAudioError> {
    if matches_conditions(&poll.failure, response)? {
        return Ok(PollState::Failure);
    }
    if matches_conditions(&poll.success, response)? {
        return Ok(PollState::Success);
    }
    if matches_conditions(&poll.pending, response)? {
        return Ok(PollState::Pending);
    }
    Err(AdvancedAudioError::PollUnexpectedState)
}

fn matches_conditions(
    conditions: &[PollCondition],
    response: &HttpResponseData,
) -> Result<bool, AdvancedAudioError> {
    if conditions.is_empty() {
        return Ok(false);
    }
    conditions
        .iter()
        .map(|condition| condition_matches(condition, response))
        .collect::<Result<Vec<_>, _>>()
        .map(|matches| matches.into_iter().all(|value| value))
}

fn condition_matches(
    condition: &PollCondition,
    response: &HttpResponseData,
) -> Result<bool, AdvancedAudioError> {
    let view = ResponseData::new(response.status.as_u16(), &response.headers, &response.body);
    let value = extractor::extract(&view, &condition.from);
    match condition.operator {
        PollOperator::Exists => match value {
            Ok(_) => Ok(true),
            Err(error) if extraction_is_missing(&error) => Ok(false),
            Err(error) => Err(AdvancedAudioError::Extraction(error)),
        },
        PollOperator::NotExists => match value {
            Ok(_) => Ok(false),
            Err(error) if extraction_is_missing(&error) => Ok(true),
            Err(error) => Err(AdvancedAudioError::Extraction(error)),
        },
        PollOperator::Eq => Ok(value.map_err(AdvancedAudioError::Extraction)?
            == condition.value.as_deref().unwrap_or_default()),
        PollOperator::Ne => Ok(value.map_err(AdvancedAudioError::Extraction)?
            != condition.value.as_deref().unwrap_or_default()),
        PollOperator::IsTrue => Ok(value
            .map_err(AdvancedAudioError::Extraction)?
            .eq_ignore_ascii_case("true")),
        PollOperator::IsFalse => Ok(value
            .map_err(AdvancedAudioError::Extraction)?
            .eq_ignore_ascii_case("false")),
        PollOperator::In => match value {
            Ok(value) => Ok(condition.values.iter().any(|expected| &value == expected)),
            Err(error) if extraction_is_missing(&error) => Ok(false),
            Err(error) => Err(AdvancedAudioError::Extraction(error)),
        },
    }
}

fn extraction_is_missing(error: &ResponseExtractionError) -> bool {
    matches!(
        error,
        ResponseExtractionError::JsonPathNoMatch | ResponseExtractionError::HeaderMissing { .. }
    )
}

async fn next_stream_chunk<S>(
    stream: &mut S,
    cancellation: &CancellationToken,
) -> Result<Option<bytes::Bytes>, AdvancedAudioError>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin,
{
    tokio::select! {
        _ = cancellation.cancelled() => Err(AdvancedAudioError::Canceled),
        item = stream.next() => item.transpose().map_err(AdvancedAudioError::StreamResponse),
    }
}

fn append_raw(raw: &mut Vec<u8>, chunk: &[u8]) -> Result<(), AdvancedAudioError> {
    const MAX_STREAM_RAW_BYTES: usize = 32 * 1024 * 1024;
    if raw.len().saturating_add(chunk.len()) > MAX_STREAM_RAW_BYTES {
        return Err(AdvancedAudioError::StreamTooLarge);
    }
    raw.extend_from_slice(chunk);
    Ok(())
}

pub(crate) fn apply_stream_rules(
    schema: &StreamResponse,
    event_name: Option<&str>,
    data: &str,
    accumulator: &mut TranscriptAccumulator,
) -> Result<(), AdvancedAudioError> {
    for rule in &schema.rules {
        if rule
            .event
            .as_deref()
            .is_some_and(|expected| Some(expected) != event_name)
        {
            continue;
        }
        let value = match &rule.path {
            Some(path) => {
                let headers = reqwest::header::HeaderMap::new();
                let view = ResponseData::new(200, &headers, data.as_bytes());
                match extractor::extract(&view, &ResponseExtractor::JsonPath { path: path.clone() })
                {
                    // A stream commonly mixes frames: for example, delta
                    // frames do not carry `done`, and the terminal frame does
                    // not carry `delta`.  A rule with no value on this frame
                    // simply does not match; malformed JSON and other real
                    // extraction errors still stop the stream.
                    Err(ResponseExtractionError::JsonPathNoMatch) => continue,
                    Ok(value) => Some(value),
                    Err(error) => return Err(AdvancedAudioError::Extraction(error)),
                }
            }
            None => None,
        };
        if rule
            .equals
            .as_deref()
            .is_some_and(|expected| value.as_deref() != Some(expected))
        {
            continue;
        }
        let action = match rule.action {
            StreamAction::Ignore => TranscriptAction::Ignore,
            StreamAction::AppendDelta => TranscriptAction::append_delta(value.unwrap_or_default()),
            StreamAction::ReplacePartial => {
                TranscriptAction::replace_partial(value.unwrap_or_default())
            }
            StreamAction::CommitSegment => {
                TranscriptAction::commit_segment(value.unwrap_or_default())
            }
            StreamAction::SetFinalText => {
                TranscriptAction::set_final_text(value.unwrap_or_default())
            }
            StreamAction::Complete => TranscriptAction::Complete,
            StreamAction::Fail => {
                TranscriptAction::fail(value.unwrap_or_else(|| "server stream failure".into()))
            }
        };
        accumulator
            .apply(action)
            .map_err(AdvancedAudioError::Accumulator)?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum AdvancedAudioError {
    #[error("Advanced Audio API is disabled")]
    Disabled,
    #[error("Advanced Audio API is enabled but has no workflow")]
    MissingWorkflow,
    #[error("invalid Advanced Audio API configuration: {0}")]
    InvalidConfig(ValidationErrors),
    // reqwest can include proxy or endpoint details in its Display output.
    // Keep sources for debugging APIs but expose a stable, credential-safe
    // message to Runtime and GUI.
    #[error("failed to create HTTP client")]
    Client(#[source] reqwest::Error),
    #[error("{0}")]
    Audio(#[from] BodyError),
    #[error("{0}")]
    Http(#[from] HttpEngineError),
    #[error("failed to read HTTP response")]
    ReadResponse(#[source] ResponseReadError),
    #[error("HTTP {status} was not accepted by the workflow")]
    UnexpectedStatus { status: u16, response: Vec<u8> },
    #[error("failed to extract workflow response: {0}")]
    Extraction(#[source] ResponseExtractionError),
    #[error("stream framing failed: {0}")]
    StreamFrame(#[source] StreamFrameError),
    #[error("stream response failed")]
    StreamResponse(#[source] reqwest::Error),
    #[error("stream response exceeds the configured limit")]
    StreamTooLarge,
    #[error("stream transcript is not complete: {0}")]
    Accumulator(#[source] TranscriptAccumulatorError),
    #[error("request canceled")]
    Canceled,
    #[error("remote audio hosting is required by this workflow but is not available")]
    RemoteAudioUnavailable,
    #[error("remote audio hosting failed: {0}")]
    RemoteAudio(#[source] super::remote_audio::RemoteAudioError),
    #[error("realtime session support is not available")]
    RealtimeUnavailable,
    #[error("the configured Advanced Audio API workflow is not a realtime session")]
    NotRealtimeWorkflow,
    #[error("realtime session failed: {0}")]
    Realtime(#[source] super::realtime::RealtimeSessionError),
    #[error("asynchronous task entered a failure state")]
    PollFailure,
    #[error("asynchronous task response did not match any pending, success, or failure condition")]
    PollUnexpectedState,
    #[error("asynchronous task did not complete within {timeout_ms}ms")]
    PollTimeout { timeout_ms: u64 },
    #[error("Advanced Audio API execution failed: {0}")]
    Execution(String),
}

impl AdvancedAudioError {
    pub fn last_response(&self) -> &[u8] {
        match self {
            Self::UnexpectedStatus { response, .. } => response,
            _ => &[],
        }
    }

    pub fn is_canceled(&self) -> bool {
        matches!(self, Self::Canceled)
            || matches!(self, Self::Realtime(error) if error.is_canceled())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::protocol::Message;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::advanced_audio::schema::{
        AudioSpec, PauseBehavior, RealtimeAudioMessage, RealtimeAudioStream, RealtimeCompletion,
        RealtimeConnect, RealtimeMessage, RealtimePacing, RealtimeTransport, RealtimeWorkflow,
        SignerConfig, StreamAction, StreamRule,
    };
    use crate::advanced_audio::{
        AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery,
        WorkflowSchemaVersion,
    };
    use crate::{Config, advanced_audio::CURRENT_SCHEMA_VERSION};

    fn replay_client(url: String, max_retry: i32, retry_base_delay: f64) -> AdvancedAudioClient {
        replay_client_with_audio_message(
            url,
            max_retry,
            retry_base_delay,
            RealtimeAudioMessage::Binary,
        )
    }

    fn replay_client_with_audio_message(
        url: String,
        max_retry: i32,
        retry_base_delay: f64,
        audio_message: RealtimeAudioMessage,
    ) -> AdvancedAudioClient {
        let workflow = AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            name: "replay test".into(),
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
                        url,
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
                    audio_message,
                    receive_rules: vec![
                        StreamRule {
                            event: Some("partial".into()),
                            path: Some("$.text".into()),
                            action: StreamAction::AppendDelta,
                            equals: None,
                        },
                        StreamRule {
                            event: Some("final".into()),
                            path: Some("$.text".into()),
                            action: StreamAction::SetFinalText,
                            equals: None,
                        },
                        StreamRule {
                            event: Some("done".into()),
                            path: None,
                            action: StreamAction::Complete,
                            equals: None,
                        },
                    ],
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
        let mut config = Config {
            max_retry,
            retry_base_delay,
            ..Config::default()
        };
        config.advanced_audio_api = AdvancedAudioConfig {
            enabled: true,
            workflow: Some(workflow),
            ..AdvancedAudioConfig::default()
        };
        AdvancedAudioClient::new(config).unwrap()
    }

    fn write_replay_wav(directory: &tempfile::TempDir) -> std::path::PathBuf {
        let path = directory.path().join("replay.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for sample in 0..320_i16 {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    #[tokio::test]
    async fn realtime_replay_retries_from_start_and_discards_partial_text() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut first_chunks = Vec::new();
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_async(stream).await.unwrap();
                loop {
                    let message = socket.next().await.unwrap().unwrap();
                    if let Message::Binary(bytes) = message {
                        first_chunks.push(bytes.to_vec());
                        if attempt == 0 {
                            socket
                                .send(Message::Text(
                                    r#"{"event":"partial","text":"discard"}"#.into(),
                                ))
                                .await
                                .unwrap();
                            socket.close(None).await.unwrap();
                        } else {
                            socket
                                .send(Message::Text(
                                    r#"{"event":"final","text":"complete"}"#.into(),
                                ))
                                .await
                                .unwrap();
                            socket
                                .send(Message::Text(r#"{"event":"done"}"#.into()))
                                .await
                                .unwrap();
                        }
                        break;
                    }
                }
            }
            first_chunks
        });
        let directory = tempfile::tempdir().unwrap();
        let wav = write_replay_wav(&directory);
        let client = replay_client(format!("ws://{address}"), 2, 0.0);

        let transcription = client
            .transcribe(&CancellationToken::new(), &wav)
            .await
            .unwrap();
        let chunks = server.await.unwrap();

        assert_eq!(transcription.text, "complete");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], chunks[1]);
        assert!(!chunks[0].is_empty());
    }

    #[tokio::test]
    async fn realtime_json_audio_message_encodes_chunk_as_base64() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected JSON audio message as text frame");
            };
            let payload: serde_json::Value = serde_json::from_str(&text).unwrap();
            let audio = STANDARD.decode(payload["audio"].as_str().unwrap()).unwrap();
            let mut expected_audio = Vec::new();
            for sample in 0_i16..160 {
                expected_audio.extend_from_slice(&sample.to_le_bytes());
            }

            assert_eq!(payload["format"].as_str(), Some("pcm_s16le"));
            assert_eq!(audio, expected_audio);

            socket
                .send(Message::Text(
                    r#"{"event":"final","text":"json base64 complete"}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(r#"{"event":"done"}"#.into()))
                .await
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let wav = write_replay_wav(&directory);
        let client = replay_client_with_audio_message(
            format!("ws://{address}"),
            1,
            0.0,
            RealtimeAudioMessage::Json {
                value: serde_json::json!({
                    "audio": "{{audio:chunk_base64}}",
                    "format": "pcm_s16le",
                }),
            },
        );

        let transcription = client
            .transcribe(&CancellationToken::new(), &wav)
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(transcription.text, "json base64 complete");
    }

    #[tokio::test]
    async fn realtime_websocket_orders_control_messages_and_committed_events() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut sequence = Vec::new();

            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected the first initial message as a text frame");
            };
            assert_eq!(text, "open-session");
            sequence.push("initial_text");

            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected the second initial message as a JSON text frame");
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                serde_json::json!({"event": "configure", "language": "en"})
            );
            sequence.push("initial_json");

            let Message::Binary(first_chunk) = socket.next().await.unwrap().unwrap() else {
                panic!("expected audio after all initial messages");
            };
            assert!(!first_chunk.is_empty());
            sequence.push("audio");
            socket
                .send(Message::Text(
                    r#"{"event":"committed","text":"first "}"#.into(),
                ))
                .await
                .unwrap();

            let Message::Binary(second_chunk) = socket.next().await.unwrap().unwrap() else {
                panic!("expected the second audio chunk before finish messages");
            };
            assert!(!second_chunk.is_empty());
            sequence.push("audio");

            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected the first finish message as a JSON text frame");
            };
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&text).unwrap(),
                serde_json::json!({"event": "finish"})
            );
            sequence.push("finish_json");

            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected the second finish message as a text frame");
            };
            assert_eq!(text, "commit-session");
            sequence.push("finish_text");

            socket
                .send(Message::Text(
                    r#"{"event":"committed","text":"second"}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(r#"{"event":"done"}"#.into()))
                .await
                .unwrap();
            sequence
        });

        let directory = tempfile::tempdir().unwrap();
        let wav = write_replay_wav(&directory);
        let mut client = replay_client(format!("ws://{address}"), 1, 0.0);
        let AdvancedRecognition::RealtimeSession { realtime } = &mut client.workflow.recognition
        else {
            panic!("replay test client must have a realtime workflow");
        };
        realtime.initial_messages = vec![
            RealtimeMessage::Text {
                value: "open-session".into(),
            },
            RealtimeMessage::Json {
                value: serde_json::json!({"event": "configure", "language": "en"}),
            },
        ];
        realtime.finish_messages = vec![
            RealtimeMessage::Json {
                value: serde_json::json!({"event": "finish"}),
            },
            RealtimeMessage::Text {
                value: "commit-session".into(),
            },
        ];
        realtime.receive_rules = vec![
            StreamRule {
                event: Some("committed".into()),
                path: Some("$.text".into()),
                action: StreamAction::CommitSegment,
                equals: None,
            },
            StreamRule {
                event: Some("done".into()),
                path: None,
                action: StreamAction::Complete,
                equals: None,
            },
        ];

        let transcription = client
            .transcribe(&CancellationToken::new(), &wav)
            .await
            .unwrap();
        let sequence = server.await.unwrap();

        assert_eq!(transcription.text, "first second");
        assert_eq!(
            sequence,
            vec![
                "initial_text",
                "initial_json",
                "audio",
                "audio",
                "finish_json",
                "finish_text",
            ]
        );
    }

    #[tokio::test]
    async fn realtime_replay_retry_wait_is_cancellable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (connection_closed, closed) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream);
            let _ = connection_closed.send(());
        });
        let directory = tempfile::tempdir().unwrap();
        let wav = write_replay_wav(&directory);
        let client = replay_client(format!("ws://{address}"), 2, 60.0);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move { client.transcribe(&task_cancellation, &wav).await });

        closed.await.unwrap();
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_millis(250), task)
            .await
            .expect("cancellation must interrupt realtime retry waiting")
            .unwrap();
        server.await.unwrap();

        assert!(matches!(result, Err(AdvancedAudioError::Canceled)));
    }

    #[test]
    fn stream_rules_skip_absent_fields_until_their_matching_frame_arrives() {
        let schema = StreamResponse {
            format: StreamFormat::Ndjson,
            rules: vec![
                StreamRule {
                    event: None,
                    path: Some("$.delta".into()),
                    action: StreamAction::AppendDelta,
                    equals: None,
                },
                StreamRule {
                    event: None,
                    path: Some("$.done".into()),
                    action: StreamAction::Complete,
                    equals: Some("true".into()),
                },
            ],
        };
        let mut accumulator = TranscriptAccumulator::new();
        apply_stream_rules(&schema, None, r#"{"delta":"he"}"#, &mut accumulator).unwrap();
        apply_stream_rules(&schema, None, r#"{"done":true}"#, &mut accumulator).unwrap();
        assert_eq!(accumulator.into_final_text().unwrap(), "he");
    }

    #[test]
    fn poll_existence_conditions_propagate_invalid_response_errors() {
        let response = HttpResponseData {
            status: reqwest::StatusCode::OK,
            headers: reqwest::header::HeaderMap::new(),
            body: b"not json".to_vec(),
        };
        let condition = PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Exists,
            value: None,
            values: vec![],
        };
        assert!(matches!(
            condition_matches(&condition, &response),
            Err(AdvancedAudioError::Extraction(_))
        ));
    }

    #[test]
    fn poll_body_retry_excludes_cancellation_and_response_limits() {
        assert!(!is_poll_retryable(&AdvancedAudioError::Canceled));
        assert!(!is_poll_retryable(&AdvancedAudioError::ReadResponse(
            ResponseReadError::Canceled,
        )));
        assert!(!is_poll_retryable(&AdvancedAudioError::ReadResponse(
            ResponseReadError::TooLarge { maximum: 1 },
        )));
    }

    #[test]
    fn sensitive_captures_are_redacted_before_an_error_response_can_escape() {
        let workflow = AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            name: "sensitive capture test".into(),
            parameters: vec![],
            secrets: vec![],
            audio: AudioSpec {
                delivery: AudioDelivery::Base64,
                mime: Some("audio/wav".into()),
            },
            recognition: AdvancedRecognition::Request {
                request: Box::new(HttpStage {
                    method: HttpMethod::Post,
                    url: "https://asr.example.test/request".into(),
                    query: BTreeMap::new(),
                    headers: BTreeMap::new(),
                    body: HttpBody::Json {
                        value: serde_json::json!({"audio": "{{audio:base64}}"}),
                    },
                    accepted_statuses: vec![200],
                    signer: SignerConfig::None,
                    captures: vec![
                        Capture {
                            id: "temporary_url".into(),
                            from: ResponseExtractor::Header {
                                name: "X-Temporary-URL".into(),
                            },
                            sensitive: true,
                        },
                        Capture {
                            id: "job_id".into(),
                            from: ResponseExtractor::JsonPath {
                                path: "$.job_id".into(),
                            },
                            sensitive: false,
                        },
                    ],
                }),
                final_text: ResponseExtractor::PlainBody,
            },
        };
        let client = AdvancedAudioClient::new(Config {
            advanced_audio_api: AdvancedAudioConfig {
                enabled: true,
                workflow: Some(workflow),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        let captures = BTreeMap::from([(
            "temporary_url".into(),
            "https://upload.example.test/?signature=private-token".into(),
        )]);
        let output = client.redact_sensitive_capture_response(
            b"provider echoed https://upload.example.test/?signature=private-token",
            &captures,
        );
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("private-token"));
        assert!(output.contains("[redacted]"));
        assert_eq!(
            client.redact_sensitive_capture_response(&[0xff, 0x00, 0xfe], &captures),
            [0xff, 0x00, 0xfe]
        );

        let ordinary_capture = BTreeMap::from([("job_id".into(), "ordinary-job-id".into())]);
        assert!(!client.sensitive_capture_ids.contains("job_id"));
        assert_eq!(
            client.redact_sensitive_capture_response(
                b"provider echoed ordinary-job-id",
                &ordinary_capture,
            ),
            b"provider echoed ordinary-job-id"
        );
    }
}
