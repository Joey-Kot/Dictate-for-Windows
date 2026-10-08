use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::FutureExt;
use thiserror::Error;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_util::sync::CancellationToken;

use super::source::{AudioChunk, RealtimeChunkSource, RealtimeSourceError, RecordedReplaySource};
use super::websocket::{WebSocketError, WebSocketTransport};
use crate::advanced_audio::LEGACY_SCHEMA_VERSION;
use crate::advanced_audio::accumulator::{TranscriptAccumulator, TranscriptAccumulatorError};
use crate::advanced_audio::client::apply_stream_rules;
use crate::advanced_audio::extractor::{self, ResponseData};
use crate::advanced_audio::http::{
    TypedTemplateRenderError, render_json_with_context, render_text_with_context,
};
use crate::advanced_audio::schema::{
    ParameterDefinition, RealtimeAudioMessage, RealtimeCompletion, RealtimeMessage,
    RealtimeWorkflow, ResponseExtractor, StreamFormat, StreamResponse, WorkflowSchemaVersion,
};
use crate::advanced_audio::template::{
    AudioTemplateValues, RuntimeTemplateValues, TemplateContext, TemplateError,
};
use crate::asr::Transcription;

const MAX_RAW_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Runs one realtime session from an already-recorded WAV. This is used for
/// CLI `--file`, workflow test audio, realtime retry, and live-failure replay.
pub async fn run_recorded_replay(
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    path: impl AsRef<std::path::Path>,
    cancellation: &CancellationToken,
) -> Result<Transcription, RealtimeSessionError> {
    run_recorded_replay_with_parameters(
        realtime,
        values,
        secrets,
        runtime,
        &[],
        WorkflowSchemaVersion(LEGACY_SCHEMA_VERSION),
        path,
        cancellation,
    )
    .await
}

/// Runs a replay with the owning workflow's parameter declarations.  The
/// public legacy wrapper intentionally retains v1 string rendering for
/// callers that only have a standalone realtime definition.
pub(crate) async fn run_recorded_replay_with_parameters(
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    path: impl AsRef<std::path::Path>,
    cancellation: &CancellationToken,
) -> Result<Transcription, RealtimeSessionError> {
    let mut source = RecordedReplaySource::from_wav(path, &realtime.audio_stream)?;
    run_realtime_session_with_parameters(
        realtime,
        values,
        secrets,
        runtime,
        parameters,
        schema_version,
        &mut source,
        cancellation,
    )
    .await
}

/// Runs a single WebSocket realtime recognition session.
///
/// The caller can supply either a recorded replay source or the recorder's
/// optional live packet source. Partial results stay in the accumulator and
/// only a completed final transcript is returned.
pub async fn run_realtime_session<S: RealtimeChunkSource + ?Sized>(
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    source: &mut S,
    cancellation: &CancellationToken,
) -> Result<Transcription, RealtimeSessionError> {
    run_realtime_session_with_parameters(
        realtime,
        values,
        secrets,
        runtime,
        &[],
        WorkflowSchemaVersion(LEGACY_SCHEMA_VERSION),
        source,
        cancellation,
    )
    .await
}

