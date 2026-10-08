//! End-to-end coverage for segmented Advanced Audio API execution.
//!
//! These tests use local TCP fakes and deliberately enter through the public
//! batch and test-workflow entry points.  They therefore verify that every
//! prepared segment runs the complete existing Advanced lifecycle rather than
//! only exercising the generic scheduler with synthetic futures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use dictate_core::Config;
use dictate_core::advanced_audio::schema::{
    AliyunOssRemoteAudioConfig, AudioSpec, Capture, PollCondition, PollOperator, PollStage,
    ResponseExtractor, S3RemoteAudioConfig, SignerConfig, StreamAction, StreamFormat,
    StreamResponse, StreamRule, WebDavRemoteAudioConfig,
};
use dictate_core::advanced_audio::{
    AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery, HttpBody,
    HttpMethod, HttpStage, RemoteAudioConfig, WorkflowSchemaVersion,
};
use dictate_core::audio_api::AudioApiClient;
use dictate_core::converter::{AudioConverter, ConvertError, SegmentAnalysis, SourceFrameInterval};
use dictate_core::runtime::test_audio_api_with_source;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct CapturedRequest {
    method: String,
    target: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    let mut wire = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        assert!(read > 0, "fake server received an incomplete request");
        wire.extend_from_slice(&buffer[..read]);

        let Some(header_end) = wire.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&wire[..header_end]);
        let body_start = header_end + 4;
        let body = if let Some(length) = header_value(&headers, "content-length") {
            let length = length.parse::<usize>().unwrap();
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

fn decode_chunked_body(wire: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = 0;
    let mut body = Vec::new();
    loop {
        let line_end = wire[cursor..]
            .windows(2)
            .position(|bytes| bytes == b"\r\n")?;
        let line_end = cursor + line_end;
        let size_text = std::str::from_utf8(&wire[cursor..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?, 16).ok()?;
        cursor = line_end + 2;
        if size == 0 {
            if wire.len() < cursor + 2 {
                return None;
            }
            return (&wire[cursor..cursor + 2] == b"\r\n").then_some(body);
        }
        if wire.len() < cursor + size + 2 {
            return None;
        }
        assert_eq!(&wire[cursor + size..cursor + size + 2], b"\r\n");
        body.extend_from_slice(&wire[cursor..cursor + size]);
        cursor += size + 2;
    }
}

async fn write_response(stream: &mut TcpStream, status: &str, body: &[u8]) {
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    stream.write_all(body).await.unwrap();
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

fn advanced_config(delivery: AudioDelivery, recognition: AdvancedRecognition) -> Config {
    Config {
        max_retry: 1,
        retry_base_delay: 0.0,
        advanced_audio_api: AdvancedAudioConfig {
            enabled: true,
            workflow: Some(AdvancedAudioWorkflow {
                schema_version: WorkflowSchemaVersion::default(),
                name: "segmented Advanced workflow integration test".into(),
                parameters: vec![],
                secrets: vec![],
                audio: AudioSpec {
                    delivery,
                    mime: Some("audio/pcm".into()),
                },
                recognition,
            }),
            values: BTreeMap::new(),
            secrets: BTreeMap::new(),
            remote_audio: RemoteAudioConfig::None,
        },
        ..Config::default()
    }
}

fn two_segments(directory: &tempfile::TempDir) -> Vec<PathBuf> {
    let first = directory.path().join("segment-000000.pcm");
    let second = directory.path().join("segment-000001.pcm");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, b"second").unwrap();
    vec![first, second]
}

fn transcript_for_audio(body: &[u8]) -> &'static [u8] {
    match body {
        b"first" => b"A",
        b"second" => b"B",
        unexpected => panic!("unexpected segment payload: {unexpected:?}"),
    }
}

#[tokio::test]
async fn batch_runs_every_advanced_request_segment_and_concatenates_source_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/recognize");
            write_response(&mut stream, "200 OK", transcript_for_audio(&request.body)).await;
        }
    });
    let config = advanced_config(
        AudioDelivery::RawAudio,
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/recognize"),
                HttpBody::RawAudio,
            )),
            final_text: ResponseExtractor::PlainBody,
        },
    );
    let client = AudioApiClient::new(config).unwrap();
    let directory = tempfile::tempdir().unwrap();

    let transcription = client
        .transcribe_segments(&CancellationToken::new(), &two_segments(&directory), 1)
        .await
        .unwrap();

    assert_eq!(transcription.text, "AB");
    assert!(transcription.raw_response.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn batch_runs_every_advanced_stream_segment_to_final_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            assert_eq!(request.method, "POST");
            let text = std::str::from_utf8(transcript_for_audio(&request.body)).unwrap();
            let response = format!(
                "event: delta\ndata: {{\"text\":\"{text}\"}}\n\nevent: done\ndata: {{}}\n\n"
            );
            write_response(&mut stream, "200 OK", response.as_bytes()).await;
        }
    });
    let config = advanced_config(
        AudioDelivery::RawAudio,
        AdvancedRecognition::RequestStream {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/stream"),
                HttpBody::RawAudio,
            )),
            stream: StreamResponse {
                format: StreamFormat::Sse,
                rules: vec![
                    StreamRule {
                        event: Some("delta".into()),
                        path: Some("$.text".into()),
                        action: StreamAction::AppendDelta,
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
    let client = AudioApiClient::new(config).unwrap();
    let directory = tempfile::tempdir().unwrap();

    let transcription = client
        .transcribe_segments(&CancellationToken::new(), &two_segments(&directory), 1)
        .await
        .unwrap();

    assert_eq!(transcription.text, "AB");
    server.await.unwrap();
}

fn terminal_poll_condition(value: &str) -> PollCondition {
    PollCondition {
        from: ResponseExtractor::JsonPath {
            path: "$.state".into(),
        },
        operator: PollOperator::Eq,
        value: Some(value.into()),
        values: vec![],
    }
}

#[tokio::test]
async fn batch_runs_provider_upload_async_poll_lifecycle_for_every_segment() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for (audio, provider, task, text) in [
            (b"first".as_slice(), "provider://first", "task-first", "A"),
            (
                b"second".as_slice(),
                "provider://second",
                "task-second",
                "B",
            ),
        ] {
            let (mut upload_stream, _) = listener.accept().await.unwrap();
            let upload = read_request(&mut upload_stream).await;
            assert_eq!(upload.method, "POST");
            assert_eq!(upload.target, "/provider-upload");
            assert_eq!(upload.body, audio);
            write_response(
                &mut upload_stream,
                "200 OK",
                format!(r#"{{"provider_audio":"{provider}"}}"#).as_bytes(),
            )
            .await;

            let (mut submit_stream, _) = listener.accept().await.unwrap();
            let submit = read_request(&mut submit_stream).await;
            assert_eq!(submit.method, "POST");
            assert_eq!(submit.target, "/submit");
            let submit_body: serde_json::Value = serde_json::from_slice(&submit.body).unwrap();
            assert_eq!(submit_body["audio"], provider);
            write_response(
                &mut submit_stream,
                "200 OK",
                format!(r#"{{"task":"{task}"}}"#).as_bytes(),
            )
            .await;

            let (mut poll_stream, _) = listener.accept().await.unwrap();
            let poll = read_request(&mut poll_stream).await;
            assert_eq!(poll.method, "POST");
            assert_eq!(poll.target, "/poll");
            let poll_body: serde_json::Value = serde_json::from_slice(&poll.body).unwrap();
            assert_eq!(poll_body["task"], task);
            write_response(&mut poll_stream, "200 OK", br#"{"state":"completed"}"#).await;

            let (mut result_stream, _) = listener.accept().await.unwrap();
            let result = read_request(&mut result_stream).await;
            assert_eq!(result.method, "GET");
            assert_eq!(result.target, format!("/result/{task}"));
            write_response(
                &mut result_stream,
                "200 OK",
                format!(r#"{{"text":"{text}"}}"#).as_bytes(),
            )
            .await;
        }
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
    let mut submit = stage(
        HttpMethod::Post,
        format!("http://{address}/submit"),
        HttpBody::Json {
            value: serde_json::json!({"audio": "{{capture:provider_audio}}"}),
        },
    );
    submit.captures = vec![Capture {
        id: "task".into(),
        from: ResponseExtractor::JsonPath {
            path: "$.task".into(),
        },
        sensitive: false,
    }];
    let poll = PollStage {
        request: stage(
            HttpMethod::Post,
            format!("http://{address}/poll"),
            HttpBody::Json {
                value: serde_json::json!({"task": "{{capture:task}}"}),
            },
        ),
        interval_ms: 100,
        timeout_ms: 2_000,
        pending: vec![terminal_poll_condition("pending")],
        success: vec![terminal_poll_condition("completed")],
        failure: vec![terminal_poll_condition("failed")],
    };
    let config = advanced_config(
        AudioDelivery::ProviderUpload,
        AdvancedRecognition::AsyncPoll {
            prepare: Some(Box::new(prepare)),
            submit: Box::new(submit),
            poll: Some(Box::new(poll)),
            result_steps: vec![stage(
                HttpMethod::Get,
                format!("http://{address}/result/{{{{capture:task}}}}"),
                HttpBody::None,
            )],
            final_text: ResponseExtractor::JsonPath {
                path: "$.text".into(),
            },
        },
    );
    let client = AudioApiClient::new(config).unwrap();
    let directory = tempfile::tempdir().unwrap();

    let transcription = client
        .transcribe_segments(&CancellationToken::new(), &two_segments(&directory), 1)
        .await
        .unwrap();

    assert_eq!(transcription.text, "AB");
    server.await.unwrap();
}

async fn assert_remote_batch_lifecycle(
    listener: TcpListener,
    expected_reference_prefix: &'static str,
) -> Vec<CapturedRequest> {
    let mut requests = Vec::new();
    let mut uploaded = Vec::new();
    for _ in 0..6 {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        match request.method.as_str() {
            "PUT" => {
                uploaded.push((request.target.clone(), request.body.clone()));
                write_response(&mut stream, "201 Created", b"").await;
            }
            "POST" => {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let reference = body["audio"].as_str().unwrap();
                assert!(
                    reference.starts_with(expected_reference_prefix),
                    "unexpected remote audio reference: {reference}"
                );
                let text = match uploaded.last().unwrap().1.as_slice() {
                    b"first" => b"A".as_slice(),
                    b"second" => b"B".as_slice(),
                    unexpected => panic!("unexpected uploaded segment: {unexpected:?}"),
                };
                write_response(&mut stream, "200 OK", text).await;
            }
            "DELETE" => {
                assert_eq!(request.target, uploaded.last().unwrap().0);
                write_response(&mut stream, "204 No Content", b"").await;
            }
            unexpected => panic!("unexpected remote lifecycle request: {unexpected}"),
        }
        requests.push(request);
    }
    assert_eq!(
        uploaded
            .iter()
            .map(|(_, body)| body.as_slice())
            .collect::<Vec<_>>(),
        vec![b"first".as_slice(), b"second".as_slice()]
    );
    assert_ne!(
        uploaded[0].0, uploaded[1].0,
        "each segment must receive an independent remote object"
    );
    requests
}

#[tokio::test]
async fn batch_runs_webdav_public_url_lifecycle_for_each_segment() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(assert_remote_batch_lifecycle(
        listener,
        "https://downloads.example.test/recognition/",
    ));
    let mut config = advanced_config(
        AudioDelivery::PublicHttpsUrl,
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/recognize"),
                HttpBody::Json {
                    value: serde_json::json!({"audio": "{{audio:public_url}}"}),
                },
            )),
            final_text: ResponseExtractor::PlainBody,
        },
    );
    config.advanced_audio_api.remote_audio = RemoteAudioConfig::Webdav(WebDavRemoteAudioConfig {
        upload_base_url: format!("http://{address}/private-dav"),
        username: "user".into(),
        password: "password".into(),
        remote_path_prefix: "segments".into(),
        public_download_base_url: "https://downloads.example.test/recognition".into(),
        delete_after_recognition: true,
    });
    let client = AudioApiClient::new(config).unwrap();
    let directory = tempfile::tempdir().unwrap();

    let transcription = client
        .transcribe_segments(&CancellationToken::new(), &two_segments(&directory), 1)
        .await
        .unwrap();

    assert_eq!(transcription.text, "AB");
    let requests = server.await.unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        vec!["PUT", "POST", "DELETE", "PUT", "POST", "DELETE"]
    );
}

