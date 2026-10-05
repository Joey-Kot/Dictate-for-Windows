use std::ffi::c_void;
use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
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
