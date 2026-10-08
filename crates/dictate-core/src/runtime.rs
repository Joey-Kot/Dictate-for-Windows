use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::advanced_audio::LiveChunkSource;
use crate::asr::{AsrError, Transcription};
use crate::audio_api::{AudioApiClient, AudioApiError};
use crate::audio_segments::SegmentPlan;
use crate::cache;
use crate::clipboard::ClipboardError;
use crate::converter::{AudioConverter, ConvertError, prepare_audio_for_upload};
use crate::hotkey::{self, HotkeyRegistration};
use crate::recorder::{
    CapturePacketReceiver, Recorder, RecorderError, RecorderState, RecordingResult,
    capture_packet_channel,
};
use crate::segmented_upload::{PreparedSegmentedUpload, prepare_segmented_upload};
use crate::text_input;

const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_millis(250);
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_RETRY_AUDIO_BYTES: u64 = 100_000_000;

/// The retry buffer intentionally retains the original WAV rather than any
/// encoded segment output.  Segment boundaries and the upload concurrency are
/// frozen with it so changing those settings after a failure cannot alter the
/// retry's source timeline or dispatch behavior.
#[derive(Clone)]
struct RetryRecording {
    original_wav: Arc<Vec<u8>>,
    upload_mode: RetryUploadMode,
}