#[tokio::test]
async fn batch_runs_s3_cloud_uri_lifecycle_for_each_segment() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(assert_remote_batch_lifecycle(
        listener,
        "s3://dictate-audio/",
    ));
    let mut config = advanced_config(
        AudioDelivery::CloudUri,
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/recognize"),
                HttpBody::Json {
                    value: serde_json::json!({"audio": "{{audio:cloud_uri}}"}),
                },
            )),
            final_text: ResponseExtractor::PlainBody,
        },
    );
    config.advanced_audio_api.remote_audio = RemoteAudioConfig::S3Compatible(S3RemoteAudioConfig {
        endpoint: format!("http://{address}/private-s3"),
        region: "us-east-1".into(),
        bucket: "dictate-audio".into(),
        access_key: "access".into(),
        secret_key: "secret".into(),
        prefix: "segments".into(),
        public_url_base: None,
        presigned: false,
        delete_after_recognition: true,
    });
    let client = AudioApiClient::new(config).unwrap();
    let directory = tempfile::tempdir().unwrap();

    let transcription = client
        .transcribe_segments(&CancellationToken::new(), &two_segments(&directory), 1)
        .await
        .unwrap();

    assert_eq!(transcription.text, "AB");
    let requests = server.await.unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        vec!["PUT", "POST", "DELETE", "PUT", "POST", "DELETE"]
    );
}

