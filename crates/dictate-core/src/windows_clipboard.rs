//! Materialized clipboard snapshots for temporary Copy and Paste transactions.
//!
//! Do not retain an IDataObject here: its delayed formats can become unavailable
//! as soon as EmptyClipboard tells the original owner to release its data.

use std::{
    cell::Cell,
    mem::size_of,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            ERROR_SUCCESS, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND, SetLastError,
        },
        Graphics::Gdi::{
            BITMAP, CopyEnhMetaFileW, DeleteEnhMetaFile, DeleteMetaFile, DeleteObject,
            GetEnhMetaFileBits, GetMetaFileBitsEx, GetObjectW, GetPaletteEntries, HENHMETAFILE,
            HGDIOBJ, HPALETTE,
        },
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
                GetClipboardSequenceNumber, IsClipboardFormatAvailable, METAFILEPICT,
                OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
            },
            Memory::{
                GMEM_MOVEABLE, GlobalAlloc, GlobalFlags, GlobalLock, GlobalSize, GlobalUnlock,
            },
            Ole::{CLIPBOARD_FORMAT, OleDuplicateData},
        },
        UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE, WS_OVERLAPPED},
    },
    core::{PCWSTR, w},
};

// Windows can expose a single large image in several bitmap/DIB formats.
// Allow typical 4K screenshots while still bounding all duplicated formats.
const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;
const MAX_FORMATS: usize = 256;
const MAX_TEXT_BYTES: usize = 1_000_000;
const UNICODE_TEXT: u32 = 13;

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

struct OwnerWindow(HWND);

