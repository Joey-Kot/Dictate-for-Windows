#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
#[cfg_attr(all(test, not(windows)), allow(dead_code))]
mod advanced_audio_prompt;
#[cfg(any(windows, test))]
mod audio_options;
#[cfg(any(windows, test))]
mod debug_log;
#[cfg(all(test, not(windows)))]
mod dropdown_scrollbar;
#[cfg(windows)]
mod i18n;
#[cfg(windows)]
mod platform;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod resources;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod taskbar;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    let _debug_logs = debug_log::install();
    if let Err(error) = window::run() {
        let message = format!("Dictate failed to start:\n{error}");
        let wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
            use windows::core::PCWSTR;
            let _ = MessageBoxW(
                None,
                PCWSTR(wide.as_ptr()),
                windows::core::w!("Dictate"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Dictate is a Windows-only native GUI");
}
