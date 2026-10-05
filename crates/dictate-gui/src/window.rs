use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Arc;

use dictate_core::Config;
use dictate_core::runtime::{Event, Runtime, State};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowRect, HCURSOR,
    HWND_TOPMOST, IDC_ARROW, IDOK, KillTimer, LoadCursorW, MA_NOACTIVATE, MB_ICONWARNING,
    MB_OKCANCEL, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY,
    WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_NCCREATE, WM_PAINT, WM_RBUTTONUP, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::i18n::Language;
use crate::platform::{self, GuiLibAvConverter};
use crate::render::{
    FULL_HEIGHT, FULL_WIDTH, MINIMAL_HEIGHT, MINIMAL_WIDTH, Renderer, button_is_disabled,
    hit_test_button,
};
use crate::resources;
use crate::settings::{
    SettingsWindow, WM_LANGUAGE_CHANGED, WM_OPACITY_CHANGED, WM_WINDOW_SCALE_CHANGED,
};
use crate::taskbar;
use crate::tray::{COMMAND_MINIMAL, COMMAND_QUIT, COMMAND_SETTINGS, TrayIcon, WM_TRAY};

const WM_RUNTIME_EVENT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;
const WM_ESCAPE_COMMAND: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 12;
const ANIMATION_TIMER_ID: usize = 0x7101;
const ANIMATION_INTERVAL_MS: u32 = 16;
const ANIMATION_STEP_SECONDS: f32 = ANIMATION_INTERVAL_MS as f32 / 1_000.0;
const HOVER_DURATION_SECONDS: f32 = 0.140;
const RECORDING_ANIMATION_PERIOD_SECONDS: f32 = 35.65;

struct WindowState {
    hwnd: HWND,
    runtime: Arc<Runtime>,
    async_runtime: Option<tokio::runtime::Runtime>,
    renderer: Renderer,
    tray: Option<TrayIcon>,
    settings: Option<SettingsWindow>,
    minimal: bool,
    rounded: bool,
    dpi: u32,
    window_scale: f64,
    opacity_alpha: u8,
    event: Event,
    language: Language,
    pressed_button: Option<i32>,
    hovered_button: Option<i32>,
    hover_lifts: [f32; 5],
    hover_start_lifts: [f32; 5],
    hover_elapsed: f32,
    mouse_tracking: bool,
    animation_time: f32,
    animation_timer_active: bool,
    drag_active: bool,
    drag_moved: bool,
    drag_start_cursor: POINT,
    drag_start_window: POINT,
}

pub fn run() -> Result<(), String> {
    platform::enable_high_dpi();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|error| error.to_string())?;
    }
    let result = run_inner();
    unsafe {
        CoUninitialize();
    }
    result
}

