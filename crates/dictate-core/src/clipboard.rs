use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard operation canceled")]
    Canceled,
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
    fn read_text(&self) -> Result<String, String>;
    fn write_text(&self, text: &str) -> Result<(), String>;
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
}

pub async fn paste_text(
    text: &str,
    cancellation: &CancellationToken,
    write_delay: Duration,
    restore_delay: Duration,
) -> Result<(), ClipboardError> {
    #[cfg(windows)]
    {
        paste_text_with(
            text,
            cancellation,
            write_delay,
            restore_delay,
            &WindowsClipboard,
        )
        .await
    }
    #[cfg(not(windows))]
    {
        let _ = (text, cancellation, write_delay, restore_delay);
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
    if cancellation.is_cancelled() {
        return Err(ClipboardError::Canceled);
    }
    let original = operations.read_text().map_err(ClipboardError::Read)?;
    if cancellation.is_cancelled() {
        return Err(ClipboardError::Canceled);
    }

    let mut paste_sent = false;
    let operation = async {
        operations.write_text(text).map_err(ClipboardError::Write)?;
        operations.wait(cancellation, write_delay).await?;
        if cancellation.is_cancelled() {
            return Err(ClipboardError::Canceled);
        }
        operations.send_paste().map_err(ClipboardError::Send)?;
        paste_sent = true;
        operations.wait(cancellation, restore_delay).await
    }
    .await;

    // Restoration is deliberately unconditional after the original text has
    // been read, including failure and cancellation paths.
    let restore = operations.write_text(&original);
    match (operation, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(message)) => Err(ClipboardError::Restore {
            paste_sent,
            message,
        }),
        (Err(error), Ok(())) => Err(error),
        (Err(operation), Err(restore)) => Err(ClipboardError::OperationAndRestore {
            operation: Box::new(operation),
            paste_sent,
            restore,
        }),
    }
}

#[cfg(windows)]
struct WindowsClipboard;

#[cfg(windows)]
#[async_trait]
impl ClipboardOperations for WindowsClipboard {
    fn read_text(&self) -> Result<String, String> {
        windows_clipboard::read_text()
    }

    fn write_text(&self, text: &str) -> Result<(), String> {
        windows_clipboard::write_text(text)
    }

    fn send_paste(&self) -> Result<(), String> {
        crate::keyboard::send_ctrl_v().map_err(|error| error.to_string())
    }
}

#[cfg(windows)]
mod windows_clipboard {
    use std::ffi::c_void;
    use std::ptr;
    use std::time::{Duration, Instant};

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn OpenClipboard(owner: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> isize;
        fn SetClipboardData(format: u32, memory: isize) -> isize;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
        fn GlobalFree(memory: isize) -> isize;
        fn GlobalLock(memory: isize) -> *mut c_void;
        fn GlobalUnlock(memory: isize) -> i32;
        fn GetLastError() -> u32;
        fn SetLastError(error: u32);
    }

