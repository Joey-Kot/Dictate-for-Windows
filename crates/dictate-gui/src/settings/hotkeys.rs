//! A foreground-only recorder hook precedes the application's global hotkeys.
//! Intercepted keys never reach RegisterHotKey, the runtime hook, or Windows shortcuts.
use super::*;
use dictate_core::hotkey::{
    capture::{self, Recorder, Update},
    parse_hotkey,
};
use std::cell::RefCell;
use windows::Win32::UI::Controls::EM_SETREADONLY;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, SetFocus,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub use crate::hotkey_validation::{KEYS, validate_draft};
const SUBCLASS: usize = 0x484b;
const INPUT: u32 = WM_APP + 80;
const NAVIGATE: u32 = WM_APP + 81;
const TRACE: u32 = WM_APP + 82;

struct Field {
    value: String,
    generation: usize,
    language: Language,
    status: HWND,
    context: Vec<(String, String)>,
    optional: bool,
    debug: bool,
    log_remaining: u8,
}

struct Hook {
    handle: HHOOK,
    target: HWND,
    generation: usize,
    swallowed: [bool; 256],
    recorder: Recorder,
    debug: bool,
    traces_remaining: u8,
}

impl Default for Hook {
    fn default() -> Self {
        Self {
            handle: HHOOK::default(),
            target: HWND::default(),
            generation: 0,
            swallowed: [false; 256],
            recorder: Recorder::default(),
            debug: false,
            traces_remaining: 0,
        }
    }
}

thread_local! { static HOOK: RefCell<Hook> = RefCell::new(Hook::default()); }

pub fn attach(
    hwnd: HWND,
    value: String,
    language: Language,
    status: HWND,
    debug: bool,
) -> Result<(), String> {
    attach_field(hwnd, value, language, status, false, debug)
}

pub fn attach_optional(
    hwnd: HWND,
    value: String,
    language: Language,
    status: HWND,
    debug: bool,
) -> Result<(), String> {
    attach_field(hwnd, value, language, status, true, debug)
}

fn attach_field(
    hwnd: HWND,
    value: String,
    language: Language,
    status: HWND,
    optional: bool,
    debug: bool,
) -> Result<(), String> {
    let field = Box::new(Field {
        value,
        generation: 0,
        language,
        status,
        context: Vec::new(),
        optional,
        debug,
        log_remaining: 0,
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

/// Clear only optional fields; the separate button keeps Delete available as a shortcut.
pub fn clear(hwnd: HWND) {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        let field = unsafe { &mut *(data as *mut Field) };
        if field.optional {
            deactivate_target(hwnd);
            field.value.clear();
            restore(hwnd, field);
            check_duplicates(hwnd, field);
        }
    }
}

pub fn value(hwnd: HWND) -> Option<String> {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        Some(unsafe { &*(data as *const Field) }.value.clone())
    } else {
        None
    }
}

pub fn set_context(hwnd: HWND, context: Vec<(String, String)>) {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        let field = unsafe { &mut *(data as *mut Field) };
        field.context = context;
        check_duplicates(hwnd, field);
    }
}

pub fn activate(hwnd: HWND) {
    let mut data = 0;
    if unsafe { GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        // WM_ACTIVATE may follow WM_SETFOCUS for the same field. Rearming here
        // would seed an already pressed modifier and discard the new chord.
        if HOOK.with_borrow(|hook| {
            hook.target == hwnd
                && !hook.handle.0.is_null()
                && hook.generation == unsafe { (*(data as *const Field)).generation }
        }) {
            return;
        }
        begin(hwnd, unsafe { &mut *(data as *mut Field) });
    }
}

/// DefWindowProc(WM_ACTIVATE) focuses the top-level window. Once a shortcut
/// field already has focus, consume activation here so its unfinished chord
/// does not receive WM_KILLFOCUS and get discarded.
pub fn activate_window(hwnd: HWND) -> bool {
    let focus = unsafe { GetFocus() };
    if focus.0.is_null() || unsafe { GetAncestor(focus, GA_ROOT) } != hwnd {
        return false;
    }
    let mut data = 0;
    if !unsafe { GetWindowSubclass(focus, Some(field_proc), SUBCLASS, Some(&mut data)) }.as_bool() {
        return false;
    }
    activate(focus);
    capture_log(
        unsafe { &mut *(data as *mut Field) },
        format_args!("window activated; retain shortcut field focus"),
    );
    true
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
        hook.recorder = Recorder::default();
        target
    });
    if !target.0.is_null() {
        let mut data = 0;
        if unsafe { GetWindowSubclass(target, Some(field_proc), SUBCLASS, Some(&mut data)) }
            .as_bool()
        {
            let field = unsafe { &mut *(data as *mut Field) };
            let generation = field.generation;
            capture_log(
                field,
                format_args!("stop generation={generation}; restore committed value"),
            );
            field.generation = 0;
            field.log_remaining = 0;
            restore(target, field);
        }
    }
    cleanup();
}