impl OwnerWindow {
    fn new() -> Result<Self, String> {
        // SetClipboardData requires a non-null owner after EmptyClipboard.
        unsafe {
            CreateWindowExW(
                Default::default(),
                w!("STATIC"),
                w!("Dictate temporary clipboard"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
            .map(Self)
            .map_err(|error| format!("Cannot create clipboard owner: {error}"))
        }
    }
}

impl Drop for OwnerWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

fn open(owner: Option<HWND>, timeout: Duration) -> Result<ClipboardGuard, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match unsafe { OpenClipboard(owner) } {
            Ok(()) => return Ok(ClipboardGuard),
            Err(error) if Instant::now() >= deadline => {
                return Err(format!("Cannot open clipboard: {error}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[derive(Clone, Copy)]
enum DataKind {
    Global,
    Bitmap,
    Palette,
    Metafile,
    EnhancedMetafile,
}

impl DataKind {
    fn for_format(format: u32) -> Result<Self, String> {
        match format {
            2 | 130 => Ok(Self::Bitmap),
            9 => Ok(Self::Palette),
            3 | 131 => Ok(Self::Metafile),
            14 | 142 => Ok(Self::EnhancedMetafile),
            1 | 4..=8 | 10..=13 | 15..=17 | 129 | 0xc000..=0xffff => Ok(Self::Global),
            // Private/owner-display formats may contain process-local handles
            // and require the original owner's cleanup or rendering callbacks.
            _ => Err(format!(
                "Cannot safely back up clipboard format {format}; clipboard was left unchanged"
            )),
        }
    }

    fn size(self, handle: HANDLE) -> Result<usize, String> {
        let bytes = unsafe {
            match self {
                Self::Global => {
                    // Zero-length registered marker formats are valid HGLOBALs.
                    if GlobalFlags(HGLOBAL(handle.0)) == 0x8000 {
                        return Err("Invalid clipboard memory handle".to_owned());
                    }
                    return Ok(GlobalSize(HGLOBAL(handle.0)));
                }
                Self::Bitmap => {
                    let mut bitmap = BITMAP::default();
                    if GetObjectW(
                        HGDIOBJ(handle.0),
                        size_of::<BITMAP>() as i32,
                        Some((&mut bitmap as *mut BITMAP).cast()),
                    ) == 0
                    {
                        return Err("Cannot inspect clipboard bitmap".to_owned());
                    }
                    // Bound both native storage and a possible 32-bit duplicate.
                    let pixels = (bitmap.bmWidth.unsigned_abs() as usize)
                        .checked_mul(bitmap.bmHeight.unsigned_abs() as usize)
                        .and_then(|n| n.checked_mul(4));
                    let native = (bitmap.bmWidthBytes.unsigned_abs() as usize)
                        .checked_mul(bitmap.bmHeight.unsigned_abs() as usize)
                        .and_then(|n| n.checked_mul(bitmap.bmPlanes as usize));
                    pixels
                        .zip(native)
                        .map(|(a, b)| a.max(b))
                        .unwrap_or(usize::MAX)
                }
                Self::Palette => GetPaletteEntries(HPALETTE(handle.0), 0, None) as usize * 4,
                Self::Metafile => {
                    let storage = GlobalSize(HGLOBAL(handle.0));
                    if storage < size_of::<METAFILEPICT>() {
                        return Err("Invalid clipboard metafile".to_owned());
                    }
                    let lock = GlobalLockGuard::new(HGLOBAL(handle.0))?;
                    let pict = lock.pointer.cast::<METAFILEPICT>().read();
                    let bits = GetMetaFileBitsEx(pict.hMF, 0, None) as usize;
                    if bits == 0 {
                        return Err("Cannot inspect clipboard metafile".to_owned());
                    }
                    storage.saturating_add(bits)
                }
                Self::EnhancedMetafile => GetEnhMetaFileBits(HENHMETAFILE(handle.0), None) as usize,
            }
        };
        if bytes == 0 {
            Err("Cannot determine clipboard data size".to_owned())
        } else {
            Ok(bytes)
        }
    }
}

struct ClipboardData {
    format: u32,
    kind: DataKind,
    handle: Cell<HANDLE>,
}

impl ClipboardData {
    fn from_bytes(format: u32, bytes: &[u8]) -> Result<Self, String> {
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }
            .map_err(|error| format!("Cannot allocate clipboard data: {error}"))?;
        let data = Self {
            format,
            kind: DataKind::Global,
            handle: Cell::new(HANDLE(memory.0)),
        };
        let lock = GlobalLockGuard::new(memory)?;
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), lock.pointer.cast(), bytes.len());
        }
        drop(lock);
        Ok(data)
    }

    fn duplicate(format: u32, kind: DataKind, source: HANDLE) -> Result<Self, String> {
        let handle = unsafe {
            match kind {
                DataKind::Global if GlobalSize(HGLOBAL(source.0)) == 0 => HANDLE(
                    GlobalAlloc(GMEM_MOVEABLE, 0)
                        .map_err(|error| format!("Cannot back up clipboard marker: {error}"))?
                        .0,
                ),
                DataKind::EnhancedMetafile => {
                    HANDLE(CopyEnhMetaFileW(HENHMETAFILE(source.0), PCWSTR::null()).0)
                }
                _ => {
                    // The display variants have the same underlying handle types.
                    let canonical = match kind {
                        DataKind::Bitmap => 2,
                        DataKind::Palette => 9,
                        DataKind::Metafile => 3,
                        _ => format,
                    };
                    OleDuplicateData(source, CLIPBOARD_FORMAT(canonical as u16), GMEM_MOVEABLE)
                }
            }
        };
        if handle.is_invalid() {
            return Err(format!("Cannot back up clipboard format {format}"));
        }
        Ok(Self {
            format,
            kind,
            handle: Cell::new(handle),
        })
    }

    fn set(&self) -> Result<(), String> {
        unsafe { SetClipboardData(self.format, Some(self.handle.get())) }
            .map_err(|error| format!("format {}: {error}", self.format))?;
        // Ownership transfers to Windows only after SetClipboardData succeeds.
        self.handle.set(HANDLE::default());
        Ok(())
    }
}

impl Drop for ClipboardData {
    fn drop(&mut self) {
        let handle = self.handle.get();
        if handle.is_invalid() {
            return;
        }
        unsafe {
            match self.kind {
                DataKind::Bitmap | DataKind::Palette => {
                    let _ = DeleteObject(HGDIOBJ(handle.0));
                }
                DataKind::EnhancedMetafile => {
                    let _ = DeleteEnhMetaFile(Some(HENHMETAFILE(handle.0)));
                }
                DataKind::Metafile => {
                    if let Ok(lock) = GlobalLockGuard::new(HGLOBAL(handle.0)) {
                        let _ = DeleteMetaFile(lock.pointer.cast::<METAFILEPICT>().read().hMF);
                    }
                    let _ = GlobalFree(Some(HGLOBAL(handle.0)));
                }
                DataKind::Global => {
                    let _ = GlobalFree(Some(HGLOBAL(handle.0)));
                }
            }
        }
    }
}

struct GlobalLockGuard {
    handle: HGLOBAL,
    pointer: *mut std::ffi::c_void,
}

impl GlobalLockGuard {
    fn new(handle: HGLOBAL) -> Result<Self, String> {
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            Err("Cannot lock clipboard data".to_owned())
        } else {
            Ok(Self { handle, pointer })
        }
    }
}

impl Drop for GlobalLockGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalUnlock(self.handle);
        }
    }
}

