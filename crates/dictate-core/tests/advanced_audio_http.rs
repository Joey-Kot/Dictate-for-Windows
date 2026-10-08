//! Local HTTP integration coverage for declarative Advanced Audio workflows.
//!
//! Every endpoint in this file is an in-process TCP fake. The tests exercise
//! `AdvancedAudioClient` rather than constructing requests directly, so they
//! cover workflow validation, template rendering, streamed bodies, response
//! framing, captures, and async control flow together.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use dictate_core::Config;
use dictate_core::advanced_audio::http::HttpEngineError;
use dictate_core::advanced_audio::schema::{
    AudioSpec, Capture, MultipartField, MultipartValue, PollCondition, PollOperator, PollStage,
    ResponseExtractor, SignerConfig, StreamAction, StreamFormat, StreamResponse, StreamRule,
};
use dictate_core::advanced_audio::{
    AdvancedAudioClient, AdvancedAudioConfig, AdvancedAudioError, AdvancedAudioWorkflow,
    AdvancedRecognition, AudioDelivery, HttpBody, HttpMethod, HttpStage, WorkflowSchemaVersion,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const SSE_COMPLETE: &[u8] = include_bytes!("fixtures/advanced_audio/sse_complete.sse");
const SSE_INCOMPLETE: &[u8] = include_bytes!("fixtures/advanced_audio/sse_incomplete.sse");
const NDJSON_COMPLETE: &[u8] = include_bytes!("fixtures/advanced_audio/ndjson_complete.ndjson");
const SSE_REPLACE_PARTIAL_FINAL: &[u8] = b"event: partial\n\
data: {\"text\":\"draft one\"}\n\
\n\
event: partial\n\
data: {\"text\":\"draft two\"}\n\
\n\
event: final\n\
data: {\"text\":\"authoritative final\"}\n\
\n\
event: done\n\
data: {}\n\
\n";

#[derive(Debug)]
struct CapturedRequest {
    method: String,
    target: String,
    headers: String,
    body: Vec<u8>,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        header_value(&self.headers, name)
    }
}

/// Reads one HTTP/1.1 request, including streamed/chunked upload bodies.
/// Reqwest deliberately uses chunked transfer encoding for the raw-audio and
/// multipart file streams, so testing only Content-Length requests would miss
/// the production upload path.
async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut wire = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0, "fake server received an incomplete HTTP request");
        wire.extend_from_slice(&buffer[..read]);

        let Some(header_end) = wire.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&wire[..header_end]).into_owned();
        let body_start = header_end + 4;
        let body = if let Some(value) = header_value(&headers, "content-length") {
            let length = value.trim().parse::<usize>().unwrap();
            if wire.len() < body_start + length {
                continue;
            }
            wire[body_start..body_start + length].to_vec()
        } else if header_value(&headers, "transfer-encoding")
            .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
        {
            let Some(body) = decode_chunked_body(&wire[body_start..]) else {
                continue;
            };
            body
        } else {
            Vec::new()
        };

        let mut request_line = headers.lines().next().unwrap().split_whitespace();
        return CapturedRequest {
            method: request_line.next().unwrap().into(),
            target: request_line.next().unwrap().into(),
            headers,
            body,
        };
    }
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

/// Returns `None` while more wire bytes are necessary; malformed chunked
/// uploads are a test failure because the fake only receives our own client.
fn decode_chunked_body(wire: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = 0;
    let mut body = Vec::new();
    loop {
        let line_end = wire[cursor..]
            .windows(2)
            .position(|bytes| bytes == b"\r\n")?;
        let line_end = cursor + line_end;
        let size_text = std::str::from_utf8(&wire[cursor..line_end]).unwrap();
        let size = usize::from_str_radix(size_text.split(';').next().unwrap(), 16).unwrap();
        cursor = line_end + 2;

        if size == 0 {
            if wire.len() < cursor + 2 {
                return None;
            }
            if &wire[cursor..cursor + 2] == b"\r\n" {
                return Some(body);
            }
            return wire[cursor..]
                .windows(4)
                .any(|bytes| bytes == b"\r\n\r\n")
                .then_some(body);
        }

        if wire.len() < cursor + size + 2 {
            return None;
        }
        assert_eq!(&wire[cursor + size..cursor + size + 2], b"\r\n");
        body.extend_from_slice(&wire[cursor..cursor + size]);
        cursor += size + 2;
    }
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut response = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
}

async fn write_chunked_response(stream: &mut TcpStream, content_type: &str, body: &[u8]) {
    write_chunked_response_with_headers(stream, content_type, &[], body).await;
}

