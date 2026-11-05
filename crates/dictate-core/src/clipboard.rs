use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[cfg(any(windows, test))]
static ACTIVE_TRANSACTIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(any(windows, test))]
struct ActiveTransaction;

#[cfg(any(windows, test))]
impl ActiveTransaction {
    fn new() -> Self {
        ACTIVE_TRANSACTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

#[cfg(any(windows, test))]
impl Drop for ActiveTransaction {
    fn drop(&mut self) {
        ACTIVE_TRANSACTIONS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Keep the async executor alive until any temporary clipboard is restored.
pub(crate) fn wait_for_cleanup() {
    #[cfg(any(windows, test))]
    while ACTIVE_TRANSACTIONS.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard operation canceled")]
    Canceled,
    #[error("clipboard operation canceled (sent Ctrl+V = {paste_sent})")]
    CanceledAfterOverwrite { paste_sent: bool },
    #[error("read original clipboard: {0}")]
    Read(String),
    #[error("write paste text to clipboard: {0}")]
    Write(String),
    #[error("send paste shortcut: {0}")]
    Send(String),
    #[error("restoring clipboard failed: {message}")]
    Restore { paste_sent: bool, message: String },
    #[error("{operation}; restoring clipboard failed: {restore}")]
    OperationAndRestore {
        operation: Box<ClipboardError>,
        paste_sent: bool,
        restore: String,
    },
    #[error("clipboard is only available on Windows")]
    UnsupportedPlatform,
}

impl ClipboardError {
    pub fn paste_was_sent_before_restore_failure(&self) -> bool {
        match self {
            Self::Restore { paste_sent, .. } | Self::OperationAndRestore { paste_sent, .. } => {
                *paste_sent
            }
            _ => false,
        }
    }
}

#[async_trait]
pub trait ClipboardOperations: Send + Sync {
    type Snapshot: Send;

    fn backup_and_write(&self, text: &str) -> Result<Self::Snapshot, ClipboardError>;
    /// Called just after a successful write, before attempting restoration.
    fn wrote_without_backup(&self, _snapshot: &Self::Snapshot) -> bool {
        false
    }
    fn temporary_is_current(&self, snapshot: &Self::Snapshot) -> Result<bool, String>;
    fn restore(&self, snapshot: &Self::Snapshot) -> Result<(), String>;
    fn send_paste(&self) -> Result<(), String>;

    async fn wait(
        &self,
        cancellation: &CancellationToken,
        duration: Duration,
    ) -> Result<(), ClipboardError> {
        tokio::select! {
            _ = cancellation.cancelled() => Err(ClipboardError::Canceled),
            _ = tokio::time::sleep(duration) => Ok(()),
        }
    }

    async fn wait_after_paste(&self, _cancellation: &CancellationToken, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub async fn paste_text(
    text: &str,
    cancellation: &CancellationToken,
    write_delay: Duration,
    restore_delay: Duration,
) -> Result<(), ClipboardError> {
    paste_text_with_policy(text, cancellation, write_delay, restore_delay, false).await
}

pub(crate) async fn paste_text_with_policy(
    text: &str,
    cancellation: &CancellationToken,
    write_delay: Duration,
    restore_delay: Duration,
    force_write: bool,
) -> Result<(), ClipboardError> {
    #[cfg(windows)]
    {
        paste_text_with(
            text,
            cancellation,
            write_delay,
            restore_delay,
            &WindowsClipboard {
                force_write,
                cancellation,
            },
        )
        .await
    }
    #[cfg(not(windows))]
    {
        let _ = (text, cancellation, write_delay, restore_delay, force_write);
        Err(ClipboardError::UnsupportedPlatform)
    }
}

pub async fn paste_text_with<O: ClipboardOperations + ?Sized>(
    text: &str,
    cancellation: &CancellationToken,
    write_delay: Duration,
    restore_delay: Duration,
    operations: &O,
) -> Result<(), ClipboardError> {
    // Register before checking cancellation so shutdown cannot miss a paste
    // transaction that has already decided to touch the clipboard.
    #[cfg(any(windows, test))]
    let _transaction = ActiveTransaction::new();
    if cancellation.is_cancelled() {
        return Err(ClipboardError::Canceled);
    }
    // Keep the clipboard open from the backup attempt through the write, so
    // another copy cannot be lost between the two steps.
    let snapshot = operations.backup_and_write(text)?;
    let wrote_without_backup = operations.wrote_without_backup(&snapshot);

    let mut paste_sent = false;
    let mut operation = operations.wait(cancellation, write_delay).await;
    if operation.is_ok() && cancellation.is_cancelled() {
        operation = Err(ClipboardError::Canceled);
    }
    if operation.is_ok() {
        operation = match operations.temporary_is_current(&snapshot) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ClipboardError::Write(
                "Clipboard changed before the paste shortcut".into(),
            )),
            Err(error) => Err(ClipboardError::Write(error)),
        };
    }
    // Opening and checking the clipboard can block; cancellation may have
    // arrived after the earlier check while another process held it open.
    if operation.is_ok() && cancellation.is_cancelled() {
        operation = Err(ClipboardError::Canceled);
    }
    if operation.is_ok() {
        operation = operations.send_paste().map_err(ClipboardError::Send);
    }
    if operation.is_ok() {
        paste_sent = true;
        // Once Ctrl+V has been sent, let the target consume the temporary
        // clipboard even if the task is canceled during the restore delay.
        operations
            .wait_after_paste(cancellation, restore_delay)
            .await;
        if cancellation.is_cancelled() {
            operation = Err(ClipboardError::Canceled);
        }
    }

    // Restore any backup even after failure or cancellation.
    let restore = operations.restore(&snapshot);
    match (operation, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(message)) => Err(ClipboardError::Restore {
            paste_sent,
            message,
        }),
        (Err(ClipboardError::Canceled), Ok(())) if wrote_without_backup => {
            Err(ClipboardError::CanceledAfterOverwrite { paste_sent })
        }
        (Err(error), Ok(())) => Err(error),
        (Err(operation), Err(restore)) => Err(ClipboardError::OperationAndRestore {
            operation: Box::new(operation),
            paste_sent,
            restore,
        }),
    }
}

#[cfg(windows)]
struct WindowsClipboard<'a> {
    force_write: bool,
    cancellation: &'a CancellationToken,
}

#[cfg(windows)]
#[async_trait]
impl ClipboardOperations for WindowsClipboard<'_> {
    type Snapshot = crate::windows_clipboard::ClipboardSnapshot;

    fn backup_and_write(&self, text: &str) -> Result<Self::Snapshot, ClipboardError> {
        use crate::windows_clipboard::BeginError;
        crate::windows_clipboard::backup_and_write(text, self.cancellation, self.force_write)
            .map_err(|error| match error {
                BeginError::Canceled => ClipboardError::Canceled,
                BeginError::Backup(message) => ClipboardError::Read(message),
                BeginError::Replace(message) => ClipboardError::Write(message),
                BeginError::ReplaceAndRestore { operation, restore } => {
                    ClipboardError::OperationAndRestore {
                        operation: Box::new(ClipboardError::Write(operation)),
                        paste_sent: false,
                        restore,
                    }
                }
            })
    }

    fn wrote_without_backup(&self, snapshot: &Self::Snapshot) -> bool {
        !snapshot.needs_restore()
    }

    fn restore(&self, snapshot: &Self::Snapshot) -> Result<(), String> {
        snapshot.restore()
    }

    fn temporary_is_current(&self, snapshot: &Self::Snapshot) -> Result<bool, String> {
        snapshot.temporary_is_current()
    }

    fn send_paste(&self) -> Result<(), String> {
        crate::keyboard::send_ctrl_v().map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Content {
        text: Option<String>,
        image: Vec<u8>,
        custom: Vec<u8>,
    }

    struct Snapshot {
        original: Option<Content>,
        temporary: Content,
    }

    #[derive(Default)]
    struct Fake {
        clipboard: Mutex<Content>,
        calls: Mutex<Vec<String>>,
        durations: Mutex<Vec<Duration>>,
        cancel_on_wait: Mutex<Option<usize>>,
        cancel_on_check: Mutex<Option<CancellationToken>>,
        change_on_wait: Mutex<Option<usize>>,
        waits: Mutex<usize>,
        pause_after_paste: Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                tokio::sync::oneshot::Receiver<()>,
            )>,
        >,
        backup_error: Mutex<bool>,
        force_write: bool,
        restore_error: Mutex<bool>,
        send_error: Mutex<bool>,
    }

    impl Fake {
        fn record_wait(&self, cancellation: &CancellationToken, duration: Duration) -> bool {
            self.calls.lock().unwrap().push("wait".into());
            self.durations.lock().unwrap().push(duration);
            let mut waits = self.waits.lock().unwrap();
            *waits += 1;
            if self
                .change_on_wait
                .lock()
                .unwrap()
                .is_some_and(|at| at == *waits)
            {
                *self.clipboard.lock().unwrap() = Content {
                    text: Some("new user copy".into()),
                    ..Content::default()
                };
            }
            if self
                .cancel_on_wait
                .lock()
                .unwrap()
                .is_some_and(|at| at == *waits)
            {
                cancellation.cancel();
                return true;
            }
            false
        }
    }

    #[async_trait]
    impl ClipboardOperations for Fake {
        type Snapshot = Snapshot;

        fn backup_and_write(&self, text: &str) -> Result<Snapshot, ClipboardError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("backup/write:{text}"));
            let backup_failed = *self.backup_error.lock().unwrap();
            if backup_failed && !self.force_write {
                return Err(ClipboardError::Read("backup failed".into()));
            }
            let mut clipboard = self.clipboard.lock().unwrap();
            let original = (!backup_failed).then(|| clipboard.clone());
            let temporary = Content {
                text: Some(text.into()),
                ..Content::default()
            };
            *clipboard = temporary.clone();
            Ok(Snapshot {
                original,
                temporary,
            })
        }