pub(crate) struct ClipboardSnapshot {
    data: Vec<ClipboardData>,
    history_marker: ClipboardData,
    restored: Cell<bool>,
    temporary_id: Option<[u8; 16]>,
}

// The snapshot exclusively owns duplicated process-wide HGLOBAL/GDI handles.
// It contains no HWND or borrowed clipboard data; an owner window is created
// and destroyed within each synchronous write on the thread performing it.
unsafe impl Send for ClipboardSnapshot {}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BeginError {
    #[error("{0}")]
    Backup(String),
    #[error("{0}")]
    Replace(String),
    #[error("{operation}; restoring clipboard failed: {restore}")]
    ReplaceAndRestore { operation: String, restore: String },
}

fn is_presence_only_marker(format: u32) -> bool {
    // Scintilla advertises these formats with a null handle. Their presence
    // alone indicates a rectangular or whole-line selection.
    unsafe {
        format != 0
            && (format == RegisterClipboardFormatW(w!("MSDEVColumnSelect"))
                || format == RegisterClipboardFormatW(w!("MSDEVLineSelect"))
                || format
                    == RegisterClipboardFormatW(w!(
                        "VisualStudioEditorOperationsLineCutCopyClipboardTag"
                    )))
    }
}

fn snapshot(history_marker: ClipboardData) -> Result<ClipboardSnapshot, String> {
    let mut formats = Vec::new();
    let mut previous = 0;
    loop {
        unsafe {
            SetLastError(ERROR_SUCCESS);
        }
        let format = unsafe { EnumClipboardFormats(previous) };
        if format == 0 {
            if unsafe { GetLastError() } != ERROR_SUCCESS {
                return Err("Cannot enumerate clipboard formats".to_owned());
            }
            break;
        }
        if formats.len() >= MAX_FORMATS {
            return Err("Clipboard has too many formats to back up safely".to_owned());
        }
        formats.push((format, DataKind::for_format(format)?));
        previous = format;
    }
    let mut data = Vec::with_capacity(formats.len());
    let mut total = 0usize;
    for (format, kind) in formats {
        // Materialize delayed formats while the original clipboard owner exists.
        let handle = match unsafe { GetClipboardData(format) } {
            Ok(handle) => handle,
            Err(_) if is_presence_only_marker(format) => {
                // A real, nonempty handle keeps the marker available after the
                // original clipboard owner has gone away.
                data.push(ClipboardData::from_bytes(format, &[0])?);
                continue;
            }
            Err(error) => return Err(format!("Cannot read clipboard format {format}: {error}")),
        };
        total = total.saturating_add(kind.size(handle)?);
        if total > MAX_SNAPSHOT_BYTES {
            return Err(
                "Clipboard backup exceeds the 256 MiB limit; clipboard was left unchanged"
                    .to_owned(),
            );
        }
        data.push(ClipboardData::duplicate(format, kind, handle)?);
    }
    Ok(ClipboardSnapshot {
        data,
        history_marker,
        // Until EmptyClipboard succeeds there is nothing to restore.
        restored: Cell::new(true),
        temporary_id: None,
    })
}