async fn write_chunked_response_with_headers(
    stream: &mut TcpStream,
    content_type: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n");
    for (name, value) in headers {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
    response.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
    stream.write_all(response.as_bytes()).await.unwrap();
    // Intentionally use arbitrary wire chunks: stream decoders must not rely
    // on a network chunk lining up with an SSE event or NDJSON record.
    for chunk in body.chunks(11) {
        stream
            .write_all(format!("{:X}\r\n", chunk.len()).as_bytes())
            .await
            .unwrap();
        stream.write_all(chunk).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
    }
    stream.write_all(b"0\r\n\r\n").await.unwrap();
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn audio_file(directory: &tempfile::TempDir, filename: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = directory.path().join(filename);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn stage(method: HttpMethod, url: String, body: HttpBody) -> HttpStage {
    HttpStage {
        method,
        url,
        query: BTreeMap::new(),
        headers: BTreeMap::new(),
        body,
        accepted_statuses: vec![200],
        signer: SignerConfig::None,
        captures: vec![],
    }
}

fn advanced_client(
    name: &str,
    delivery: AudioDelivery,
    mime: &str,
    recognition: AdvancedRecognition,
) -> AdvancedAudioClient {
    let workflow = AdvancedAudioWorkflow {
        schema_version: WorkflowSchemaVersion::default(),
        name: name.into(),
        parameters: vec![],
        secrets: vec![],
        audio: AudioSpec {
            delivery,
            mime: Some(mime.into()),
        },
        recognition,
    };
    let config = Config {
        request_timeout: 5,
        max_retry: 3,
        retry_base_delay: 0.0,
        enable_http2: false,
        advanced_audio_api: AdvancedAudioConfig {
            enabled: true,
            workflow: Some(workflow),
            ..AdvancedAudioConfig::default()
        },
        ..Config::default()
    };
    AdvancedAudioClient::new(config).unwrap()
}

fn stream_rules_for_sse() -> Vec<StreamRule> {
    vec![
        StreamRule {
            event: Some("delta".into()),
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
    ]
}

fn poll_state_condition(value: &str) -> PollCondition {
    PollCondition {
        from: ResponseExtractor::JsonPath {
            path: "$.state".into(),
        },
        operator: PollOperator::Eq,
        value: Some(value.into()),
        values: vec![],
    }
}

fn poll_stage(request: HttpStage, interval_ms: u64, timeout_ms: u64) -> PollStage {
    PollStage {
        request,
        interval_ms,
        timeout_ms,
        pending: vec![poll_state_condition("pending")],
        success: vec![poll_state_condition("completed")],
        failure: vec![poll_state_condition("failed")],
    }
}

#[tokio::test]
async fn request_multipart_streams_audio_and_preserves_typed_parts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/recognize?mode=multipart");
        assert!(
            request
                .header("content-type")
                .unwrap()
                .starts_with("multipart/form-data; boundary=")
        );
        assert_eq!(request.header("x-workflow"), Some("multipart"));
        assert!(contains_bytes(
            &request.body,
            b"Content-Disposition: form-data; name=\"language\""
        ));
        assert!(contains_bytes(&request.body, b"\r\nzh-CN\r\n"));
        assert!(contains_bytes(
            &request.body,
            b"Content-Disposition: form-data; name=\"audio\"; filename=\"spoken.wav\""
        ));
        assert!(contains_bytes(&request.body, b"multipart audio\x00bytes"));
        assert!(contains_bytes(
            &request.body,
            b"Content-Disposition: form-data; name=\"metadata\""
        ));
        assert!(contains_bytes(&request.body, b"fixture-metadata"));
        write_response(
            &mut stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"text":"multipart transcript"}"#,
        )
        .await;
    });

    let mut request = stage(
        HttpMethod::Post,
        format!("http://{address}/recognize"),
        HttpBody::Multipart {
            fields: vec![
                MultipartField {
                    name: "language".into(),
                    value: MultipartValue::Text {
                        value: "zh-CN".into(),
                    },
                },
                MultipartField {
                    name: "audio".into(),
                    value: MultipartValue::AudioFile,
                },
                MultipartField {
                    name: "metadata".into(),
                    value: MultipartValue::Bytes {
                        value: STANDARD.encode(b"fixture-metadata"),
                    },
                },
            ],
        },
    );
    request.query.insert("mode".into(), "multipart".into());
    request
        .headers
        .insert("x-workflow".into(), "multipart".into());
    let client = advanced_client(
        "multipart request fixture",
        AudioDelivery::MultipartFile,
        "audio/wav",
        AdvancedRecognition::Request {
            request: Box::new(request),
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "spoken.wav", b"multipart audio\x00bytes");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "multipart transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn request_raw_audio_streams_exact_bytes_and_mime() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "PUT");
        assert_eq!(request.target, "/raw");
        assert_eq!(request.header("content-type"), Some("audio/pcm"));
        assert_eq!(request.body, b"\x01\x00\xff\x7f\x00\x80".to_vec());
        write_response(
            &mut stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"text":"raw transcript"}"#,
        )
        .await;
    });

    let client = advanced_client(
        "raw audio request fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Put,
                format!("http://{address}/raw"),
                HttpBody::RawAudio,
            )),
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "samples.pcm", b"\x01\x00\xff\x7f\x00\x80");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "raw transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn request_base64_json_uses_the_encoded_audio_template_once() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/base64");
        assert_eq!(request.header("content-type"), Some("application/json"));
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["audio"], "AAECaGVsbG8=");
        assert_eq!(body["filename"], "base64.wav");
        assert_eq!(body["size"], "8");
        write_response(
            &mut stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"text":"base64 transcript"}"#,
        )
        .await;
    });

    let client = advanced_client(
        "base64 request fixture",
        AudioDelivery::Base64,
        "audio/wav",
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/base64"),
                HttpBody::Json {
                    value: serde_json::json!({
                        "audio": "{{audio:base64}}",
                        "filename": "{{audio:filename}}",
                        "size": "{{audio:size}}"
                    }),
                },
            )),
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "base64.wav", b"\0\x01\x02hello");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "base64 transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn request_stream_sse_requires_done_and_prefers_final_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/sse");
        write_chunked_response(&mut stream, "text/event-stream", SSE_COMPLETE).await;
    });
    let client = advanced_client(
        "SSE stream fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::RequestStream {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/sse"),
                HttpBody::RawAudio,
            )),
            stream: StreamResponse {
                format: StreamFormat::Sse,
                rules: stream_rules_for_sse(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "stream.pcm", b"stream input");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "complete transcript");
    assert_eq!(transcription.raw_response, SSE_COMPLETE);
    server.await.unwrap();
}

