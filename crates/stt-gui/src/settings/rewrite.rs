//! Rewrite settings and the prompt editor share the settings draft and theme.
use super::*;
use stt_core::rewrite::{Provider, RewriteConfig, RewritePrompt};
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
const ID_OPTION: usize = 0x6820;
const WM_PROVIDER_ACTION: u32 = WM_APP + 86;
const EDITOR_WIDTH: i32 = 700;
const EDITOR_HEIGHT: i32 = 610;
const PROMPTS_TOP: i32 = 304;
const PROMPTS_HEIGHT: i32 = 132;

pub fn handles_escape(id: usize) -> bool {
    id == ID_PROVIDER || (ID_OPTION..ID_OPTION + Provider::ALL.len()).contains(&id)
}

pub fn resize_list(state: &SettingsState) {
    let scale = |value| platform::scale(value, state.dpi);
    let right = scale(EDIT_LEFT + EDIT_WIDTH);
    let scrollbar_left = right - dropdown_scrollbar::thickness(state.dpi);
    unsafe {
        // Keep a stable gutter; the shared bar draws no thumb for a short list.
        let _ = SetWindowPos(
            state.rewrite.list,
            None,
            scale(CONTENT_LEFT),
            scale(PROMPTS_TOP),
            scrollbar_left - scale(CONTENT_LEFT),
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
            top: scale(PROMPTS_TOP),
            right,
            bottom: scale(PROMPTS_TOP) + scale(PROMPTS_HEIGHT),
        },
        state.dpi,
    );
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
    list: HWND,
    scrollbar: HWND,
}

