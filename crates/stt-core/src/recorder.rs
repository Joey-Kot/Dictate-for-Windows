use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::Config;
use crate::audio_devices::{CaptureFormat, SystemAudioBackend};
use crate::capture_wav::CaptureWav;

const MAX_CONSECUTIVE_READ_ERRORS: usize = 10;
const READ_ERROR_RETRY_DELAY: Duration = Duration::from_millis(10);
const PAUSED_POLL_DELAY: Duration = Duration::from_millis(100);
const SUCCESSFUL_WRITE_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderState {
    Idle,
    Recording,
    Paused,
    Stopping,
    Canceled,
}

#[derive(Debug, Clone)]
pub struct RecordingResult {
    pub wav_path: Option<PathBuf>,
    pub canceled: bool,
}

#[derive(Debug, Error, Clone)]
pub enum RecorderError {
    #[error("recorder not idle")]
    NotIdle,
    #[error("recorder not running")]
    NotRunning,
    #[error("recording canceled")]
    Canceled,
    #[error("audio backend init failed: {0}")]
    Initialize(String),
    #[error("open stream failed: {0}")]
    OpenStream(String),
    #[error("start stream failed: {0}")]
    StartStream(String),
    #[error("create wav failed: {0}")]
    CreateWav(String),
    #[error("stream read failed after {count} consecutive errors: {message}")]
    Read { count: usize, message: String },
    #[error("wav write failed: {0}")]
    WriteWav(String),
    #[error("wav close failed: {0}")]
    CloseWav(String),
    #[error("recorder worker stopped unexpectedly")]
    WorkerStopped,
    #[error("Microphone capture is only available on Windows")]
    UnsupportedPlatform,
}

// Streams are created, used and dropped on the recorder thread (COM apartment).
pub trait AudioStream {
    fn format(&self) -> &CaptureFormat;
    fn description(&self) -> String {
        String::new()
    }
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
    fn close(&mut self) -> Result<(), String>;
    /// Returns available whole frames; an empty packet means try later.
    fn read(&mut self, buffer: &mut Vec<u8>) -> Result<(), String>;
}

pub trait AudioBackend: Send + Sync + 'static {
    fn initialize(&self) -> Result<(), String> {
        Ok(())
    }
    fn terminate(&self) -> Result<(), String> {
        Ok(())
    }
    fn open_stream(&self, device_id: &str) -> Result<Box<dyn AudioStream>, String>;
}

struct ActiveRecording {
    done: mpsc::Receiver<Result<RecordingResult, RecorderError>>,
    cancellation: CancellationToken,
}

struct RecorderInner {
    state: RecorderState,
    active: Option<ActiveRecording>,
    on_error: Option<Arc<dyn Fn(RecorderError) + Send + Sync>>,
}

pub struct Recorder {
    config: Config,
    temp_dir: PathBuf,
    backend: Arc<dyn AudioBackend>,
    inner: Arc<Mutex<RecorderInner>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Recorder")
            .field("state", &self.state())
            .field("temp_dir", &self.temp_dir)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    pub fn new(config: Config, temp_dir: PathBuf) -> Self {
        Self::with_backend(config, temp_dir, Arc::new(SystemAudioBackend))
    }

    pub fn with_backend(config: Config, temp_dir: PathBuf, backend: Arc<dyn AudioBackend>) -> Self {
        Self {
            config,
            temp_dir,
            backend,
            inner: Arc::new(Mutex::new(RecorderInner {
                state: RecorderState::Idle,
                active: None,
                on_error: None,
            })),
        }
    }

    pub fn set_error_handler(&self, handler: Option<Arc<dyn Fn(RecorderError) + Send + Sync>>) {
        self.inner.lock().on_error = handler;
    }

    pub fn state(&self) -> RecorderState {
        self.inner.lock().state
    }

    pub async fn start(&self, lifecycle: CancellationToken) -> Result<(), RecorderError> {
        if lifecycle.is_cancelled() {
            return Err(RecorderError::Canceled);
        }
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let cancellation = lifecycle.child_token();
        let captured_handler;
        {
            let mut inner = self.inner.lock();
            if inner.state != RecorderState::Idle {
                return Err(RecorderError::NotIdle);
            }
            inner.state = RecorderState::Recording;
            captured_handler = inner.on_error.clone();
            inner.active = Some(ActiveRecording {
                done: done_rx,
                cancellation: cancellation.clone(),
            });
        }

        let config = self.config.clone();
        let temp_dir = self.temp_dir.clone();
        let backend = self.backend.clone();
        let inner = self.inner.clone();
        if let Err(error) = thread::Builder::new()
            .name("stt-audio-recorder".into())
            .spawn(move || {
                record_loop(
                    config,
                    temp_dir,
                    backend,
                    inner,
                    cancellation,
                    started_tx,
                    done_tx,
                    captured_handler,
                )
            })
        {
            let mut inner = self.inner.lock();
            inner.state = RecorderState::Idle;
            inner.active = None;
            return Err(RecorderError::Initialize(error.to_string()));
        }

        started_rx
            .await
            .unwrap_or(Err(RecorderError::WorkerStopped))
    }