/// Runs one realtime session with the parameter declarations of its owning
/// workflow.  Only this path enables v2 JSON-native parameter leaves.
pub(crate) async fn run_realtime_session_with_parameters<S: RealtimeChunkSource + ?Sized>(
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    source: &mut S,
    cancellation: &CancellationToken,
) -> Result<Transcription, RealtimeSessionError> {
    let mut transport = WebSocketTransport::connect(
        &realtime.connect,
        values,
        secrets,
        runtime,
        parameters,
        schema_version,
        cancellation,
    )
    .await?;
    let result = run_connected(
        &mut transport,
        realtime,
        values,
        secrets,
        runtime,
        parameters,
        schema_version,
        source,
        cancellation,
    )
    .await;
    transport.close().await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_connected<S: RealtimeChunkSource + ?Sized>(
    transport: &mut WebSocketTransport,
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    source: &mut S,
    cancellation: &CancellationToken,
) -> Result<Transcription, RealtimeSessionError> {
    let mut accumulator = TranscriptAccumulator::new();
    let mut raw_response = Vec::new();
    for message in &realtime.initial_messages {
        send_message(
            transport,
            message,
            values,
            secrets,
            runtime,
            parameters,
            schema_version,
            None,
            cancellation,
        )
        .await?;
    }

    while let Some(chunk) = source.next_chunk(cancellation).await? {
        send_audio(
            transport,
            &realtime.audio_message,
            values,
            secrets,
            runtime,
            parameters,
            schema_version,
            &chunk,
            cancellation,
        )
        .await?;
        if source.requires_realtime_pacing() {
            // Recorded replay is decoded ahead of its media timeline, so it
            // must wait here. A live source already becomes ready only as the
            // recorder captures audio and must not be delayed a second time.
            // The paced receive loop also lets an early server completion
            // stop replay before another audio frame is sent.
            wait_with_messages(
                transport,
                realtime,
                values,
                secrets,
                runtime,
                &mut accumulator,
                &mut raw_response,
                Duration::from_millis(chunk.duration_ms),
                cancellation,
            )
            .await?;
        } else {
            // Do not block microphone streaming, but consume responses that
            // have already arrived. This keeps live sessions receive-capable
            // without adding another chunk-duration delay to capture.
            drain_available_messages(
                transport,
                realtime,
                values,
                secrets,
                runtime,
                &mut accumulator,
                &mut raw_response,
                cancellation,
            )?;
        }
        if accumulator.is_complete() {
            return completed_transcription(accumulator, raw_response);
        }
    }

    for message in &realtime.finish_messages {
        send_message(
            transport,
            message,
            values,
            secrets,
            runtime,
            parameters,
            schema_version,
            None,
            cancellation,
        )
        .await?;
    }
    wait_until_complete(
        transport,
        realtime,
        values,
        secrets,
        runtime,
        &mut accumulator,
        &mut raw_response,
        Duration::from_millis(realtime.finalization_timeout_ms),
        cancellation,
    )
    .await?;
    completed_transcription(accumulator, raw_response)
}

#[allow(clippy::too_many_arguments)]
fn drain_available_messages(
    transport: &mut WebSocketTransport,
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    accumulator: &mut TranscriptAccumulator,
    raw_response: &mut Vec<u8>,
    cancellation: &CancellationToken,
) -> Result<(), RealtimeSessionError> {
    loop {
        // `now_or_never` polls the WebSocket once and drops the receive
        // future if it would wait. Unlike a zero/short timeout, it cannot
        // introduce artificial delay into the live capture path.
        let Some(received) = transport.receive(cancellation).now_or_never() else {
            return Ok(());
        };
        match received {
            Ok(Some(message)) => process_incoming(
                message,
                realtime,
                values,
                secrets,
                runtime,
                accumulator,
                raw_response,
            )?,
            Ok(None) => return Err(RealtimeSessionError::ClosedBeforeComplete),
            Err(error) => return Err(error.into()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn wait_with_messages(
    transport: &mut WebSocketTransport,
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    accumulator: &mut TranscriptAccumulator,
    raw_response: &mut Vec<u8>,
    duration: Duration,
    cancellation: &CancellationToken,
) -> Result<(), RealtimeSessionError> {
    let deadline = tokio::time::Instant::now() + duration;
    while !accumulator.is_complete() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        match tokio::time::timeout(remaining, transport.receive(cancellation)).await {
            Err(_) => return Ok(()),
            Ok(Ok(Some(message))) => process_incoming(
                message,
                realtime,
                values,
                secrets,
                runtime,
                accumulator,
                raw_response,
            )?,
            Ok(Ok(None)) => return Err(RealtimeSessionError::ClosedBeforeComplete),
            Ok(Err(error)) => return Err(error.into()),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn wait_until_complete(
    transport: &mut WebSocketTransport,
    realtime: &RealtimeWorkflow,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    accumulator: &mut TranscriptAccumulator,
    raw_response: &mut Vec<u8>,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<(), RealtimeSessionError> {
    let deadline = tokio::time::Instant::now() + timeout;
    while !accumulator.is_complete() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(RealtimeSessionError::FinalizationTimeout);
        }
        match tokio::time::timeout(remaining, transport.receive(cancellation)).await {
            Err(_) => return Err(RealtimeSessionError::FinalizationTimeout),
            Ok(Ok(Some(message))) => process_incoming(
                message,
                realtime,
                values,
                secrets,
                runtime,
                accumulator,
                raw_response,
            )?,
            Ok(Ok(None)) => return Err(RealtimeSessionError::ClosedBeforeComplete),
            Ok(Err(error)) => return Err(error.into()),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_incoming(
    message: Message,
    realtime: &RealtimeWorkflow,
    _values: &BTreeMap<String, String>,
    _secrets: &BTreeMap<String, String>,
    _runtime: &RuntimeTemplateValues,
    accumulator: &mut TranscriptAccumulator,
    raw_response: &mut Vec<u8>,
) -> Result<(), RealtimeSessionError> {
    let data = match message {
        Message::Text(text) => text.to_string(),
        Message::Binary(bytes) => std::str::from_utf8(&bytes)
            .map(str::to_owned)
            .map_err(|_| RealtimeSessionError::InvalidIncomingText)?,
        Message::Close(_) => return Err(RealtimeSessionError::ClosedBeforeComplete),
        Message::Ping(_) | Message::Pong(_) => return Ok(()),
        _ => return Ok(()),
    };
    append_raw(raw_response, data.as_bytes())?;
    let event_name = websocket_event_name(&data);
    let schema = StreamResponse {
        format: StreamFormat::JsonChunks,
        rules: realtime.receive_rules.clone(),
    };
    apply_stream_rules(&schema, event_name.as_deref(), &data, accumulator)
        .map_err(|_| RealtimeSessionError::ProtocolEvent)?;
    if !accumulator.is_complete()
        && completion_matches(&realtime.completion, event_name.as_deref(), &data)?
    {
        accumulator.complete()?;
    }
    Ok(())
}

fn websocket_event_name(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    ["event", "type"].into_iter().find_map(|key| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
}

fn completion_matches(
    completion: &RealtimeCompletion,
    event_name: Option<&str>,
    data: &str,
) -> Result<bool, RealtimeSessionError> {
    if completion.event.is_none() && completion.path.is_none() && completion.equals.is_none() {
        return Ok(false);
    }
    if completion
        .event
        .as_deref()
        .is_some_and(|event| event_name != Some(event))
    {
        return Ok(false);
    }
    let value = match &completion.path {
        None => None,
        Some(path) => {
            let headers = reqwest::header::HeaderMap::new();
            let response = ResponseData::new(200, &headers, data.as_bytes());
            Some(
                extractor::extract(
                    &response,
                    &ResponseExtractor::JsonPath { path: path.clone() },
                )
                .map_err(|_| RealtimeSessionError::ProtocolEvent)?,
            )
        }
    };
    Ok(completion
        .equals
        .as_deref()
        .is_none_or(|expected| value.as_deref() == Some(expected)))
}

async fn send_message(
    transport: &mut WebSocketTransport,
    message: &RealtimeMessage,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    chunk: Option<&AudioChunk>,
    cancellation: &CancellationToken,
) -> Result<(), RealtimeSessionError> {
    match message {
        RealtimeMessage::Json { value } => {
            let text = render_json(
                value,
                values,
                secrets,
                runtime,
                parameters,
                schema_version,
                chunk,
            )?
            .to_string();
            transport.send_text(text, cancellation).await?;
        }
        RealtimeMessage::Text { value } => {
            transport
                .send_text(
                    render_text(
                        value,
                        values,
                        secrets,
                        runtime,
                        parameters,
                        schema_version,
                        chunk,
                    )?,
                    cancellation,
                )
                .await?;
        }
        RealtimeMessage::Binary { value } => {
            let encoded = render_text(
                value,
                values,
                secrets,
                runtime,
                parameters,
                schema_version,
                chunk,
            )?;
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| RealtimeSessionError::InvalidBinaryTemplate)?;
            transport.send_binary(bytes, cancellation).await?;
        }
    }
    Ok(())
}

async fn send_audio(
    transport: &mut WebSocketTransport,
    message: &RealtimeAudioMessage,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    chunk: &AudioChunk,
    cancellation: &CancellationToken,
) -> Result<(), RealtimeSessionError> {
    match message {
        RealtimeAudioMessage::Binary => {
            transport
                .send_binary(chunk.bytes.clone(), cancellation)
                .await?
        }
        RealtimeAudioMessage::Text { value } => {
            transport
                .send_text(
                    render_text(
                        value,
                        values,
                        secrets,
                        runtime,
                        parameters,
                        schema_version,
                        Some(chunk),
                    )?,
                    cancellation,
                )
                .await?
        }
        RealtimeAudioMessage::Json { value } => {
            transport
                .send_text(
                    render_json(
                        value,
                        values,
                        secrets,
                        runtime,
                        parameters,
                        schema_version,
                        Some(chunk),
                    )?
                    .to_string(),
                    cancellation,
                )
                .await?
        }
    }
    Ok(())
}

fn render_text(
    value: &str,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    chunk: Option<&AudioChunk>,
) -> Result<String, RealtimeSessionError> {
    let captures = BTreeMap::new();
    let audio = AudioTemplateValues {
        chunk_base64: chunk.map(|chunk| STANDARD.encode(&chunk.bytes)),
        ..Default::default()
    };
    let context = TemplateContext {
        values,
        secrets,
        captures: &captures,
        audio: &audio,
        runtime,
    };
    render_text_with_context(value, &context, parameters, schema_version)
        .map_err(map_template_render_error)
}

fn render_json(
    value: &serde_json::Value,
    values: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
    runtime: &RuntimeTemplateValues,
    parameters: &[ParameterDefinition],
    schema_version: WorkflowSchemaVersion,
    chunk: Option<&AudioChunk>,
) -> Result<serde_json::Value, RealtimeSessionError> {
    let captures = BTreeMap::new();
    let audio = AudioTemplateValues {
        chunk_base64: chunk.map(|chunk| STANDARD.encode(&chunk.bytes)),
        ..Default::default()
    };
    let context = TemplateContext {
        values,
        secrets,
        captures: &captures,
        audio: &audio,
        runtime,
    };
    render_json_with_context(value, &context, parameters, schema_version)
        .map_err(map_template_render_error)
}

fn map_template_render_error(error: TypedTemplateRenderError) -> RealtimeSessionError {
    match error {
        TypedTemplateRenderError::Template(error) => RealtimeSessionError::Template(error),
        error => RealtimeSessionError::TypedTemplate(error.to_string()),
    }
}

fn append_raw(raw: &mut Vec<u8>, value: &[u8]) -> Result<(), RealtimeSessionError> {
    if raw.len().saturating_add(value.len()) > MAX_RAW_RESPONSE_BYTES {
        return Err(RealtimeSessionError::ResponseTooLarge);
    }
    raw.extend_from_slice(value);
    Ok(())
}

fn completed_transcription(
    accumulator: TranscriptAccumulator,
    raw_response: Vec<u8>,
) -> Result<Transcription, RealtimeSessionError> {
    Ok(Transcription {
        text: accumulator.into_final_text()?,
        raw_response,
    })
}

#[derive(Debug, Error)]
pub enum RealtimeSessionError {
    #[error("{0}")]
    Source(#[from] RealtimeSourceError),
    #[error("{0}")]
    WebSocket(#[from] WebSocketError),
    #[error("realtime session template failed: {0}")]
    Template(#[source] TemplateError),
    #[error("realtime typed parameter template failed: {0}")]
    TypedTemplate(String),
    #[error("realtime binary message template must render Base64")]
    InvalidBinaryTemplate,
    #[error("realtime server sent a non-text transcript event")]
    InvalidIncomingText,
    #[error("realtime server closed before an explicit completion event")]
    ClosedBeforeComplete,
    #[error("realtime server did not complete before finalization timeout")]
    FinalizationTimeout,
    #[error("realtime transcript event does not match the workflow rules")]
    ProtocolEvent,
    #[error("realtime response exceeds the configured limit")]
    ResponseTooLarge,
    #[error("realtime session was canceled")]
    Canceled,
    #[error("{0}")]
    Accumulator(#[from] TranscriptAccumulatorError),
}

impl RealtimeSessionError {
    pub fn is_canceled(&self) -> bool {
        matches!(
            self,
            Self::Canceled
                | Self::Source(RealtimeSourceError::Canceled)
                | Self::WebSocket(WebSocketError::Canceled)
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::time::{Duration, Instant};

    use async_trait::async_trait;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::protocol::Message;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::advanced_audio::schema::{
        ParameterDefinition, ParameterOption, ParameterType, PauseBehavior, RealtimeAudioMessage,
        RealtimeAudioStream, RealtimeCompletion, RealtimeConnect, RealtimeMessage, RealtimePacing,
        RealtimeTransport, SignerConfig, StreamAction, StreamRule,
    };
    use crate::advanced_audio::{CURRENT_SCHEMA_VERSION, LEGACY_SCHEMA_VERSION};

    struct FixedChunkSource {
        chunks: VecDeque<AudioChunk>,
        requires_pacing: bool,
        ready: Option<oneshot::Receiver<()>>,
    }

    #[async_trait]
    impl RealtimeChunkSource for FixedChunkSource {
        async fn next_chunk(
            &mut self,
            cancellation: &CancellationToken,
        ) -> Result<Option<AudioChunk>, RealtimeSourceError> {
            if cancellation.is_cancelled() {
                return Err(RealtimeSourceError::Canceled);
            }
            if let Some(ready) = self.ready.take() {
                let _ = ready.await;
            }
            Ok(self.chunks.pop_front())
        }

        fn requires_realtime_pacing(&self) -> bool {
            self.requires_pacing
        }
    }

    fn workflow(url: String) -> RealtimeWorkflow {
        RealtimeWorkflow {
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
                chunk_duration_ms: 500,
                pacing: RealtimePacing::Realtime,
            },
            audio_message: RealtimeAudioMessage::Binary,
            receive_rules: vec![
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
            finish_messages: vec![RealtimeMessage::Text {
                value: "finish".into(),
            }],
            completion: RealtimeCompletion {
                event: None,
                path: None,
                equals: None,
            },
            pause_behavior: PauseBehavior::RestartSession,
            finalization_timeout_ms: 1_000,
        }
    }

    fn parameter(id: &str, parameter_type: ParameterType) -> ParameterDefinition {
        ParameterDefinition {
            id: id.into(),
            label: id.into(),
            required: false,
            default: None,
            description: None,
            parameter_type: Some(parameter_type),
            options: vec![],
            visible_when: None,
        }
    }

    #[test]
    fn realtime_json_uses_native_v2_parameter_values_for_initial_and_audio_messages() {
        let values = BTreeMap::from([
            ("sample_rate".into(), "16000".into()),
            ("partial_results".into(), "true".into()),
            ("languages".into(), r#"["zh","en"]"#.into()),
            (
                "vocabulary".into(),
                r#"{"wake_phrase":"redacted-vocabulary","boost":2}"#.into(),
            ),
            (
                "language_hints".into(),
                r#"[{"locale":"zh-CN"},"en-US"]"#.into(),
            ),
        ]);
        let secrets = BTreeMap::new();
        let runtime = RuntimeTemplateValues::default();
        let mut languages = parameter("languages", ParameterType::MultiSelect);
        languages.options = vec![
            ParameterOption {
                value: "zh".into(),
                label: "Chinese".into(),
            },
            ParameterOption {
                value: "en".into(),
                label: "English".into(),
            },
        ];
        let parameters = vec![
            parameter("sample_rate", ParameterType::Integer),
            parameter("partial_results", ParameterType::Boolean),
            languages,
            parameter("vocabulary", ParameterType::JsonObject),
            parameter("language_hints", ParameterType::JsonArray),
        ];

        let initial = render_json(
            &serde_json::json!({
                "sample_rate": "{{var:sample_rate}}",
                "vocabulary": "{{var:vocabulary}}",
            }),
            &values,
            &secrets,
            &runtime,
            &parameters,
            WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            None,
        )
        .unwrap();
        let audio = render_json(
            &serde_json::json!({
                "partial_results": "{{var:partial_results}}",
                "languages": "{{var:languages}}",
                "language_hints": "{{var:language_hints}}",
                "audio": "{{audio:chunk_base64}}",
            }),
            &values,
            &secrets,
            &runtime,
            &parameters,
            WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            Some(&AudioChunk {
                bytes: vec![1, 2],
                duration_ms: 20,
            }),
        )
        .unwrap();

        assert_eq!(
            initial,
            serde_json::json!({
                "sample_rate": 16000,
                "vocabulary": {"wake_phrase": "redacted-vocabulary", "boost": 2},
            })
        );
        assert_eq!(
            audio,
            serde_json::json!({
                "partial_results": true,
                "languages": ["zh", "en"],
                "language_hints": [{"locale": "zh-CN"}, "en-US"],
                "audio": "AQI=",
            })
        );

        let legacy = render_json(
            &serde_json::json!({"sample_rate": "{{var:sample_rate}}"}),
            &values,
            &secrets,
            &runtime,
            &parameters,
            WorkflowSchemaVersion(LEGACY_SCHEMA_VERSION),
            None,
        )
        .unwrap();
        assert_eq!(legacy, serde_json::json!({"sample_rate": "16000"}));
    }

    #[test]
    fn realtime_text_rejects_multi_select_without_exposing_its_value() {
        let values = BTreeMap::from([("languages".into(), r#"["zh","en"]"#.into())]);
        let secrets = BTreeMap::new();
        let runtime = RuntimeTemplateValues::default();
        let mut languages = parameter("languages", ParameterType::MultiSelect);
        languages.options = vec![
            ParameterOption {
                value: "zh".into(),
                label: "Chinese".into(),
            },
            ParameterOption {
                value: "en".into(),
                label: "English".into(),
            },
        ];

        let error = render_text(
            "languages={{var:languages}}",
            &values,
            &secrets,
            &runtime,
            &[languages],
            WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
            None,
        )
        .unwrap_err();

        match error {
            RealtimeSessionError::TypedTemplate(message) => {
                assert!(message.contains("languages"));
                assert!(!message.contains("zh"));
                assert!(!message.contains("en"));
            }
            error => panic!("expected typed template error, got {error:?}"),
        }
    }

    #[test]
    fn realtime_text_and_binary_reject_json_object_and_array_without_exposing_values() {
        let vocabulary = r#"{"wake_phrase":"redacted-vocabulary"}"#;
        let language_hints = r#"["redacted-language-hint"]"#;
        let values = BTreeMap::from([
            ("vocabulary".into(), vocabulary.into()),
            ("language_hints".into(), language_hints.into()),
        ]);
        let secrets = BTreeMap::new();
        let runtime = RuntimeTemplateValues::default();
        let parameters = [
            parameter("vocabulary", ParameterType::JsonObject),
            parameter("language_hints", ParameterType::JsonArray),
        ];

        // Text and binary realtime message payloads both call render_text
        // before any frame is sent (and before a binary template is decoded).
        for (message_kind, id, stored) in [
            ("text", "vocabulary", vocabulary),
            ("binary", "language_hints", language_hints),
        ] {
            let template = match message_kind {
                "text" => format!("event={{{{var:{id}}}}}"),
                "binary" => format!("{{{{var:{id}}}}}"),
                _ => unreachable!(),
            };
            let error = render_text(
                &template,
                &values,
                &secrets,
                &runtime,
                &parameters,
                WorkflowSchemaVersion(CURRENT_SCHEMA_VERSION),
                None,
            )
            .unwrap_err();

            match error {
                RealtimeSessionError::TypedTemplate(message) => {
                    assert!(message.contains(id));
                    assert!(!message.contains(stored));
                }
                error => panic!("expected {message_kind} template error, got {error:?}"),
            }
        }
    }

    #[tokio::test]
    async fn live_chunks_do_not_add_recorded_replay_pacing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut first_audio_at = None;
            let mut audio_chunks = 0;
            loop {
                let message = socket.next().await.unwrap().unwrap();
                match message {
                    Message::Binary(_) => {
                        first_audio_at.get_or_insert_with(Instant::now);
                        audio_chunks += 1;
                    }
                    Message::Text(text) if text == "finish" => {
                        let elapsed = first_audio_at.unwrap().elapsed();
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
                        return (elapsed, audio_chunks);
                    }
                    _ => panic!("unexpected realtime frame"),
                }
            }
        });
        let mut source = FixedChunkSource {
            chunks: VecDeque::from([
                AudioChunk {
                    bytes: vec![1, 2],
                    duration_ms: 500,
                },
                AudioChunk {
                    bytes: vec![3, 4],
                    duration_ms: 500,
                },
            ]),
            requires_pacing: false,
            ready: None,
        };
        let realtime = workflow(format!("ws://{address}"));

        let transcription = tokio::time::timeout(
            Duration::from_secs(2),
            run_realtime_session(
                &realtime,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &RuntimeTemplateValues::default(),
                &mut source,
                &CancellationToken::new(),
            ),
        )
        .await
        .expect("live session must not wait for two replay-paced chunks")
        .unwrap();
        let (elapsed, audio_chunks) = server.await.unwrap();

        assert_eq!(transcription.text, "complete");
        assert_eq!(audio_chunks, 2);
        assert!(
            elapsed < Duration::from_millis(400),
            "live source waited {elapsed:?} instead of following capture timing"
        );
    }

    #[tokio::test]
    async fn recorded_replay_chunks_follow_media_timeline_pacing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let mut first_audio_at = None;
            let mut audio_chunks = 0;
            loop {
                let message = socket.next().await.unwrap().unwrap();
                match message {
                    Message::Binary(_) => {
                        first_audio_at.get_or_insert_with(Instant::now);
                        audio_chunks += 1;
                    }
                    Message::Text(text) if text == "finish" => {
                        let elapsed = first_audio_at.unwrap().elapsed();
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
                        return (elapsed, audio_chunks);
                    }
                    _ => panic!("unexpected realtime frame"),
                }
            }
        });
        let mut source = FixedChunkSource {
            chunks: VecDeque::from([
                AudioChunk {
                    bytes: vec![1, 2],
                    duration_ms: 100,
                },
                AudioChunk {
                    bytes: vec![3, 4],
                    duration_ms: 100,
                },
            ]),
            requires_pacing: true,
            ready: None,
        };
        let mut realtime = workflow(format!("ws://{address}"));
        realtime.audio_stream.chunk_duration_ms = 100;

        let transcription = run_realtime_session(
            &realtime,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &RuntimeTemplateValues::default(),
            &mut source,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let (elapsed, audio_chunks) = server.await.unwrap();

        assert_eq!(transcription.text, "complete");
        assert_eq!(audio_chunks, 2);
        assert!(
            elapsed >= Duration::from_millis(150),
            "recorded replay skipped its media-timeline pacing: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn live_source_drains_completion_already_received_before_the_next_chunk() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"event":"final","text":"early"}"#.into()))
                .await
                .unwrap();
            socket
                .send(Message::Text(r#"{"event":"done"}"#.into()))
                .await
                .unwrap();
            let _ = ready_tx.send(());
            let mut audio_chunks = 0;
            while let Some(message) = socket.next().await {
                match message.unwrap() {
                    Message::Binary(_) => audio_chunks += 1,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            audio_chunks
        });
        let mut source = FixedChunkSource {
            chunks: VecDeque::from([
                AudioChunk {
                    bytes: vec![1, 2],
                    duration_ms: 500,
                },
                AudioChunk {
                    bytes: vec![3, 4],
                    duration_ms: 500,
                },
            ]),
            requires_pacing: false,
            ready: Some(ready_rx),
        };
        let realtime = workflow(format!("ws://{address}"));

        let transcription = run_realtime_session(
            &realtime,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &RuntimeTemplateValues::default(),
            &mut source,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let audio_chunks = tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("client must close after an already-received completion")
            .unwrap();

        assert_eq!(transcription.text, "early");
        assert_eq!(audio_chunks, 1);
    }
}
