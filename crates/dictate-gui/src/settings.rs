use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use dictate_core::Config;
use dictate_core::runtime::{Event, Runtime};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen, CreatePolygonRgn,
    CreateSolidBrush, DC_BRUSH, DC_PEN, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW,
    EndPaint, FF_DONTCARE, FW_BOLD, FW_NORMAL, FillRect, GetStockObject, HBRUSH, HDC, HFONT,
    HGDIOBJ, InvalidateRect, LineTo, MoveToEx, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID,
    RoundRect, ScreenToClient, SelectObject, SetBkColor, SetBkMode, SetDCBrushColor, SetDCPenColor,
    SetTextColor, SetWindowRgn, TRANSPARENT, WINDING,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, EM_SETMARGINS, ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_SELECTED,
    SetWindowTheme,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_OWNERDRAW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DI_NORMAL,
    DefWindowProcW, DestroyIcon, DestroyWindow, DrawIconEx, EC_LEFTMARGIN, EC_RIGHTMARGIN,
    EN_CHANGE, EN_KILLFOCUS, EN_SETFOCUS, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE,
    ES_PASSWORD, ES_READONLY, ES_WANTRETURN, GWLP_USERDATA, GetClientRect, GetMessagePos,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, HCURSOR, HMENU,
    HTCAPTION, HTCLIENT, HWND_TOP, IDC_ARROW, IsWindowVisible, LoadCursorW, MB_ICONERROR, MB_OK,
    MessageBoxW, PostMessageW, RegisterClassExW, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WINDOWPOS, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX,
    WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_SETFONT, WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT, WS_POPUP, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

use crate::i18n::Language;
use crate::platform;
use crate::render::{PANEL_CORNER_RADIUS, RoundedOutlineRenderer, continuous_rounded_rect_polygon};
use crate::resources;

mod advanced_audio;
mod audio;
mod debug;
mod dropdown;
#[path = "dropdown_scrollbar.rs"]
mod dropdown_scrollbar;
mod hotkeys;
mod microphone;
mod rewrite;
use microphone::{
    ID_MICROPHONE, ID_MICROPHONE_LIST, MICROPHONE_TIMER, MicrophonePicker, WM_MICROPHONE_ACTION,
};

pub fn microphone_handles_escape(hwnd: HWND) -> bool {
    if hotkeys::value(hwnd).is_some() {
        return true;
    }
    unsafe {
        let id = windows::Win32::UI::WindowsAndMessaging::GetDlgCtrlID(hwnd) as usize;
        rewrite::handles_escape(id)
            || id == ID_MICROPHONE_LIST
            || (audio::ID_LIST_BASE..audio::ID_LIST_BASE + 6).contains(&id)
            || (0x6410..0x6416).contains(&id)
    }
}

const ID_SAVE: usize = 0x6101;
const ID_CANCEL: usize = 0x6102;
const ID_LANGUAGE: usize = 0x6103;
const ID_CLOSE: usize = 0x6104;
const ID_TEST_CONNECTIVITY: usize = 0x6105;
const ID_PAGE_BASE: usize = 0x6110;
const ID_LANGUAGE_ITEM_BASE: usize = 0x6180;
const ID_FIELD_BASE: usize = 0x6200;
const SAVE_TIMER: usize = 0x6701;
pub const WM_LANGUAGE_CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 22;
const CONNECTIVITY_TIMER: usize = 0x6700;
pub const WM_OPACITY_CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 24;
pub const WM_WINDOW_SCALE_CHANGED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 25;

const WINDOW_WIDTH: i32 = 760;
const WINDOW_HEIGHT: i32 = 662;
const HEADER_HEIGHT: i32 = 72;
const FOOTER_HEIGHT: i32 = 58;
const SIDEBAR_WIDTH: i32 = 170;
const CONTENT_LEFT: i32 = 196;
const LABEL_WIDTH: i32 = 148;
const EDIT_LEFT: i32 = 354;
const EDIT_WIDTH: i32 = 378;
const FIELD_TOP: i32 = 94;
const FIELD_HEIGHT: i32 = 34;
const SS_CENTERIMAGE_STYLE: u32 = 0x0000_0200;
const CLIPBOARD_DELAY_KEYS: [&str; 2] = ["CLIPBOARD_WRITE_DELAY", "CLIPBOARD_RESTORE_DELAY"];
const SEGMENTED_UPLOAD_KEYS: [&str; 3] = [
    "MAX_UPLOAD_SEGMENT_SECONDS",
    "MIN_UPLOAD_PAUSE_MS",
    "MAX_UPLOAD_CONCURRENCY",
];
const CONNECTIVITY_TEST_SAMPLE_RATE: i32 = 16_000;
const CONNECTIVITY_TEST_PCM: &str = include_str!("../../../scripts/connectivity_test.pcm.b64");

const GROUPS: &[(&str, &str)] = &[
    ("Display", "display"),
    ("API", "audio_api"),
    (advanced_audio::GROUP, "advanced_audio_api"),
    ("Audio", "audio_record"),
    ("Rewrite", "rewrite_api"),
    ("Network", "network"),
    ("Hotkeys", "audio_hotkeys"),
    ("Cache", "cache"),
    ("Debug", "debug"),
    ("About", "about"),
];

#[derive(Clone, Copy)]
enum FieldKind {
    Text,
    Password,
    Integer,
    Float,
    Boolean,
    Multiline,
}

struct FieldSpec {
    key: &'static str,
    label: &'static str,
    group: &'static str,
    kind: FieldKind,
}

const FIELDS: &[FieldSpec] = &[
    FieldSpec {
        key: "OPACITY",
        label: "Opacity",
        group: "Display",
        kind: FieldKind::Float,
    },
    FieldSpec {
        key: "WINDOW_SCALE",
        label: "Floating window scale",
        group: "Display",
        kind: FieldKind::Float,
    },
    FieldSpec {
        key: "API_ENDPOINT",
        label: "API endpoint",
        group: "API",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "TOKEN",
        label: "Token",
        group: "API",
        kind: FieldKind::Password,
    },
    FieldSpec {
        key: "MODEL",
        label: "Model",
        group: "API",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "LANGUAGE",
        label: "Language",
        group: "API",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "PROMPT",
        label: "Prompt",
        group: "API",
        kind: FieldKind::Multiline,
    },
    FieldSpec {
        key: "TEXT_PATH",
        label: "Text path",
        group: "API",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "ExtraConfig",
        label: "Extra config",
        group: "API",
        kind: FieldKind::Multiline,
    },
    FieldSpec {
        key: "CHANNELS",
        label: "Output channels",
        group: "Audio",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "SAMPLING_RATE",
        label: "Output sample rate",
        group: "Audio",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "SAMPLING_RATE_DEPTH",
        label: "Output sample depth",
        group: "Audio",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "BIT_RATE",
        label: "Bit rate",
        group: "Audio",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "CODECS",
        label: "Codec",
        group: "Audio",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "CONTAINER",
        label: "Container",
        group: "Audio",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "ENABLE_VAD",
        label: "Voice activity detection",
        group: "Audio",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "VAD_PADDING_MS",
        label: "VAD padding (ms)",
        group: "Audio",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "VAD_START_THRESHOLD",
        label: "VAD start threshold",
        group: "Audio",
        kind: FieldKind::Float,
    },
    FieldSpec {
        key: "REQUEST_TIMEOUT",
        label: "Request timeout",
        group: "Network",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "MAX_RETRY",
        label: "Max retry",
        group: "Network",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "RETRY_BASE_DELAY",
        label: "Retry delay",
        group: "Network",
        kind: FieldKind::Float,
    },
    FieldSpec {
        key: "ENABLE_HTTP2",
        label: "HTTP/2",
        group: "Network",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "VERIFY_SSL",
        label: "Verify SSL",
        group: "Network",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "ENABLE_SEGMENTED_UPLOAD",
        label: "Segmented upload",
        group: "Network",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "MAX_UPLOAD_SEGMENT_SECONDS",
        label: "Max segment (s)",
        group: "Network",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "MIN_UPLOAD_PAUSE_MS",
        label: "Min pause (ms)",
        group: "Network",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "MAX_UPLOAD_CONCURRENCY",
        label: "Upload concurrency",
        group: "Network",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "START_KEY",
        label: "Start key",
        group: "Hotkeys",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "PAUSE_KEY",
        label: "Pause key",
        group: "Hotkeys",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "CANCEL_OR_RETRY_KEY",
        label: "Cancel or Retry Key",
        group: "Hotkeys",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "HOTKEY_HOOK",
        label: "Low-level hook",
        group: "Hotkeys",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "CLIPBOARD_WRITE_DELAY",
        label: "Paste delay (ms)",
        group: "Hotkeys",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "CLIPBOARD_RESTORE_DELAY",
        label: "Restore delay (ms)",
        group: "Hotkeys",
        kind: FieldKind::Integer,
    },
    FieldSpec {
        key: "USE_SENDINPUT",
        label: "Use SendInput",
        group: "Hotkeys",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "CACHE_DIR",
        label: "Cache dir",
        group: "Cache",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "KEEP_CACHE",
        label: "Keep cache",
        group: "Cache",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "REQUEST_FAILED_NOTIFICATION",
        label: "Request failed placeholder",
        group: "Cache",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "FFMPEG_DEBUG",
        label: "FFmpeg debug",
        group: "Debug",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "RECORD_DEBUG",
        label: "Record debug",
        group: "Debug",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "HOTKEY_DEBUG",
        label: "Hotkey debug",
        group: "Debug",
        kind: FieldKind::Boolean,
    },
    FieldSpec {
        key: "UPLOAD_DEBUG",
        label: "Upload debug",
        group: "Debug",
        kind: FieldKind::Boolean,
    },
];