        fn wrote_without_backup(&self, snapshot: &Snapshot) -> bool {
            snapshot.original.is_none()
        }

        fn temporary_is_current(&self, snapshot: &Snapshot) -> Result<bool, String> {
            self.calls.lock().unwrap().push("check".into());
            if let Some(cancellation) = self.cancel_on_check.lock().unwrap().take() {
                cancellation.cancel();
            }
            Ok(*self.clipboard.lock().unwrap() == snapshot.temporary)
        }

        fn restore(&self, snapshot: &Snapshot) -> Result<(), String> {
            self.calls.lock().unwrap().push("restore".into());
            let Some(original) = &snapshot.original else {
                return Ok(());
            };
            if *self.restore_error.lock().unwrap() {
                return Err("restore failed".into());
            }
            let mut clipboard = self.clipboard.lock().unwrap();
            if *clipboard == snapshot.temporary {
                *clipboard = original.clone();
            }
            Ok(())
        }

        fn send_paste(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("send".into());
            if *self.send_error.lock().unwrap() {
                return Err("send failed".into());
            }
            Ok(())
        }

        async fn wait(
            &self,
            cancellation: &CancellationToken,
            duration: Duration,
        ) -> Result<(), ClipboardError> {
            if self.record_wait(cancellation, duration) {
                return Err(ClipboardError::Canceled);
            }
            Ok(())
        }