#[derive(Clone)]
enum RetryUploadMode {
    Single,
    Segmented {
        plan: Option<Arc<SegmentPlan>>,
        max_segment_seconds: u32,
        min_pause_ms: u32,
        max_concurrency: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Idle,
    Recording,
    Paused,
    Uploading,
    Rewriting,
    Error,
}

impl std::fmt::Display for State {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub state: State,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default)]
    pub retry_available: bool,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            state: State::Idle,
            message: String::new(),
            error: String::new(),
            retry_available: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("{0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("{0}")]
    AudioApi(#[from] AudioApiError),
    #[error("{0}")]
    Recorder(#[from] RecorderError),
    #[error("{0}")]
    Convert(#[from] ConvertError),
    #[error("{0}")]
    Clipboard(#[from] ClipboardError),
    #[error("{0}")]
    Hotkey(#[from] hotkey::HotkeyError),
    #[error("runtime stopped")]
    Stopped,
    #[error("runtime action is busy")]
    Busy,
    #[error("cannot save settings while {0}")]
    CannotReload(State),
    #[error("recording completed without a WAV path")]
    MissingWav,
    #[error("input file '{path}' is unavailable: {source}")]
    InputFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write transcription '{path}': {source}")]
    OutputFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

struct RuntimeInner {
    config: Config,
    temp_dir: PathBuf,
    recorder: Arc<Recorder>,
    audio_client: Arc<AudioApiClient>,
    hotkeys: Option<HotkeyRegistration>,
    // A failed registration must not disable the user's intent to use hotkeys.
    hotkeys_requested: bool,
    event: Event,
    event_handler: Option<Arc<dyn Fn(Event) + Send + Sync>>,
    next_session: u64,
    active_session: u64,
    active_request_cancellation: Option<CancellationToken>,
    retry_buffer_enabled: bool,
    retry_recording: Option<Arc<RetryRecording>>,
    live_realtime: Option<LiveRealtimeSession>,
    // The live session is normally owned by `live_realtime`. While Pause is
    // waiting for its terminal event, it is temporarily taken out of that
    // slot. Keep its token reachable only during that interval so Cancel can
    // interrupt connect/receive/finalization without changing Recording's
    // ordinary cancel path.
    live_finalization_cancellation: Option<CancellationToken>,
    completed_live_transcriptions: Vec<Transcription>,
    live_realtime_failed: bool,
}

struct LiveRealtimeSession {
    cancellation: CancellationToken,
    task: JoinHandle<Result<Transcription, AudioApiError>>,
}

/// The live portions of one logical recording. A failed portion invalidates
/// every committed portion: the only correct recovery is a new full replay of
/// the local WAV from audio offset zero.
struct LiveRealtimeState {
    active: Option<LiveRealtimeSession>,
    committed: Vec<Transcription>,
    failed: bool,
}

pub struct Runtime {
    inner: Mutex<RuntimeInner>,
    action_lock: Arc<AsyncMutex<()>>,
    lifecycle: CancellationToken,
    converter: Arc<dyn AudioConverter>,
    executor: Handle,
    #[cfg(test)]
    hotkey_registrar: Mutex<Option<TestHotkeyRegistrar>>,
}

#[cfg(test)]
type TestHotkeyRegistrar =
    Arc<dyn Fn(&Config) -> Result<HotkeyRegistration, hotkey::HotkeyError> + Send + Sync>;

impl std::fmt::Debug for Runtime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Runtime")
            .field("event", &self.snapshot())
            .finish_non_exhaustive()
    }
}

impl Runtime {
    pub fn new(
        mut config: Config,
        converter: Arc<dyn AudioConverter>,
    ) -> Result<Arc<Self>, RuntimeError> {
        config.validate()?;
        let temp_dir = cache::initialize_cache_dir(&mut config);
        cache::cleanup_old_temp_items(&temp_dir);
        let audio_client = Arc::new(AudioApiClient::new(config.clone())?);
        let recorder = Arc::new(Recorder::new(config.clone(), temp_dir.clone()));
        let runtime = Arc::new(Self {
            inner: Mutex::new(RuntimeInner {
                config,
                temp_dir,
                recorder,
                audio_client,
                hotkeys: None,
                hotkeys_requested: false,
                event: Event::default(),
                event_handler: None,
                next_session: 0,
                active_session: 0,
                active_request_cancellation: None,
                retry_buffer_enabled: false,
                retry_recording: None,
                live_realtime: None,
                live_finalization_cancellation: None,
                completed_live_transcriptions: Vec::new(),
                live_realtime_failed: false,
            }),
            action_lock: Arc::new(AsyncMutex::new(())),
            lifecycle: CancellationToken::new(),
            converter,
            executor: Handle::current(),
            #[cfg(test)]
            hotkey_registrar: Mutex::new(None),
        });
        Ok(runtime)
    }

    pub fn set_event_handler(&self, handler: Option<Arc<dyn Fn(Event) + Send + Sync>>) {
        self.inner.lock().event_handler = handler;
    }

    pub fn snapshot(&self) -> Event {
        self.inner.lock().event.clone()
    }

    pub fn config(&self) -> Config {
        self.inner.lock().config.clone()
    }

    /// Enables the bounded, in-memory recording buffer used by interactive retry controls.
    pub fn enable_retry_buffer(&self) {
        self.inner.lock().retry_buffer_enabled = true;
    }

    pub fn has_retryable_recording(&self) -> bool {
        let inner = self.inner.lock();
        inner.retry_buffer_enabled && inner.retry_recording.is_some()
    }

    pub fn can_reload(&self) -> bool {
        matches!(self.snapshot().state, State::Idle | State::Error)
    }

    pub fn is_stopped(&self) -> bool {
        self.lifecycle.is_cancelled()
    }

    pub fn try_toggle_recording(self: &Arc<Self>) -> bool {
        self.try_action(1)
    }

    pub fn try_toggle_pause(self: &Arc<Self>) -> bool {
        self.try_action(2)
    }

    pub fn try_cancel_or_retry(self: &Arc<Self>) -> bool {
        self.try_action(3)
    }

    pub async fn handle_action(self: &Arc<Self>, id: i32) -> bool {
        if self.is_stopped() {
            return false;
        }
        if id == 3 && self.cancel_active_request() {
            return true;
        }
        let Ok(guard) = self.action_lock.clone().try_lock_owned() else {
            return false;
        };
        self.clone().handle_action_with_guard(id, guard).await;
        true
    }

    fn try_action(self: &Arc<Self>, id: i32) -> bool {
        if self.is_stopped() {
            return false;
        }
        if id == 3 && self.cancel_active_request() {
            return true;
        }
        let Ok(guard) = self.action_lock.clone().try_lock_owned() else {
            return false;
        };
        let runtime = self.clone();
        self.executor.spawn(async move {
            runtime.handle_action_with_guard(id, guard).await;
        });
        true
    }

    async fn handle_action_with_guard(self: Arc<Self>, id: i32, _guard: OwnedMutexGuard<()>) {
        if self.is_stopped() {
            return;
        }
        match id {
            1 => self.toggle_recording_locked().await,
            2 => self.toggle_pause_locked().await,
            3 => self.cancel_or_retry_locked().await,
            _ => {}
        }
    }

    pub fn try_rewrite(self: &Arc<Self>, prompt_id: &str) -> bool {
        if self.is_stopped() {
            return false;
        }
        let Ok(guard) = self.action_lock.clone().try_lock_owned() else {
            return false;
        };
        if !self.can_reload() {
            return false;
        }
        let config = self.config();
        let Some(prompt) = config
            .rewrite
            .prompts
            .iter()
            .find(|p| p.id == prompt_id)
            .cloned()
        else {
            return false;
        };
        let cancellation = self.begin_active_request();
        self.set_state(
            State::Rewriting,
            "Rewriting selected text",
            None::<&RuntimeError>,
        );
        let runtime = self.clone();
        self.executor.spawn(async move {
            let _guard = guard;
            let input = match crate::selection::read(
                &cancellation,
                Duration::from_millis(config.clipboard_write_delay),
                Duration::from_millis(config.clipboard_restore_delay),
            )
            .await
            {
                Ok(input) => Ok(input),
                Err(crate::selection::SelectionError::Restore(error)) => {
                    // A cancellation must not hide loss of clipboard contents.
                    if !runtime.is_stopped() {
                        runtime.set_state(State::Idle, "Clipboard restore failed", Some(&error));
                    }
                    runtime.clear_active_request();
                    return;
                }
                Err(error) => Err(error.to_string()),
            };
            let result = async {
                let input = input?;
                let client = crate::rewrite::RewriteClient::new(config.clone())
                    .map_err(|e| e.to_string())?;
                client
                    .execute(&prompt, &input, &cancellation, true)
                    .await
                    .map_err(|e| e.to_string())
            }
            .await;
            runtime.finish_rewrite(result, &cancellation, &config).await;
            runtime.clear_active_request();
        });
        true
    }

    async fn finish_rewrite(
        &self,
        result: Result<String, String>,
        cancellation: &CancellationToken,
        config: &Config,
    ) {
        self.finish_rewrite_with(result, cancellation, |text| async move {
            text_input::send_text(&text, cancellation, config).await
        })
        .await;
    }

    async fn finish_rewrite_with<F, Fut>(
        &self,
        result: Result<String, String>,
        cancellation: &CancellationToken,
        output: F,
    ) where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), text_input::TextInputError>>,
    {
        if self.is_stopped() {
            return;
        }
        if cancellation.is_cancelled() {
            self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
            return;
        }
        match result {
            Ok(text) if !text.trim().is_empty() => match output(text).await {
                Ok(()) => self.set_state(State::Idle, "Rewrite completed", None::<&RuntimeError>),
                Err(error) if error.canceled_before_output() => {
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>)
                }
                Err(error) => self.set_state(State::Idle, error.status(), Some(&error)),
            },
            Ok(_) => self.set_state(
                State::Idle,
                "Rewrite failed",
                Some("Rewrite response contains no text"),
            ),
            Err(error) => self.set_state(State::Idle, "Rewrite failed", Some(&error)),
        }
    }

    /// Connectivity probes share the same admission, cancellation and task lifetime.
    /// The supplied operation never delivers text or changes the recording buffer.
    pub async fn test_connection<F, Fut>(
        self: &Arc<Self>,
        rewrite: bool,
        external: CancellationToken,
        operation: F,
    ) -> Result<(), String>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let _guard = self
            .action_lock
            .clone()
            .try_lock_owned()
            .map_err(|_| "A task is already running".to_owned())?;
        if self.is_stopped() || !self.can_reload() {
            return Err("A task is already running".into());
        }
        let cancellation = self.begin_active_request();
        self.set_state(
            if rewrite {
                State::Rewriting
            } else {
                State::Uploading
            },
            "Testing API connection",
            None::<&RuntimeError>,
        );
        let result = {
            let operation = operation(cancellation.clone());
            tokio::pin!(operation);
            tokio::select! { biased;
                _ = external.cancelled() => {
                    cancellation.cancel();
                    let _ = operation.await;
                    Err("Request canceled".into())
                }
                result = &mut operation => result,
            }
        };
        let result = if cancellation.is_cancelled() {
            Err("Request canceled".into())
        } else {
            result
        };
        self.clear_active_request();
        self.set_state(
            State::Idle,
            if cancellation.is_cancelled() {
                "Request canceled"
            } else if result.is_ok() {
                "Connection test completed"
            } else {
                "Connection test failed"
            },
            result.as_ref().err(),
        );
        result
    }

    pub fn start_hotkeys(self: &Arc<Self>) -> Result<(), RuntimeError> {
        if self.is_stopped() {
            return Err(RuntimeError::Stopped);
        }
        let config = {
            let mut inner = self.inner.lock();
            inner.hotkeys_requested = true;
            if inner.hotkeys.is_some() {
                return Ok(());
            }
            inner.config.clone()
        };
        let registration = self.register_hotkeys(&config);
        match registration {
            Ok(registration) => {
                if self.is_stopped() {
                    registration.stop();
                    return Err(RuntimeError::Stopped);
                }
                self.inner.lock().hotkeys = Some(registration);
                Ok(())
            }
            Err(error) => {
                self.set_state(State::Error, "Hotkey registration failed", Some(&error));
                Err(error.into())
            }
        }
    }

    fn register_hotkeys(
        self: &Arc<Self>,
        config: &Config,
    ) -> Result<HotkeyRegistration, hotkey::HotkeyError> {
        #[cfg(test)]
        {
            let register = self.hotkey_registrar.lock().clone();
            if let Some(register) = register {
                return register(config);
            }
        }
        let weak = Arc::downgrade(self);
        let prompt_ids: Vec<_> = config
            .rewrite
            .prompts
            .iter()
            .map(|p| p.id.clone())
            .collect();
        hotkey::register(
            &config.start_key,
            &config.pause_key,
            &config.cancel_or_retry_key,
            &config.rewrite.prompts,
            config.hotkey_hook,
            move |id| {
                if let Some(runtime) = weak.upgrade() {
                    let accepted = if id >= 1000 {
                        prompt_ids
                            .get((id - 1000) as usize)
                            .is_some_and(|prompt| runtime.try_rewrite(prompt))
                    } else {
                        runtime.try_action(id)
                    };
                    if !accepted && runtime.config().hotkey_debug {
                        crate::debug_log::write(
                            crate::debug_log::Category::Hotkey,
                            format_args!(
                                "[hotkey-debug] dropped action id={id} while another action is in progress"
                            ),
                        );
                    }
                }
            },
            config.hotkey_debug,
        )
    }

    pub async fn reload(self: &Arc<Self>, config: Config) -> Result<(), RuntimeError> {
        self.apply_config(config, None).await
    }

    pub async fn save_and_reload(
        self: &Arc<Self>,
        config: Config,
        path: &Path,
    ) -> Result<(), RuntimeError> {
        self.apply_config(config, Some(path)).await
    }

    async fn apply_config(
        self: &Arc<Self>,
        mut config: Config,
        path: Option<&Path>,
    ) -> Result<(), RuntimeError> {
        if self.is_stopped() {
            return Err(RuntimeError::Stopped);
        }
        let _guard = self
            .action_lock
            .clone()
            .try_lock_owned()
            .map_err(|_| RuntimeError::Busy)?;
        let current_state = self.snapshot().state;
        if !matches!(current_state, State::Idle | State::Error) {
            return Err(RuntimeError::CannotReload(current_state));
        }
        config.validate()?;
        let temp_dir = cache::initialize_cache_dir(&mut config);
        let audio_client = Arc::new(AudioApiClient::new(config.clone())?);
        let recorder = Arc::new(Recorder::new(config.clone(), temp_dir.clone()));
        let (previous_config, previous_hotkeys, hotkeys_requested) = {
            let mut inner = self.inner.lock();
            (
                inner.config.clone(),
                inner.hotkeys.take(),
                inner.hotkeys_requested,
            )
        };
        let had_hotkeys = previous_hotkeys.is_some();
        if let Some(hotkeys) = previous_hotkeys {
            hotkeys.stop_and_wait();
        }
        let mut replacement = None;
        let result = (|| -> Result<(), RuntimeError> {
            if hotkeys_requested {
                replacement = Some(self.register_hotkeys(&config)?);
            }
            let mut inner = self.inner.lock();
            if self.is_stopped() {
                return Err(RuntimeError::Stopped);
            }
            if let Some(path) = path {
                config.save(path)?;
            }
            inner.config = config;
            inner.temp_dir = temp_dir;
            inner.audio_client = audio_client;
            inner.recorder = recorder;
            inner.hotkeys = replacement.take();
            inner.active_session = 0;
            inner.active_request_cancellation = None;
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(hotkeys) = replacement {
                hotkeys.stop_and_wait();
            }
            if had_hotkeys && !self.is_stopped() {
                match self.register_hotkeys(&previous_config) {
                    Ok(hotkeys) => {
                        let mut inner = self.inner.lock();
                        if self.is_stopped() {
                            drop(inner);
                            hotkeys.stop();
                        } else {
                            inner.hotkeys = Some(hotkeys);
                        }
                    }
                    Err(rollback) => {
                        let message = format!("{error}; failed to restore hotkeys: {rollback}");
                        self.set_state(State::Error, "Hotkey registration failed", Some(&message));
                        return Err(hotkey::HotkeyError::Registration(message).into());
                    }
                }
            }
            return Err(error);
        }
        self.set_state(State::Idle, "Settings saved", None::<&RuntimeError>);
        Ok(())
    }

    pub fn stop(&self) {
        if self.lifecycle.is_cancelled() {
            return;
        }
        self.lifecycle.cancel();
        self.cancel_live_realtime();
        let (recorder, hotkeys, request_cancellation) = {
            let mut inner = self.inner.lock();
            inner.retry_recording = None;
            (
                inner.recorder.clone(),
                inner.hotkeys.take(),
                inner.active_request_cancellation.take(),
            )
        };
        recorder.set_capture_packet_sink(None);
        recorder.request_cancel();
        if let Some(cancellation) = request_cancellation {
            cancellation.cancel();
        }
        if let Some(hotkeys) = hotkeys {
            hotkeys.stop();
        }

        // Copy may already be queued in another application. The native worker
        // restores the clipboard independently of the async task/executor.
        crate::selection::wait_for_cleanup();

        let deadline = Instant::now() + SHUTDOWN_GRACE_PERIOD;
        while Instant::now() < deadline {
            if let Ok(guard) = self.action_lock.try_lock() {
                drop(guard);
                return;
            }
            std::thread::sleep(SHUTDOWN_POLL_INTERVAL);
        }
    }

    async fn toggle_recording_locked(self: &Arc<Self>) {
        let state = self.snapshot().state;
        if matches!(state, State::Idle | State::Error) {
            let (recorder, session) = self.begin_recording_session();
            self.reset_live_realtime(&recorder);
            let live_receiver = self.prepare_live_realtime(&recorder);
            if let Err(error) = recorder.start(self.lifecycle.child_token()).await {
                recorder.set_capture_packet_sink(None);
                self.clear_recording_session(&recorder, session);
                if !self.is_stopped() {
                    self.set_retryable_error("Recording start failed", &error);
                }
                return;
            }
            if self.is_stopped() {
                recorder.set_capture_packet_sink(None);
                self.cancel_live_realtime();
                recorder.request_cancel();
                self.clear_recording_session(&recorder, session);
                return;
            }
            if let Some(receiver) = live_receiver {
                self.start_live_realtime(receiver);
            }
            self.set_state(State::Recording, "Recording started", None::<&RuntimeError>);
            return;
        }
        if !matches!(state, State::Recording | State::Paused) {
            return;
        }
        let (recorder, session) = {
            let inner = self.inner.lock();
            (inner.recorder.clone(), inner.active_session)
        };
        // Closing the tee before stopping the WAV writer lets the live source
        // flush its final partial packet and send the realtime finish message.
        recorder.set_capture_packet_sink(None);
        match recorder.stop().await {
            Ok(result) => {
                self.clear_recording_session(&recorder, session);
                if self.is_stopped() {
                    discard_recording(&result);
                    return;
                }
                if result.canceled {
                    self.cancel_live_realtime();
                    self.set_state(State::Idle, "Recording canceled", None::<&RuntimeError>);
                    return;
                }
                self.save_completed_recording_for_retry(&result);
                let cancellation = self.begin_active_request();
                self.set_state(
                    State::Uploading,
                    "Uploading ASR request",
                    None::<&RuntimeError>,
                );
                self.transcribe_recording(result, &cancellation, true).await;
                self.clear_active_request();
            }
            Err(error) => {
                if !matches!(
                    error,
                    RecorderError::NotRunning | RecorderError::WorkerStopped
                ) {
                    self.clear_recording_session(&recorder, session);
                }
                if !self.is_stopped() {
                    self.set_retryable_error("Recording stop failed", &error);
                }
            }
        }
    }

    fn begin_recording_session(self: &Arc<Self>) -> (Arc<Recorder>, u64) {
        let (recorder, session) = {
            let mut inner = self.inner.lock();
            inner.next_session += 1;
            inner.active_session = inner.next_session;
            (inner.recorder.clone(), inner.active_session)
        };
        let weak: Weak<Self> = Arc::downgrade(self);
        let recorder_for_callback = Arc::downgrade(&recorder);
        recorder.set_error_handler(Some(Arc::new(move |error| {
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            let Some(recorder) = recorder_for_callback.upgrade() else {
                return;
            };
            let executor = runtime.executor.clone();
            executor.spawn(async move {
                let _guard = runtime.action_lock.lock().await;
                runtime.handle_recorder_error(&recorder, session, error);
            });
        })));
        (recorder, session)
    }

    fn clear_recording_session(&self, recorder: &Arc<Recorder>, session: u64) {
        let mut inner = self.inner.lock();
        if Arc::ptr_eq(&inner.recorder, recorder) && inner.active_session == session {
            inner.active_session = 0;
        }
    }

    /// Cancels any state left by a prior logical recording before a new one
    /// starts. A recorder keeps its local WAV independently of this tee.
    fn reset_live_realtime(&self, recorder: &Arc<Recorder>) {
        recorder.set_capture_packet_sink(None);
        let (finalization_cancellation, previous) = {
            let mut inner = self.inner.lock();
            inner.completed_live_transcriptions.clear();
            inner.live_realtime_failed = false;
            (
                inner.live_finalization_cancellation.take(),
                inner.live_realtime.take(),
            )
        };
        if let Some(cancellation) = finalization_cancellation {
            cancellation.cancel();
        }
        if let Some(previous) = previous {
            previous.cancellation.cancel();
            previous.task.abort();
        }
    }

    /// Creates the recorder's optional bounded packet tee before capture
    /// starts or before a paused recorder resumes. The local WAV is always
    /// written first by `Recorder`.
    fn prepare_live_realtime(&self, recorder: &Arc<Recorder>) -> Option<CapturePacketReceiver> {
        let realtime = {
            let inner = self.inner.lock();
            inner.audio_client.is_realtime_workflow()
                && !inner.live_realtime_failed
                && inner.live_realtime.is_none()
        };
        if !realtime {
            recorder.set_capture_packet_sink(None);
            return None;
        }
        let (sink, receiver) = capture_packet_channel(128);
        recorder.set_capture_packet_sink(Some(sink));
        Some(receiver)
    }

    fn start_live_realtime(&self, receiver: CapturePacketReceiver) {
        let (client, target) = {
            let inner = self.inner.lock();
            (
                inner.audio_client.clone(),
                inner.audio_client.realtime_audio_stream(),
            )
        };
        let Some(target) = target else {
            self.inner.lock().live_realtime_failed = true;
            return;
        };
        let cancellation = self.lifecycle.child_token();
        let task_cancellation = cancellation.clone();
        let task = self.executor.spawn(async move {
            let mut source = LiveChunkSource::new(receiver, target);
            client
                .transcribe_live(&task_cancellation, &mut source)
                .await
        });
        let mut inner = self.inner.lock();
        if inner.live_realtime.is_some() {
            // Runtime actions are serialized, so this branch is defensive.
            // Do not merge an ambiguous live session; replay the local WAV.
            inner.live_realtime_failed = true;
            drop(inner);
            cancellation.cancel();
            task.abort();
        } else {
            inner.live_realtime = Some(LiveRealtimeSession { cancellation, task });
        }
    }

    async fn finalize_live_realtime_segment(&self, recorder: &Arc<Recorder>) {
        // Dropping the sender, rather than canceling the task, lets the
        // session flush the converter and receive its explicit final event.
        recorder.set_capture_packet_sink(None);
        let session = self.inner.lock().live_realtime.take();
        let Some(session) = session else {
            return;
        };
        // `toggle_pause_locked` holds `action_lock` while it waits below.
        // Expose this exact session token for that interval so the Cancel
        // hotkey can wake WebSocket connect/receive/finalization. It is not
        // installed while ordinary microphone capture is active.
        let finalization_cancellation = session.cancellation.clone();
        self.inner.lock().live_finalization_cancellation = Some(finalization_cancellation.clone());
        let result = session.task.await;
        let canceled = finalization_cancellation.is_cancelled();
        let mut inner = self.inner.lock();
        inner.live_finalization_cancellation = None;
        if canceled {
            // A canceled finalization may have accumulated a partial final
            // segment. Keep no live result; a later Stop will replay the
            // authoritative WAV from its beginning.
            inner.completed_live_transcriptions.clear();
            inner.live_realtime_failed = true;
            return;
        }
        match result {
            Ok(Ok(transcription)) if !inner.live_realtime_failed => {
                inner.completed_live_transcriptions.push(transcription);
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => {
                // Do not retain a transcript from any preceding segment. A
                // later stop must replay the entire local recording.
                inner.completed_live_transcriptions.clear();
                inner.live_realtime_failed = true;
            }
        }
    }

    fn take_live_realtime(&self) -> LiveRealtimeState {
        let mut inner = self.inner.lock();
        let state = LiveRealtimeState {
            active: inner.live_realtime.take(),
            committed: std::mem::take(&mut inner.completed_live_transcriptions),
            failed: inner.live_realtime_failed,
        };
        inner.live_realtime_failed = false;
        state
    }

    fn cancel_live_realtime(&self) {
        let (finalization_cancellation, session) = {
            let mut inner = self.inner.lock();
            inner.completed_live_transcriptions.clear();
            inner.live_realtime_failed = false;
            (
                inner.live_finalization_cancellation.take(),
                inner.live_realtime.take(),
            )
        };
        if let Some(cancellation) = finalization_cancellation {
            cancellation.cancel();
        }
        if let Some(session) = session {
            session.cancellation.cancel();
            session.task.abort();
        }
    }

    fn begin_active_request(&self) -> CancellationToken {
        let cancellation = self.lifecycle.child_token();
        self.inner.lock().active_request_cancellation = Some(cancellation.clone());
        cancellation
    }

    fn clear_active_request(&self) {
        self.inner.lock().active_request_cancellation = None;
    }

    fn cancel_active_request(&self) -> bool {
        let cancellation = {
            let inner = self.inner.lock();
            if matches!(inner.event.state, State::Uploading | State::Rewriting) {
                inner.active_request_cancellation.clone()
            } else {
                // This is populated only after Pause has closed the recorder
                // tee and while its live realtime task is being finalized.
                // In particular, do not expose a live session token during
                // normal Recording: Cancel must keep acquiring the action
                // lock and canceling the recorder as it did before realtime.
                inner.live_finalization_cancellation.clone()
            }
        };
        let Some(cancellation) = cancellation else {
            return false;
        };
        cancellation.cancel();
        true
    }

    fn handle_recorder_error(&self, recorder: &Arc<Recorder>, session: u64, error: RecorderError) {
        if self.is_stopped() {
            return;
        }
        let should_report = {
            let mut inner = self.inner.lock();
            if !Arc::ptr_eq(&inner.recorder, recorder)
                || inner.active_session != session
                || !matches!(
                    inner.event.state,
                    State::Recording | State::Paused | State::Error
                )
            {
                false
            } else {
                inner.active_session = 0;
                true
            }
        };
        if should_report {
            recorder.set_capture_packet_sink(None);
            self.cancel_live_realtime();
            self.set_retryable_error("Recording failed", &error);
        }
    }

    async fn toggle_pause_locked(&self) {
        let (state, recorder, debug) = {
            let inner = self.inner.lock();
            (
                inner.event.state,
                inner.recorder.clone(),
                inner.config.hotkey_debug,
            )
        };
        // Install the new bounded target before unpausing the microphone so
        // the resumed session cannot miss its first capture packet.
        let live_receiver = if state == State::Paused {
            self.prepare_live_realtime(&recorder)
        } else {
            None
        };
        match recorder.toggle_pause() {
            Ok(RecorderState::Paused) => {
                self.finalize_live_realtime_segment(&recorder).await;
                self.set_state(State::Paused, "Recording paused", None::<&RuntimeError>)
            }
            Ok(RecorderState::Recording) => {
                if let Some(receiver) = live_receiver {
                    self.start_live_realtime(receiver);
                }
                self.set_state(State::Recording, "Recording resumed", None::<&RuntimeError>)
            }
            Ok(_) => {}
            Err(_) => {
                recorder.set_capture_packet_sink(None);
                if debug {
                    crate::debug_log::write(
                        crate::debug_log::Category::Hotkey,
                        format_args!("[hotkey] not recording; cannot pause/resume"),
                    );
                }
            }
        }
    }

    async fn cancel_recording_locked(&self) {
        let (state, recorder, session, debug) = {
            let inner = self.inner.lock();
            (
                inner.event.state,
                inner.recorder.clone(),
                inner.active_session,
                inner.config.hotkey_debug,
            )
        };
        if !matches!(state, State::Recording | State::Paused) {
            if debug {
                crate::debug_log::write(
                    crate::debug_log::Category::Hotkey,
                    format_args!("[hotkey] not recording; nothing to cancel"),
                );
            }
            return;
        }
        self.cancel_live_realtime();
        match recorder.cancel().await {
            Ok(_) => {
                self.clear_recording_session(&recorder, session);
                if !self.is_stopped() {
                    self.set_state(State::Idle, "Recording canceled", None::<&RuntimeError>);
                }
            }
            Err(error) => {
                if !matches!(
                    error,
                    RecorderError::NotRunning | RecorderError::WorkerStopped
                ) {
                    self.clear_recording_session(&recorder, session);
                }
                if !self.is_stopped() {
                    self.set_retryable_error("Cancel failed", &error);
                }
            }
        }
    }

    async fn cancel_or_retry_locked(&self) {
        let retry_available = {
            let inner = self.inner.lock();
            inner.event.state == State::Idle
                && inner.retry_buffer_enabled
                && inner.retry_recording.is_some()
        };
        if retry_available {
            self.retry_recording_locked().await;
        } else {
            self.cancel_recording_locked().await;
        }
    }

    async fn retry_recording_locked(&self) {
        let (temp_dir, recording) = {
            let inner = self.inner.lock();
            if inner.event.state != State::Idle || !inner.retry_buffer_enabled {
                return;
            }
            (inner.temp_dir.clone(), inner.retry_recording.clone())
        };
        let Some(recording) = recording else {
            return;
        };

        // Enter the cancelable request state before restoring the bounded in-memory WAV. The
        // restoration can still take noticeable time for a 100 MB recording.
        let cancellation = self.begin_active_request();
        self.set_state(
            State::Uploading,
            "Retrying ASR request",
            None::<&RuntimeError>,
        );

        let wav_path = cache::temporary_output_path(&temp_dir, "wav");
        let upload_mode = recording.upload_mode.clone();
        let write_cancellation = cancellation.clone();
        let write_result = tokio::task::spawn_blocking({
            let wav_path = wav_path.clone();
            move || {
                if write_cancellation.is_cancelled() {
                    return Ok(());
                }
                std::fs::write(&wav_path, recording.original_wav.as_slice())
            }
        })
        .await;
        if self.is_stopped() {
            let _ = std::fs::remove_file(&wav_path);
            self.clear_active_request();
            return;
        }
        if cancellation.is_cancelled() {
            let _ = std::fs::remove_file(&wav_path);
            self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
            self.clear_active_request();
            return;
        }
        let write_error = match write_result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = write_error {
            // std::fs::write may have created a partial WAV before reporting an error.
            let _ = std::fs::remove_file(&wav_path);
            if cancellation.is_cancelled() {
                self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
            } else {
                self.set_retryable_error("Retry preparation failed", &error);
            }
            self.clear_active_request();
            return;
        }

        self.transcribe_recording_with_upload_mode(
            RecordingResult {
                wav_path: Some(wav_path),
                canceled: false,
            },
            &cancellation,
            false,
            upload_mode,
        )
        .await;
        self.clear_active_request();
    }

    fn save_completed_recording_for_retry(&self, result: &RecordingResult) {
        if result.canceled {
            return;
        }

        let upload_mode = {
            let mut inner = self.inner.lock();
            if !inner.retry_buffer_enabled {
                return;
            }
            // A completed recording always supersedes the old retry task. If it cannot be kept
            // within the limit, retry is unavailable until another recording completes.
            inner.retry_recording = None;
            retry_upload_mode(&inner.config, &inner.audio_client)
        };

        let recording = result.wav_path.as_deref().and_then(load_retry_recording);
        let mut inner = self.inner.lock();
        if !self.is_stopped() && inner.retry_buffer_enabled {
            inner.retry_recording = recording.map(|original_wav| {
                Arc::new(RetryRecording {
                    original_wav,
                    upload_mode,
                })
            });
        }
    }

    /// Records the first valid plan for the currently retryable recording.
    /// This happens before segment export so an export failure still leaves a
    /// stable source-frame partition for Retry. Later retries export exactly
    /// these ranges even if the user changes segmented-upload settings.
    fn save_segment_plan_for_retry(&self, plan: SegmentPlan) {
        let mut inner = self.inner.lock();
        let Some(recording) = inner.retry_recording.as_ref() else {
            return;
        };
        let mut replacement = (**recording).clone();
        let RetryUploadMode::Segmented {
            plan: stored_plan, ..
        } = &mut replacement.upload_mode
        else {
            return;
        };
        if stored_plan.is_some() {
            return;
        }
        *stored_plan = Some(Arc::new(plan));
        inner.retry_recording = Some(Arc::new(replacement));
    }

    fn set_retryable_error<E: std::fmt::Display + ?Sized>(&self, message: &str, error: &E) {
        let state = if self.has_retryable_recording() {
            State::Idle
        } else {
            State::Error
        };
        self.set_state(state, message, Some(error));
    }

    async fn transcribe_recording(
        &self,
        result: RecordingResult,
        cancellation: &CancellationToken,
        cache_attempt: bool,
    ) {
        let upload_mode = {
            let inner = self.inner.lock();
            retry_upload_mode(&inner.config, &inner.audio_client)
        };
        self.transcribe_recording_with_upload_mode(
            result,
            cancellation,
            cache_attempt,
            upload_mode,
        )
        .await;
    }

    async fn transcribe_recording_with_upload_mode(
        &self,
        result: RecordingResult,
        cancellation: &CancellationToken,
        cache_attempt: bool,
        upload_mode: RetryUploadMode,
    ) {
        let Some(wav_path) = result.wav_path else {
            self.set_retryable_error("Recording failed", &RuntimeError::MissingWav);
            return;
        };
        let (config, client, temp_dir) = {
            let inner = self.inner.lock();
            (
                inner.config.clone(),
                inner.audio_client.clone(),
                inner.temp_dir.clone(),
            )
        };
        if client.is_realtime_workflow() {
            self.transcribe_realtime_recording(
                &wav_path,
                &config,
                &client,
                cancellation,
                cache_attempt,
            )
            .await;
            return;
        }
        if let RetryUploadMode::Segmented {
            plan,
            max_segment_seconds,
            min_pause_ms,
            max_concurrency,
        } = upload_mode
        {
            self.transcribe_segmented_recording(
                &wav_path,
                &config,
                &client,
                &temp_dir,
                cancellation,
                cache_attempt,
                plan,
                max_segment_seconds,
                min_pause_ms,
                max_concurrency,
            )
            .await;
            return;
        }
        let output_path = cache::recording_output_path(&wav_path, &config.container);
        if let Err(error) = prepare_audio_for_upload(
            self.converter.as_ref(),
            cancellation,
            &config,
            &wav_path,
            &output_path,
            config.sampling_rate,
        )
        .await
        {
            clean_up_recording_attempt(&config, false, &wav_path, &output_path, false, &[]);
            if !self.is_stopped() {
                if cancellation.is_cancelled() || matches!(error, ConvertError::Canceled) {
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                } else if matches!(error, ConvertError::NoSpeech) {
                    self.inner.lock().retry_recording = None;
                    self.set_state(State::Idle, "No speech detected", None::<&RuntimeError>);
                } else {
                    self.set_retryable_error("FFmpeg conversion failed", &error);
                }
            }
            return;
        }
        if self.is_stopped() {
            clean_up_recording_attempt(&config, cache_attempt, &wav_path, &output_path, false, &[]);
            return;
        }
        if cancellation.is_cancelled() {
            clean_up_recording_attempt(&config, cache_attempt, &wav_path, &output_path, false, &[]);
            self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
            return;
        }

        let transcription = client
            .transcribe_with_retry_prepare(cancellation, &output_path, || async {
                if !config.enable_vad {
                    return Ok(());
                }
                prepare_audio_for_upload(
                    self.converter.as_ref(),
                    cancellation,
                    &config,
                    &wav_path,
                    &output_path,
                    config.sampling_rate,
                )
                .await
                .map_err(AsrError::from)
            })
            .await;
        match transcription {
            Ok(transcription) => {
                if self.is_stopped() {
                    clean_up_recording_attempt(
                        &config,
                        cache_attempt,
                        &wav_path,
                        &output_path,
                        true,
                        &transcription.raw_response,
                    );
                    return;
                }
                if cancellation.is_cancelled() {
                    clean_up_recording_attempt(
                        &config,
                        cache_attempt,
                        &wav_path,
                        &output_path,
                        false,
                        &[],
                    );
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    return;
                }
                if transcription.text.is_empty() {
                    clean_up_recording_attempt(
                        &config,
                        cache_attempt,
                        &wav_path,
                        &output_path,
                        true,
                        &transcription.raw_response,
                    );
                    self.set_state(State::Idle, "Empty result from ASR", None::<&RuntimeError>);
                    return;
                }
                let paste = text_input::send_text(&transcription.text, cancellation, &config).await;
                clean_up_recording_attempt(
                    &config,
                    cache_attempt,
                    &wav_path,
                    &output_path,
                    true,
                    &transcription.raw_response,
                );
                match paste {
                    Ok(()) if !self.is_stopped() => self.set_state(
                        State::Idle,
                        if config.use_sendinput {
                            "Text input sent"
                        } else {
                            "Transcription pasted"
                        },
                        None::<&RuntimeError>,
                    ),
                    Err(error) if error.canceled_before_output() && !self.is_stopped() => {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>)
                    }
                    Err(error) if !self.is_stopped() => {
                        let message = error.status();
                        self.set_retryable_error(message, &error);
                    }
                    _ => {}
                }
            }
            Err(error) if error.is_canceled() => {
                clean_up_recording_attempt(
                    &config,
                    cache_attempt,
                    &wav_path,
                    &output_path,
                    false,
                    &[],
                );
                if !self.is_stopped() {
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                }
            }
            Err(error) => {
                if cancellation.is_cancelled() {
                    clean_up_recording_attempt(
                        &config,
                        cache_attempt,
                        &wav_path,
                        &output_path,
                        false,
                        &[],
                    );
                    if !self.is_stopped() {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    }
                    return;
                }
                let raw = error.last_response().to_vec();
                if config.request_failed_notification
                    && error.is_retry_exhausted()
                    && let Err(paste_error) =
                        text_input::send_text("[request failed]", cancellation, &config).await
                    && !self.is_stopped()
                    && !cancellation.is_cancelled()
                {
                    eprintln!("[text input] failed: {paste_error}");
                }
                clean_up_recording_attempt(
                    &config,
                    cache_attempt,
                    &wav_path,
                    &output_path,
                    error.is_text_extraction_error(),
                    &raw,
                );
                if !self.is_stopped() {
                    if cancellation.is_cancelled() {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    } else {
                        self.set_retryable_error("Upload failed", &error);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn transcribe_segmented_recording(
        &self,
        wav_path: &Path,
        config: &Config,
        client: &AudioApiClient,
        temporary_root: &Path,
        cancellation: &CancellationToken,
        cache_attempt: bool,
        frozen_plan: Option<Arc<SegmentPlan>>,
        max_segment_seconds: u32,
        min_pause_ms: u32,
        max_concurrency: u32,
    ) {
        let mut preparation_config = config.clone();
        // This function is reached only through the frozen Segmented retry
        // mode or a currently enabled new recording. Keep that mode explicit
        // even if the user has turned the setting off since a failed retry.
        preparation_config.enable_segmented_upload = true;
        preparation_config.max_upload_segment_seconds = max_segment_seconds;
        preparation_config.min_upload_pause_ms = min_pause_ms;
        preparation_config.max_upload_concurrency = max_concurrency;
        self.set_state(
            State::Uploading,
            "Preparing segmented upload",
            None::<&RuntimeError>,
        );
        let save_plan = |plan: &SegmentPlan| self.save_segment_plan_for_retry(plan.clone());
        let prepared = match prepare_segmented_upload(
            self.converter.as_ref(),
            cancellation,
            &preparation_config,
            wav_path,
            temporary_root,
            frozen_plan.as_deref(),
            Some(&save_plan),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                clean_up_segmented_recording_attempt(config, cache_attempt, wav_path, None, false);
                if !self.is_stopped() {
                    if cancellation.is_cancelled() || matches!(error, ConvertError::Canceled) {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    } else if matches!(error, ConvertError::NoSpeech) {
                        self.inner.lock().retry_recording = None;
                        self.set_state(State::Idle, "No speech detected", None::<&RuntimeError>);
                    } else {
                        self.set_retryable_error("FFmpeg conversion failed", &error);
                    }
                }
                return;
            }
        };
        if self.is_stopped() {
            clean_up_segmented_recording_attempt(
                config,
                cache_attempt,
                wav_path,
                Some(&prepared),
                false,
            );
            return;
        }
        if cancellation.is_cancelled() {
            clean_up_segmented_recording_attempt(
                config,
                cache_attempt,
                wav_path,
                Some(&prepared),
                false,
            );
            self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
            return;
        }
        let max_concurrency = match segment_concurrency(max_concurrency) {
            Ok(value) => value,
            Err(error) => {
                clean_up_segmented_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    Some(&prepared),
                    false,
                );
                if !self.is_stopped() {
                    self.set_retryable_error("Segmented upload preparation failed", &error);
                }
                return;
            }
        };
        let upload_message = format!("Uploading {} audio segments", prepared.files.len());
        self.set_state(State::Uploading, &upload_message, None::<&RuntimeError>);
        let transcription = client
            .transcribe_segments(cancellation, &prepared.files, max_concurrency)
            .await;
        match transcription {
            Ok(transcription) => {
                if self.is_stopped() {
                    clean_up_segmented_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        Some(&prepared),
                        false,
                    );
                    return;
                }
                if cancellation.is_cancelled() {
                    clean_up_segmented_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        Some(&prepared),
                        false,
                    );
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    return;
                }
                if transcription.text.is_empty() {
                    clean_up_segmented_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        Some(&prepared),
                        false,
                    );
                    self.set_state(State::Idle, "Empty result from ASR", None::<&RuntimeError>);
                    return;
                }
                let paste = text_input::send_text(&transcription.text, cancellation, config).await;
                // An upload is only a cacheable segmented attempt once its
                // single, complete result has actually been delivered.  In
                // particular, retain neither the original WAV nor generated
                // segments when output is canceled or fails: Retry restores
                // and re-exports the authoritative in-memory WAV instead.
                let delivered = paste.is_ok() && !self.is_stopped() && !cancellation.is_cancelled();
                clean_up_segmented_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    Some(&prepared),
                    delivered,
                );
                match paste {
                    Ok(()) if !self.is_stopped() => self.set_state(
                        State::Idle,
                        if config.use_sendinput {
                            "Text input sent"
                        } else {
                            "Transcription pasted"
                        },
                        None::<&RuntimeError>,
                    ),
                    Err(error) if error.canceled_before_output() && !self.is_stopped() => {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>)
                    }
                    Err(error) if !self.is_stopped() => {
                        let message = error.status();
                        self.set_retryable_error(message, &error);
                    }
                    _ => {}
                }
            }
            Err(error) if error.is_canceled() => {
                clean_up_segmented_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    Some(&prepared),
                    false,
                );
                if !self.is_stopped() {
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                }
            }
            Err(error) => {
                if cancellation.is_cancelled() {
                    clean_up_segmented_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        Some(&prepared),
                        false,
                    );
                    if !self.is_stopped() {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    }
                    return;
                }
                if config.request_failed_notification
                    && error.is_retry_exhausted()
                    && let Err(paste_error) =
                        text_input::send_text("[request failed]", cancellation, config).await
                    && !self.is_stopped()
                    && !cancellation.is_cancelled()
                {
                    eprintln!("[text input] failed: {paste_error}");
                }
                clean_up_segmented_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    Some(&prepared),
                    false,
                );
                if !self.is_stopped() {
                    if cancellation.is_cancelled() {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    } else {
                        self.set_retryable_error("Upload failed", &error);
                    }
                }
            }
        }
    }

    async fn transcribe_realtime_recording(
        &self,
        wav_path: &Path,
        config: &Config,
        client: &AudioApiClient,
        cancellation: &CancellationToken,
        cache_attempt: bool,
    ) {
        let cache_path = cache::recording_output_path(wav_path, "wav");
        let mut live = self.take_live_realtime();
        let had_live_session = live.active.is_some() || !live.committed.is_empty();
        if live.failed {
            if let Some(session) = live.active.take() {
                session.cancellation.cancel();
                session.task.abort();
            }
            live.committed.clear();
        }
        let transcription = if !live.failed && had_live_session {
            self.set_state(
                State::Uploading,
                "Finalizing realtime ASR",
                None::<&RuntimeError>,
            );
            if let Some(session) = live.active.take() {
                let live_cancellation = session.cancellation;
                let mut task = session.task;
                let live_result = tokio::select! {
                    _ = cancellation.cancelled() => {
                        live_cancellation.cancel();
                        task.abort();
                        clean_up_recording_attempt(config, cache_attempt, wav_path, &cache_path, false, &[]);
                        if !self.is_stopped() {
                            self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                        }
                        return;
                    }
                    result = &mut task => result,
                };
                match live_result {
                    Ok(Ok(transcription)) => {
                        live.committed.push(transcription);
                    }
                    Ok(Err(_)) | Err(_) => {
                        // A live websocket failure never affects the local
                        // WAV. Discard every committed segment and replay the
                        // complete recording through a fresh session.
                        live.failed = true;
                        live.committed.clear();
                    }
                }
            }
            if !live.failed {
                let mut text = String::new();
                let mut raw_response = Vec::new();
                for transcription in live.committed {
                    text.push_str(&transcription.text);
                    if raw_response
                        .len()
                        .saturating_add(transcription.raw_response.len())
                        <= 32 * 1024 * 1024
                    {
                        raw_response.extend_from_slice(&transcription.raw_response);
                    }
                }
                Ok(Transcription { text, raw_response })
            } else {
                self.set_state(
                    State::Uploading,
                    "Replaying audio for transcription",
                    None::<&RuntimeError>,
                );
                client.transcribe(cancellation, wav_path).await
            }
        } else {
            self.set_state(
                State::Uploading,
                "Replaying audio for transcription",
                None::<&RuntimeError>,
            );
            client.transcribe(cancellation, wav_path).await
        };

        match transcription {
            Ok(transcription) => {
                if self.is_stopped() {
                    clean_up_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        &cache_path,
                        true,
                        &transcription.raw_response,
                    );
                    return;
                }
                if cancellation.is_cancelled() {
                    clean_up_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        &cache_path,
                        false,
                        &[],
                    );
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                    return;
                }
                if transcription.text.is_empty() {
                    clean_up_recording_attempt(
                        config,
                        cache_attempt,
                        wav_path,
                        &cache_path,
                        true,
                        &transcription.raw_response,
                    );
                    self.set_state(State::Idle, "Empty result from ASR", None::<&RuntimeError>);
                    return;
                }
                let paste = text_input::send_text(&transcription.text, cancellation, config).await;
                clean_up_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    &cache_path,
                    true,
                    &transcription.raw_response,
                );
                match paste {
                    Ok(()) if !self.is_stopped() => self.set_state(
                        State::Idle,
                        if config.use_sendinput {
                            "Text input sent"
                        } else {
                            "Transcription pasted"
                        },
                        None::<&RuntimeError>,
                    ),
                    Err(error) if error.canceled_before_output() && !self.is_stopped() => {
                        self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>)
                    }
                    Err(error) if !self.is_stopped() => {
                        self.set_retryable_error(error.status(), &error);
                    }
                    _ => {}
                }
            }
            Err(error) if error.is_canceled() || cancellation.is_cancelled() => {
                clean_up_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    &cache_path,
                    false,
                    &[],
                );
                if !self.is_stopped() {
                    self.set_state(State::Idle, "Request canceled", None::<&RuntimeError>);
                }
            }
            Err(error) => {
                let raw = error.last_response().to_vec();
                clean_up_recording_attempt(
                    config,
                    cache_attempt,
                    wav_path,
                    &cache_path,
                    false,
                    &raw,
                );
                if !self.is_stopped() {
                    self.set_retryable_error("Realtime transcription failed", &error);
                }
            }
        }
    }

    fn set_state<E: std::fmt::Display + ?Sized>(
        &self,
        state: State,
        message: &str,
        error: Option<&E>,
    ) {
        if self.is_stopped() {
            return;
        }
        let (event, handler) = {
            let mut inner = self.inner.lock();
            inner.event = Event {
                state,
                message: message.into(),
                error: error.map(ToString::to_string).unwrap_or_default(),
                retry_available: inner.retry_buffer_enabled && inner.retry_recording.is_some(),
            };
            (inner.event.clone(), inner.event_handler.clone())
        };
        if let Some(handler) = handler
            && !self.is_stopped()
        {
            handler(event);
        }
    }
}

