use windows::Win32::{
    Foundation::{GetLastError, HWND, SetLastError, WIN32_ERROR},
    UI::{
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::{
            GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
        },
    },
};

use super::{CopyBackend, windows_clipboard};

pub(super) struct WindowsCopy {
    foreground: usize,
    focus: usize,
}

impl WindowsCopy {
    pub(super) fn capture() -> Result<Self, String> {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.is_invalid() {
                return Err("No foreground window to copy from".into());
            }
            let focus = focused_window(foreground)?;
            Ok(Self {
                foreground: foreground.0 as usize,
                focus: focus.0 as usize,
            })
        }
    }
}

fn focused_window(foreground: HWND) -> Result<HWND, String> {
    unsafe {
        let thread = GetWindowThreadProcessId(foreground, None);
        if thread == 0 {
            return Err("Cannot find the foreground window's thread".into());
        }
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread, &mut info)
            .map_err(|error| format!("Cannot read keyboard focus: {error}"))?;
        if info.hwndFocus.is_invalid() {
            return Err("No focused control to copy from".into());
        }
        Ok(info.hwndFocus)
    }
}

impl CopyBackend for WindowsCopy {
    type Snapshot = windows_clipboard::ClipboardSnapshot;

    fn target_is_current(&self) -> Result<bool, String> {
        let foreground = unsafe { GetForegroundWindow() };
        Ok(foreground.0 as usize == self.foreground
            && focused_window(foreground)?.0 as usize == self.focus)
    }

    fn keys_pressed(&self) -> bool {
        [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN, VK_C]
            .iter()
            .any(|key| unsafe { GetAsyncKeyState(key.0 as i32) < 0 })
    }

    fn backup_and_clear(&self) -> Result<(Self::Snapshot, u32), String> {
        windows_clipboard::backup_and_clear()
    }

    fn send_copy(&self) -> Result<(), String> {
        fn key(key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            }
        }
        let events = [
            key(VK_CONTROL, Default::default()),
            key(VK_C, Default::default()),
            key(VK_C, KEYEVENTF_KEYUP),
            key(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        unsafe {
            SetLastError(WIN32_ERROR(0));
            let sent = SendInput(&events, size_of::<INPUT>() as i32);
            if sent == events.len() as u32 {
                return Ok(());
            }
            let error = GetLastError().0;
            // Undo only the key-down events that were actually inserted.
            let releases: Vec<_> = match sent {
                1 | 3 => vec![key(VK_CONTROL, KEYEVENTF_KEYUP)],
                2 => vec![key(VK_C, KEYEVENTF_KEYUP), key(VK_CONTROL, KEYEVENTF_KEYUP)],
                _ => Vec::new(),
            };
            if !releases.is_empty() {
                let _ = SendInput(&releases, size_of::<INPUT>() as i32);
            }
            Err(format!(
                "Cannot send Ctrl+C: inserted {sent}/4 keyboard events (system error {error}); check the target application's permissions"
            ))
        }
    }

    fn read_changed(&self, sequence: u32) -> Result<Option<String>, String> {
        windows_clipboard::read_changed(sequence)
    }

    fn restore(&self, snapshot: &Self::Snapshot) -> Result<(), String> {
        snapshot.restore()
    }
}
