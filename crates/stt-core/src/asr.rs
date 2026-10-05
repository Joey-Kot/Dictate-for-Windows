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

const USER_AGENT: &str = "stt-go-client/1.0";

#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    pub raw_response: Vec<u8>,
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
    #[error("{0}")]
    Attempt(String),
    #[error("request canceled")]
    Canceled,
    #[error("exceeded max retries ({max_retry}), attempts: {attempts}")]
    RetryExhausted {
        max_retry: i32,
        attempts: i32,
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
                "[upload] Audio attempt {attempt}/{}",
                self.config.max_retry
            ));
            if attempt > 1 {
                prepare().await?;
            }
            let (succeeded, response) = self.upload_once(cancellation, file_path).await?;
            if succeeded {
                let text = jsonpath::extract_text_from_response(&response, &self.text_path)
                    .map_err(|source| AsrError::TextExtraction {
                        source,
                        last_response: response.clone(),
                    })?;
                return Ok(Transcription {
                    text,
                    raw_response: response,
                });
            }
            if self.config.upload_debug {
                let clean = crate::debug_log::request_message(
                    &String::from_utf8_lossy(&response),
                    &self.config,
                );
                self.debug(format_args!(
                    "[upload] attempt {attempt} failed: {}",
                    format_response(clean.as_bytes())
                ));
            }
            if attempt >= self.config.max_retry {
                return Err(AsrError::RetryExhausted {
                    max_retry: self.config.max_retry,
                    attempts: attempt,
                    last_response: response,
                });
            }
            let duration = Duration::from_secs_f64(delay.max(0.0));
            self.debug(format_args!(
                "[upload] Audio retry in {:.3}s",
                duration.as_secs_f64()
            ));
            tokio::select! {
                _ = cancellation.cancelled() => {
                    self.debug(format_args!("[upload] Audio canceled during retry wait"));
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
        self.debug(format_args!("[upload] Audio connectivity test"));
        let started = Instant::now();
        let result = self.test_connection_once(cancellation, file_path).await;
        match &result {
            Ok(()) => self.debug(format_args!(
                "[upload] Audio connectivity succeeded, elapsed={}ms",
                started.elapsed().as_millis()
            )),
            Err(error) => self.debug(format_args!(
                "[upload] Audio connectivity failed, elapsed={}ms: {error}",
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
        if status == StatusCode::OK {
            return Ok(());
        }
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(AsrError::Canceled),
            result = response.bytes() => result.map_err(AsrError::Client)?,
        };
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

    async fn upload_once(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<(bool, Vec<u8>), AsrError> {
        let started = Instant::now();
        let response = match self.send_request(cancellation, file_path).await {
            Ok(response) => response,
            Err(AsrError::Canceled) => return Err(AsrError::Canceled),
            Err(AsrError::Attempt(message)) => return Ok((false, message.into_bytes())),
            Err(error) => return Ok((false, format!("request error: {error}").into_bytes())),
        };
        let status = response.status();
        let bytes = tokio::select! {
            _ = cancellation.cancelled() => {
                self.debug(format_args!("[upload] Audio canceled while reading response"));
                return Err(AsrError::Canceled);
            },
            result = response.bytes() => match result {
                Ok(bytes) => bytes.to_vec(),
                Err(error) => format!("read response error: {error}").into_bytes(),
            }
        };
        self.debug(format_args!(
            "[upload] Audio response complete: HTTP {status}, bytes={}, elapsed={}ms",
            bytes.len(),
            started.elapsed().as_millis()
        ));
        Ok((status == StatusCode::OK, bytes))
    }

    async fn send_request(
        &self,
        cancellation: &CancellationToken,
        file_path: &Path,
    ) -> Result<reqwest::Response, AsrError> {
        self.debug(format_args!(
            "[upload] Audio POST {}",
            crate::debug_log::safe_url(&self.config.api_endpoint)
        ));
        let started = Instant::now();
        // The file and multipart body are reconstructed for every retry.
        let file = File::open(file_path)
            .await
            .map_err(|error| AsrError::Attempt(format!("open file error: {error}")))?;
        let file_name = file_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio".into());
        let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
        let part = Part::stream(body).file_name(file_name);
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
                "[upload] Audio HTTP {}, headers after {}ms",
                response.status(),
                started.elapsed().as_millis()
            )),
            Err(error) => self.debug(format_args!(
                "[upload] Audio request failed after {}ms: {error}",
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
            (r#"{"text":null}"#, "$.text"),
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
}
