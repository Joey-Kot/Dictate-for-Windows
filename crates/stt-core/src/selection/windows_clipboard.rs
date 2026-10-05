//! Materialized clipboard snapshots for the short Ctrl+C selection transaction.
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
                OpenClipboard, SetClipboardData,
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

const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
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
                w!("STT selection clipboard"),
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

pub(super) struct ClipboardSnapshot {
    data: Vec<ClipboardData>,
    owner: OwnerWindow,
    restored: Cell<bool>,
}

pub(super) fn backup_and_clear() -> Result<(ClipboardSnapshot, u32), String> {
    let owner = OwnerWindow::new()?;
    let _clipboard = open(Some(owner.0), Duration::from_millis(300))?;
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
        let handle = unsafe { GetClipboardData(format) }
            .map_err(|error| format!("Cannot read clipboard format {format}: {error}"))?;
        total = total.saturating_add(kind.size(handle)?);
        if total > MAX_SNAPSHOT_BYTES {
            return Err(
                "Clipboard backup exceeds the 64 MiB limit; clipboard was left unchanged"
                    .to_owned(),
            );
        }
        data.push(ClipboardData::duplicate(format, kind, handle)?);
    }
    // All data has independent, owned storage before the destructive operation.
    unsafe { EmptyClipboard() }.map_err(|error| format!("Cannot clear clipboard: {error}"))?;
    // Capture the baseline only after our own clipboard transaction is closed.
    drop(_clipboard);
    let sequence = unsafe { GetClipboardSequenceNumber() };
    Ok((
        ClipboardSnapshot {
            data,
            owner,
            restored: Cell::new(false),
        },
        sequence,
    ))
}

pub(super) fn read_changed(sequence: u32) -> Result<Option<String>, String> {
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
    pub(super) fn restore(&self) -> Result<(), String> {
        if self.restored.get() {
            return Ok(());
        }
        let _clipboard = open(Some(self.owner.0), Duration::from_secs(1))?;
        unsafe { EmptyClipboard() }
            .map_err(|error| format!("Cannot restore clipboard: {error}"))?;
        let mut failures = Vec::new();
        for data in &self.data {
            match unsafe { SetClipboardData(data.format, Some(data.handle.get())) } {
                Ok(_) => data.handle.set(HANDLE::default()), // Ownership transfers to Windows.
                Err(error) => failures.push(format!("format {}: {error}", data.format)),
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
