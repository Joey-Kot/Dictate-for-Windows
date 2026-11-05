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
    let (snapshot, sequence) = backup_and_clear().map_err(|error| error.to_string())?;
    assert_history_excluded()?;
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
    with_saved_clipboard(exercise_native_copy)
}

fn with_saved_clipboard(test: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let (external, _) = backup_and_clear().map_err(|error| error.to_string())?;
    let mut external = ExternalClipboard(Some(external));
    let outcome = test();
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

fn history_format() -> Result<u32, String> {
    let format =
        unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
    (format != 0)
        .then_some(format)
        .ok_or_else(|| "Cannot register clipboard history exclusion format".into())
}

fn assert_history_excluded() -> Result<(), String> {
    let _clipboard = open(None, Duration::from_secs(1))?;
    unsafe { IsClipboardFormatAvailable(history_format()?) }
        .map_err(|_| "Clipboard history exclusion marker is missing".into())
}

fn seed_image_and_text(
    custom_format: u32,
    include_text: bool,
    original_marker: bool,
) -> Result<(), String> {
    let owner = OwnerWindow::new()?;
    let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
    unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
    if include_text {
        set_text("Original text — 原始文本")?;
    }
    // BITMAPINFOHEADER followed by four 32-bit pixels for a 2x2 CF_DIB.
    let mut dib = vec![0u8; 40 + 16];
    dib[0..4].copy_from_slice(&40u32.to_le_bytes());
    dib[4..8].copy_from_slice(&2i32.to_le_bytes());
    dib[8..12].copy_from_slice(&2i32.to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&32u16.to_le_bytes());
    dib[20..24].copy_from_slice(&16u32.to_le_bytes());
    dib[40..].copy_from_slice(&[
        0x00, 0x00, 0xff, 0xff, 0x00, 0xff, 0x00, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        0xff,
    ]);
    set_bytes(8, &dib)?; // CF_DIB
    set_bytes(custom_format, b"registered-format\0image-fixture")?;
    if original_marker {
        set_bytes(history_format()?, b"original-exclusion-marker")?;
    }
    Ok(())
}

fn exercise_native_output() -> Result<(), String> {
    let custom_format = unsafe {
        RegisterClipboardFormatW(w!("Dictate.Output.NativeRegression.88b4f604-c5bb-4fd8"))
    };
    if custom_format == 0 {
        return Err("Cannot register native clipboard test format".into());
    }
    let cancellation = CancellationToken::new();
    for (include_text, original_marker, force_write) in [
        (false, false, false),
        (true, false, true),
        (true, true, false),
    ] {
        seed_image_and_text(custom_format, include_text, original_marker)?;
        let original_dib = read_bytes(8)?;
        let original_custom = read_bytes(custom_format)?;
        let marker_bytes = if original_marker {
            Some(read_bytes(history_format()?)?)
        } else {
            None
        };
        let snapshot = backup_and_write("Temporary transcription", &cancellation, force_write)
            .map_err(|error| error.to_string())?;
        assert_history_excluded()?;
        if read_changed(0)?.as_deref() != Some("Temporary transcription") {
            return Err("Temporary paste text differs from expected".into());
        }
        snapshot.restore()?;
        drop(snapshot);
        assert_history_excluded()?;
        if include_text {
            if read_changed(0)?.as_deref() != Some("Original text — 原始文本") {
                return Err("Original clipboard text was not restored".into());
            }
        } else {
            let _clipboard = open(None, Duration::from_secs(1))?;
            if unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) }.is_ok() {
                return Err("An image-only clipboard gained text data".into());
            }
        }
        if read_bytes(8)? != original_dib || read_bytes(custom_format)? != original_custom {
            return Err("Original image or registered format was not restored".into());
        }
        if let Some(original_marker) = marker_bytes
            && read_bytes(history_format()?)? != original_marker
        {
            return Err("Original exclusion marker was not preserved".into());
        }
    }

    seed_image_and_text(custom_format, true, false)?;
    // A new Copy can contain exactly the same text as the temporary output.
    // Its identity, rather than a text comparison, must control restoration.
    let snapshot = backup_and_write("Copied selection — 选中的文本", &cancellation, false)
        .map_err(|error| error.to_string())?;
    if !snapshot.temporary_is_current()? {
        return Err("The temporary clipboard was not recognized".into());
    }
    native_writer(&AtomicBool::new(false))?;
    if snapshot.temporary_is_current()? {
        return Err("A newer copy was mistaken for temporary output".into());
    }
    snapshot.restore()?;
    if read_changed(0)?.as_deref() != Some("Copied selection — 选中的文本") {
        return Err("Restoration overwrote a newer clipboard copy".into());
    }

    // An originally empty clipboard must not become an empty text item.
    {
        let owner = OwnerWindow::new()?;
        let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
        unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
    }
    let snapshot = backup_and_write("Temporary transcription", &cancellation, false)
        .map_err(|error| error.to_string())?;
    drop(snapshot); // Also exercise the best-effort Drop cleanup.
    assert_history_excluded()?;
    let _clipboard = open(None, Duration::from_secs(1))?;
    if unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) }.is_ok()
        || unsafe { IsClipboardFormatAvailable(8) }.is_ok()
    {
        return Err("Restoring an empty clipboard created text or image data".into());
    }
    Ok(())
}