fn run_inner() -> Result<(), String> {
    let async_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let _guard = async_runtime.enter();
    let config_path = platform::config_path()?;
    let config = if config_path.exists() {
        Config::load(&config_path).map_err(|error| error.to_string())?
    } else {
        let config = Config::default();
        config
            .save(&config_path)
            .map_err(|error| error.to_string())?;
        config
    };
    let window_scale = config.window_scale;
    let runtime =
        Runtime::new(config, Arc::new(GuiLibAvConverter)).map_err(|error| error.to_string())?;
    runtime.enable_retry_buffer();
    let instance = unsafe { GetModuleHandleW(None).map_err(|error| error.to_string())? };
    let cursor: HCURSOR =
        unsafe { LoadCursorW(None, IDC_ARROW).map_err(|error| error.to_string())? };
    let icon = resources::load_app_icon().map_err(|error| error.to_string())?;
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        hIcon: icon,
        hCursor: cursor,
        lpszClassName: w!("DictateRustNativeWindow"),
        hIconSm: icon,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(windows::core::Error::from_win32().to_string());
    }

    // Direct2D supplies the same per-pixel-alpha outline on Windows 10 and 11; the native
    // frame is disabled after creation so the system does not add another border or corner.
    let rounded = true;
    let dpi = platform::system_dpi();
    let mut renderer = Renderer::new().map_err(|error| error.to_string())?;
    renderer.set_dpi(dpi);
    renderer.set_window_scale(window_scale as f32);
    let state = Box::new(WindowState {
        hwnd: HWND::default(),
        runtime: runtime.clone(),
        async_runtime: Some(async_runtime),
        renderer,
        tray: None,
        settings: None,
        minimal: false,
        rounded,
        dpi,
        window_scale,
        opacity_alpha: platform::window_opacity_alpha(runtime.config().opacity),
        event: runtime.snapshot(),
        language: Language::load(),
        pressed_button: None,
        hovered_button: None,
        hover_lifts: [0.0; 5],
        hover_start_lifts: [0.0; 5],
        hover_elapsed: HOVER_DURATION_SECONDS,
        mouse_tracking: false,
        animation_time: 0.0,
        animation_timer_active: false,
        drag_active: false,
        drag_moved: false,
        drag_start_cursor: POINT::default(),
        drag_start_window: POINT::default(),
    });
    let raw_state = Box::into_raw(state);
    let hwnd = unsafe {
        CreateWindowExW(
            // Keep the control surface interactive without allowing it to steal the foreground
            // window from the app receiving dictated text.
            WINDOW_EX_STYLE(WS_EX_LAYERED.0 | WS_EX_TOPMOST.0 | WS_EX_NOACTIVATE.0),
            w!("DictateRustNativeWindow"),
            w!("Dictate"),
            WS_POPUP,
            windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
            windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
            platform::scale_with_factor(FULL_WIDTH, dpi, window_scale),
            platform::scale_with_factor(FULL_HEIGHT, dpi, window_scale),
            None,
            None,
            Some(instance.into()),
            Some(raw_state.cast::<c_void>()),
        )
    }
    .map_err(|error| {
        unsafe { drop(Box::from_raw(raw_state)) };
        error.to_string()
    })?;
    unsafe {
        platform::disable_native_window_frame(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = redraw_overlay(&mut *raw_state);
        let _ = taskbar::set_visible(hwnd, true);
    }
    let weak = Arc::downgrade(&runtime);
    let hwnd_value = hwnd.0 as usize;
    runtime.set_event_handler(Some(Arc::new(move |event| {
        if weak.upgrade().is_none() {
            return;
        }
        let boxed = Box::new(event);
        let pointer = Box::into_raw(boxed);
        let hwnd = HWND(hwnd_value as *mut c_void);
        if unsafe {
            PostMessageW(
                Some(hwnd),
                WM_RUNTIME_EVENT,
                WPARAM(0),
                LPARAM(pointer as isize),
            )
        }
        .is_err()
        {
            unsafe { drop(Box::from_raw(pointer)) };
        }
    })));
    let _ = runtime.start_hotkeys();

    let mut message = MSG::default();
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if message.message == WM_KEYDOWN
                && !crate::settings::microphone_handles_escape(message.hwnd)
                && message.wParam.0
                    == windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 as usize
            {
                let _ = PostMessageW(Some(hwnd), WM_ESCAPE_COMMAND, WPARAM(0), LPARAM(0));
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        let state = create.lpCreateParams as *mut WindowState;
        unsafe {
            (*state).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *pointer };
    match message {
        WM_CREATE => {
            state.tray = TrayIcon::create(hwnd).ok();
            if state.event.state == State::Recording {
                ensure_animation_timer(state);
            }
            LRESULT(0)
        }
        // Keep mouse actions (click, drag, and hover) working without making the overlay active.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut paint);
            }
            let _ = redraw_overlay(state);
            unsafe {
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 & 0xffff) as u32;
            let height = ((lparam.0 >> 16) & 0xffff) as u32;
            state.renderer.resize(width, height);
            let _ = redraw_overlay(state);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            state.dpi = ((wparam.0 >> 16) & 0xffff) as u32;
            state.renderer.set_dpi(state.dpi);
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = logical_point(lparam, state.dpi, state.window_scale);
            state.pressed_button = hit_test_button(x, y, state.minimal);
            let _ = redraw_overlay(state);
            if state.pressed_button.is_some() {
                unsafe {
                    SetCapture(hwnd);
                }
            }
            if state.minimal || state.pressed_button.is_none() {
                state.drag_active = true;
                state.drag_moved = false;
                let mut cursor = POINT::default();
                let mut window = RECT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                    let _ = GetWindowRect(hwnd, &mut window);
                    SetCapture(hwnd);
                }
                state.drag_start_cursor = cursor;
                state.drag_start_window = POINT {
                    x: window.left,
                    y: window.top,
                };
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            track_mouse_leave(state);
            let (x, y) = logical_point(lparam, state.dpi, state.window_scale);
            if state.drag_active {
                let mut cursor = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut cursor);
                }
                let delta_x = cursor.x - state.drag_start_cursor.x;
                let delta_y = cursor.y - state.drag_start_cursor.y;
                if !state.drag_moved
                    && delta_x.abs() + delta_y.abs()
                        >= platform::scale_with_factor(4, state.dpi, state.window_scale)
                {
                    state.drag_moved = true;
                }
                if state.drag_moved {
                    set_hovered_button(state, None);
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            state.drag_start_window.x + delta_x,
                            state.drag_start_window.y + delta_y,
                            0,
                            0,
                            SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
                        );
                    }
                } else {
                    update_hover(state, x, y);
                }
            } else {
                update_hover(state, x, y);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            state.mouse_tracking = false;
            set_hovered_button(state, None);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ANIMATION_TIMER_ID => {
            if advance_animation(state) {
                let _ = redraw_overlay(state);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = logical_point(lparam, state.dpi, state.window_scale);
            let released = hit_test_button(x, y, state.minimal);
            let pressed = state.pressed_button.take();
            let _ = redraw_overlay(state);
            let moved = state.drag_moved;
            if state.drag_active {
                state.drag_active = false;
                state.drag_moved = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
            } else if pressed.is_some() {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            if !moved
                && let Some(button) = pressed
                && Some(button) == released
            {
                handle_button(state, button);
            }
            update_hover(state, x, y);
            LRESULT(0)
        }
        WM_RBUTTONUP if state.minimal => {
            show_tray_menu(state);
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_command(state, wparam.0 & 0xffff);
            LRESULT(0)
        }
        WM_TRAY => {
            let event = (lparam.0 & 0xffff) as u32;
            if event == windows::Win32::UI::WindowsAndMessaging::WM_CONTEXTMENU
                || event == windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP
            {
                show_tray_menu(state);
            } else if event == windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDBLCLK {
                set_minimal(state, false);
            }
            LRESULT(0)
        }
        WM_RUNTIME_EVENT => {
            let event = unsafe { Box::from_raw(lparam.0 as *mut Event) };
            let previous_state = state.event.state;
            state.event = *event;
            if previous_state != State::Recording && state.event.state == State::Recording {
                state.animation_time = 0.0;
            }
            if state
                .hovered_button
                .is_some_and(|button| button_is_disabled(button, &state.event))
            {
                set_hovered_button(state, None);
            }
            if state.event.state == State::Recording {
                ensure_animation_timer(state);
            } else if !hover_transition_pending(state) {
                stop_animation_timer(state);
            }
            let _ = redraw_overlay(state);
            LRESULT(0)
        }
        WM_LANGUAGE_CHANGED => {
            if let Some(language) = Language::ALL.get(wparam.0).copied() {
                state.language = language;
                let _ = redraw_overlay(state);
            }
            LRESULT(0)
        }
        WM_OPACITY_CHANGED => {
            state.opacity_alpha = wparam.0.min(u8::MAX as usize) as u8;
            let _ = redraw_overlay(state);
            LRESULT(0)
        }
        WM_WINDOW_SCALE_CHANGED => {
            state.window_scale = state.runtime.config().window_scale;
            state.renderer.set_window_scale(state.window_scale as f32);
            resize_overlay(state);
            let _ = redraw_overlay(state);
            LRESULT(0)
        }
        WM_ESCAPE_COMMAND => {
            if !close_settings(state) {
                request_quit(state);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            request_quit(state);
            LRESULT(0)
        }
        WM_DESTROY => {
            stop_animation_timer(state);
            state.runtime.set_event_handler(None);
            state.runtime.stop();
            close_settings(state);
            state.tray = None;
            if let Some(async_runtime) = state.async_runtime.take() {
                async_runtime.shutdown_background();
            }
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(pointer));
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn redraw_overlay(state: &mut WindowState) -> windows::core::Result<()> {
    state.renderer.paint(
        state.hwnd,
        &state.event,
        state.minimal,
        state.rounded,
        state.language,
        state.pressed_button,
        &state.hover_lifts,
        state.animation_time,
        state.opacity_alpha,
    )
}

fn show_tray_menu(state: &mut WindowState) {
    let command = state
        .tray
        .as_ref()
        .and_then(|tray| tray.show_menu(state.minimal, state.language));
    if let Some(command) = command {
        handle_command(state, command);
    }
}

fn handle_command(state: &mut WindowState, command: usize) {
    match command {
        COMMAND_MINIMAL => set_minimal(state, !state.minimal),
        COMMAND_SETTINGS => open_settings(state),
        COMMAND_QUIT => request_quit(state),
        _ => {}
    }
}

fn handle_button(state: &mut WindowState, button: i32) {
    if button_is_disabled(button, &state.event) {
        return;
    }
    match button {
        0 => {
            state.runtime.try_toggle_recording();
        }
        1 => {
            state.runtime.try_toggle_pause();
        }
        2 => {
            state.runtime.try_cancel_or_retry();
        }
        3 if state.minimal => set_minimal(state, false),
        3 => open_settings(state),
        4 => set_minimal(state, true),
        _ => {}
    }
}

fn set_minimal(state: &mut WindowState, minimal: bool) {
    state.minimal = minimal;
    state.hovered_button = None;
    state.hover_lifts = [0.0; 5];
    state.hover_start_lifts = [0.0; 5];
    state.hover_elapsed = HOVER_DURATION_SECONDS;
    state.mouse_tracking = false;
    if state.event.state == State::Recording {
        ensure_animation_timer(state);
    } else {
        stop_animation_timer(state);
    }
    if minimal {
        close_settings(state);
    }
    resize_overlay(state);
    let _ = taskbar::set_visible(state.hwnd, !minimal);
    let _ = redraw_overlay(state);
}

fn resize_overlay(state: &WindowState) {
    let (width, height) = if state.minimal {
        (MINIMAL_WIDTH, MINIMAL_HEIGHT)
    } else {
        (FULL_WIDTH, FULL_HEIGHT)
    };
    unsafe {
        let _ = SetWindowPos(
            state.hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            platform::scale_with_factor(width, state.dpi, state.window_scale),
            platform::scale_with_factor(height, state.dpi, state.window_scale),
            SWP_NOMOVE | SWP_NOACTIVATE,
        );
    }
}

fn open_settings(state: &mut WindowState) {
    set_minimal(state, false);
    if let Some(settings) = &state.settings {
        if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(settings.hwnd())) }
            .as_bool()
        {
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(settings.hwnd());
            }
            return;
        }
        state.settings = None;
    }
    match SettingsWindow::open(state.hwnd, state.runtime.clone(), state.language) {
        Ok(settings) => state.settings = Some(settings),
        Err(error) => show_message(state.hwnd, &error, false),
    }
}

fn close_settings(state: &mut WindowState) -> bool {
    let Some(settings) = state.settings.take() else {
        return false;
    };
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(settings.hwnd())) }.as_bool()
    {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(settings.hwnd());
        }
        return true;
    }
    false
}