#[tokio::test]
async fn request_stream_captures_response_header_before_sse_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/sse-header-capture");
        write_chunked_response_with_headers(
            &mut stream,
            "text/event-stream",
            &[("X-Stream-Session", "header-capture-42")],
            SSE_COMPLETE,
        )
        .await;
    });

    let mut request = stage(
        HttpMethod::Post,
        format!("http://{address}/sse-header-capture"),
        HttpBody::RawAudio,
    );
    request.captures = vec![Capture {
        id: "stream_session".into(),
        from: ResponseExtractor::Header {
            name: "x-stream-session".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "SSE header capture fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::RequestStream {
            request: Box::new(request),
            stream: StreamResponse {
                format: StreamFormat::Sse,
                rules: stream_rules_for_sse(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "stream.pcm", b"stream input");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "complete transcript");
    assert_eq!(transcription.raw_response, SSE_COMPLETE);
    server.await.unwrap();
}

#[tokio::test]
async fn request_stream_sse_replaces_partial_and_prefers_authoritative_final() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/sse-replace-partial");
        write_chunked_response(&mut stream, "text/event-stream", SSE_REPLACE_PARTIAL_FINAL).await;
    });
    let client = advanced_client(
        "SSE partial replacement fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::RequestStream {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/sse-replace-partial"),
                HttpBody::RawAudio,
            )),
            stream: StreamResponse {
                format: StreamFormat::Sse,
                rules: vec![
                    StreamRule {
                        event: Some("partial".into()),
                        path: Some("$.text".into()),
                        action: StreamAction::ReplacePartial,
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
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "stream.pcm", b"stream input");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "authoritative final");
    assert_eq!(transcription.raw_response, SSE_REPLACE_PARTIAL_FINAL);
    server.await.unwrap();
}

#[tokio::test]
async fn request_stream_ndjson_applies_complete_rule() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.target, "/ndjson");
        write_chunked_response(&mut stream, "application/x-ndjson", NDJSON_COMPLETE).await;
    });
    let client = advanced_client(
        "NDJSON stream fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::RequestStream {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/ndjson"),
                HttpBody::RawAudio,
            )),
            stream: StreamResponse {
                format: StreamFormat::Ndjson,
                rules: vec![
                    StreamRule {
                        event: None,
                        path: Some("$.text".into()),
                        action: StreamAction::AppendDelta,
                        equals: None,
                    },
                    StreamRule {
                        event: None,
                        path: None,
                        action: StreamAction::Complete,
                        equals: None,
                    },
                ],
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "stream.pcm", b"stream input");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "NDJSON transcript");
    assert_eq!(transcription.raw_response, NDJSON_COMPLETE);
    server.await.unwrap();
}

