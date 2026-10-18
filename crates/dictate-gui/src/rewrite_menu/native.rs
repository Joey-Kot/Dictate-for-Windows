use std::cell::Cell;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use dictate_core::rewrite::RewritePrompt;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use super::layout::{Bounds, Layout, SCROLLBAR_INSET, SCROLLBAR_SCALE};
use super::surface::Surface;
use super::theme::Theme;
use crate::prompt_icons::IconRenderer;

/// Selection is available through `take_selection` only after hiding the menu
/// and releasing temporary hooks/capture. The owner can then invoke Rewrite.
pub(crate) const WM_REWRITE_MENU_PICK: u32 = WM_APP + 180;
const DISMISS: u32 = WM_APP + 181;
const POINTER: u32 = WM_APP + 182;
const WHEEL: u32 = WM_APP + 183;
const CLASS: windows::core::PCWSTR = w!("DictateRewriteMenu");

thread_local! {
    static HOOK_WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static ESCAPE_HELD: Cell<bool> = const { Cell::new(false) };
    static INITIAL_ESCAPE: Cell<bool> = const { Cell::new(false) };
    static LEFT_OWNED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct RewriteMenu {
    hwnd: HWND,
    _state: Box<MenuState>,
}

struct MenuState {
    hwnd: HWND,
    owner: HWND,
    open: bool,
    generation: u32,
    prompts: Vec<RewritePrompt>,
    config_path: PathBuf,
    layout: Layout,
    dpi: u32,
    offset: usize,
    wheel: i32,
    hover: Option<usize>,
    pressed: Option<usize>,
    drag: Option<f32>,
    tracking: bool,
    selected: Option<String>,
    theme: Theme,
    acrylic_requested: bool,
    surface: Option<Surface>,
    icons: Option<IconRenderer>,
    keyboard_hook: Option<HHOOK>,
    mouse_hook: Option<HHOOK>,
}

impl RewriteMenu {
    pub(crate) fn new(owner: HWND) -> Result<Self, String> {
        static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
        REGISTERED
            .get_or_init(|| unsafe {
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
                    lpfnWndProc: Some(window_proc),
                    hInstance: GetModuleHandleW(None).map_err(|e| e.to_string())?.into(),
                    hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
                    // No opaque class background: only the composition surface is
                    // painted, so there is no extra rectangular background frame.
                    lpszClassName: CLASS,
                    ..Default::default()
                };
                if RegisterClassExW(&class) == 0 {
                    Err(windows::core::Error::from_win32().to_string())
                } else {
                    Ok(())
                }
            })
            .clone()?;
        let mut state = Box::new(MenuState {
            hwnd: HWND::default(),
            owner,
            open: false,
            generation: 0,
            prompts: Vec::new(),
            config_path: PathBuf::new(),
            layout: Layout::new(0, 1920, 1080, 96),
            dpi: 96,
            offset: 0,
            wheel: 0,
            hover: None,
            pressed: None,
            drag: None,
            tracking: false,
            selected: None,
            theme: Theme::read(),
            acrylic_requested: false,
            surface: None,
            icons: None,
            keyboard_hook: None,
            mouse_hook: None,
        });
        let ptr = (&mut *state as *mut MenuState).cast();
        let created = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                w!("Rewrite"),
                WS_POPUP | WS_THICKFRAME,
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(GetModuleHandleW(None).map_err(|e| e.to_string())?.into()),
                Some(ptr),
            )
        };
        match created {
            Ok(hwnd) => Ok(Self {
                hwnd,
                _state: state,
            }),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        unsafe { self.state().as_ref().is_some_and(|s| s.open) }
    }

    pub(crate) fn toggle(
        &mut self,
        prompts: &[RewritePrompt],
        config_path: &Path,
    ) -> Result<(), String> {
        let mut pointer = POINT::default();
        unsafe { GetCursorPos(&mut pointer) }.map_err(|e| e.to_string())?;
        self.toggle_at(prompts, config_path, pointer)
    }

    pub(crate) fn toggle_at(
        &mut self,
        prompts: &[RewritePrompt],
        config_path: &Path,
        pointer: POINT,
    ) -> Result<(), String> {
        if self.is_open() {
            self.close();
            return Ok(());
        }
        if prompts.is_empty() {
            return Ok(());
        }
        unsafe {
            let ptr = self.state();
            if ptr.is_null() {
                return Err("Rewrite menu window is unavailable".into());
            }
            // Do not recycle the window while input from the previous menu is
            // still draining, or while its stable-ID selection awaits dispatch.
            if ESCAPE_HELD.get()
                || LEFT_OWNED.get()
                || (*ptr).selected.is_some()
                || (*ptr).keyboard_hook.is_some()
                || (*ptr).mouse_hook.is_some()
            {
                return Ok(());
            }
            close(ptr, false);
            (*ptr).generation = (*ptr).generation.wrapping_add(1);
            (*ptr).prompts = prompts.to_vec();
            if (*ptr).icons.is_none() || (*ptr).config_path != config_path {
                (*ptr).icons = Some(IconRenderer::new(config_path));
            }
            (*ptr).config_path = config_path.to_path_buf();
            (*ptr).offset = 0;
            (*ptr).wheel = 0;
            (*ptr).selected = None;
            (*ptr).theme = Theme::read();
            position(ptr, pointer)?;
            let theme = (*ptr).theme;
            (*ptr).acrylic_requested = theme.apply(self.hwnd);
            // Allocate and present before exposing the window. A failed GPU
            // allocation is returned as an error, never an empty black panel.
            paint(ptr)?;
            install_hooks(ptr)?;
            (*ptr).open = true;
            if let Err(error) = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
            ) {
                close(ptr, false);
                return Err(error.to_string());
            }
            Ok(())
        }
    }

    pub(crate) fn close(&mut self) {
        unsafe {
            let ptr = self.state();
            if !ptr.is_null() {
                close(ptr, false);
            }
        }
    }

    pub(crate) fn take_selection(&mut self) -> Option<String> {
        unsafe { self.state().as_mut().and_then(|s| s.selected.take()) }
    }

    pub(crate) fn material_status(&self) -> &'static str {
        unsafe {
            if let Some(state) = self.state().as_ref().filter(|s| s.acrylic_requested) {
                if state.theme.dark {
                    "Compositor Acrylic accent accepted; dark 80% tint opacity, live desktop blur"
                } else {
                    "Compositor Acrylic accent accepted; light 70% tint opacity, live desktop blur"
                }
            } else {
                "Solid theme fallback (Windows 11 rounded Acrylic unavailable, transparency disabled, high contrast, or battery saver)"
            }
        }
    }

    unsafe fn state(&self) -> *mut MenuState {
        unsafe { GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) as *mut MenuState }
    }
}