fn request_quit(state: &mut WindowState) {
    let busy = matches!(
        state.runtime.snapshot().state,
        State::Recording | State::Paused | State::Uploading | State::Rewriting
    );
    if busy {
        let message = wide(state.language.text("quit_busy"));
        let title = wide(state.language.text("quit_title"));
        let result = unsafe {
            MessageBoxW(
                Some(state.hwnd),
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OKCANCEL | MB_ICONWARNING,
            )
        };
        if result != IDOK {
            return;
        }
    }
    state.runtime.stop();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(state.hwnd);
    }
}

fn show_message(owner: HWND, text: &str, warning: bool) {
    let text = wide(text);
    unsafe {
        let _ = MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            w!("Dictate"),
            if warning {
                MB_ICONWARNING
            } else {
                windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR
            } | windows::Win32::UI::WindowsAndMessaging::MB_OK,
        );
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn point_from_lparam(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 as u16) as i16 as i32;
    let y = ((lparam.0 >> 16) as u16) as i16 as i32;
    (x, y)
}

fn logical_point(lparam: LPARAM, dpi: u32, window_scale: f64) -> (i32, i32) {
    let (x, y) = point_from_lparam(lparam);
    (
        platform::unscale_with_factor(x, dpi, window_scale),
        platform::unscale_with_factor(y, dpi, window_scale),
    )
}

