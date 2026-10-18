use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use thiserror::Error;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::jsonpath;

const USER_AGENT: &str = "dictate-client/1.0";
const ASR_RESPONSE_HAVE_NO_WORDS: &str = "ASR_RESPONSE_HAVE_NO_WORDS";

/// `Content-Type` for the Legacy Audio API upload part.
///
/// The part must carry one. Multipart parsers classify a part as text when it has no
/// `Content-Type` and a charset-less `Content-Disposition` (n8n's parser does exactly
/// that), decode the audio as UTF-8 and corrupt it irreversibly before the ASR engine
/// ever sees it -- the caller gets a 500/502 "failed to decode audio" that looks like a
/// server fault. The prepared file's extension is the container the user configured, so
/// it is also the honest media type to advertise.
fn audio_content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    match extension.as_deref() {
        Some("opus" | "ogg" | "oga" | "spx" | "speex") => "audio/ogg",
        Some("mp3" | "mp2" | "mpeg" | "mpg") => "audio/mpeg",
        Some("wav" | "wave") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("m4a" | "mp4" | "aac" | "mov") => "audio/mp4",
        Some("amr") => "audio/amr",
        Some("wma" | "asf") => "audio/x-ms-wma",
        Some("aiff" | "aif") => "audio/aiff",
        Some("wv") => "audio/x-wavpack",
        Some("mka") => "audio/x-matroska",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        Some("flv") => "video/x-flv",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    pub raw_response: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrRequestFailureKind {
    Http,
    Timeout,
    Connection,
    Network,
    ResponseRead,
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("{0}")]
    Preparation(#[from] crate::converter::ConvertError),
    #[error("API endpoint is empty")]
    EmptyEndpoint,
    #[error("{0}")]
    InvalidTextPath(#[from] jsonpath::TextPathError),
    #[error("{source}")]
    TextExtraction {
        #[source]
        source: jsonpath::TextExtractionError,
        last_response: Vec<u8>,
    },
    #[error("invalid extra-config JSON: {0}")]
    InvalidExtraConfig(#[from] serde_json::Error),
    #[error("{0}")]
    Client(#[source] reqwest::Error),
    #[error("could not open prepared audio: {0}")]
    PreparedAudio(#[source] std::io::Error),
    #[error("{0}")]
    Attempt(String),
    #[error("request canceled")]
    Canceled,
    #[error("exceeded max retries ({max_retry}), attempts: {attempts}")]
    RetryExhausted {
        max_retry: i32,
        attempts: i32,
        last_status: Option<u16>,
        last_failure: AsrRequestFailureKind,
        last_response: Vec<u8>,
    },
}

impl AsrError {
    pub fn is_retry_exhausted(&self) -> bool {
        matches!(self, Self::RetryExhausted { .. })
    }

    pub fn last_response(&self) -> &[u8] {
        match self {
            Self::RetryExhausted { last_response, .. }
            | Self::TextExtraction { last_response, .. } => last_response,
            _ => &[],
        }
    }

    /// Long segmented batches retain their individual failures until every
    /// started request has drained. The caller needs the error category, not
    /// an arbitrary-size response body, once a safe placeholder has been
    /// derived or the error is being propagated.
    pub(crate) fn discard_response_body(mut self) -> Self {
        match &mut self {
            Self::RetryExhausted { last_response, .. }
            | Self::TextExtraction { last_response, .. } => last_response.clear(),
            _ => {}
        }
        self
    }
}

enum UploadAttempt {
    Accepted(Vec<u8>),
    /// Some ASR services use HTTP 400 for a structurally valid response that
    /// means the recording contained no recognizable words. Treat that as an
    /// empty transcription rather than retrying or surfacing an HTTP failure.
    NoSpeech(Vec<u8>),
    Rejected {
        status: Option<u16>,
        failure: AsrRequestFailureKind,
        response: Vec<u8>,
    },
}

#[derive(Clone)]
pub struct AsrClient {
    config: Config,
    client: Client,
    text_path: serde_json_path::JsonPath,
    extra_config: Option<BTreeMap<String, Value>>,
}

impl AsrClient {
    pub fn new(config: Config) -> Result<Self, AsrError> {
        let text_path = jsonpath::parse_text_path(&config.text_path)?;
        let extra_config = if config.extra_config.trim().is_empty() {
            None
        } else {
            Some(serde_json::from_str::<BTreeMap<String, Value>>(
                &config.extra_config,
            )?)
        };
        if extra_config
            .as_ref()
            .is_some_and(|fields| fields.contains_key("file"))
        {
            return Err(AsrError::Attempt(
                "Extra config file conflicts with the audio upload".into(),
            ));
        }
        let client = crate::network::client(&config).map_err(AsrError::Client)?;
        Ok(Self {
            config,
            client,
            extra_config,
            text_path,
        })
    }

    pub async fn transcribe(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<Transcription, AsrError> {
        self.transcribe_with_retry_prepare(cancellation, file_path, || async { Ok(()) })
            .await
    }

    /// Rebuild audio from its original source before each automatic retry.
    pub async fn transcribe_with_retry_prepare<F, Fut>(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
        mut prepare: F,
    ) -> Result<Transcription, AsrError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<(), AsrError>>,
    {
        if cancellation.is_cancelled() {
            self.debug(format_args!(
                "[upload] audio event=canceled phase=before-request"
            ));
            return Err(AsrError::Canceled);
        }
        if self.config.api_endpoint.is_empty() {
            return Err(AsrError::EmptyEndpoint);
        }

        let mut attempt = 0;
        let mut delay = self.config.retry_base_delay;
        loop {
            attempt += 1;
            self.debug(format_args!(
                "[upload] audio event=attempt-start attempt={attempt} total={}",
                self.config.max_retry
            ));
            if attempt > 1 {
                prepare().await?;
            }
            let (status, failure, response) =
                match self.upload_once(cancellation, file_path).await? {
                    UploadAttempt::Accepted(response) => {
                        let text = match jsonpath::extract_transcript_from_response(
                            &response,
                            &self.text_path,
                        ) {
                            Ok(text) => {
                                self.debug(format_args!(
                                    "[upload] audio event=parse-complete characters={}",
                                    text.chars().count()
                                ));
                                if text.trim().is_empty() {
                                    String::new()
                                } else {
                                    text
                                }
                            }
                            Err(source) => {
                                self.debug(format_args!(
                                    "[upload] audio event=parse-failed kind=text-path"
                                ));
                                return Err(AsrError::TextExtraction {
                                    source,
                                    last_response: response.clone(),
                                });
                            }
                        };
                        self.debug(format_args!("[upload] audio event=completed"));
                        return Ok(Transcription {
                            text,
                            raw_response: response,
                        });
                    }
                    UploadAttempt::NoSpeech(response) => {
                        self.debug(format_args!("[upload] audio event=no-speech"));
                        return Ok(Transcription {
                            text: String::new(),
                            raw_response: response,
                        });
                    }
                    UploadAttempt::Rejected {
                        status,
                        failure,
                        response,
                    } => (status, failure, response),
                };
            self.debug(format_args!(
                "[upload] audio event=attempt-failed attempt={attempt} response_bytes={}",
                response.len()
            ));
            if attempt >= self.config.max_retry {
                self.debug(format_args!("[upload] audio event=failed kind=request"));
                return Err(AsrError::RetryExhausted {
                    max_retry: self.config.max_retry,
                    attempts: attempt,
                    last_status: status,
                    last_failure: failure,
                    last_response: response,
                });
            }
            let duration = Duration::from_secs_f64(delay.max(0.0));
            self.debug(format_args!(
                "[upload] audio event=retry delay_ms={}",
                duration.as_millis()
            ));
            tokio::select! {
                _ = cancellation.cancelled() => {
                    self.debug(format_args!("[upload] audio event=canceled phase=retry-wait"));
                    return Err(AsrError::Canceled);
                },
                _ = tokio::time::sleep(duration) => {}
            }
            delay *= 2.0;
        }
    }

    /// Sends one transcription request without retrying. This is used by the
    /// settings dialog to verify the endpoint and credentials while preserving
    /// the exact transport or HTTP error for the user.
    pub async fn test_connection(&self, file_path: &Path) -> Result<(), AsrError> {
        if self.config.api_endpoint.is_empty() {
            return Err(AsrError::EmptyEndpoint);
        }
        self.test_connection_cancellable(&CancellationToken::new(), file_path)
            .await
    }

    pub async fn test_connection_cancellable(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<(), AsrError> {
        self.debug(format_args!("[upload] audio event=connectivity-start"));
        let started = Instant::now();
        let result = self.test_connection_once(cancellation, file_path).await;
        match &result {
            Ok(()) => self.debug(format_args!(
                "[upload] audio event=connectivity-complete elapsed_ms={}",
                started.elapsed().as_millis()
            )),
            Err(error) => self.debug(format_args!(
                "[upload] audio event=connectivity-failed kind={} elapsed_ms={}",
                asr_error_kind(error),
                started.elapsed().as_millis()
            )),
        }
        result
    }

    async fn test_connection_once(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<(), AsrError> {
        let response = self.send_request(cancellation, file_path).await?;
        let status = response.status();
        let response = tokio::select! {
            _ = cancellation.cancelled() => {
                self.debug(format_args!("[upload] audio event=canceled phase=response-read"));
                return Err(AsrError::Canceled)
            },
            result = response.bytes() => result.map_err(AsrError::Client)?,
        };
        self.debug(format_args!(
            "[upload] audio event=response-complete status={} bytes={}",
            status.as_u16(),
            response.len()
        ));
        if status == StatusCode::OK || is_no_words_response(status, &response) {
            self.debug(format_args!(
                "[upload] audio event=connectivity-response-accepted"
            ));
            return Ok(());
        }
        Err(AsrError::Attempt(format!(
            "HTTP {status}: {}",
            format_response(
                crate::debug_log::request_message(
                    &String::from_utf8_lossy(&response),
                    &self.config
                )
                .as_bytes()
            )
        )))
    }

    pub(crate) fn terminal_failure_placeholder(&self, error: &AsrError) -> Option<String> {
        match error {
            AsrError::RetryExhausted {
                last_status: Some(status),
                last_failure: AsrRequestFailureKind::Http,
                last_response,
                ..
            } => Some(crate::audio_failure::http_placeholder(
                *status,
                last_response,
                &self.config,
            )),
            AsrError::RetryExhausted {
                last_failure: AsrRequestFailureKind::Http,
                ..
            } => Some(crate::audio_failure::placeholder("ASR request failed")),
            AsrError::RetryExhausted {
                last_failure: AsrRequestFailureKind::Timeout,
                ..
            } => Some(crate::audio_failure::placeholder("Network timeout")),
            AsrError::RetryExhausted {
                last_failure: AsrRequestFailureKind::Connection,
                ..
            } => Some(crate::audio_failure::placeholder(
                "Network connection failed",
            )),
            AsrError::RetryExhausted {
                last_failure: AsrRequestFailureKind::Network,
                ..
            } => Some(crate::audio_failure::placeholder("Network request failed")),
            AsrError::RetryExhausted {
                last_failure: AsrRequestFailureKind::ResponseRead,
                ..
            } => Some(crate::audio_failure::placeholder(
                "ASR response could not be read",
            )),
            AsrError::TextExtraction { .. } => {
                Some(crate::audio_failure::placeholder("Invalid ASR response"))
            }
            AsrError::Preparation(_)
            | AsrError::EmptyEndpoint
            | AsrError::InvalidTextPath(_)
            | AsrError::InvalidExtraConfig(_)
            | AsrError::Client(_)
            | AsrError::PreparedAudio(_)
            | AsrError::Attempt(_)
            | AsrError::Canceled => None,
        }
    }

    async fn upload_once(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<UploadAttempt, AsrError> {
        let started = Instant::now();
        let response = match self.send_request(cancellation, file_path).await {
            Ok(response) => response,
            Err(AsrError::Canceled) => return Err(AsrError::Canceled),
            // A malformed endpoint or authorization value fails while
            // building the request, before anything can reach the service.
            // Do not turn that local configuration error into a retried
            // network failure or an inserted placeholder.
            Err(AsrError::Client(error)) if error.is_builder() => {
                return Err(AsrError::Client(error));
            }
            Err(AsrError::Client(error)) => {
                let failure = if error.is_timeout() {
                    AsrRequestFailureKind::Timeout
                } else if error.is_connect() {
                    AsrRequestFailureKind::Connection
                } else {
                    AsrRequestFailureKind::Network
                };
                self.debug(format_args!(
                    "[upload] audio event=request-failed kind={}",
                    match failure {
                        AsrRequestFailureKind::Timeout => "timeout",
                        AsrRequestFailureKind::Connection => "connection",
                        AsrRequestFailureKind::Network => "network",
                        AsrRequestFailureKind::Http | AsrRequestFailureKind::ResponseRead =>
                            "request",
                    }
                ));
                return Ok(UploadAttempt::Rejected {
                    status: None,
                    failure,
                    response: format!("request error: {error}").into_bytes(),
                });
            }
            Err(error) => return Err(error),
        };
        let status = response.status();
        let bytes = tokio::select! {
            _ = cancellation.cancelled() => {
                self.debug(format_args!("[upload] audio event=canceled phase=response-read"));
                return Err(AsrError::Canceled);
            },
            result = response.bytes() => match result {
                Ok(bytes) => bytes.to_vec(),
                Err(error) => {
                    self.debug(format_args!("[upload] audio event=response-read-failed kind=network"));
                    return Ok(UploadAttempt::Rejected {
                        status: Some(status.as_u16()),
                        failure: AsrRequestFailureKind::ResponseRead,
                        response: format!("read response error: {error}").into_bytes(),
                    });
                },
            }
        };
        self.debug(format_args!(
            "[upload] audio event=response-complete status={} bytes={} elapsed_ms={}",
            status.as_u16(),
            bytes.len(),
            started.elapsed().as_millis()
        ));
        if status == StatusCode::OK {
            Ok(UploadAttempt::Accepted(bytes))
        } else if is_no_words_response(status, &bytes) {
            Ok(UploadAttempt::NoSpeech(bytes))
        } else {
            Ok(UploadAttempt::Rejected {
                status: Some(status.as_u16()),
                failure: AsrRequestFailureKind::Http,
                response: bytes,
            })
        }
    }

    async fn send_request(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<reqwest::Response, AsrError> {
        self.debug(format_args!(
            "[upload] audio event=request-start method=POST endpoint={}",
            crate::debug_log::safe_url(&self.config.api_endpoint)
        ));
        let started = Instant::now();
        // The file and multipart body are reconstructed for every retry.
        let file = File::open(file_path)
            .await
            .map_err(AsrError::PreparedAudio)?;
        let file_name = file_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio".into());
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        let part = Part::stream(body)
            .file_name(file_name)
            .mime_str(audio_content_type(file_path))
            .map_err(AsrError::Client)?;
        let mut form = Form::new().part("file", part);
        for (key, value) in self.request_fields() {
            form = form.text(key, value);
        }

        let mut request = self
            .client
            .post(&self.config.api_endpoint)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .multipart(form);
        if !self.config.token.is_empty() {
            request = request.bearer_auth(&self.config.token);
        }
        let result = tokio::select! {
            _ = cancellation.cancelled() => Err(AsrError::Canceled),
            result = request.send() => result.map_err(AsrError::Client),
        };
        match &result {
            Ok(response) => self.debug(format_args!(
                "[upload] audio event=response-headers status={} elapsed_ms={}",
                response.status().as_u16(),
                started.elapsed().as_millis()
            )),
            Err(error) => self.debug(format_args!(
                "[upload] audio event=request-failed kind={} elapsed_ms={}",
                asr_error_kind(error),
                started.elapsed().as_millis()
            )),
        }
        result
    }

    fn debug(&self, message: std::fmt::Arguments<'_>) {
        if self.config.upload_debug {
            let message = crate::debug_log::request_message(&message.to_string(), &self.config);
            crate::debug_log::write(
                crate::debug_log::Category::Upload,
                format_args!("{message}"),
            );
        }
    }

    pub fn request_fields(&self) -> BTreeMap<String, String> {
        let mut fields = BTreeMap::new();
        if !self.config.model.is_empty() {
            fields.insert("model".into(), self.config.model.clone());
        }
        if !self.config.language.is_empty() {
            fields.insert("language".into(), self.config.language.clone());
        }
        if !self.config.prompt.is_empty() {
            fields.insert("prompt".into(), self.config.prompt.clone());
        }
        if let Some(extra) = &self.extra_config {
            let base = fields
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect();
            let extra = extra
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            fields = crate::additional_parameters::merge(&base, &extra)
                .iter()
                .map(|(key, value)| (key.clone(), value_to_form_field(value)))
                .collect();
        }
        fields
    }
}

/// Some ASR endpoints use this exact error code with HTTP 400 to say that
/// transcription completed but did not contain a word. Restrict the exception
/// to the documented code and response shape so ordinary 400s remain visible
/// configuration or service failures.
pub(crate) fn is_no_words_response(status: StatusCode, response: &[u8]) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    serde_json::from_slice::<Value>(response).is_ok_and(|value| is_no_words_error(&value))
}

/// Shared only by explicit ASR failure responses/events, never by arbitrary
/// transcript strings or remote-resource upload errors.
pub(crate) fn is_no_words_error(response: &Value) -> bool {
    let Some(object) = response.as_object() else {
        return false;
    };
    let has_no_words_code = |object: &serde_json::Map<String, Value>| {
        ["code", "message", "info", "detail"]
            .into_iter()
            .any(|field| {
                object.get(field).and_then(Value::as_str) == Some(ASR_RESPONSE_HAVE_NO_WORDS)
            })
    };
    has_no_words_code(object)
        || match object.get("error") {
            Some(Value::String(value)) => value == ASR_RESPONSE_HAVE_NO_WORDS,
            Some(Value::Object(error)) => has_no_words_code(error),
            _ => false,
        }
}

fn asr_error_kind(error: &AsrError) -> &'static str {
    match error {
        AsrError::Preparation(_) => "preparation",
        AsrError::EmptyEndpoint => "configuration",
        AsrError::InvalidTextPath(_) => "configuration",
        AsrError::TextExtraction { .. } => "text-path",
        AsrError::InvalidExtraConfig(_) => "configuration",
        AsrError::Client(_) => "network",
        AsrError::PreparedAudio(_) => "preparation",
        AsrError::Attempt(_) => "http",
        AsrError::Canceled => "canceled",
        AsrError::RetryExhausted { .. } => "http",
    }
}

fn value_to_form_field(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

pub fn format_response(response: &[u8]) -> String {
    if response.is_empty() {
        return "<empty>".into();
    }
    if let Ok(text) = std::str::from_utf8(response) {
        if text.len() > 1_000 {
            let end = text
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index <= 1_000)
                .last()
                .unwrap_or(0);
            return format!(
                "{}... (truncated, total {} bytes)",
                &text[..end],
                response.len()
            );
        }
        return text.into();
    }
    let prefix = &response[..response.len().min(256)];
    let hex: String = prefix.iter().map(|byte| format!("{byte:02x}")).collect();
    if response.len() > 256 {
        format!("<binary {} bytes, prefix hex: {hex}...>", response.len())
    } else {
        format!("<binary {} bytes, hex: {hex}>", response.len())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_invalid_text_paths_before_creating_client() {
        for path in ["", "text", "$.segments["] {
            let config = crate::Config {
                text_path: path.into(),
                ..Default::default()
            };
            assert!(
                config
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("TEXT_PATH")
            );
            assert!(matches!(
                super::AsrClient::new(config),
                Err(super::AsrError::InvalidTextPath(_))
            ));
        }
    }

    #[tokio::test]
    async fn extraction_errors_preserve_response_and_do_not_retry() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (body, path) in [
            ("not JSON", "$.text"),
            (r#"{"text":"fallback","other":"fallback"}"#, "$.missing"),
            (r#"{"text":[]}"#, "$.text"),
            (
                r#"{"segments":[{"text":"a"},{"text":"b"}]}"#,
                "$.segments[*].text",
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|s| s.strip_prefix("content-length: "))
                            .and_then(|s| s.trim().parse::<usize>().ok());
                        if length.is_some_and(|n| request.len() >= end + 4 + n)
                            || request.ends_with(b"0\r\n\r\n")
                        {
                            break;
                        }
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let dir = tempfile::tempdir().unwrap();
            let audio = dir.path().join("audio.wav");
            std::fs::write(&audio, b"audio").unwrap();
            let client = super::AsrClient::new(crate::Config {
                api_endpoint: format!("http://{address}"),
                text_path: path.into(),
                max_retry: 3,
                retry_base_delay: 0.0,
                ..Default::default()
            })
            .unwrap();
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                client.transcribe_with_retry_prepare(
                    &tokio_util::sync::CancellationToken::new(),
                    &audio,
                    || async { panic!("extraction errors must not retry the upload") },
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(matches!(error, super::AsrError::TextExtraction { .. }));
            assert!(!error.is_retry_exhausted());
            assert_eq!(error.last_response(), body.as_bytes());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn retry_rebuilds_audio_before_uploading_again() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for status in ["503 Service Unavailable", "200 OK"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|s| s.strip_prefix("content-length: "))
                            .and_then(|s| s.trim().parse::<usize>().ok());
                        if length.is_some_and(|n| request.len() >= end + 4 + n)
                            || request.ends_with(b"0\r\n\r\n")
                        {
                            break;
                        }
                    }
                }
                let body = r#"{"text":"done"}"#;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("audio.wav");
        std::fs::write(&audio, b"audio").unwrap();
        let client = super::AsrClient::new(crate::Config {
            api_endpoint: format!("http://{address}"),
            max_retry: 2,
            retry_base_delay: 0.0,
            ..Default::default()
        })
        .unwrap();
        let prepared = Arc::new(AtomicUsize::new(0));
        let count = prepared.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.transcribe_with_retry_prepare(
                &tokio_util::sync::CancellationToken::new(),
                &audio,
                || async {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.text, "done");
        assert_eq!(prepared.load(Ordering::SeqCst), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn empty_transcriptions_succeed_without_retry() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for (status, body) in [
            (200, r#"{"text":""}"#),
            (200, r#"{"text":" \t\n　"}"#),
            (200, r#"{"text":null}"#),
            (400, r#"{"error":"ASR_RESPONSE_HAVE_NO_WORDS"}"#),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|value| value.strip_prefix("content-length: "))
                            .and_then(|value| value.trim().parse::<usize>().ok());
                        if length.is_some_and(|value| request.len() >= end + 4 + value)
                            || request.ends_with(b"0\r\n\r\n")
                        {
                            break;
                        }
                    }
                }
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let directory = tempfile::tempdir().unwrap();
            let audio = directory.path().join("audio.wav");
            std::fs::write(&audio, b"audio").unwrap();
            let client = super::AsrClient::new(crate::Config {
                api_endpoint: format!("http://{address}"),
                max_retry: 3,
                retry_base_delay: 0.0,
                ..Default::default()
            })
            .unwrap();
            let prepared = Arc::new(AtomicUsize::new(0));
            let count = prepared.clone();
            let transcription = client
                .transcribe_with_retry_prepare(
                    &tokio_util::sync::CancellationToken::new(),
                    &audio,
                    || async {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                )
                .await
                .unwrap();
            assert!(transcription.text.is_empty());
            assert_eq!(transcription.raw_response, body.as_bytes());
            assert_eq!(prepared.load(Ordering::SeqCst), 0);
            server.await.unwrap();
        }
    }

    #[test]
    fn no_words_response_requires_the_exact_code_and_http_status() {
        assert!(super::is_no_words_response(
            StatusCode::BAD_REQUEST,
            br#"{"code":"ASR_RESPONSE_HAVE_NO_WORDS"}"#,
        ));
        assert!(super::is_no_words_response(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"ASR_RESPONSE_HAVE_NO_WORDS"}}"#,
        ));
        assert!(super::is_no_words_response(
            StatusCode::BAD_REQUEST,
            br#"{"message":"ASR_RESPONSE_HAVE_NO_WORDS"}"#,
        ));
        assert!(!super::is_no_words_response(
            StatusCode::BAD_REQUEST,
            br#"{"error":"other"}"#,
        ));
        assert!(!super::is_no_words_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            br#"{"error":"ASR_RESPONSE_HAVE_NO_WORDS"}"#,
        ));
    }

    #[tokio::test]
    async fn request_builder_errors_are_not_retried_or_reclassified_as_network_failures() {
        let directory = tempfile::tempdir().unwrap();
        let audio = directory.path().join("audio.wav");
        std::fs::write(&audio, b"audio").unwrap();
        let client = super::AsrClient::new(crate::Config {
            // The malformed URL is rejected by reqwest before it can send a
            // request, so this must remain a local client error.
            api_endpoint: "not a URL".into(),
            max_retry: 3,
            retry_base_delay: 0.0,
            ..Default::default()
        })
        .unwrap();

        let error = client
            .transcribe(&tokio_util::sync::CancellationToken::new(), &audio)
            .await
            .unwrap_err();
        assert!(matches!(error, super::AsrError::Client(ref error) if error.is_builder()));
    }
    use super::*;

    #[test]
    fn extra_config_recursively_normalizes_before_multipart_serialization() {
        let cfg = Config {
            model: "base".into(),
            language: "zh".into(),
            prompt: "hello".into(),
            extra_config:
                r#"{"language":null,"temperature":0.25,"stream":false,"metadata":{"tier":"test","drop":null,"nested":{"drop":null,"keep":true}},"items":[null,{"drop":null,"keep":2}]}"#
                    .into(),
            ..Config::default()
        };
        let fields = AsrClient::new(cfg).unwrap().request_fields();
        assert!(!fields.contains_key("language"));
        assert_eq!(fields["model"], "base");
        assert_eq!(fields["temperature"], "0.25");
        assert_eq!(fields["stream"], "false");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fields["metadata"]).unwrap(),
            serde_json::json!({"tier":"test","nested":{"keep":true}})
        );
        assert_eq!(fields["items"], r#"[null,{"keep":2}]"#);
    }

    #[test]
    fn rejects_non_object_extra_config() {
        let config = Config {
            extra_config: "[]".into(),
            ..Config::default()
        };
        assert!(AsrClient::new(config).is_err());
    }

    #[test]
    fn formats_text_and_binary_responses() {
        assert_eq!(format_response(&[]), "<empty>");
        assert_eq!(format_response(b"hello"), "hello");
        assert_eq!(format_response(&[0xff, 0]), "<binary 2 bytes, hex: ff00>");
        let unicode = format!("{}é", "a".repeat(999));
        assert!(format_response(unicode.as_bytes()).starts_with(&"a".repeat(999)));
    }

    #[test]
    fn connection_failures_keep_the_http_response() {
        let error = AsrError::Attempt(format!(
            "HTTP {}: {}",
            StatusCode::UNAUTHORIZED,
            format_response(br#"{"error":"invalid API key"}"#)
        ));
        assert_eq!(
            error.to_string(),
            r#"HTTP 401 Unauthorized: {"error":"invalid API key"}"#
        );
    }

    #[test]
    fn terminal_http_failures_use_a_safe_placeholder() {
        let client = AsrClient::new(Config {
            token: "private-token".into(),
            ..Config::default()
        })
        .unwrap();
        let error = AsrError::RetryExhausted {
            max_retry: 1,
            attempts: 1,
            last_status: Some(503),
            last_failure: AsrRequestFailureKind::Http,
            last_response: br#"{"error":{"info":"private-token retry later"}}"#.to_vec(),
        };

        assert_eq!(
            client.terminal_failure_placeholder(&error),
            Some("[503 Service Unavailable: redacted retry later]".into())
        );
    }

    #[test]
    fn discarding_response_bodies_keeps_the_failure_classification() {
        let error = AsrError::RetryExhausted {
            max_retry: 1,
            attempts: 1,
            last_status: Some(503),
            last_failure: AsrRequestFailureKind::Http,
            last_response: b"large service response".to_vec(),
        }
        .discard_response_body();

        assert!(error.is_retry_exhausted());
        assert!(error.last_response().is_empty());
    }

    #[test]
    fn upload_content_type_follows_the_prepared_container() {
        use std::path::Path;
        for (name, expected) in [
            ("RecordTemp_1.opus", "audio/ogg"),
            ("a.ogg", "audio/ogg"),
            ("a.mp3", "audio/mpeg"),
            ("a.MP3", "audio/mpeg"),
            ("a.wav", "audio/wav"),
            ("a.flac", "audio/flac"),
            ("a.m4a", "audio/mp4"),
            ("a.amr", "audio/amr"),
            ("a.wma", "audio/x-ms-wma"),
            ("a.mkv", "video/x-matroska"),
            ("audio", "application/octet-stream"),
            ("a.unknown", "application/octet-stream"),
        ] {
            assert_eq!(
                super::audio_content_type(Path::new(name)),
                expected,
                "wrong content type for {name}"
            );
        }
    }

    /// A part without `Content-Type` is classified as text by servers such as n8n, which
    /// then decode the audio as UTF-8 and fail with "could not decode audio". Assert the
    /// header is really on the wire, not just in the helper above.
    #[tokio::test]
    async fn upload_part_carries_a_content_type_on_the_wire() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (file_name, expected) in [
            ("audio.opus", "content-type: audio/ogg"),
            ("audio.mp3", "content-type: audio/mpeg"),
            ("audio", "content-type: application/octet-stream"),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|s| s.strip_prefix("content-length: "))
                            .and_then(|s| s.trim().parse::<usize>().ok());
                        if length.is_some_and(|n| request.len() >= end + 4 + n)
                            || request.ends_with(b"0\r\n\r\n")
                        {
                            break;
                        }
                    }
                }
                let body = r#"{"text":"done"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                request
            });
            let dir = tempfile::tempdir().unwrap();
            let audio = dir.path().join(file_name);
            std::fs::write(&audio, b"audio").unwrap();
            let client = super::AsrClient::new(crate::Config {
                api_endpoint: format!("http://{address}"),
                max_retry: 1,
                retry_base_delay: 0.0,
                ..Default::default()
            })
            .unwrap();
            client
                .transcribe_with_retry_prepare(
                    &tokio_util::sync::CancellationToken::new(),
                    &audio,
                    || async { Ok(()) },
                )
                .await
                .unwrap();
            let request = String::from_utf8_lossy(&server.await.unwrap()).to_lowercase();
            assert!(
                request.contains(expected),
                "upload of {file_name} must carry '{expected}', got: {request}"
            );
        }
    }
}
