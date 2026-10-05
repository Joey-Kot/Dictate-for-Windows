use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use stt_core::Config;
use stt_core::asr::AsrClient;
use stt_core::debug_log::{self, Category, Record};
use stt_core::rewrite::{RewriteClient, RewritePrompt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        request.extend_from_slice(&buffer[..count]);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse::<usize>().ok());
            if length.is_some_and(|length| request.len() >= end + 4 + length)
                || request.ends_with(b"0\r\n\r\n")
            {
                return request;
            }
        }
    }
}

async fn server(responses: Vec<(u16, &'static str)>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = tokio::spawn(async move {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (endpoint, worker)
}

async fn basic_auth_error_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://basic%2buser%22name:basic+pass%2Fword%5Cvalue@{}",
        listener.local_addr().unwrap()
    );
    let worker = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        let end = request
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&request[..end]);
        let authorization = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim().strip_prefix("Basic ").unwrap())
            .expect("request must contain Basic authentication");
        let credentials = String::from_utf8(STANDARD.decode(authorization).unwrap()).unwrap();
        let (username, password) = credentials.split_once(':').unwrap();
        assert_eq!(username, "basic+user\"name");
        assert_eq!(password, "basic+pass/word\\value");
        let body = serde_json::json!({
            "error": { "code": "basic-auth-echo", "username": username, "password": password }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (endpoint, worker)
}

// One integration test owns the process-wide subscriber for both switch states.
#[tokio::test]
async fn debug_switches_gate_requests_and_native_logs_and_credentials_are_redacted() {
    tokio::time::timeout(std::time::Duration::from_secs(20), exercise())
        .await
        .unwrap();
}

async fn exercise() {
    let records: Arc<Mutex<Vec<Record>>> = Arc::default();
    let output = records.clone();
    let _subscription = debug_log::subscribe(move |record| output.lock().push(record));
    let directory = tempfile::tempdir().unwrap();
    let audio = directory.path().join("input.wav");
    std::fs::write(&audio, b"audio").unwrap();
    let cancel = CancellationToken::new();
    for enabled in [false, true] {
        records.lock().clear();
        let mut config = Config {
            upload_debug: enabled,
            token: "audio-secret".into(),
            max_retry: 2,
            retry_base_delay: 0.0,
            ..Default::default()
        };
        config.rewrite.api_key = "rewrite-secret".into();
        config.rewrite.model = "test".into();

        let (endpoint, task) = server(vec![
            (503, r#"{"error":"audio-secret query-secret"}"#),
            (200, r#"{"text":"private audio output"}"#),
        ])
        .await;
        config.api_endpoint = format!("{endpoint}?token=query-secret");
        assert_eq!(
            AsrClient::new(config.clone())
                .unwrap()
                .transcribe(&cancel, &audio)
                .await
                .unwrap()
                .text,
            "private audio output"
        );
        task.await.unwrap();

        let (endpoint, task) = server(vec![(401, r#"{"error":"audio-secret"}"#)]).await;
        config.api_endpoint = endpoint;
        assert!(
            AsrClient::new(config.clone())
                .unwrap()
                .test_connection_cancellable(&cancel, &audio)
                .await
                .is_err()
        );
        task.await.unwrap();

        // A transport error includes the actual URL with its original escape
        // spelling. Close a real connection without a response to exercise the
        // same reqwest error and subscriber path used after network failures.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        config.api_endpoint = format!(
            "http://{}/?api_key=secret%2fvalue&encoded=%73%65cret-token&space=secret+with%20space&mixed=mix%2fed%2Fsecret",
            listener.local_addr().unwrap()
        );
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
        });
        let error = AsrClient::new(config.clone())
            .unwrap()
            .test_connection_cancellable(&cancel, &audio)
            .await
            .unwrap_err();
        task.await.unwrap();
        assert!(error.to_string().contains("secret%2fvalue"));

        // Both audio request paths must actually send decoded userinfo in Basic
        // Auth and redact the credentials echoed by the server's JSON error.
        for connectivity in [false, true] {
            let (endpoint, task) = basic_auth_error_server().await;
            let mut basic_config = config.clone();
            basic_config.max_retry = 1;
            basic_config.token.clear();
            basic_config.api_endpoint = endpoint;
            let client = AsrClient::new(basic_config).unwrap();
            let error = if connectivity {
                client
                    .test_connection_cancellable(&cancel, &audio)
                    .await
                    .unwrap_err()
                    .to_string()
            } else {
                let error = client.transcribe(&cancel, &audio).await.unwrap_err();
                String::from_utf8_lossy(error.last_response()).into_owned()
            };
            task.await.unwrap();
            assert!(
                error.contains("basic-auth-echo"),
                "error response was not consumed: {error}"
            );
        }

        let (endpoint, task) = server(vec![
            (429, r#"{"error":"rewrite-secret"}"#),
            (
                200,
                r#"{"choices":[{"message":{"content":"private rewrite output"}}]}"#,
            ),
        ])
        .await;
        config.rewrite.base_url = endpoint;
        let prompt = RewritePrompt {
            prompt: "private prompt".into(),
            ..Default::default()
        };
        assert_eq!(
            RewriteClient::new(config.clone())
                .unwrap()
                .execute(&prompt, "private input", &cancel, true)
                .await
                .unwrap(),
            "private rewrite output"
        );
        task.await.unwrap();

        let (endpoint, task) =
            server(vec![(200, r#"{"choices":[{"message":{"content":"OK"}}]}"#)]).await;
        config.rewrite.base_url = endpoint;
        RewriteClient::new(config)
            .unwrap()
            .test_connection(&cancel)
            .await
            .unwrap();
        task.await.unwrap();
        let captured = records.lock();
        if enabled {
            assert!(
                captured
                    .iter()
                    .all(|record| record.category == Category::Upload)
            );
            let text = captured
                .iter()
                .map(|record| record.message.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            for expected in [
                "Audio attempt 2/2",
                "HTTP 503",
                "HTTP 429",
                "Rewrite retry",
                "Audio connectivity",
                "Audio request failed",
                "Rewrite connectivity",
                "elapsed=",
                "[redacted]",
                "basic-auth-echo",
            ] {
                assert!(text.contains(expected), "missing {expected}: {text}");
            }
            for private in [
                "audio-secret",
                "rewrite-secret",
                "query-secret",
                "secret%2fvalue",
                "secret/value",
                "%73%65cret-token",
                "secret-token",
                "secret+with%20space",
                "secret with space",
                "mix%2fed%2Fsecret",
                "mix/ed/secret",
                "basic+user\"name",
                "basic+user\\\"name",
                "basic%2buser%22name",
                "basic+pass/word\\value",
                "basic+pass/word\\\\value",
                "basic+pass%2Fword%5Cvalue",
                "private prompt",
                "private input",
                "private audio output",
                "private rewrite output",
            ] {
                assert!(!text.contains(private), "leaked {private}: {text}");
            }
        } else {
            assert!(captured.is_empty());
        }
    }
    #[cfg(feature = "static-libav")]
    native_logs(&records, directory.path()).await;
}

#[cfg(feature = "static-libav")]
async fn native_logs(records: &Mutex<Vec<Record>>, directory: &std::path::Path) {
    use stt_core::converter::AudioConverter;
    use stt_core::embedded_ffmpeg::EmbeddedFfmpegConverter;
    unsafe extern "C" {
        fn av_log(context: *mut std::ffi::c_void, level: i32, format: *const std::ffi::c_char, ...);
    }
    let input = directory.join("source.wav");
    let output = directory.join("converted.wav");
    let mut wav = hound::WavWriter::create(
        &input,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for _ in 0..1600 {
        wav.write_sample(1_i16).unwrap();
    }
    wav.finalize().unwrap();
    for enabled in [false, true] {
        records.lock().clear();
        let config = Config {
            ffmpeg_debug: enabled,
            enable_vad: false,
            codecs: "pcm_s16le".into(),
            sampling_rate: 16000,
            sampling_rate_depth: 16,
            channels: 1,
            container: "wav".into(),
            ..Default::default()
        };
        EmbeddedFfmpegConverter
            .convert(&CancellationToken::new(), &config, &input, &output, 16000)
            .await
            .unwrap();
        unsafe {
            av_log(
                std::ptr::null_mut(),
                16,
                c"native-debug-callback: %s\n".as_ptr(),
                c"sample".as_ptr(),
            );
        }
        let captured = records.lock();
        assert_eq!(
            captured
                .iter()
                .any(|record| record.category == Category::Ffmpeg
                    && record.message.contains("native-debug-callback: sample")),
            enabled
        );
        if !enabled {
            assert!(captured.is_empty());
        }
    }
}
