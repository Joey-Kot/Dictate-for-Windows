//! Read through the target application's Copy command, restoring the clipboard
//! before releasing the shared Audio/Rewrite task.
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[cfg(any(windows, test))]
use std::time::Instant;

#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows_clipboard;
#[cfg(windows)]
mod windows_copy;

#[cfg(windows)]
static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Shutdown cannot leave a Copy transaction behind when the process exits.
/// Wait for the native worker directly, without depending on Tokio scheduling.
pub(crate) fn wait_for_cleanup() {
    #[cfg(windows)]
    while BUSY.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("Request canceled")]
    Canceled,
    #[error("{0}")]
    Read(String),
    #[error("{0}")]
    Restore(String),
}

pub async fn read(
    cancel: &CancellationToken,
    write_delay: Duration,
    restore_delay: Duration,
) -> Result<String, SelectionError> {
    if cancel.is_cancelled() {
        return Err(SelectionError::Canceled);
    }
    #[cfg(windows)]
    {
        use std::sync::atomic::Ordering;
        // Keep the guard on the worker even if its caller is dropped. Native
        // clipboard providers can take longer than our own polling deadline.
        if BUSY.swap(true, Ordering::AcqRel) {
            return Err(SelectionError::Read(
                "Selected-text reader is still busy".into(),
            ));
        }
        struct Release;
        impl Drop for Release {
            fn drop(&mut self) {
                BUSY.store(false, Ordering::Release);
            }
        }
        let release = Release;
        let backend = windows_copy::WindowsCopy::capture().map_err(SelectionError::Read)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let token = cancel.clone();
        std::thread::Builder::new()
            .name("dictate-selection".into())
            .spawn(move || {
                let _release = release;
                let result = read_with(&backend, &token, write_delay, restore_delay);
                let _ = tx.send(result);
            })
            .map_err(|error| SelectionError::Read(error.to_string()))?;
        // Do not race cancellation/timeout against this receiver: Ctrl+C may
        // already be queued. The worker must finish restoration before a new
        // task is allowed to read or write the clipboard.
        rx.await
            .map_err(|_| SelectionError::Read("Selected-text reader stopped".into()))?
    }
    #[cfg(not(windows))]
    {
        let _ = (write_delay, restore_delay);
        Err(SelectionError::Read(
            "Selected-text reading is only available on Windows".into(),
        ))
    }
}

#[cfg(any(windows, test))]
trait CopyBackend {
    type Snapshot;
    fn target_is_current(&self) -> Result<bool, String>;
    fn keys_pressed(&self) -> bool;
    fn backup_and_clear(&self) -> Result<(Self::Snapshot, u32), String>;
    fn send_copy(&self) -> Result<(), String>;
    // None means unchanged/busy. A read error is returned only after fresh
    // clipboard data is advertised and the copy writer has released its lock.
    fn read_changed(&self, sequence: u32) -> Result<Option<String>, String>;
    fn restore(&self, snapshot: &Self::Snapshot) -> Result<(), String>;
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(any(windows, test))]
fn check_target(
    backend: &impl CopyBackend,
    cancel: &CancellationToken,
) -> Result<(), SelectionError> {
    if cancel.is_cancelled() {
        return Err(SelectionError::Canceled);
    }
    if !backend.target_is_current().map_err(SelectionError::Read)? {
        return Err(SelectionError::Read(
            "Focus changed while reading selected text".into(),
        ));
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn read_with(
    backend: &impl CopyBackend,
    cancel: &CancellationToken,
    write_delay: Duration,
    restore_delay: Duration,
) -> Result<String, SelectionError> {
    let deadline = backend.now() + Duration::from_secs(2);
    loop {
        check_target(backend, cancel)?;
        if !backend.keys_pressed() {
            break;
        }
        if backend.now() >= deadline {
            return Err(SelectionError::Read(
                "Release shortcut keys before copying selected text".into(),
            ));
        }
        backend.sleep(Duration::from_millis(10));
    }
    let (snapshot, sequence) = backend.backup_and_clear().map_err(SelectionError::Read)?;
    let mut copy_attempted = false;
    let mut result = (|| {
        // Reuse the same delay as the clipboard output path: allow our write
        // (here, clearing the clipboard) to settle before sending the shortcut.
        let mut remaining = write_delay;
        while !remaining.is_zero() {
            check_target(backend, cancel)?;
            let step = remaining.min(Duration::from_millis(10));
            backend.sleep(step);
            remaining -= step;
        }
        check_target(backend, cancel)?;
        if backend.keys_pressed() {
            return Err(SelectionError::Read(
                "Shortcut keys pressed while preparing Copy".into(),
            ));
        }
        copy_attempted = true;
        let mut error = backend.send_copy().map_err(SelectionError::Read).err();
        let deadline = backend.now() + Duration::from_secs(3);
        loop {
            // Once Copy has been injected, cancellation/focus changes discard
            // its result but still wait for it to finish before restoration.
            backend.sleep(Duration::from_millis(25));
            if error.is_none() {
                error = check_target(backend, cancel).err();
            }
            if let Some(text) = backend
                .read_changed(sequence)
                .map_err(SelectionError::Read)?
            {
                if let Some(error) = error {
                    return Err(error);
                }
                if text.trim().is_empty() {
                    return Err(SelectionError::Read("No text copied by Ctrl+C".into()));
                }
                if text.len() > 1_000_000 {
                    return Err(SelectionError::Read(
                        "Copied text exceeds 1,000,000 UTF-8 bytes".into(),
                    ));
                }
                return Ok(text);
            }
            if backend.now() >= deadline {
                return Err(error.unwrap_or_else(|| {
                    SelectionError::Read("No text copied: timed out waiting for Ctrl+C".into())
                }));
            }
        }
    })();
    if copy_attempted {
        // The shared configured restore delay also applies to reading, even
        // when output uses SendInput. Restoration is not cancellable.
        backend.sleep(restore_delay);
    }
    if result.is_ok() {
        result = check_target(backend, cancel).and(result);
    }
    if let Err(restore) = backend.restore(&snapshot) {
        return Err(SelectionError::Restore(match result {
            Ok(_) => format!("Cannot restore clipboard: {restore}"),
            Err(operation) => format!("{operation}; cannot restore clipboard: {restore}"),
        }));
    }
    check_target(backend, cancel)?;
    result
}