#[tokio::test]
async fn request_stream_never_returns_partial_text_without_complete() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _request = read_request(&mut stream).await;
        write_chunked_response(&mut stream, "text/event-stream", SSE_INCOMPLETE).await;
    });
    let client = advanced_client(
        "incomplete SSE stream fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::RequestStream {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/incomplete"),
                HttpBody::RawAudio,
            )),
            stream: StreamResponse {
                format: StreamFormat::Sse,
                rules: stream_rules_for_sse(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "stream.pcm", b"stream input");

    let error = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap_err();
    assert!(matches!(error, AdvancedAudioError::Accumulator(_)));
    server.await.unwrap();
}

#[tokio::test]
async fn async_poll_uses_terminal_absolute_capture_url_for_result_step() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    // Providers commonly return presigned result URLs. Keep both the encoded
    // path and the query string intact when the complete capture becomes the
    // next request URL.
    let result_url = format!(
        "http://{address}/results/final%3A42?Expires=1726211500&OSSAccessKeyId=test-access&Signature=opaque%2Bvalue%3D"
    );
    let completed = format!(r#"{{"state":"completed","result_url":"{result_url}"}}"#);
    let result_response = format!(r#"{{"text":"async transcript","source_url":"{result_url}"}}"#);
    let server = tokio::spawn(async move {
        let mut targets = Vec::new();
        for (index, response) in [
            br#"{"job_id":"job-42"}"#.as_slice(),
            br#"{"state":"processing"}"#.as_slice(),
            completed.as_bytes(),
            result_response.as_bytes(),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            targets.push((request.method.clone(), request.target.clone()));
            match index {
                0 => {
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.target, "/submit");
                    assert_eq!(request.body, b"async audio".to_vec());
                }
                1 | 2 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.target, "/tasks/job-42");
                }
                3 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(
                        request.target,
                        "/results/final%3A42?Expires=1726211500&OSSAccessKeyId=test-access&Signature=opaque%2Bvalue%3D"
                    );
                }
                _ => unreachable!(),
            }
            write_response(
                &mut stream,
                "200 OK",
                &[("Content-Type", "application/json")],
                response,
            )
            .await;
        }
        targets
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/submit"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];

    let mut poll_request = stage(
        HttpMethod::Get,
        format!("http://{address}/tasks/{{{{capture:job_id}}}}"),
        HttpBody::None,
    );
    poll_request.captures = vec![Capture {
        id: "result_url".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.result_url".into(),
        },
        sensitive: false,
    }];
    let poll = PollStage {
        request: poll_request,
        interval_ms: 100,
        timeout_ms: 2_000,
        pending: vec![PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Eq,
            value: Some("processing".into()),
            values: vec![],
        }],
        success: vec![PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Eq,
            value: Some("completed".into()),
            values: vec![],
        }],
        failure: vec![PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Eq,
            value: Some("failed".into()),
            values: vec![],
        }],
    };
    let result = stage(
        HttpMethod::Get,
        "{{capture:result_url}}".into(),
        HttpBody::None,
    );
    let client = advanced_client(
        "async poll fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll)),
            result_steps: vec![result],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "async.pcm", b"async audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "async transcript");
    let raw_response = String::from_utf8(transcription.raw_response).unwrap();
    assert!(!raw_response.contains("test-access"));
    assert!(!raw_response.contains("opaque%2Bvalue%3D"));
    assert!(raw_response.contains("[redacted]"));
    let targets = server.await.unwrap();
    assert_eq!(targets.len(), 4);
    assert_eq!(
        targets
            .iter()
            .filter(|(method, target)| method == "POST" && target == "/submit")
            .count(),
        1,
        "a repeated poll must not resubmit a non-idempotent recognition task"
    );
}

#[tokio::test]
async fn async_result_errors_redact_an_automatic_sensitive_capture_url() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let result_url = format!("http://{address}/results/final?Signature=temporary-result-token");
    let submit_response = format!(r#"{{"result_url":"{result_url}"}}"#);
    let failure_response = format!(r#"{{"error":"could not fetch {result_url}"}}"#);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/submit");
        write_response(
            &mut stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            submit_response.as_bytes(),
        )
        .await;

        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.target,
            "/results/final?Signature=temporary-result-token"
        );
        write_response(
            &mut stream,
            "400 Bad Request",
            &[("Content-Type", "application/json")],
            failure_response.as_bytes(),
        )
        .await;
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/submit"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "result_url".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.result_url".into(),
        },
        // The whole-URL use below must classify this as sensitive even though
        // the workflow did not declare it manually.
        sensitive: false,
    }];
    let result = stage(
        HttpMethod::Get,
        "{{capture:result_url}}".into(),
        HttpBody::None,
    );
    let client = advanced_client(
        "automatic dynamic URL redaction fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: None,
            result_steps: vec![result],
            final_text: ResponseExtractor::PlainBody,
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "automatic-redaction.pcm", b"async audio");

    let error = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdvancedAudioError::UnexpectedStatus { status: 400, .. }
    ));
    let response = String::from_utf8(error.last_response().to_vec()).unwrap();
    assert!(!response.contains("temporary-result-token"));
    assert!(response.contains("[redacted]"));
    server.await.unwrap();
}

