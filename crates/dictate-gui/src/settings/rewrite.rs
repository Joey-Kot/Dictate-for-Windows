//! Rewrite settings and the prompt editor share the settings draft and theme.
mod icon_picker;

use super::*;
use crate::prompt_icons::{IconCache, IconDraft};
use dictate_core::rewrite::{
    BuiltinPromptIcon, PromptIcon, Provider, RewriteConfig, RewritePrompt,
};
use std::cell::Cell;
use windows::Win32::Graphics::Gdi::{RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RedrawWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const ID_PROVIDER: usize = 0x6800;
pub const ID_PROMPTS: usize = 0x6801;
pub const ID_ADD: usize = 0x6802;
const ID_EDIT: usize = 0x6803;
const ID_DELETE: usize = 0x6804;
const ID_UP: usize = 0x6805;
const ID_DOWN: usize = 0x6806;
pub const ID_TEST: usize = 0x6807;
const ID_MENU_ENABLED: usize = 0x6808;
const ID_MENU_KEY: usize = 0x6809;
const ID_MENU_CLEAR: usize = 0x680a;
const ID_OPTION: usize = 0x6820;
const WM_PROVIDER_ACTION: u32 = WM_APP + 86;
const PROVIDER_PANEL_SUBCLASS_ID: usize = 10;
const EDITOR_WIDTH: i32 = 700;
const EDITOR_HEADER: i32 = 38;
const EDITOR_ICON_HEIGHT: i32 = 114;
const EDITOR_BODY_HEIGHT: i32 = 598 + EDITOR_ICON_HEIGHT;
const EDITOR_API_HEIGHT: i32 = 126;
const EDITOR_TEST_BOTTOM_GAP: i32 = 10;
const ID_EDITOR_PROVIDER: usize = 0x69f0;
const ID_EDITOR_TEST: usize = 0x69f1;
const ID_EDITOR_TEST_STATUS: usize = 0x69f3;
const ID_EDITOR_API: usize = 0x6920;
const ID_EDITOR_OPTION: usize = 0x6a00;
const WM_EDITOR_REVEAL: u32 = WM_APP + 87;
const EDITOR_TEST_TIMER: usize = 0x69f2;
const ID_EDITOR_ICON_PREVIEW: usize = 0x69d0;
const ID_EDITOR_ICON_IMPORT: usize = 0x69d1;
const ID_EDITOR_KEY_CLEAR: usize = 0x69d2;
const ID_EDITOR_ICON_SELECT: usize = 0x69d3;
// Keep the entire palette away from the provider and connectivity controls.
const ID_EDITOR_ICON_FIRST: usize = 0x6b00;
const PROMPTS_TOP: i32 = 304;
const PROMPTS_HEIGHT: i32 = 132;

pub fn handles_escape(id: usize) -> bool {
    id == ID_PROVIDER || (ID_OPTION..ID_OPTION + Provider::ALL.len()).contains(&id)
}

pub(super) fn is_action_button(id: usize) -> bool {
    (ID_ADD..=ID_TEST).contains(&id) || id == ID_MENU_CLEAR
}

pub fn resize_list(state: &SettingsState) {
    let scale = |value| platform::scale(value, state.dpi);
    let origin = if state.rewrite.viewport.is_invalid() {
        POINT::default()
    } else {
        POINT {
            x: scale(SIDEBAR_WIDTH + 1),
            y: scale(HEADER_HEIGHT) + state.rewrite.page_offset.get(),
        }
    };
    let right = scale(EDIT_LEFT + EDIT_WIDTH) - origin.x;
    let scrollbar_left = right - dropdown_scrollbar::thickness(state.dpi);
    unsafe {
        // Keep a stable gutter; the shared bar draws no thumb for a short list.
        let _ = SetWindowPos(
            state.rewrite.list,
            None,
            scale(CONTENT_LEFT) - origin.x,
            scale(PROMPTS_TOP) - origin.y,
            scrollbar_left - scale(CONTENT_LEFT) + origin.x,
            scale(PROMPTS_HEIGHT),
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        SendMessageW(
            state.rewrite.list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(scale(dropdown::ROW_HEIGHT) as isize)),
        );
    }
    dropdown_scrollbar::position_rect(
        state.rewrite.scrollbar,
        RECT {
            left: scrollbar_left,
            top: scale(PROMPTS_TOP) - origin.y,
            right,
            bottom: scale(PROMPTS_TOP) + scale(PROMPTS_HEIGHT) - origin.y,
        },
        state.dpi,
    );
}

fn page_rect(state: &SettingsState, rect: RECT) -> RECT {
    let mut rect = scaled_rect(rect, state.dpi);
    let x = platform::scale(SIDEBAR_WIDTH + 1, state.dpi);
    let y = platform::scale(HEADER_HEIGHT, state.dpi) + state.rewrite.page_offset.get();
    rect.left -= x;
    rect.right -= x;
    rect.top -= y;
    rect.bottom -= y;
    rect
}

fn create_page_viewport(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(page_proc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())? },
        lpszClassName: w!("DictateRewriteSettingsPage"),
        ..Default::default()
    };
    unsafe {
        RegisterClassExW(&class);
    }
    let viewport = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT | WS_EX_COMPOSITED,
            w!("DictateRewriteSettingsPage"),
            w!(""),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            Some(state.hwnd),
            None,
            Some(instance.into()),
            Some(state.hwnd.0.cast_const()),
        )
    }
    .map_err(|e| e.to_string())?;
    state.rewrite.viewport = viewport;
    state.rewrite.page_scrollbar = dropdown_scrollbar::attach_viewport(viewport, state.dpi)?;
    for (&handle, &group) in &state.control_groups {
        if group != "Rewrite" {
            continue;
        }
        let hwnd = HWND(handle as *mut c_void);
        unsafe {
            SetParent(hwnd, Some(viewport)).map_err(|e| e.to_string())?;
            if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_TABSTOP.0 != 0 {
                windows::Win32::UI::Shell::SetWindowSubclass(
                    hwnd,
                    Some(page_focus_proc),
                    15,
                    state.hwnd.0 as usize,
                )
                .ok()
                .map_err(|e| e.to_string())?;
            }
        }
    }
    state.control_groups.insert(viewport.0 as usize, "Rewrite");
    resize_page(state);
    Ok(())
}

pub fn resize_page(state: &SettingsState) {
    if state.rewrite.viewport.is_invalid() {
        return;
    }
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(state.hwnd, &mut client);
    }
    let scale = |value| platform::scale(value, state.dpi);
    let top = scale(HEADER_HEIGHT);
    let height = (client.bottom - top - scale(FOOTER_HEIGHT)).max(1);
    let width = (client.right - scale(SIDEBAR_WIDTH + 1)).max(1);
    state.rewrite.page_offset.set(
        state
            .rewrite
            .page_offset
            .get()
            .clamp(0, (scale(756 - HEADER_HEIGHT) - height).max(0)),
    );
    unsafe {
        let _ = SetWindowPos(
            state.rewrite.viewport,
            None,
            scale(SIDEBAR_WIDTH + 1),
            top,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
    let controls = state
        .layouts
        .borrow()
        .iter()
        .filter(|(hwnd, _)| unsafe { GetParent(*hwnd).ok() == Some(state.rewrite.viewport) })
        .map(|(hwnd, rect)| (*hwnd, page_rect(state, *rect), true))
        .collect::<Vec<_>>();
    position_editor_controls(controls);
    resize_list(state);
    dropdown_scrollbar::position_rect(
        state.rewrite.page_scrollbar,
        RECT {
            left: width - scale(18),
            top: scale(4),
            right: width - scale(4),
            bottom: height - scale(4),
        },
        state.dpi,
    );
    unsafe {
        let _ = ShowWindow(
            state.rewrite.page_scrollbar,
            if scale(756 - HEADER_HEIGHT) > height {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
    }
    position_test_result(state);
    redraw_editor_controls(state.rewrite.viewport);
}

pub fn position_test_result(state: &SettingsState) {
    let view = state.connectivity_result;
    if view.editor.is_invalid() {
        return;
    }
    let rewrite = GROUPS[state.active_group].0 == "Rewrite";
    let parent = if rewrite {
        state.rewrite.viewport
    } else {
        state.hwnd
    };
    unsafe {
        if GetParent(view.editor).ok() != Some(parent) {
            let _ = SetParent(view.editor, Some(parent));
        }
    }
    if rewrite && connectivity_result_bounds(state).is_some() {
        let layout = test_result_layout(
            view.editor,
            CONTENT_LEFT,
            REWRITE_TEST_RESULT_TOP,
            EDIT_LEFT + EDIT_WIDTH,
        );
        let rect = page_rect(state, layout.editor);
        unsafe {
            let _ = SetWindowPos(
                view.editor,
                None,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            let _ = InvalidateRect(Some(state.rewrite.viewport), None, true);
        }
    }
}

unsafe extern "system" fn page_focus_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    if message == WM_SETFOCUS {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(data as *mut c_void)),
                WM_EDITOR_REVEAL,
                WPARAM(0),
                LPARAM(hwnd.0 as isize),
            );
        }
    } else if message == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(page_focus_proc), id);
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

