//! Read-only debug output for the current GUI session.
use super::*;
use windows::Win32::UI::Controls::{
    EM_GETFIRSTVISIBLELINE, EM_GETSEL, EM_LINEFROMCHAR, EM_LINEINDEX, EM_LINESCROLL,
    EM_SCROLLCARET, EM_SETLIMITTEXT, EM_SETSEL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    ES_READONLY, IsChild, IsWindowVisible, SetTimer, WM_COPY, WM_SETREDRAW,
};

pub const TIMER: usize = 0x6b00;
const ID_OUTPUT: usize = 0x6b01;
const ID_COPY: usize = 0x6b02;
const ID_CLEAR: usize = 0x6b03;

#[derive(Default)]
pub struct Page {
    view: HWND,
    font: HFONT,
    revision: u64,
    start: u64,
}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
    }
}

pub fn is_button(id: usize) -> bool {
    matches!(id, ID_COPY | ID_CLEAR)
}

pub fn create(
    state: &mut SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<(), String> {
    let title = create_label(
        state,
        state.language.text("debug_output"),
        CONTENT_LEFT,
        264,
        240,
        34,
        instance,
    )?;
    state.control_groups.insert(title.0 as usize, "Debug");
    state.localized_controls.push((title, "debug_output"));
    for (key, id, left, width) in [
        ("debug_copy", ID_COPY, EDIT_LEFT + EDIT_WIDTH - 222, 126),
        ("debug_clear", ID_CLEAR, EDIT_LEFT + EDIT_WIDTH - 86, 86),
    ] {
        let button = create_child(
            state,
            w!("BUTTON"),
            state.language.text(key),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            left,
            264,
            width,
            34,
            id,
            instance,
        )?;
        dropdown::track_hover(button, false)?;
        state.controls.insert(key, button);
        state.control_groups.insert(button.0 as usize, "Debug");
        state.localized_controls.push((button, key));
    }
    let rect = RECT {
        left: CONTENT_LEFT,
        top: 308,
        right: EDIT_LEFT + EDIT_WIDTH,
        bottom: 550,
    };
    let view = create_child(
        state,
        w!("EDIT"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_CLIPCHILDREN
            | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY) as u32),
        rect.left + 10,
        rect.top + 7,
        rect.right - rect.left - 20,
        rect.bottom - rect.top - 14,
        ID_OUTPUT,
        instance,
    )?;
    unsafe {
        SendMessageW(view, EM_SETLIMITTEXT, Some(WPARAM(1024 * 1024)), None);
    }
    apply_dark_theme(view);
    state.debug.view = view;
    resize(&mut state.debug, state.dpi);
    dropdown_scrollbar::attach_edit(view, state.dpi)?;
    state.controls.insert("__debug_output", view);
    state.control_groups.insert(view.0 as usize, "Debug");
    state.input_frames.push(InputFrame {
        rect,
        group: "Debug",
        control: view,
    });
    let hint = create_child(
        state,
        w!("STATIC"),
        state.language.text("debug_hint"),
        WS_CHILD | WS_VISIBLE,
        CONTENT_LEFT,
        560,
        536,
        38,
        0,
        instance,
    )?;
    state.control_groups.insert(hint.0 as usize, "Debug");
    state.localized_controls.push((hint, "debug_hint"));
    if unsafe { SetTimer(Some(state.hwnd), TIMER, 200, None) } == 0 {
        return Err("Unable to start debug output refresh".into());
    }
    Ok(())
}

pub fn resize(page: &mut Page, dpi: u32) {
    let previous = page.font;
    page.font = create_font_face(dpi, 12, false, w!("Consolas"));
    set_font(page.view, page.font);
    unsafe {
        let _ = DeleteObject(HGDIOBJ(previous.0));
    }
}

#[derive(Clone, Copy)]
struct Position {
    selection_start: u32,
    selection_end: u32,
    top_character: u32,
}

unsafe fn position(view: HWND) -> Position {
    unsafe {
        let mut start: u32 = 0;
        let mut end: u32 = 0;
        SendMessageW(
            view,
            EM_GETSEL,
            Some(WPARAM(&mut start as *mut u32 as usize)),
            Some(LPARAM(&mut end as *mut u32 as isize)),
        );
        let line = SendMessageW(view, EM_GETFIRSTVISIBLELINE, None, None).0;
        let character = SendMessageW(view, EM_LINEINDEX, Some(WPARAM(line as usize)), None)
            .0
            .max(0) as u32;
        Position {
            selection_start: start,
            selection_end: end,
            top_character: character,
        }
    }
}

unsafe fn restore(view: HWND, position: Position) {
    unsafe {
        SendMessageW(
            view,
            EM_SETSEL,
            Some(WPARAM(position.selection_start as usize)),
            Some(LPARAM(position.selection_end as isize)),
        );
        let target = SendMessageW(
            view,
            EM_LINEFROMCHAR,
            Some(WPARAM(position.top_character as usize)),
            None,
        )
        .0;
        let current = SendMessageW(view, EM_GETFIRSTVISIBLELINE, None, None).0;
        SendMessageW(view, EM_LINESCROLL, None, Some(LPARAM(target - current)));
    }
}

pub fn refresh(page: &mut Page) {
    unsafe {
        if !IsWindowVisible(page.view).as_bool() {
            return;
        }
        let capture = GetCapture();
        // Do not disturb an active text selection or scrollbar drag.
        if capture == page.view || IsChild(page.view, capture).as_bool() {
            return;
        }
        let Some(snapshot) = crate::debug_log::snapshot(page.revision) else {
            return;
        };
        let previous = position(page.view);
        let follow = previous.selection_start == previous.selection_end
            && dropdown_scrollbar::edit_at_bottom(page.view);
        let text = wide(&snapshot.text());
        SendMessageW(page.view, WM_SETREDRAW, Some(WPARAM(0)), None);
        let _ = SetWindowTextW(page.view, PCWSTR(text.as_ptr()));
        if follow {
            let end = text.len().saturating_sub(1);
            SendMessageW(
                page.view,
                EM_SETSEL,
                Some(WPARAM(end)),
                Some(LPARAM(end as isize)),
            );
            SendMessageW(page.view, EM_SCROLLCARET, None, None);
        } else {
            restore(
                page.view,
                Position {
                    selection_start: snapshot.rebase(page.start, previous.selection_start) as u32,
                    selection_end: snapshot.rebase(page.start, previous.selection_end) as u32,
                    top_character: snapshot.rebase(page.start, previous.top_character) as u32,
                },
            );
        }
        SendMessageW(page.view, WM_SETREDRAW, Some(WPARAM(1)), None);
        let _ = InvalidateRect(Some(page.view), None, true);
        page.revision = snapshot.revision;
        page.start = snapshot.start;
    }
}

pub fn command(state: &mut SettingsState, id: usize, notification: u32) -> bool {
    if id == ID_OUTPUT && matches!(notification, EN_SETFOCUS | EN_KILLFOCUS) {
        unsafe {
            let _ = InvalidateRect(Some(state.hwnd), None, true);
        }
        return true;
    }
    if !is_button(id) || notification != BN_CLICKED {
        return false;
    }
    if id == ID_CLEAR {
        crate::debug_log::clear();
        refresh(&mut state.debug);
    } else {
        refresh(&mut state.debug);
        unsafe {
            let view = state.debug.view;
            let previous = position(view);
            SendMessageW(view, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            // Native copying also supports Ctrl+C for an ordinary selection.
            SendMessageW(view, WM_COPY, None, None);
            restore(view, previous);
        }
    }
    true
}