#[tokio::test]
async fn async_poll_rejects_non_http_captured_result_urls_before_fetching() {
    for captured_url in [
        "file:///tmp/transcript.json",
        "ftp://downloads.example.test/transcript.json",
        "/results/transcript.json",
        "https://",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured_url = captured_url.to_owned();
        let returned_url = captured_url.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/submit");
            let body = format!(r#"{{"result_url":"{returned_url}"}}"#);
            write_response(
                &mut stream,
                "200 OK",
                &[("Content-Type", "application/json")],
                body.as_bytes(),
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "a rejected dynamic URL must not create a result request"
            );
        });

        let mut submit = stage(
            HttpMethod::Post,
            format!("http://{address}/submit"),
            HttpBody::RawAudio,
        );
        submit.captures = vec![Capture {
            id: "result_url".into(),
            from: ResponseExtractor::JsonPath {
                path: "$.result_url".into(),
            },
            sensitive: true,
        }];
        let result = stage(
            HttpMethod::Get,
            "{{capture:result_url}}".into(),
            HttpBody::None,
        );
        let client = advanced_client(
            "rejected dynamic result URL fixture",
            AudioDelivery::RawAudio,
            "audio/pcm",
            AdvancedRecognition::AsyncPoll {
                prepare: None,
                submit: Box::new(submit),
                poll: None,
                result_steps: vec![result],
                final_text: ResponseExtractor::PlainBody,
            },
        );
        let directory = tempfile::tempdir().unwrap();
        let audio = audio_file(&directory, "dynamic-url.pcm", b"dynamic result URL");

        let error = client
            .transcribe(&CancellationToken::new(), &audio)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AdvancedAudioError::Http(HttpEngineError::InvalidUrl)),
            "expected {captured_url:?} to be rejected before a result request"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn async_poll_retries_truncated_response_body_without_resubmitting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut targets = Vec::new();
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            targets.push((request.method.clone(), request.target.clone()));
            match index {
                0 => {
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.target, "/submit-retry-body");
                    assert_eq!(request.body, b"poll retry audio".to_vec());
                    write_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        br#"{"job_id":"retry-body-42"}"#,
                    )
                    .await;
                }
                1 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.target, "/tasks/retry-body-42");
                    // Declare a longer body than is sent, then close the
                    // connection. Reqwest receives the response headers but
                    // reports a transport error while its body is read.
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{\"state\":\"completed\"}",
                        )
                        .await
                        .unwrap();
                }
                2 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.target, "/tasks/retry-body-42");
                    write_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        br#"{"state":"completed","text":"poll retry transcript"}"#,
                    )
                    .await;
                }
                _ => unreachable!(),
            }
        }
        targets
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/submit-retry-body"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];
    let poll = poll_stage(
        stage(
            HttpMethod::Get,
            format!("http://{address}/tasks/{{{{capture:job_id}}}}"),
            HttpBody::None,
        ),
        100,
        2_000,
    );
    let client = advanced_client(
        "poll response-body retry fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll)),
            result_steps: vec![],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "poll-retry.pcm", b"poll retry audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "poll retry transcript");

    let targets = server.await.unwrap();
    assert_eq!(
        targets,
        vec![
            ("POST".into(), "/submit-retry-body".into()),
            ("GET".into(), "/tasks/retry-body-42".into()),
            ("GET".into(), "/tasks/retry-body-42".into()),
        ],
        "a body-read retry must repeat only the poll request"
    );
}