    pub async fn stop(&self) -> Result<RecordingResult, RecorderError> {
        self.finish_request(RecorderState::Stopping).await
    }

    pub async fn cancel(&self) -> Result<RecordingResult, RecorderError> {
        self.finish_request(RecorderState::Canceled).await
    }

    pub fn request_cancel(&self) -> bool {
        let mut inner = self.inner.lock();
        if !matches!(
            inner.state,
            RecorderState::Recording | RecorderState::Paused
        ) {
            return false;
        }
        inner.state = RecorderState::Canceled;
        if let Some(active) = &inner.active {
            active.cancellation.cancel();
        }
        true
    }

    pub fn toggle_pause(&self) -> Result<RecorderState, RecorderError> {
        let mut inner = self.inner.lock();
        inner.state = match inner.state {
            RecorderState::Recording => RecorderState::Paused,
            RecorderState::Paused => RecorderState::Recording,
            _ => return Err(RecorderError::NotRunning),
        };
        Ok(inner.state)
    }

    async fn finish_request(
        &self,
        requested_state: RecorderState,
    ) -> Result<RecordingResult, RecorderError> {
        let receiver = {
            let mut inner = self.inner.lock();
            if !matches!(
                inner.state,
                RecorderState::Recording | RecorderState::Paused
            ) {
                return Err(RecorderError::NotRunning);
            }
            inner.state = requested_state;
            let active = inner.active.take().ok_or(RecorderError::WorkerStopped)?;
            active.cancellation.cancel();
            active.done
        };
        tokio::task::spawn_blocking(move || receiver.recv())
            .await
            .map_err(|_| RecorderError::WorkerStopped)?
            .map_err(|_| RecorderError::WorkerStopped)?
    }
}