pub async fn run_file_mode(
    config: Config,
    converter: Arc<dyn AudioConverter>,
    input_path: &Path,
    output_path: Option<&Path>,
) -> Result<PathBuf, RuntimeError> {
    run_file_mode_with_cancellation(
        config,
        converter,
        input_path,
        output_path,
        CancellationToken::new(),
    )
    .await
}

/// Runs the formal audio API test workflow against a caller-owned source WAV.
///
/// Settings UI code uses this high-level entry point so segmented tests share
/// the same analysis, export, bounded batch dispatch, and realtime exclusion
/// as actual transcription.  It never deletes `input_path`; only generated
/// scratch files are removed before returning.
pub async fn test_audio_api_with_source(
    config: Config,
    converter: &dyn AudioConverter,
    input_path: &Path,
    source_rate: i32,
    scratch_root: &Path,
    cancellation: CancellationToken,
) -> Result<(), RuntimeError> {
    config.validate()?;
    let client = AudioApiClient::new(config.clone())?;
    if config.enable_segmented_upload && !client.is_realtime_workflow() {
        let prepared = prepare_segmented_upload(
            converter,
            &cancellation,
            &config,
            input_path,
            scratch_root,
            None,
            None,
        )
        .await?;
        let result = match segment_concurrency(config.max_upload_concurrency) {
            Ok(max_concurrency) => client
                .test_connection_segments_cancellable(
                    &cancellation,
                    &prepared.files,
                    max_concurrency,
                )
                .await
                .map_err(RuntimeError::from),
            Err(error) => Err(error.into()),
        };
        prepared.remove_files();
        return result;
    }

    let preparation_config =
        file_mode_preparation_config(&config, client.realtime_audio_stream().as_ref());
    let temporary =
        cache::temporary_output_path(scratch_root, &preparation_config.container_extension());
    let result = async {
        prepare_audio_for_upload(
            converter,
            &cancellation,
            &preparation_config,
            input_path,
            &temporary,
            source_rate,
        )
        .await?;
        client
            .test_connection_cancellable(&cancellation, &temporary)
            .await
            .map_err(RuntimeError::from)
    }
    .await;
    let _ = std::fs::remove_file(&temporary);
    result
}