#[test]
#[ignore = "uses the real Windows desktop clipboard; run manually with --ignored --test-threads=1"]
fn native_output_restores_image_text_and_history_markers() -> Result<(), String> {
    with_saved_clipboard(exercise_native_output)
}

fn exercise_selection_markers() -> Result<(), String> {
    let cancellation = CancellationToken::new();
    let markers = unsafe {
        [
            RegisterClipboardFormatW(w!("MSDEVColumnSelect")),
            RegisterClipboardFormatW(w!("MSDEVLineSelect")),
            RegisterClipboardFormatW(w!("VisualStudioEditorOperationsLineCutCopyClipboardTag")),
        ]
    };
    if markers.contains(&0) {
        return Err("Cannot register selection marker formats".into());
    }

    for marker in markers {
        for write_output in [false, true] {
            let owner = OwnerWindow::new()?;
            {
                let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
                unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
                set_text("Original selection")?;
                // Scintilla advertises the format with a null handle and does
                // not render data for it when the format is requested.
                let _ = unsafe { SetClipboardData(marker, None) };
                if marker == markers[1] {
                    // Scintilla advertises both markers for a whole-line copy.
                    let _ = unsafe { SetClipboardData(markers[2], None) };
                }
                unsafe { IsClipboardFormatAvailable(marker) }
                    .map_err(|_| "Selection marker was not advertised".to_owned())?;
            }
            let snapshot = if write_output {
                backup_and_write("Temporary output", &cancellation, false)
                    .map_err(|error| error.to_string())?
            } else {
                backup_and_clear().map_err(|error| error.to_string())?.0
            };
            drop(owner);
            snapshot.restore()?;
            if read_changed(0)?.as_deref() != Some("Original selection") {
                return Err("Text with a selection marker was not restored".into());
            }
            if read_bytes(marker)?.first() != Some(&0) {
                return Err("Selection marker was not restored".into());
            }
            if marker == markers[1] && read_bytes(markers[2])?.first() != Some(&0) {
                return Err("Additional whole-line marker was not restored".into());
            }
            assert_history_excluded()?;
        }
    }

    // Visual Studio and other producers may attach real data to the same
    // formats. Preserve those bytes when the handle can be read.
    let owner = OwnerWindow::new()?;
    {
        let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
        unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
        set_text("Original selection")?;
        set_bytes(markers[0], b"selection-marker-data")?;
    }
    let original_marker = read_bytes(markers[0])?;
    let snapshot = backup_and_write("Temporary output", &cancellation, false)
        .map_err(|error| error.to_string())?;
    drop(owner);
    snapshot.restore()?;
    if read_bytes(markers[0])? != original_marker {
        return Err("Selection marker data was not preserved".into());
    }

    Ok(())
}

#[test]
#[ignore = "uses the real Windows desktop clipboard; run manually with --ignored --test-threads=1"]
fn native_selection_markers_survive_copy_and_paste_transactions() -> Result<(), String> {
    with_saved_clipboard(exercise_selection_markers)
}

fn exercise_unrendered_format() -> Result<(), String> {
    let unknown =
        unsafe { RegisterClipboardFormatW(w!("Dictate.NativeRegression.UnrenderedFormat")) };
    if unknown == 0 {
        return Err("Cannot register unrendered test format".into());
    }
    let owner = OwnerWindow::new()?;
    {
        let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
        unsafe { EmptyClipboard() }.map_err(|error| error.to_string())?;
        set_text("Original selection")?;
        let _ = unsafe { SetClipboardData(unknown, None) };
        unsafe { IsClipboardFormatAvailable(unknown) }
            .map_err(|_| "Unrendered test format was not advertised".to_owned())?;
    }
    let cancellation = CancellationToken::new();
    assert!(matches!(
        backup_and_write("Temporary output", &cancellation, false),
        Err(BeginError::Backup(_))
    ));
    assert!(matches!(backup_and_clear(), Err(BeginError::Backup(_))));
    let canceled = CancellationToken::new();
    canceled.cancel();
    assert!(matches!(
        backup_and_write("Temporary output", &canceled, true),
        Err(BeginError::Canceled)
    ));
    assert_eq!(read_changed(0)?.as_deref(), Some("Original selection"));

    let snapshot = backup_and_write("Temporary output", &cancellation, true)
        .map_err(|error| error.to_string())?;
    drop(owner);
    assert!(snapshot.temporary_is_current()?);
    snapshot.restore()?;
    drop(snapshot);
    assert_history_excluded()?;
    assert_eq!(read_changed(0)?.as_deref(), Some("Temporary output"));
    let _clipboard = open(None, Duration::from_secs(1))?;
    assert!(unsafe { IsClipboardFormatAvailable(unknown) }.is_err());
    Ok(())
}

#[test]
#[ignore = "uses the real Windows desktop clipboard; run manually with --ignored --test-threads=1"]
fn native_unrendered_format_can_be_overwritten_when_forced() -> Result<(), String> {
    with_saved_clipboard(exercise_unrendered_format)
}