#[allow(clippy::too_many_arguments)]
fn record_loop(
    config: Config,
    temp_dir: PathBuf,
    backend: Arc<dyn AudioBackend>,
    inner: Arc<Mutex<RecorderInner>>,
    cancellation: CancellationToken,
    started: tokio::sync::oneshot::Sender<Result<(), RecorderError>>,
    done: mpsc::SyncSender<Result<RecordingResult, RecorderError>>,
    captured_handler: Option<Arc<dyn Fn(RecorderError) + Send + Sync>>,
) {
    let wav_path = generate_temp_wav(&temp_dir);
    let initialized = match backend.initialize() {
        Ok(()) => true,
        Err(error) => {
            finish_start_failure(
                &inner,
                started,
                done,
                RecorderError::Initialize(error),
                false,
                backend.as_ref(),
            );
            return;
        }
    };
    let mut stream = match backend.open_stream(&config.input_device) {
        Ok(stream) => stream,
        Err(error) => {
            finish_start_failure(
                &inner,
                started,
                done,
                RecorderError::OpenStream(error),
                initialized,
                backend.as_ref(),
            );
            return;
        }
    };
    if let Err(error) = stream.start() {
        let _ = stream.close();
        drop(stream);
        finish_start_failure(
            &inner,
            started,
            done,
            RecorderError::StartStream(error),
            initialized,
            backend.as_ref(),
        );
        return;
    }
    let format = stream.format().clone();
    if config.record_debug {
        crate::debug_log::write(
            crate::debug_log::Category::Record,
            format_args!("[record] device={} format={format:?}", stream.description()),
        );
    }
    let mut writer = match CaptureWav::create(&wav_path, &format) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = stream.stop();
            let _ = stream.close();
            drop(stream);
            let _ = fs::remove_file(&wav_path);
            finish_start_failure(
                &inner,
                started,
                done,
                RecorderError::CreateWav(error.to_string()),
                initialized,
                backend.as_ref(),
            );
            return;
        }
    };

    let _ = started.send(Ok(()));
    let mut samples = Vec::new();
    let mut stream_paused = false;
    let mut consecutive_errors = 0;
    let result = 'recording: loop {
        let state = inner.lock().state;
        if cancellation.is_cancelled() {
            break if state == RecorderState::Stopping {
                match writer.finalize() {
                    Ok(()) => Ok(RecordingResult {
                        wav_path: Some(wav_path.clone()),
                        canceled: false,
                    }),
                    Err(error) => {
                        let _ = fs::remove_file(&wav_path);
                        Err(RecorderError::CloseWav(error.to_string()))
                    }
                }
            } else {
                drop(writer);
                let _ = fs::remove_file(&wav_path);
                Ok(RecordingResult {
                    wav_path: None,
                    canceled: true,
                })
            };
        }
        if state == RecorderState::Paused && stream_paused {
            thread::sleep(PAUSED_POLL_DELAY);
            continue;
        }
        let reading = if state == RecorderState::Paused {
            stream.stop().map(|()| {
                stream_paused = true;
                samples.clear();
            })
        } else if stream_paused {
            stream.start().map(|()| {
                stream_paused = false;
                samples.clear();
            })
        } else {
            stream.read(&mut samples)
        };
        match reading {
            Ok(()) => {
                consecutive_errors = 0;
                if let Err(error) = writer.write(&samples) {
                    drop(writer);
                    let _ = fs::remove_file(&wav_path);
                    break 'recording Err(RecorderError::WriteWav(error.to_string()));
                }
                if samples.is_empty() {
                    thread::sleep(SUCCESSFUL_WRITE_DELAY);
                }
            }
            Err(error) => {
                consecutive_errors += 1;
                if config.record_debug {
                    crate::debug_log::write(
                        crate::debug_log::Category::Record,
                        format_args!(
                            "[record] stream read error ({consecutive_errors}/{MAX_CONSECUTIVE_READ_ERRORS}): {error}"
                        ),
                    );
                }
                if consecutive_errors >= MAX_CONSECUTIVE_READ_ERRORS {
                    drop(writer);
                    let _ = fs::remove_file(&wav_path);
                    break Err(RecorderError::Read {
                        count: consecutive_errors,
                        message: error,
                    });
                }
                thread::sleep(READ_ERROR_RETRY_DELAY);
            }
        }
    };

    let _ = stream.stop();
    let _ = stream.close();
    drop(stream);
    let _ = backend.terminate();

    let previous_state = {
        let mut guard = inner.lock();
        let previous = guard.state;
        guard.state = RecorderState::Idle;
        guard.active = None;
        previous
    };
    let asynchronous_error = result.as_ref().err().cloned();
    let _ = done.send(result);
    if matches!(
        previous_state,
        RecorderState::Recording | RecorderState::Paused
    ) && let (Some(handler), Some(error)) = (captured_handler, asynchronous_error)
    {
        handler(error);
    }
}

fn finish_start_failure(
    inner: &Mutex<RecorderInner>,
    started: tokio::sync::oneshot::Sender<Result<(), RecorderError>>,
    done: mpsc::SyncSender<Result<RecordingResult, RecorderError>>,
    error: RecorderError,
    initialized: bool,
    backend: &dyn AudioBackend,
) {
    if initialized {
        let _ = backend.terminate();
    }
    {
        let mut guard = inner.lock();
        guard.state = RecorderState::Idle;
        guard.active = None;
    }
    let _ = done.send(Err(error.clone()));
    let _ = started.send(Err(error));
}