impl Drop for RewriteMenu {
    fn drop(&mut self) {
        unsafe {
            let ptr = self.state();
            if !ptr.is_null() {
                close(ptr, true);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe fn position(ptr: *mut MenuState, pointer: POINT) -> Result<(), String> {
    unsafe {
        let monitor = MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(windows::core::Error::from_win32().to_string());
        }
        let mut dpi = 96;
        let mut dpi_y = 96;
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
        let work = Bounds {
            left: info.rcWork.left,
            top: info.rcWork.top,
            right: info.rcWork.right,
            bottom: info.rcWork.bottom,
        };
        let layout = Layout::new(
            (*ptr).prompts.len(),
            work.right - work.left,
            work.bottom - work.top,
            dpi,
        );
        let bounds = layout.place((pointer.x, pointer.y), work, dpi);
        (*ptr).layout = layout;
        (*ptr).dpi = dpi;
        (*ptr).offset = (*ptr).offset.min(layout.max_offset());
        (*ptr).surface = None;
        SetWindowPos(
            (*ptr).hwnd,
            Some(HWND_TOPMOST),
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        )
        .map_err(|e| e.to_string())
    }
}

unsafe fn paint(ptr: *mut MenuState) -> Result<(), String> {
    unsafe {
        let state = &mut *ptr;
        if state.surface.is_none() {
            let mut rect = RECT::default();
            GetClientRect(state.hwnd, &mut rect).map_err(|e| e.to_string())?;
            state.surface = Some(
                Surface::new(state.hwnd, rect.right as u32, rect.bottom as u32, state.dpi)
                    .map_err(|e| e.to_string())?,
            );
            if let Some(icons) = state.icons.as_mut() {
                icons.discard_device_resources();
            }
        }
        let result = state.surface.as_mut().unwrap().paint(
            state.layout,
            &state.prompts,
            state.offset,
            state.hover,
            state.pressed,
            state.dpi,
            state.theme,
            state.acrylic_requested,
            state
                .icons
                .as_mut()
                .ok_or("Rewrite menu icon cache is unavailable")?,
        );
        if result.is_err() {
            state.surface = None;
        }
        result.map_err(|e| e.to_string())
    }
}

unsafe fn install_hooks(ptr: *mut MenuState) -> Result<(), String> {
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let keyboard = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(keyboard_hook),
            Some(instance.into()),
            0,
        )
        .map_err(|e| e.to_string())?;
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(instance.into()), 0) {
            Ok(mouse) => {
                (*ptr).keyboard_hook = Some(keyboard);
                (*ptr).mouse_hook = Some(mouse);
                HOOK_WINDOW.set((*ptr).hwnd);
                ESCAPE_HELD.set(false);
                LEFT_OWNED.set(false);
                INITIAL_ESCAPE.set(GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0);
                Ok(())
            }
            Err(error) => {
                let _ = UnhookWindowsHookEx(keyboard);
                Err(error.to_string())
            }
        }
    }
}