unsafe extern "system" fn page_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(lparam.0 as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
    }
    let root = HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut c_void);
    let pointer = unsafe { GetWindowLongPtrW(root, GWLP_USERDATA) } as *mut SettingsState;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *pointer };
    match message {
        dropdown_scrollbar::VIEWPORT_GET_EXTENT => {
            LRESULT(platform::scale(756 - HEADER_HEIGHT, state.dpi) as isize)
        }
        dropdown_scrollbar::VIEWPORT_GET_OFFSET => {
            LRESULT(state.rewrite.page_offset.get() as isize)
        }
        dropdown_scrollbar::VIEWPORT_SET_OFFSET => {
            close_provider(state);
            state.rewrite.page_offset.set(wparam.0 as i32);
            resize_page(state);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => unsafe {
            SendMessageW(
                state.rewrite.page_scrollbar,
                message,
                Some(wparam),
                Some(lparam),
            )
        },
        WM_LBUTTONDOWN => {
            close_provider(state);
            LRESULT(0)
        }
        WM_COMMAND | WM_DRAWITEM | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN
        | WM_CTLCOLORLISTBOX => unsafe {
            if message == WM_COMMAND
                && matches!((wparam.0 >> 16) as u32, EN_SETFOCUS | EN_KILLFOCUS)
            {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            SendMessageW(state.hwnd, message, Some(wparam), Some(lparam))
        },
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            FillRect(dc, &client, state.background_brush);
            for frame in state
                .input_frames
                .iter()
                .filter(|frame| frame.group == "Rewrite")
            {
                rounded_box(
                    dc,
                    page_rect(state, frame.rect),
                    rgb(26, 35, 39),
                    if GetFocus() == frame.control {
                        rgb(92, 192, 176)
                    } else {
                        rgb(54, 68, 74)
                    },
                    platform::scale(7, state.dpi),
                );
            }
            if connectivity_result_bounds(state).is_some() {
                let layout = test_result_layout(
                    state.connectivity_result.editor,
                    CONTENT_LEFT,
                    REWRITE_TEST_RESULT_TOP,
                    EDIT_LEFT + EDIT_WIDTH,
                );
                paint_test_result(
                    dc,
                    state.dpi,
                    connectivity_result_state(state),
                    TestResultLayout {
                        icon: page_rect(state, layout.icon),
                        frame: page_rect(state, layout.frame),
                        editor: page_rect(state, layout.editor),
                    },
                    GetFocus() == state.connectivity_result.editor,
                );
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe extern "system" fn provider_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::DefSubclassProc;
    if message == WM_KEYDOWN
        && [VK_UP.0, VK_DOWN.0, VK_RETURN.0, VK_ESCAPE.0, VK_TAB.0].contains(&(wparam.0 as u16))
    {
        unsafe {
            let _ = PostMessageW(
                Some(GetAncestor(hwnd, GA_ROOT)),
                WM_PROVIDER_ACTION,
                wparam,
                LPARAM(hwnd.0 as isize),
            );
        }
        return LRESULT(0);
    }
    if message == WM_KILLFOCUS {
        unsafe {
            let _ = PostMessageW(
                Some(GetAncestor(hwnd, GA_ROOT)),
                WM_PROVIDER_ACTION,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub fn handle_message(
    state: &mut SettingsState,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> bool {
    if message == WM_EDITOR_REVEAL {
        let control = HWND(lparam.0 as *mut c_void);
        if let Some((_, rect)) = state
            .layouts
            .borrow()
            .iter()
            .find(|(hwnd, _)| *hwnd == control)
        {
            let rect = page_rect(state, *rect);
            let mut client = RECT::default();
            unsafe {
                let _ = GetClientRect(state.rewrite.viewport, &mut client);
            }
            let margin = platform::scale(12, state.dpi);
            let shift = if rect.top < margin {
                rect.top - margin
            } else if rect.bottom > client.bottom - margin {
                rect.bottom - client.bottom + margin
            } else {
                0
            };
            state
                .rewrite
                .page_offset
                .set(state.rewrite.page_offset.get() + shift);
        }
        resize_page(state);
        return true;
    }
    if message != WM_PROVIDER_ACTION {
        return false;
    }
    let focus = unsafe { GetFocus() };
    let button = state.controls["rewrite_provider"];
    let key = wparam.0 as u16;
    if key == 0 {
        if focus != button && !state.rewrite.provider_items.contains(&focus) {
            close_provider(state);
        }
    } else if key == VK_ESCAPE.0 || key == VK_TAB.0 {
        close_provider(state);
        unsafe {
            let target = if key == VK_TAB.0 {
                GetNextDlgTabItem(state.hwnd, Some(button), GetKeyState(VK_SHIFT.0 as i32) < 0)
                    .unwrap_or(button)
            } else {
                button
            };
            let _ = SetFocus(Some(target));
        }
    } else if !state.rewrite.provider_open {
        provider_menu(state);
    } else if key == VK_RETURN.0 {
        if let Some(index) = state
            .rewrite
            .provider_items
            .iter()
            .position(|hwnd| hwnd.0 as isize == lparam.0)
        {
            command(state, ID_OPTION + index, BN_CLICKED);
        }
    } else {
        let current = state
            .rewrite
            .provider_items
            .iter()
            .position(|hwnd| *hwnd == focus)
            .unwrap_or(0);
        let next = (current
            + if key == VK_UP.0 {
                Provider::ALL.len() - 1
            } else {
                1
            })
            % Provider::ALL.len();
        reveal_provider_option(state, next);
        unsafe {
            let _ = SetFocus(Some(state.rewrite.provider_items[next]));
        }
    }
    true
}

pub struct Page {
    pub config: RewriteConfig,
    pub provider_open: bool,
    pub provider_items: Vec<HWND>,
    panel: HWND,
    provider_scrollbar: HWND,
    provider_offset: i32,
    list: HWND,
    scrollbar: HWND,
    viewport: HWND,
    page_scrollbar: HWND,
    page_offset: Cell<i32>,
    pub icon_draft: IconDraft,
}

impl Page {
    pub fn new(config: RewriteConfig, config_path: &Path) -> Self {
        Self {
            config,
            provider_open: false,
            provider_items: vec![],
            panel: HWND::default(),
            provider_scrollbar: HWND::default(),
            provider_offset: 0,
            list: HWND::default(),
            scrollbar: HWND::default(),
            viewport: HWND::default(),
            page_scrollbar: HWND::default(),
            page_offset: Cell::new(0),
            icon_draft: IconDraft::new(config_path),
        }
    }
}

pub fn create(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let config = state.rewrite.config.clone();
    for (index, (key, value)) in [
        ("rewrite_provider", config.provider.label()),
        ("rewrite_url", config.base_url.as_str()),
        ("rewrite_key", config.api_key.as_str()),
        ("rewrite_model", config.model.as_str()),
    ]
    .into_iter()
    .enumerate()
    {
        let y = FIELD_TOP + index as i32 * 42;
        let label = create_label(
            state,
            state.language.text(key),
            CONTENT_LEFT,
            y,
            LABEL_WIDTH,
            FIELD_HEIGHT,
            instance,
        )?;
        state.control_groups.insert(label.0 as usize, "Rewrite");
        state.localized_controls.push((label, key));
        let control = if index == 0 {
            create_child(
                state,
                w!("BUTTON"),
                value,
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
                EDIT_LEFT,
                y,
                EDIT_WIDTH,
                FIELD_HEIGHT,
                ID_PROVIDER,
                instance,
            )?
        } else {
            let style = WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WINDOW_STYLE(
                    ES_AUTOHSCROLL as u32 | if index == 2 { ES_PASSWORD as u32 } else { 0 },
                );
            let control = create_child(
                state,
                w!("EDIT"),
                value,
                style,
                EDIT_LEFT + 10,
                y + 7,
                EDIT_WIDTH - 20,
                20,
                ID_PROVIDER + 0x10 + index,
                instance,
            )?;
            apply_dark_theme(control);
            state.input_frames.push(InputFrame {
                rect: RECT {
                    left: EDIT_LEFT,
                    top: y,
                    right: EDIT_LEFT + EDIT_WIDTH,
                    bottom: y + FIELD_HEIGHT,
                },
                group: "Rewrite",
                control,
            });
            control
        };
        state.control_groups.insert(control.0 as usize, "Rewrite");
        state.controls.insert(key, control);
    }
    let label = create_label(
        state,
        state.language.text("rewrite_prompts"),
        CONTENT_LEFT,
        262,
        LABEL_WIDTH,
        34,
        instance,
    )?;
    state.control_groups.insert(label.0 as usize, "Rewrite");
    state.localized_controls.push((label, "rewrite_prompts"));
    for (key, id, x, y, width, label) in [
        ("__rewrite_add", ID_ADD, EDIT_LEFT, 262, 168, "rewrite_add"),
        (
            "__rewrite_edit",
            ID_EDIT,
            CONTENT_LEFT,
            448,
            96,
            "rewrite_edit_action",
        ),
        (
            "__rewrite_delete",
            ID_DELETE,
            CONTENT_LEFT + 104,
            448,
            96,
            "rewrite_delete",
        ),
        ("__rewrite_up", ID_UP, CONTENT_LEFT + 212, 448, 48, "↑"),
        ("__rewrite_down", ID_DOWN, CONTENT_LEFT + 268, 448, 48, "↓"),
        (
            "__rewrite_test",
            ID_TEST,
            CONTENT_LEFT + 332,
            448,
            204,
            "test_connectivity",
        ),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(label),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            x,
            y,
            width,
            34,
            id,
            instance,
        )?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, "Rewrite");
        state.localized_controls.push((button, label));
    }
    for (key, y) in [("rewrite_menu_enabled", 492), ("rewrite_menu_key", 534)] {
        let label = create_label(
            state,
            state.language.text(key),
            CONTENT_LEFT,
            y,
            LABEL_WIDTH,
            FIELD_HEIGHT,
            instance,
        )?;
        state.control_groups.insert(label.0 as usize, "Rewrite");
        state.localized_controls.push((label, key));
    }
    let enabled = create_child(
        state,
        w!("BUTTON"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
        EDIT_LEFT,
        492,
        34,
        34,
        ID_MENU_ENABLED,
        instance,
    )?;
    state.controls.insert("rewrite_menu_enabled", enabled);
    state
        .boolean_values
        .insert("rewrite_menu_enabled", config.menu_enabled);
    state
        .boolean_ids
        .insert(ID_MENU_ENABLED, "rewrite_menu_enabled");
    state.control_groups.insert(enabled.0 as usize, "Rewrite");
    let menu_key = create_child(
        state,
        w!("EDIT"),
        &config.menu_key,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
        EDIT_LEFT + 10,
        541,
        EDIT_WIDTH - 112,
        20,
        ID_MENU_KEY,
        instance,
    )?;
    apply_dark_theme(menu_key);
    state.controls.insert("rewrite_menu_key", menu_key);
    state.control_groups.insert(menu_key.0 as usize, "Rewrite");
    state.input_frames.push(InputFrame {
        rect: RECT {
            left: EDIT_LEFT,
            top: 534,
            right: EDIT_LEFT + EDIT_WIDTH - 92,
            bottom: 568,
        },
        group: "Rewrite",
        control: menu_key,
    });
    let clear = create_child(
        state,
        w!("BUTTON"),
        state.language.text("hotkey_clear"),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
        EDIT_LEFT + EDIT_WIDTH - 84,
        534,
        84,
        34,
        ID_MENU_CLEAR,
        instance,
    )?;
    dropdown::track_hover(clear, false)?;
    state.controls.insert("__rewrite_menu_clear", clear);
    state.control_groups.insert(clear.0 as usize, "Rewrite");
    state.localized_controls.push((clear, "hotkey_clear"));
    let status = create_child(
        state,
        w!("STATIC"),
        state.language.text("hotkey_optional_help"),
        WS_CHILD | WS_VISIBLE,
        CONTENT_LEFT,
        578,
        520,
        44,
        0,
        instance,
    )?;
    state.control_groups.insert(status.0 as usize, "Rewrite");
    state
        .localized_controls
        .push((status, "hotkey_optional_help"));
    hotkeys::attach_optional(menu_key, config.menu_key.clone(), state.language, status)?;
    let list = create_child(
        state,
        w!("LISTBOX"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WINDOW_STYLE(
                (LBS_OWNERDRAWFIXED | LBS_HASSTRINGS | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32,
            ),
        CONTENT_LEFT,
        PROMPTS_TOP,
        EDIT_LEFT + EDIT_WIDTH - CONTENT_LEFT,
        PROMPTS_HEIGHT,
        ID_PROMPTS,
        instance,
    )?;
    apply_dark_theme(list);
    dropdown::track_hover(list, true)?;
    state.rewrite.list = list;
    state.controls.insert("__rewrite_list", list);
    state.control_groups.insert(list.0 as usize, "Rewrite");
    let scrollbar = dropdown_scrollbar::create(state, state.hwnd, list, instance)?;
    state.rewrite.scrollbar = scrollbar;
    state.controls.insert("__rewrite_scrollbar", scrollbar);
    state.control_groups.insert(scrollbar.0 as usize, "Rewrite");
    state.rewrite.panel = dropdown::create_panel(state, instance)?;
    if !unsafe {
        windows::Win32::UI::Shell::SetWindowSubclass(
            state.rewrite.panel,
            Some(provider_menu_scroll_proc),
            PROVIDER_PANEL_SUBCLASS_ID,
            state.hwnd.0 as usize,
        )
    }
    .as_bool()
    {
        return Err("Unable to initialize provider dropdown scrolling".into());
    }
    state.rewrite.provider_scrollbar = dropdown_scrollbar::attach_viewport_with_background(
        state.rewrite.panel,
        state.dpi,
        dropdown::background(),
    )?;
    for (index, provider) in Provider::ALL.iter().enumerate() {
        let item = create_child(
            state,
            w!("BUTTON"),
            provider.label(),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            0,
            0,
            1,
            1,
            ID_OPTION + index,
            instance,
        )?;
        unsafe {
            SetParent(item, Some(state.rewrite.panel)).map_err(|e| e.to_string())?;
        }
        dropdown::track_hover(item, false)?;
        state.rewrite.provider_items.push(item);
    }
    let hint = create_child(
        state,
        w!("STATIC"),
        state.language.text("rewrite_network_hint"),
        WS_CHILD | WS_VISIBLE,
        CONTENT_LEFT,
        710,
        520,
        36,
        0,
        instance,
    )?;
    state.control_groups.insert(hint.0 as usize, "Rewrite");
    state
        .localized_controls
        .push((hint, "rewrite_network_hint"));
    for hwnd in std::iter::once(&state.controls["rewrite_provider"])
        .chain(state.rewrite.provider_items.iter())
    {
        if !unsafe {
            windows::Win32::UI::Shell::SetWindowSubclass(*hwnd, Some(provider_proc), 3, 0)
        }
        .as_bool()
        {
            return Err("Unable to initialize provider dropdown".into());
        }
    }
    create_page_viewport(state, instance)?;
    refresh_list(state, 0);
    update_menu_controls(state);
    Ok(())
}

pub fn read(state: &SettingsState) -> RewriteConfig {
    let mut config = state.rewrite.config.clone();
    for (key, value) in [
        ("rewrite_url", &mut config.base_url),
        ("rewrite_key", &mut config.api_key),
        ("rewrite_model", &mut config.model),
    ] {
        if let Some(hwnd) = state.controls.get(key) {
            *value = read_text(*hwnd).trim().into();
        }
    }
    config.menu_enabled = state
        .boolean_values
        .get("rewrite_menu_enabled")
        .copied()
        .unwrap_or(false);
    config.menu_key = state
        .controls
        .get("rewrite_menu_key")
        .and_then(|hwnd| hotkeys::value(*hwnd))
        .unwrap_or_default();
    config
}

pub fn refresh_labels(state: &SettingsState) {
    for key in hotkeys::KEYS.into_iter().chain(["rewrite_menu_key"]) {
        if let Some(hwnd) = state.controls.get(key) {
            hotkeys::set_language(*hwnd, state.language);
        }
    }
}

pub fn close_provider(state: &mut SettingsState) {
    state.rewrite.provider_open = false;
    unsafe {
        let _ = ShowWindow(state.rewrite.panel, SW_HIDE);
    }
}

fn provider_menu(state: &mut SettingsState) {
    if state.rewrite.provider_open {
        close_provider(state);
        return;
    }
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(state.hwnd, &mut client);
    }
    // The provider list can exceed a short settings window. Bound it to the
    // visible content area and use the menu's viewport for the remaining rows.
    let available = (client.bottom
        - platform::scale(
            HEADER_HEIGHT + FOOTER_HEIGHT + dropdown::PADDING * 2,
            state.dpi,
        ))
    .max(1);
    let content_height =
        platform::scale(state.rewrite.provider_items.len() as i32 * 32, state.dpi).min(available);
    dropdown::position_pixels(
        state.rewrite.panel,
        state.controls["rewrite_provider"],
        content_height,
        state.dpi,
    );
    state.rewrite.provider_offset = 0;
    state.rewrite.provider_open = true;
    let index = Provider::ALL
        .iter()
        .position(|p| *p == state.rewrite.config.provider)
        .unwrap_or(0);
    reveal_provider_option(state, index);
    for item in &state.rewrite.provider_items {
        unsafe {
            let _ = ShowWindow(*item, SW_SHOW);
        }
    }
    unsafe {
        let _ = ShowWindow(state.rewrite.panel, SW_SHOW);
        let _ = SetFocus(Some(state.rewrite.provider_items[index]));
    }
}

fn layout_provider_menu(state: &SettingsState) {
    let panel = state.rewrite.panel;
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(panel, &mut client);
    }
    let padding = platform::scale(dropdown::PADDING, state.dpi);
    let extent = platform::scale(
        state.rewrite.provider_items.len() as i32 * 32 + dropdown::PADDING * 2,
        state.dpi,
    );
    let scrolling = extent > client.bottom;
    let gutter = if scrolling {
        dropdown_scrollbar::thickness(state.dpi)
    } else {
        0
    };
    for (index, item) in state.rewrite.provider_items.iter().enumerate() {
        let top =
            padding + platform::scale(index as i32 * 32, state.dpi) - state.rewrite.provider_offset;
        unsafe {
            let _ = SetWindowPos(
                *item,
                None,
                padding,
                top,
                (client.right - padding * 2 - gutter).max(1),
                platform::scale(32, state.dpi),
                SWP_NOACTIVATE,
            );
        }
    }
    dropdown_scrollbar::position_rect(
        state.rewrite.provider_scrollbar,
        RECT {
            left: client.right - padding - gutter,
            top: padding,
            right: client.right - padding,
            bottom: client.bottom - padding,
        },
        state.dpi,
    );
    unsafe {
        let _ = ShowWindow(
            state.rewrite.provider_scrollbar,
            if scrolling { SW_SHOW } else { SW_HIDE },
        );
        let _ = InvalidateRect(Some(panel), None, true);
    }
}

fn reveal_provider_option(state: &mut SettingsState, index: usize) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(state.rewrite.panel, &mut client);
    }
    let padding = platform::scale(dropdown::PADDING, state.dpi);
    let top = platform::scale(index as i32 * 32, state.dpi);
    let bottom = platform::scale((index as i32 + 1) * 32, state.dpi);
    let page = (client.bottom - padding * 2).max(1);
    let extent = platform::scale(state.rewrite.provider_items.len() as i32 * 32, state.dpi);
    let target = if top < state.rewrite.provider_offset {
        top
    } else if bottom > state.rewrite.provider_offset + page {
        bottom - page
    } else {
        state.rewrite.provider_offset
    };
    state.rewrite.provider_offset = target.clamp(0, (extent - page).max(0));
    layout_provider_menu(state);
}

unsafe extern "system" fn provider_menu_scroll_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    let root = HWND(data as *mut std::ffi::c_void);
    let pointer = unsafe { GetWindowLongPtrW(root, GWLP_USERDATA) } as *mut SettingsState;
    if pointer.is_null() {
        if message == WM_NCDESTROY {
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(provider_menu_scroll_proc), id);
            }
        }
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *pointer };
    match message {
        dropdown_scrollbar::VIEWPORT_GET_EXTENT => LRESULT(platform::scale(
            state.rewrite.provider_items.len() as i32 * 32 + dropdown::PADDING * 2,
            state.dpi,
        ) as isize),
        dropdown_scrollbar::VIEWPORT_GET_OFFSET => LRESULT(state.rewrite.provider_offset as isize),
        dropdown_scrollbar::VIEWPORT_SET_OFFSET => {
            if state.rewrite.provider_offset != wparam.0 as i32 {
                state.rewrite.provider_offset = wparam.0 as i32;
                layout_provider_menu(state);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => unsafe {
            SendMessageW(
                state.rewrite.provider_scrollbar,
                message,
                Some(wparam),
                Some(lparam),
            )
        },
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(provider_menu_scroll_proc), id);
            DefSubclassProc(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

fn refresh_list(state: &SettingsState, selected: usize) {
    resize_list(state);
    unsafe {
        SendMessageW(state.rewrite.list, LB_RESETCONTENT, None, None);
        for prompt in &state.rewrite.config.prompts {
            SendMessageW(
                state.rewrite.list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(wide(&prompt.title).as_ptr() as isize)),
            );
        }
        SendMessageW(
            state.rewrite.list,
            LB_SETCURSEL,
            Some(WPARAM(selected)),
            None,
        );
        let _ = InvalidateRect(Some(state.rewrite.list), None, true);
    }
    refresh_hotkey_context(state);
}

pub fn refresh_hotkey_context(state: &SettingsState) {
    let config = read(state);
    for key in hotkeys::KEYS {
        if let Some(hwnd) = state.controls.get(key) {
            let mut context: Vec<_> = config
                .prompts
                .iter()
                .map(|p| (p.title.clone(), p.hotkey.clone()))
                .collect();
            if config.menu_enabled {
                context.push((
                    state.language.text("rewrite_menu_key").into(),
                    config.menu_key.clone(),
                ));
            }
            hotkeys::set_context(*hwnd, context);
        }
    }
    if let Some(hwnd) = state.controls.get("rewrite_menu_key") {
        let mut context: Vec<_> = config
            .prompts
            .iter()
            .map(|p| (p.title.clone(), p.hotkey.clone()))
            .collect();
        context.extend(hotkeys::KEYS.into_iter().filter_map(|key| {
            state
                .controls
                .get(key)
                .and_then(|hwnd| hotkeys::value(*hwnd))
                .map(|value| (state.language.text(key).into(), value))
        }));
        hotkeys::set_context(*hwnd, context);
    }
}

pub fn update_menu_controls(state: &SettingsState) {
    let enabled = !state.saving
        && state
            .boolean_values
            .get("rewrite_menu_enabled")
            .copied()
            .unwrap_or(false);
    for key in ["rewrite_menu_key", "__rewrite_menu_clear"] {
        if let Some(hwnd) = state.controls.get(key) {
            unsafe {
                let _ = EnableWindow(*hwnd, enabled);
                let _ = InvalidateRect(Some(*hwnd), None, true);
            }
        }
    }
}

pub fn command(state: &mut SettingsState, id: usize, notification: u32) -> bool {
    if id == ID_MENU_KEY && matches!(notification, EN_CHANGE | EN_SETFOCUS | EN_KILLFOCUS) {
        refresh_hotkey_context(state);
        unsafe {
            let _ = InvalidateRect(Some(state.rewrite.viewport), None, true);
        }
        return true;
    }
    if id == ID_MENU_CLEAR && notification == BN_CLICKED {
        if !state.saving {
            hotkeys::clear(state.controls["rewrite_menu_key"]);
            refresh_hotkey_context(state);
        }
        return true;
    }
    if id == ID_PROVIDER && notification == BN_CLICKED && !state.saving {
        provider_menu(state);
        return true;
    }
    if (ID_OPTION..ID_OPTION + Provider::ALL.len()).contains(&id) && notification == BN_CLICKED {
        state.rewrite.config.provider = Provider::ALL[id - ID_OPTION];
        unsafe {
            let _ = SetWindowTextW(
                state.controls["rewrite_provider"],
                PCWSTR(wide(state.rewrite.config.provider.label()).as_ptr()),
            );
        }
        close_provider(state);
        unsafe {
            let _ = SetFocus(Some(state.controls["rewrite_provider"]));
        }
        return true;
    }
    let edit_double = id == ID_PROMPTS && notification == LBN_DBLCLK;
    if !((ID_ADD..=ID_TEST).contains(&id) && notification == BN_CLICKED || edit_double) {
        return false;
    }
    if state.saving {
        return true;
    }
    close_provider(state);
    if id == ID_TEST {
        advanced_audio::clear_workflow_test(state);
        start_connectivity(state, true);
        return true;
    }
    let selection = unsafe { SendMessageW(state.rewrite.list, LB_GETCURSEL, None, None).0 };
    let selected = usize::try_from(selection)
        .ok()
        .filter(|i| *i < state.rewrite.config.prompts.len());
    if id == ID_ADD || id == ID_EDIT || edit_double {
        let original = if id == ID_ADD {
            RewritePrompt::default()
        } else if let Some(index) = selected {
            state.rewrite.config.prompts[index].clone()
        } else {
            return true;
        };
        let owner = state.hwnd;
        let runtime = state.runtime.clone();
        let result = read_config(state).and_then(|config| {
            edit_prompt(
                owner,
                state.language,
                state.dpi,
                runtime,
                config,
                original,
                &state.config_path,
                state.rewrite.icon_draft.clone(),
            )
        });
        if !unsafe { IsWindow(Some(owner)) }.as_bool() {
            return true;
        }
        match result {
            Ok(Some((prompt, icons))) => {
                state.rewrite.icon_draft.absorb(icons);
                let index = state
                    .rewrite
                    .config
                    .prompts
                    .iter()
                    .position(|p| p.id == prompt.id);
                if let Some(index) = index {
                    state.rewrite.config.prompts[index] = prompt;
                } else {
                    state.rewrite.config.prompts.push(prompt);
                }
                refresh_list(
                    state,
                    index.unwrap_or(state.rewrite.config.prompts.len() - 1),
                );
            }
            Ok(None) => {}
            Err(error) => show_error(state.hwnd, &error),
        }
    } else if let Some(index) = selected {
        let mut next = index;
        match id {
            ID_DELETE => {
                state.rewrite.config.prompts.remove(index);
                next = index.saturating_sub(1);
            }
            ID_UP if index > 0 => {
                next -= 1;
                state.rewrite.config.prompts.swap(index, next);
            }
            ID_DOWN if index + 1 < state.rewrite.config.prompts.len() => {
                next += 1;
                state.rewrite.config.prompts.swap(index, next);
            }
            _ => {}
        }
        refresh_list(state, next);
    }
    true
}

pub fn draw(state: &SettingsState, item: &DRAWITEMSTRUCT) -> bool {
    let id = item.CtlID as usize;
    if id == ID_PROMPTS {
        if item.itemID == u32::MAX {
            return true;
        }
        let Some(prompt) = state.rewrite.config.prompts.get(item.itemID as usize) else {
            return true;
        };
        let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
        dropdown::draw_row(
            item.hDC,
            item.rcItem,
            selected,
            dropdown::hovered_row(item.hwndItem, item.itemID),
            state.dpi,
        );
        let mut rect = item.rcItem;
        rect.left += platform::scale(10, state.dpi);
        rect.right -= platform::scale(10, state.dpi);
        let hotkey_left = rect.right - platform::scale(200, state.dpi);
        let mut title_rect = rect;
        title_rect.right = hotkey_left - platform::scale(8, state.dpi);
        rect.left = hotkey_left;
        unsafe {
            draw_text_line(
                item.hDC,
                state.font,
                &prompt.title,
                rgb(230, 240, 240),
                title_rect,
                DT_LEFT,
            );
            draw_text_line(
                item.hDC,
                state.font,
                &prompt.hotkey,
                rgb(150, 180, 180),
                rect,
                windows::Win32::Graphics::Gdi::DT_RIGHT,
            );
        }
        return true;
    }
    if (ID_OPTION..ID_OPTION + Provider::ALL.len()).contains(&id) {
        let provider = Provider::ALL[id - ID_OPTION];
        dropdown::draw_row(
            item.hDC,
            item.rcItem,
            provider == state.rewrite.config.provider,
            item.itemState.0 & (ODS_FOCUS.0 | ODS_SELECTED.0) != 0
                || dropdown::hovered(item.hwndItem),
            state.dpi,
        );
        unsafe {
            draw_button_text(
                state,
                item,
                rgb(230, 240, 240),
                DT_LEFT,
                platform::scale(12, state.dpi),
                0,
            );
        }
        return true;
    }
    false
}

struct EditorLayout {
    hwnd: HWND,
    rect: RECT,
    frame: Option<RECT>,
    after_api: bool,
    independent_only: bool,
}

struct Editor {
    hwnd: HWND,
    body: HWND,
    scrollbar: HWND,
    offset: i32,
    language: Language,
    dpi: u32,
    font: HFONT,
    background: HBRUSH,
    input: HBRUSH,
    fields: [HWND; 4],
    api_fields: [HWND; 3],
    provider_button: HWND,
    provider_panel: HWND,
    provider_scrollbar: HWND,
    provider_offset: i32,
    provider_items: Vec<HWND>,
    provider_open: bool,
    buttons: [HWND; 3],
    status: HWND,
    test_status: HWND,
    test_result_state: TestResultState,
    test_result_height: i32,
    test_cancel: tokio_util::sync::CancellationToken,
    test_receiver: Option<std::sync::mpsc::Receiver<(Result<(), String>, u128)>>,
    runtime: Arc<Runtime>,
    config: Config,
    prompt: RewritePrompt,
    icon_draft: IconDraft,
    icon_cache: RefCell<IconCache>,
    icon_preview: HWND,
    icon_name: HWND,
    icon_selector: HWND,
    icon_panel: HWND,
    icon_open: bool,
    icon_key_latch: Option<u16>,
    icon_buttons: Vec<HWND>,
    result: Option<RewritePrompt>,
    layouts: Vec<EditorLayout>,
    frame_overlay: Option<SettingsFrameOverlay>,
}

impl Editor {
    fn independent(&self) -> bool {
        self.prompt.provider.is_some()
    }

    fn content_height(&self) -> i32 {
        EDITOR_BODY_HEIGHT
            + if self.independent() {
                EDITOR_API_HEIGHT
                    + if self.test_result_state != TestResultState::Idle {
                        self.test_result_height + EDITOR_TEST_BOTTOM_GAP
                    } else {
                        0
                    }
            } else {
                0
            }
    }

    fn provider_label(&self) -> &str {
        self.prompt.provider.map_or_else(
            || self.language.text("rewrite_same_provider"),
            Provider::label,
        )
    }

    fn content_rect(&self, rect: RECT, after_api: bool) -> RECT {
        let mut rect = rect;
        if after_api && self.independent() {
            rect.top += EDITOR_API_HEIGHT;
            rect.bottom += EDITOR_API_HEIGHT;
        }
        let mut scaled = scaled_rect(rect, self.dpi);
        if rect.right >= 650 {
            let mut client = RECT::default();
            unsafe {
                let _ = GetClientRect(self.hwnd, &mut client);
            }
            scaled.right = client.right - platform::scale(EDITOR_WIDTH - rect.right, self.dpi);
        }
        scaled
    }
}

// The editor borrows the outer draft's resource root and takes a cheap asset
// snapshot so cancel can discard imports independently from the settings draft.
#[allow(clippy::too_many_arguments)]
fn edit_prompt(
    owner: HWND,
    language: Language,
    dpi: u32,
    runtime: Arc<Runtime>,
    config: Config,
    prompt: RewritePrompt,
    config_path: &Path,
    icon_draft: IconDraft,
) -> Result<Option<(RewritePrompt, IconDraft)>, String> {
    let instance = unsafe { GetModuleHandleW(None).map_err(|e| e.to_string())? };
    let classes: [(PCWSTR, WNDPROC); 2] = [
        (w!("DictateRewritePromptEditor"), Some(editor_proc)),
        (w!("DictateRewritePromptBody"), Some(editor_body_proc)),
    ];
    for (name, proc) in classes {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: instance.into(),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())? },
            lpszClassName: name,
            ..Default::default()
        };
        unsafe { RegisterClassExW(&class) };
    }
    let mut editor = Box::new(Editor {
        hwnd: HWND::default(),
        body: HWND::default(),
        scrollbar: HWND::default(),
        offset: 0,
        language,
        dpi,
        font: create_font(dpi, 14, false),
        background: unsafe { CreateSolidBrush(rgb(16, 22, 25)) },
        input: unsafe { CreateSolidBrush(rgb(26, 35, 39)) },
        fields: [HWND::default(); 4],
        api_fields: [HWND::default(); 3],
        provider_button: HWND::default(),
        provider_panel: HWND::default(),
        provider_scrollbar: HWND::default(),
        provider_offset: 0,
        provider_items: vec![],
        provider_open: false,
        buttons: [HWND::default(); 3],
        status: HWND::default(),
        test_status: HWND::default(),
        test_result_state: TestResultState::Idle,
        test_result_height: 0,
        test_cancel: tokio_util::sync::CancellationToken::new(),
        test_receiver: None,
        runtime,
        config,
        prompt,
        icon_draft,
        icon_cache: RefCell::new(IconCache::new(config_path)),
        icon_preview: HWND::default(),
        icon_name: HWND::default(),
        icon_selector: HWND::default(),
        icon_panel: HWND::default(),
        icon_open: false,
        icon_key_latch: None,
        icon_buttons: vec![],
        result: None,
        layouts: vec![],
        frame_overlay: None,
    });
    let pointer: *mut Editor = &mut *editor;
    let mut bounds = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner, &mut bounds);
    }
    bounds.left += platform::scale(30, dpi);
    bounds.top += platform::scale(20, dpi);
    let bounds = editor_bounds(&editor, bounds);
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("DictateRewritePromptEditor"),
            PCWSTR(wide(language.text("rewrite_edit")).as_ptr()),
            WS_POPUP | WS_CLIPCHILDREN,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            Some(owner),
            None,
            Some(instance.into()),
            Some(pointer.cast()),
        )
    };
    let result = match created {
        Ok(hwnd) => {
            unsafe {
                let _ = EnableWindow(owner, false);
                platform::apply_dark_mode(hwnd);
                platform::disable_native_window_frame(hwnd);
                apply_settings_region(hwnd, dpi);
                let _ = ShowWindow(hwnd, SW_SHOW);
                sync_editor_frame(&mut editor, true);
                let _ = SetFocus(Some(editor.provider_button));
                let mut message = MSG::default();
                while IsWindow(Some(hwnd)).as_bool() {
                    let code = GetMessageW(&mut message, None, 0, 0).0;
                    if code <= 0 {
                        if code == 0 {
                            PostQuitMessage(message.wParam.0 as i32);
                        }
                        break;
                    }
                    if icon_picker::filter_message(&mut editor, &message) {
                        continue;
                    }
                    let focus = GetFocus();
                    if message.message == WM_KEYDOWN
                        && (focus == editor.provider_button
                            || editor.provider_items.contains(&focus))
                        && [VK_UP.0, VK_DOWN.0, VK_RETURN.0, VK_ESCAPE.0, VK_TAB.0]
                            .contains(&(message.wParam.0 as u16))
                        // With the menu closed, Escape cancels the editor below.
                        && (message.wParam.0 != VK_ESCAPE.0 as usize || editor.provider_open)
                    {
                        SendMessageW(
                            hwnd,
                            WM_PROVIDER_ACTION,
                            Some(message.wParam),
                            Some(LPARAM(focus.0 as isize)),
                        );
                        continue;
                    }
                    if message.message == WM_KEYDOWN
                        && message.wParam.0 == VK_ESCAPE.0 as usize
                        && hotkeys::value(focus).is_none()
                    {
                        if editor.provider_open {
                            close_editor_provider(&mut editor);
                        } else {
                            let _ = DestroyWindow(hwnd);
                        }
                        continue;
                    }
                    if hotkeys::value(focus).is_some()
                        || !IsDialogMessageW(hwnd, &message).as_bool()
                    {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                if IsWindow(Some(hwnd)).as_bool() {
                    let _ = DestroyWindow(hwnd);
                }
                if IsWindow(Some(owner)).as_bool() {
                    let _ = EnableWindow(owner, true);
                    let _ = SetForegroundWindow(owner);
                }
            }
            Ok(editor
                .result
                .take()
                .map(|prompt| (prompt, editor.icon_draft.clone())))
        }
        Err(error) => Err(error.to_string()),
    };
    editor.test_cancel.cancel();
    unsafe {
        let _ = DeleteObject(HGDIOBJ(editor.font.0));
        let _ = DeleteObject(HGDIOBJ(editor.background.0));
        let _ = DeleteObject(HGDIOBJ(editor.input.0));
    }
    result
}

fn editor_bounds(editor: &Editor, mut bounds: RECT) -> RECT {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect,
    };
    let mut width = platform::scale(EDITOR_WIDTH, editor.dpi);
    let mut height = platform::scale(
        editor.content_height() + EDITOR_HEADER + FOOTER_HEIGHT,
        editor.dpi,
    );
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(
            MonitorFromRect(&bounds, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        )
        .as_bool()
        {
            let work = monitor.rcWork;
            let margin = platform::scale(12, editor.dpi);
            width = width.min((work.right - work.left - margin * 2).max(1));
            height = height.min((work.bottom - work.top - margin * 2).max(1));
            bounds.left = bounds
                .left
                .clamp(work.left, (work.right - width).max(work.left));
            bounds.top = bounds.top.clamp(
                work.top + margin,
                (work.bottom - margin - height).max(work.top + margin),
            );
        }
    }
    bounds.right = bounds.left + width;
    bounds.bottom = bounds.top + height;
    bounds
}