#[tokio::test]
async fn request_data_uri_json_renders_declared_mime_and_exact_audio() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/data-uri");
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["audio"], "data:audio/ogg;base64,AP9oaQ==");
        assert_eq!(body["filename"], "voice.ogg");
        write_response(
            &mut stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"text":"data URI transcript"}"#,
        )
        .await;
    });

    let client = advanced_client(
        "data URI request fixture",
        AudioDelivery::DataUri,
        "audio/ogg",
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/data-uri"),
                HttpBody::Json {
                    value: serde_json::json!({
                        "audio": "{{audio:data_uri}}",
                        "filename": "{{audio:filename}}"
                    }),
                },
            )),
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "voice.ogg", b"\0\xffhi");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "data URI transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn provider_upload_prepare_capture_flows_into_submit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut upload_stream, _) = listener.accept().await.unwrap();
        let upload = read_request(&mut upload_stream).await;
        assert_eq!(upload.method, "POST");
        assert_eq!(upload.target, "/provider-upload");
        assert_eq!(upload.header("content-type"), Some("audio/pcm"));
        assert_eq!(upload.body, b"provider upload audio".to_vec());
        write_response(
            &mut upload_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"provider_audio":"provider://upload-42"}"#,
        )
        .await;

        let (mut submit_stream, _) = listener.accept().await.unwrap();
        let submit = read_request(&mut submit_stream).await;
        assert_eq!(submit.method, "POST");
        assert_eq!(submit.target, "/recognize");
        let body: serde_json::Value = serde_json::from_slice(&submit.body).unwrap();
        assert_eq!(body["audio"], "provider://upload-42");
        write_response(
            &mut submit_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"text":"provider upload transcript"}"#,
        )
        .await;
    });

    let mut prepare = stage(
        HttpMethod::Post,
        format!("http://{address}/provider-upload"),
        HttpBody::RawAudio,
    );
    prepare.captures = vec![Capture {
        id: "provider_audio".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.provider_audio".into(),
        },
        sensitive: false,
    }];
    let submit = stage(
        HttpMethod::Post,
        format!("http://{address}/recognize"),
        HttpBody::Json {
            value: serde_json::json!({"audio": "{{capture:provider_audio}}"}),
        },
    );
    let client = advanced_client(
        "provider upload fixture",
        AudioDelivery::ProviderUpload,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: Some(Box::new(prepare)),
            submit: Box::new(submit),
            poll: None,
            result_steps: vec![],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "provider.pcm", b"provider upload audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "provider upload transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn async_poll_supports_post_without_resending_audio() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for (index, response) in [
            br#"{"job_id":"post-42"}"#.as_slice(),
            br#"{"state":"queued"}"#.as_slice(),
            br#"{"state":"completed","text":"POST poll transcript"}"#.as_slice(),
        ]
        .into_iter()
        .enumerate()
        {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            match index {
                0 => {
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.target, "/submit-post-poll");
                    assert_eq!(request.body, b"post poll audio".to_vec());
                }
                1 | 2 => {
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.target, "/poll-post");
                    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(body, serde_json::json!({"job_id": "post-42"}));
                }
                _ => unreachable!(),
            }
            write_response(
                &mut stream,
                "200 OK",
                &[("Content-Type", "application/json")],
                response,
            )
            .await;
        }
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/submit-post-poll"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];
    let poll = PollStage {
        request: stage(
            HttpMethod::Post,
            format!("http://{address}/poll-post"),
            HttpBody::Json {
                value: serde_json::json!({"job_id": "{{capture:job_id}}"}),
            },
        ),
        interval_ms: 100,
        timeout_ms: 2_000,
        pending: vec![PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Eq,
            value: Some("queued".into()),
            values: vec![],
        }],
        success: vec![PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Eq,
            value: Some("completed".into()),
            values: vec![],
        }],
        failure: vec![PollCondition {
            from: ResponseExtractor::JsonPath {
                path: "$.state".into(),
            },
            operator: PollOperator::Eq,
            value: Some("failed".into()),
            values: vec![],
        }],
    };
    let client = advanced_client(
        "POST poll fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll)),
            result_steps: vec![],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "post-poll.pcm", b"post poll audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();
    assert_eq!(transcription.text, "POST poll transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn request_captures_response_header_before_extracting_final_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/request-header-capture");
        write_response(
            &mut stream,
            "200 OK",
            &[
                ("Content-Type", "application/json"),
                ("X-Request-Session", "request-header-42"),
            ],
            br#"{"text":"request header transcript"}"#,
        )
        .await;
    });

    let mut request = stage(
        HttpMethod::Post,
        format!("http://{address}/request-header-capture"),
        HttpBody::RawAudio,
    );
    request.captures = vec![Capture {
        id: "request_session".into(),
        from: ResponseExtractor::Header {
            name: "x-request-session".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "request header capture fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::Request {
            request: Box::new(request),
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "request-header.pcm", b"request header audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();

    assert_eq!(transcription.text, "request header transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn request_cancellation_interrupts_an_in_flight_http_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (request_started, request_received) = oneshot::channel();
    let (release_server, release_received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.target, "/cancel-request");
        let _ = request_started.send(());
        let _ = release_received.await;
    });

    let client = advanced_client(
        "request cancellation fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/cancel-request"),
                HttpBody::RawAudio,
            )),
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "cancel-request.pcm", b"cancel request audio");
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move { client.transcribe(&task_cancellation, &audio).await });

    request_received.await.unwrap();
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("request cancellation must interrupt the HTTP request")
        .unwrap();
    let _ = release_server.send(());
    server.await.unwrap();

    assert!(matches!(result, Err(AdvancedAudioError::Canceled)));
}