        async fn wait_after_paste(&self, cancellation: &CancellationToken, duration: Duration) {
            let pause = self.pause_after_paste.lock().unwrap().take();
            if let Some((started, release)) = pause {
                let _ = started.send(());
                let _ = release.await;
            }
            self.record_wait(cancellation, duration);
        }
    }

    #[tokio::test]
    async fn restores_empty_text_image_and_multiple_formats() {
        for original in [
            Content::default(),
            Content {
                text: Some("original".into()),
                ..Content::default()
            },
            Content {
                image: vec![1, 2, 3],
                ..Content::default()
            },
            Content {
                text: Some("original".into()),
                image: vec![4, 5],
                custom: vec![6, 7],
            },
        ] {
            let fake = Fake::default();
            *fake.clipboard.lock().unwrap() = original.clone();
            paste_text_with(
                "transcription",
                &CancellationToken::new(),
                Duration::from_millis(25),
                Duration::from_millis(75),
                &fake,
            )
            .await
            .unwrap();
            assert_eq!(*fake.clipboard.lock().unwrap(), original);
            assert_eq!(
                *fake.calls.lock().unwrap(),
                [
                    "backup/write:transcription",
                    "wait",
                    "check",
                    "send",
                    "wait",
                    "restore"
                ]
            );
            assert_eq!(
                *fake.durations.lock().unwrap(),
                [Duration::from_millis(25), Duration::from_millis(75)]
            );
        }
    }

    #[tokio::test]
    async fn cancellation_before_and_after_send_restores() {
        for wait in [1, 2] {
            let fake = Fake::default();
            let original = Content {
                image: vec![1, 2, 3],
                ..Content::default()
            };
            *fake.clipboard.lock().unwrap() = original.clone();
            *fake.cancel_on_wait.lock().unwrap() = Some(wait);
            let error = paste_text_with(
                "transcription",
                &CancellationToken::new(),
                Duration::from_millis(80),
                Duration::from_millis(120),
                &fake,
            )
            .await
            .unwrap_err();
            assert!(matches!(error, ClipboardError::Canceled));
            assert_eq!(*fake.clipboard.lock().unwrap(), original);
            if wait == 2 {
                assert_eq!(
                    fake.durations.lock().unwrap().last(),
                    Some(&Duration::from_millis(120))
                );
            }
        }
    }

    #[tokio::test]
    async fn cancellation_after_forced_overwrite_reports_whether_paste_was_sent() {
        for (cancel_on_wait, paste_sent) in [(1, false), (2, true)] {
            let fake = Fake {
                clipboard: Mutex::new(Content {
                    text: Some("original".into()),
                    ..Content::default()
                }),
                cancel_on_wait: Mutex::new(Some(cancel_on_wait)),
                backup_error: Mutex::new(true),
                force_write: true,
                ..Fake::default()
            };
            let error = paste_text_with(
                "transcription",
                &CancellationToken::new(),
                Duration::ZERO,
                Duration::ZERO,
                &fake,
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                ClipboardError::CanceledAfterOverwrite { paste_sent: sent } if sent == paste_sent
            ));
            assert_eq!(
                fake.clipboard.lock().unwrap().text.as_deref(),
                Some("transcription")
            );
            assert_eq!(
                fake.calls.lock().unwrap().iter().any(|call| call == "send"),
                paste_sent
            );
        }
    }

    #[tokio::test]
    async fn cancellation_during_clipboard_check_restores_without_pasting() {
        let fake = Fake::default();
        let original = Content {
            text: Some("original".into()),
            image: vec![1, 2, 3],
            ..Content::default()
        };
        *fake.clipboard.lock().unwrap() = original.clone();
        let cancellation = CancellationToken::new();
        *fake.cancel_on_check.lock().unwrap() = Some(cancellation.clone());

        let result = paste_text_with(
            "transcription",
            &cancellation,
            Duration::ZERO,
            Duration::ZERO,
            &fake,
        )
        .await;

        assert!(matches!(result, Err(ClipboardError::Canceled)));
        assert_eq!(*fake.clipboard.lock().unwrap(), original);
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["backup/write:transcription", "wait", "check", "restore"]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_paste_to_restore_clipboard() {
        let fake = Arc::new(Fake::default());
        let original = Content {
            image: vec![1, 2, 3],
            ..Content::default()
        };
        *fake.clipboard.lock().unwrap() = original.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *fake.pause_after_paste.lock().unwrap() = Some((started_tx, release_rx));
        let cancellation = CancellationToken::new();
        let paste_token = cancellation.clone();
        let paste_fake = Arc::clone(&fake);
        let paste = tokio::spawn(async move {
            paste_text_with(
                "transcription",
                &paste_token,
                Duration::ZERO,
                Duration::ZERO,
                paste_fake.as_ref(),
            )
            .await
        });
        started_rx.await.unwrap();
        cancellation.cancel();

        let mut cleanup = tokio::task::spawn_blocking(wait_for_cleanup);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut cleanup)
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        assert!(matches!(
            paste.await.unwrap(),
            Err(ClipboardError::Canceled)
        ));
        tokio::time::timeout(Duration::from_secs(1), cleanup)
            .await
            .expect("shutdown did not finish after clipboard restoration")
            .unwrap();
        assert_eq!(*fake.clipboard.lock().unwrap(), original);
    }

    #[tokio::test]
    async fn failed_backup_does_not_write_or_restore() {
        let fake = Fake::default();
        *fake.backup_error.lock().unwrap() = true;
        let error = paste_text_with(
            "transcription",
            &CancellationToken::new(),
            Duration::ZERO,
            Duration::ZERO,
            &fake,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ClipboardError::Read(_)));
        assert_eq!(*fake.calls.lock().unwrap(), ["backup/write:transcription"]);
    }

    #[tokio::test]
    async fn a_new_user_copy_is_preserved_before_and_after_paste() {
        for change_at in [1, 2] {
            let fake = Fake::default();
            *fake.clipboard.lock().unwrap() = Content {
                image: vec![1, 2, 3],
                ..Content::default()
            };
            *fake.change_on_wait.lock().unwrap() = Some(change_at);
            let result = paste_text_with(
                "transcription",
                &CancellationToken::new(),
                Duration::ZERO,
                Duration::ZERO,
                &fake,
            )
            .await;
            if change_at == 1 {
                assert!(matches!(result, Err(ClipboardError::Write(_))));
                assert!(!fake.calls.lock().unwrap().iter().any(|call| call == "send"));
            } else {
                assert!(result.is_ok());
            }
            assert_eq!(
                fake.clipboard.lock().unwrap().text.as_deref(),
                Some("new user copy")
            );
        }
    }

    #[tokio::test]
    async fn restore_after_sent_is_a_distinct_error() {
        let fake = Fake::default();
        *fake.restore_error.lock().unwrap() = true;
        let error = paste_text_with(
            "transcription",
            &CancellationToken::new(),
            Duration::ZERO,
            Duration::ZERO,
            &fake,
        )
        .await
        .unwrap_err();
        assert!(error.paste_was_sent_before_restore_failure());
    }

    #[tokio::test]
    async fn send_and_restore_errors_are_both_preserved() {
        let fake = Fake::default();
        *fake.send_error.lock().unwrap() = true;
        *fake.restore_error.lock().unwrap() = true;
        let error = paste_text_with(
            "transcription",
            &CancellationToken::new(),
            Duration::ZERO,
            Duration::ZERO,
            &fake,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ClipboardError::OperationAndRestore {
                paste_sent: false,
                ..
            }
        ));
        assert!(error.to_string().contains("send paste shortcut"));
        assert!(error.to_string().contains("restoring clipboard failed"));
    }
}