#[tokio::test]
async fn batch_runs_aliyun_oss_cloud_uri_lifecycle_for_each_segment() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(assert_remote_batch_lifecycle(
        listener,
        "oss://dictate-audio/",
    ));
    let mut config = advanced_config(
        AudioDelivery::CloudUri,
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/recognize"),
                HttpBody::Json {
                    value: serde_json::json!({"audio": "{{audio:cloud_uri}}"}),
                },
            )),
            final_text: ResponseExtractor::PlainBody,
        },
    );
    config.advanced_audio_api.remote_audio =
        RemoteAudioConfig::AliyunOss(AliyunOssRemoteAudioConfig {
            endpoint: format!("http://{address}/private-oss"),
            bucket: "dictate-audio".into(),
            access_key: "access".into(),
            secret_key: "secret".into(),
            prefix: "segments".into(),
            public_url_base: None,
            presigned: false,
            delete_after_recognition: true,
        });
    let client = AudioApiClient::new(config).unwrap();
    let directory = tempfile::tempdir().unwrap();

    let transcription = client
        .transcribe_segments(&CancellationToken::new(), &two_segments(&directory), 1)
        .await
        .unwrap();

    assert_eq!(transcription.text, "AB");
    let requests = server.await.unwrap();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        vec!["PUT", "POST", "DELETE", "PUT", "POST", "DELETE"]
    );
}