#[tokio::test]
async fn request_stream_cancellation_interrupts_a_pending_event_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (response_started, response_received) = oneshot::channel();
    let (release_server, release_received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert_eq!(request.target, "/cancel-stream");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let _ = response_started.send(());
        let _ = release_received.await;
    });

    let client = advanced_client(
        "stream cancellation fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::RequestStream {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/cancel-stream"),
                HttpBody::RawAudio,
            )),
            stream: StreamResponse {
                format: StreamFormat::Sse,
                rules: stream_rules_for_sse(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "cancel-stream.pcm", b"cancel stream audio");
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move { client.transcribe(&task_cancellation, &audio).await });

    response_received.await.unwrap();
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("stream cancellation must interrupt the pending event stream")
        .unwrap();
    let _ = release_server.send(());
    server.await.unwrap();

    assert!(matches!(result, Err(AdvancedAudioError::Canceled)));
}

#[tokio::test]
async fn async_poll_reports_terminal_failure_state() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut submit_stream, _) = listener.accept().await.unwrap();
        let submit = read_request(&mut submit_stream).await;
        assert_eq!(submit.method, "POST");
        assert_eq!(submit.target, "/failure-submit");
        write_response(
            &mut submit_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"job_id":"failed-42"}"#,
        )
        .await;

        let (mut poll_stream, _) = listener.accept().await.unwrap();
        let poll = read_request(&mut poll_stream).await;
        assert_eq!(poll.method, "GET");
        assert_eq!(poll.target, "/failure-tasks/failed-42");
        write_response(
            &mut poll_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"state":"failed"}"#,
        )
        .await;
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/failure-submit"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "async poll failure fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll_stage(
                stage(
                    HttpMethod::Get,
                    format!("http://{address}/failure-tasks/{{{{capture:job_id}}}}"),
                    HttpBody::None,
                ),
                100,
                500,
            ))),
            result_steps: vec![],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "failure.pcm", b"failure audio");

    let error = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap_err();

    assert!(matches!(error, AdvancedAudioError::PollFailure));
    server.await.unwrap();
}

#[tokio::test]
async fn async_poll_times_out_after_pending_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_server, mut stop_received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut submit_stream, _) = listener.accept().await.unwrap();
        let submit = read_request(&mut submit_stream).await;
        assert_eq!(submit.target, "/timeout-submit");
        write_response(
            &mut submit_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"job_id":"timeout-42"}"#,
        )
        .await;

        loop {
            tokio::select! {
                _ = &mut stop_received => break,
                accepted = listener.accept() => {
                    let (mut poll_stream, _) = accepted.unwrap();
                    let poll = read_request(&mut poll_stream).await;
                    assert_eq!(poll.target, "/timeout-tasks/timeout-42");
                    write_response(
                        &mut poll_stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        br#"{"state":"pending"}"#,
                    )
                    .await;
                }
            }
        }
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/timeout-submit"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "async poll timeout fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll_stage(
                stage(
                    HttpMethod::Get,
                    format!("http://{address}/timeout-tasks/{{{{capture:job_id}}}}"),
                    HttpBody::None,
                ),
                100,
                150,
            ))),
            result_steps: vec![],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "timeout.pcm", b"timeout audio");

    let error = tokio::time::timeout(
        Duration::from_secs(2),
        client.transcribe(&CancellationToken::new(), &audio),
    )
    .await
    .expect("pending poll must reach its workflow timeout")
    .unwrap_err();
    let _ = stop_server.send(());
    server.await.unwrap();

    assert!(matches!(
        error,
        AdvancedAudioError::PollTimeout { timeout_ms: 150 }
    ));
}

#[tokio::test]
async fn async_poll_cancellation_interrupts_a_pending_poll_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (poll_started, poll_received) = oneshot::channel();
    let (release_server, release_received) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut submit_stream, _) = listener.accept().await.unwrap();
        let submit = read_request(&mut submit_stream).await;
        assert_eq!(submit.target, "/cancel-poll-submit");
        write_response(
            &mut submit_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"job_id":"cancel-42"}"#,
        )
        .await;

        let (mut poll_stream, _) = listener.accept().await.unwrap();
        let poll = read_request(&mut poll_stream).await;
        assert_eq!(poll.target, "/cancel-poll-tasks/cancel-42");
        let _ = poll_started.send(());
        let _ = release_received.await;
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/cancel-poll-submit"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "async poll cancellation fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll_stage(
                stage(
                    HttpMethod::Get,
                    format!("http://{address}/cancel-poll-tasks/{{{{capture:job_id}}}}"),
                    HttpBody::None,
                ),
                100,
                2_000,
            ))),
            result_steps: vec![],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "cancel-poll.pcm", b"cancel poll audio");
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move { client.transcribe(&task_cancellation, &audio).await });

    poll_received.await.unwrap();
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("poll cancellation must interrupt the HTTP request")
        .unwrap();
    let _ = release_server.send(());
    server.await.unwrap();

    assert!(matches!(result, Err(AdvancedAudioError::Canceled)));
}