pub async fn run_file_mode_with_cancellation(
    mut config: Config,
    converter: Arc<dyn AudioConverter>,
    input_path: &Path,
    output_path: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<PathBuf, RuntimeError> {
    config.validate()?;
    let temp_dir = cache::initialize_cache_dir(&mut config);
    std::fs::metadata(input_path).map_err(|source| RuntimeError::InputFile {
        path: input_path.to_path_buf(),
        source,
    })?;
    // `--file` remains caller-owned even if it was placed in CACHE_DIR and
    // happens to share the RecordTemp_ prefix used by stale-artifact cleanup.
    // Verify it first, then protect its resolved identity during cleanup.
    cache::cleanup_old_temp_items_excluding(&temp_dir, Some(input_path));
    let client = AudioApiClient::new(config.clone())?;
    if config.enable_segmented_upload && !client.is_realtime_workflow() {
        return run_segmented_file_mode(
            &config,
            converter.as_ref(),
            input_path,
            output_path,
            &temp_dir,
            &client,
            &cancellation,
        )
        .await;
    }
    // Recorded realtime replay currently consumes a WAV and converts it into
    // the workflow-declared PCM stream itself. Keep that preparation entirely
    // within the normal --file path so all recognition modes still enter
    // AudioApiClient through the same public interface.
    let realtime_stream = client.realtime_audio_stream();
    let preparation_config = file_mode_preparation_config(&config, realtime_stream.as_ref());
    let temporary =
        cache::temporary_output_path(&temp_dir, &preparation_config.container_extension());
    if let Err(error) = prepare_audio_for_upload(
        converter.as_ref(),
        &cancellation,
        &preparation_config,
        input_path,
        &temporary,
        preparation_config.sampling_rate,
    )
    .await
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    let transcription = match client
        .transcribe_with_retry_prepare(&cancellation, &temporary, || async {
            if !preparation_config.enable_vad {
                return Ok(());
            }
            prepare_audio_for_upload(
                converter.as_ref(),
                &cancellation,
                &preparation_config,
                input_path,
                &temporary,
                preparation_config.sampling_rate,
            )
            .await
            .map_err(AsrError::from)
        })
        .await
    {
        Ok(transcription) => transcription,
        Err(error) => {
            let raw = error.last_response().to_vec();
            cache::handle_cache(
                &config,
                None,
                Some(&temporary),
                error.is_text_extraction_error(),
                &raw,
            );
            return Err(error.into());
        }
    };
    let output = output_path.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from(".").join(format!(
            "{}.txt",
            input_path
                .file_stem()
                .map(|stem| stem.to_string_lossy())
                .unwrap_or_default()
        ))
    });
    finish_file_mode_output(&config, &temporary, output, transcription)
}

