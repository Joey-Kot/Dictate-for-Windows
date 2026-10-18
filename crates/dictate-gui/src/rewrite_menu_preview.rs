//! Native material/input probe. It deliberately needs no API or saved configuration.

pub fn item_count(args: impl IntoIterator<Item = String>) -> Result<Option<usize>, String> {
    let mut result = None;
    for argument in args {
        if argument == "--rewrite-menu-preview" {
            result = Some(6);
        } else if let Some(value) = argument.strip_prefix("--rewrite-menu-preview=") {
            let count = value
                .parse::<usize>()
                .ok()
                .filter(|count| *count <= 1_000)
                .ok_or("Rewrite menu preview count must be between 0 and 1000")?;
            result = Some(count);
        }
    }
    Ok(result)
}

#[cfg(windows)]
pub use native::run;

#[cfg(windows)]
mod native {
    use std::io::Write;
    use std::mem::size_of;
    use std::time::{SystemTime, UNIX_EPOCH};

    use dictate_core::rewrite::{BuiltinPromptIcon, PromptIcon, RewritePrompt};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_F8, VK_F9,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    use crate::platform;
    use crate::rewrite_menu::{RewriteMenu, WM_REWRITE_MENU_PICK};

    pub fn run(count: usize) -> Result<(), String> {
        platform::enable_high_dpi();
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|error| error.to_string())?;
        let result = run_inner(count);
        unsafe { CoUninitialize() };
        result
    }

    fn run_inner(count: usize) -> Result<(), String> {
        let instance = unsafe { GetModuleHandleW(None) }.map_err(|error| error.to_string())?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(owner_proc),
            hInstance: instance.into(),
            lpszClassName: w!("DictateRewriteMenuPreview"),
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(windows::core::Error::from_win32().to_string());
        }
        let owner = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.lpszClassName,
                w!("Dictate menu preview"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .map_err(|error| error.to_string())?;
        let result = preview_loop(owner, count);
        unsafe {
            let _ = UnregisterHotKey(Some(owner), 1);
            let _ = UnregisterHotKey(Some(owner), 2);
            let _ = DestroyWindow(owner);
        }
        result
    }

    fn preview_loop(owner: HWND, count: usize) -> Result<(), String> {
        unsafe {
            RegisterHotKey(Some(owner), 1, MOD_NOREPEAT, u32::from(VK_F8.0))
                .map_err(|error| format!("Preview F8 registration: {error}"))?;
            RegisterHotKey(Some(owner), 2, MOD_NOREPEAT, u32::from(VK_F9.0))
                .map_err(|error| format!("Preview F9 registration: {error}"))?;
        }
        let mut menu = RewriteMenu::new(owner)?;
        let prompts = samples(count);
        let config_path = platform::config_path()?;
        let log_path = std::env::temp_dir().join("dictate-menu-preview.log");
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|error| error.to_string())?;
        let _ = writeln!(
            log,
            "preview-start items={count}; F8=toggle F9=quit; no Rewrite requests"
        );
        let mut original = focus_snapshot();
        let mut observed = original;
        unsafe { SetTimer(Some(owner), 1, 100, None) };
        let mut message = MSG::default();
        loop {
            let status = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
            if status <= 0 {
                menu.close();
                unsafe {
                    let _ = KillTimer(Some(owner), 1);
                }
                return if status < 0 {
                    Err(windows::core::Error::from_win32().to_string())
                } else {
                    Ok(())
                };
            }
            if message.hwnd == owner {
                match message.message {
                    WM_HOTKEY if message.wParam.0 == 1 => {
                        if !menu.is_open() {
                            original = focus_snapshot();
                            observed = original;
                        }
                        menu.toggle(&prompts, &config_path)?;
                        let _ = writeln!(log, "material={}", menu.material_status());
                        write_focus(
                            &mut log,
                            if menu.is_open() { "opened" } else { "closed" },
                            original,
                        );
                        continue;
                    }
                    WM_HOTKEY if message.wParam.0 == 2 => {
                        menu.close();
                        unsafe {
                            let _ = KillTimer(Some(owner), 1);
                        }
                        let _ = writeln!(log, "preview-end");
                        return Ok(());
                    }
                    WM_REWRITE_MENU_PICK => {
                        if let Some(id) = menu.take_selection() {
                            let _ = writeln!(log, "picked={id}");
                            write_focus(&mut log, "picked-after-close", original);
                        }
                        continue;
                    }
                    WM_TIMER => {
                        let current = focus_snapshot();
                        if menu.is_open() && observed != current {
                            observed = current;
                            write_focus(
                                &mut log,
                                "focus-or-menu-mode-changed-while-open",
                                original,
                            );
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct FocusSnapshot {
        foreground: HWND,
        focus: HWND,
        flags: u32,
    }

    fn focus_snapshot() -> FocusSnapshot {
        let foreground = unsafe { GetForegroundWindow() };
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let valid = unsafe { GetGUIThreadInfo(0, &mut info) }.is_ok();
        FocusSnapshot {
            foreground,
            focus: if valid {
                info.hwndFocus
            } else {
                HWND::default()
            },
            flags: if valid {
                info.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0 | GUI_SYSTEMMENUMODE.0)
            } else {
                0
            },
        }
    }

    fn write_focus(log: &mut std::fs::File, event: &str, original: FocusSnapshot) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let current = focus_snapshot();
        let _ = writeln!(
            log,
            "{timestamp} {event} foreground={:p} focus={:p} flags=0x{:X} original={:p} original-focus={:p} original-flags=0x{:X} unchanged={}",
            current.foreground.0,
            current.focus.0,
            current.flags,
            original.foreground.0,
            original.focus.0,
            original.flags,
            current == original
        );
    }

    fn samples(count: usize) -> Vec<RewritePrompt> {
        (0..count)
            .map(|index| {
                let icon = BuiltinPromptIcon::ALL[index % BuiltinPromptIcon::ALL.len()];
                RewritePrompt {
                    id: format!("preview-{index}"),
                    title: icon.label().into(),
                    icon: PromptIcon::Builtin(icon),
                    ..Default::default()
                }
            })
            .collect()
    }

    unsafe extern "system" fn owner_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // No activation, focus restoration, selection access, or runtime side effects.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

#[cfg(test)]
mod tests {
    use super::item_count;

    #[test]
    fn preview_is_opt_in_and_accepts_empty_and_scrolling_cases() {
        assert_eq!(item_count(Vec::<String>::new()).unwrap(), None);
        assert_eq!(
            item_count(["--rewrite-menu-preview".into()]).unwrap(),
            Some(6)
        );
        for count in [0, 1, 6, 7, 8, 1_000] {
            assert_eq!(
                item_count([format!("--rewrite-menu-preview={count}")]).unwrap(),
                Some(count)
            );
        }
        assert!(item_count(["--rewrite-menu-preview=1001".into()]).is_err());
        assert!(item_count(["--rewrite-menu-preview=bad".into()]).is_err());
    }
}
