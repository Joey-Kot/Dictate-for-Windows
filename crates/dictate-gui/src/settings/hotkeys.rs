//! A foreground-only recorder hook precedes the application's global hotkeys.
//! Intercepted keys never reach RegisterHotKey, the runtime hook, or Windows shortcuts.
use super::*;
use dictate_core::hotkey::{
    capture::{self, Recorder, Update},
    parse_hotkey,
};
use std::cell::RefCell;
use windows::Win32::UI::Controls::EM_SETREADONLY;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, SetFocus};
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const KEYS: [&str; 3] = ["START_KEY", "PAUSE_KEY", "CANCEL_OR_RETRY_KEY"];
const SUBCLASS: usize = 0x484b;
const INPUT: u32 = WM_APP + 80;
const NAVIGATE: u32 = WM_APP + 81;

struct Field {
    value: String,
    recorder: Recorder,
    generation: usize,
    language: Language,
    status: HWND,
    context: Vec<(String, String)>,
    hook: bool,
}

struct Hook {
    handle: HHOOK,
    target: HWND,
    generation: usize,
    swallowed: [bool; 256],
}

impl Default for Hook {
    fn default() -> Self {
        Self {
            handle: HHOOK::default(),
            target: HWND::default(),
            generation: 0,
            swallowed: [false; 256],
        }
    }
}

thread_local! { static HOOK: RefCell<Hook> = RefCell::new(Hook::default()); }

pub fn attach(hwnd: HWND, value: String, language: Language, status: HWND) -> Result<(), String> {
    let field = Box::new(Field {
        value,
        recorder: Recorder::default(),
        generation: 0,
        language,
        status,
        context: Vec::new(),
        hook: true,
    });
    let pointer = Box::into_raw(field);
    if !unsafe { SetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, pointer as usize) }.as_bool() {
        unsafe {
            drop(Box::from_raw(pointer));
        }
        return Err("Unable to initialize hotkey recorder".into());
    }
    unsafe {
        SendMessageW(hwnd, EM_SETREADONLY, Some(WPARAM(1)), None);
    }
    restore(hwnd, unsafe { &*pointer });
    Ok(())
}

pub fn value(hwnd: HWND) -> Option<String> {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        Some(unsafe { &*(data as *const Field) }.value.clone())
    } else {
        None
    }
}

pub fn set_context(hwnd: HWND, context: Vec<(String, String)>, hook: bool) {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        let field = unsafe { &mut *(data as *mut Field) };
        field.context = context;
        field.hook = hook;
    }
}

pub fn activate(hwnd: HWND) {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        begin(hwnd, unsafe { &mut *(data as *mut Field) });
    }
}

pub fn set_language(hwnd: HWND, language: Language) {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        unsafe {
            (*(data as *mut Field)).language = language;
        }
    }
}

pub fn deactivate() {
    let target = HOOK.with_borrow_mut(|hook| {
        let target = hook.target;
        hook.target = HWND::default();
        target
    });
    if !target.0.is_null() {
        let mut data = 0;
        if unsafe { GetWindowSubclass(target, Some(field_proc), SUBCLASS, Some(&mut data)) }
            .as_bool()
        {
            let field = unsafe { &mut *(data as *mut Field) };
            field.generation = 0;
            field.recorder = Recorder::default();
            restore(target, field);
        }
    }
    cleanup();
}

fn cleanup() {
    HOOK.with_borrow_mut(|hook| {
        if hook.target.0.is_null()
            && !hook.swallowed.iter().any(|key| *key)
            && !hook.handle.0.is_null()
        {
            unsafe {
                let _ = UnhookWindowsHookEx(hook.handle);
            }
            hook.handle = HHOOK::default();
            dictate_core::hotkey::set_capture_active(false);
        }
    });
}

fn set_text(hwnd: HWND, text: &str) {
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wide(text).as_ptr()));
    }
}

fn restore(hwnd: HWND, field: &Field) {
    let display = parse_hotkey(&field.value)
        .ok()
        .map(|key| capture::format(key, true))
        .unwrap_or_else(|| field.value.clone());
    set_text(hwnd, &display);
}