struct SettingsState {
    hwnd: HWND,
    owner: HWND,
    runtime: Arc<Runtime>,
    controls: HashMap<&'static str, HWND>,
    layouts: RefCell<Vec<(HWND, RECT)>>,
    config_path: std::path::PathBuf,
    saving: bool,
    save_receiver: Option<std::sync::mpsc::Receiver<Result<Event, String>>>,
    connectivity_status: ConnectivityStatus,
    connectivity_cancel: tokio_util::sync::CancellationToken,
    connectivity_receiver: Option<std::sync::mpsc::Receiver<(Result<(), String>, u128)>>,
    rewrite_test: bool,
    connectivity_elapsed: u128,
    advanced_audio: advanced_audio::Page,
    rewrite: rewrite::Page,
    debug: debug::Page,
    language: Language,
    language_control: HWND,
    language_items: Vec<HWND>,
    language_panel: HWND,
    language_open: bool,
    microphone: Option<MicrophonePicker>,
    audio_draft: crate::audio_options::AudioDraft,
    audio_pickers: Vec<audio::AudioPicker>,
    boolean_values: HashMap<&'static str, bool>,
    boolean_ids: HashMap<usize, &'static str>,
    input_frames: Vec<InputFrame>,
    control_groups: HashMap<usize, &'static str>,
    localized_controls: Vec<(HWND, &'static str)>,
    page_buttons: Vec<HWND>,
    active_group: usize,
    dpi: u32,
    background_brush: HBRUSH,
    input_brush: HBRUSH,
    font: HFONT,
    title_font: HFONT,
    small_font: HFONT,
    frame_overlay: Option<SettingsFrameOverlay>,
}

struct SettingsFrameOverlay {
    hwnd: HWND,
    renderer: RoundedOutlineRenderer,
}

enum ConnectivityStatus {
    Idle,
    Testing,
    Succeeded,
    Failed(String),
}

#[derive(Clone, Copy)]
struct InputFrame {
    rect: RECT,
    group: &'static str,
    control: HWND,
}

pub struct SettingsWindow {
    hwnd: HWND,
}

impl SettingsFrameOverlay {
    fn create(
        owner: HWND,
        instance: windows::Win32::Foundation::HMODULE,
        dpi: u32,
        width: i32,
        height: i32,
    ) -> Result<Self, String> {
        let renderer = RoundedOutlineRenderer::new(
            width as f32,
            height as f32,
            PANEL_CORNER_RADIUS as f32,
            dpi,
        )
        .map_err(|error| error.to_string())?;
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(
                    WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0,
                ),
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                platform::scale(width, dpi),
                platform::scale(height, dpi),
                Some(owner),
                None,
                Some(instance.into()),
                None,
            )
        }
        .map_err(|error| error.to_string())?;
        platform::disable_native_window_frame(hwnd);
        Ok(Self { hwnd, renderer })
    }

    fn set_dpi(&mut self, dpi: u32) {
        self.renderer.set_dpi(dpi);
    }

    fn sync(&mut self, owner: HWND, repaint: bool) {
        let mut window = RECT::default();
        if unsafe { GetWindowRect(owner, &mut window) }.is_err() {
            return;
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                window.left,
                window.top,
                window.right - window.left,
                window.bottom - window.top,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        if repaint {
            let _ = self.renderer.paint(self.hwnd);
        }
    }
}

impl SettingsWindow {
    pub fn open(owner: HWND, runtime: Arc<Runtime>, language: Language) -> Result<Self, String> {
        let instance = unsafe { GetModuleHandleW(None).map_err(|error| error.to_string())? };
        let cursor: HCURSOR =
            unsafe { LoadCursorW(None, IDC_ARROW).map_err(|error| error.to_string())? };
        let icon = resources::load_app_icon().map_err(|error| error.to_string())?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(settings_proc),
            hInstance: instance.into(),
            hIcon: icon,
            hCursor: cursor,
            lpszClassName: w!("DictateRustSettingsWindow"),
            hIconSm: icon,
            ..Default::default()
        };
        unsafe {
            RegisterClassExW(&class);
        }

