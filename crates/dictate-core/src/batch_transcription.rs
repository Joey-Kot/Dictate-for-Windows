//! Bounded file batches with a single recognition budget shared by every file/segment.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::audio_api::UploadLimiter;
use crate::cache;
use crate::converter::{AudioConverter, ConvertError};
use crate::file_transcription::{FileTranscriber, OutputPolicy};
use crate::runtime::RuntimeError;

#[derive(Debug, Clone)]
pub struct BatchJob {
    pub id: u64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    Pending,
    Processing,
    /// None means local analysis found no speech; successful empty ASR still writes a file.
    Completed {
        output: Option<PathBuf>,
    },
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct BatchUpdate {
    pub id: u64,
    pub status: FileStatus,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    pub completed: usize,
    pub failed: usize,
    pub canceled: bool,
}

/// GUI removals skip undispatched files and cancel only the selected active files.
#[derive(Clone, Default)]
pub struct BatchControl(Arc<parking_lot::Mutex<ControlledFiles>>);

#[derive(Default)]
struct ControlledFiles {
    removed: HashSet<u64>,
    active: HashMap<u64, CancellationToken>,
}

impl BatchControl {
    pub fn remove_files(&self, ids: &[u64]) {
        let mut files = self.0.lock();
        for &id in ids {
            files.removed.insert(id);
            if let Some(token) = files.active.get(&id) {
                token.cancel();
            }
        }
    }

    fn begin(&self, id: u64, parent: &CancellationToken) -> Option<CancellationToken> {
        let mut files = self.0.lock();
        if parent.is_cancelled() || files.removed.contains(&id) {
            return None;
        }
        let token = parent.child_token();
        files.active.insert(id, token.clone());
        Some(token)
    }