unsafe fn close(ptr: *mut MenuState, force: bool) {
    unsafe {
        (*ptr).open = false;
        (*ptr).hover = None;
        (*ptr).pressed = None;
        (*ptr).drag = None;
        (*ptr).tracking = false;
        // Change state before releasing capture, which synchronously sends
        // WM_CAPTURECHANGED. Hooks also stop routing mouse input immediately.
        if GetCapture() == (*ptr).hwnd {
            let _ = ReleaseCapture();
        }
        if force || !LEFT_OWNED.get() {
            if let Some(hook) = (*ptr).mouse_hook.take() {
                let _ = UnhookWindowsHookEx(hook);
            }
            LEFT_OWNED.set(false);
        }
        if force || !ESCAPE_HELD.get() {
            if let Some(hook) = (*ptr).keyboard_hook.take() {
                let _ = UnhookWindowsHookEx(hook);
            }
            ESCAPE_HELD.set(false);
        }
        if (*ptr).mouse_hook.is_none() && (*ptr).keyboard_hook.is_none() {
            HOOK_WINDOW.set(HWND::default());
            INITIAL_ESCAPE.set(false);
        }
        // SW_HIDE is documented to activate another window. In particular,
        // close() also runs before opening an already-hidden popup. Keep both
        // visibility transitions explicitly nonactivating, without restoring
        // any saved foreground window or moving the owner's Z order.
        if IsWindowVisible((*ptr).hwnd).as_bool() {
            let _ = SetWindowPos(
                (*ptr).hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_HIDEWINDOW
                    | SWP_NOACTIVATE
                    | SWP_NOOWNERZORDER
                    | SWP_NOZORDER
                    | SWP_NOMOVE
                    | SWP_NOSIZE,
            );
        }
    }
}

unsafe fn cursor_dip(ptr: *mut MenuState, packed: LPARAM) -> (f32, f32) {
    unsafe {
        let mut point = POINT {
            x: packed.0 as i32,
            y: (packed.0 as u64 >> 32) as i32,
        };
        let _ = ScreenToClient((*ptr).hwnd, &mut point);
        let scale = 96.0 / (*ptr).dpi.max(1) as f32;
        (point.x as f32 * scale, point.y as f32 * scale)
    }
}

fn packed_point(point: POINT) -> LPARAM {
    LPARAM(((point.x as u32 as u64) | ((point.y as u32 as u64) << 32)) as isize)
}