fn track_mouse_leave(state: &mut WindowState) {
    if state.mouse_tracking {
        return;
    }
    let mut tracking = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: state.hwnd,
        dwHoverTime: 0,
    };
    if unsafe { TrackMouseEvent(&mut tracking) }.is_ok() {
        state.mouse_tracking = true;
    }
}

fn update_hover(state: &mut WindowState, x: i32, y: i32) {
    let hovered = hit_test_button(x, y, state.minimal)
        .filter(|button| !button_is_disabled(*button, &state.event));
    set_hovered_button(state, hovered);
}

fn set_hovered_button(state: &mut WindowState, hovered: Option<i32>) {
    if state.hovered_button == hovered {
        return;
    }
    state.hovered_button = hovered;
    state.hover_start_lifts = state.hover_lifts;
    state.hover_elapsed = 0.0;
    ensure_animation_timer(state);
}

fn ensure_animation_timer(state: &mut WindowState) {
    if state.animation_timer_active {
        return;
    }
    let timer = unsafe {
        SetTimer(
            Some(state.hwnd),
            ANIMATION_TIMER_ID,
            ANIMATION_INTERVAL_MS,
            None,
        )
    };
    state.animation_timer_active = timer != 0;
}

fn stop_animation_timer(state: &mut WindowState) {
    if !state.animation_timer_active {
        return;
    }
    unsafe {
        let _ = KillTimer(Some(state.hwnd), ANIMATION_TIMER_ID);
    }
    state.animation_timer_active = false;
}