    fn finish(&self, id: u64) -> bool {
        let mut files = self.0.lock();
        files.active.remove(&id);
        files.removed.contains(&id)
    }
}

#[cfg(test)]
pub(crate) async fn run<F>(
    config: Config,
    converter: Arc<dyn AudioConverter>,
    jobs: Vec<BatchJob>,
    cancellation: CancellationToken,
    update: F,
) -> Result<BatchSummary, RuntimeError>
where
    F: Fn(BatchUpdate) + Send + Sync,
{
    run_controlled(
        config,
        converter,
        jobs,
        cancellation,
        BatchControl::default(),
        update,
    )
    .await
}

pub(crate) async fn run_controlled<F>(
    mut config: Config,
    converter: Arc<dyn AudioConverter>,
    jobs: Vec<BatchJob>,
    cancellation: CancellationToken,
    control: BatchControl,
    update: F,
) -> Result<BatchSummary, RuntimeError>
where
    F: Fn(BatchUpdate) + Send + Sync,
{
    if cancellation.is_cancelled() {
        return Ok(BatchSummary {
            canceled: true,
            ..BatchSummary::default()
        });
    }
    config.validate()?;
    let output_root = PathBuf::from(config.batch_transcription.output_dir.trim());
    if output_root.as_os_str().is_empty() {
        return Err(
            crate::config::ConfigError::Invalid("Output directory is required".into()).into(),
        );
    }
    std::fs::create_dir_all(&output_root).map_err(|source| RuntimeError::OutputFile {
        path: output_root.clone(),
        source,
    })?;
    // Fail before recognition if the destination cannot accept a complete output file.
    tempfile::NamedTempFile::new_in(&output_root).map_err(|source| RuntimeError::OutputFile {
        path: output_root.clone(),
        source,
    })?;
    let scratch = cache::initialize_cache_dir(&mut config);
    let file_limit = (config.batch_transcription.batch_size as usize).min(jobs.len());
    let limiter = UploadLimiter::new(config.max_upload_concurrency);
    let transcriber = FileTranscriber::new(config.clone(), converter, scratch, Some(limiter))?;
    let mut next = 0;
    let mut running = FuturesUnordered::new();
    let mut summary = BatchSummary::default();
    loop {
        while !cancellation.is_cancelled() && running.len() < file_limit && next < jobs.len() {
            let job = jobs[next].clone();
            next += 1;
            let Some(token) = control.begin(job.id, &cancellation) else {
                continue;
            };
            update(BatchUpdate {
                id: job.id,
                status: FileStatus::Processing,
            });
            let mut name = job.path.file_stem().unwrap_or_default().to_os_string();
            name.push(".txt");
            let output = output_root.join(name);
            let transcriber = &transcriber;
            running.push(async move {
                let result = transcriber
                    .transcribe(&job.path, &output, &token, OutputPolicy::Unique)
                    .await;
                (job.id, result)
            });
        }
        let Some((id, result)) = running.next().await else {
            break;
        };
        let removed = control.finish(id);
        let status = match result {
            Ok(output) => {
                summary.completed += 1;
                FileStatus::Completed {
                    output: Some(output),
                }
            }
            Err(RuntimeError::Convert(ConvertError::NoSpeech))
                if !cancellation.is_cancelled() && !removed =>
            {
                summary.completed += 1;
                FileStatus::Completed { output: None }
            }
            Err(error) if removed || cancellation.is_cancelled() || is_canceled(&error) => {
                FileStatus::Pending
            }
            Err(error) => {
                summary.failed += 1;
                FileStatus::Failed(
                    crate::debug_log::request_message(&error.to_string(), &config)
                        .chars()
                        .take(2000)
                        .collect(),
                )
            }
        };
        update(BatchUpdate { id, status });
    }
    // Every started future has been drained, including remote cleanup. Undispatched rows stay pending.
    summary.canceled = cancellation.is_cancelled();
    Ok(summary)
}

fn is_canceled(error: &RuntimeError) -> bool {
    matches!(error, RuntimeError::Convert(ConvertError::Canceled))
        || matches!(error, RuntimeError::AudioApi(error) if error.is_canceled())
}

#[derive(Debug, Default)]
pub struct CollectedFiles {
    pub paths: Vec<PathBuf>,
    pub errors: Vec<String>,
}

pub const AUDIO_FILE_EXTENSIONS: &[&str] = &[
    "wav", "wave", "mp3", "mp2", "mp1", "flac", "ogg", "oga", "opus", "spx", "aac", "m4a", "mp4",
    "amr", "awb", "wma", "asf", "aif", "aiff", "aifc", "afc", "wv", "ac3", "eac3", "mka", "mkv",
    "webm", "mov", "avi", "flv", "mpg", "mpeg",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            AUDIO_FILE_EXTENSIONS
                .iter()
                .any(|extension| value.eq_ignore_ascii_case(extension))
        })
}

/// Recursion stays off directory links/reparse points and preserves deterministic import order.
pub fn collect_files(roots: &[PathBuf], cancellation: &CancellationToken) -> CollectedFiles {
    let mut result = CollectedFiles::default();
    let mut seen = HashSet::new();
    let mut stack: Vec<_> = roots.iter().rev().cloned().collect();
    while let Some(path) = stack.pop() {
        if cancellation.is_cancelled() {
            break;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) => {
                result.errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if metadata.is_symlink() || (metadata.is_dir() && is_reparse_point(&metadata)) {
            continue;
        }
        if metadata.is_dir() {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    let mut children = Vec::new();
                    for entry in entries {
                        match entry {
                            Ok(entry) => children.push(entry.path()),
                            Err(error) => {
                                result.errors.push(format!("{}: {error}", path.display()))
                            }
                        }
                    }
                    children.sort();
                    stack.extend(children.into_iter().rev());
                }
                Err(error) => result.errors.push(format!("{}: {error}", path.display())),
            }
        } else if metadata.is_file() && is_audio_file(&path) {
            match std::fs::canonicalize(&path) {
                Ok(path) if seen.insert(path_key(&path)) => result.paths.push(path),
                Ok(_) => {}
                Err(error) => result.errors.push(format!("{}: {error}", path.display())),
            }
        }
    }
    result
}

