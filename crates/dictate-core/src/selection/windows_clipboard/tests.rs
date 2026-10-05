//! These tests use the actual desktop clipboard and must be run individually
//! with `--ignored --test-threads=1` while other clipboard activity is stopped.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use windows::Win32::{
    Foundation::HANDLE,
    System::{
        DataExchange::RegisterClipboardFormatW,
        Memory::{GMEM_ZEROINIT, GlobalAlloc},
    },
    UI::WindowsAndMessaging::{DispatchMessageW, MSG, PM_REMOVE, PeekMessageW},
};

use super::*;

/// Keep the user's real clipboard separate from the fixture snapshot. Explicit
/// restoration reports errors; Drop also attempts restoration on test panic.
struct ExternalClipboard(Option<ClipboardSnapshot>);

impl ExternalClipboard {
    fn restore(&mut self) -> Result<(), String> {
        if let Some(snapshot) = self.0.as_ref() {
            snapshot.restore()?;
        }
        self.0 = None;
        Ok(())
    }
}

impl Drop for ExternalClipboard {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("Could not restore desktop clipboard after native test: {error}");
        }
    }
}

fn set_bytes(format: u32, bytes: &[u8]) -> Result<(), String> {
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len()) }
        .map_err(|error| format!("Cannot allocate native test clipboard data: {error}"))?;
    let data = ClipboardData {
        format,
        kind: DataKind::Global,
        handle: Cell::new(HANDLE(memory.0)),
    };
    {
        let locked = GlobalLockGuard::new(memory)?;
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked.pointer.cast(), bytes.len());
        }
    }
    unsafe { SetClipboardData(format, Some(data.handle.get())) }
        .map_err(|error| format!("Cannot set native test clipboard data: {error}"))?;
    data.handle.set(HANDLE::default());
    Ok(())
}

fn set_text(text: &str) -> Result<(), String> {
    let bytes: Vec<_> = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    set_bytes(UNICODE_TEXT, &bytes)
}

fn seed_clipboard(format: u32) -> Result<(), String> {
    // The HWND is constructed, used and destroyed on this same native thread.
    let owner = OwnerWindow::new()?;
    let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
    unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
    set_text("Original clipboard — 原始内容")?;
    set_bytes(format, b"registered-format\0binary\xff\x01")
    // _clipboard closes before owner is destroyed, as on the production path.
}

fn read_bytes(format: u32) -> Result<Vec<u8>, String> {
    let _clipboard = open(None, Duration::from_secs(1))?;
    let handle = unsafe { GetClipboardData(format) }.map_err(|error| error.to_string())?;
    let memory = HGLOBAL(handle.0);
    let size = unsafe { GlobalSize(memory) };
    if size == 0 || size > 4096 {
        return Err(format!(
            "Unexpected native test clipboard data size: {size}"
        ));
    }
    let locked = GlobalLockGuard::new(memory)?;
    Ok(unsafe { std::slice::from_raw_parts(locked.pointer.cast::<u8>(), size) }.to_vec())
}

fn native_writer(cancel: &AtomicBool) -> Result<(), String> {
    let owner = OwnerWindow::new()?;
    let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
    if cancel.load(Ordering::Acquire) {
        return Err("Native copy test was canceled before writing".into());
    }
    // This is the actual cross-thread ownership transition. A snapshot owner
    // on the waiting test thread would require that thread to pump messages.
    unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
    if cancel.load(Ordering::Acquire) {
        return Err("Native copy test was canceled during EmptyClipboard".into());
    }
    set_text("Copied selection — 选中的文本")
}

fn exercise_native_copy() -> Result<(), String> {
    let format = unsafe {
        RegisterClipboardFormatW(w!(
            "Dictate.Selection.NativeRegression.58513af0-94b5-4608-860c"
        ))
    };
    if format == 0 {
        return Err("Cannot register native clipboard test format".into());
    }
    seed_clipboard(format)?;
    // Include allocation padding in the comparison, just as a real snapshot
    // must preserve every byte of an application's registered format.
    let original_custom = read_bytes(format)?;
    let (snapshot, sequence) = backup_and_clear()?;
    let canceled = Arc::new(AtomicBool::new(false));
    let writer_cancel = Arc::clone(&canceled);
    let (tx, rx) = mpsc::channel();
    let writer = thread::Builder::new()
        .name("clipboard-regression-writer".into())
        .spawn(move || {
            // Send the completion only after the writer's clipboard lock and
            // HWND have both been released by native_writer's stack unwinding.
            let result = native_writer(&writer_cancel);
            let _ = tx.send(result);
        })
        .map_err(|error| error.to_string())?;

    // Deliberately no GetMessage/PeekMessage here. The pre-fix implementation
    // deadlocks the writer in EmptyClipboard and reaches this bounded timeout.
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => {
            writer
                .join()
                .map_err(|_| "Native clipboard writer panicked".to_owned())?;
            result?;
        }
        Err(error) => {
            canceled.store(true, Ordering::Release);
            // Pump only during failure cleanup, while the old implementation's
            // snapshot owner still exists. This releases WM_DESTROYCLIPBOARD
            // without weakening the no-message-pump assertion above. Cleanup
            // must finish before restoring the user's clipboard; a live writer
            // may still hold it open and prevent that restoration.
            let mut message = MSG::default();
            while !writer.is_finished() {
                unsafe {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&message);
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
            let cleanup = writer.join();
            drop(snapshot);
            return Err(format!(
                "Native Copy did not finish without a message pump: {error}; cleanup: {cleanup:?}"
            ));
        }
    }

    let copied = read_changed(sequence)?;
    if copied.as_deref() != Some("Copied selection — 选中的文本") {
        return Err(format!("Native copied text mismatch: {copied:?}"));
    }
    snapshot.restore()?;
    drop(snapshot);

    // The temporary restoration HWND and the snapshot have now been destroyed.
    // Restored data must remain independently available to other applications.
    let restored = read_changed(sequence)?;
    if restored.as_deref() != Some("Original clipboard — 原始内容") {
        return Err(format!("Restored native text mismatch: {restored:?}"));
    }
    if read_bytes(format)? != original_custom {
        return Err("Restored registered clipboard format differs from the original".into());
    }
    Ok(())
}

#[test]
#[ignore = "uses the real Windows desktop clipboard; run manually with --ignored --test-threads=1"]
fn native_copy_without_message_pump_preserves_clipboard_formats() -> Result<(), String> {
    let (external, _) = backup_and_clear()?;
    let mut external = ExternalClipboard(Some(external));
    let outcome = exercise_native_copy();
    let cleanup = external.restore();
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("Desktop clipboard restoration failed: {error}")),
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; desktop clipboard restoration also failed: {cleanup}"
        )),
    }
}