impl Page {
    pub fn new(config: RewriteConfig) -> Self {
        Self {
            config,
            provider_open: false,
            provider_items: vec![],
            panel: HWND::default(),
            list: HWND::default(),
            scrollbar: HWND::default(),
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
        564,
        536,
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
    refresh_list(state, 0);
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
    config
}

pub fn refresh_labels(state: &SettingsState) {
    for key in hotkeys::KEYS {
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
    let width = dropdown::position(
        state.rewrite.panel,
        state.controls["rewrite_provider"],
        Provider::ALL.len() as i32 * 32,
        state.dpi,
    );
    for (index, item) in state.rewrite.provider_items.iter().enumerate() {
        unsafe {
            let _ = SetWindowPos(
                *item,
                None,
                platform::scale(8, state.dpi),
                platform::scale(8 + index as i32 * 32, state.dpi),
                width,
                platform::scale(32, state.dpi),
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(*item, SW_SHOW);
        }
    }
    state.rewrite.provider_open = true;
    unsafe {
        let _ = ShowWindow(state.rewrite.panel, SW_SHOW);
        let index = Provider::ALL
            .iter()
            .position(|p| *p == state.rewrite.config.provider)
            .unwrap_or(0);
        let _ = SetFocus(Some(state.rewrite.provider_items[index]));
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
    for key in hotkeys::KEYS {
        if let Some(hwnd) = state.controls.get(key) {
            hotkeys::set_context(
                *hwnd,
                state
                    .rewrite
                    .config
                    .prompts
                    .iter()
                    .map(|p| (p.title.clone(), p.hotkey.clone()))
                    .collect(),
                state
                    .boolean_values
                    .get("HOTKEY_HOOK")
                    .copied()
                    .unwrap_or(true),
            );
        }
    }
}

pub fn command(state: &mut SettingsState, id: usize, notification: u32) -> bool {
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
        let result = read_config(state)
            .and_then(|config| edit_prompt(owner, state.language, state.dpi, config, original));
        if !unsafe { IsWindow(Some(owner)) }.as_bool() {
            return true;
        }
        match result {
            Ok(Some(prompt)) => {
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

struct Editor {
    hwnd: HWND,
    language: Language,
    dpi: u32,
    font: HFONT,
    background: HBRUSH,
    input: HBRUSH,
    fields: [HWND; 4],
    status: HWND,
    config: Config,
    prompt: RewritePrompt,
    result: Option<RewritePrompt>,
    controls: Vec<(HWND, RECT)>,
    layouts: Vec<(HWND, RECT)>,
    frame_overlay: Option<SettingsFrameOverlay>,
}

fn edit_prompt(
    owner: HWND,
    language: Language,
    dpi: u32,
    config: Config,
    prompt: RewritePrompt,
) -> Result<Option<RewritePrompt>, String> {
    let instance = unsafe { GetModuleHandleW(None).map_err(|e| e.to_string())? };
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(editor_proc),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())? },
        lpszClassName: w!("STTRewritePromptEditor"),
        ..Default::default()
    };
    unsafe {
        RegisterClassExW(&class);
    }
    let mut editor = Box::new(Editor {
        hwnd: HWND::default(),
        language,
        dpi,
        font: create_font(dpi, 14, false),
        background: unsafe { CreateSolidBrush(rgb(16, 22, 25)) },
        input: unsafe { CreateSolidBrush(rgb(26, 35, 39)) },
        fields: [HWND::default(); 4],
        status: HWND::default(),
        config,
        prompt,
        result: None,
        controls: vec![],
        layouts: vec![],
        frame_overlay: None,
    });
    let pointer: *mut Editor = &mut *editor;
    let mut bounds = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner, &mut bounds);
    }
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STTRewritePromptEditor"),
            PCWSTR(wide(language.text("rewrite_edit")).as_ptr()),
            WS_POPUP | WS_CLIPCHILDREN,
            bounds.left + platform::scale(30, dpi),
            bounds.top + platform::scale(20, dpi),
            platform::scale(EDITOR_WIDTH, dpi),
            platform::scale(EDITOR_HEIGHT, dpi),
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
                if let Some(frame) = &mut editor.frame_overlay {
                    frame.sync(hwnd, true);
                }
                let _ = SetFocus(Some(editor.fields[0]));
                let mut message = MSG::default();
                while IsWindow(Some(hwnd)).as_bool() {
                    let code = GetMessageW(&mut message, None, 0, 0).0;
                    if code <= 0 {
                        if code == 0 {
                            PostQuitMessage(message.wParam.0 as i32);
                        }
                        break;
                    }
                    if message.message == WM_KEYDOWN
                        && message.wParam.0 == VK_ESCAPE.0 as usize
                        && hotkeys::value(GetFocus()).is_none()
                    {
                        let _ = DestroyWindow(hwnd);
                        continue;
                    }
                    // Edit controls retain Enter for multiline text; the recorded shortcut
                    // field handles its own navigation and suppresses dialog commands.
                    if hotkeys::value(GetFocus()).is_some()
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
            Ok(editor.result.take())
        }
        Err(error) => Err(error.to_string()),
    };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(editor.font.0));
        let _ = DeleteObject(HGDIOBJ(editor.background.0));
        let _ = DeleteObject(HGDIOBJ(editor.input.0));
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn editor_child(
    editor: &mut Editor,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    rect: RECT,
    id: usize,
) -> Result<HWND, String> {
    let dpi = editor.dpi;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR(wide(text).as_ptr()),
            WS_CHILD | WS_VISIBLE | style,
            platform::scale(rect.left, dpi),
            platform::scale(rect.top, dpi),
            platform::scale(rect.right - rect.left, dpi),
            platform::scale(rect.bottom - rect.top, dpi),
            Some(editor.hwnd),
            (id != 0).then_some(HMENU(id as *mut c_void)),
            None,
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    set_font(hwnd, editor.font);
    editor.layouts.push((hwnd, rect));
    Ok(hwnd)
}

fn create_editor(editor: &mut Editor) -> Result<(), String> {
    let values = [
        editor.prompt.title.clone(),
        editor.prompt.prompt.clone(),
        editor.prompt.extra_config.clone(),
        editor.prompt.hotkey.clone(),
    ];
    for (index, (label, y, height)) in [
        ("rewrite_title", 66, 34),
        ("rewrite_content", 138, 132),
        ("ExtraConfig", 310, 106),
        ("rewrite_hotkey", 456, 34),
    ]
    .into_iter()
    .enumerate()
    {
        editor_child(
            editor,
            w!("STATIC"),
            editor.language.text(label),
            WINDOW_STYLE(0),
            RECT {
                left: 24,
                top: y - 26,
                right: 676,
                bottom: y - 4,
            },
            0,
        )?;
        let multiline = index == 1 || index == 2;
        let style = WS_TABSTOP
            | WINDOW_STYLE(if multiline {
                (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32 | WS_CLIPCHILDREN.0
            } else {
                ES_AUTOHSCROLL as u32
            });
        let rect = RECT {
            left: 24,
            top: y,
            right: 676,
            bottom: y + height,
        };
        let input_rect = RECT {
            left: 34,
            top: y + 6,
            right: 666,
            bottom: y + height - 6,
        };
        let field = editor_child(
            editor,
            w!("EDIT"),
            &values[index],
            style,
            input_rect,
            0x6900 + index,
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
        editor.controls.push((field, rect));
    }
    editor.status = editor_child(
        editor,
        w!("STATIC"),
        editor.language.text("hotkey_help"),
        WINDOW_STYLE(0),
        RECT {
            left: 24,
            top: 500,
            right: 676,
            bottom: 546,
        },
        0,
    )?;
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
    hotkeys::attach(
        editor.fields[3],
        editor.prompt.hotkey.clone(),
        editor.language,
        editor.status,
    )?;
    hotkeys::set_context(editor.fields[3], context, editor.config.hotkey_hook);
    for (id, key, left, right) in [
        (ID_SAVE, "save", EDITOR_WIDTH - 110, EDITOR_WIDTH - 16),
        (ID_CANCEL, "cancel", EDITOR_WIDTH - 208, EDITOR_WIDTH - 120),
    ] {
        let button = editor_child(
            editor,
            w!("BUTTON"),
            editor.language.text(key),
            WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            RECT {
                left,
                top: EDITOR_HEIGHT - 46,
                right,
                bottom: EDITOR_HEIGHT - 12,
            },
            id,
        )?;
        dropdown::track_hover(button, false)?;
    }
    let instance = unsafe { GetModuleHandleW(None).map_err(|error| error.to_string())? };
    editor.frame_overlay = Some(SettingsFrameOverlay::create(
        editor.hwnd,
        instance,
        editor.dpi,
        EDITOR_WIDTH,
        EDITOR_HEIGHT,
    )?);
    Ok(())
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
            editor.dpi = ((wparam.0 >> 16) & 0xffff) as u32;
            if let Some(frame) = &mut editor.frame_overlay {
                frame.set_dpi(editor.dpi);
            }
            let old = editor.font;
            editor.font = create_font(editor.dpi, 14, false);
            unsafe {
                let rect = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                for (child, rect) in &editor.layouts {
                    let rect = scaled_rect(*rect, editor.dpi);
                    let _ = SetWindowPos(
                        *child,
                        None,
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    );
                    set_font(*child, editor.font);
                }
                let _ = DeleteObject(HGDIOBJ(old.0));
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            let position = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            let resized = position.flags.0 & SWP_NOSIZE.0 == 0;
            let result = unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            if resized {
                apply_settings_region(hwnd, editor.dpi);
            }
            if let Some(frame) = &mut editor.frame_overlay {
                frame.sync(hwnd, resized);
            }
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
        WM_COMMAND => {
            let notification = (wparam.0 >> 16) as u32;
            if notification == EN_KILLFOCUS && lparam.0 == editor.fields[2].0 as isize {
                format_json_input(editor.fields[2]);
            }
            if matches!(notification, EN_SETFOCUS | EN_KILLFOCUS) {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            }
            match wparam.0 & 0xffff {
                ID_CANCEL => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                ID_SAVE => {
                    let mut prompt = editor.prompt.clone();
                    prompt.title = read_text(editor.fields[0]).trim().into();
                    prompt.prompt = read_text(editor.fields[1]);
                    prompt.extra_config = read_text(editor.fields[2]).trim().into();
                    prompt.hotkey = hotkeys::value(editor.fields[3]).unwrap_or_default();
                    let mut config = editor.config.clone();
                    config.rewrite.prompts.retain(|p| p.id != prompt.id);
                    config.rewrite.prompts.push(prompt.clone());
                    match config.validate() {
                        Ok(()) => {
                            editor.result = Some(prompt);
                            unsafe {
                                let _ = DestroyWindow(hwnd);
                            }
                        }
                        Err(error) => show_error(hwnd, &error.to_string()),
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            FillRect(hdc, &rect, editor.background);
            let footer_top = rect.bottom - platform::scale(FOOTER_HEIGHT, editor.dpi);
            fill_color(
                hdc,
                RECT {
                    top: footer_top,
                    ..rect
                },
                rgb(13, 19, 22),
            );
            line(hdc, 0, footer_top, rect.right, footer_top, rgb(38, 49, 54));
            draw_text_line(
                hdc,
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
            for (field, rect) in &editor.controls {
                rounded_box(
                    hdc,
                    scaled_rect(*rect, editor.dpi),
                    rgb(26, 35, 39),
                    if GetFocus() == *field {
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
        WM_DRAWITEM => unsafe {
            let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
            draw_dialog_button(item, editor.font, editor.dpi);
            LRESULT(1)
        },
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => unsafe {
            let hdc = HDC(wparam.0 as *mut c_void);
            SetTextColor(hdc, rgb(230, 240, 240));
            let edit = editor.fields.contains(&HWND(lparam.0 as *mut c_void));
            SetBkColor(
                hdc,
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
            hotkeys::deactivate();
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