fn resize_editor(editor: &mut Editor, suggested: Option<RECT>) {
    let mut bounds = suggested.unwrap_or_default();
    if suggested.is_none() {
        unsafe {
            let _ = GetWindowRect(editor.hwnd, &mut bounds);
        }
    }
    let bounds = editor_bounds(editor, bounds);
    unsafe {
        let _ = SetWindowPos(
            editor.hwnd,
            None,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
    layout_editor(editor);
}

#[allow(clippy::too_many_arguments)]
fn editor_child(
    editor: &Editor,
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    rect: RECT,
    id: usize,
) -> Result<HWND, String> {
    let rect = scaled_rect(rect, editor.dpi);
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR(wide(text).as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            Some(parent),
            (id != 0).then_some(HMENU(id as *mut c_void)),
            None,
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    set_font(hwnd, editor.font);
    if style.0 & WS_TABSTOP.0 != 0
        && !unsafe {
            windows::Win32::UI::Shell::SetWindowSubclass(
                hwnd,
                Some(editor_focus_proc),
                8,
                editor.hwnd.0 as usize,
            )
        }
        .as_bool()
    {
        return Err("Unable to initialize prompt focus tracking".into());
    }
    Ok(hwnd)
}

#[allow(clippy::too_many_arguments)]
fn editor_body_child(
    editor: &mut Editor,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    rect: RECT,
    id: usize,
    after_api: bool,
    independent_only: bool,
    frame: Option<RECT>,
) -> Result<HWND, String> {
    let hwnd = editor_child(editor, editor.body, class, text, style, rect, id)?;
    editor.layouts.push(EditorLayout {
        hwnd,
        rect,
        frame,
        after_api,
        independent_only,
    });
    Ok(hwnd)
}

fn palette_icon(index: usize) -> PromptIcon {
    BuiltinPromptIcon::ALL
        .get(index)
        .copied()
        .map(PromptIcon::Builtin)
        .unwrap_or_default()
}

fn icon_label(icon: &PromptIcon) -> &'static str {
    match icon {
        PromptIcon::Default => "rewrite_icon_chat",
        PromptIcon::Builtin(BuiltinPromptIcon::Translate) => "rewrite_icon_translate",
        PromptIcon::Builtin(BuiltinPromptIcon::TextCard) => "rewrite_icon_text_card",
        PromptIcon::Builtin(BuiltinPromptIcon::Layers) => "rewrite_icon_layers",
        PromptIcon::Builtin(BuiltinPromptIcon::Feather) => "rewrite_icon_feather",
        PromptIcon::Builtin(BuiltinPromptIcon::Document) => "rewrite_icon_document",
        PromptIcon::Builtin(BuiltinPromptIcon::Messages) => "rewrite_icon_messages",
        PromptIcon::Builtin(BuiltinPromptIcon::Shield) => "rewrite_icon_shield",
        PromptIcon::Builtin(BuiltinPromptIcon::Chat) => "rewrite_icon_chat",
        PromptIcon::Builtin(BuiltinPromptIcon::Eye) => "rewrite_icon_eye",
        PromptIcon::Builtin(BuiltinPromptIcon::PieChart) => "rewrite_icon_pie_chart",
        PromptIcon::Builtin(BuiltinPromptIcon::Tags) => "rewrite_icon_tags",
        PromptIcon::Builtin(BuiltinPromptIcon::Box) => "rewrite_icon_box",
        PromptIcon::Builtin(BuiltinPromptIcon::Flask) => "rewrite_icon_flask",
        PromptIcon::Builtin(BuiltinPromptIcon::Globe) => "rewrite_icon_globe",
        PromptIcon::Builtin(BuiltinPromptIcon::Cookie) => "rewrite_icon_cookie",
        PromptIcon::Builtin(BuiltinPromptIcon::Window) => "rewrite_icon_window",
        PromptIcon::Builtin(BuiltinPromptIcon::Plug) => "rewrite_icon_plug",
        PromptIcon::Builtin(BuiltinPromptIcon::Settings) => "rewrite_icon_settings",
        PromptIcon::Builtin(BuiltinPromptIcon::Key) => "rewrite_icon_key",
        PromptIcon::Builtin(BuiltinPromptIcon::Network) => "rewrite_icon_network",
        PromptIcon::Builtin(BuiltinPromptIcon::Hierarchy) => "rewrite_icon_hierarchy",
        PromptIcon::Builtin(BuiltinPromptIcon::BarChart) => "rewrite_icon_bar_chart",
        PromptIcon::Builtin(BuiltinPromptIcon::Microphone) => "rewrite_icon_microphone",
        PromptIcon::Builtin(BuiltinPromptIcon::Fingerprint) => "rewrite_icon_fingerprint",
        PromptIcon::Builtin(BuiltinPromptIcon::Layout) => "rewrite_icon_layout",
        PromptIcon::Custom(_) => "rewrite_icon_custom",
    }
}

fn create_icon_controls(editor: &mut Editor) -> Result<(), String> {
    editor_body_child(
        editor,
        w!("STATIC"),
        editor.language.text("rewrite_icon"),
        WINDOW_STYLE(0),
        RECT {
            left: 24,
            top: 82,
            right: 660,
            bottom: 104,
        },
        0,
        true,
        false,
        None,
    )?;
    editor.icon_preview = editor_body_child(
        editor,
        w!("BUTTON"),
        editor.language.text("rewrite_icon_preview"),
        WINDOW_STYLE(BS_OWNERDRAW as u32),
        RECT {
            left: 24,
            top: 108,
            right: 72,
            bottom: 156,
        },
        ID_EDITOR_ICON_PREVIEW,
        true,
        false,
        None,
    )?;
    for (id, key, left, right) in [
        (ID_EDITOR_ICON_SELECT, "rewrite_icon_select", 88, 296),
        (ID_EDITOR_ICON_IMPORT, "rewrite_icon_import", 308, 516),
    ] {
        let button = editor_body_child(
            editor,
            w!("BUTTON"),
            editor.language.text(key),
            WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            RECT {
                left,
                top: 115,
                right,
                bottom: 149,
            },
            id,
            true,
            false,
            None,
        )?;
        dropdown::track_hover(button, false)?;
        if id == ID_EDITOR_ICON_SELECT {
            editor.icon_selector = button;
        }
    }
    let name = editor
        .language
        .text(icon_label(&editor.prompt.icon))
        .to_owned();
    editor.icon_name = editor_body_child(
        editor,
        w!("STATIC"),
        &name,
        WINDOW_STYLE(0),
        RECT {
            left: 24,
            top: 164,
            right: 660,
            bottom: 186,
        },
        0,
        true,
        false,
        None,
    )?;
    Ok(())
}

fn choose_icon(editor: &mut Editor, icon: PromptIcon) {
    editor.prompt.icon = icon;
    unsafe {
        let _ = SetWindowTextW(
            editor.icon_name,
            PCWSTR(wide(editor.language.text(icon_label(&editor.prompt.icon))).as_ptr()),
        );
        for hwnd in std::iter::once(&editor.icon_preview).chain(editor.icon_buttons.iter()) {
            let _ = InvalidateRect(Some(*hwnd), None, true);
        }
    }
}

fn import_icon(editor: &mut Editor) -> Result<(), String> {
    use windows::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::PWSTR;
    hotkeys::deactivate();
    let mut filename = vec![0_u16; 32_768];
    let filter = wide("PNG, JPEG, SVG\0*.png;*.jpg;*.jpeg;*.svg\0\0");
    let title = wide(editor.language.text("rewrite_icon_import"));
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: editor.hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        let error = unsafe { CommDlgExtendedError() };
        return if error.0 == 0 {
            Ok(())
        } else {
            Err(format!(
                "{}: {}",
                editor.language.text("rewrite_icon_import_failed"),
                error.0
            ))
        };
    }
    let length = filename
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(filename.len());
    let path = PathBuf::from(String::from_utf16_lossy(&filename[..length]));
    let icon = editor.icon_draft.stage(&path).map_err(|error| {
        format!(
            "{}: {error}",
            editor.language.text("rewrite_icon_import_failed")
        )
    })?;
    choose_icon(editor, icon);
    Ok(())
}

fn create_editor(editor: &mut Editor) -> Result<(), String> {
    let instance = unsafe { GetModuleHandleW(None).map_err(|e| e.to_string())? };
    editor.body = unsafe {
        CreateWindowExW(
            // Compose the opaque body and its native controls together. Keep
            // the dropdown's layered rounded surface outside this subtree.
            WS_EX_CONTROLPARENT | WS_EX_COMPOSITED,
            w!("DictateRewritePromptBody"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            Some(editor.hwnd),
            None,
            Some(instance.into()),
            Some((editor as *mut Editor).cast()),
        )
    }
    .map_err(|e| e.to_string())?;
    editor.scrollbar = dropdown_scrollbar::attach_viewport(editor.body, editor.dpi)?;
    editor_body_child(
        editor,
        w!("STATIC"),
        editor.language.text("rewrite_provider"),
        WINDOW_STYLE(0),
        RECT {
            left: 24,
            top: 8,
            right: 660,
            bottom: 30,
        },
        0,
        false,
        false,
        None,
    )?;
    let provider_label = editor.provider_label().to_string();
    editor.provider_button = editor_body_child(
        editor,
        w!("BUTTON"),
        &provider_label,
        WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
        RECT {
            left: 24,
            top: 34,
            right: 660,
            bottom: 68,
        },
        ID_EDITOR_PROVIDER,
        false,
        false,
        None,
    )?;
    dropdown::track_hover(editor.provider_button, false)?;
    let api_values = [
        editor.prompt.base_url.clone(),
        editor.prompt.api_key.clone(),
        editor.prompt.model.clone(),
    ];
    for (index, key) in ["rewrite_url", "rewrite_key", "rewrite_model"]
        .into_iter()
        .enumerate()
    {
        let y = 82 + index as i32 * 42;
        editor_body_child(
            editor,
            w!("STATIC"),
            editor.language.text(key),
            WINDOW_STYLE(0),
            RECT {
                left: 24,
                top: y + 6,
                right: 156,
                bottom: y + 28,
            },
            0,
            false,
            true,
            None,
        )?;
        let frame = RECT {
            left: 166,
            top: y,
            right: 660,
            bottom: y + 34,
        };
        let field = editor_body_child(
            editor,
            w!("EDIT"),
            &api_values[index],
            WS_TABSTOP
                | WINDOW_STYLE(
                    ES_AUTOHSCROLL as u32 | if index == 1 { ES_PASSWORD as u32 } else { 0 },
                ),
            RECT {
                left: 176,
                top: y + 6,
                right: 650,
                bottom: y + 28,
            },
            ID_EDITOR_API + index,
            false,
            true,
            Some(frame),
        )?;
        apply_dark_theme(field);
        editor.api_fields[index] = field;
    }
    create_icon_controls(editor)?;
    icon_picker::create(editor, instance)?;
    let values = [
        editor.prompt.title.clone(),
        editor.prompt.prompt.clone(),
        editor.prompt.extra_config.clone(),
        editor.prompt.hotkey.clone(),
    ];
    for (index, (key, y, height)) in [
        ("rewrite_title", 108, 34),
        ("rewrite_content", 182, 132),
        ("ExtraConfig", 354, 106),
        ("rewrite_hotkey", 500, 34),
    ]
    .into_iter()
    .enumerate()
    {
        let y = y + EDITOR_ICON_HEIGHT;
        editor_body_child(
            editor,
            w!("STATIC"),
            editor.language.text(key),
            WINDOW_STYLE(0),
            RECT {
                left: 24,
                top: y - 26,
                right: 660,
                bottom: y - 4,
            },
            0,
            true,
            false,
            None,
        )?;
        let multiline = index == 1 || index == 2;
        let style = WS_TABSTOP
            | WINDOW_STYLE(if multiline {
                (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32 | WS_CLIPCHILDREN.0
            } else {
                ES_AUTOHSCROLL as u32
            });
        let field = editor_body_child(
            editor,
            w!("EDIT"),
            &values[index],
            style,
            RECT {
                left: 34,
                top: y + 6,
                right: if index == 3 { 552 } else { 650 },
                bottom: y + height - 6,
            },
            0x6900 + index,
            true,
            false,
            Some(RECT {
                left: 24,
                top: y,
                right: if index == 3 { 562 } else { 660 },
                bottom: y + height,
            }),
        )?;
        unsafe {
            SendMessageW(
                field,
                windows::Win32::UI::Controls::EM_SETLIMITTEXT,
                Some(WPARAM(1_000_000)),
                None,
            );
        }
        apply_dark_theme(field);
        if multiline {
            dropdown_scrollbar::attach_edit(field, editor.dpi)?;
        }
        editor.fields[index] = field;
    }
    editor.status = editor_body_child(
        editor,
        w!("STATIC"),
        editor.language.text("hotkey_optional_help"),
        WINDOW_STYLE(0),
        RECT {
            left: 24,
            top: 544 + EDITOR_ICON_HEIGHT,
            right: 660,
            bottom: 588 + EDITOR_ICON_HEIGHT,
        },
        0,
        true,
        false,
        None,
    )?;
    editor.test_status = editor_body_child(
        editor,
        w!("EDIT"),
        "",
        WS_TABSTOP
            | WINDOW_STYLE(
                (ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32 | WS_CLIPCHILDREN.0,
            ),
        RECT {
            left: 60,
            top: 603,
            right: 654,
            bottom: 667,
        },
        ID_EDITOR_TEST_STATUS,
        true,
        true,
        None,
    )?;
    apply_dark_theme(editor.test_status);
    let margin = platform::scale(7, editor.dpi) as u32;
    unsafe {
        SendMessageW(
            editor.test_status,
            EM_SETMARGINS,
            Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
            Some(LPARAM((margin | (margin << 16)) as isize)),
        );
    }
    dropdown_scrollbar::attach_edit(editor.test_status, editor.dpi)?;
    let mut context = vec![
        ("Start".into(), editor.config.start_key.clone()),
        ("Pause".into(), editor.config.pause_key.clone()),
        (
            "Cancel / Retry".into(),
            editor.config.cancel_or_retry_key.clone(),
        ),
    ];
    context.extend(
        editor
            .config
            .rewrite
            .prompts
            .iter()
            .filter(|p| p.id != editor.prompt.id)
            .map(|p| (p.title.clone(), p.hotkey.clone())),
    );
    if editor.config.rewrite.menu_enabled {
        context.push((
            editor.language.text("rewrite_menu_key").into(),
            editor.config.rewrite.menu_key.clone(),
        ));
    }
    hotkeys::attach_optional(
        editor.fields[3],
        editor.prompt.hotkey.clone(),
        editor.language,
        editor.status,
    )?;
    hotkeys::set_context(editor.fields[3], context);
    let clear = editor_body_child(
        editor,
        w!("BUTTON"),
        editor.language.text("hotkey_clear"),
        WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
        RECT {
            left: 574,
            top: 500 + EDITOR_ICON_HEIGHT,
            right: 660,
            bottom: 534 + EDITOR_ICON_HEIGHT,
        },
        ID_EDITOR_KEY_CLEAR,
        true,
        false,
        None,
    )?;
    dropdown::track_hover(clear, false)?;
    for (index, (id, key)) in [
        (ID_EDITOR_TEST, "test_connectivity"),
        (ID_CANCEL, "cancel"),
        (ID_SAVE, "save"),
    ]
    .into_iter()
    .enumerate()
    {
        let button = editor_child(
            editor,
            editor.hwnd,
            w!("BUTTON"),
            editor.language.text(key),
            WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            id,
        )?;
        dropdown::track_hover(button, false)?;
        editor.buttons[index] = button;
    }
    editor.provider_panel = dropdown::create_panel_for(editor.hwnd, editor.dpi, instance)?;
    if !unsafe {
        windows::Win32::UI::Shell::SetWindowSubclass(
            editor.provider_panel,
            Some(editor_menu_scroll_proc),
            9,
            editor as *mut Editor as usize,
        )
    }
    .as_bool()
    {
        return Err("Unable to initialize prompt provider scrolling".into());
    }
    editor.provider_scrollbar = dropdown_scrollbar::attach_viewport_with_background(
        editor.provider_panel,
        editor.dpi,
        dropdown::background(),
    )?;
    for index in 0..=Provider::ALL.len() {
        let text = if index == 0 {
            editor.language.text("rewrite_same_provider")
        } else {
            Provider::ALL[index - 1].label()
        };
        let item = editor_child(
            editor,
            editor.provider_panel,
            w!("BUTTON"),
            text,
            WINDOW_STYLE(BS_OWNERDRAW as u32),
            RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            ID_EDITOR_OPTION + index,
        )?;
        dropdown::track_hover(item, false)?;
        editor.provider_items.push(item);
    }
    for child in
        std::iter::once(editor.provider_button).chain(editor.provider_items.iter().copied())
    {
        if !unsafe {
            windows::Win32::UI::Shell::SetWindowSubclass(child, Some(provider_proc), 3, 0)
        }
        .as_bool()
        {
            return Err("Unable to initialize prompt provider dropdown".into());
        }
    }
    editor.frame_overlay = Some(SettingsFrameOverlay::create(
        editor.hwnd,
        instance,
        editor.dpi,
        EDITOR_WIDTH,
        editor.content_height() + EDITOR_HEADER + FOOTER_HEIGHT,
    )?);
    layout_editor(editor);
    sync_editor_frame(editor, true);
    Ok(())
}

fn layout_editor(editor: &mut Editor) {
    if editor.body.is_invalid() {
        return;
    }
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(editor.hwnd, &mut client);
        let top = platform::scale(EDITOR_HEADER, editor.dpi);
        let height = (client.bottom - top - platform::scale(FOOTER_HEIGHT, editor.dpi)).max(1);
        let _ = SetWindowPos(
            editor.body,
            None,
            0,
            top,
            client.right,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        let max = (platform::scale(editor.content_height(), editor.dpi) - height).max(0);
        editor.offset = editor.offset.clamp(0, max);
        layout_editor_body(editor);
        dropdown_scrollbar::position_rect(
            editor.scrollbar,
            RECT {
                left: client.right - platform::scale(26, editor.dpi),
                top: 4,
                right: client.right - platform::scale(12, editor.dpi),
                bottom: height - 4,
            },
            editor.dpi,
        );
        let _ = ShowWindow(editor.scrollbar, if max > 0 { SW_SHOW } else { SW_HIDE });
        for (index, (left, width)) in [
            (platform::scale(24, editor.dpi), 210),
            (client.right - platform::scale(208, editor.dpi), 88),
            (client.right - platform::scale(110, editor.dpi), 94),
        ]
        .into_iter()
        .enumerate()
        {
            let _ = SetWindowPos(
                editor.buttons[index],
                None,
                left,
                client.bottom - platform::scale(46, editor.dpi),
                platform::scale(width, editor.dpi),
                platform::scale(34, editor.dpi),
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let _ = ShowWindow(
            editor.buttons[0],
            if editor.independent() {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        let _ = InvalidateRect(Some(editor.hwnd), None, true);
    }
}

// All controls in a batch share a parent. Suppress intermediate paints and
// visibility changes; the caller redraws the whole subtree after layout.
fn position_editor_controls(controls: impl IntoIterator<Item = (HWND, RECT, bool)>) {
    let controls: Vec<_> = controls
        .into_iter()
        .map(|(hwnd, rect, visible)| {
            let shown = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_VISIBLE.0 != 0 };
            let mut flags = SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOREDRAW | SWP_NOCOPYBITS;
            if visible != shown {
                flags |= if visible {
                    SWP_SHOWWINDOW
                } else {
                    SWP_HIDEWINDOW
                };
            }
            (hwnd, rect, flags)
        })
        .collect();
    if controls.is_empty() {
        return;
    }
    unsafe {
        let positioned = (|| -> windows::core::Result<()> {
            let mut batch = BeginDeferWindowPos(controls.len() as i32)?;
            for &(hwnd, rect, flags) in &controls {
                batch = DeferWindowPos(
                    batch,
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    flags,
                )?;
            }
            EndDeferWindowPos(batch)
        })();
        if positioned.is_err() {
            // A failed DeferWindowPos discards its batch; do not end it again.
            // Complete the layout without repainting each control separately.
            for (hwnd, rect, flags) in controls {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    flags,
                );
            }
        }
    }
}

fn redraw_editor_controls(parent: HWND) {
    unsafe {
        // NOREDRAW also suppresses child updates. Invalidate every descendant,
        // including the native EDIT controls and their custom scrollbars.
        let _ = RedrawWindow(
            Some(parent),
            None,
            None,
            RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_ERASE,
        );
    }
}

fn editor_test_result_visible(editor: &Editor) -> bool {
    editor.independent() && editor.test_result_state != TestResultState::Idle
}

fn editor_test_result_layout(editor: &Editor) -> TestResultLayout {
    test_result_layout_with_height(
        24,
        EDITOR_BODY_HEIGHT,
        660,
        editor.test_result_height.max(TEST_RESULT_MIN_HEIGHT),
    )
}

fn editor_test_result_layout_in_body(editor: &Editor) -> TestResultLayout {
    let layout = editor_test_result_layout(editor);
    let translate = |rect| editor.content_rect(rect, true);
    TestResultLayout {
        icon: translate(layout.icon),
        frame: translate(layout.frame),
        editor: translate(layout.editor),
    }
}

fn layout_editor_body(editor: &Editor) {
    position_editor_controls(editor.layouts.iter().map(|item| {
        let (mut rect, visible) = if item.hwnd == editor.test_status {
            (
                editor_test_result_layout_in_body(editor).editor,
                editor_test_result_visible(editor),
            )
        } else {
            (
                editor.content_rect(item.rect, item.after_api),
                !item.independent_only || editor.independent(),
            )
        };
        rect.top -= editor.offset;
        rect.bottom -= editor.offset;
        (item.hwnd, rect, visible)
    }));
    redraw_editor_controls(editor.body);
}

fn reveal_editor_control(editor: &mut Editor, hwnd: HWND) {
    let Some(item) = editor.layouts.iter().find(|item| item.hwnd == hwnd) else {
        return;
    };
    if item.independent_only && !editor.independent() {
        return;
    }
    let mut rect = if hwnd == editor.test_status {
        editor.content_rect(editor_test_result_layout(editor).frame, item.after_api)
    } else if let Some(frame) = item.frame {
        editor.content_rect(frame, item.after_api)
    } else {
        editor.content_rect(item.rect, item.after_api)
    };
    rect.top = (rect.top - platform::scale(28, editor.dpi)).max(0);
    rect.bottom += platform::scale(8, editor.dpi);
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(editor.body, &mut client);
    }
    let new_offset = if rect.top < editor.offset {
        rect.top
    } else if rect.bottom > editor.offset + client.bottom {
        (rect.bottom - client.bottom).min(rect.top)
    } else {
        editor.offset
    };
    let max = (platform::scale(editor.content_height(), editor.dpi) - client.bottom).max(0);
    if new_offset.clamp(0, max) != editor.offset {
        close_editor_provider(editor);
        icon_picker::close(editor, true);
        editor.offset = new_offset.clamp(0, max);
        layout_editor_body(editor);
    }
}

fn sync_editor_frame(editor: &mut Editor, resized: bool) {
    let Some(frame) = &mut editor.frame_overlay else {
        return;
    };
    if resized {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(editor.hwnd, &mut rect);
        }
        if let Ok(renderer) = RoundedOutlineRenderer::new(
            rect.right as f32 * 96.0 / editor.dpi as f32,
            rect.bottom as f32 * 96.0 / editor.dpi as f32,
            PANEL_CORNER_RADIUS as f32,
            editor.dpi,
        ) {
            frame.renderer = renderer;
        }
    }
    frame.sync(editor.hwnd, resized);
}

fn close_editor_provider(editor: &mut Editor) {
    if !editor.provider_open {
        return;
    }
    editor.provider_open = false;
    unsafe {
        let _ = ShowWindow(editor.provider_panel, SW_HIDE);
        let _ = InvalidateRect(Some(editor.provider_button), None, false);
    }
}

fn layout_editor_menu(editor: &Editor) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(editor.provider_panel, &mut client);
    }
    let padding = platform::scale(dropdown::PADDING, editor.dpi);
    let extent = platform::scale(
        editor.provider_items.len() as i32 * 32 + dropdown::PADDING * 2,
        editor.dpi,
    );
    let scrolling = extent > client.bottom;
    let gutter = if scrolling {
        dropdown_scrollbar::thickness(editor.dpi)
    } else {
        0
    };
    position_editor_controls(
        editor
            .provider_items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let top = padding + platform::scale(index as i32 * 32, editor.dpi)
                    - editor.provider_offset;
                (
                    *item,
                    RECT {
                        left: padding,
                        top,
                        right: client.right - padding - gutter,
                        bottom: top + platform::scale(32, editor.dpi),
                    },
                    true,
                )
            }),
    );
    dropdown_scrollbar::position_rect(
        editor.provider_scrollbar,
        RECT {
            left: client.right - padding - gutter,
            top: padding,
            right: client.right - padding,
            bottom: client.bottom - padding,
        },
        editor.dpi,
    );
    unsafe {
        let _ = ShowWindow(
            editor.provider_scrollbar,
            if scrolling { SW_SHOW } else { SW_HIDE },
        );
    }
    redraw_editor_controls(editor.provider_panel);
}