fn deactivate_target(hwnd: HWND) {
    if HOOK.with_borrow(|hook| hook.target == hwnd) {
        deactivate();
    }
}

/// An activation/destroy message from an owner or a previous dialog must not
/// cancel a recorder that now belongs to another settings window.
pub fn deactivate_window(hwnd: HWND) {
    let owns_capture = HOOK.with_borrow(|hook| {
        !hook.target.0.is_null() && unsafe { GetAncestor(hook.target, GA_ROOT) == hwnd }
    });
    if owns_capture {
        let target = HOOK.with_borrow(|hook| hook.target);
        let mut data = 0;
        if unsafe { GetWindowSubclass(target, Some(field_proc), SUBCLASS, Some(&mut data)) }
            .as_bool()
        {
            capture_log(
                unsafe { &mut *(data as *mut Field) },
                format_args!("owning window deactivated/destroyed"),
            );
        }
        deactivate();
    }
}

fn foreground_focus() -> HWND {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(0, &mut info) }.is_ok() {
        info.hwndFocus
    } else {
        HWND::default()
    }
}

fn capture_log(field: &mut Field, message: std::fmt::Arguments<'_>) {
    if field.debug && field.log_remaining > 0 {
        field.log_remaining -= 1;
        crate::debug_log::hotkey_capture(message);
    }
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
    field.log_remaining = 64;
    let swallowed = HOOK.with_borrow(|hook| hook.swallowed);
    let held: Vec<_> = (8..256)
        .filter(|vk| {
            // Aggregate modifier aliases and unmapped virtual states need not
            // have matching physical key-up events. Do not wait for them.
            !matches!(*vk, 0x10..=0x12)
                && (swallowed[*vk as usize]
                    || unsafe {
                        MapVirtualKeyW(*vk, MAPVK_VK_TO_VSC_EX) != 0
                            && GetAsyncKeyState(*vk as i32) < 0
                    })
        })
        .collect();
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
        hook.recorder = Recorder::default();
        hook.recorder.seed(held.iter().copied());
        hook.debug = field.debug;
        hook.traces_remaining = 32;
        hook.target = hwnd;
        dictate_core::hotkey::set_capture_active(true);
        Ok::<_, String>(hook.generation)
    });
    match result {
        Ok(generation) => {
            field.generation = generation;
            capture_log(
                field,
                format_args!(
                    "start control={:#x} generation={generation} held={held:?}",
                    unsafe { GetDlgCtrlID(hwnd) }
                ),
            );
            set_text(hwnd, field.language.text("hotkey_press"));
            check_duplicates(hwnd, field);
        }
        Err(error) => {
            field.generation = 0;
            capture_log(field, format_args!("hook installation failed: {error}"));
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
        let focused = !hook.target.0.is_null() && unsafe { GetFocus() } == hook.target;
        let foreground = !hook.target.0.is_null() && foreground_focus() == hook.target;
        let active = !hook.target.0.is_null() && foreground;
        if !hook.target.0.is_null() && hook.debug && hook.traces_remaining > 0 {
            hook.traces_remaining -= 1;
            // Do not log keys delivered to another foreground application.
            let key_info = if active {
                key.vkCode | (key.flags.0 << 8)
            } else {
                0
            };
            let trace = key_info
                | ((pressed as u32) << 16)
                | ((focused as u32) << 17)
                | ((foreground as u32) << 18);
            unsafe {
                let _ = PostMessageW(
                    Some(hook.target),
                    TRACE,
                    WPARAM(hook.generation),
                    LPARAM(trace as isize),
                );
            }
        }
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
                    let update = if navigation {
                        LPARAM((backwards as isize) << 9)
                    } else {
                        // Resolve entry state and chord edges in hook order.
                        // Posted UI messages may run after several more edges;
                        // polling Windows there would observe a later state.
                        let swallowed = hook.swallowed;
                        hook.recorder.reconcile_entry_keys(|vk| {
                            swallowed[vk as usize] || GetAsyncKeyState(vk as i32) < 0
                        });
                        encode_update(hook.recorder.event(vk as u32, pressed))
                    };
                    let _ = PostMessageW(
                        Some(hook.target),
                        if navigation { NAVIGATE } else { INPUT },
                        WPARAM(hook.generation),
                        update,
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

// Keep hook callbacks free of UI work and heap-owned message payloads. Encode
// the completed state update instead of queueing raw events to poll later.
fn encode_update(update: Update) -> LPARAM {
    let (kind, key) = match update {
        Update::Preview(key) => (0, Some(key)),
        Update::Complete(key) => (1, Some(key)),
        Update::Invalid => (2, None),
        Update::Waiting => (3, None),
    };
    let key = key
        .map(|key| key.virtual_key | (key.modifiers << 8))
        .unwrap_or_default();
    LPARAM(((kind << 12) | key) as isize)
}

fn decode_update(value: LPARAM) -> Update {
    let key = dictate_core::hotkey::ParsedHotkey {
        modifiers: (value.0 as u32 >> 8) & 15,
        virtual_key: value.0 as u32 & 255,
    };
    match value.0 >> 12 {
        0 => Update::Preview(key),
        1 => Update::Complete(key),
        2 => Update::Invalid,
        _ => Update::Waiting,
    }
}

fn check_duplicates(hwnd: HWND, field: &Field) {
    if let Ok(current) = parse_hotkey(&field.value) {
        for (name, spec) in &field.context {
            if let Ok(other) = parse_hotkey(spec)
                && current == other
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
                // This loop only compares the three audio fields. Rewrite
                // conflicts were already checked against field.context above.
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
    set_text(
        field.status,
        field.language.text(if field.optional {
            "hotkey_optional_help"
        } else {
            "hotkey_help"
        }),
    );
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
            let field = unsafe { &mut *(data as *mut Field) };
            capture_log(field, format_args!("focus lost; next={:#x}", wparam.0));
            deactivate_target(hwnd);
        }
        WM_LBUTTONDOWN if unsafe { GetFocus() } == hwnd => {
            // A deliberate click starts a fresh attempt, also recovering a
            // recorder hook silently removed by Windows after a timeout.
            begin(hwnd, unsafe { &mut *(data as *mut Field) });
        }
        TRACE if wparam.0 != 0 && wparam.0 == unsafe { (*(data as *const Field)).generation } => {
            let field = unsafe { &mut *(data as *mut Field) };
            capture_log(
                field,
                format_args!(
                    "key vk={:#04x} {} flags={:#04x} focus={} foreground={}",
                    lparam.0 & 255,
                    if lparam.0 & (1 << 16) != 0 {
                        "down"
                    } else {
                        "up"
                    },
                    (lparam.0 >> 8) & 255,
                    lparam.0 & (1 << 17) != 0,
                    lparam.0 & (1 << 18) != 0,
                ),
            );
            return LRESULT(0);
        }
        INPUT if wparam.0 == unsafe { (*(data as *const Field)).generation } && wparam.0 != 0 => {
            let field = unsafe { &mut *(data as *mut Field) };
            let update = decode_update(lparam);
            capture_log(field, format_args!("result {update:?}"));
            match update {
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
                    set_text(hwnd, field.language.text("hotkey_press"));
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
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            let field = unsafe { &mut *(data as *mut Field) };
            if field.generation != 0 {
                capture_log(
                    field,
                    format_args!(
                        "native key message={message:#x} vk={:#04x} reached field; suppressed",
                        wparam.0
                    ),
                );
            }
            return LRESULT(0);
        }
        WM_CHAR | WM_SYSCHAR | WM_PASTE | WM_CUT | WM_CLEAR | WM_UNDO => return LRESULT(0),
        WM_NCDESTROY => {
            deactivate_target(hwnd);
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRoot(HWND);

    impl Drop for TestRoot {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    /// Drives the real focus/hook/message path. Supply physical keyboard events
    /// from an interactive desktop; SendInput is deliberately ignored by capture.
    #[test]
    #[ignore = "requires an interactive desktop and external keyboard events"]
    fn foreground_capture_with_runtime_hotkeys() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let hook_backend = std::env::var("DICTATE_CAPTURE_BACKEND").as_deref() != Ok("register");
        let full_settings = std::env::var_os("DICTATE_CAPTURE_FULL_SETTINGS").is_some();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _executor_guard = executor.enter();
        let scratch = tempfile::tempdir().unwrap();
        let config = Config {
            start_key: "ctrl+alt+k".into(),
            hotkey_hook: hook_backend,
            ..Default::default()
        };
        let actions = std::sync::Arc::new(AtomicUsize::new(0));
        let action_count = actions.clone();
        let registration = dictate_core::hotkey::register(&config, false, move |_| {
            action_count.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        unsafe {
            let root = TestRoot(
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!("Dictate Capture Input Test"),
                    WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                    60,
                    60,
                    540,
                    220,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let mut field = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                20,
                30,
                460,
                36,
                Some(root.0),
                None,
                None,
                None,
            )
            .unwrap();
            let mut status = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!(""),
                WS_CHILD | WS_VISIBLE,
                20,
                80,
                460,
                70,
                Some(root.0),
                None,
                None,
                None,
            )
            .unwrap();
            attach(field, "ctrl+alt+q".into(), Language::English, status, true).unwrap();
            let settings = if full_settings {
                use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
                CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
                let mut draft = config.clone();
                draft.start_key = "ctrl+alt+q".into();
                draft.cache_dir = scratch.path().to_string_lossy().into_owned();
                let runtime = Runtime::new(draft, Arc::new(platform::GuiLibAvConverter)).unwrap();
                let settings = SettingsWindow::open(root.0, runtime, Language::English).unwrap();
                let pointer =
                    GetWindowLongPtrW(settings.hwnd(), GWLP_USERDATA) as *mut SettingsState;
                (*pointer).active_group = GROUPS.iter().position(|g| g.0 == "Hotkeys").unwrap();
                update_page_visibility(&*pointer);
                field = (&(*pointer).controls)["START_KEY"];
                let mut data = 0;
                assert!(
                    GetWindowSubclass(field, Some(field_proc), SUBCLASS, Some(&mut data)).as_bool()
                );
                status = (*(data as *const Field)).status;
                SetWindowTextW(settings.hwnd(), w!("Dictate Capture Input Test Full")).unwrap();
                Some(TestRoot(settings.hwnd()))
            } else {
                None
            };
            let focus_root = settings.as_ref().map_or(root.0, |s| s.0);
            let _ = ShowWindow(focus_root, SW_SHOW);
            let _ = SetForegroundWindow(focus_root);
            SetFocus(Some(field)).unwrap();
            let mut root_rect = RECT::default();
            let mut field_rect = RECT::default();
            GetWindowRect(focus_root, &mut root_rect).unwrap();
            GetWindowRect(field, &mut field_rect).unwrap();
            eprintln!(
                "CAPTURE_READY backend={hook_backend} target={field:?} text={:?} CLICK_X={} CLICK_Y={}",
                read_text(field),
                (field_rect.left + field_rect.right) / 2 - root_rect.left,
                (field_rect.top + field_rect.bottom) / 2 - root_rect.top,
            );
            let started = Instant::now();
            let mut activation_checked = false;
            while started.elapsed() < Duration::from_secs(12)
                && value(field).as_deref() != Some("ctrl+alt+k")
            {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                    if full_settings && !activation_checked && read_text(field) == "Ctrl" {
                        activation_checked = true;
                        SendMessageW(focus_root, WM_ACTIVATE, Some(WPARAM(1)), None);
                        assert_eq!(
                            GetFocus(),
                            field,
                            "WM_ACTIVATE moved focus away; display={:?}",
                            read_text(field)
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            if std::env::var_os("DICTATE_CAPTURE_TRACE").is_some()
                && let Some(log) = crate::debug_log::snapshot(0)
            {
                eprintln!("{}", log.text());
            }
            assert_eq!(
                value(field).as_deref(),
                Some("ctrl+alt+k"),
                "display={:?}; status={:?}; actions={}",
                read_text(field),
                read_text(status),
                actions.load(Ordering::SeqCst),
            );
            assert_eq!(actions.load(Ordering::SeqCst), 0);
            deactivate();
        }
        registration.stop_and_wait();
    }

    #[test]
    fn hook_updates_preserve_preview_commit_and_waiting_states() {
        for vk in 0..256 {
            for modifiers in 0..16 {
                let key = dictate_core::hotkey::ParsedHotkey {
                    modifiers,
                    virtual_key: vk,
                };
                assert_eq!(
                    decode_update(encode_update(Update::Preview(key))),
                    Update::Preview(key)
                );
                assert_eq!(
                    decode_update(encode_update(Update::Complete(key))),
                    Update::Complete(key)
                );
            }
        }
        assert_eq!(
            decode_update(encode_update(Update::Invalid)),
            Update::Invalid
        );
        assert_eq!(
            decode_update(encode_update(Update::Waiting)),
            Update::Waiting
        );
    }

    #[test]
    fn required_and_optional_fields_display_and_commit_queued_hook_updates() {
        unsafe {
            let root = TestRoot(
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    320,
                    100,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            for optional in [false, true] {
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("EDIT"),
                    w!(""),
                    WS_CHILD,
                    0,
                    0,
                    300,
                    32,
                    Some(root.0),
                    None,
                    None,
                    None,
                )
                .unwrap();
                attach_field(
                    hwnd,
                    "ctrl+alt+q".into(),
                    Language::English,
                    root.0,
                    optional,
                    false,
                )
                .unwrap();
                let mut data = 0;
                assert!(
                    GetWindowSubclass(hwnd, Some(field_proc), SUBCLASS, Some(&mut data)).as_bool()
                );
                let generation = 7;
                (*(data as *mut Field)).generation = generation;
                let dispatch = |generation, update| {
                    PostMessageW(Some(hwnd), INPUT, WPARAM(generation), encode_update(update))
                        .unwrap();
                    let mut message = MSG::default();
                    assert!(
                        PeekMessageW(&mut message, Some(hwnd), INPUT, INPUT, PM_REMOVE).as_bool()
                    );
                    DispatchMessageW(&message);
                };
                dispatch(generation, Update::Waiting);
                assert_eq!(read_text(hwnd), Language::English.text("hotkey_press"));
                assert_eq!(value(hwnd).as_deref(), Some("ctrl+alt+q"));

                let mut recorder = Recorder::default();
                for (vk, pressed, display) in [
                    (0xa2, true, "Ctrl"),
                    (0xa0, true, "Ctrl + Shift"),
                    (0xa4, true, "Ctrl + Shift + Alt"),
                    (0x41, true, "Ctrl + Shift + Alt + A"),
                    (0x41, false, "Ctrl + Shift + Alt + A"),
                    (0xa4, false, "Ctrl + Shift + Alt + A"),
                    (0xa0, false, "Ctrl + Shift + Alt + A"),
                    (0xa2, false, "Ctrl + Shift + Alt + A"),
                ] {
                    dispatch(generation, recorder.event(vk, pressed));
                    assert_eq!(read_text(hwnd), display);
                }
                assert_eq!(value(hwnd).as_deref(), Some("ctrl+shift+alt+a"));

                let old = parse_hotkey("f1").unwrap();
                dispatch(generation - 1, Update::Complete(old));
                assert_eq!(value(hwnd).as_deref(), Some("ctrl+shift+alt+a"));
                dispatch(generation, Update::Invalid);
                assert_eq!(read_text(hwnd), "Ctrl + Shift + Alt + A");
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    #[test]
    fn duplicate_activation_and_previous_fields_cannot_cancel_current_chord() {
        unsafe {
            let root = TestRoot(
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    320,
                    100,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let previous_window = TestRoot(
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    320,
                    100,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let create_field = || {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("EDIT"),
                    w!(""),
                    WS_CHILD,
                    0,
                    0,
                    300,
                    32,
                    Some(root.0),
                    None,
                    None,
                    None,
                )
                .unwrap()
            };
            let previous = create_field();
            let current = create_field();
            attach(
                previous,
                "ctrl+alt+q".into(),
                Language::English,
                root.0,
                false,
            )
            .unwrap();
            attach(
                current,
                "ctrl+alt+s".into(),
                Language::English,
                root.0,
                false,
            )
            .unwrap();
            activate(previous);
            deactivate_target(previous);
            activate(current);
            let generation = HOOK.with_borrow(|hook| hook.generation);
            HOOK.with_borrow_mut(|hook| {
                hook.recorder.event(0xa2, true);
            });

            // WM_ACTIVATE can arrive after the field's WM_SETFOCUS and the
            // first modifier edge. It must retain that recording session.
            activate(current);
            SendMessageW(
                previous,
                WM_KILLFOCUS,
                Some(WPARAM(current.0 as usize)),
                None,
            );
            deactivate_window(previous_window.0);
            HOOK.with_borrow_mut(|hook| {
                assert_eq!(hook.target, current);
                assert_eq!(hook.generation, generation);
                assert_eq!(
                    hook.recorder.event(0x4b, true),
                    Update::Preview(parse_hotkey("ctrl+k").unwrap()),
                );
                hook.recorder.event(0x4b, false);
                assert_eq!(
                    hook.recorder.event(0xa2, false),
                    Update::Complete(parse_hotkey("ctrl+k").unwrap())
                );
            });
            deactivate_window(root.0);
            assert!(HOOK.with_borrow(|hook| hook.target.0.is_null()));
        }
    }
}
