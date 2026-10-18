use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{
    GetLastError, HINSTANCE, LPARAM, LRESULT, SetLastError, WIN32_ERROR, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    SendInput, UnregisterHotKey, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

use super::hook_state::{HookKeys, KeyDown, matching_action};
use super::{HotkeyError, HotkeyRegistration, ParsedHotkey, binding_definitions};

pub fn register(
    config: &crate::Config,
    menu_available: bool,
    handler: impl Fn(i32) + Send + Sync + 'static,
) -> Result<HotkeyRegistration, HotkeyError> {
    let definitions = binding_definitions(config, menu_available)?;
    if config.hotkey_hook {
        start_low_level_hook(definitions, Arc::new(handler), config.hotkey_debug)
    } else {
        start_registered_hotkeys(definitions, Arc::new(handler), config.hotkey_debug)
    }
}

fn start_registered_hotkeys(
    definitions: Vec<(i32, String, ParsedHotkey)>,
    handler: Arc<dyn Fn(i32) + Send + Sync>,
    debug: bool,
) -> Result<HotkeyRegistration, HotkeyError> {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("dictate-register-hotkey".into())
        .spawn(move || unsafe {
            let thread_id = GetCurrentThreadId();
            let mut registered = Vec::new();
            for (id, spec, parsed) in &definitions {
                let modifiers = parsed.modifiers | MOD_NOREPEAT.0;
                if RegisterHotKey(None, *id, HOT_KEY_MODIFIERS(modifiers), parsed.virtual_key)
                    .is_err()
                {
                    for registered_id in registered {
                        let _ = UnregisterHotKey(None, registered_id);
                    }
                    let _ = result_tx.send(Err(HotkeyError::Registration(format!(
                        "RegisterHotKey failed for '{spec}' (id={id})"
                    ))));
                    return;
                }
                registered.push(*id);
            }
            let mut message = MSG::default();
            // Publish readiness only after the thread can receive WM_QUIT.
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            if result_tx.send(Ok(thread_id)).is_err() {
                for id in registered {
                    let _ = UnregisterHotKey(None, id);
                }
                return;
            }
            loop {
                let result = GetMessageW(&mut message, None, 0, 0);
                if result.0 <= 0 {
                    break;
                }
                if message.message == WM_HOTKEY && !super::capture_active() {
                    if debug {
                        crate::debug_log::write(
                            crate::debug_log::Category::Hotkey,
                            format_args!(
                                "[hotkey-debug] WM_HOTKEY received id={}",
                                message.wParam.0
                            ),
                        );
                    }
                    handler(message.wParam.0 as i32);
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            for id in registered {
                let _ = UnregisterHotKey(None, id);
            }
        })
        .map_err(|error| HotkeyError::Registration(error.to_string()))?;
    let thread_id = result_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| HotkeyError::Registration("timeout registering hotkeys".into()))??;
    let worker_thread = worker.thread().id();
    Ok(HotkeyRegistration::new(move |wait| {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if wait && thread::current().id() != worker_thread {
            let _ = worker.join();
        }
    }))
}

struct HookState {
    lookup: HashMap<u32, Vec<(i32, u32)>>,
    keys: HookKeys,
    handler: Arc<dyn Fn(i32) + Send + Sync>,
    debug: bool,
}

static HOOK_STATE: AtomicPtr<HookState> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let keyboard = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if keyboard.flags.contains(LLKHF_INJECTED) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let message = wparam.0 as u32;
    let virtual_key = keyboard.vkCode;
    let state = HOOK_STATE.load(Ordering::Acquire);
    if state.is_null() {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let state = unsafe { &mut *state };
    if super::capture_active() {
        state.keys.clear();
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
        let pressed_modifiers = current_modifiers();
        let matched_action = state
            .lookup
            .get(&virtual_key)
            .and_then(|candidates| matching_action(candidates, pressed_modifiers));
        match state
            .keys
            .key_down(virtual_key, matched_action, pressed_modifiers)
        {
            KeyDown::Suppress => return LRESULT(1),
            KeyDown::Trigger { id, mask_alt } => {
                // This must precede the action handler and the physical Alt
                // release. The injected key is ignored by this hook above.
                if mask_alt {
                    mask_alt_menu(state.debug);
                }
                if state.debug {
                    crate::debug_log::write(
                        crate::debug_log::Category::Hotkey,
                        format_args!(
                            "[hotkey-debug] swallowed keydown vk=0x{virtual_key:X} id={id}"
                        ),
                    );
                }
                (state.handler)(id);
                return LRESULT(1);
            }
            KeyDown::Pass => {}
        }
    }
    if (message == WM_KEYUP || message == WM_SYSKEYUP) && state.keys.key_up(virtual_key) {
        return LRESULT(1);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn current_modifiers() -> u32 {
    let mut modifiers = 0;
    unsafe {
        if GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 {
            modifiers |= MOD_CONTROL.0;
        }
        if GetAsyncKeyState(VK_MENU.0 as i32) < 0 {
            modifiers |= MOD_ALT.0;
        }
        if GetAsyncKeyState(VK_SHIFT.0 as i32) < 0 {
            modifiers |= MOD_SHIFT.0;
        }
        if GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0 {
            modifiers |= MOD_WIN.0;
        }
    }
    modifiers
}

fn mask_alt_menu(debug: bool) {
    // VK_E8 is unassigned and has no text/modifier side effects. A consumed
    // Alt chord otherwise looks like bare Alt to the foreground app. Do not
    // synthesize Ctrl or swallow Alt-up: either can change held key state.
    // https://www.autohotkey.com/docs/v2/lib/A_MenuMaskKey.htm
    const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: MASK_KEY,
                ..Default::default()
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: MASK_KEY,
                dwFlags: KEYEVENTF_KEYUP,
                ..Default::default()
            },
        },
    };
    unsafe {
        SetLastError(WIN32_ERROR(0));
        let sent = SendInput(&[down, up], size_of::<INPUT>() as i32);
        let error = GetLastError().0;
        // A partial send must release only our mask key, never a physical
        // modifier or the consumed main key. No injected key is retried on 0.
        let released = if sent == 1 {
            SendInput(&[up], size_of::<INPUT>() as i32) == 1
        } else {
            sent == 2
        };
        if debug && sent != 2 {
            crate::debug_log::write(
                crate::debug_log::Category::Hotkey,
                format_args!(
                    "[hotkey-debug] Alt menu mask inserted {sent}/2 events; system error={error}; mask key released={released}"
                ),
            );
        }
    }
}

fn start_low_level_hook(
    definitions: Vec<(i32, String, ParsedHotkey)>,
    handler: Arc<dyn Fn(i32) + Send + Sync>,
    debug: bool,
) -> Result<HotkeyRegistration, HotkeyError> {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("dictate-low-level-hook".into())
        .spawn(move || unsafe {
            let mut lookup: HashMap<u32, Vec<(i32, u32)>> = HashMap::new();
            for (id, _, parsed) in &definitions {
                lookup
                    .entry(parsed.virtual_key)
                    .or_default()
                    .push((*id, parsed.modifiers));
            }
            let state = Box::new(HookState {
                lookup,
                keys: HookKeys::default(),
                handler,
                debug,
            });
            HOOK_STATE.store(Box::into_raw(state), Ordering::Release);
            let instance = GetModuleHandleW(None)
                .map(HINSTANCE::from)
                .unwrap_or_default();
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance), 0);
            let hook = match hook {
                Ok(hook) => hook,
                Err(error) => {
                    let state = HOOK_STATE.swap(std::ptr::null_mut(), Ordering::AcqRel);
                    if !state.is_null() {
                        drop(Box::from_raw(state));
                    }
                    let _ = result_tx.send(Err(HotkeyError::Registration(format!(
                        "SetWindowsHookExW failed: {error}"
                    ))));
                    return;
                }
            };
            let thread_id = GetCurrentThreadId();
            let mut message = MSG::default();
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            let ready = result_tx.send(Ok(thread_id)).is_ok();
            while ready && GetMessageW(&mut message, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = UnhookWindowsHookEx(hook);
            let state = HOOK_STATE.swap(std::ptr::null_mut(), Ordering::AcqRel);
            if !state.is_null() {
                drop(Box::from_raw(state));
            }
        })
        .map_err(|error| HotkeyError::Registration(error.to_string()))?;
    let thread_id = result_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| HotkeyError::Registration("timeout installing low-level hook".into()))??;
    let worker_thread = worker.thread().id();
    Ok(HotkeyRegistration::new(move |wait| {
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if wait && thread::current().id() != worker_thread {
            let _ = worker.join();
        }
    }))
}
