//! The prompt icon palette shares the settings dropdown surface and owner drawing.
use super::*;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LEFT, VK_RIGHT, VK_SPACE};

const COLUMNS: usize = 5;
const CELL: i32 = 44;
const GAP: i32 = 4;

pub(super) fn create(editor: &mut Editor, instance: HMODULE) -> Result<(), String> {
    editor.icon_panel = dropdown::create_panel_for(editor.hwnd, editor.dpi, instance)?;
    for index in 0..BuiltinPromptIcon::ALL.len() {
        let item = editor_child(
            editor,
            editor.icon_panel,
            w!("BUTTON"),
            editor.language.text(icon_label(&palette_icon(index))),
            WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            ID_EDITOR_ICON_FIRST + index,
        )?;
        dropdown::track_hover(item, false)?;
        editor.icon_buttons.push(item);
    }
    Ok(())
}

pub(super) fn close(editor: &mut Editor, return_focus: bool) {
    if !editor.icon_open {
        return;
    }
    editor.icon_open = false;
    unsafe {
        let focused_item = editor.icon_buttons.contains(&GetFocus());
        let _ = ShowWindow(editor.icon_panel, SW_HIDE);
        if return_focus && focused_item {
            let _ = SetFocus(Some(editor.icon_selector));
        }
        let _ = InvalidateRect(Some(editor.icon_selector), None, false);
    }
}

pub(super) fn select(editor: &mut Editor, index: usize) {
    if index < editor.icon_buttons.len() {
        choose_icon(editor, palette_icon(index));
        close(editor, true);
    }
}

pub(super) fn toggle(editor: &mut Editor) {
    if editor.icon_open {
        close(editor, true);
        return;
    }
    close_editor_provider(editor);
    reveal_editor_control(editor, editor.icon_selector);
    let mut client = RECT::default();
    let mut anchor = RECT::default();
    unsafe {
        if GetClientRect(editor.hwnd, &mut client).is_err()
            || GetWindowRect(editor.icon_selector, &mut anchor).is_err()
        {
            return;
        }
    }
    let mut origin = POINT {
        x: anchor.left,
        y: anchor.bottom,
    };
    unsafe {
        let _ = ScreenToClient(editor.hwnd, &mut origin);
    }
    let padding = platform::scale(dropdown::PADDING, editor.dpi);
    let margin = platform::scale(8, editor.dpi);
    let top_limit = platform::scale(EDITOR_HEADER, editor.dpi) + margin;
    let bottom_limit = client.bottom - platform::scale(FOOTER_HEIGHT, editor.dpi) - margin;
    let full_inner = platform::scale(COLUMNS as i32 * CELL - GAP, editor.dpi);
    let size = (full_inner + padding * 2)
        .min(client.right - margin * 2)
        .min(bottom_limit - top_limit);
    if size <= padding * 2 + COLUMNS as i32 {
        return;
    }
    let left = origin
        .x
        .clamp(margin, (client.right - margin - size).max(margin));
    let below = origin.y + platform::scale(4, editor.dpi);
    let top = if below + size <= bottom_limit {
        below
    } else {
        origin.y - (anchor.bottom - anchor.top) - size - platform::scale(4, editor.dpi)
    }
    .clamp(top_limit, (bottom_limit - size).max(top_limit));
    // Initialize the shared surface's DPI, then give the matrix its own width.
    dropdown::position_pixels(
        editor.icon_panel,
        editor.icon_selector,
        size - padding * 2,
        editor.dpi,
    );
    unsafe {
        let _ = SetWindowPos(
            editor.icon_panel,
            Some(HWND_TOP),
            left,
            top,
            size,
            size,
            SWP_NOACTIVATE,
        );
    }
    let inner = size - padding * 2;
    let coordinate = |value| {
        padding
            + (i64::from(platform::scale(value, editor.dpi)) * i64::from(inner)
                / i64::from(full_inner)) as i32
    };
    position_editor_controls(editor.icon_buttons.iter().enumerate().map(|(index, item)| {
        let x = (index % COLUMNS) as i32 * CELL;
        let y = (index / COLUMNS) as i32 * CELL;
        (
            *item,
            RECT {
                left: coordinate(x),
                top: coordinate(y),
                right: coordinate(x + CELL - GAP),
                bottom: coordinate(y + CELL - GAP),
            },
            true,
        )
    }));
    let index = editor
        .icon_buttons
        .iter()
        .enumerate()
        .position(|(index, _)| palette_icon(index) == editor.prompt.icon)
        .unwrap_or(0);
    editor.icon_open = true;
    unsafe {
        let _ = ShowWindow(editor.icon_panel, SW_SHOW);
        let _ = SetFocus(Some(editor.icon_buttons[index]));
        let _ = InvalidateRect(Some(editor.icon_selector), None, false);
    }
    redraw_editor_controls(editor.icon_panel);
}

