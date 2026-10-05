use std::mem::size_of;

use crate::i18n::Language;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetForegroundWindow, MF_CHECKED,
    MF_STRING, MF_UNCHECKED, SetForegroundWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu,
};

use crate::resources;

pub const WM_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 11;
pub const COMMAND_MINIMAL: usize = 0x5101;
pub const COMMAND_SETTINGS: usize = 0x5102;
pub const COMMAND_QUIT: usize = 0x5103;

pub struct TrayIcon {
    data: NOTIFYICONDATAW,
}

impl TrayIcon {
    pub fn create(hwnd: HWND) -> windows::core::Result<Self> {
        let icon = resources::load_app_icon()?;
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: icon,
            ..Default::default()
        };
        write_wide(&mut data.szTip, "Dictate");
        unsafe {
            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                return Err(windows::core::Error::from_win32());
            }
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
        Ok(Self { data })
    }

    pub fn show_menu(&self, minimal: bool, language: Language) -> Option<usize> {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return None;
            };
            let minimal_flags = MF_STRING | if minimal { MF_CHECKED } else { MF_UNCHECKED };
            let minimal_text = wide(language.text("minimal"));
            let settings_text = wide(language.text("settings"));
            let quit_text = wide(language.text("quit"));
            let _ = AppendMenuW(
                menu,
                minimal_flags,
                COMMAND_MINIMAL,
                windows::core::PCWSTR(minimal_text.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                COMMAND_SETTINGS,
                windows::core::PCWSTR(settings_text.as_ptr()),
            );
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                COMMAND_QUIT,
                windows::core::PCWSTR(quit_text.as_ptr()),
            );
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            let previous_foreground = GetForegroundWindow();
            let _ = SetForegroundWindow(self.data.hWnd);
            // Receive the command only after the menu closes, so focus can be restored before
            // any command opens a window or changes the floating overlay.
            let command = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                self.data.hWnd,
                None,
            )
            .0 as usize;
            let _ = DestroyMenu(menu);
            if !previous_foreground.is_invalid() && previous_foreground != self.data.hWnd {
                let _ = SetForegroundWindow(previous_foreground);
            }
            (command != 0).then_some(command)
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
        }
    }
}

fn write_wide(target: &mut [u16], text: &str) {
    for (target, source) in target
        .iter_mut()
        .zip(text.encode_utf16().chain(std::iter::once(0)))
    {
        *target = source;
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