fn reveal_editor_option(editor: &mut Editor, index: usize) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(editor.provider_panel, &mut client);
    }
    let padding = platform::scale(dropdown::PADDING, editor.dpi);
    let top = platform::scale(index as i32 * 32, editor.dpi);
    let bottom = platform::scale((index as i32 + 1) * 32, editor.dpi);
    let page = (client.bottom - padding * 2).max(1);
    if top < editor.provider_offset {
        editor.provider_offset = top;
    } else if bottom > editor.provider_offset + page {
        editor.provider_offset = bottom - page;
    }
    layout_editor_menu(editor);
}

fn editor_provider_menu(editor: &mut Editor) {
    icon_picker::close(editor, false);
    if editor.provider_open {
        close_editor_provider(editor);
        return;
    }
    reveal_editor_control(editor, editor.provider_button);
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(editor.hwnd, &mut client);
    }
    // The shared panel positions itself above HEADER_HEIGHT when space below
    // the selector is tight. Bound its height to that same content area.
    let available = (client.bottom
        - platform::scale(
            HEADER_HEIGHT + FOOTER_HEIGHT + dropdown::PADDING * 2,
            editor.dpi,
        ))
    .max(1);
    let content_height =
        platform::scale(editor.provider_items.len() as i32 * 32, editor.dpi).min(available);
    dropdown::position_pixels(
        editor.provider_panel,
        editor.provider_button,
        content_height,
        editor.dpi,
    );
    editor.provider_offset = 0;
    editor.provider_open = true;
    let index = editor
        .prompt
        .provider
        .and_then(|p| Provider::ALL.iter().position(|candidate| *candidate == p))
        .map_or(0, |i| i + 1);
    reveal_editor_option(editor, index);
    unsafe {
        let _ = ShowWindow(editor.provider_panel, SW_SHOW);
        let _ = SetFocus(Some(editor.provider_items[index]));
        let _ = InvalidateRect(Some(editor.provider_button), None, false);
    }
}