unsafe fn pointer_event(ptr: *mut MenuState, message: u32, packed: LPARAM) {
    unsafe {
        if !(*ptr).open {
            return;
        }
        let (x, y) = cursor_dip(ptr, packed);
        let state = &mut *ptr;
        match message {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                if state.layout.hit_scrollbar(x, y) {
                    let metrics = state.layout.scrollbar(state.offset);
                    let thumb_top = SCROLLBAR_INSET + metrics.top as f32 / SCROLLBAR_SCALE;
                    let thumb_height = metrics.thumb as f32 / SCROLLBAR_SCALE;
                    if y >= thumb_top && y <= thumb_top + thumb_height {
                        state.drag = Some(y - thumb_top);
                    } else {
                        let max = state.layout.max_offset();
                        state.offset = if y < thumb_top {
                            state.offset.saturating_sub(state.layout.visible)
                        } else {
                            (state.offset + state.layout.visible).min(max)
                        };
                        let new_top = SCROLLBAR_INSET
                            + state.layout.scrollbar(state.offset).top as f32 / SCROLLBAR_SCALE;
                        state.drag = Some((y - new_top).clamp(0.0, thumb_height));
                    }
                    state.pressed = None;
                } else {
                    state.pressed = state.layout.hit(x, y, state.offset);
                }
                SetCapture(state.hwnd);
            }
            WM_MOUSEMOVE => {
                if let Some(grab) = state.drag {
                    let metrics = state.layout.scrollbar(state.offset);
                    state.offset = metrics
                        .index_at(((y - SCROLLBAR_INSET - grab) * SCROLLBAR_SCALE).round() as i32)
                        as usize;
                    state.hover = None;
                } else {
                    state.hover = state.layout.hit(x, y, state.offset);
                }
            }
            WM_LBUTTONUP => {
                let selected = if state.drag.is_none() {
                    state
                        .pressed
                        .filter(|index| state.layout.hit(x, y, state.offset) == Some(*index))
                } else {
                    None
                };
                state.drag = None;
                state.pressed = None;
                if let Some(index) = selected {
                    state.selected = state.prompts.get(index).map(|prompt| prompt.id.clone());
                    let owner = state.owner;
                    close(ptr, false);
                    let _ = PostMessageW(Some(owner), WM_REWRITE_MENU_PICK, WPARAM(0), LPARAM(0));
                    return;
                }
                if GetCapture() == (*ptr).hwnd {
                    let _ = ReleaseCapture();
                }
            }
            _ => {}
        }
        let _ = InvalidateRect(Some((*ptr).hwnd), None, false);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = create.lpCreateParams as *mut MenuState;
            (*ptr).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
            return LRESULT(1);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        match message {
            WM_NCCALCSIZE => LRESULT(0),
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                if (*ptr).open && paint(ptr).is_err() {
                    close(ptr, false);
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_SIZE => {
                (*ptr).surface = None;
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if !(*ptr).tracking {
                    let mut tracking = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = TrackMouseEvent(&mut tracking);
                    (*ptr).tracking = true;
                }
                let mut point = POINT::default();
                if GetCursorPos(&mut point).is_ok() {
                    pointer_event(ptr, WM_MOUSEMOVE, packed_point(point));
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                (*ptr).tracking = false;
                (*ptr).hover = None;
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            POINTER => {
                if (wparam.0 >> 32) as u32 == (*ptr).generation {
                    pointer_event(ptr, wparam.0 as u32, lparam);
                }
                LRESULT(0)
            }
            WHEEL => {
                if (*ptr).open && wparam.0 as u32 == (*ptr).generation {
                    (*ptr).wheel += lparam.0 as i32;
                    let steps = (*ptr).wheel / WHEEL_DELTA as i32;
                    (*ptr).wheel %= WHEEL_DELTA as i32;
                    (*ptr).offset = ((*ptr).offset as i64 - steps as i64 * 3)
                        .clamp(0, (*ptr).layout.max_offset() as i64)
                        as usize;
                    (*ptr).pressed = None;
                    (*ptr).hover = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            DISMISS => {
                if wparam.0 as u32 == (*ptr).generation {
                    close(ptr, false);
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                (*ptr).pressed = None;
                (*ptr).drag = None;
                LRESULT(0)
            }
            WM_CANCELMODE | WM_CLOSE => {
                close(ptr, false);
                LRESULT(0)
            }
            WM_SETTINGCHANGE
            | WM_SYSCOLORCHANGE
            | WM_THEMECHANGED
            | WM_DWMCOMPOSITIONCHANGED
            | WM_POWERBROADCAST => {
                (*ptr).theme = Theme::read();
                let theme = (*ptr).theme;
                (*ptr).acrylic_requested = theme.apply(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                if (*ptr).open {
                    let mut point = POINT::default();
                    if GetCursorPos(&mut point).is_err() || position(ptr, point).is_err() {
                        close(ptr, false);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_NCDESTROY => {
                close(ptr, true);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 && (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode == VK_ESCAPE.0 as u32 {
            let hwnd = HOOK_WINDOW.get();
            if !hwnd.is_invalid() {
                let message = wparam.0 as u32;
                if INITIAL_ESCAPE.get() {
                    if message == WM_KEYUP || message == WM_SYSKEYUP {
                        INITIAL_ESCAPE.set(false);
                    }
                    return CallNextHookEx(None, code, wparam, lparam);
                }
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
                if ptr.is_null() || (!(*ptr).open && !ESCAPE_HELD.get()) {
                    return CallNextHookEx(None, code, wparam, lparam);
                }
                if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
                    if !ESCAPE_HELD.replace(true) {
                        let _ = PostMessageW(
                            Some(hwnd),
                            DISMISS,
                            WPARAM((*ptr).generation as usize),
                            LPARAM(0),
                        );
                    }
                    return LRESULT(1);
                }
                if (message == WM_KEYUP || message == WM_SYSKEYUP) && ESCAPE_HELD.get() {
                    // Keep the temporary keyboard hook until release: repeats
                    // and key-up must not cancel the application's selection.
                    ESCAPE_HELD.set(false);
                    let _ = PostMessageW(
                        Some(hwnd),
                        DISMISS,
                        WPARAM((*ptr).generation as usize),
                        LPARAM(0),
                    );
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 {
            let hwnd = HOOK_WINDOW.get();
            if !hwnd.is_invalid() {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut MenuState;
                if !ptr.is_null() {
                    let event = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                    let message = wparam.0 as u32;
                    if !(*ptr).open {
                        if message == WM_LBUTTONUP && LEFT_OWNED.replace(false) {
                            let _ = PostMessageW(
                                Some(hwnd),
                                DISMISS,
                                WPARAM((*ptr).generation as usize),
                                LPARAM(0),
                            );
                            return LRESULT(1);
                        }
                        return CallNextHookEx(None, code, wparam, lparam);
                    }
                    let mut rect = RECT::default();
                    let inside = GetWindowRect(hwnd, &mut rect).is_ok()
                        && event.pt.x >= rect.left
                        && event.pt.x < rect.right
                        && event.pt.y >= rect.top
                        && event.pt.y < rect.bottom;
                    let captured = LEFT_OWNED.get();
                    match message {
                        WM_MOUSEWHEEL | WM_MOUSEHWHEEL if inside => {
                            if message == WM_MOUSEWHEEL {
                                let delta = (event.mouseData >> 16) as u16 as i16 as i32;
                                let _ = PostMessageW(
                                    Some(hwnd),
                                    WHEEL,
                                    WPARAM((*ptr).generation as usize),
                                    LPARAM(delta as isize),
                                );
                            }
                            return LRESULT(1);
                        }
                        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK if inside => {
                            // Ownership changes in the hook, before posting UI
                            // work: a fast outside release must never escape
                            // while the matching down is still queued.
                            LEFT_OWNED.set(true);
                            let _ = PostMessageW(
                                Some(hwnd),
                                POINTER,
                                WPARAM(((*ptr).generation as usize) << 32 | message as usize),
                                packed_point(event.pt),
                            );
                            return LRESULT(1);
                        }
                        WM_LBUTTONUP if captured => {
                            LEFT_OWNED.set(false);
                            let _ = PostMessageW(
                                Some(hwnd),
                                POINTER,
                                WPARAM(((*ptr).generation as usize) << 32 | message as usize),
                                packed_point(event.pt),
                            );
                            return LRESULT(1);
                        }
                        WM_MOUSEMOVE if captured => {
                            let _ = PostMessageW(
                                Some(hwnd),
                                POINTER,
                                WPARAM(((*ptr).generation as usize) << 32 | message as usize),
                                packed_point(event.pt),
                            );
                        }
                        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
                            if !inside =>
                        {
                            let _ = PostMessageW(
                                Some(hwnd),
                                DISMISS,
                                WPARAM((*ptr).generation as usize),
                                LPARAM(0),
                            );
                            // Outside click is deliberately forwarded unchanged.
                        }
                        _ => {}
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}