fn contains_screen_point(hwnd: HWND, point: POINT) -> bool {
    let mut rect = RECT::default();
    (unsafe { GetWindowRect(hwnd, &mut rect).is_ok() })
        && point.x >= rect.left
        && point.x < rect.right
        && point.y >= rect.top
        && point.y < rect.bottom
}

/// Handles the popup before IsDialogMessage so Escape cannot close the editor.
/// Outside clicks are only observed; the original message is still dispatched.
pub(super) fn filter_message(editor: &mut Editor, message: &MSG) -> bool {
    let key = message.wParam.0 as u16;
    if editor.icon_key_latch == Some(key) {
        if message.message == WM_KEYUP {
            editor.icon_key_latch = None;
            return true;
        }
        if message.message == WM_KEYDOWN {
            return true;
        }
    }
    let focus = unsafe { GetFocus() };
    let current = editor.icon_buttons.iter().position(|item| *item == focus);
    if editor.icon_open {
        if message.message == WM_KEYDOWN && key == VK_ESCAPE.0 {
            close(editor, true);
            editor.icon_key_latch = Some(key);
            return true;
        }
        if matches!(
            message.message,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_NCLBUTTONDOWN
                | WM_NCRBUTTONDOWN
                | WM_NCMBUTTONDOWN
                | WM_NCXBUTTONDOWN
        ) && !contains_screen_point(editor.icon_panel, message.pt)
            && !contains_screen_point(editor.icon_selector, message.pt)
        {
            close(editor, true);
            return false;
        }
        if current.is_none() && focus != editor.icon_selector {
            close(editor, false);
            return false;
        }
    }
    if message.message != WM_KEYDOWN || (current.is_none() && focus != editor.icon_selector) {
        return false;
    }
    if !editor.icon_open {
        if [VK_DOWN.0, VK_UP.0, VK_RETURN.0, VK_SPACE.0].contains(&key) {
            toggle(editor);
            if key == VK_RETURN.0 || key == VK_SPACE.0 {
                editor.icon_key_latch = Some(key);
            }
            return true;
        }
        return false;
    }
    let current = current.unwrap_or(0);
    if key == VK_RETURN.0 || key == VK_SPACE.0 {
        select(editor, current);
        editor.icon_key_latch = Some(key);
        return true;
    }
    let count = editor.icon_buttons.len();
    let next = if key == VK_TAB.0 {
        let backwards = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        if (!backwards && current + 1 == count) || (backwards && current == 0) {
            close(editor, true);
            unsafe {
                let next = GetNextDlgTabItem(editor.hwnd, Some(editor.icon_selector), backwards)
                    .unwrap_or(editor.icon_selector);
                let _ = SetFocus(Some(next));
            }
            return true;
        }
        if backwards { current - 1 } else { current + 1 }
    } else if key == VK_LEFT.0 {
        current / COLUMNS * COLUMNS + (current % COLUMNS + COLUMNS - 1) % COLUMNS
    } else if key == VK_RIGHT.0 {
        current / COLUMNS * COLUMNS + (current % COLUMNS + 1) % COLUMNS
    } else if key == VK_UP.0 {
        (current + count - COLUMNS) % count
    } else if key == VK_DOWN.0 {
        (current + COLUMNS) % count
    } else {
        return false;
    };
    unsafe {
        let _ = SetFocus(Some(editor.icon_buttons[next]));
    }
    true
}