unsafe extern "system" fn editor_menu_scroll_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    let editor = unsafe { &mut *(data as *mut Editor) };
    match message {
        dropdown_scrollbar::VIEWPORT_GET_EXTENT => LRESULT(platform::scale(
            editor.provider_items.len() as i32 * 32 + dropdown::PADDING * 2,
            editor.dpi,
        ) as isize),
        dropdown_scrollbar::VIEWPORT_GET_OFFSET => LRESULT(editor.provider_offset as isize),
        dropdown_scrollbar::VIEWPORT_SET_OFFSET => {
            if editor.provider_offset != wparam.0 as i32 {
                editor.provider_offset = wparam.0 as i32;
                layout_editor_menu(editor);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => unsafe {
            SendMessageW(
                editor.provider_scrollbar,
                message,
                Some(wparam),
                Some(lparam),
            )
        },
        WM_NCDESTROY => unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(editor_menu_scroll_proc), id);
            DefSubclassProc(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

fn editor_provider_action(editor: &mut Editor, key: u16, origin: HWND) {
    let focus = unsafe { GetFocus() };
    if key == 0 {
        if focus != editor.provider_button && !editor.provider_items.contains(&focus) {
            close_editor_provider(editor);
        }
    } else if key == VK_TAB.0 || key == VK_ESCAPE.0 {
        close_editor_provider(editor);
        unsafe {
            let target = if key == VK_TAB.0 {
                GetNextDlgTabItem(
                    editor.hwnd,
                    Some(editor.provider_button),
                    GetKeyState(VK_SHIFT.0 as i32) < 0,
                )
                .unwrap_or(editor.provider_button)
            } else {
                editor.provider_button
            };
            let _ = SetFocus(Some(target));
        }
    } else if !editor.provider_open {
        editor_provider_menu(editor);
    } else if key == VK_RETURN.0 {
        if let Some(index) = editor
            .provider_items
            .iter()
            .position(|item| *item == origin)
        {
            choose_editor_provider(editor, index);
        }
    } else {
        let current = editor
            .provider_items
            .iter()
            .position(|item| *item == focus)
            .unwrap_or(0);
        let next = (current
            + if key == VK_UP.0 {
                editor.provider_items.len() - 1
            } else {
                1
            })
            % editor.provider_items.len();
        reveal_editor_option(editor, next);
        unsafe {
            let _ = SetFocus(Some(editor.provider_items[next]));
        }
    }
}

fn choose_editor_provider(editor: &mut Editor, index: usize) {
    close_editor_provider(editor);
    cancel_editor_test(editor);
    editor.prompt.provider = index
        .checked_sub(1)
        .and_then(|index| Provider::ALL.get(index).copied());
    unsafe {
        let _ = SetWindowTextW(
            editor.provider_button,
            PCWSTR(wide(editor.provider_label()).as_ptr()),
        );
        let _ = SetFocus(Some(editor.provider_button));
    }
    resize_editor(editor, None);
    reveal_editor_control(editor, editor.provider_button);
}

fn read_editor_prompt(editor: &Editor) -> RewritePrompt {
    let mut prompt = editor.prompt.clone();
    prompt.title = read_text(editor.fields[0]).trim().into();
    prompt.prompt = read_text(editor.fields[1]);
    prompt.extra_config = read_text(editor.fields[2]).trim().into();
    prompt.hotkey = hotkeys::value(editor.fields[3]).unwrap_or_default();
    prompt.base_url = read_text(editor.api_fields[0]).trim().into();
    prompt.api_key = read_text(editor.api_fields[1]).trim().into();
    prompt.model = read_text(editor.api_fields[2]).trim().into();
    prompt
}

fn set_editor_test_result(editor: &mut Editor, status: TestResultState, message: &str) {
    editor.test_result_state = status;
    // Lay out at the maximum height first so the native EDIT control computes
    // wrapped line count against its final width even when it was hidden.
    editor.test_result_height = TEST_RESULT_MAX_HEIGHT;
    resize_editor(editor, None);
    unsafe {
        let _ = SetWindowTextW(editor.test_status, PCWSTR(wide(message).as_ptr()));
        SendMessageW(
            editor.test_status,
            windows::Win32::UI::Controls::EM_SETSEL,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        );
        SendMessageW(
            editor.test_status,
            windows::Win32::UI::Controls::EM_SCROLLCARET,
            None,
            None,
        );
    }
    editor.test_result_height = test_result_height(editor.test_status);
    resize_editor(editor, None);
}

fn clear_editor_test_result(editor: &mut Editor) {
    editor.test_result_state = TestResultState::Idle;
    editor.test_result_height = 0;
    unsafe {
        let _ = SetWindowTextW(editor.test_status, w!(""));
    }
    resize_editor(editor, None);
}

fn cancel_editor_test(editor: &mut Editor) {
    editor.test_cancel.cancel();
    editor.test_receiver = None;
    unsafe {
        let _ = KillTimer(Some(editor.hwnd), EDITOR_TEST_TIMER);
        let _ = EnableWindow(editor.buttons[0], true);
        let _ = EnableWindow(editor.buttons[2], true);
    }
    clear_editor_test_result(editor);
}

fn start_editor_test(editor: &mut Editor) {
    if !editor.independent() || editor.test_receiver.is_some() {
        return;
    }
    let prompt = read_editor_prompt(editor);
    let mut config = editor.config.clone();
    let applied = editor.runtime.config();
    config.ffmpeg_debug = applied.ffmpeg_debug;
    config.record_debug = applied.record_debug;
    config.hotkey_debug = applied.hotkey_debug;
    config.upload_debug = applied.upload_debug;
    let (tx, rx) = std::sync::mpsc::channel();
    editor.test_cancel = tokio_util::sync::CancellationToken::new();
    editor.test_receiver = Some(rx);
    let external = editor.test_cancel.clone();
    let runtime = editor.runtime.clone();
    let testing_message = editor.language.text("testing_connectivity").to_owned();
    set_editor_test_result(editor, TestResultState::Testing, &testing_message);
    unsafe {
        let _ = EnableWindow(editor.buttons[0], false);
        let _ = EnableWindow(editor.buttons[2], false);
        SetTimer(Some(editor.hwnd), EDITOR_TEST_TIMER, 50, None);
    }
    reveal_editor_control(editor, editor.test_status);
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = (|| -> Result<(), String> {
            let executor = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            executor.block_on(
                runtime.test_connection(true, external, |cancel| async move {
                    let client = dictate_core::rewrite::RewriteClient::new(config)
                        .map_err(|e| e.to_string())?;
                    client
                        .test_prompt_connection(&prompt, &cancel)
                        .await
                        .map_err(|e| e.to_string())
                }),
            )
        })();
        let _ = tx.send((result, started.elapsed().as_millis()));
    });
}

fn poll_editor_test(editor: &mut Editor) {
    let Some(receiver) = &editor.test_receiver else {
        return;
    };
    let result = match receiver.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            (Err("Connectivity worker stopped".into()), 0)
        }
    };
    editor.test_receiver = None;
    let (status, message) = match result.0 {
        Ok(()) => (
            TestResultState::Succeeded,
            format!(
                "{} ({} ms)",
                editor.language.text("connectivity_success"),
                result.1
            ),
        ),
        Err(error) => (
            TestResultState::Failed,
            format!(
                "{}{}",
                editor.language.text("connectivity_failed"),
                editor.language.text(&error)
            ),
        ),
    };
    unsafe {
        let _ = KillTimer(Some(editor.hwnd), EDITOR_TEST_TIMER);
        let _ = EnableWindow(editor.buttons[0], true);
        let _ = EnableWindow(editor.buttons[2], true);
    }
    set_editor_test_result(editor, status, &message);
}