async fn run_segmented_file_mode(
    config: &Config,
    converter: &dyn AudioConverter,
    input_path: &Path,
    output_path: Option<&Path>,
    temporary_root: &Path,
    client: &AudioApiClient,
    cancellation: &CancellationToken,
) -> Result<PathBuf, RuntimeError> {
    let prepared = prepare_segmented_upload(
        converter,
        cancellation,
        config,
        input_path,
        temporary_root,
        None,
        None,
    )
    .await?;
    let max_concurrency = match segment_concurrency(config.max_upload_concurrency) {
        Ok(value) => value,
        Err(error) => {
            prepared.remove_files();
            return Err(error.into());
        }
    };
    let transcription = match client
        .transcribe_segments(cancellation, &prepared.files, max_concurrency)
        .await
    {
        Ok(transcription) => transcription,
        Err(error) => {
            prepared.remove_files();
            return Err(error.into());
        }
    };
    if cancellation.is_cancelled() {
        prepared.remove_files();
        return Err(ConvertError::Canceled.into());
    }
    let output = output_path.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from(".").join(format!(
            "{}.txt",
            input_path
                .file_stem()
                .map(|stem| stem.to_string_lossy())
                .unwrap_or_default()
        ))
    });
    if let Err(source) = std::fs::write(&output, transcription.text) {
        prepared.remove_files();
        return Err(RuntimeError::OutputFile {
            path: output,
            source,
        });
    }
    cache::handle_segmented_cache(config, None, &prepared.directory, true);
    Ok(output)
}

/// Chooses the file preparation format before the unified AudioApiClient is
/// invoked. HTTP-style and Legacy workflows retain their existing settings.
/// A realtime replay is deliberately prepared as a PCM WAV whose shape comes
/// from the validated workflow: `RecordedReplaySource` can then use the same
/// decode/chunk/pacing path as retry and live-failure replay.
fn file_mode_preparation_config(
    config: &Config,
    realtime_stream: Option<&crate::advanced_audio::schema::RealtimeAudioStream>,
) -> Config {
    let Some(stream) = realtime_stream else {
        return config.clone();
    };

    let mut preparation = config.clone();
    preparation.codecs = "pcm".into();
    preparation.container = "wav".into();
    preparation.channels = i32::from(stream.channels);
    preparation.sampling_rate = stream.sample_rate as i32;
    preparation.sampling_rate_depth = 16;
    preparation
}

fn finish_file_mode_output(
    config: &Config,
    temporary: &Path,
    output: PathBuf,
    transcription: Transcription,
) -> Result<PathBuf, RuntimeError> {
    if let Err(source) = std::fs::write(&output, transcription.text) {
        cache::handle_cache(
            config,
            None,
            Some(temporary),
            true,
            &transcription.raw_response,
        );
        return Err(RuntimeError::OutputFile {
            path: output,
            source,
        });
    }
    cache::handle_cache(
        config,
        None,
        Some(temporary),
        true,
        &transcription.raw_response,
    );
    Ok(output)
}

fn discard_recording(result: &RecordingResult) {
    if let Some(path) = &result.wav_path {
        let _ = std::fs::remove_file(path);
    }
}

fn retry_upload_mode(config: &Config, client: &AudioApiClient) -> RetryUploadMode {
    if config.enable_segmented_upload && !client.is_realtime_workflow() {
        RetryUploadMode::Segmented {
            plan: None,
            max_segment_seconds: config.max_upload_segment_seconds,
            min_pause_ms: config.min_upload_pause_ms,
            max_concurrency: config.max_upload_concurrency,
        }
    } else {
        RetryUploadMode::Single
    }
}

fn segment_concurrency(value: u32) -> Result<usize, ConvertError> {
    let value = usize::try_from(value).map_err(|_| ConvertError::Failed {
        message: "segmented upload concurrency is too large for this platform".into(),
    })?;
    if value == 0 {
        return Err(ConvertError::Failed {
            message: "segmented upload concurrency must be greater than zero".into(),
        });
    }
    Ok(value)
}

fn clean_up_segmented_recording_attempt(
    config: &Config,
    cache_attempt: bool,
    wav_path: &Path,
    prepared: Option<&PreparedSegmentedUpload>,
    upload_succeeded: bool,
) {
    let Some(prepared) = prepared else {
        let _ = std::fs::remove_file(wav_path);
        return;
    };
    if cache_attempt {
        cache::handle_segmented_cache(
            config,
            Some(wav_path),
            &prepared.directory,
            upload_succeeded,
        );
    } else {
        let _ = std::fs::remove_file(wav_path);
        prepared.remove_files();
    }
}

fn load_retry_recording(path: &Path) -> Option<Arc<Vec<u8>>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_RETRY_AUDIO_BYTES {
        return None;
    }

    let capacity = usize::try_from(metadata.len()).ok()?;
    let mut recording = Vec::with_capacity(capacity);
    let file = std::fs::File::open(path).ok()?;
    file.take(MAX_RETRY_AUDIO_BYTES.saturating_add(1))
        .read_to_end(&mut recording)
        .ok()?;
    if recording.len() as u64 > MAX_RETRY_AUDIO_BYTES {
        return None;
    }
    Some(Arc::new(recording))
}