pub fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::{SegmentAnalysis, SegmentExportRequest};
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    struct TestConverter;

    #[async_trait]
    impl AudioConverter for TestConverter {
        async fn convert(
            &self,
            cancellation: &CancellationToken,
            _: &Config,
            input: &Path,
            output: &Path,
            _: i32,
        ) -> Result<(), ConvertError> {
            let bytes = std::fs::read(input).unwrap();
            if bytes == b"silence" {
                return Err(ConvertError::NoSpeech);
            }
            tokio::select! { biased;
                _ = cancellation.cancelled() => return Err(ConvertError::Canceled),
                _ = tokio::time::sleep(Duration::from_millis(2)) => {},
            }
            std::fs::write(output, bytes).unwrap();
            Ok(())
        }

        async fn analyze_segments(
            &self,
            _: &CancellationToken,
            _: &Config,
            _: &Path,
            _: u32,
        ) -> Result<SegmentAnalysis, ConvertError> {
            Ok(SegmentAnalysis {
                source_rate: 1000,
                source_frames: 3000,
                silence_intervals: vec![],
            })
        }

        async fn export_segments(
            &self,
            _: &CancellationToken,
            request: SegmentExportRequest<'_>,
        ) -> Result<(), ConvertError> {
            for output in request.outputs {
                std::fs::copy(request.input, output).unwrap();
            }
            Ok(())
        }
    }

    async fn request_body(stream: &mut TcpStream) -> Vec<u8> {
        let mut wire = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            wire.extend_from_slice(&buffer[..count]);
            if let Some(end) = wire.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&wire[..end]);
                let size = headers.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                });
                if let Some(size) = size {
                    if wire.len() >= end + 4 + size {
                        return wire[end + 4..end + 4 + size].to_vec();
                    }
                } else if headers
                    .to_ascii_lowercase()
                    .contains("transfer-encoding: chunked")
                {
                    if let Some(body) = decode_chunks(&wire[end + 4..]) {
                        return body;
                    }
                } else {
                    return Vec::new();
                }
            }
        }
    }

    fn decode_chunks(wire: &[u8]) -> Option<Vec<u8>> {
        let mut cursor = 0;
        let mut body = Vec::new();
        loop {
            let end = cursor
                + wire
                    .get(cursor..)?
                    .windows(2)
                    .position(|bytes| bytes == b"\r\n")?;
            let length = usize::from_str_radix(
                std::str::from_utf8(&wire[cursor..end])
                    .ok()?
                    .split(';')
                    .next()?,
                16,
            )
            .ok()?;
            cursor = end + 2;
            if length == 0 {
                return (wire.get(cursor..cursor + 2)? == b"\r\n").then_some(body);
            }
            body.extend_from_slice(wire.get(cursor..cursor + length)?);
            cursor += length + 2;
            if cursor > wire.len() {
                return None;
            }
        }
    }

    struct Server {
        endpoint: String,
        requests: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn server(delay: Duration) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/recognize", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        let maximum = peak.clone();
        let task = tokio::spawn(async move {
            let active = Arc::new(AtomicUsize::new(0));
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let active = active.clone();
                let maximum = maximum.clone();
                let count = request_count.clone();
                handlers.spawn(async move {
                    let body = request_body(&mut stream).await;
                    count.fetch_add(1, Ordering::SeqCst);
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    let body = String::from_utf8_lossy(&body);
                    let (status, text) = if body.contains("FAIL") { ("503 Service Unavailable", r#"{"error":"unavailable"}"#) } else if body.contains("EMPTY") { ("200 OK", r#"{"text":""}"#) } else { ("200 OK", r#"{"text":"recognized"}"#) };
                    active.fetch_sub(1, Ordering::SeqCst);
                    let _ = stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes()).await;
                });
                while handlers.try_join_next().is_some() {}
            }
        });
        Server {
            endpoint,
            requests,
            peak,
            task,
        }
    }

    fn config(root: &Path, endpoint: &str) -> Config {
        Config {
            api_endpoint: endpoint.into(),
            cache_dir: root.join("scratch").to_string_lossy().into_owned(),
            max_retry: 1,
            retry_base_delay: 0.0,
            hotkey_debug: false,
            batch_transcription: crate::config::BatchTranscriptionConfig {
                output_dir: root.join("out").to_string_lossy().into_owned(),
                batch_size: 5,
            },
            ..Config::default()
        }
    }

    fn jobs(root: &Path, count: usize) -> Vec<BatchJob> {
        (0..count)
            .map(|index| {
                let path = root.join(format!("file-{index}.wav"));
                std::fs::write(&path, "audio").unwrap();
                BatchJob {
                    id: index as u64,
                    path,
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn network_limit_is_shared_across_files_and_segments_for_legacy_and_advanced() {
        use crate::advanced_audio::schema::{AudioSpec, ResponseExtractor, SignerConfig};
        use crate::advanced_audio::{
            AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery,
            HttpBody, HttpMethod, HttpStage,
        };
        for advanced in [false, true] {
            for (files, uploads, segmented) in [(5, 2, false), (5, 2, true), (1, 3, true)] {
                let root = tempfile::tempdir().unwrap();
                let server = server(Duration::from_millis(15)).await;
                let mut config = config(root.path(), &server.endpoint);
                config.batch_transcription.batch_size = files;
                config.max_upload_concurrency = uploads;
                config.enable_segmented_upload = segmented;
                config.max_upload_segment_seconds = 1;
                if advanced {
                    config.advanced_audio_api = AdvancedAudioConfig {
                        enabled: true,
                        workflow: Some(AdvancedAudioWorkflow {
                            schema_version: Default::default(),
                            name: "batch test".into(),
                            parameters: vec![],
                            secrets: vec![],
                            audio: AudioSpec {
                                delivery: AudioDelivery::RawAudio,
                                mime: Some("audio/wav".into()),
                            },
                            recognition: AdvancedRecognition::Request {
                                request: Box::new(HttpStage {
                                    method: HttpMethod::Post,
                                    url: server.endpoint.clone(),
                                    query: Default::default(),
                                    headers: Default::default(),
                                    body: HttpBody::RawAudio,
                                    accepted_statuses: vec![200],
                                    signer: SignerConfig::None,
                                    captures: vec![],
                                }),
                                final_text: ResponseExtractor::JsonPath {
                                    path: "$.text".into(),
                                },
                            },
                        }),
                        ..Default::default()
                    };
                }
                let jobs = jobs(root.path(), 3);
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    run(
                        config,
                        Arc::new(TestConverter),
                        jobs.clone(),
                        CancellationToken::new(),
                        |_| {},
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(result.completed, 3);
                assert_eq!(result.failed, 0);
                assert_eq!(
                    server.requests.load(Ordering::SeqCst),
                    if segmented { 9 } else { 3 }
                );
                assert_eq!(
                    server.peak.load(Ordering::SeqCst),
                    uploads as usize,
                    "advanced={advanced}, segmented={segmented}, files={files}"
                );
                assert!(jobs.iter().all(|job| job.path.exists()));
                assert_eq!(
                    std::fs::read_dir(root.path().join("scratch"))
                        .unwrap()
                        .count(),
                    0
                );
            }
        }
    }

    #[tokio::test]
    async fn failures_do_not_stop_other_files_empty_and_silence_succeed_and_names_do_not_collide() {
        let root = tempfile::tempdir().unwrap();
        let server = server(Duration::from_millis(5)).await;
        let mut config = config(root.path(), &server.endpoint);
        config.max_retry = 2;
        config.max_upload_concurrency = 2;
        let mut jobs = jobs(root.path(), 5);
        std::fs::write(&jobs[1].path, "FAIL").unwrap();
        std::fs::write(&jobs[2].path, "EMPTY").unwrap();
        std::fs::write(&jobs[3].path, "silence").unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        jobs[4].path = sub.join("file-0.mp3");
        std::fs::write(&jobs[4].path, "audio").unwrap();
        let output = root.path().join("out");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("file-0.txt"), "existing").unwrap();
        let updates = Mutex::new(Vec::new());
        let summary = run(
            config,
            Arc::new(TestConverter),
            jobs.clone(),
            CancellationToken::new(),
            |update| updates.lock().push(update),
        )
        .await
        .unwrap();
        assert_eq!(summary.completed, 4);
        assert_eq!(summary.failed, 1);
        assert_eq!(server.requests.load(Ordering::SeqCst), 5);
        assert!(server.peak.load(Ordering::SeqCst) <= 2);
        assert_eq!(
            std::fs::read_to_string(output.join("file-0.txt")).unwrap(),
            "existing"
        );
        assert_eq!(
            std::fs::read_to_string(output.join("file-0 (2).txt")).unwrap(),
            "recognized"
        );
        assert_eq!(
            std::fs::read_to_string(output.join("file-0 (3).txt")).unwrap(),
            "recognized"
        );
        assert_eq!(
            std::fs::read_to_string(output.join("file-2.txt")).unwrap(),
            ""
        );
        assert!(!output.join("file-1.txt").exists());
        assert!(!output.join("file-3.txt").exists());
        assert!(updates.lock().iter().any(
            |update| update.id == 3 && update.status == FileStatus::Completed { output: None }
        ));
        assert!(jobs.iter().all(|job| job.path.exists()));
    }

    #[tokio::test]
    async fn missing_output_ancestors_are_created_when_a_batch_starts() {
        let root = tempfile::tempdir().unwrap();
        let server = server(Duration::from_millis(1)).await;
        let mut config = config(root.path(), &server.endpoint);
        let output = root.path().join("Documents").join("Transcripts");
        config.batch_transcription.output_dir = output.to_string_lossy().into_owned();
        let inputs = jobs(root.path(), 1);
        assert!(!root.path().join("Documents").exists());

        let result = run(
            config,
            Arc::new(TestConverter),
            inputs,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        assert_eq!(result.completed, 1);
        assert_eq!(result.failed, 0);
        assert!(output.is_dir());
        assert_eq!(std::fs::read_dir(output).unwrap().count(), 1);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn destination_validation_and_pre_cancellation_never_send_recognition() {
        let root = tempfile::tempdir().unwrap();
        let server = server(Duration::from_millis(1)).await;
        let config = config(root.path(), &server.endpoint);
        let inputs = jobs(root.path(), 1);
        std::fs::write(root.path().join("out"), "existing file").unwrap();
        let updates = Mutex::new(Vec::new());
        assert!(
            run(
                config.clone(),
                Arc::new(TestConverter),
                inputs.clone(),
                CancellationToken::new(),
                |update| updates.lock().push(update)
            )
            .await
            .is_err()
        );
        assert!(updates.lock().is_empty());
        let token = CancellationToken::new();
        token.cancel();
        let result = run(
            config,
            Arc::new(TestConverter),
            inputs.clone(),
            token,
            |_| panic!("canceled before dispatch"),
        )
        .await
        .unwrap();
        assert!(result.canceled);
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
        assert!(!root.path().join("scratch").exists());
        assert!(inputs[0].path.exists());
    }

    #[tokio::test]
    async fn ui_cancellation_during_a_ready_preparation_wins_over_no_speech_success() {
        struct CancelInPreparation(CancellationToken);
        #[async_trait]
        impl AudioConverter for CancelInPreparation {
            async fn convert(
                &self,
                _: &CancellationToken,
                _: &Config,
                _: &Path,
                _: &Path,
                _: i32,
            ) -> Result<(), ConvertError> {
                // No await between Cancel and the local result: an outer select alone is too late.
                self.0.cancel();
                Err(ConvertError::NoSpeech)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let config = config(root.path(), "http://127.0.0.1:9");
        let token = CancellationToken::new();
        let runtime = crate::runtime::Runtime::new(
            config.clone(),
            Arc::new(CancelInPreparation(token.clone())),
        )
        .unwrap();
        let inputs = jobs(root.path(), 1);
        let updates = Mutex::new(Vec::new());
        let result = runtime
            .transcribe_batch(config, inputs, token, |update| updates.lock().push(update))
            .await
            .unwrap();
        assert!(result.canceled);
        assert_eq!(result.completed, 0);
        assert_eq!(result.failed, 0);
        assert_eq!(updates.lock().last().unwrap().status, FileStatus::Pending);
        assert!(runtime.can_reload());
    }

    #[tokio::test]
    async fn runtime_cancel_drains_waiters_preserves_sources_and_releases_the_task_lock() {
        let root = tempfile::tempdir().unwrap();
        let server = server(Duration::from_secs(30)).await;
        let config = config(root.path(), &server.endpoint);
        let runtime =
            crate::runtime::Runtime::new(config.clone(), Arc::new(TestConverter)).unwrap();
        let jobs = jobs(root.path(), 10);
        let source = root.path().join("scratch/RecordTemp_caller.wav");
        std::fs::write(&source, "audio").unwrap();
        let mut jobs = jobs;
        jobs[0].path = source.clone();
        let running = runtime.clone();
        let inputs = jobs.clone();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observed = updates.clone();
        let task = tokio::spawn(async move {
            running
                .transcribe_batch(config, inputs, CancellationToken::new(), |update| {
                    observed.lock().push(update)
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while server.requests.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!runtime.try_toggle_recording());
        assert!(!runtime.try_rewrite("any"));
        assert!(runtime.try_cancel_or_retry());
        let summary = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(summary.canceled);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.completed, 0);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            updates
                .lock()
                .iter()
                .filter(|update| update.status == FileStatus::Processing)
                .count(),
            5
        );
        assert_eq!(
            updates
                .lock()
                .iter()
                .filter(|update| update.status == FileStatus::Pending)
                .count(),
            5
        );
        assert!(jobs.iter().all(|job| job.path.exists()));
        assert_eq!(
            std::fs::read_dir(root.path().join("scratch"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_dir(root.path().join("out")).unwrap().count(),
            0
        );
        assert!(runtime.can_reload());
        runtime
            .test_connection(false, CancellationToken::new(), |_| async { Ok(()) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn removing_active_waiting_and_queued_files_keeps_other_files_running() {
        let root = tempfile::tempdir().unwrap();
        let server = server(Duration::from_millis(100)).await;
        let mut config = config(root.path(), &server.endpoint);
        config.batch_transcription.batch_size = 2;
        let runtime =
            crate::runtime::Runtime::new(config.clone(), Arc::new(TestConverter)).unwrap();
        let inputs = jobs(root.path(), 5);
        let control = BatchControl::default();
        let worker_control = control.clone();
        let updates = Arc::new(Mutex::new(Vec::new()));
        let observed = updates.clone();
        let worker = runtime.clone();
        let jobs = inputs.clone();
        let task = tokio::spawn(async move {
            worker
                .transcribe_batch_controlled(
                    config,
                    jobs,
                    CancellationToken::new(),
                    worker_control,
                    |update| observed.lock().push(update),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while server.requests.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        control.remove_files(&[inputs[0].id, inputs[1].id, inputs[3].id]);
        let summary = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(summary.completed, 2);
        assert_eq!(summary.failed, 0);
        assert!(!summary.canceled);
        assert_eq!(server.requests.load(Ordering::SeqCst), 3);
        let updates = updates.lock();
        assert!(!updates.iter().any(|update| update.id == inputs[3].id));
        for id in [0, 1] {
            assert!(
                updates.iter().any(
                    |update| update.id == inputs[id].id && update.status == FileStatus::Pending
                )
            );
        }
        for id in [0, 1, 3] {
            assert!(!root.path().join(format!("out/file-{id}.txt")).exists());
        }
        for id in [2, 4] {
            assert_eq!(
                std::fs::read_to_string(root.path().join(format!("out/file-{id}.txt"))).unwrap(),
                "recognized"
            );
        }
        assert!(inputs.iter().all(|input| input.path.exists()));
        assert_eq!(
            std::fs::read_dir(root.path().join("scratch"))
                .unwrap()
                .count(),
            0
        );
        assert!(control.0.lock().active.is_empty());
        assert!(runtime.can_reload());
    }

    #[test]
    fn recursive_collection_deduplicates_overlapping_roots_and_filters_non_audio() {
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(root.path().join("a.WAV"), "a").unwrap();
        std::fs::write(sub.join("b.mp3"), "b").unwrap();
        std::fs::write(sub.join("ignore.txt"), "text").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path(), sub.join("loop")).unwrap();
        let result = collect_files(&[root.path().to_owned(), sub], &CancellationToken::new());
        assert!(result.errors.is_empty());
        assert_eq!(result.paths.len(), 2);
        let token = CancellationToken::new();
        token.cancel();
        assert!(
            collect_files(&[root.path().to_owned()], &token)
                .paths
                .is_empty()
        );
    }
}