fn begin(temporary_text: Option<&str>) -> Result<(ClipboardSnapshot, u32), BeginError> {
    // Windows checks this registered format when processing clipboard changes.
    // Allocate both markers and the text before touching the existing clipboard,
    // including a spare marker for restoration if the temporary write fails.
    let marker_format =
        unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
    if marker_format == 0 {
        return Err(BeginError::Replace(
            "Cannot register clipboard history exclusion format".into(),
        ));
    }
    // The exclusion format accepts arbitrary bytes. A unique value lets us
    // distinguish the temporary output from a later normal clipboard copy.
    let temporary_id = temporary_text.map(|_| *uuid::Uuid::new_v4().as_bytes());
    let temporary_marker = ClipboardData::from_bytes(
        marker_format,
        temporary_id.as_ref().map_or(&[0][..], |id| &id[..]),
    )
    .map_err(BeginError::Replace)?;
    let restore_marker =
        ClipboardData::from_bytes(marker_format, &[0]).map_err(BeginError::Replace)?;
    let text = temporary_text
        .map(|text| {
            let bytes: Vec<u8> = text
                .encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(u16::to_le_bytes)
                .collect();
            ClipboardData::from_bytes(UNICODE_TEXT, &bytes)
        })
        .transpose()
        .map_err(BeginError::Replace)?;

    // The window must be gone before waiting for another process to Copy; a
    // live owner on a worker without a message loop can block that process.
    let owner = OwnerWindow::new().map_err(BeginError::Replace)?;
    let clipboard = open(Some(owner.0), Duration::from_millis(1000)).map_err(BeginError::Backup)?;
    let mut snapshot = snapshot(restore_marker).map_err(BeginError::Backup)?;
    unsafe { EmptyClipboard() }
        .map_err(|error| BeginError::Replace(format!("Cannot clear clipboard: {error}")))?;
    snapshot.restored.set(false);

    let write = temporary_marker
        .set()
        .and_then(|()| text.as_ref().map_or(Ok(()), ClipboardData::set));
    if let Err(operation) = write {
        let restore = snapshot.restore_open();
        // If restoration failed before clearing, release the open clipboard
        // before the snapshot's Drop makes a final best-effort attempt.
        drop(clipboard);
        drop(owner);
        return match restore {
            Ok(()) => Err(BeginError::Replace(operation)),
            Err(restore) => Err(BeginError::ReplaceAndRestore { operation, restore }),
        };
    }
    snapshot.temporary_id = temporary_id;
    drop(clipboard);
    drop(owner);
    let sequence = unsafe { GetClipboardSequenceNumber() };
    Ok((snapshot, sequence))
}

pub(crate) fn backup_and_clear() -> Result<(ClipboardSnapshot, u32), BeginError> {
    begin(None)
}

pub(crate) fn backup_and_write(text: &str) -> Result<ClipboardSnapshot, BeginError> {
    begin(Some(text)).map(|(snapshot, _)| snapshot)
}