unsafe extern "system" fn editor_focus_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    unsafe {
        if message == WM_SETFOCUS {
            let _ = PostMessageW(
                Some(HWND(data as *mut c_void)),
                WM_EDITOR_REVEAL,
                WPARAM(0),
                LPARAM(hwnd.0 as isize),
            );
        } else if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(editor_focus_proc), id);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

unsafe extern "system" fn editor_body_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        unsafe {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Editor;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let editor = unsafe { &mut *pointer };
    match message {
        dropdown_scrollbar::VIEWPORT_GET_EXTENT => {
            LRESULT(platform::scale(editor.content_height(), editor.dpi) as isize)
        }
        dropdown_scrollbar::VIEWPORT_GET_OFFSET => LRESULT(editor.offset as isize),
        dropdown_scrollbar::VIEWPORT_SET_OFFSET => {
            if editor.offset != wparam.0 as i32 {
                editor.offset = wparam.0 as i32;
                close_editor_provider(editor);
                icon_picker::close(editor, true);
                layout_editor_body(editor);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => unsafe {
            SendMessageW(editor.scrollbar, message, Some(wparam), Some(lparam))
        },
        WM_COMMAND | WM_DRAWITEM | WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            SendMessageW(editor.hwnd, message, Some(wparam), Some(lparam))
        },
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            FillRect(dc, &client, editor.background);
            for item in &editor.layouts {
                if item.independent_only && !editor.independent() {
                    continue;
                }
                if item.hwnd == editor.test_status {
                    if editor_test_result_visible(editor) {
                        let mut layout = editor_test_result_layout_in_body(editor);
                        for rect in [&mut layout.icon, &mut layout.frame, &mut layout.editor] {
                            rect.top -= editor.offset;
                            rect.bottom -= editor.offset;
                        }
                        paint_test_result(
                            dc,
                            editor.dpi,
                            editor.test_result_state,
                            layout,
                            GetFocus() == editor.test_status,
                        );
                    }
                    continue;
                }
                let Some(frame) = item.frame else {
                    continue;
                };
                let mut rect = editor.content_rect(frame, item.after_api);
                rect.top -= editor.offset;
                rect.bottom -= editor.offset;
                rounded_box(
                    dc,
                    rect,
                    rgb(26, 35, 39),
                    if GetFocus() == item.hwnd {
                        rgb(92, 192, 176)
                    } else {
                        rgb(54, 68, 74)
                    },
                    platform::scale(7, editor.dpi),
                );
            }
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn draw_editor_button(editor: &Editor, item: &DRAWITEMSTRUCT) {
    let id = item.CtlID as usize;
    if id == ID_EDITOR_ICON_PREVIEW
        || (ID_EDITOR_ICON_FIRST..ID_EDITOR_ICON_FIRST + editor.icon_buttons.len()).contains(&id)
    {
        let icon = if id == ID_EDITOR_ICON_PREVIEW {
            editor.prompt.icon.clone()
        } else {
            palette_icon(id - ID_EDITOR_ICON_FIRST)
        };
        let selected = (icon == editor.prompt.icon
            || editor.prompt.icon == PromptIcon::Default
                && icon == PromptIcon::Builtin(BuiltinPromptIcon::Chat))
            && id != ID_EDITOR_ICON_PREVIEW;
        let focused = item.itemState.0 & (ODS_FOCUS.0 | ODS_SELECTED.0) != 0
            || dropdown::hovered(item.hwndItem);
        let available_size = ((item.rcItem.right - item.rcItem.left)
            .min(item.rcItem.bottom - item.rcItem.top)
            - platform::scale(8, editor.dpi))
        .max(1);
        let size = platform::scale(
            if id == ID_EDITOR_ICON_PREVIEW { 36 } else { 28 },
            editor.dpi,
        )
        .min(available_size) as u32;
        let pixels =
            editor
                .icon_cache
                .borrow_mut()
                .pixels(&icon, size, true, Some(&editor.icon_draft));
        unsafe {
            if id == ID_EDITOR_ICON_PREVIEW {
                fill_color(item.hDC, item.rcItem, rgb(16, 22, 25));
                rounded_box(
                    item.hDC,
                    item.rcItem,
                    rgb(24, 33, 37),
                    rgb(54, 68, 74),
                    platform::scale(7, editor.dpi),
                );
            } else {
                dropdown::draw_row(item.hDC, item.rcItem, selected, focused, editor.dpi);
            }
        }
        let x = item.rcItem.left + (item.rcItem.right - item.rcItem.left - pixels.size as i32) / 2;
        let y = item.rcItem.top + (item.rcItem.bottom - item.rcItem.top - pixels.size as i32) / 2;
        let _ = crate::prompt_icons::draw_gdi(item.hDC, x, y, &pixels);
        return;
    }
    if id == ID_SAVE || id == ID_CANCEL {
        unsafe { draw_dialog_button(item, editor.font, editor.dpi) };
        return;
    }
    if id == ID_EDITOR_KEY_CLEAR {
        unsafe { draw_action_button(item, editor.font, editor.dpi) };
        return;
    }
    let focused = item.itemState.0 & (ODS_FOCUS.0 | ODS_SELECTED.0) != 0;
    let hot = focused || dropdown::hovered(item.hwndItem);
    let mut rect = item.rcItem;
    let option = (ID_EDITOR_OPTION..ID_EDITOR_OPTION + editor.provider_items.len()).contains(&id);
    if option {
        let provider = (id - ID_EDITOR_OPTION)
            .checked_sub(1)
            .map(|i| Provider::ALL[i]);
        dropdown::draw_row(
            item.hDC,
            rect,
            provider == editor.prompt.provider,
            hot,
            editor.dpi,
        );
    } else {
        let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
        unsafe {
            fill_color(item.hDC, rect, rgb(16, 22, 25));
        }
        unsafe {
            rounded_box(
                item.hDC,
                rect,
                if id == ID_EDITOR_PROVIDER {
                    rgb(26, 35, 39)
                } else {
                    rgb(16, 22, 25)
                },
                if disabled {
                    rgb(48, 61, 66)
                } else if hot {
                    rgb(82, 192, 176)
                } else {
                    rgb(56, 84, 86)
                },
                platform::scale(7, editor.dpi),
            );
        }
    }
    rect.left += platform::scale(12, editor.dpi);
    rect.right -= platform::scale(if id == ID_EDITOR_PROVIDER { 32 } else { 12 }, editor.dpi);
    unsafe {
        draw_text_line(
            item.hDC,
            editor.font,
            &read_text(item.hwndItem),
            if item.itemState.0 & ODS_DISABLED.0 != 0 {
                rgb(107, 123, 126)
            } else {
                rgb(230, 240, 240)
            },
            rect,
            if matches!(
                id,
                ID_EDITOR_TEST | ID_EDITOR_ICON_SELECT | ID_EDITOR_ICON_IMPORT
            ) {
                DT_CENTER
            } else {
                DT_LEFT
            },
        );
        if id == ID_EDITOR_PROVIDER {
            let x = item.rcItem.right - platform::scale(17, editor.dpi);
            let y = (item.rcItem.top + item.rcItem.bottom) / 2;
            let direction = if editor.provider_open { -1 } else { 1 };
            stroke_polyline(
                item.hDC,
                &[
                    POINT {
                        x: x - platform::scale(4, editor.dpi),
                        y: y - direction * platform::scale(2, editor.dpi),
                    },
                    POINT {
                        x,
                        y: y + direction * platform::scale(2, editor.dpi),
                    },
                    POINT {
                        x: x + platform::scale(4, editor.dpi),
                        y: y - direction * platform::scale(2, editor.dpi),
                    },
                ],
                rgb(185, 207, 207),
                platform::scale(1, editor.dpi).max(1),
            );
        }
    }
}

unsafe extern "system" fn editor_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        unsafe {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let editor = create.lpCreateParams.cast::<Editor>();
            (*editor).hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, editor as isize);
        }
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Editor;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let editor = unsafe { &mut *pointer };
    match message {
        WM_CREATE => {
            if let Err(error) = create_editor(editor) {
                show_error(hwnd, &error);
                return LRESULT(-1);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            close_editor_provider(editor);
            icon_picker::close(editor, true);
            let old_dpi = editor.dpi;
            editor.dpi = ((wparam.0 >> 16) & 0xffff) as u32;
            editor.offset =
                (i64::from(editor.offset) * i64::from(editor.dpi) / i64::from(old_dpi)) as i32;
            let old_font = editor.font;
            editor.font = create_font(editor.dpi, 14, false);
            for child in editor
                .layouts
                .iter()
                .map(|item| item.hwnd)
                .chain(editor.buttons)
                .chain(editor.provider_items.iter().copied())
                .chain(editor.icon_buttons.iter().copied())
            {
                set_font(child, editor.font);
            }
            resize_editor(editor, Some(unsafe { *(lparam.0 as *const RECT) }));
            sync_editor_frame(editor, true);
            unsafe {
                let _ = DeleteObject(HGDIOBJ(old_font.0));
            }
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            icon_picker::close(editor, true);
            let resized = unsafe { (*(lparam.0 as *const WINDOWPOS)).flags.0 & SWP_NOSIZE.0 == 0 };
            let result = unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            if resized {
                layout_editor(editor);
                apply_settings_region(hwnd, editor.dpi);
            }
            sync_editor_frame(editor, resized);
            result
        }
        WM_NCHITTEST => {
            let mut point = POINT {
                x: (lparam.0 as u16) as i16 as i32,
                y: ((lparam.0 as u32 >> 16) as u16) as i16 as i32,
            };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut point);
            }
            LRESULT(if point.y < platform::scale(34, editor.dpi) {
                HTCAPTION
            } else {
                HTCLIENT
            } as isize)
        }
        WM_EDITOR_REVEAL => {
            let control = HWND(lparam.0 as *mut c_void);
            if unsafe { GetFocus() } == control {
                reveal_editor_control(editor, control);
            }
            LRESULT(0)
        }
        WM_PROVIDER_ACTION => {
            editor_provider_action(editor, wparam.0 as u16, HWND(lparam.0 as *mut c_void));
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == EDITOR_TEST_TIMER => {
            poll_editor_test(editor);
            LRESULT(0)
        }
        WM_COMMAND => {
            let notification = (wparam.0 >> 16) as u32;
            let id = wparam.0 & 0xffff;
            if notification == EN_KILLFOCUS && lparam.0 == editor.fields[2].0 as isize {
                format_json_input(editor.fields[2]);
            }
            if notification == EN_CHANGE
                && (ID_EDITOR_API..ID_EDITOR_API + 3).contains(&id)
                && !editor.buttons[0].is_invalid()
            {
                cancel_editor_test(editor);
            }
            if matches!(notification, EN_SETFOCUS | EN_KILLFOCUS) {
                unsafe {
                    let _ = InvalidateRect(Some(editor.body), None, true);
                }
            }
            if notification == BN_CLICKED {
                if (ID_EDITOR_ICON_FIRST..ID_EDITOR_ICON_FIRST + editor.icon_buttons.len())
                    .contains(&id)
                {
                    icon_picker::select(editor, id - ID_EDITOR_ICON_FIRST);
                } else if id == ID_EDITOR_ICON_SELECT {
                    icon_picker::toggle(editor);
                } else if id == ID_EDITOR_ICON_IMPORT {
                    icon_picker::close(editor, false);
                    if let Err(error) = import_icon(editor) {
                        show_error(hwnd, &error);
                    }
                } else if id == ID_EDITOR_KEY_CLEAR {
                    hotkeys::clear(editor.fields[3]);
                } else if id == ID_EDITOR_PROVIDER {
                    editor_provider_menu(editor);
                } else if (ID_EDITOR_OPTION..ID_EDITOR_OPTION + editor.provider_items.len())
                    .contains(&id)
                {
                    choose_editor_provider(editor, id - ID_EDITOR_OPTION);
                } else if id == ID_EDITOR_TEST {
                    start_editor_test(editor);
                } else if id == ID_CANCEL {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                } else if id == ID_SAVE && editor.test_receiver.is_none() {
                    let prompt = read_editor_prompt(editor);
                    let mut config = editor.config.clone();
                    config.rewrite.prompts.retain(|p| p.id != prompt.id);
                    config.rewrite.prompts.push(prompt.clone());
                    let validation = config
                        .rewrite
                        .validate_prompts()
                        .map_err(|error| error.to_string())
                        .and_then(|()| hotkeys::validate_draft(&config, editor.language));
                    match validation {
                        Ok(()) => {
                            editor.result = Some(prompt);
                            unsafe {
                                let _ = DestroyWindow(hwnd);
                            }
                        }
                        Err(error) => show_error(hwnd, &error),
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            FillRect(dc, &rect, editor.background);
            let footer_top = rect.bottom - platform::scale(FOOTER_HEIGHT, editor.dpi);
            fill_color(
                dc,
                RECT {
                    top: footer_top,
                    ..rect
                },
                rgb(13, 19, 22),
            );
            line(dc, 0, footer_top, rect.right, footer_top, rgb(38, 49, 54));
            draw_text_line(
                dc,
                editor.font,
                editor.language.text("rewrite_edit"),
                rgb(230, 240, 240),
                scaled_rect(
                    RECT {
                        left: 24,
                        top: 8,
                        right: 676,
                        bottom: 34,
                    },
                    editor.dpi,
                ),
                DT_LEFT,
            );
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        },
        WM_DRAWITEM => {
            unsafe {
                draw_editor_button(editor, &*(lparam.0 as *const DRAWITEMSTRUCT));
            }
            LRESULT(1)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => unsafe {
            let dc = HDC(wparam.0 as *mut c_void);
            let child = HWND(lparam.0 as *mut c_void);
            SetTextColor(
                dc,
                if child == editor.test_status {
                    test_result_text_color(editor.test_result_state)
                } else {
                    rgb(230, 240, 240)
                },
            );
            let edit = editor.fields.contains(&child)
                || editor.api_fields.contains(&child)
                || child == editor.test_status;
            SetBkColor(
                dc,
                if edit {
                    rgb(26, 35, 39)
                } else {
                    rgb(16, 22, 25)
                },
            );
            LRESULT(
                if edit {
                    editor.input
                } else {
                    editor.background
                }
                .0 as isize,
            )
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_ACTIVATE => {
            if wparam.0 & 0xffff == 0 {
                close_editor_provider(editor);
                icon_picker::close(editor, false);
                editor.icon_key_latch = None;
                hotkeys::deactivate();
            } else {
                hotkeys::activate(unsafe { GetFocus() });
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_CLOSE => unsafe {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        },
        WM_DESTROY => {
            icon_picker::close(editor, false);
            editor.test_cancel.cancel();
            editor.test_receiver = None;
            hotkeys::deactivate();
            unsafe {
                let _ = KillTimer(Some(hwnd), EDITOR_TEST_TIMER);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