#[derive(Default)]
struct TestSegmentConverter {
    analyses: AtomicUsize,
    exports: AtomicUsize,
}

#[async_trait]
impl AudioConverter for TestSegmentConverter {
    async fn convert(
        &self,
        _: &CancellationToken,
        _: &Config,
        _: &Path,
        _: &Path,
        _: i32,
    ) -> Result<(), ConvertError> {
        Err(ConvertError::Failed {
            message: "the segmented GUI test path must not call convert".into(),
        })
    }

    async fn analyze_segments(
        &self,
        _: &CancellationToken,
        _: &Config,
        _: &Path,
        min_pause_ms: u32,
    ) -> Result<SegmentAnalysis, ConvertError> {
        assert_eq!(min_pause_ms, 1);
        self.analyses.fetch_add(1, Ordering::SeqCst);
        Ok(SegmentAnalysis {
            source_rate: 1_000,
            source_frames: 2_000,
            silence_intervals: vec![],
        })
    }

    async fn export_segments(
        &self,
        _: &CancellationToken,
        config: &Config,
        _: &Path,
        outputs: &[PathBuf],
        intervals: &[SourceFrameInterval],
        expected_source_rate: u32,
        expected_source_frames: u64,
    ) -> Result<(), ConvertError> {
        assert!(!config.enable_vad);
        assert_eq!(expected_source_rate, 1_000);
        assert_eq!(expected_source_frames, 2_000);
        assert_eq!(
            intervals,
            [
                SourceFrameInterval {
                    start_frame: 0,
                    end_frame: 1_000,
                },
                SourceFrameInterval {
                    start_frame: 1_000,
                    end_frame: 2_000,
                },
            ]
        );
        assert_eq!(outputs.len(), 2);
        std::fs::write(&outputs[0], b"first").map_err(|error| ConvertError::Failed {
            message: error.to_string(),
        })?;
        std::fs::write(&outputs[1], b"second").map_err(|error| ConvertError::Failed {
            message: error.to_string(),
        })?;
        self.exports.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn gui_test_workflow_entry_runs_the_same_advanced_segment_batch() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for expected in [b"first".as_slice(), b"second".as_slice()] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/recognize");
            assert_eq!(request.body, expected);
            write_response(&mut stream, "200 OK", b"test workflow success").await;
        }
    });
    let mut config = advanced_config(
        AudioDelivery::RawAudio,
        AdvancedRecognition::Request {
            request: Box::new(stage(
                HttpMethod::Post,
                format!("http://{address}/recognize"),
                HttpBody::RawAudio,
            )),
            final_text: ResponseExtractor::PlainBody,
        },
    );
    config.enable_segmented_upload = true;
    config.max_upload_segment_seconds = 1;
    config.min_upload_pause_ms = 1;
    config.max_upload_concurrency = 1;
    config.codecs = "pcm".into();
    config.container = "wav".into();
    let source_directory = tempfile::tempdir().unwrap();
    let source = source_directory.path().join("test-source.wav");
    std::fs::write(&source, b"caller-owned source").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let converter = TestSegmentConverter::default();

    test_audio_api_with_source(
        config,
        &converter,
        &source,
        16_000,
        scratch.path(),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(converter.analyses.load(Ordering::SeqCst), 1);
    assert_eq!(converter.exports.load(Ordering::SeqCst), 1);
    assert!(
        source.exists(),
        "the GUI-owned source is removed by its caller"
    );
    assert!(
        std::fs::read_dir(scratch.path()).unwrap().next().is_none(),
        "the high-level GUI test helper must remove its generated segment directory"
    );
    server.await.unwrap();
}