fn clean_up_recording_attempt(
    config: &Config,
    cache_attempt: bool,
    wav_path: &Path,
    output_path: &Path,
    upload_succeeded: bool,
    response: &[u8],
) {
    if cache_attempt {
        cache::handle_cache(
            config,
            Some(wav_path),
            Some(output_path),
            upload_succeeded,
            response,
        );
        return;
    }

    let _ = std::fs::remove_file(wav_path);
    let _ = std::fs::remove_file(output_path);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use futures_util::{SinkExt, StreamExt, future::join_all};
    use parking_lot::Mutex as ParkingMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Barrier;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::protocol::Message;

    use super::*;
    use crate::advanced_audio::schema::{
        AdvancedAudioConfig, AdvancedAudioWorkflow, AdvancedRecognition, AudioDelivery, AudioSpec,
        PauseBehavior, RealtimeAudioMessage, RealtimeAudioStream, RealtimeCompletion,
        RealtimeConnect, RealtimeMessage, RealtimePacing, RealtimeTransport, RealtimeWorkflow,
        SignerConfig, StreamAction, StreamRule, WorkflowSchemaVersion,
    };
    use crate::audio_devices::{CaptureFormat, test_capture_format};
    use crate::converter::{SegmentAnalysis, SegmentExportRequest, SourceFrameInterval};
    use crate::recorder::{AudioBackend, AudioStream};

    fn retry_recording_for_test(bytes: &[u8]) -> Arc<RetryRecording> {
        Arc::new(RetryRecording {
            original_wav: Arc::new(bytes.to_vec()),
            upload_mode: RetryUploadMode::Single,
        })
    }

    struct NoopConverter;

    #[tokio::test]
    async fn pause_finalizes_live_segment_before_resume() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let recorder = runtime.inner.lock().recorder.clone();
        let cancellation = CancellationToken::new();
        let task: JoinHandle<Result<Transcription, AudioApiError>> = tokio::spawn(async {
            Ok(Transcription {
                text: "first segment".into(),
                raw_response: b"first".to_vec(),
            })
        });
        runtime.inner.lock().live_realtime = Some(LiveRealtimeSession { cancellation, task });

        runtime.finalize_live_realtime_segment(&recorder).await;

        let live = runtime.take_live_realtime();
        assert!(!live.failed);
        assert!(live.active.is_none());
        assert_eq!(live.committed.len(), 1);
        assert_eq!(live.committed[0].text, "first segment");
    }

    #[tokio::test]
    async fn failed_live_segment_discards_committed_segments_for_full_replay() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let recorder = runtime.inner.lock().recorder.clone();
        let cancellation = CancellationToken::new();
        let task: JoinHandle<Result<Transcription, AudioApiError>> =
            tokio::spawn(async { Err(AudioApiError::LiveRealtimeUnavailable) });
        {
            let mut inner = runtime.inner.lock();
            inner.completed_live_transcriptions.push(Transcription {
                text: "must be discarded".into(),
                raw_response: b"partial".to_vec(),
            });
            inner.live_realtime = Some(LiveRealtimeSession { cancellation, task });
        }

        runtime.finalize_live_realtime_segment(&recorder).await;

        let live = runtime.take_live_realtime();
        assert!(live.failed);
        assert!(live.active.is_none());
        assert!(live.committed.is_empty());
    }

    #[tokio::test]
    async fn cancel_bypasses_action_lock_during_live_finalization_only() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let recorder = runtime.inner.lock().recorder.clone();
        runtime.set_state(State::Recording, "Recording started", None::<&RuntimeError>);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let task: JoinHandle<Result<Transcription, AudioApiError>> = tokio::spawn(async move {
            let _ = started_sender.send(());
            task_cancellation.cancelled().await;
            // A result that races with cancellation must not become an
            // incomplete committed segment.
            Ok(Transcription {
                text: "must not commit".into(),
                raw_response: Vec::new(),
            })
        });
        runtime.inner.lock().live_realtime = Some(LiveRealtimeSession { cancellation, task });

        let finalizing_runtime = runtime.clone();
        let finalizing = tokio::spawn(async move {
            let _guard = finalizing_runtime.action_lock.clone().lock_owned().await;
            finalizing_runtime
                .finalize_live_realtime_segment(&recorder)
                .await;
        });
        started_receiver.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if runtime
                    .inner
                    .lock()
                    .live_finalization_cancellation
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pause should expose the live finalization cancellation token");

        assert!(runtime.try_cancel_or_retry());
        tokio::time::timeout(Duration::from_secs(1), finalizing)
            .await
            .expect("cancel should interrupt live finalization")
            .unwrap();

        let live = runtime.take_live_realtime();
        assert!(live.failed);
        assert!(live.active.is_none());
        assert!(live.committed.is_empty());
    }

    #[tokio::test]
    async fn recording_does_not_expose_live_finalization_cancel_path() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        runtime.set_state(State::Recording, "Recording started", None::<&RuntimeError>);
        let _guard = runtime.action_lock.clone().lock_owned().await;

        assert!(!runtime.try_cancel_or_retry());
    }

    struct NoSpeechConverter;
    #[async_trait]
    impl AudioConverter for NoSpeechConverter {
        async fn convert(
            &self,
            _: &CancellationToken,
            _: &Config,
            _: &Path,
            _: &Path,
            _: i32,
        ) -> Result<(), ConvertError> {
            Err(ConvertError::NoSpeech)
        }
    }

    #[tokio::test]
    async fn no_speech_clears_retry_and_returns_idle_without_request() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("original.wav");
        std::fs::write(&input, b"original recording").unwrap();
        let runtime = Runtime::new(
            Config {
                enable_vad: true,
                ..Default::default()
            },
            Arc::new(NoSpeechConverter),
        )
        .unwrap();
        runtime.enable_retry_buffer();
        let result = RecordingResult {
            wav_path: Some(input.clone()),
            canceled: false,
        };
        runtime.save_completed_recording_for_retry(&result);
        assert!(runtime.has_retryable_recording());
        runtime
            .transcribe_recording(result, &CancellationToken::new(), true)
            .await;
        let event = runtime.snapshot();
        assert_eq!(event.state, State::Idle);
        assert_eq!(event.message, "No speech detected");
        assert!(event.error.is_empty());
        assert!(!runtime.has_retryable_recording());
        assert!(!input.exists());
    }

    #[async_trait]
    impl AudioConverter for NoopConverter {
        async fn convert(
            &self,
            _cancellation: &CancellationToken,
            _config: &Config,
            _input: &Path,
            _output: &Path,
            _source_rate: i32,
        ) -> Result<(), ConvertError> {
            Ok(())
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SegmentExportCall {
        segmented_upload_enabled: bool,
        max_segment_seconds: u32,
        min_pause_ms: u32,
        max_concurrency: u32,
        enable_vad: bool,
        intervals: Vec<SourceFrameInterval>,
    }

    #[derive(Default)]
    struct SegmentedConverterCalls {
        analyses: usize,
        analyzed_min_pause_ms: Vec<u32>,
        exports: Vec<SegmentExportCall>,
    }

    #[derive(Clone, Copy)]
    enum SegmentExportOutcome {
        Succeed,
        Fail,
        Canceled,
    }

    struct InstrumentedSegmentConverter {
        calls: Arc<ParkingMutex<SegmentedConverterCalls>>,
        outcome: SegmentExportOutcome,
    }

    #[async_trait]
    impl AudioConverter for InstrumentedSegmentConverter {
        async fn convert(
            &self,
            _: &CancellationToken,
            _: &Config,
            _: &Path,
            _: &Path,
            _: i32,
        ) -> Result<(), ConvertError> {
            panic!("segmented Runtime paths must not use single-file conversion")
        }

        async fn analyze_segments(
            &self,
            _: &CancellationToken,
            _: &Config,
            _: &Path,
            min_pause_ms: u32,
        ) -> Result<SegmentAnalysis, ConvertError> {
            let mut calls = self.calls.lock();
            calls.analyses += 1;
            calls.analyzed_min_pause_ms.push(min_pause_ms);
            Ok(SegmentAnalysis {
                source_rate: 10,
                source_frames: 25,
                silence_intervals: vec![SourceFrameInterval {
                    start_frame: 8,
                    end_frame: 10,
                }],
            })
        }

        async fn export_segments(
            &self,
            _: &CancellationToken,
            request: SegmentExportRequest<'_>,
        ) -> Result<(), ConvertError> {
            self.calls.lock().exports.push(SegmentExportCall {
                segmented_upload_enabled: request.config.enable_segmented_upload,
                max_segment_seconds: request.config.max_upload_segment_seconds,
                min_pause_ms: request.config.min_upload_pause_ms,
                max_concurrency: request.config.max_upload_concurrency,
                enable_vad: request.config.enable_vad,
                intervals: request.intervals.to_vec(),
            });
            match self.outcome {
                SegmentExportOutcome::Succeed => {
                    for output in request.outputs {
                        std::fs::write(output, b"segment").map_err(|error| {
                            ConvertError::Failed {
                                message: error.to_string(),
                            }
                        })?;
                    }
                    Ok(())
                }
                SegmentExportOutcome::Fail => Err(ConvertError::Failed {
                    message: "simulated segment export failure".into(),
                }),
                SegmentExportOutcome::Canceled => {
                    if let Some(output) = request.outputs.first() {
                        std::fs::write(output, b"partial segment").map_err(|error| {
                            ConvertError::Failed {
                                message: error.to_string(),
                            }
                        })?;
                    }
                    Err(ConvertError::Canceled)
                }
            }
        }
    }

    fn segmented_runtime_config(cache_dir: &Path, api_endpoint: String) -> Config {
        Config {
            api_endpoint,
            cache_dir: cache_dir.to_string_lossy().into_owned(),
            codecs: "pcm".into(),
            container: "wav".into(),
            enable_vad: true,
            enable_segmented_upload: true,
            max_upload_segment_seconds: 1,
            min_upload_pause_ms: 700,
            max_upload_concurrency: 1,
            max_retry: 1,
            retry_base_delay: 0.0,
            ..Config::default()
        }
    }

    async fn read_complete_http_request(stream: &mut tokio::net::TcpStream) {
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0, "client closed before sending a complete request");
            request.extend_from_slice(&buffer[..count]);
            let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..headers_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse::<usize>().ok());
            if content_length.is_some_and(|length| request.len() >= headers_end + 4 + length)
                || request.ends_with(b"0\r\n\r\n")
            {
                return;
            }
        }
    }

    async fn legacy_segment_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_complete_http_request(&mut stream).await;
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (endpoint, worker)
    }

    async fn legacy_concurrency_failure_server(
        expected_concurrency: usize,
        batches: usize,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = tokio::spawn(async move {
            for _ in 0..batches {
                let mut streams = Vec::with_capacity(expected_concurrency);
                for _ in 0..expected_concurrency {
                    streams.push(listener.accept().await.unwrap().0);
                }
                join_all(streams.iter_mut().map(read_complete_http_request)).await;
                for stream in &mut streams {
                    let body = r#"{"error":"simulated failure"}"#;
                    let response = format!(
                        "HTTP/1.1 500 Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
        });
        (endpoint, worker)
    }

    #[tokio::test]
    async fn segmented_export_failure_freezes_retry_plan_and_settings_before_export() {
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(ParkingMutex::new(SegmentedConverterCalls::default()));
        let converter = Arc::new(InstrumentedSegmentConverter {
            calls: calls.clone(),
            outcome: SegmentExportOutcome::Fail,
        });
        let mut config = segmented_runtime_config(directory.path(), String::new());
        config.max_upload_concurrency = 2;
        let runtime = Runtime::new(config, converter).unwrap();
        runtime.enable_retry_buffer();

        let source = directory.path().join("original.wav");
        std::fs::write(&source, b"authoritative original recording").unwrap();
        let recording = RecordingResult {
            wav_path: Some(source.clone()),
            canceled: false,
        };
        runtime.save_completed_recording_for_retry(&recording);
        runtime
            .transcribe_recording(recording, &CancellationToken::new(), true)
            .await;

        let (plan, max_seconds, min_pause_ms, max_concurrency) = {
            let inner = runtime.inner.lock();
            let recording = inner.retry_recording.as_ref().unwrap();
            match &recording.upload_mode {
                RetryUploadMode::Segmented {
                    plan,
                    max_segment_seconds,
                    min_pause_ms,
                    max_concurrency,
                } => (
                    plan.clone()
                        .expect("export failure must retain the planned boundaries"),
                    *max_segment_seconds,
                    *min_pause_ms,
                    *max_concurrency,
                ),
                RetryUploadMode::Single => {
                    panic!("segmented recording selected the single upload mode")
                }
            }
        };
        assert_eq!(max_seconds, 1);
        assert_eq!(min_pause_ms, 700);
        assert_eq!(max_concurrency, 2);
        assert_eq!(
            plan.segments,
            vec![
                crate::audio_segments::SourceInterval::new(0, 10),
                crate::audio_segments::SourceInterval::new(10, 20),
                crate::audio_segments::SourceInterval::new(20, 25),
            ]
        );
        assert_eq!(runtime.snapshot().state, State::Idle);
        assert!(runtime.has_retryable_recording());
        assert!(!source.exists());

        // Settings are mutable while the retry is idle.  The retry must still
        // use its original segmentation mode, boundaries, and concurrency.
        {
            let mut inner = runtime.inner.lock();
            inner.config.enable_segmented_upload = false;
            inner.config.max_upload_segment_seconds = 9;
            inner.config.min_upload_pause_ms = 1;
            inner.config.max_upload_concurrency = 5;
        }
        runtime.retry_recording_locked().await;

        let calls = calls.lock();
        assert_eq!(calls.analyses, 1, "Retry must not analyze pauses again");
        assert_eq!(calls.analyzed_min_pause_ms, vec![700]);
        assert_eq!(calls.exports.len(), 2);
        assert!(calls.exports.iter().all(|call| {
            call.segmented_upload_enabled
                && call.max_segment_seconds == 1
                && call.min_pause_ms == 700
                && call.max_concurrency == 2
                && !call.enable_vad
                && call.intervals
                    == vec![
                        SourceFrameInterval {
                            start_frame: 0,
                            end_frame: 10,
                        },
                        SourceFrameInterval {
                            start_frame: 10,
                            end_frame: 20,
                        },
                        SourceFrameInterval {
                            start_frame: 20,
                            end_frame: 25,
                        },
                    ]
        }));
        drop(calls);
        assert!(runtime.has_retryable_recording());
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("RecordTemp_"))
        );
    }

    #[tokio::test]
    async fn segmented_retry_dispatches_with_its_frozen_concurrency() {
        let directory = tempfile::tempdir().unwrap();
        let (endpoint, server) = legacy_concurrency_failure_server(2, 2).await;
        let calls = Arc::new(ParkingMutex::new(SegmentedConverterCalls::default()));
        let converter = Arc::new(InstrumentedSegmentConverter {
            calls: calls.clone(),
            outcome: SegmentExportOutcome::Succeed,
        });
        let mut config = segmented_runtime_config(directory.path(), endpoint);
        config.max_upload_concurrency = 2;
        let runtime = Runtime::new(config, converter).unwrap();
        runtime.enable_retry_buffer();

        let source = directory.path().join("original.wav");
        std::fs::write(&source, b"authoritative original recording").unwrap();
        let recording = RecordingResult {
            wav_path: Some(source),
            canceled: false,
        };
        runtime.save_completed_recording_for_retry(&recording);
        tokio::time::timeout(
            Duration::from_secs(2),
            runtime.transcribe_recording(recording, &CancellationToken::new(), true),
        )
        .await
        .expect("initial batch should start its two configured workflows");

        // If Retry accidentally used this new value, the fake server would
        // wait forever for a second request in the second batch.
        {
            let mut inner = runtime.inner.lock();
            inner.config.enable_segmented_upload = false;
            inner.config.max_upload_concurrency = 1;
        }
        tokio::time::timeout(Duration::from_secs(2), runtime.retry_recording_locked())
            .await
            .expect("Retry must retain and dispatch with concurrency two");
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("both bounded batches should finish")
            .unwrap();

        let calls = calls.lock();
        assert_eq!(calls.analyses, 1);
        assert_eq!(calls.exports.len(), 2);
        assert!(calls.exports.iter().all(|call| call.max_concurrency == 2));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn segmented_paste_failure_discards_attempt_artifacts_even_when_cache_is_enabled() {
        let directory = tempfile::tempdir().unwrap();
        let (endpoint, server) = legacy_segment_server(vec![
            (200, r#"{"text":"first"}"#),
            (200, r#"{"text":"second"}"#),
            (200, r#"{"text":"third"}"#),
        ])
        .await;
        let calls = Arc::new(ParkingMutex::new(SegmentedConverterCalls::default()));
        let converter = Arc::new(InstrumentedSegmentConverter {
            calls: calls.clone(),
            outcome: SegmentExportOutcome::Succeed,
        });
        let mut config = segmented_runtime_config(directory.path(), endpoint);
        config.keep_cache = true;
        let runtime = Runtime::new(config, converter).unwrap();
        runtime.enable_retry_buffer();

        let source = directory.path().join("original.wav");
        std::fs::write(&source, b"authoritative original recording").unwrap();
        let recording = RecordingResult {
            wav_path: Some(source.clone()),
            canceled: false,
        };
        runtime.save_completed_recording_for_retry(&recording);
        runtime
            .transcribe_recording(recording, &CancellationToken::new(), true)
            .await;
        server.await.unwrap();

        assert_eq!(runtime.snapshot().state, State::Idle);
        assert_eq!(runtime.snapshot().message, "Paste failed");
        assert!(runtime.has_retryable_recording());
        assert!(!source.exists());
        assert_eq!(calls.lock().exports.len(), 1);
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    !name.starts_with("RecordTemp_") && !name.starts_with("audio-")
                })
        );
    }

    #[tokio::test]
    async fn canceled_segmented_export_removes_partial_attempt_and_keeps_retry_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(ParkingMutex::new(SegmentedConverterCalls::default()));
        let converter = Arc::new(InstrumentedSegmentConverter {
            calls: calls.clone(),
            outcome: SegmentExportOutcome::Canceled,
        });
        let mut config = segmented_runtime_config(directory.path(), String::new());
        config.keep_cache = true;
        let runtime = Runtime::new(config, converter).unwrap();
        runtime.enable_retry_buffer();

        let source = directory.path().join("original.wav");
        std::fs::write(&source, b"authoritative original recording").unwrap();
        let recording = RecordingResult {
            wav_path: Some(source.clone()),
            canceled: false,
        };
        runtime.save_completed_recording_for_retry(&recording);
        runtime
            .transcribe_recording(recording, &CancellationToken::new(), true)
            .await;

        assert_eq!(runtime.snapshot().state, State::Idle);
        assert_eq!(runtime.snapshot().message, "Request canceled");
        assert!(runtime.has_retryable_recording());
        assert_eq!(calls.lock().exports.len(), 1);
        assert!(!source.exists());
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("RecordTemp_"))
        );
    }

    #[tokio::test]
    async fn segmented_file_mode_never_writes_partial_text_or_removes_the_input() {
        let directory = tempfile::tempdir().unwrap();
        let (endpoint, server) = legacy_segment_server(vec![
            (200, r#"{"text":"first"}"#),
            (500, r#"{"error":"failed"}"#),
        ])
        .await;
        let calls = Arc::new(ParkingMutex::new(SegmentedConverterCalls::default()));
        let converter = Arc::new(InstrumentedSegmentConverter {
            calls: calls.clone(),
            outcome: SegmentExportOutcome::Succeed,
        });
        // This deliberately shares the runtime temporary prefix and lives in
        // CACHE_DIR.  Startup cleanup must preserve an explicit --file input.
        let source = directory.path().join("RecordTemp_caller-owned.wav");
        let output = directory.path().join("transcription.txt");
        std::fs::write(&source, b"caller-owned original recording").unwrap();

        let error = run_file_mode_with_cancellation(
            segmented_runtime_config(directory.path(), endpoint),
            converter,
            &source,
            Some(&output),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        server.await.unwrap();

        assert!(matches!(error, RuntimeError::AudioApi(_)));
        assert!(
            source.exists(),
            "--file must never delete caller-owned input"
        );
        assert!(
            !output.exists(),
            "a failed later segment must not leave partial output text"
        );
        assert_eq!(calls.lock().exports.len(), 1);
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| {
                    let path = entry.path();
                    path == source
                        || !entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("RecordTemp_")
                })
        );
    }

    struct RealtimeCaptureBackend {
        captured_packets: Arc<AtomicUsize>,
    }

    impl AudioBackend for RealtimeCaptureBackend {
        fn open_stream(&self, _: &str) -> Result<Box<dyn AudioStream>, String> {
            Ok(Box::new(RealtimeCaptureStream {
                format: test_capture_format(16_000, 1, 16, 16, false),
                captured_packets: self.captured_packets.clone(),
            }))
        }
    }

    struct RealtimeCaptureStream {
        format: CaptureFormat,
        captured_packets: Arc<AtomicUsize>,
    }

    impl AudioStream for RealtimeCaptureStream {
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
            buffer.clear();
            for _ in 0..160 {
                buffer.extend_from_slice(&0x0102_i16.to_le_bytes());
            }
            // Keep the fake close to microphone cadence. This gives the live
            // source time to consume the bounded tee before the test stops.
            std::thread::sleep(Duration::from_millis(3));
            self.captured_packets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn realtime_replay_workflow(url: String) -> AdvancedAudioWorkflow {
        AdvancedAudioWorkflow {
            schema_version: WorkflowSchemaVersion::default(),
            name: "runtime realtime replay fake".into(),
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
                        query: Default::default(),
                        headers: Default::default(),
                        signer: SignerConfig::None,
                        subprotocol: None,
                    },
                    initial_messages: vec![RealtimeMessage::Text {
                        value: "start".into(),
                    }],
                    audio_stream: RealtimeAudioStream {
                        codec: "pcm_s16le".into(),
                        sample_rate: 16_000,
                        channels: 1,
                        chunk_duration_ms: 10,
                        pacing: RealtimePacing::Realtime,
                    },
                    audio_message: RealtimeAudioMessage::Binary,
                    receive_rules: vec![
                        StreamRule {
                            event: Some("partial".into()),
                            path: Some("$.text".into()),
                            action: StreamAction::ReplacePartial,
                            equals: None,
                        },
                        StreamRule {
                            event: Some("segment".into()),
                            path: Some("$.text".into()),
                            action: StreamAction::CommitSegment,
                            equals: None,
                        },
                        StreamRule {
                            event: Some("final".into()),
                            path: Some("$.text".into()),
                            action: StreamAction::SetFinalText,
                            equals: None,
                        },
                        StreamRule {
                            event: Some("complete".into()),
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
                }),
            },
        }
    }

    #[test]
    fn segmented_upload_mode_is_explicitly_excluded_for_realtime_workflows() {
        let config = Config {
            enable_segmented_upload: true,
            advanced_audio_api: AdvancedAudioConfig {
                enabled: true,
                workflow: Some(realtime_replay_workflow("ws://127.0.0.1:1/realtime".into())),
                ..AdvancedAudioConfig::default()
            },
            ..Config::default()
        };
        let client = AudioApiClient::new(config.clone()).unwrap();

        assert!(client.is_realtime_workflow());
        assert!(matches!(
            retry_upload_mode(&config, &client),
            RetryUploadMode::Single
        ));
    }

    #[tokio::test]
    async fn live_websocket_failure_replays_the_complete_local_wav_in_a_fresh_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/realtime", listener.local_addr().unwrap());
        let (first_audio_sender, first_audio_receiver) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut first = accept_async(stream).await.unwrap();
            match first.next().await.unwrap().unwrap() {
                Message::Text(text) => assert_eq!(text.as_str(), "start"),
                message => panic!("expected first-session start message, got {message:?}"),
            }
            let live_audio_bytes = loop {
                match first.next().await.unwrap().unwrap() {
                    Message::Binary(bytes) => break bytes.len(),
                    Message::Ping(payload) => first.send(Message::Pong(payload)).await.unwrap(),
                    message => panic!("expected first-session binary audio, got {message:?}"),
                }
            };
            let _ = first_audio_sender.send(());
            // A transport-level failure ends only the live session. The
            // recorder keeps its complete WAV while Runtime opens a fresh
            // connection for replay below.
            drop(first);

            let (stream, _) = listener.accept().await.unwrap();
            let mut replay = accept_async(stream).await.unwrap();
            match replay.next().await.unwrap().unwrap() {
                Message::Text(text) => assert_eq!(text.as_str(), "start"),
                message => panic!("expected replay start message, got {message:?}"),
            }
            let mut replay_audio_bytes = 0;
            loop {
                match replay.next().await.unwrap().unwrap() {
                    Message::Binary(bytes) => replay_audio_bytes += bytes.len(),
                    Message::Text(text) if text.as_str() == "finish" => break,
                    Message::Ping(payload) => replay.send(Message::Pong(payload)).await.unwrap(),
                    message => panic!("unexpected replay message: {message:?}"),
                }
            }
            for event in [
                r#"{"event":"partial","text":"draft"}"#,
                r#"{"event":"segment","text":"committed"}"#,
                r#"{"event":"final","text":""}"#,
                r#"{"event":"complete"}"#,
            ] {
                replay.send(Message::Text(event.into())).await.unwrap();
            }
            (live_audio_bytes, replay_audio_bytes)
        });

        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            cache_dir: directory.path().to_string_lossy().into_owned(),
            advanced_audio_api: AdvancedAudioConfig {
                enabled: true,
                workflow: Some(realtime_replay_workflow(endpoint)),
                ..AdvancedAudioConfig::default()
            },
            ..Config::default()
        };
        let runtime = Runtime::new(config, Arc::new(NoopConverter)).unwrap();
        let captured_packets = Arc::new(AtomicUsize::new(0));
        let recorder = Arc::new(Recorder::with_backend(
            runtime.config(),
            directory.path().to_path_buf(),
            Arc::new(RealtimeCaptureBackend {
                captured_packets: captured_packets.clone(),
            }),
        ));
        runtime.inner.lock().recorder = recorder;
        runtime.enable_retry_buffer();

        runtime.toggle_recording_locked().await;
        tokio::time::timeout(Duration::from_secs(2), first_audio_receiver)
            .await
            .expect("the live websocket should receive recorder audio")
            .expect("the fake server should report the live audio");
        runtime.toggle_recording_locked().await;

        let (live_audio_bytes, replay_audio_bytes) =
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .expect("the replay websocket should finish")
                .unwrap();
        assert!(live_audio_bytes > 0);
        assert!(replay_audio_bytes > live_audio_bytes);
        assert_eq!(
            replay_audio_bytes,
            captured_packets.load(Ordering::SeqCst) * 320,
            "replay must include every 160-frame capture packet in the local WAV",
        );
        assert!(runtime.has_retryable_recording());
        assert_eq!(runtime.snapshot().state, State::Idle);
        assert_eq!(runtime.snapshot().message, "Empty result from ASR");
    }

    struct CancelAwareConverter {
        started: Arc<Barrier>,
    }

    #[async_trait]
    impl AudioConverter for CancelAwareConverter {
        async fn convert(
            &self,
            cancellation: &CancellationToken,
            _config: &Config,
            _input: &Path,
            _output: &Path,
            _source_rate: i32,
        ) -> Result<(), ConvertError> {
            self.started.wait().await;
            cancellation.cancelled().await;
            Err(ConvertError::Canceled)
        }
    }

    #[tokio::test]
    async fn busy_actions_are_dropped_not_queued() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let _guard = runtime.action_lock.clone().lock_owned().await;
        assert!(!runtime.try_toggle_recording());
        assert!(!runtime.try_toggle_pause());
        assert!(!runtime.try_cancel_or_retry());
        assert_eq!(runtime.snapshot().state, State::Idle);
    }

    #[tokio::test]
    async fn uploading_request_cancel_bypasses_the_busy_action_lock() {
        let directory = tempfile::tempdir().unwrap();
        let wav_path = directory.path().join("RecordTemp_cancel.wav");
        std::fs::write(&wav_path, b"wav").unwrap();
        let started = Arc::new(Barrier::new(2));
        let runtime = Runtime::new(
            Config::default(),
            Arc::new(CancelAwareConverter {
                started: started.clone(),
            }),
        )
        .unwrap();
        runtime.enable_retry_buffer();
        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(wav_path.clone()),
            canceled: false,
        });
        let task_runtime = runtime.clone();
        let task = tokio::spawn(async move {
            let _guard = task_runtime.action_lock.clone().lock_owned().await;
            let cancellation = task_runtime.begin_active_request();
            task_runtime.set_state(
                State::Uploading,
                "Uploading ASR request",
                None::<&RuntimeError>,
            );
            task_runtime
                .transcribe_recording(
                    RecordingResult {
                        wav_path: Some(wav_path.clone()),
                        canceled: false,
                    },
                    &cancellation,
                    true,
                )
                .await;
            task_runtime.clear_active_request();
            wav_path
        });

        started.wait().await;
        assert!(runtime.try_cancel_or_retry());
        let wav_path = task.await.unwrap();
        assert_eq!(
            runtime.snapshot(),
            Event {
                state: State::Idle,
                message: "Request canceled".into(),
                error: String::new(),
                retry_available: true,
            }
        );
        assert!(!wav_path.exists());
        assert!(runtime.has_retryable_recording());
    }

    #[tokio::test]
    async fn retry_buffer_keeps_the_previous_audio_after_recording_cancel_and_drops_oversize() {
        let directory = tempfile::tempdir().unwrap();
        let previous = directory.path().join("previous.wav");
        let canceled = directory.path().join("canceled.wav");
        let replacement = directory.path().join("replacement.wav");
        let oversized = directory.path().join("oversized.wav");
        std::fs::write(&previous, b"previous").unwrap();
        std::fs::write(&canceled, b"canceled").unwrap();
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(MAX_RETRY_AUDIO_BYTES + 1)
            .unwrap();

        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        runtime.enable_retry_buffer();
        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(previous),
            canceled: false,
        });
        assert_eq!(
            runtime
                .inner
                .lock()
                .retry_recording
                .as_deref()
                .map(|recording| recording.original_wav.as_slice()),
            Some(b"previous".as_slice())
        );

        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(canceled),
            canceled: true,
        });
        assert_eq!(
            runtime
                .inner
                .lock()
                .retry_recording
                .as_deref()
                .map(|recording| recording.original_wav.as_slice()),
            Some(b"previous".as_slice())
        );

        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(replacement),
            canceled: false,
        });
        assert_eq!(
            runtime
                .inner
                .lock()
                .retry_recording
                .as_deref()
                .map(|recording| recording.original_wav.as_slice()),
            Some(b"replacement".as_slice())
        );

        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(oversized),
            canceled: false,
        });
        assert!(!runtime.has_retryable_recording());
        runtime.set_state(State::Idle, "", None::<&RuntimeError>);
        assert!(!runtime.snapshot().retry_available);
    }

    #[tokio::test]
    async fn failed_retry_keeps_the_same_buffer_available() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        std::fs::write(&source, b"retry this recording").unwrap();
        let config = Config {
            cache_dir: directory.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let runtime = Runtime::new(config, Arc::new(NoopConverter)).unwrap();
        runtime.enable_retry_buffer();
        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(source),
            canceled: false,
        });
        runtime.set_state(State::Idle, "", None::<&RuntimeError>);

        assert!(runtime.handle_action(3).await);

        assert_eq!(runtime.snapshot().state, State::Idle);
        assert!(runtime.snapshot().retry_available);
        assert_eq!(
            runtime
                .inner
                .lock()
                .retry_recording
                .as_deref()
                .map(|recording| recording.original_wav.as_slice()),
            Some(b"retry this recording".as_slice())
        );
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("RecordTemp_"))
        );
    }

    #[tokio::test]
    async fn cancel_or_retry_action_retries_the_buffer_and_can_be_canceled() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        std::fs::write(&source, b"retry this recording").unwrap();
        let started = Arc::new(Barrier::new(2));
        let config = Config {
            cache_dir: directory.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let runtime = Runtime::new(
            config,
            Arc::new(CancelAwareConverter {
                started: started.clone(),
            }),
        )
        .unwrap();
        runtime.enable_retry_buffer();
        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(source),
            canceled: false,
        });

        let task_runtime = runtime.clone();
        let task = tokio::spawn(async move { task_runtime.handle_action(3).await });

        started.wait().await;
        assert_eq!(runtime.snapshot().state, State::Uploading);
        assert!(runtime.try_cancel_or_retry());
        assert!(task.await.unwrap());

        assert_eq!(runtime.snapshot().state, State::Idle);
        assert_eq!(runtime.snapshot().message, "Request canceled");
        assert!(runtime.snapshot().retry_available);
        assert!(runtime.has_retryable_recording());
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("RecordTemp_"))
        );
    }

    #[tokio::test]
    async fn stopping_runtime_releases_the_retry_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.wav");
        std::fs::write(&source, b"retry this recording").unwrap();
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        runtime.enable_retry_buffer();
        runtime.save_completed_recording_for_retry(&RecordingResult {
            wav_path: Some(source),
            canceled: false,
        });
        assert!(runtime.has_retryable_recording());

        runtime.stop();

        assert!(!runtime.has_retryable_recording());
    }

    #[tokio::test]
    async fn stopped_runtime_rejects_actions_and_late_events() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        runtime.set_state(State::Uploading, "uploading", None::<&RuntimeError>);
        runtime.stop();
        assert!(!runtime.try_toggle_recording());
        runtime.set_state(State::Error, "late", Some(&RuntimeError::Stopped));
        assert_eq!(runtime.snapshot().state, State::Uploading);
    }

    #[tokio::test]
    async fn reload_is_only_allowed_when_idle_or_error() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        runtime.set_state(State::Recording, "recording", None::<&RuntimeError>);
        assert!(matches!(
            runtime.reload(Config::default()).await,
            Err(RuntimeError::CannotReload(State::Recording))
        ));
    }

    #[test]
    fn file_mode_output_failure_still_cleans_temporary_audio() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("RecordTemp_audio.ogg");
        std::fs::write(&temporary, b"converted").unwrap();
        let result = finish_file_mode_output(
            &Config::default(),
            &temporary,
            directory.path().to_path_buf(),
            Transcription {
                text: "hello".into(),
                raw_response: br#"{"text":"hello"}"#.to_vec(),
            },
        );
        assert!(matches!(result, Err(RuntimeError::OutputFile { .. })));
        assert!(!temporary.exists());
    }

    #[test]
    fn file_mode_realtime_replay_prepares_workflow_pcm_wav() {
        let config = Config {
            codecs: "opus".into(),
            container: "opus".into(),
            channels: 2,
            sampling_rate: 48_000,
            sampling_rate_depth: 24,
            ..Config::default()
        };
        let stream = crate::advanced_audio::schema::RealtimeAudioStream {
            codec: "pcm_s16le".into(),
            sample_rate: 16_000,
            channels: 1,
            chunk_duration_ms: 80,
            pacing: crate::advanced_audio::schema::RealtimePacing::Realtime,
        };

        let preparation = file_mode_preparation_config(&config, Some(&stream));

        assert_eq!(preparation.codecs, "pcm");
        assert_eq!(preparation.container, "wav");
        assert_eq!(preparation.channels, 1);
        assert_eq!(preparation.sampling_rate, 16_000);
        assert_eq!(preparation.sampling_rate_depth, 16);
    }

    #[test]
    fn file_mode_non_realtime_preparation_preserves_existing_config() {
        let config = Config {
            codecs: "opus".into(),
            container: "opus".into(),
            channels: 2,
            sampling_rate: 48_000,
            sampling_rate_depth: 24,
            ..Config::default()
        };

        assert_eq!(file_mode_preparation_config(&config, None), config);
    }

    #[tokio::test]
    async fn failed_empty_canceled_and_late_rewrites_never_deliver_or_replace_retry_audio() {
        let config = Config {
            request_failed_notification: true,
            ..Default::default()
        };
        let runtime = Runtime::new(config, Arc::new(NoopConverter)).unwrap();
        runtime.enable_retry_buffer();
        runtime.inner.lock().retry_recording = Some(retry_recording_for_test(b"previous audio"));
        for (result, canceled, stopped) in [
            (Err("network failure".into()), false, false),
            (Ok(" \n".into()), false, false),
            (Ok("late result".into()), true, false),
            (Ok("late result".into()), false, true),
        ] {
            let token = CancellationToken::new();
            if canceled {
                token.cancel();
            }
            if stopped {
                runtime.lifecycle.cancel();
            }
            runtime
                .finish_rewrite_with(result, &token, |_| async {
                    panic!("failed or canceled Rewrite must never call output")
                })
                .await;
            assert_eq!(
                runtime
                    .inner
                    .lock()
                    .retry_recording
                    .as_deref()
                    .unwrap()
                    .original_wav
                    .as_slice(),
                b"previous audio"
            );
        }
    }

    #[tokio::test]
    async fn successful_rewrite_delivers_exactly_once_and_preserves_retry_audio() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        runtime.enable_retry_buffer();
        runtime.inner.lock().retry_recording = Some(retry_recording_for_test(b"audio"));
        let calls = AtomicUsize::new(0);
        runtime
            .finish_rewrite_with(
                Ok("rewritten text".into()),
                &CancellationToken::new(),
                |text| {
                    assert_eq!(text, "rewritten text");
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                },
            )
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.snapshot().message, "Rewrite completed");
        assert!(runtime.snapshot().retry_available);
    }

    #[tokio::test]
    async fn active_audio_and_rewrite_states_reject_new_tasks_and_probes() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        for state in [
            State::Recording,
            State::Paused,
            State::Uploading,
            State::Rewriting,
        ] {
            runtime.set_state(state, "active", None::<&RuntimeError>);
            assert!(!runtime.try_rewrite("any"));
            assert!(
                runtime
                    .test_connection(true, CancellationToken::new(), |_| async {
                        panic!("probe should not start")
                    })
                    .await
                    .is_err()
            );
            assert!(matches!(
                runtime.reload(Config::default()).await,
                Err(RuntimeError::CannotReload(_))
            ));
        }
    }

    #[tokio::test]
    async fn probe_holds_shared_lock_through_cancel_and_cleanup() {
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let started = Arc::new(Barrier::new(2));
        let cleaning = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let task_runtime = runtime.clone();
        let (a, b, c) = (started.clone(), cleaning.clone(), release.clone());
        let task = tokio::spawn(async move {
            task_runtime
                .test_connection(true, CancellationToken::new(), |cancel| async move {
                    a.wait().await;
                    cancel.cancelled().await;
                    b.wait().await;
                    c.wait().await;
                    Ok(())
                })
                .await
        });
        started.wait().await;
        assert_eq!(runtime.snapshot().state, State::Rewriting);
        assert!(!runtime.try_toggle_recording());
        assert!(!runtime.try_rewrite("any"));
        assert!(runtime.try_cancel_or_retry());
        cleaning.wait().await;
        assert!(!runtime.try_toggle_recording());
        assert!(!runtime.try_rewrite("any"));
        release.wait().await;
        assert!(task.await.unwrap().is_err());
        assert_eq!(runtime.snapshot().message, "Request canceled");
        assert!(
            runtime
                .test_connection(false, CancellationToken::new(), |_| async { Ok(()) })
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn saving_retries_failed_startup_hotkeys_and_preserves_config_on_failure() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let attempts = mock_hotkeys(&runtime, &[false, false, true]);
        let original = runtime.config();
        original.save(&path).unwrap();
        assert!(runtime.start_hotkeys().is_err());
        assert_eq!(runtime.snapshot().state, State::Error);

        let mut next = original.clone();
        next.start_key = "ctrl+alt+j".into();
        assert!(matches!(
            runtime.save_and_reload(next.clone(), &path).await,
            Err(RuntimeError::Hotkey(_))
        ));
        assert_eq!(runtime.config(), original);
        assert_eq!(Config::load(&path).unwrap(), original);
        assert_eq!(runtime.snapshot().state, State::Error);

        runtime.save_and_reload(next.clone(), &path).await.unwrap();
        assert_eq!(runtime.config(), next);
        assert_eq!(Config::load(&path).unwrap(), next);
        assert!(runtime.inner.lock().hotkeys.is_some());
        assert_eq!(runtime.snapshot().state, State::Idle);
        assert_eq!(
            attempts.lock().keys,
            [original.start_key, next.start_key.clone(), next.start_key]
        );
        runtime.stop();
        assert_eq!(attempts.lock().stopped, 1);
    }

    #[tokio::test]
    async fn saving_recovers_after_both_replacement_and_rollback_hotkeys_fail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        let attempts = mock_hotkeys(&runtime, &[true, false, false, true]);
        let original = runtime.config();
        original.save(&path).unwrap();
        runtime.start_hotkeys().unwrap();

        let mut next = original.clone();
        next.start_key = "ctrl+alt+j".into();
        let error = runtime
            .save_and_reload(next.clone(), &path)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("failed to restore hotkeys"));
        assert_eq!(runtime.config(), original);
        assert_eq!(Config::load(&path).unwrap(), original);
        assert_eq!(runtime.snapshot().state, State::Error);
        assert!(runtime.inner.lock().hotkeys.is_none());
        assert_eq!(attempts.lock().stopped, 1);

        next.start_key = "ctrl+alt+k".into();
        runtime.save_and_reload(next.clone(), &path).await.unwrap();
        assert_eq!(runtime.config(), next);
        assert_eq!(Config::load(&path).unwrap(), next);
        assert!(runtime.inner.lock().hotkeys.is_some());
        assert_eq!(runtime.snapshot().state, State::Idle);
        assert_eq!(
            attempts.lock().keys,
            [
                original.start_key.clone(),
                "ctrl+alt+j".into(),
                original.start_key,
                next.start_key,
            ]
        );
        runtime.stop();
        assert_eq!(attempts.lock().stopped, 2);
    }

    struct HotkeyAttempts {
        keys: Vec<String>,
        outcomes: std::collections::VecDeque<bool>,
        stopped: usize,
    }

    fn mock_hotkeys(runtime: &Runtime, outcomes: &[bool]) -> Arc<Mutex<HotkeyAttempts>> {
        let attempts = Arc::new(Mutex::new(HotkeyAttempts {
            keys: Vec::new(),
            outcomes: outcomes.iter().copied().collect(),
            stopped: 0,
        }));
        let history = attempts.clone();
        *runtime.hotkey_registrar.lock() = Some(Arc::new(move |config| {
            let succeeds = {
                let mut attempts = history.lock();
                attempts.keys.push(config.start_key.clone());
                attempts
                    .outcomes
                    .pop_front()
                    .expect("unexpected registration")
            };
            if succeeds {
                let history = history.clone();
                Ok(HotkeyRegistration::new(move |_| {
                    history.lock().stopped += 1
                }))
            } else {
                Err(hotkey::HotkeyError::Registration("Hotkey occupied".into()))
            }
        }));
        attempts
    }

    #[tokio::test]
    async fn save_failure_and_busy_save_preserve_file_and_runtime_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let runtime = Runtime::new(Config::default(), Arc::new(NoopConverter)).unwrap();
        *runtime.hotkey_registrar.lock() = Some(Arc::new(|_| {
            panic!("saving must not enable hotkeys that were never requested")
        }));
        let original = runtime.config();
        original.save(&path).unwrap();
        let mut next = original.clone();
        next.model = "changed".into();
        assert!(
            runtime
                .save_and_reload(next.clone(), directory.path())
                .await
                .is_err()
        );
        assert_eq!(runtime.config(), original);
        let guard = runtime.action_lock.clone().lock_owned().await;
        assert!(matches!(
            runtime.save_and_reload(next.clone(), &path).await,
            Err(RuntimeError::Busy)
        ));
        assert_eq!(Config::load(&path).unwrap(), original);
        drop(guard);
        runtime.save_and_reload(next, &path).await.unwrap();
        assert_eq!(Config::load(&path).unwrap(), runtime.config());
        assert_eq!(runtime.config().model, "changed");
    }
}
