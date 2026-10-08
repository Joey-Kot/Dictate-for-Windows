use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dictate_core::Config;
use dictate_core::advanced_audio::schema::{
    AudioSpec, ResponseExtractor, S3RemoteAudioConfig, SignerConfig, WebDavRemoteAudioConfig,
};
use dictate_core::advanced_audio::{
    AdvancedAudioClient, AdvancedAudioConfig, AdvancedAudioError, AdvancedAudioWorkflow,
    AdvancedRecognition, AudioDelivery, HttpBody, HttpMethod, HttpStage, RemoteAudioConfig,
    WorkflowSchemaVersion,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum RecognitionResponse {
    Success,
    Failure,
    WaitForCancellation,
}

#[derive(Clone)]
struct CapturedRequest {
    method: String,
    target: String,
    headers: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> CapturedRequest {
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
        let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
        let content_length = headers
            .to_ascii_lowercase()
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default();
        if request.len() < header_end + 4 + content_length {
            continue;
        }
        let mut request_line = headers.lines().next().unwrap().split_whitespace();
        return CapturedRequest {
            method: request_line.next().unwrap().to_owned(),
            target: request_line.next().unwrap().to_owned(),
            headers,
            body: request[header_end + 4..].to_vec(),
        };
    }
}

async fn start_webdav_workflow_server(
    recognition: RecognitionResponse,
    post_started: Option<oneshot::Sender<()>>,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<Vec<CapturedRequest>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server = tokio::spawn({
        let captured = Arc::clone(&captured);
        async move {
            let mut post_started = post_started;
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                let method = request.method.clone();
                captured.lock().unwrap().push(request);
                match method.as_str() {
                    "PUT" => {
                        stream
                            .write_all(
                                b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await
                            .unwrap();
                    }
                    "POST" => match recognition {
                        RecognitionResponse::Success => {
                            let body = br#"{"text":"complete"}"#;
                            stream
                                .write_all(
                                    format!(
                                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len()
                                    )
                                    .as_bytes(),
                                )
                                .await
                                .unwrap();
                            stream.write_all(body).await.unwrap();
                        }
                        RecognitionResponse::Failure => {
                            let body = b"failed";
                            stream
                                .write_all(
                                    format!(
                                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        body.len()
                                    )
                                    .as_bytes(),
                                )
                                .await
                                .unwrap();
                            stream.write_all(body).await.unwrap();
                        }
                        RecognitionResponse::WaitForCancellation => {
                            post_started.take().unwrap().send(()).unwrap();
                            // Do not respond to recognition.  The outer
                            // listener can still receive the later cleanup
                            // request once the client cancellation wins.
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                drop(stream);
                            });
                        }
                    },
                    "DELETE" => {
                        stream
                            .write_all(
                                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await
                            .unwrap();
                    }
                    other => panic!("unexpected fake-server method: {other}"),
                }
            }
            captured.lock().unwrap().clone()
        }
    });
    (address, server)
}

fn config_for_webdav(address: std::net::SocketAddr, delete_after_recognition: bool) -> Config {
    Config {
        max_retry: 1,
        retry_base_delay: 0.0,
        advanced_audio_api: AdvancedAudioConfig {
            enabled: true,
            workflow: Some(AdvancedAudioWorkflow {
                schema_version: WorkflowSchemaVersion::default(),
                name: "WebDAV cleanup lifecycle test".into(),
                parameters: vec![],
                secrets: vec![],
                audio: AudioSpec {
                    delivery: AudioDelivery::PublicHttpsUrl,
                    mime: Some("audio/wav".into()),
                },
                recognition: AdvancedRecognition::Request {
                    request: Box::new(HttpStage {
                        method: HttpMethod::Post,
                        url: format!("http://{address}/recognize"),
                        query: BTreeMap::new(),
                        headers: BTreeMap::new(),
                        body: HttpBody::Json {
                            value: serde_json::json!({"audio": "{{audio:public_url}}"}),
                        },
                        accepted_statuses: vec![200],
                        signer: SignerConfig::None,
                        captures: vec![],
                    }),
                    final_text: ResponseExtractor::JsonPath {
                        path: "$.text".into(),
                    },
                },
            }),
            values: BTreeMap::new(),
            secrets: BTreeMap::new(),
            remote_audio: RemoteAudioConfig::Webdav(WebDavRemoteAudioConfig {
                upload_base_url: format!("http://{address}/private-dav"),
                username: "upload-user".into(),
                password: "upload-password".into(),
                remote_path_prefix: "dictate/2026".into(),
                public_download_base_url: "https://downloads.example.test/recognition".into(),
                delete_after_recognition,
            }),
        },
        ..Default::default()
    }
}