        let dpi = platform::window_dpi(owner);
        let state = Box::new(SettingsState {
            hwnd: HWND::default(),
            owner,
            runtime: runtime.clone(),
            controls: HashMap::new(),
            layouts: RefCell::new(Vec::new()),
            config_path: platform::config_path()?,
            saving: false,
            save_receiver: None,
            connectivity_status: ConnectivityStatus::Idle,
            connectivity_cancel: tokio_util::sync::CancellationToken::new(),
            connectivity_receiver: None,
            rewrite_test: false,
            connectivity_elapsed: 0,
            advanced_audio: advanced_audio::Page::new(&runtime.config()),
            rewrite: rewrite::Page::new(runtime.config().rewrite),
            debug: debug::Page::default(),
            language,
            language_control: HWND::default(),
            language_items: Vec::new(),
            language_panel: HWND::default(),
            language_open: false,
            microphone: None,
            audio_draft: crate::audio_options::AudioDraft::new(&runtime.config()),
            audio_pickers: Vec::new(),
            boolean_values: HashMap::new(),
            boolean_ids: HashMap::new(),
            input_frames: Vec::new(),
            control_groups: HashMap::new(),
            localized_controls: Vec::new(),
            page_buttons: Vec::new(),
            active_group: 0,
            dpi,
            background_brush: unsafe { CreateSolidBrush(rgb(16, 22, 25)) },
            input_brush: unsafe { CreateSolidBrush(rgb(26, 35, 39)) },
            font: create_font(dpi, 14, false),
            title_font: create_font(dpi, 20, true),
            small_font: create_font(dpi, 11, false),
            frame_overlay: None,
        });
        let pointer = Box::into_raw(state);
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("DictateRustSettingsWindow"),
                PCWSTR(wide(&format!("Dictate - {}", language.text("settings"))).as_ptr()),
                WS_POPUP | WS_CLIPCHILDREN,
                windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                windows::Win32::UI::WindowsAndMessaging::CW_USEDEFAULT,
                platform::scale(WINDOW_WIDTH, dpi),
                platform::scale(WINDOW_HEIGHT, dpi),
                Some(owner),
                None,
                Some(instance.into()),
                Some(pointer.cast::<c_void>()),
            )
        }
        .map_err(|error| {
            unsafe { drop(Box::from_raw(pointer)) };
            error.to_string()
        })?;
        let frame_overlay =
            match SettingsFrameOverlay::create(hwnd, instance, dpi, WINDOW_WIDTH, WINDOW_HEIGHT) {
                Ok(frame_overlay) => frame_overlay,
                Err(error) => {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                    return Err(error);
                }
            };
        unsafe {
            (*pointer).frame_overlay = Some(frame_overlay);
            apply_settings_region(hwnd, dpi);
            platform::apply_dark_mode(hwnd);
            platform::disable_native_window_frame(hwnd);
            let _ = ShowWindow(hwnd, SW_SHOW);
            sync_settings_frame(&mut *pointer, true);
        }
        Ok(Self { hwnd })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        let state = create.lpCreateParams as *mut SettingsState;
        unsafe {
            (*state).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut SettingsState;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *pointer };
    if rewrite::handle_message(state, message, wparam, lparam) {
        return LRESULT(0);
    }
    if advanced_audio::handle_message(state, message, wparam, lparam) {
        return LRESULT(0);
    }
    match message {
        WM_CREATE => {
            if let Err(error) = create_controls(state) {
                show_error(hwnd, &error);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let packed = unsafe { GetMessagePos() };
            let mut point = POINT {
                x: (packed as u16) as i16 as i32,
                y: ((packed >> 16) as u16) as i16 as i32,
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut point);
            }
            let logical_y = platform::unscale(point.y, state.dpi);
            let logical_x = platform::unscale(point.x, state.dpi);
            if logical_y < HEADER_HEIGHT && logical_x < WINDOW_WIDTH - 64 {
                LRESULT(HTCAPTION as isize)
            } else {
                LRESULT(HTCLIENT as isize)
            }
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            let notification = ((wparam.0 >> 16) & 0xffff) as u32;
            if debug::command(state, id, notification) {
                return LRESULT(0);
            }
            if rewrite::command(state, id, notification) {
                return LRESULT(0);
            }
            if advanced_audio::command(state, id, notification) {
                return LRESULT(0);
            }
            if notification == BN_CLICKED
                && let Some(field) = state
                    .audio_pickers
                    .iter()
                    .find(|p| p.button.0 as isize == lparam.0)
                    .map(|p| p.field)
            {
                audio::action(state, field, 4, LPARAM(0));
                return LRESULT(0);
            }
            if id >= ID_PAGE_BASE && id < ID_PAGE_BASE + GROUPS.len() && notification == BN_CLICKED
            {
                hotkeys::deactivate();
                rewrite::close_provider(state);
                advanced_audio::close_dynamic_select(state);
                set_language_dropdown(state, false);
                audio::close_all(state);
                if let Some(picker) = &mut state.microphone {
                    picker.set_open(false, state.language, state.dpi);
                }
                state.active_group = id - ID_PAGE_BASE;
                update_page_visibility(state);
                audio::refresh(state);
                for button in &state.page_buttons {
                    unsafe {
                        let _ = InvalidateRect(Some(*button), None, true);
                    }
                }
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            } else if id == ID_LANGUAGE && notification == BN_CLICKED {
                set_language_dropdown(state, !state.language_open);
            } else if id == ID_MICROPHONE && notification == BN_CLICKED && !state.saving {
                audio::close_all(state);
                if let Some(picker) = &mut state.microphone {
                    picker.set_open(!picker.open, state.language, state.dpi);
                }
            } else if id >= ID_LANGUAGE_ITEM_BASE
                && id < ID_LANGUAGE_ITEM_BASE + Language::ALL.len()
                && notification == BN_CLICKED
            {
                select_language(state, id - ID_LANGUAGE_ITEM_BASE);
            } else if let Some(key) = state.boolean_ids.get(&id).copied()
                && notification == BN_CLICKED
            {
                let value = state.boolean_values.entry(key).or_default();
                *value = !*value;
                update_input_controls(state);
                rewrite::refresh_hotkey_context(state);
                if let Some(control) = state.controls.get(key) {
                    unsafe {
                        let _ = InvalidateRect(Some(*control), None, true);
                    }
                }
            } else if id >= ID_FIELD_BASE
                && id < ID_FIELD_BASE + FIELDS.len()
                && (notification == EN_SETFOCUS || notification == EN_KILLFOCUS)
            {
                if notification == EN_KILLFOCUS && FIELDS[id - ID_FIELD_BASE].key == "ExtraConfig" {
                    format_json_input(HWND(lparam.0 as *mut c_void));
                }
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            } else {
                match id {
                    ID_SAVE => save(state),
                    ID_CANCEL | ID_CLOSE => unsafe {
                        let _ = DestroyWindow(hwnd);
                    },
                    ID_TEST_CONNECTIVITY => test_connectivity(state),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            rewrite::close_provider(state);
            advanced_audio::close_dynamic_select(state);
            audio::close_all(state);
            if let Some(picker) = &mut state.microphone {
                picker.set_open(false, state.language, state.dpi);
            }
            if state.language_open {
                set_language_dropdown(state, false);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_PAINT => {
            paint_window(state);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DRAWITEM => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if rewrite::draw(state, item) {
                return LRESULT(1);
            }
            if advanced_audio::draw_dynamic_list(state, item) {
                return LRESULT(1);
            }
            if item.CtlID as usize == ID_MICROPHONE_LIST {
                if let Some(picker) = &state.microphone {
                    picker.draw(state, item);
                }
            } else if let Some(picker) =
                state.audio_pickers.iter().find(|p| item.hwndItem == p.list)
            {
                picker.draw(state, item);
            } else {
                draw_owner_button(state, item);
            }
            LRESULT(1)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_MEASUREITEM => {
            let item =
                unsafe { &mut *(lparam.0 as *mut windows::Win32::UI::Controls::MEASUREITEMSTRUCT) };
            if let Some(height) =
                advanced_audio::dynamic_list_item_height(item.CtlID as usize, state.dpi)
            {
                item.itemHeight = height as u32;
                LRESULT(1)
            } else if item.CtlID as usize == ID_MICROPHONE_LIST
                || item.CtlID as usize == rewrite::ID_PROMPTS
                || (audio::ID_LIST_BASE..audio::ID_LIST_BASE + 6).contains(&(item.CtlID as usize))
            {
                item.itemHeight = platform::scale(36, state.dpi) as u32;
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == MICROPHONE_TIMER => {
            if let Some(picker) = &mut state.microphone {
                picker.poll(state.language, state.dpi);
            }
            LRESULT(0)
        }
        audio::WM_ACTION => {
            audio::action(state, wparam.0 >> 8, wparam.0 & 0xff, lparam);
            LRESULT(0)
        }
        WM_MICROPHONE_ACTION => {
            if let Some(picker) = &mut state.microphone {
                match wparam.0 {
                    1 if picker.open && !state.saving => picker.select(state.language, state.dpi),
                    2 => {
                        picker.set_open(false, state.language, state.dpi);
                        unsafe {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(
                                picker.button,
                            ));
                        }
                    }
                    3 if lparam.0 != picker.button.0 as isize
                        && lparam.0 != picker.list.0 as isize =>
                    {
                        picker.set_open(false, state.language, state.dpi)
                    }
                    4 if !state.saving && state.active_group == 3 => {
                        picker.set_open(true, state.language, state.dpi)
                    }
                    5 => {
                        picker.set_open(false, state.language, state.dpi);
                        unsafe {
                            if let Ok(next) =
                                windows::Win32::UI::WindowsAndMessaging::GetNextDlgTabItem(
                                    hwnd,
                                    Some(picker.button),
                                    lparam.0 != 0,
                                )
                            {
                                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(
                                    Some(next),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut c_void);
            unsafe {
                SetBkMode(hdc, TRANSPARENT);
                let control = HWND(lparam.0 as *mut c_void);
                if advanced_audio::is_generation_status_control(state, control) {
                    SetBkMode(hdc, windows::Win32::Graphics::Gdi::OPAQUE);
                    SetBkColor(hdc, rgb(26, 35, 39));
                    SetTextColor(hdc, rgb(221, 183, 105));
                    return LRESULT(state.input_brush.0 as isize);
                }
                // Keep these STATIC labels enabled to avoid embossed disabled text.
                // Their color follows the corresponding input's enabled state.
                let color_control = state
                    .localized_controls
                    .iter()
                    .find(|(label, key)| *label == control && CLIPBOARD_DELAY_KEYS.contains(key))
                    .and_then(|(_, key)| state.controls.get(key))
                    .copied()
                    .unwrap_or(control);
                let enabled =
                    windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(color_control)
                        .as_bool();
                SetTextColor(
                    hdc,
                    if enabled {
                        rgb(188, 202, 205)
                    } else {
                        rgb(110, 126, 130)
                    },
                );
                if state.controls.values().any(|hwnd| *hwnd == control) {
                    SetBkMode(hdc, windows::Win32::Graphics::Gdi::OPAQUE);
                    SetBkColor(hdc, rgb(26, 35, 39));
                    return LRESULT(state.input_brush.0 as isize);
                }
            }
            LRESULT(state.background_brush.0 as isize)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let control = HWND(lparam.0 as *mut c_void);
            unsafe {
                SetBkColor(hdc, rgb(26, 35, 39));
                SetTextColor(
                    hdc,
                    if advanced_audio::is_generation_status_control(state, control) {
                        rgb(221, 183, 105)
                    } else {
                        rgb(235, 242, 243)
                    },
                );
            }
            LRESULT(state.input_brush.0 as isize)
        }
        WM_CTLCOLORDLG => LRESULT(state.background_brush.0 as isize),
        WM_DPICHANGED => {
            rewrite::close_provider(state);
            audio::close_all(state);
            state.dpi = ((wparam.0 >> 16) & 0xffff) as u32;
            if let Some(frame) = &mut state.frame_overlay {
                frame.set_dpi(state.dpi);
            }
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOACTIVATE,
                );
            }
            resize_controls(state);
            advanced_audio::resize(state);
            if let Some(picker) = &mut state.microphone {
                picker.rebuild(state.language, state.dpi);
            }
            set_language_dropdown(state, state.language_open);
            audio::refresh(state);
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            let position = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            let resized = position.flags.0 & SWP_NOSIZE.0 == 0;
            let result = unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            if resized {
                apply_settings_region(hwnd, state.dpi);
            }
            sync_settings_frame(state, resized);
            result
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == SAVE_TIMER => {
            poll_save(state);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == CONNECTIVITY_TIMER => {
            poll_connectivity(state);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == advanced_audio::TIMER => {
            advanced_audio::poll_generation(state);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER if wparam.0 == debug::TIMER => {
            debug::refresh(&mut state.debug);
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATE => {
            if wparam.0 & 0xffff == 0 {
                hotkeys::deactivate();
            } else {
                hotkeys::activate(unsafe { GetFocus() });
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CLOSE => {
            if !state.saving {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            advanced_audio::destroy(state);
            state.connectivity_cancel.cancel();
            advanced_audio::cancel_generation(state);
            hotkeys::deactivate();
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(hwnd), debug::TIMER);
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
                    Some(hwnd),
                    MICROPHONE_TIMER,
                );
                let _ = DeleteObject(HGDIOBJ(state.background_brush.0));
                let _ = DeleteObject(HGDIOBJ(state.input_brush.0));
                let _ = DeleteObject(HGDIOBJ(state.font.0));
                let _ = DeleteObject(HGDIOBJ(state.title_font.0));
                let _ = DeleteObject(HGDIOBJ(state.small_font.0));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(pointer));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn apply_settings_region(hwnd: HWND, dpi: u32) {
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return;
    }
    let width = platform::unscale(client.right - client.left, dpi) as f32;
    let height = platform::unscale(client.bottom - client.top, dpi) as f32;
    let inset = 0.75;
    let points = continuous_rounded_rect_polygon(
        inset,
        inset,
        width - inset,
        height - inset,
        PANEL_CORNER_RADIUS as f32 - inset,
        12,
    )
    .into_iter()
    .map(|(x, y)| POINT {
        x: (x * dpi as f32 / 96.0).round() as i32,
        y: (y * dpi as f32 / 96.0).round() as i32,
    })
    .collect::<Vec<_>>();
    unsafe {
        let region = CreatePolygonRgn(&points, WINDING);
        if region.is_invalid() {
            return;
        }
        if SetWindowRgn(hwnd, Some(region), true) == 0 {
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
    }
}

fn sync_settings_frame(state: &mut SettingsState, repaint: bool) {
    if let Some(frame) = &mut state.frame_overlay {
        frame.sync(state.hwnd, repaint);
    }
}

fn create_controls(state: &mut SettingsState) -> Result<(), String> {
    let config = state.runtime.config();
    let values = serde_json::to_value(&config).map_err(|error| error.to_string())?;
    let object = values
        .as_object()
        .ok_or("config serialization was not an object")?;
    let instance = unsafe { GetModuleHandleW(None).map_err(|error| error.to_string())? };

    for (index, (group, key)) in GROUPS.iter().enumerate() {
        let button = create_child(
            state,
            w!("BUTTON"),
            if *key == "API" {
                "API"
            } else {
                state.language.text(key)
            },
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            12,
            84 + index as i32 * 36,
            146,
            32,
            ID_PAGE_BASE + index,
            instance,
        )?;
        state.page_buttons.push(button);
        state.localized_controls.push((button, key));
        let _ = group;
    }

    let close = create_child(
        state,
        w!("BUTTON"),
        "×",
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        706,
        17,
        38,
        38,
        ID_CLOSE,
        instance,
    )?;
    state.controls.insert("__close", close);

    let display_label = create_label(
        state,
        state.language.text("display_language"),
        CONTENT_LEFT,
        96,
        LABEL_WIDTH,
        FIELD_HEIGHT,
        instance,
    )?;
    state
        .control_groups
        .insert(display_label.0 as usize, "Display");
    state
        .localized_controls
        .push((display_label, "display_language"));
    state.language_control = create_child(
        state,
        w!("BUTTON"),
        state.language.native_name(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        EDIT_LEFT,
        96,
        EDIT_WIDTH,
        FIELD_HEIGHT,
        ID_LANGUAGE,
        instance,
    )?;
    state
        .control_groups
        .insert(state.language_control.0 as usize, "Display");

    let mut group_y: HashMap<&'static str, i32> = HashMap::new();
    group_y.insert("Display", 138);
    let microphone_label = create_label(
        state,
        state.language.text("INPUT_DEVICE"),
        CONTENT_LEFT,
        FIELD_TOP,
        LABEL_WIDTH,
        FIELD_HEIGHT,
        instance,
    )?;
    state
        .control_groups
        .insert(microphone_label.0 as usize, "Audio");
    state
        .localized_controls
        .push((microphone_label, "INPUT_DEVICE"));
    let picker = MicrophonePicker::create(state, instance)?;
    state
        .control_groups
        .insert(picker.button.0 as usize, "Audio");
    state.microphone = Some(picker);
    group_y.insert("Audio", FIELD_TOP + 42);
    let hotkey_status = create_child(
        state,
        w!("STATIC"),
        state.language.text("hotkey_help"),
        WS_CHILD | WS_VISIBLE,
        CONTENT_LEFT,
        FIELD_TOP + 7 * 42,
        EDIT_LEFT + EDIT_WIDTH - CONTENT_LEFT,
        72,
        0,
        instance,
    )?;
    state
        .control_groups
        .insert(hotkey_status.0 as usize, "Hotkeys");
    state
        .localized_controls
        .push((hotkey_status, "hotkey_help"));
    for (index, field) in FIELDS.iter().enumerate() {
        let y = *group_y.entry(field.group).or_insert(FIELD_TOP);
        let label = create_label(
            state,
            state.language.text(field.key),
            CONTENT_LEFT,
            y,
            LABEL_WIDTH,
            FIELD_HEIGHT,
            instance,
        )?;
        state.control_groups.insert(label.0 as usize, field.group);
        state.localized_controls.push((label, field.key));

        let value = object.get(field.key).cloned().unwrap_or_default();
        let control = if let Some(audio_field) = crate::audio_options::KEYS
            .iter()
            .position(|key| *key == field.key)
        {
            let picker =
                audio::AudioPicker::create(state, audio_field, ID_FIELD_BASE + index, y, instance)?;
            let button = picker.button;
            state.audio_pickers.push(picker);
            button
        } else {
            match field.kind {
                FieldKind::Boolean => {
                    let id = ID_FIELD_BASE + index;
                    let hwnd = create_child(
                        state,
                        w!("BUTTON"),
                        "",
                        WINDOW_STYLE(
                            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32,
                        ),
                        EDIT_LEFT,
                        y + 5,
                        24,
                        24,
                        id,
                        instance,
                    )?;
                    state
                        .boolean_values
                        .insert(field.key, value.as_bool().unwrap_or(false));
                    state.boolean_ids.insert(id, field.key);
                    hwnd
                }
                kind => {
                    let text = match value {
                        serde_json::Value::String(text) => text,
                        other => other.to_string(),
                    };
                    let multiline = matches!(kind, FieldKind::Multiline);
                    let frame_height = if multiline { 68 } else { FIELD_HEIGHT };
                    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP;
                    style |= WINDOW_STYLE(if multiline {
                        (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32 | WS_CLIPCHILDREN.0
                    } else {
                        ES_AUTOHSCROLL as u32
                    });
                    if matches!(kind, FieldKind::Password) {
                        style |= WINDOW_STYLE(ES_PASSWORD as u32);
                    }
                    let hwnd = create_child(
                        state,
                        w!("EDIT"),
                        &text,
                        style,
                        EDIT_LEFT + 3,
                        y + if multiline { 5 } else { 7 },
                        EDIT_WIDTH - 6,
                        if multiline { frame_height - 10 } else { 20 },
                        ID_FIELD_BASE + index,
                        instance,
                    )?;
                    apply_dark_theme(hwnd);
                    if hotkeys::KEYS.contains(&field.key) {
                        hotkeys::attach(hwnd, text, state.language, hotkey_status)?;
                    }
                    unsafe {
                        let margin = platform::scale(7, state.dpi) as u32;
                        SendMessageW(
                            hwnd,
                            EM_SETMARGINS,
                            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
                            Some(LPARAM((margin | (margin << 16)) as isize)),
                        );
                    }
                    if multiline {
                        dropdown_scrollbar::attach_edit(hwnd, state.dpi)?;
                    }
                    state.input_frames.push(InputFrame {
                        rect: RECT {
                            left: EDIT_LEFT,
                            top: y,
                            right: EDIT_LEFT + EDIT_WIDTH,
                            bottom: y + frame_height,
                        },
                        group: field.group,
                        control: hwnd,
                    });
                    hwnd
                }
            }
        };
        state.controls.insert(field.key, control);
        state.control_groups.insert(control.0 as usize, field.group);
        group_y.insert(
            field.group,
            y + if matches!(field.kind, FieldKind::Multiline) {
                80
            } else {
                42
            },
        );
    }

    advanced_audio::create(state, instance)?;
    rewrite::create(state, instance)?;
    debug::create(state, instance)?;
    audio::refresh(state);
    let vad_hint = create_label(
        state,
        state.language.text("vad_hint"),
        CONTENT_LEFT,
        group_y.get("Audio").copied().unwrap_or(FIELD_TOP),
        510,
        56,
        instance,
    )?;
    state.control_groups.insert(vad_hint.0 as usize, "Audio");
    state.localized_controls.push((vad_hint, "vad_hint"));

    let api_test_top = group_y.get("API").copied().unwrap_or(FIELD_TOP);
    let test_connectivity = create_child(
        state,
        w!("BUTTON"),
        state.language.text("test_connectivity"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        EDIT_LEFT,
        api_test_top,
        142,
        FIELD_HEIGHT,
        ID_TEST_CONNECTIVITY,
        instance,
    )?;
    state
        .controls
        .insert("__test_connectivity", test_connectivity);
    state
        .control_groups
        .insert(test_connectivity.0 as usize, "API");
    state
        .localized_controls
        .push((test_connectivity, "test_connectivity"));

    let cancel = create_child(
        state,
        w!("BUTTON"),
        state.language.text("cancel"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        552,
        WINDOW_HEIGHT - 46,
        88,
        34,
        ID_CANCEL,
        instance,
    )?;
    let save = create_child(
        state,
        w!("BUTTON"),
        state.language.text("save"),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
        650,
        WINDOW_HEIGHT - 46,
        94,
        34,
        ID_SAVE,
        instance,
    )?;
    state.controls.insert("__cancel", cancel);
    state.controls.insert("__save", save);
    dropdown::track_hover(cancel, false)?;
    dropdown::track_hover(save, false)?;
    state.localized_controls.push((cancel, "cancel"));
    state.localized_controls.push((save, "save"));

    state.language_panel = dropdown::create_panel(state, instance)?;
    for (index, language) in Language::ALL.iter().enumerate() {
        let item = create_child(
            state,
            w!("BUTTON"),
            language.native_name(),
            WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            EDIT_LEFT,
            136 + index as i32 * 36,
            EDIT_WIDTH,
            32,
            ID_LANGUAGE_ITEM_BASE + index,
            instance,
        )?;
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetParent(item, Some(state.language_panel))
                .map_err(|error| error.to_string())?;
            let _ = ShowWindow(item, SW_HIDE);
        }
        dropdown::track_hover(item, false)?;
        state.language_items.push(item);
    }

    update_page_visibility(state);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_child(
    state: &SettingsState,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: usize,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let text = wide(text);
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR(text.as_ptr()),
            // The display-language menu deliberately overlaps controls below it.
            // Without sibling clipping, a lower-z native EDIT control may repaint
            // over an open menu item.
            style | WS_CLIPSIBLINGS,
            platform::scale(x, state.dpi),
            platform::scale(y, state.dpi),
            platform::scale(width, state.dpi),
            platform::scale(height, state.dpi),
            Some(state.hwnd),
            (id != 0).then_some(HMENU(id as *mut c_void)),
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| error.to_string())?;
    set_font(hwnd, state.font);
    state.layouts.borrow_mut().push((
        hwnd,
        RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        },
    ));
    Ok(hwnd)
}

fn resize_controls(state: &mut SettingsState) {
    use windows::Win32::UI::WindowsAndMessaging::GetParent;
    let old = [state.font, state.title_font, state.small_font];
    state.font = create_font(state.dpi, 14, false);
    state.title_font = create_font(state.dpi, 20, true);
    state.small_font = create_font(state.dpi, 11, false);
    for (hwnd, logical) in state.layouts.borrow().iter() {
        unsafe {
            if GetParent(*hwnd).ok() == Some(state.hwnd) {
                let rect = scaled_rect(*logical, state.dpi);
                let _ = SetWindowPos(
                    *hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
                );
            }
            set_font(*hwnd, state.font);
        }
    }
    rewrite::resize_list(state);
    debug::resize(&mut state.debug, state.dpi);
    unsafe {
        for font in old {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
}

fn create_label(
    state: &SettingsState,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    create_child(
        state,
        w!("STATIC"),
        text,
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | SS_CENTERIMAGE_STYLE),
        x,
        y,
        width,
        height,
        0,
        instance,
    )
}

fn apply_dark_theme(hwnd: HWND) {
    unsafe {
        let _ = SetWindowTheme(hwnd, w!("DarkMode_Explorer"), w!(""));
    }
}

fn paint_window(state: &SettingsState) {
    let mut paint = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(state.hwnd, &mut paint) };
    let scale = |value| platform::scale(value, state.dpi);
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(state.hwnd, &mut client);
        FillRect(hdc, &client, state.background_brush);
        fill_color(
            hdc,
            RECT {
                left: 0,
                top: scale(HEADER_HEIGHT),
                right: scale(SIDEBAR_WIDTH),
                bottom: client.bottom - scale(FOOTER_HEIGHT),
            },
            rgb(12, 18, 21),
        );
        fill_color(
            hdc,
            RECT {
                left: 0,
                top: client.bottom - scale(FOOTER_HEIGHT),
                right: client.right,
                bottom: client.bottom,
            },
            rgb(13, 19, 22),
        );
        line(
            hdc,
            0,
            scale(HEADER_HEIGHT),
            client.right,
            scale(HEADER_HEIGHT),
            rgb(38, 49, 54),
        );
        line(
            hdc,
            scale(SIDEBAR_WIDTH),
            scale(HEADER_HEIGHT),
            scale(SIDEBAR_WIDTH),
            client.bottom - scale(FOOTER_HEIGHT),
            rgb(38, 49, 54),
        );
        line(
            hdc,
            0,
            client.bottom - scale(FOOTER_HEIGHT),
            client.right,
            client.bottom - scale(FOOTER_HEIGHT),
            rgb(38, 49, 54),
        );

        let old = SelectObject(hdc, HGDIOBJ(state.title_font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(239, 246, 247));
        let mut title = wide(state.language.text("settings"));
        let title_len = title.len() - 1;
        let mut title_rect = RECT {
            left: scale(18),
            top: scale(14),
            right: scale(690),
            bottom: scale(42),
        };
        DrawTextW(
            hdc,
            &mut title[..title_len],
            &mut title_rect,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
        );
        SelectObject(hdc, HGDIOBJ(state.font.0));
        SetTextColor(hdc, rgb(126, 146, 151));
        let mut path = wide(&state.config_path.to_string_lossy());
        let path_len = path.len() - 1;
        let mut path_rect = RECT {
            left: scale(18),
            top: scale(40),
            right: scale(690),
            bottom: scale(65),
        };
        DrawTextW(
            hdc,
            &mut path[..path_len],
            &mut path_rect,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );

        let active = GROUPS
            .get(state.active_group)
            .map(|group| group.0)
            .unwrap_or("Display");
        let focused = GetFocus();
        for frame in state.input_frames.iter().filter(|frame| {
            frame.group == active
                && IsWindowVisible(frame.control).as_bool()
                && windows::Win32::UI::WindowsAndMessaging::GetParent(frame.control).ok()
                    == Some(state.hwnd)
        }) {
            rounded_box(
                hdc,
                scaled_rect(frame.rect, state.dpi),
                rgb(26, 35, 39),
                if focused == frame.control {
                    rgb(92, 192, 176)
                } else {
                    rgb(54, 68, 74)
                },
                scale(7),
            );
        }
        if active == "About" {
            paint_about(state, hdc);
        }
        if active == "API" || active == "Rewrite" {
            paint_connectivity_status(state, hdc);
        }
        if active == "API" {
            advanced_audio::paint_legacy_notice(state, hdc);
        } else if active == advanced_audio::GROUP {
            advanced_audio::paint(state, hdc);
        }
        SelectObject(hdc, old);
        let _ = EndPaint(state.hwnd, &paint);
    }
}

fn draw_owner_button(state: &SettingsState, item: &DRAWITEMSTRUCT) {
    let id = item.CtlID as usize;
    if id == ID_SAVE || id == ID_CANCEL {
        unsafe { draw_dialog_button(item, state.font, state.dpi) };
        return;
    }
    if let Some(key) = state.boolean_ids.get(&id).copied() {
        unsafe {
            draw_checkbox(
                state,
                item,
                state.boolean_values.get(key).copied().unwrap_or(false),
            );
        }
        return;
    }

    let selected_page = id >= ID_PAGE_BASE && id == ID_PAGE_BASE + state.active_group;
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let hot = item.itemState.0 & ODS_HOTLIGHT.0 != 0 || dropdown::hovered(item.hwndItem);
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    let scale = |value| platform::scale(value, state.dpi);

    unsafe {
        let base = if id >= ID_PAGE_BASE && id < ID_PAGE_BASE + GROUPS.len() {
            rgb(12, 18, 21)
        } else {
            rgb(16, 22, 25)
        };
        fill_color(item.hDC, item.rcItem, base);
        if id == ID_CLOSE {
            rounded_box(
                item.hDC,
                item.rcItem,
                if pressed {
                    rgb(40, 51, 56)
                } else {
                    rgb(27, 36, 40)
                },
                if hot || focused {
                    rgb(82, 111, 111)
                } else {
                    rgb(52, 66, 72)
                },
                scale(8),
            );
            let cx = (item.rcItem.left + item.rcItem.right) / 2;
            let cy = (item.rcItem.top + item.rcItem.bottom) / 2;
            stroke_polyline(
                item.hDC,
                &[
                    POINT {
                        x: cx - scale(5),
                        y: cy - scale(5),
                    },
                    POINT {
                        x: cx + scale(5),
                        y: cy + scale(5),
                    },
                ],
                rgb(159, 218, 209),
                scale(2).max(1),
            );
            stroke_polyline(
                item.hDC,
                &[
                    POINT {
                        x: cx + scale(5),
                        y: cy - scale(5),
                    },
                    POINT {
                        x: cx - scale(5),
                        y: cy + scale(5),
                    },
                ],
                rgb(159, 218, 209),
                scale(2).max(1),
            );
            return;
        }

        if id == ID_LANGUAGE
            || id == rewrite::ID_PROVIDER
            || id == ID_MICROPHONE
            || advanced_audio::is_dynamic_select_button(id)
            || state
                .audio_pickers
                .iter()
                .any(|p| p.button == item.hwndItem)
        {
            let open = if id == ID_LANGUAGE {
                state.language_open
            } else if id == rewrite::ID_PROVIDER {
                state.rewrite.provider_open
            } else if id == ID_MICROPHONE {
                state.microphone.as_ref().is_some_and(|p| p.open)
            } else if advanced_audio::is_dynamic_select_button(id) {
                advanced_audio::dynamic_select_is_open_for_draw(state, id)
            } else {
                state
                    .audio_pickers
                    .iter()
                    .any(|p| p.button == item.hwndItem && p.open)
            };
            rounded_box(
                item.hDC,
                item.rcItem,
                if pressed {
                    rgb(31, 43, 47)
                } else {
                    rgb(26, 35, 39)
                },
                if open || focused || hot {
                    rgb(82, 171, 158)
                } else {
                    rgb(54, 68, 74)
                },
                scale(7),
            );
            draw_button_text(
                state,
                item,
                if item.itemState.0 & ODS_DISABLED.0 != 0 {
                    rgb(110, 126, 130)
                } else {
                    rgb(234, 242, 243)
                },
                DT_LEFT,
                scale(14),
                scale(42),
            );
            let cx = item.rcItem.right - scale(17);
            let cy = (item.rcItem.top + item.rcItem.bottom) / 2;
            let direction = if open { -1 } else { 1 };
            stroke_polyline(
                item.hDC,
                &[
                    POINT {
                        x: cx - scale(4),
                        y: cy - direction * scale(2),
                    },
                    POINT {
                        x: cx,
                        y: cy + direction * scale(2),
                    },
                    POINT {
                        x: cx + scale(4),
                        y: cy - direction * scale(2),
                    },
                ],
                rgb(185, 207, 207),
                scale(1).max(1),
            );
            return;
        }

        if id >= ID_LANGUAGE_ITEM_BASE && id < ID_LANGUAGE_ITEM_BASE + Language::ALL.len() {
            let index = id - ID_LANGUAGE_ITEM_BASE;
            let selected = Language::ALL.get(index).copied() == Some(state.language);
            dropdown::draw_row(
                item.hDC,
                item.rcItem,
                selected,
                pressed || hot || focused || dropdown::hovered(item.hwndItem),
                state.dpi,
            );
            draw_button_text(
                state,
                item,
                if selected {
                    rgb(231, 250, 246)
                } else {
                    rgb(190, 205, 208)
                },
                DT_LEFT,
                scale(14),
                scale(14),
            );
            return;
        }

        let (fill, border, text_color, radius) = if id == ID_TEST_CONNECTIVITY
            || (rewrite::ID_ADD..=rewrite::ID_TEST).contains(&id)
            || debug::is_button(id)
            || advanced_audio::is_button(id)
        {
            (
                if disabled {
                    rgb(29, 39, 43)
                } else if pressed {
                    rgb(33, 61, 61)
                } else {
                    rgb(16, 22, 25)
                },
                if disabled {
                    rgb(48, 61, 66)
                } else if focused || hot {
                    rgb(82, 192, 176)
                } else {
                    rgb(56, 84, 86)
                },
                if disabled {
                    rgb(107, 123, 126)
                } else {
                    rgb(226, 239, 239)
                },
                7,
            )
        } else if selected_page {
            (rgb(27, 51, 52), rgb(55, 115, 106), rgb(237, 248, 246), 7)
        } else {
            (
                if pressed || hot {
                    rgb(22, 31, 35)
                } else {
                    rgb(12, 18, 21)
                },
                rgb(12, 18, 21),
                if disabled {
                    rgb(83, 96, 100)
                } else {
                    rgb(166, 183, 187)
                },
                7,
            )
        };
        rounded_box(item.hDC, item.rcItem, fill, border, scale(radius));
        if selected_page {
            fill_color(
                item.hDC,
                RECT {
                    left: item.rcItem.left,
                    top: item.rcItem.top + scale(7),
                    right: item.rcItem.left + scale(3),
                    bottom: item.rcItem.bottom - scale(7),
                },
                rgb(112, 215, 195),
            );
        }
        SetBkMode(item.hDC, TRANSPARENT);
        draw_button_text(
            state,
            item,
            text_color,
            if id >= ID_PAGE_BASE && id < ID_PAGE_BASE + GROUPS.len() {
                DT_LEFT
            } else {
                DT_CENTER
            },
            if id >= ID_PAGE_BASE && id < ID_PAGE_BASE + GROUPS.len() {
                scale(11)
            } else {
                0
            },
            0,
        );
    }
}

unsafe fn draw_dialog_button(item: &DRAWITEMSTRUCT, font: HFONT, dpi: u32) {
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let hot = item.itemState.0 & ODS_HOTLIGHT.0 != 0 || dropdown::hovered(item.hwndItem);
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    let (fill, border, text_color) = if item.CtlID as usize == ID_SAVE {
        (
            if disabled {
                rgb(54, 86, 81)
            } else if pressed {
                rgb(80, 187, 169)
            } else {
                rgb(112, 215, 195)
            },
            if disabled {
                rgb(54, 86, 81)
            } else {
                rgb(112, 215, 195)
            },
            if disabled {
                rgb(118, 145, 140)
            } else {
                rgb(7, 28, 24)
            },
        )
    } else {
        (
            if pressed {
                rgb(36, 47, 52)
            } else {
                rgb(26, 35, 39)
            },
            if focused || hot {
                rgb(82, 102, 109)
            } else {
                rgb(56, 70, 76)
            },
            if disabled {
                rgb(92, 105, 109)
            } else {
                rgb(215, 226, 228)
            },
        )
    };
    unsafe {
        fill_color(item.hDC, item.rcItem, rgb(13, 19, 22));
        rounded_box(item.hDC, item.rcItem, fill, border, platform::scale(7, dpi));
        draw_text_line(
            item.hDC,
            font,
            &read_text(item.hwndItem),
            text_color,
            item.rcItem,
            DT_CENTER,
        );
    }
}

unsafe fn draw_checkbox(state: &SettingsState, item: &DRAWITEMSTRUCT, checked: bool) {
    let scale = |value| platform::scale(value, state.dpi);
    let side = scale(20);
    let left = item.rcItem.left + (item.rcItem.right - item.rcItem.left - side) / 2;
    let top = item.rcItem.top + (item.rcItem.bottom - item.rcItem.top - side) / 2;
    let rect = RECT {
        left,
        top,
        right: left + side,
        bottom: top + side,
    };
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    unsafe {
        fill_color(item.hDC, item.rcItem, rgb(16, 22, 25));
        rounded_box(
            item.hDC,
            rect,
            if disabled {
                rgb(29, 39, 43)
            } else if checked {
                if pressed {
                    rgb(82, 188, 170)
                } else {
                    rgb(112, 215, 195)
                }
            } else if pressed {
                rgb(35, 47, 52)
            } else {
                rgb(24, 33, 37)
            },
            if disabled {
                rgb(48, 61, 66)
            } else if checked || focused {
                rgb(112, 215, 195)
            } else {
                rgb(67, 82, 88)
            },
            scale(5),
        );
        if checked {
            let cx = (rect.left + rect.right) / 2;
            let cy = (rect.top + rect.bottom) / 2;
            stroke_polyline(
                item.hDC,
                &[
                    POINT {
                        x: cx - scale(5),
                        y: cy,
                    },
                    POINT {
                        x: cx - scale(1),
                        y: cy + scale(4),
                    },
                    POINT {
                        x: cx + scale(6),
                        y: cy - scale(5),
                    },
                ],
                if disabled {
                    rgb(88, 108, 105)
                } else {
                    rgb(7, 31, 26)
                },
                scale(2).max(1),
            );
        }
    }
}

unsafe fn draw_button_text(
    state: &SettingsState,
    item: &DRAWITEMSTRUCT,
    color: COLORREF,
    alignment: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
    left_padding: i32,
    right_padding: i32,
) {
    let text = read_text(item.hwndItem);
    // The button chrome is already painted by the caller. Avoid sending an
    // empty Rust slice through the Win32 text-drawing FFI for a blank caption.
    if text.is_empty() {
        return;
    }
    let mut text = text.encode_utf16().collect::<Vec<_>>();
    let mut rect = item.rcItem;
    rect.left += left_padding;
    rect.right -= right_padding;
    unsafe {
        let old = SelectObject(item.hDC, HGDIOBJ(state.font.0));
        SetBkMode(item.hDC, TRANSPARENT);
        SetTextColor(item.hDC, color);
        DrawTextW(
            item.hDC,
            &mut text,
            &mut rect,
            alignment | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        SelectObject(item.hDC, old);
    }
}

unsafe fn paint_about(state: &SettingsState, hdc: HDC) {
    let s = |value| platform::scale(value, state.dpi);
    unsafe {
        if let Ok(icon) = resources::load_app_icon_sized(s(70), s(70)) {
            let _ = DrawIconEx(hdc, s(196), s(96), icon, s(70), s(70), 0, None, DI_NORMAL);
            let _ = DestroyIcon(icon);
        }
        draw_text_line(
            hdc,
            state.title_font,
            "Dictate for Windows",
            rgb(239, 246, 247),
            RECT {
                left: s(286),
                top: s(96),
                right: s(732),
                bottom: s(126),
            },
            DT_LEFT,
        );
        draw_text_line(
            hdc,
            state.font,
            concat!("Version ", env!("CARGO_PKG_VERSION")),
            rgb(112, 215, 195),
            RECT {
                left: s(286),
                top: s(129),
                right: s(732),
                bottom: s(151),
            },
            DT_LEFT,
        );
        draw_text_line(
            hdc,
            state.small_font,
            "Speech transcription and text rewriting for Windows",
            rgb(132, 151, 156),
            RECT {
                left: s(286),
                top: s(151),
                right: s(732),
                bottom: s(171),
            },
            DT_LEFT,
        );

        let card = RECT {
            left: s(196),
            top: s(194),
            right: s(732),
            bottom: s(400),
        };
        rounded_box(hdc, card, rgb(20, 28, 32), rgb(45, 58, 64), s(10));
        let rows = [
            ("Author", "Joey Kot", false),
            ("Email", "joey.kot.x@gmail.com", false),
            ("License", "GPL-3.0-or-later", false),
            (
                "Repository",
                "github.com/Joey-Kot/Dictate-for-Windows",
                true,
            ),
        ];
        for (index, (label, value, accent)) in rows.iter().enumerate() {
            let top = 206 + index as i32 * 46;
            draw_text_line(
                hdc,
                state.small_font,
                label,
                rgb(116, 137, 142),
                RECT {
                    left: s(216),
                    top: s(top),
                    right: s(310),
                    bottom: s(top + 30),
                },
                DT_LEFT,
            );
            draw_text_line(
                hdc,
                state.font,
                value,
                if *accent {
                    rgb(112, 215, 195)
                } else {
                    rgb(217, 228, 230)
                },
                RECT {
                    left: s(316),
                    top: s(top),
                    right: s(710),
                    bottom: s(top + 30),
                },
                DT_LEFT,
            );
            if index + 1 < rows.len() {
                line(
                    hdc,
                    s(216),
                    s(top + 38),
                    s(712),
                    s(top + 38),
                    rgb(39, 50, 55),
                );
            }
        }
    }
}

unsafe fn paint_connectivity_status(state: &SettingsState, hdc: HDC) {
    let s = |value| platform::scale(value, state.dpi);
    let active = GROUPS
        .get(state.active_group)
        .map(|group| group.0)
        .unwrap_or("Display");
    if (active == "Rewrite") != state.rewrite_test {
        return;
    }
    let left = if state.rewrite_test {
        CONTENT_LEFT
    } else {
        EDIT_LEFT
    };
    match &state.connectivity_status {
        ConnectivityStatus::Idle => {}
        ConnectivityStatus::Testing => unsafe {
            draw_text_line(
                hdc,
                state.font,
                state.language.text("testing_connectivity"),
                rgb(230, 240, 240),
                RECT {
                    left: s(left),
                    top: s(507),
                    right: s(EDIT_LEFT + EDIT_WIDTH),
                    bottom: s(541),
                },
                DT_LEFT,
            );
        },
        ConnectivityStatus::Succeeded => unsafe {
            let icon = RECT {
                left: s(left),
                top: s(514),
                right: s(left + 20),
                bottom: s(534),
            };
            rounded_box(hdc, icon, rgb(104, 201, 88), rgb(104, 201, 88), s(10));
            let center_x = (icon.left + icon.right) / 2;
            let center_y = (icon.top + icon.bottom) / 2;
            stroke_polyline(
                hdc,
                &[
                    POINT {
                        x: center_x - s(5),
                        y: center_y,
                    },
                    POINT {
                        x: center_x - s(1),
                        y: center_y + s(4),
                    },
                    POINT {
                        x: center_x + s(6),
                        y: center_y - s(5),
                    },
                ],
                rgb(8, 49, 35),
                s(2).max(1),
            );
            draw_text_line(
                hdc,
                state.font,
                &format!(
                    "{} ({} ms)",
                    state.language.text("connectivity_success"),
                    state.connectivity_elapsed
                ),
                rgb(108, 207, 94),
                RECT {
                    left: s(left + 30),
                    top: s(507),
                    right: s(EDIT_LEFT + EDIT_WIDTH),
                    bottom: s(541),
                },
                DT_LEFT,
            );
        },
        ConnectivityStatus::Failed(error) => unsafe {
            let card = RECT {
                left: s(left),
                top: s(506),
                right: s(EDIT_LEFT + EDIT_WIDTH),
                bottom: s(554),
            };
            rounded_box(hdc, card, rgb(47, 27, 30), rgb(226, 67, 74), s(7));
            let icon = RECT {
                left: s(left + 11),
                top: s(519),
                right: s(left + 31),
                bottom: s(539),
            };
            rounded_box(hdc, icon, rgb(239, 86, 91), rgb(239, 86, 91), s(4));
            draw_text_line(hdc, state.title_font, "!", rgb(41, 22, 24), icon, DT_CENTER);
            let message = format!("{}{}", state.language.text("connectivity_failed"), error);
            draw_text_wrapped(
                hdc,
                state.small_font,
                &message,
                rgb(255, 112, 116),
                RECT {
                    left: s(left + 42),
                    top: s(512),
                    right: s(EDIT_LEFT + EDIT_WIDTH - 10),
                    bottom: s(548),
                },
            );
        },
    }
}

fn refresh_language(state: &SettingsState) {
    unsafe {
        let _ = SetWindowTextW(
            state.hwnd,
            PCWSTR(wide(&format!("Dictate - {}", state.language.text("settings"))).as_ptr()),
        );
    }
    rewrite::refresh_labels(state);
    advanced_audio::sync_visibility(state);
    for (hwnd, key) in &state.localized_controls {
        let text = if *key == "API" {
            "API"
        } else {
            state.language.text(key)
        };
        unsafe {
            let _ = SetWindowTextW(*hwnd, PCWSTR(wide(text).as_ptr()));
            let _ = InvalidateRect(Some(*hwnd), None, true);
        }
    }
    unsafe {
        let _ = SetWindowTextW(
            state.language_control,
            PCWSTR(wide(state.language.native_name()).as_ptr()),
        );
        let _ = InvalidateRect(Some(state.language_control), None, true);
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
}

fn update_page_visibility(state: &SettingsState) {
    update_input_controls(state);
    let active = GROUPS
        .get(state.active_group)
        .map(|group| group.0)
        .unwrap_or("Display");
    unsafe {
        for (handle, group) in &state.control_groups {
            let hwnd = HWND(*handle as *mut c_void);
            let _ = ShowWindow(hwnd, if *group == active { SW_SHOW } else { SW_HIDE });
        }
    }
    advanced_audio::sync_visibility(state);
    if active != "Display" || !state.language_open {
        unsafe {
            let _ = ShowWindow(state.language_panel, SW_HIDE);
        }
        for item in &state.language_items {
            unsafe {
                let _ = ShowWindow(*item, SW_HIDE);
            }
        }
    }
}

fn set_language_dropdown(state: &mut SettingsState, open: bool) {
    state.language_open = open && state.active_group == 0;
    let width = dropdown::position(
        state.language_panel,
        state.language_control,
        Language::ALL.len() as i32 * dropdown::ROW_HEIGHT,
        state.dpi,
    );
    unsafe {
        let _ = ShowWindow(
            state.language_panel,
            if state.language_open {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
    }
    for (index, item) in state.language_items.iter().enumerate() {
        unsafe {
            let _ = ShowWindow(
                *item,
                if state.language_open {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
            if state.language_open {
                let _ = SetWindowPos(
                    *item,
                    Some(HWND_TOP),
                    platform::scale(dropdown::PADDING, state.dpi),
                    platform::scale(
                        dropdown::PADDING + index as i32 * dropdown::ROW_HEIGHT,
                        state.dpi,
                    ),
                    width,
                    platform::scale(dropdown::ROW_HEIGHT, state.dpi),
                    SWP_NOACTIVATE,
                );
                let _ = InvalidateRect(Some(*item), None, true);
            }
        }
    }
    unsafe {
        let _ = InvalidateRect(Some(state.language_control), None, true);
    }
}

fn select_language(state: &mut SettingsState, index: usize) {
    let Some(language) = Language::ALL.get(index).copied() else {
        return;
    };
    state.language = language;
    language.save();
    set_language_dropdown(state, false);
    refresh_language(state);
    for key in hotkeys::KEYS {
        if let Some(hwnd) = state.controls.get(key) {
            hotkeys::set_language(*hwnd, language);
        }
    }
    audio::refresh(state);
    if let Some(picker) = &mut state.microphone {
        picker.rebuild(state.language, state.dpi);
    }
    for item in &state.language_items {
        unsafe {
            let _ = InvalidateRect(Some(*item), None, true);
        }
    }
    unsafe {
        let _ = PostMessageW(
            Some(state.owner),
            WM_LANGUAGE_CHANGED,
            WPARAM(index),
            LPARAM(0),
        );
    }
}

fn test_connectivity(state: &mut SettingsState) {
    start_connectivity(state, false);
}

fn start_connectivity(state: &mut SettingsState, rewrite: bool) {
    if matches!(state.connectivity_status, ConnectivityStatus::Testing) {
        return;
    }
    state.rewrite_test = rewrite;
    state.connectivity_elapsed = 0;
    let mut config = match read_config(state).and_then(|config| {
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }) {
        Ok(config) => config,
        Err(error) => {
            state.connectivity_status = ConnectivityStatus::Failed(error);
            unsafe {
                let _ = InvalidateRect(Some(state.hwnd), None, true);
            }
            return;
        }
    };

    // API/network fields are tested as drafts; debug switches apply only on Save.
    let applied = state.runtime.config();
    config.ffmpeg_debug = applied.ffmpeg_debug;
    config.record_debug = applied.record_debug;
    config.hotkey_debug = applied.hotkey_debug;
    config.upload_debug = applied.upload_debug;

    state.connectivity_status = ConnectivityStatus::Testing;
    state.rewrite_test = rewrite;
    state.connectivity_cancel = tokio_util::sync::CancellationToken::new();
    for key in ["__test_connectivity", "__rewrite_test"] {
        if let Some(button) = state.controls.get(key) {
            unsafe {
                let _ = EnableWindow(*button, false);
            }
        }
    }
    advanced_audio::update_controls(state);
    unsafe {
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    state.connectivity_receiver = Some(rx);
    let runtime = state.runtime.clone();
    let external = state.connectivity_cancel.clone();
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetTimer(
            Some(state.hwnd),
            CONNECTIVITY_TIMER,
            50,
            None,
        );
    }
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = (|| -> Result<(), String> {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            executor.block_on(
                runtime.test_connection(rewrite, external, |cancel| async move {
                    if rewrite {
                        let client = dictate_core::rewrite::RewriteClient::new(config)
                            .map_err(|e| e.to_string())?;
                        client
                            .test_connection(&cancel)
                            .await
                            .map_err(|e| e.to_string())
                    } else {
                        run_connectivity_test(config, cancel).await
                    }
                }),
            )
        })();
        let _ = tx.send((result, started.elapsed().as_millis()));
    });
}

fn poll_connectivity(state: &mut SettingsState) {
    let Some(result) = state
        .connectivity_receiver
        .as_ref()
        .and_then(|rx| rx.try_recv().ok())
    else {
        return;
    };
    state.connectivity_receiver = None;
    state.connectivity_elapsed = result.1;
    state.connectivity_status = match result.0 {
        Ok(()) => ConnectivityStatus::Succeeded,
        Err(e) => ConnectivityStatus::Failed(e),
    };
    for key in ["__test_connectivity", "__rewrite_test"] {
        if let Some(button) = state.controls.get(key) {
            unsafe {
                let _ = EnableWindow(*button, !state.saving);
            }
        }
    }
    advanced_audio::update_controls(state);
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(
            Some(state.hwnd),
            CONNECTIVITY_TIMER,
        );
        let _ = InvalidateRect(Some(state.hwnd), None, true);
    }
}

async fn run_connectivity_test(
    config: Config,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    if !config.advanced_audio_api.enabled && config.api_endpoint.is_empty() {
        return Err("API endpoint is empty".into());
    }
    let input = connectivity_test_source_path();
    let result = async {
        write_connectivity_test_wav(&input)?;
        dictate_core::runtime::test_audio_api_with_source(
            config,
            &platform::GuiLibAvConverter,
            &input,
            CONNECTIVITY_TEST_SAMPLE_RATE,
            &std::env::temp_dir(),
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())
    }
    .await;
    let _ = fs::remove_file(&input);
    result
}

fn connectivity_test_source_path() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let stem = format!("dictate-connectivity-{}-{timestamp}", std::process::id());
    let directory = std::env::temp_dir();
    directory.join(format!("{stem}-source.wav"))
}

fn write_connectivity_test_wav(path: &Path) -> Result<(), String> {
    let encoded: String = CONNECTIVITY_TEST_PCM
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let pcm = STANDARD
        .decode(encoded)
        .map_err(|error| format!("failed to decode embedded connectivity test audio: {error}"))?;
    let data_size = u32::try_from(pcm.len())
        .map_err(|_| "embedded connectivity test audio is too large".to_string())?;
    let byte_rate = CONNECTIVITY_TEST_SAMPLE_RATE as u32 * 2;
    let mut wav = Vec::with_capacity(pcm.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&(CONNECTIVITY_TEST_SAMPLE_RATE as u32).to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(&pcm);
    fs::write(path, wav)
        .map_err(|error| format!("failed to create connectivity test audio: {error}"))
}

fn poll_save(state: &mut SettingsState) {
    let Some(result) = state
        .save_receiver
        .as_ref()
        .and_then(|rx| rx.try_recv().ok())
    else {
        return;
    };
    state.save_receiver = None;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(state.hwnd), SAVE_TIMER);
    }
    state.saving = false;
    enable_controls(state, true);
    let config = state.runtime.config();
    unsafe {
        let alpha = platform::window_opacity_alpha(config.opacity);
        let _ = PostMessageW(
            Some(state.owner),
            WM_OPACITY_CHANGED,
            WPARAM(alpha as usize),
            LPARAM(0),
        );
        let _ = PostMessageW(
            Some(state.owner),
            WM_WINDOW_SCALE_CHANGED,
            WPARAM(0),
            LPARAM(0),
        );
    }
    match result {
        Ok(_) => unsafe {
            let _ = DestroyWindow(state.hwnd);
        },
        Err(error) => show_error(state.hwnd, &error),
    }
}

fn save(state: &mut SettingsState) {
    if state.saving {
        return;
    }
    if !state.runtime.can_reload() {
        show_error(
            state.hwnd,
            "Settings can only be saved while idle or in Error state.",
        );
        return;
    }
    let config = match read_config(state) {
        Ok(config) => config,
        Err(error) => {
            show_error(state.hwnd, &error);
            return;
        }
    };
    if let Err(dictate_core::hotkey::HotkeyError::DuplicateBinding {
        name,
        previous_name,
        ..
    }) = dictate_core::hotkey::validate_bindings(
        &config.start_key,
        &config.pause_key,
        &config.cancel_or_retry_key,
    ) {
        show_error(
            state.hwnd,
            &format!(
                "{}: {} / {}",
                state.language.text("hotkey_duplicate"),
                state.language.text(name),
                state.language.text(previous_name)
            ),
        );
        return;
    }
    if let Err(error) = config.validate() {
        show_error(state.hwnd, &error.to_string());
        return;
    }
    state.saving = true;
    rewrite::close_provider(state);
    audio::close_all(state);
    if let Some(picker) = &mut state.microphone {
        picker.set_open(false, state.language, state.dpi);
    }
    enable_controls(state, false);
    let runtime = state.runtime.clone();
    let config_path = state.config_path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    state.save_receiver = Some(rx);
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(state.hwnd), SAVE_TIMER, 50, None);
    }
    std::thread::spawn(move || {
        let result = (|| -> Result<Event, String> {
            let async_runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            async_runtime
                .block_on(runtime.save_and_reload(config, &config_path))
                .map_err(|error| error.to_string())?;
            Ok(runtime.snapshot())
        })();
        let _ = tx.send(result);
    });
}

fn read_config(state: &SettingsState) -> Result<Config, String> {
    let mut value =
        serde_json::to_value(state.runtime.config()).map_err(|error| error.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or("config serialization was not an object")?;
    if let Some(picker) = &state.microphone {
        object.insert(
            "INPUT_DEVICE".into(),
            serde_json::Value::String(picker.selected_id.clone()),
        );
        object.insert(
            "INPUT_DEVICE_NAME".into(),
            serde_json::Value::String(picker.selected_name.clone()),
        );
    }
    let audio = audio::draft(state);
    for field in FIELDS {
        let hwnd = state.controls[field.key];
        if let Some(text) = hotkeys::value(hwnd) {
            object.insert(field.key.into(), serde_json::Value::String(text));
            continue;
        }
        if let Some(index) = crate::audio_options::KEYS
            .iter()
            .position(|key| *key == field.key)
        {
            let text = &audio.values[index];
            let value = if index < 4 {
                serde_json::Value::Number(
                    text.parse::<i64>()
                        .map_err(|_| format!("{} must be an integer", field.label))?
                        .into(),
                )
            } else {
                serde_json::Value::String(text.clone())
            };
            object.insert(field.key.into(), value);
            continue;
        }
        let value = match field.kind {
            FieldKind::Boolean => serde_json::Value::Bool(
                state
                    .boolean_values
                    .get(field.key)
                    .copied()
                    .unwrap_or(false),
            ),
            FieldKind::Integer => serde_json::Value::Number(
                read_text(hwnd)
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("{} must be an integer", field.label))?
                    .into(),
            ),
            FieldKind::Float => serde_json::Number::from_f64(
                read_text(hwnd)
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("{} must be a number", field.label))?,
            )
            .map(serde_json::Value::Number)
            .ok_or_else(|| format!("{} is not a finite number", field.label))?,
            _ => serde_json::Value::String(read_text(hwnd)),
        };
        object.insert(field.key.into(), value);
    }
    let mut config: Config = serde_json::from_value(value).map_err(|error| error.to_string())?;
    config.advanced_audio_api = advanced_audio::read(state)?;
    config.rewrite = rewrite::read(state);
    dictate_core::additional_parameters::parse(&config.extra_config)
        .map_err(|e| format!("Extra config: {e}"))?;
    Ok(config)
}

fn read_text(hwnd: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(hwnd) }.max(0) as usize;
    let mut buffer = vec![0_u16; length + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

fn format_json_input(hwnd: HWND) {
    let input = read_text(hwnd);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&input) else {
        return;
    };
    let Ok(formatted) = serde_json::to_string_pretty(&value) else {
        return;
    };
    // Native multiline EDIT controls use CRLF line endings.
    let formatted = formatted.replace('\n', "\r\n");
    if formatted != input {
        unsafe {
            let _ = SetWindowTextW(hwnd, PCWSTR(wide(&formatted).as_ptr()));
        }
    }
}

fn enable_controls(state: &SettingsState, enabled: bool) {
    for control in state
        .controls
        .values()
        .chain(state.rewrite.provider_items.iter())
        .chain(state.page_buttons.iter())
        .chain(std::iter::once(&state.language_control))
        .chain(state.language_items.iter())
        .chain(state.microphone.iter().map(|picker| &picker.button))
    {
        unsafe {
            let _ = EnableWindow(*control, enabled);
        }
    }
    update_input_controls(state);
}

fn update_input_controls(state: &SettingsState) {
    for key in ["__test_connectivity", "__rewrite_test"] {
        if let Some(button) = state.controls.get(key) {
            unsafe {
                let _ = EnableWindow(
                    *button,
                    !state.saving
                        && !matches!(state.connectivity_status, ConnectivityStatus::Testing),
                );
            }
        }
    }
    for picker in &state.audio_pickers {
        let enabled = !state.saving && state.audio_draft.enabled(picker.field);
        unsafe {
            let _ = EnableWindow(picker.button, enabled);
            let _ = EnableWindow(picker.editor, enabled);
        }
    }
    for key in ["VAD_PADDING_MS", "VAD_START_THRESHOLD"] {
        let Some(control) = state.controls.get(key) else {
            continue;
        };
        let enabled = !state.saving
            && state
                .boolean_values
                .get("ENABLE_VAD")
                .copied()
                .unwrap_or(false);
        unsafe {
            let _ = EnableWindow(*control, enabled);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    let segmented_upload_enabled = !state.saving
        && state
            .boolean_values
            .get("ENABLE_SEGMENTED_UPLOAD")
            .copied()
            .unwrap_or(false);
    for key in SEGMENTED_UPLOAD_KEYS {
        let Some(control) = state.controls.get(key) else {
            continue;
        };
        unsafe {
            let _ = EnableWindow(*control, segmented_upload_enabled);
            let _ = InvalidateRect(Some(*control), None, true);
        }
    }
    let enabled = !state.saving;
    for key in CLIPBOARD_DELAY_KEYS {
        if let Some(control) = state.controls.get(key) {
            unsafe {
                let _ = EnableWindow(*control, enabled);
                let _ = InvalidateRect(Some(*control), None, true);
            }
        }
        for (label, label_key) in &state.localized_controls {
            if *label_key == key {
                unsafe {
                    let _ = InvalidateRect(Some(*label), None, true);
                }
            }
        }
    }
    advanced_audio::update_controls(state);
}

fn create_font(dpi: u32, points: i32, bold: bool) -> HFONT {
    create_font_face(dpi, points, bold, w!("Segoe UI"))
}

fn create_font_face(dpi: u32, points: i32, bold: bool, face: PCWSTR) -> HFONT {
    unsafe {
        CreateFontW(
            -platform::scale(points, dpi),
            0,
            0,
            0,
            if bold {
                FW_BOLD.0 as i32
            } else {
                FW_NORMAL.0 as i32
            },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            u32::from(DEFAULT_PITCH.0 | FF_DONTCARE.0),
            face,
        )
    }
}

fn set_font(hwnd: HWND, font: HFONT) {
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
}

fn scaled_rect(rect: RECT, dpi: u32) -> RECT {
    RECT {
        left: platform::scale(rect.left, dpi),
        top: platform::scale(rect.top, dpi),
        right: platform::scale(rect.right, dpi),
        bottom: platform::scale(rect.bottom, dpi),
    }
}

unsafe fn rounded_box(hdc: HDC, rect: RECT, fill: COLORREF, border: COLORREF, radius: i32) {
    if crate::render::draw_rounded_box(hdc, rect, fill, border, radius).is_ok() {
        return;
    }
    // Keep controls usable if the Direct2D target cannot be created or is lost.
    unsafe {
        let old_brush = SelectObject(hdc, GetStockObject(DC_BRUSH));
        let old_pen = SelectObject(hdc, GetStockObject(DC_PEN));
        SetDCBrushColor(hdc, fill);
        SetDCPenColor(hdc, border);
        let diameter = radius.max(1) * 2;
        let _ = RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            diameter,
            diameter,
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
    }
}

unsafe fn stroke_polyline(hdc: HDC, points: &[POINT], color: COLORREF, width: i32) {
    if points.len() < 2 {
        return;
    }
    unsafe {
        let pen = CreatePen(PS_SOLID, width.max(1), color);
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = MoveToEx(hdc, points[0].x, points[0].y, None);
        for point in &points[1..] {
            let _ = LineTo(hdc, point.x, point.y);
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
}

unsafe fn draw_text_line(
    hdc: HDC,
    font: HFONT,
    text: &str,
    color: COLORREF,
    mut rect: RECT,
    alignment: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let mut text = text.encode_utf16().collect::<Vec<_>>();
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        DrawTextW(
            hdc,
            &mut text,
            &mut rect,
            alignment | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        SelectObject(hdc, old);
    }
}

unsafe fn draw_text_wrapped(hdc: HDC, font: HFONT, text: &str, color: COLORREF, mut rect: RECT) {
    let mut text = text.encode_utf16().collect::<Vec<_>>();
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        DrawTextW(
            hdc,
            &mut text,
            &mut rect,
            DT_LEFT | DT_WORDBREAK | DT_VCENTER | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
    }
}

unsafe fn fill_color(hdc: HDC, rect: RECT, color: COLORREF) {
    let brush = unsafe { CreateSolidBrush(color) };
    unsafe {
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

unsafe fn line(hdc: HDC, left: i32, top: i32, right: i32, bottom: i32, color: COLORREF) {
    if bottom == top {
        unsafe {
            fill_color(
                hdc,
                RECT {
                    left,
                    top,
                    right,
                    bottom: top + 1,
                },
                color,
            );
        }
    } else {
        unsafe {
            fill_color(
                hdc,
                RECT {
                    left,
                    top,
                    right: left + 1,
                    bottom,
                },
                color,
            );
        }
    }
}

fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16))
}

fn show_error(owner: HWND, message: &str) {
    let message = wide(message);
    unsafe {
        let _ = MessageBoxW(
            Some(owner),
            PCWSTR(message.as_ptr()),
            w!("Dictate Settings"),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