fn generate_temp_wav(directory: &Path) -> PathBuf {
    let id = Uuid::new_v4().simple().to_string();
    directory.join(format!("RecordTemp_{}.wav", &id[..16]))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        selected_devices: Mutex<Vec<String>>,
        open_error: Mutex<Option<String>>,
        initialize_error: Mutex<Option<String>>,
        reads: Arc<Mutex<VecDeque<Result<(), String>>>>,
        read_calls: Arc<AtomicUsize>,
        terminate_calls: AtomicUsize,
    }

    impl AudioBackend for FakeBackend {
        fn initialize(&self) -> Result<(), String> {
            self.initialize_error.lock().clone().map_or(Ok(()), Err)
        }

        fn terminate(&self) -> Result<(), String> {
            self.terminate_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn open_stream(&self, device_id: &str) -> Result<Box<dyn AudioStream>, String> {
            self.selected_devices.lock().push(device_id.into());
            if let Some(error) = self.open_error.lock().clone() {
                return Err(error);
            }
            Ok(Box::new(FakeStream {
                format: test_format(),
                reads: self.reads.clone(),
                read_calls: self.read_calls.clone(),
            }))
        }
    }

    struct FakeStream {
        format: CaptureFormat,
        reads: Arc<Mutex<VecDeque<Result<(), String>>>>,
        read_calls: Arc<AtomicUsize>,
    }

    fn test_format() -> CaptureFormat {
        // 44.1 kHz stereo 24-bit capture, independent of the output config.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&44100_u32.to_le_bytes());
        bytes.extend_from_slice(&(44100_u32 * 6).to_le_bytes());
        bytes.extend_from_slice(&6_u16.to_le_bytes());
        bytes.extend_from_slice(&24_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        CaptureFormat::from_wave_format(&bytes).unwrap()
    }

    #[tokio::test]
    async fn selected_device_and_native_format_ignore_output_settings() {
        let backend = Arc::new(FakeBackend::default());
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            input_device: "stable-endpoint-id".into(),
            sampling_rate: 16000,
            channels: 1,
            sampling_rate_depth: 16,
            ..Config::default()
        };
        let recorder = Recorder::with_backend(config, dir.path().to_path_buf(), backend.clone());
        recorder.start(CancellationToken::new()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let result = recorder.stop().await.unwrap();
        assert_eq!(*backend.selected_devices.lock(), ["stable-endpoint-id"]);
        let wav = hound::WavReader::open(result.wav_path.unwrap()).unwrap();
        assert_eq!(wav.spec().sample_rate, 44100);
        assert_eq!(wav.spec().channels, 2);
        assert_eq!(wav.spec().bits_per_sample, 24);
        assert!(wav.duration() > 0);
    }

    #[tokio::test]
    async fn unavailable_device_does_not_fall_back_and_can_reconnect() {
        let backend = Arc::new(FakeBackend::default());
        *backend.open_error.lock() = Some("Selected microphone unavailable".into());
        let dir = tempfile::tempdir().unwrap();
        let recorder = Recorder::with_backend(
            Config {
                input_device: "missing".into(),
                ..Config::default()
            },
            dir.path().to_path_buf(),
            backend.clone(),
        );
        assert!(matches!(
            recorder.start(CancellationToken::new()).await,
            Err(RecorderError::OpenStream(_))
        ));
        assert_eq!(recorder.state(), RecorderState::Idle);
        assert_eq!(*backend.selected_devices.lock(), ["missing"]);
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        *backend.open_error.lock() = None;
        recorder.start(CancellationToken::new()).await.unwrap();
        recorder.cancel().await.unwrap();
        assert_eq!(*backend.selected_devices.lock(), ["missing", "missing"]);
    }

    impl AudioStream for FakeStream {
        fn format(&self) -> &CaptureFormat {
            &self.format
        }
        fn start(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn close(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn read(&mut self, buffer: &mut Vec<u8>) -> Result<(), String> {
            self.read_calls.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(5));
            buffer.clear();
            buffer.resize(600, 1);
            self.reads.lock().pop_front().unwrap_or(Ok(()))
        }
    }

    #[tokio::test]
    async fn startup_errors_return_without_async_notification() {
        let backend = Arc::new(FakeBackend::default());
        *backend.initialize_error.lock() = Some("unavailable".into());
        let recorder = Recorder::with_backend(
            Config::default(),
            tempfile::tempdir().unwrap().path().to_path_buf(),
            backend,
        );
        let notifications = Arc::new(AtomicUsize::new(0));
        let count = notifications.clone();
        recorder.set_error_handler(Some(Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })));
        assert!(matches!(
            recorder.start(CancellationToken::new()).await,
            Err(RecorderError::Initialize(_))
        ));
        assert_eq!(recorder.state(), RecorderState::Idle);
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pause_does_not_read_and_cancel_removes_wav() {
        let backend = Arc::new(FakeBackend::default());
        let temp = tempfile::tempdir().unwrap();
        let recorder = Recorder::with_backend(
            Config::default(),
            temp.path().to_path_buf(),
            backend.clone(),
        );
        recorder.start(CancellationToken::new()).await.unwrap();
        recorder.toggle_pause().unwrap();
        let reads = backend.read_calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(backend.read_calls.load(Ordering::SeqCst), reads);
        assert!(recorder.cancel().await.unwrap().canceled);
        assert!(fs::read_dir(temp.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn ten_consecutive_read_errors_terminate_and_delete_partial_file() {
        let backend = Arc::new(FakeBackend::default());
        backend
            .reads
            .lock()
            .extend((0..MAX_CONSECUTIVE_READ_ERRORS).map(|_| Err("device disconnected".into())));
        let temp = tempfile::tempdir().unwrap();
        let recorder = Recorder::with_backend(
            Config::default(),
            temp.path().to_path_buf(),
            backend.clone(),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        recorder.set_error_handler(Some(Arc::new(move |error| {
            let _ = tx.send(error);
        })));
        recorder.start(CancellationToken::new()).await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(error.to_string().contains("10 consecutive errors"));
        assert_eq!(recorder.state(), RecorderState::Idle);
        assert!(fs::read_dir(temp.path()).unwrap().next().is_none());
    }
}
