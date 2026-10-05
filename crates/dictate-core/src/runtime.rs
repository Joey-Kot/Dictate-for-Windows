use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::asr::{AsrClient, AsrError, Transcription};
use crate::cache;
use crate::clipboard::ClipboardError;
use crate::converter::{AudioConverter, ConvertError, prepare_audio_for_upload};
use crate::hotkey::{self, HotkeyRegistration};
use crate::recorder::{Recorder, RecorderError, RecorderState, RecordingResult};
use crate::text_input;

const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_millis(250);
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_RETRY_AUDIO_BYTES: u64 = 100_000_000;

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
    Asr(#[from] AsrError),
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
    asr_client: Arc<AsrClient>,
    hotkeys: Option<HotkeyRegistration>,
    // A failed registration must not disable the user's intent to use hotkeys.
    hotkeys_requested: bool,
    event: Event,
    event_handler: Option<Arc<dyn Fn(Event) + Send + Sync>>,
    next_session: u64,
    active_session: u64,
    active_request_cancellation: Option<CancellationToken>,
    retry_buffer_enabled: bool,
    retry_recording: Option<Arc<Vec<u8>>>,
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
        let asr_client = Arc::new(AsrClient::new(config.clone())?);
        let recorder = Arc::new(Recorder::new(config.clone(), temp_dir.clone()));
        let runtime = Arc::new(Self {
            inner: Mutex::new(RuntimeInner {
                config,
                temp_dir,
                recorder,
                asr_client,
                hotkeys: None,
                hotkeys_requested: false,
                event: Event::default(),
                event_handler: None,
                next_session: 0,
                active_session: 0,
                active_request_cancellation: None,
                retry_buffer_enabled: false,
                retry_recording: None,
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
            2 => self.toggle_pause_locked(),
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
        let asr_client = Arc::new(AsrClient::new(config.clone())?);
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
            inner.asr_client = asr_client;
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
        let (recorder, hotkeys, request_cancellation) = {
            let mut inner = self.inner.lock();
            inner.retry_recording = None;
            (
                inner.recorder.clone(),
                inner.hotkeys.take(),
                inner.active_request_cancellation.take(),
            )
        };
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
            if let Err(error) = recorder.start(self.lifecycle.child_token()).await {
                self.clear_recording_session(&recorder, session);
                if !self.is_stopped() {
                    self.set_retryable_error("Recording start failed", &error);
                }
                return;
            }
            if self.is_stopped() {
                recorder.request_cancel();
                self.clear_recording_session(&recorder, session);
                return;
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
        match recorder.stop().await {
            Ok(result) => {
                self.clear_recording_session(&recorder, session);
                if self.is_stopped() {
                    discard_recording(&result);
                    return;
                }
                if result.canceled {
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
            if !matches!(inner.event.state, State::Uploading | State::Rewriting) {
                return false;
            }
            inner.active_request_cancellation.clone()
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
            self.set_retryable_error("Recording failed", &error);
        }
    }

    fn toggle_pause_locked(&self) {
        let (recorder, debug) = {
            let inner = self.inner.lock();
            (inner.recorder.clone(), inner.config.hotkey_debug)
        };
        match recorder.toggle_pause() {
            Ok(RecorderState::Paused) => {
                self.set_state(State::Paused, "Recording paused", None::<&RuntimeError>)
            }
            Ok(RecorderState::Recording) => {
                self.set_state(State::Recording, "Recording resumed", None::<&RuntimeError>)
            }
            Ok(_) => {}
            Err(_) if debug => crate::debug_log::write(
                crate::debug_log::Category::Hotkey,
                format_args!("[hotkey] not recording; cannot pause/resume"),
            ),
            Err(_) => {}
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
        let write_cancellation = cancellation.clone();
        let write_result = tokio::task::spawn_blocking({
            let wav_path = wav_path.clone();
            move || {
                if write_cancellation.is_cancelled() {
                    return Ok(());
                }
                std::fs::write(&wav_path, recording.as_slice())
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

        self.transcribe_recording(
            RecordingResult {
                wav_path: Some(wav_path),
                canceled: false,
            },
            &cancellation,
            false,
        )
        .await;
        self.clear_active_request();
    }

    fn save_completed_recording_for_retry(&self, result: &RecordingResult) {
        if result.canceled {
            return;
        }

        {
            let mut inner = self.inner.lock();
            if !inner.retry_buffer_enabled {
                return;
            }
            // A completed recording always supersedes the old retry task. If it cannot be kept
            // within the limit, retry is unavailable until another recording completes.
            inner.retry_recording = None;
        }

        let recording = result.wav_path.as_deref().and_then(load_retry_recording);
        let mut inner = self.inner.lock();
        if !self.is_stopped() && inner.retry_buffer_enabled {
            inner.retry_recording = recording;
        }
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
        let Some(wav_path) = result.wav_path else {
            self.set_retryable_error("Recording failed", &RuntimeError::MissingWav);
            return;
        };
        let (config, client) = {
            let inner = self.inner.lock();
            (inner.config.clone(), inner.asr_client.clone())
        };
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
            Err(AsrError::Canceled) => {
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
                    matches!(&error, AsrError::TextExtraction { .. }),
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

pub async fn run_file_mode_with_cancellation(
    mut config: Config,
    converter: Arc<dyn AudioConverter>,
    input_path: &Path,
    output_path: Option<&Path>,
    cancellation: CancellationToken,
) -> Result<PathBuf, RuntimeError> {
    config.validate()?;
    let temp_dir = cache::initialize_cache_dir(&mut config);
    cache::cleanup_old_temp_items(&temp_dir);
    std::fs::metadata(input_path).map_err(|source| RuntimeError::InputFile {
        path: input_path.to_path_buf(),
        source,
    })?;
    let client = AsrClient::new(config.clone())?;
    let temporary = cache::temporary_output_path(&temp_dir, &config.container_extension());
    if let Err(error) = prepare_audio_for_upload(
        converter.as_ref(),
        &cancellation,
        &config,
        input_path,
        &temporary,
        config.sampling_rate,
    )
    .await
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    let transcription = match client
        .transcribe_with_retry_prepare(&cancellation, &temporary, || async {
            if !config.enable_vad {
                return Ok(());
            }
            prepare_audio_for_upload(
                converter.as_ref(),
                &cancellation,
                &config,
                input_path,
                &temporary,
                config.sampling_rate,
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
                matches!(&error, AsrError::TextExtraction { .. }),
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
    use async_trait::async_trait;
    use tokio::sync::Barrier;

    use super::*;

    struct NoopConverter;

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
                .map(Vec::as_slice),
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
                .map(Vec::as_slice),
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
                .map(Vec::as_slice),
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
                .map(Vec::as_slice),
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

    #[tokio::test]
    async fn failed_empty_canceled_and_late_rewrites_never_deliver_or_replace_retry_audio() {
        let config = Config {
            request_failed_notification: true,
            ..Default::default()
        };
        let runtime = Runtime::new(config, Arc::new(NoopConverter)).unwrap();
        runtime.enable_retry_buffer();
        runtime.inner.lock().retry_recording = Some(Arc::new(b"previous audio".to_vec()));
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
        runtime.inner.lock().retry_recording = Some(Arc::new(b"audio".to_vec()));
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