    struct ClipboardGuard;

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }

    struct GlobalMemory(isize);

    impl Drop for GlobalMemory {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    let _ = GlobalFree(self.0);
                }
            }
        }
    }

    fn last_error(operation: &str) -> String {
        format!("{operation} failed with Win32 error {}", unsafe {
            GetLastError()
        })
    }

    fn open_clipboard() -> Result<ClipboardGuard, String> {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if unsafe { OpenClipboard(0) } != 0 {
                return Ok(ClipboardGuard);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(last_error("OpenClipboard"))
    }

    pub(super) fn read_text() -> Result<String, String> {
        // A clipboard without Unicode text is treated as an empty previous value.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
            return Ok(String::new());
        }
        let _clipboard = open_clipboard()?;
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle == 0 {
            return Err(last_error("GetClipboardData"));
        }
        let pointer = unsafe { GlobalLock(handle) }.cast::<u16>();
        if pointer.is_null() {
            return Err(last_error("GlobalLock"));
        }
        let mut length = 0;
        unsafe {
            while *pointer.add(length) != 0 {
                length += 1;
            }
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) });
        unsafe {
            SetLastError(0);
            if GlobalUnlock(handle) == 0 && GetLastError() != 0 {
                return Err(last_error("GlobalUnlock"));
            }
        }
        Ok(text)
    }

    pub(super) fn write_text(text: &str) -> Result<(), String> {
        let _clipboard = open_clipboard()?;
        if unsafe { EmptyClipboard() } == 0 {
            return Err(last_error("EmptyClipboard"));
        }
        let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = utf16.len() * std::mem::size_of::<u16>();
        let mut memory = GlobalMemory(unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) });
        if memory.0 == 0 {
            return Err(last_error("GlobalAlloc"));
        }
        let pointer = unsafe { GlobalLock(memory.0) }.cast::<u16>();
        if pointer.is_null() {
            return Err(last_error("GlobalLock"));
        }
        unsafe {
            ptr::copy_nonoverlapping(utf16.as_ptr(), pointer, utf16.len());
            SetLastError(0);
            if GlobalUnlock(memory.0) == 0 && GetLastError() != 0 {
                return Err(last_error("GlobalUnlock"));
            }
        }
        if unsafe { SetClipboardData(CF_UNICODETEXT, memory.0) } == 0 {
            return Err(last_error("SetClipboardData"));
        }
        memory.0 = 0; // Ownership transfers to the system.
        Ok(())
    }
}

impl fmt::Display for dyn ClipboardOperations + '_ {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("clipboard operations")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        durations: Mutex<Vec<Duration>>,
        cancel_on_wait: Mutex<Option<usize>>,
        waits: Mutex<usize>,
        fail_write_at: Mutex<Option<usize>>,
        writes: Mutex<usize>,
        send_error: Mutex<bool>,
    }

    #[async_trait]
    impl ClipboardOperations for Fake {
        fn read_text(&self) -> Result<String, String> {
            self.calls.lock().unwrap().push("read".into());
            Ok("original".into())
        }

        fn write_text(&self, text: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("write:{text}"));
            let mut writes = self.writes.lock().unwrap();
            *writes += 1;
            if self
                .fail_write_at
                .lock()
                .unwrap()
                .is_some_and(|at| at == *writes)
            {
                return Err("write failed".into());
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
            self.calls.lock().unwrap().push("wait".into());
            self.durations.lock().unwrap().push(duration);
            let mut waits = self.waits.lock().unwrap();
            *waits += 1;
            if self
                .cancel_on_wait
                .lock()
                .unwrap()
                .is_some_and(|at| at == *waits)
            {
                cancellation.cancel();
                return Err(ClipboardError::Canceled);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn writes_sends_and_restores_in_order() {
        let fake = Fake::default();
        paste_text_with(
            "transcription",
            &CancellationToken::new(),
            Duration::from_millis(25),
            Duration::from_millis(75),
            &fake,
        )
        .await
        .unwrap();
        assert_eq!(
            *fake.calls.lock().unwrap(),
            [
                "read",
                "write:transcription",
                "wait",
                "send",
                "wait",
                "write:original"
            ]
        );
        assert_eq!(
            *fake.durations.lock().unwrap(),
            [Duration::from_millis(25), Duration::from_millis(75)]
        );
    }

    #[tokio::test]
    async fn cancellation_before_and_after_send_restores() {
        for wait in [1, 2] {
            let fake = Fake::default();
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
            assert!(
                fake.calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|call| call == "write:original")
            );
        }
    }

    #[tokio::test]
    async fn restore_after_sent_is_a_distinct_error() {
        let fake = Fake::default();
        *fake.fail_write_at.lock().unwrap() = Some(2);
        let error = paste_text_with(
            "transcription",
            &CancellationToken::new(),
            Duration::from_millis(80),
            Duration::from_millis(120),
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
        *fake.fail_write_at.lock().unwrap() = Some(2);
        let error = paste_text_with(
            "transcription",
            &CancellationToken::new(),
            Duration::from_millis(80),
            Duration::from_millis(120),
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