fn begin(hwnd: HWND, field: &mut Field) {
    // Reinstall on focus entry so this hook precedes any reloaded runtime hook.
    let result = HOOK.with_borrow_mut(|hook| unsafe {
        if !hook.handle.0.is_null() {
            let _ = UnhookWindowsHookEx(hook.handle);
        }
        hook.handle = HHOOK::default();
        hook.target = HWND::default();
        dictate_core::hotkey::set_capture_active(false);
        let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
        hook.handle = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_hook),
            Some(instance.into()),
            0,
        )
        .map_err(|error| error.to_string())?;
        hook.generation = hook.generation.wrapping_add(1).max(1);
        hook.target = hwnd;
        dictate_core::hotkey::set_capture_active(true);
        Ok::<_, String>(hook.generation)
    });
    match result {
        Ok(generation) => {
            field.generation = generation;
            field.recorder = Recorder::default();
            let swallowed = HOOK.with_borrow(|hook| hook.swallowed);
            field.recorder.seed((8..256).filter(|vk| {
                // Only seed physical left/right modifier codes, never their aggregate aliases.
                !matches!(*vk, 0x10..=0x12)
                    && (swallowed[*vk as usize] || unsafe { GetAsyncKeyState(*vk as i32) < 0 })
            }));
            set_text(hwnd, field.language.text("hotkey_press"));
            check_duplicates(hwnd, field);
        }
        Err(error) => {
            field.generation = 0;
            set_text(
                field.status,
                &format!("{}: {error}", field.language.text("hotkey_capture_failed")),
            );
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if key.vkCode >= 256 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let pressed = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let swallowed = HOOK.with_borrow_mut(|hook| {
        let active = !hook.target.0.is_null()
            && unsafe {
                GetFocus() == hook.target
                    && GetAncestor(hook.target, GA_ROOT) == GetForegroundWindow()
            };
        let vk = key.vkCode as usize;
        let previously_swallowed = hook.swallowed[vk];
        if active {
            // Ignore synthetic input while recording, including paste injection.
            if key.flags.contains(LLKHF_INJECTED) {
                return true;
            }
            {
                let navigation =
                    pressed
                        && vk == 9
                        && !hook.swallowed.iter().enumerate().any(|(vk, down)| {
                            *down && vk != 9 && capture::modifier(vk as u32) != 4
                        })
                        && unsafe {
                            GetAsyncKeyState(0x11) >= 0
                                && GetAsyncKeyState(0x12) >= 0
                                && GetAsyncKeyState(0x5b) >= 0
                                && GetAsyncKeyState(0x5c) >= 0
                        };
                unsafe {
                    let backwards =
                        hook.swallowed[0xa0] || hook.swallowed[0xa1] || GetAsyncKeyState(0x10) < 0;
                    let _ = PostMessageW(
                        Some(hook.target),
                        if navigation { NAVIGATE } else { INPUT },
                        WPARAM(hook.generation),
                        LPARAM(
                            (vk | ((pressed as usize) << 8) | ((backwards as usize) << 9)) as isize,
                        ),
                    );
                }
            }
            hook.swallowed[vk] = pressed;
            // Let an already-held modifier's release reach Windows to avoid sticky keys.
            pressed || previously_swallowed
        } else if previously_swallowed {
            hook.swallowed[vk] = pressed;
            true
        } else {
            false
        }
    });
    cleanup();
    if swallowed {
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

fn check_duplicates(hwnd: HWND, field: &Field) {
    if let Ok(current) = parse_hotkey(&field.value) {
        for (name, spec) in &field.context {
            if let Ok(other) = parse_hotkey(spec)
                && (current == other || (field.hook && current.virtual_key == other.virtual_key))
            {
                set_text(
                    field.status,
                    &format!("{}: {name}", field.language.text("hotkey_duplicate")),
                );
                return;
            }
        }
    }
    let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
    let mut bindings: Vec<(&str, dictate_core::hotkey::ParsedHotkey)> = Vec::new();
    for spec in FIELDS.iter().filter(|spec| KEYS.contains(&spec.key)) {
        let index = FIELDS
            .iter()
            .position(|candidate| candidate.key == spec.key)
            .unwrap();
        let other =
            unsafe { GetDlgItem(Some(parent), (ID_FIELD_BASE + index) as i32) }.unwrap_or_default();
        let text = if other == hwnd {
            Some(field.value.clone())
        } else {
            value(other)
        };
        if let Some(parsed) = text.and_then(|text| parse_hotkey(&text).ok()) {
            for (previous, key) in &bindings {
                if *key == parsed {
                    set_text(
                        field.status,
                        &format!(
                            "{}: {} / {}",
                            field.language.text("hotkey_duplicate"),
                            field.language.text(previous),
                            field.language.text(spec.key)
                        ),
                    );
                    return;
                }
            }
            bindings.push((spec.key, parsed));
        }
    }
    set_text(field.status, field.language.text("hotkey_help"));
}

unsafe extern "system" fn field_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_SETFOCUS => {
            begin(hwnd, unsafe { &mut *(data as *mut Field) });
        }
        WM_KILLFOCUS => {
            deactivate();
        }
        INPUT if wparam.0 == unsafe { (*(data as *const Field)).generation } && wparam.0 != 0 => {
            let field = unsafe { &mut *(data as *mut Field) };
            match field
                .recorder
                .event((lparam.0 & 255) as u32, lparam.0 & 256 != 0)
            {
                Update::Preview(key) => {
                    check_duplicates(hwnd, field);
                    let text = capture::format(key, true);
                    set_text(
                        hwnd,
                        if text.is_empty() {
                            field.language.text("hotkey_press")
                        } else {
                            &text
                        },
                    );
                }
                Update::Complete(key) => {
                    field.value = capture::format(key, false);
                    restore(hwnd, field);
                    check_duplicates(hwnd, field);
                }
                Update::Invalid => {
                    restore(hwnd, field);
                    set_text(field.status, field.language.text("hotkey_invalid"));
                }
                Update::Waiting => {
                    restore(hwnd, field);
                }
            }
            return LRESULT(0);
        }
        NAVIGATE
            if wparam.0 == unsafe { (*(data as *const Field)).generation } && wparam.0 != 0 =>
        {
            // Prompt fields can live inside a scrollable child container; Tab
            // must also reach the dialog's fixed footer controls.
            let parent = unsafe { GetAncestor(hwnd, GA_ROOT) };
            let next = unsafe { GetNextDlgTabItem(parent, Some(hwnd), lparam.0 & 512 != 0) };
            if let Ok(next) = next {
                unsafe {
                    let _ = SetFocus(Some(next));
                }
            }
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR | WM_PASTE
        | WM_CUT | WM_CLEAR | WM_UNDO => return LRESULT(0),
        WM_NCDESTROY => {
            if HOOK.with_borrow(|hook| hook.target == hwnd) {
                deactivate();
            }
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(field_proc), SUBCLASS);
                drop(Box::from_raw(data as *mut Field));
            }
            cleanup();
        }
        _ => {}
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
