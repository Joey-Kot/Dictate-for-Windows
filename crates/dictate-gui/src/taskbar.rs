use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_FORCE_ICONIC_REPRESENTATION, DWMWA_HAS_ICONIC_BITMAP, DWMWINDOWATTRIBUTE,
    DwmInvalidateIconicBitmaps, DwmSetIconicLivePreviewBitmap, DwmSetIconicThumbnail,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

const MAX_THUMBNAIL_EDGE: u32 = 4096;

pub fn set_visible(hwnd: HWND, visible: bool) -> windows::core::Result<()> {
    unsafe {
        let taskbar: ITaskbarList = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
        taskbar.HrInit()?;
        if visible {
            taskbar.AddTab(hwnd)
        } else {
            taskbar.DeleteTab(hwnd)
        }
    }
}

pub fn enable_custom_previews(hwnd: HWND) -> bool {
    let result = set_iconic_attribute(hwnd, DWMWA_HAS_ICONIC_BITMAP, true)
        .and_then(|()| set_iconic_attribute(hwnd, DWMWA_FORCE_ICONIC_REPRESENTATION, true));
    if result.is_err() {
        use_default_previews(hwnd);
    }
    result.is_ok()
}

pub fn use_default_previews(hwnd: HWND) {
    let _ = set_iconic_attribute(hwnd, DWMWA_FORCE_ICONIC_REPRESENTATION, false);
    let _ = set_iconic_attribute(hwnd, DWMWA_HAS_ICONIC_BITMAP, false);
}

fn set_iconic_attribute(
    hwnd: HWND,
    attribute: DWMWINDOWATTRIBUTE,
    enabled: bool,
) -> windows::core::Result<()> {
    let enabled = i32::from(enabled);
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            std::ptr::from_ref(&enabled).cast(),
            size_of::<i32>() as u32,
        )
    }
}

pub fn invalidate_previews(hwnd: HWND) {
    let _ = unsafe { DwmInvalidateIconicBitmaps(hwnd) };
}

#[derive(Clone, Copy)]
pub enum Preview {
    Thumbnail(LPARAM),
    Peek,
}

impl Preview {
    pub fn size(self, hwnd: HWND, panel: (i32, i32)) -> windows::core::Result<(u32, u32)> {
        match self {
            Self::Thumbnail(size) => {
                let max_width = ((size.0 >> 16 & 0xffff) as u32).min(MAX_THUMBNAIL_EDGE);
                let max_height = ((size.0 & 0xffff) as u32).min(MAX_THUMBNAIL_EDGE);
                let (width, height) = (panel.0 as f32, panel.1 as f32);
                let scale = (max_width as f32 / width).min(max_height as f32 / height);
                Ok((
                    ((width * scale).round() as u32).max(1).min(max_width),
                    ((height * scale).round() as u32).max(1).min(max_height),
                ))
            }
            Self::Peek => {
                let mut client = RECT::default();
                unsafe { GetClientRect(hwnd, &mut client)? };
                Ok((client.right.max(0) as u32, client.bottom.max(0) as u32))
            }
        }
    }

    pub fn publish(self, hwnd: HWND, bitmap: HBITMAP) -> windows::core::Result<()> {
        unsafe {
            match self {
                Self::Thumbnail(_) => DwmSetIconicThumbnail(hwnd, bitmap, 0),
                Self::Peek => DwmSetIconicLivePreviewBitmap(hwnd, bitmap, None, 0),
            }
        }
    }
}
