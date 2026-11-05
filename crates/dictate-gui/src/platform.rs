use std::ffi::c_void;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CLOAKED, DWMWA_COLOR_NONE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::IntersectRect;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GW_HWNDPREV, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HWND_TOP, HWND_TOPMOST, IsIconic, IsWindowVisible, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SetWindowPos, WS_EX_TOPMOST,
};

pub fn enable_high_dpi() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn system_dpi() -> u32 {
    unsafe { GetDpiForSystem() }.max(96)
}

pub fn window_dpi(hwnd: HWND) -> u32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

pub fn scale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}

pub fn scale_with_factor(value: i32, dpi: u32, factor: f64) -> i32 {
    (f64::from(value) * f64::from(dpi) * factor / 96.0).round() as i32
}

pub fn unscale(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * 96 + i64::from(dpi / 2)) / i64::from(dpi.max(1))) as i32
}

pub fn unscale_with_factor(value: i32, dpi: u32, factor: f64) -> i32 {
    (f64::from(value) * 96.0 / (f64::from(dpi.max(1)) * factor)).round() as i32
}

pub fn window_opacity_alpha(opacity: f64) -> u8 {
    (opacity.clamp(0.1, 1.0) * 255.0).round() as u8
}

pub fn maintain_overlay_z_order(hwnd: HWND) {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return;
        }
        let topmost = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0;
        if (topmost && !covered_by_other_app(hwnd)) || is_cloaked(hwnd) {
            return;
        }
        let insert_after = if topmost { HWND_TOP } else { HWND_TOPMOST };
        let _ = SetWindowPos(
            hwnd,
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

fn covered_by_other_app(hwnd: HWND) -> bool {
    unsafe {
        let mut overlay_bounds = RECT::default();
        if GetWindowRect(hwnd, &mut overlay_bounds).is_err() {
            return false;
        }
        let own_process = std::process::id();
        let mut below = hwnd;
        // The Z order can change while walking it. Bound the work of each check.
        for _ in 0..256 {
            let Ok(above) = GetWindow(below, GW_HWNDPREV) else {
                break;
            };
            if above == hwnd {
                break;
            }
            below = above;
            let mut bounds = RECT::default();
            if !IsWindowVisible(above).as_bool()
                || IsIconic(above).as_bool()
                || GetWindowRect(above, &mut bounds).is_err()
                || !IntersectRect(&mut RECT::default(), &overlay_bounds, &bounds).as_bool()
            {
                continue;
            }
            let mut process = 0;
            if GetWindowThreadProcessId(above, Some(&mut process)) != 0
                && process != own_process
                && !is_cloaked(above)
            {
                return true;
            }
        }
        false
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0_u32;
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::from_mut(&mut cloaked).cast(),
            size_of::<u32>() as u32,
        )
    };
    result.is_ok() && cloaked != 0
}

pub fn disable_native_window_frame(hwnd: HWND) {
    unsafe {
        let preference = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const c_void,
            std::mem::size_of_val(&preference) as u32,
        );
        let border_color = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const _ as *const c_void,
            std::mem::size_of_val(&border_color) as u32,
        );
    }
}

pub fn apply_dark_mode(hwnd: HWND) {
    let enabled = 1_i32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &enabled as *const _ as *const c_void,
            std::mem::size_of_val(&enabled) as u32,
        );
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    let app_data = std::env::var_os("APPDATA").ok_or("APPDATA is not available")?;
    Ok(PathBuf::from(app_data).join("dictate").join("config.json"))
}

pub use dictate_core::embedded_ffmpeg::EmbeddedFfmpegConverter as GuiLibAvConverter;