fn advance_animation(state: &mut WindowState) -> bool {
    let mut changed = false;
    if state.event.state == State::Recording {
        state.animation_time = (state.animation_time + ANIMATION_STEP_SECONDS)
            .rem_euclid(RECORDING_ANIMATION_PERIOD_SECONDS);
        changed = true;
    }

    if hover_transition_pending(state) {
        state.hover_elapsed =
            (state.hover_elapsed + ANIMATION_STEP_SECONDS).min(HOVER_DURATION_SECONDS);
        let progress = state.hover_elapsed / HOVER_DURATION_SECONDS;
        let eased = css_ease(progress);
        for index in 0..state.hover_lifts.len() {
            let target = hover_target(state, index);
            state.hover_lifts[index] =
                state.hover_start_lifts[index] + (target - state.hover_start_lifts[index]) * eased;
        }
        if state.hover_elapsed >= HOVER_DURATION_SECONDS {
            for index in 0..state.hover_lifts.len() {
                state.hover_lifts[index] = hover_target(state, index);
            }
        }
        changed = true;
    }

    if state.event.state != State::Recording && !hover_transition_pending(state) {
        stop_animation_timer(state);
    }
    changed
}

fn hover_transition_pending(state: &WindowState) -> bool {
    state
        .hover_lifts
        .iter()
        .enumerate()
        .any(|(index, lift)| (*lift - hover_target(state, index)).abs() > f32::EPSILON)
}

fn hover_target(state: &WindowState, index: usize) -> f32 {
    if state.hovered_button == Some(index as i32) {
        -1.0
    } else {
        0.0
    }
}

fn css_ease(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress == 0.0 || progress == 1.0 {
        return progress;
    }

    let mut parameter = progress;
    for _ in 0..6 {
        let estimate = cubic_bezier_component(parameter, 0.25, 0.25);
        let derivative = cubic_bezier_derivative(parameter, 0.25, 0.25);
        if derivative.abs() <= f32::EPSILON {
            break;
        }
        parameter = (parameter - (estimate - progress) / derivative).clamp(0.0, 1.0);
    }
    cubic_bezier_component(parameter, 0.10, 1.0)
}

fn cubic_bezier_component(parameter: f32, first: f32, second: f32) -> f32 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * parameter * first
        + 3.0 * inverse * parameter * parameter * second
        + parameter * parameter * parameter
}

fn cubic_bezier_derivative(parameter: f32, first: f32, second: f32) -> f32 {
    let inverse = 1.0 - parameter;
    3.0 * inverse * inverse * first
        + 6.0 * inverse * parameter * (second - first)
        + 3.0 * parameter * parameter * (1.0 - second)
}