pub(crate) fn read_changed(sequence: u32) -> Result<Option<String>, String> {
    if unsafe { OpenClipboard(None) }.is_err() {
        return Ok(None);
    }
    let _clipboard = ClipboardGuard;
    if unsafe { IsClipboardFormatAvailable(UNICODE_TEXT) }.is_err() {
        return Ok((unsafe { GetClipboardSequenceNumber() } != sequence).then(String::new));
    }
    // The clipboard was fully cleared before Copy, so an advertised text format
    // is new even when its data uses delayed rendering. Such providers may not
    // advance the sequence until GetClipboardData asks them to render it.
    let handle = unsafe { GetClipboardData(UNICODE_TEXT) }
        .map_err(|error| format!("Cannot read copied text: {error}"))?;
    let bytes = unsafe { GlobalSize(HGLOBAL(handle.0)) };
    if bytes < size_of::<u16>() || bytes % size_of::<u16>() != 0 {
        return Err("Copied text has invalid UTF-16 storage".to_owned());
    }
    let lock = GlobalLockGuard::new(HGLOBAL(handle.0))?;
    let units = unsafe {
        std::slice::from_raw_parts(
            lock.pointer.cast::<u16>(),
            (bytes / size_of::<u16>()).min(MAX_TEXT_BYTES + 1),
        )
    };
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .ok_or_else(|| "Copied text exceeds the limit or has no terminator".to_owned())?;
    let text = String::from_utf16_lossy(&units[..end]);
    if text.len() > MAX_TEXT_BYTES {
        return Err("Selected text is too large".to_owned());
    }
    Ok(Some(text))
}

impl ClipboardSnapshot {
    // Called only while the clipboard is open.
    fn temporary_is_current_open(&self) -> Result<bool, String> {
        let Some(id) = self.temporary_id else {
            return Ok(true);
        };
        if unsafe { IsClipboardFormatAvailable(self.history_marker.format) }.is_err() {
            return Ok(false);
        }
        let handle = unsafe { GetClipboardData(self.history_marker.format) }
            .map_err(|error| format!("Cannot check temporary clipboard: {error}"))?;
        let memory = HGLOBAL(handle.0);
        if unsafe { GlobalSize(memory) } < id.len() {
            return Ok(false);
        }
        let lock = GlobalLockGuard::new(memory)?;
        let bytes = unsafe { std::slice::from_raw_parts(lock.pointer.cast::<u8>(), id.len()) };
        Ok(bytes == id)
    }

    pub(crate) fn temporary_is_current(&self) -> Result<bool, String> {
        let _clipboard = open(None, Duration::from_secs(1))?;
        self.temporary_is_current_open()
    }

    pub(crate) fn restore(&self) -> Result<(), String> {
        if self.restored.get() {
            return Ok(());
        }
        // SetClipboardData needs an owner, but only during this write. All data
        // is materialized; no window needs to survive for delayed rendering.
        // Declare the owner first so the clipboard closes before it is destroyed.
        let owner = OwnerWindow::new()?;
        let _clipboard = open(Some(owner.0), Duration::from_secs(1))?;
        if !self.temporary_is_current_open()? {
            // Another application has replaced our temporary output. Keep the
            // newer clipboard rather than putting the older snapshot over it.
            self.restored.set(true);
            return Ok(());
        }
        self.restore_open()
    }

    // Called only while a clipboard owner has the clipboard open.
    fn restore_open(&self) -> Result<(), String> {
        unsafe { EmptyClipboard() }
            .map_err(|error| format!("Cannot restore clipboard: {error}"))?;
        let mut failures = Vec::new();
        // Reuse an original exclusion format if present. Otherwise add one so
        // restoring an old item does not create another history/cloud entry.
        let original_marker = self
            .data
            .iter()
            .find(|data| data.format == self.history_marker.format);
        if let Err(error) = original_marker.unwrap_or(&self.history_marker).set() {
            failures.push(error);
        }
        for data in &self.data {
            if data.format != self.history_marker.format
                && let Err(error) = data.set()
            {
                failures.push(error);
            }
        }
        // Never clear a partially restored clipboard on a subsequent call.
        self.restored.set(true);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Cannot fully restore clipboard ({})",
                failures.join("; ")
            ))
        }
    }
}

impl Drop for ClipboardSnapshot {
    fn drop(&mut self) {
        // A dropped async output task must still make a best effort to restore.
        if !self.restored.get() {
            let _ = self.restore();
        }
    }
}

#[cfg(test)]
mod tests;