fn config_for_s3_cloud_uri(address: std::net::SocketAddr) -> Config {
    Config {
        max_retry: 1,
        retry_base_delay: 0.0,
        advanced_audio_api: AdvancedAudioConfig {
            enabled: true,
            workflow: Some(AdvancedAudioWorkflow {
                schema_version: WorkflowSchemaVersion::default(),
                name: "S3 cloud URI rendering test".into(),
                parameters: vec![],
                secrets: vec![],
                audio: AudioSpec {
                    delivery: AudioDelivery::CloudUri,
                    mime: Some("audio/wav".into()),
                },
                recognition: AdvancedRecognition::Request {
                    request: Box::new(HttpStage {
                        method: HttpMethod::Post,
                        url: format!("http://{address}/recognize"),
                        query: BTreeMap::new(),
                        headers: BTreeMap::new(),
                        body: HttpBody::Json {
                            value: serde_json::json!({"audio": "{{audio:cloud_uri}}"}),
                        },
                        accepted_statuses: vec![200],
                        signer: SignerConfig::None,
                        captures: vec![],
                    }),
                    final_text: ResponseExtractor::JsonPath {
                        path: "$.text".into(),
                    },
                },
            }),
            values: BTreeMap::new(),
            secrets: BTreeMap::new(),
            remote_audio: RemoteAudioConfig::S3Compatible(S3RemoteAudioConfig {
                endpoint: format!("http://{address}/private-s3"),
                region: "us-east-1".into(),
                bucket: "dictate-audio".into(),
                access_key: "s3-access-key".into(),
                secret_key: "s3-secret-key".into(),
                prefix: "dictate/2026".into(),
                public_url_base: None,
                presigned: false,
                delete_after_recognition: true,
            }),
        },
        ..Default::default()
    }
}

fn temporary_audio() -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private meeting with Alice.wav");
    std::fs::write(&path, b"recording bytes").unwrap();
    (directory, path)
}

fn assert_webdav_lifecycle(requests: &[CapturedRequest]) {
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[2].method, "DELETE");
    assert_eq!(requests[0].target, requests[2].target);
    assert!(requests[0].target.starts_with("/private-dav/dictate/2026/"));
    assert!(requests[0].target.ends_with(".wav"));
    let remote_name = requests[0].target.rsplit('/').next().unwrap();
    assert!(!remote_name.contains("private"));
    assert!(!remote_name.contains("Alice"));
    assert!(
        requests[0]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: basic ")
    );
    assert!(
        requests[2]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: basic ")
    );
    assert_eq!(requests[0].body, b"recording bytes");
    let recognition_body = String::from_utf8_lossy(&requests[1].body);
    assert!(recognition_body.contains("https://downloads.example.test/recognition/dictate/2026/"));
    assert!(!recognition_body.contains("private meeting with Alice"));
}

#[tokio::test]
async fn webdav_cleanup_waits_for_successful_workflow_completion() {
    let (address, server) = start_webdav_workflow_server(RecognitionResponse::Success, None).await;
    let (_directory, audio) = temporary_audio();
    let client = AdvancedAudioClient::new(config_for_webdav(address, true)).unwrap();

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();

    assert_eq!(transcription.text, "complete");
    assert_webdav_lifecycle(&server.await.unwrap());
}

#[tokio::test]
async fn s3_cloud_uri_is_rendered_into_recognition_request_after_upload() {
    let (address, server) = start_webdav_workflow_server(RecognitionResponse::Success, None).await;
    let (_directory, audio) = temporary_audio();
    let client = AdvancedAudioClient::new(config_for_s3_cloud_uri(address)).unwrap();

    let transcription = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap();

    assert_eq!(transcription.text, "complete");
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[2].method, "DELETE");
    assert_eq!(requests[0].target, requests[2].target);

    let object_key = requests[0]
        .target
        .strip_prefix("/private-s3/dictate-audio/")
        .unwrap();
    assert!(object_key.starts_with("dictate/2026/"));
    assert!(object_key.ends_with(".wav"));
    let recognition_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let expected_cloud_uri = format!("s3://dictate-audio/{object_key}");
    assert_eq!(
        recognition_body
            .get("audio")
            .and_then(serde_json::Value::as_str),
        Some(expected_cloud_uri.as_str())
    );
}

#[tokio::test]
async fn webdav_cleanup_runs_after_failed_workflow() {
    let (address, server) = start_webdav_workflow_server(RecognitionResponse::Failure, None).await;
    let (_directory, audio) = temporary_audio();
    // Retention applies only after successful recognition. A failed workflow
    // must clean up a recording that was already uploaded.
    let client = AdvancedAudioClient::new(config_for_webdav(address, false)).unwrap();

    let error = client
        .transcribe(&CancellationToken::new(), &audio)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        AdvancedAudioError::UnexpectedStatus { status: 500, .. }
    ));
    assert_webdav_lifecycle(&server.await.unwrap());
}

#[tokio::test]
async fn webdav_cleanup_runs_after_workflow_cancellation() {
    let (post_started, post_received) = oneshot::channel();
    let (address, mut server) =
        start_webdav_workflow_server(RecognitionResponse::WaitForCancellation, Some(post_started))
            .await;
    let (_directory, audio) = temporary_audio();
    // Cancellation after upload also forces cleanup despite the normal
    // success-retention preference.
    let client = AdvancedAudioClient::new(config_for_webdav(address, false)).unwrap();
    let cancellation = CancellationToken::new();
    let request_cancellation = cancellation.clone();
    let transcription =
        tokio::spawn(async move { client.transcribe(&request_cancellation, &audio).await });

    post_received.await.unwrap();
    cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), transcription)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();

    assert!(matches!(error, AdvancedAudioError::Canceled));
    let requests = tokio::time::timeout(Duration::from_secs(2), &mut server)
        .await
        .unwrap()
        .unwrap();
    assert_webdav_lifecycle(&requests);
}