#[tokio::test]
async fn async_poll_captures_terminal_header_for_a_result_step() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut submit_stream, _) = listener.accept().await.unwrap();
        let submit = read_request(&mut submit_stream).await;
        assert_eq!(submit.target, "/header-poll-submit");
        write_response(
            &mut submit_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"job_id":"header-42"}"#,
        )
        .await;

        let (mut poll_stream, _) = listener.accept().await.unwrap();
        let poll = read_request(&mut poll_stream).await;
        assert_eq!(poll.target, "/header-poll-tasks/header-42");
        write_response(
            &mut poll_stream,
            "200 OK",
            &[
                ("Content-Type", "application/json"),
                ("X-Result-Token", "result-header-42"),
            ],
            br#"{"state":"completed"}"#,
        )
        .await;

        let (mut result_stream, _) = listener.accept().await.unwrap();
        let result = read_request(&mut result_stream).await;
        assert_eq!(result.target, "/header-results/result-header-42");
        write_response(
            &mut result_stream,
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"text":"poll header transcript"}"#,
        )
        .await;
    });

    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/header-poll-submit"),
        HttpBody::RawAudio,
    );
    submit.captures = vec![Capture {
        id: "job_id".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.job_id".into(),
        },
        sensitive: false,
    }];
    let mut poll_request = stage(
        HttpMethod::Get,
        format!("http://{address}/header-poll-tasks/{{{{capture:job_id}}}}"),
        HttpBody::None,
    );
    poll_request.captures = vec![Capture {
        id: "result_token".into(),
        from: ResponseExtractor::Header {
            name: "x-result-token".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "async poll header capture fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(submit),
            poll: Some(Box::new(poll_stage(poll_request, 100, 500))),
            result_steps: vec![stage(
                HttpMethod::Get,
                format!("http://{address}/header-results/{{{{capture:result_token}}}}"),
                HttpBody::None,
            )],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "header-poll.pcm", b"header poll audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();

    assert_eq!(transcription.text, "poll header transcript");
    server.await.unwrap();
}

#[tokio::test]
async fn async_poll_runs_two_result_steps_and_retries_a_read_only_fetch() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for index in 0..4 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            match index {
                0 => {
                    assert_eq!(request.method, "POST");
                    assert_eq!(request.target, "/two-results-submit");
                    write_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        br#"{"task":"two-results-42"}"#,
                    )
                    .await;
                }
                1 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.target, "/two-results/first");
                    write_response(
                        &mut stream,
                        "200 OK",
                        &[
                            ("Content-Type", "application/json"),
                            ("X-Next-Result", "final-42"),
                        ],
                        br#"{}"#,
                    )
                    .await;
                }
                2 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.target, "/two-results/final-42");
                    write_response(
                        &mut stream,
                        "503 Service Unavailable",
                        &[("Content-Type", "application/json")],
                        br#"{"error":"retry this read"}"#,
                    )
                    .await;
                }
                3 => {
                    assert_eq!(request.method, "GET");
                    assert_eq!(request.target, "/two-results/final-42");
                    write_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/json")],
                        br#"{"text":"two result transcript"}"#,
                    )
                    .await;
                }
                _ => unreachable!(),
            }
        }
    });

    let mut first_result = stage(
        HttpMethod::Get,
        format!("http://{address}/two-results/first"),
        HttpBody::None,
    );
    first_result.captures = vec![Capture {
        id: "next_result".into(),
        from: ResponseExtractor::Header {
            name: "x-next-result".into(),
        },
        sensitive: false,
    }];
    let client = advanced_client(
        "two result stages fixture",
        AudioDelivery::RawAudio,
        "audio/pcm",
        AdvancedRecognition::AsyncPoll {
            prepare: None,
            submit: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/two-results-submit"),
                HttpBody::RawAudio,
            )),
            poll: None,
            result_steps: vec![
                first_result,
                stage(
                    HttpMethod::Get,
                    format!("http://{address}/two-results/{{{{capture:next_result}}}}"),
                    HttpBody::None,
                ),
            ],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let directory = tempfile::tempdir().unwrap();
    let audio = audio_file(&directory, "two-results.pcm", b"two results audio");

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();

    assert_eq!(transcription.text, "two result transcript");
    server.await.unwrap();
}
