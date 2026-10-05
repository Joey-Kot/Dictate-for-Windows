//! Audio selectors share the existing rounded, owner-drawn dropdown surface.
use super::*;
use crate::audio_options::{AudioDraft, CODECS, canonical};
use windows::Win32::UI::Controls::{EM_SETLIMITTEXT, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

pub(super) const ID_LIST_BASE: usize = 0x6400;
pub(super) const WM_ACTION: u32 = WM_APP + 27;

pub(super) struct AudioPicker {
    pub field: usize,
    pub button: HWND,
    pub list: HWND,
    pub editor: HWND,
    panel: HWND,
    scrollbar: HWND,
    pub open: bool,
    pub custom: bool,
    rows: Vec<Option<String>>,
}

impl AudioPicker {
    pub fn create(
        state: &SettingsState,
        field: usize,
        id: usize,
        y: i32,
        instance: windows::Win32::Foundation::HMODULE,
    ) -> Result<Self, String> {
        let button = create_child(
            state,
            w!("BUTTON"),
            "",
            WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            EDIT_LEFT,
            y,
            EDIT_WIDTH,
            FIELD_HEIGHT,
            id,
            instance,
        )?;
        let panel = dropdown::create_panel(state, instance)?;
        let list = create_child(
            state,
            w!("LISTBOX"),
            "",
            WINDOW_STYLE(
                WS_CHILD.0
                    | LBS_NOTIFY as u32
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_HASSTRINGS as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
            ),
            0,
            0,
            EDIT_WIDTH,
            36,
            ID_LIST_BASE + field,
            instance,
        )?;
        let editor = create_child(
            state,
            w!("EDIT"),
            "",
            WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32),
            EDIT_LEFT + 12,
            y + 7,
            EDIT_WIDTH - 56,
            20,
            0x6410 + field,
            instance,
        )?;
        apply_dark_theme(list);
        apply_dark_theme(editor);
        dropdown::track_hover(list, true)?;
        unsafe {
            SetParent(list, Some(panel)).map_err(|error| error.to_string())?;
            for hwnd in [button, list, editor] {
                if !SetWindowSubclass(hwnd, Some(control_proc), field, state.hwnd.0 as usize)
                    .as_bool()
                {
                    return Err("Unable to initialize audio dropdown".into());
                }
            }
            SendMessageW(editor, EM_SETLIMITTEXT, Some(WPARAM(16)), None);
        }
        let scrollbar = dropdown_scrollbar::create(state, panel, list, instance)?;
        Ok(Self {
            field,
            button,
            panel,
            scrollbar,
            list,
            editor,
            open: false,
            custom: false,
            rows: vec![],
        })
    }

    pub fn close(&mut self) {
        self.open = false;
        unsafe {
            let _ = ShowWindow(self.panel, SW_HIDE);
            let _ = InvalidateRect(Some(self.button), None, false);
        }
    }

    pub fn rebuild(
        &mut self,
        draft: &AudioDraft,
        language: Language,
        dpi: u32,
        visible: bool,
        saving: bool,
    ) {
        let value = &draft.values[self.field];
        self.rows = draft.options(self.field).into_iter().map(Some).collect();
        let selected = self.rows.iter().position(|row| {
            row.as_ref().is_some_and(|row| {
                if self.field == 4 {
                    canonical(row) == canonical(value)
                } else {
                    row.eq_ignore_ascii_case(value)
                }
            })
        });
        let selected = selected.unwrap_or_else(|| {
            self.rows.insert(0, Some(value.clone()));
            0
        });
        if matches!(self.field, 2 | 3) {
            self.rows.push(None);
        }
        let mut label = label(draft, self.field, value, language);
        if !draft.options(self.field).iter().any(|option| {
            if self.field == 4 {
                canonical(option) == canonical(value)
            } else {
                option.eq_ignore_ascii_case(value)
            }
        }) {
            label = format!("{}: {label}", language.text("audio_current"));
        }
        if !draft.enabled(self.field) {
            label = format!(
                "{} — {}",
                label,
                language.text(if self.field == 3 {
                    "audio_unused"
                } else {
                    "audio_depth_encoder"
                })
            );
        }
        unsafe {
            let _ = SetWindowTextW(self.button, PCWSTR(wide(&label).as_ptr()));
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
            for row in &self.rows {
                let text = row
                    .as_ref()
                    .map(|v| label_for_row(draft, self.field, v, language))
                    .unwrap_or_else(|| language.text("audio_custom").into());
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(&text).as_ptr() as isize)),
                );
            }
            SendMessageW(self.list, LB_SETCURSEL, Some(WPARAM(selected)), None);
            SendMessageW(
                self.list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(platform::scale(36, dpi) as isize)),
            );
            let height = self.rows.len().min(6) as i32 * platform::scale(36, dpi);
            let width = dropdown::position_pixels(self.panel, self.button, height, dpi);
            let width = dropdown_scrollbar::position(
                self.scrollbar,
                width,
                height,
                dpi,
                self.rows.len() > 6,
            );
            let _ = SetWindowPos(
                self.list,
                Some(HWND_TOP),
                platform::scale(8, dpi),
                platform::scale(8, dpi),
                width,
                height,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.list, SW_SHOW);
            let _ = ShowWindow(
                self.panel,
                if visible && self.open {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
            let _ = ShowWindow(
                self.editor,
                if visible && self.custom {
                    SW_SHOW
                } else {
                    SW_HIDE
                },
            );
            if visible && self.custom {
                let _ = SetWindowPos(
                    self.editor,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            let enabled = !saving && draft.enabled(self.field);
            let _ = EnableWindow(self.button, enabled);
            let _ = EnableWindow(self.editor, enabled);
            let _ = InvalidateRect(Some(self.button), None, false);
        }
    }

    pub fn draw(&self, state: &SettingsState, item: &DRAWITEMSTRUCT) {
        let Some(row) = self.rows.get(item.itemID as usize) else {
            return;
        };
        let value = &state.audio_draft.values[self.field];
        let selected = row.as_ref().is_some_and(|row| {
            if self.field == 4 {
                canonical(row) == canonical(value)
            } else {
                row.eq_ignore_ascii_case(value)
            }
        });
        let text = row
            .as_ref()
            .map(|v| label_for_row(&state.audio_draft, self.field, v, state.language))
            .unwrap_or_else(|| state.language.text("audio_custom").into());
        unsafe {
            dropdown::draw_row(
                item.hDC,
                item.rcItem,
                selected,
                item.itemState.0 & ODS_SELECTED.0 != 0
                    || dropdown::hovered_row(self.list, item.itemID),
                state.dpi,
            );
            let mut rect = item.rcItem;
            rect.left += platform::scale(14, state.dpi);
            rect.right -= platform::scale(8, state.dpi);
            SetTextColor(item.hDC, rgb(231, 242, 240));
            SetBkMode(item.hDC, TRANSPARENT);
            let old = SelectObject(item.hDC, HGDIOBJ(state.font.0));
            DrawTextW(
                item.hDC,
                &mut wide(&text)[..text.encode_utf16().count()],
                &mut rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SelectObject(item.hDC, old);
        }
    }
}

fn label(draft: &AudioDraft, field: usize, value: &str, language: Language) -> String {
    match field {
        0 if value == "1" => format!("1 — {}", language.text("audio_mono")),
        0 if value == "2" => format!("2 — {}", language.text("audio_stereo")),
        1 => {
            // The codec determines PCM storage depth, even if an old config's depth differs.
            let actual = match draft.values[4].to_ascii_lowercase().as_str() {
                "pcm_s8" | "pcm_alaw" | "pcm_mulaw" => "8",
                "pcm" | "pcm_s16le" | "pcm_s16be" => "16",
                "pcm_s24le" | "pcm_s24be" => "24",
                "pcm_s32le" | "pcm_f32le" | "pcm_s32be" | "pcm_f32be" => "32",
                "pcm_f64le" | "pcm_s64le" | "pcm_f64be" => "64",
                _ => value,
            };
            if actual != value {
                format!("{value} bit ({}: {actual} bit)", language.text("CODECS"))
            } else {
                format!("{value} bit")
            }
        }
        2 => format!("{value} Hz"),
        3 => format!("{value} kbps"),
        4 => CODECS
            .iter()
            .find(|(key, _)| *key == canonical(value))
            .map(|(_, title)| (*title).into())
            .unwrap_or_else(|| value.into()),
        _ => value.into(),
    }
}

fn label_for_row(draft: &AudioDraft, field: usize, value: &str, language: Language) -> String {
    let text = if field == 1 {
        format!("{value} bit")
    } else {
        label(draft, field, value, language)
    };
    if !draft.options(field).iter().any(|v| {
        if field == 4 {
            canonical(v) == canonical(value)
        } else {
            v.eq_ignore_ascii_case(value)
        }
    }) {
        format!("{}: {text}", language.text("audio_current"))
    } else {
        text
    }
}

pub(super) fn close_all(state: &mut SettingsState) {
    for picker in &mut state.audio_pickers {
        picker.close();
    }
}

pub(super) fn refresh(state: &mut SettingsState) {
    for picker in &mut state.audio_pickers {
        picker.rebuild(
            &state.audio_draft,
            state.language,
            state.dpi,
            state.active_group == 2,
            state.saving,
        );
    }
}

pub(super) fn draft(state: &SettingsState) -> AudioDraft {
    let mut draft = state.audio_draft.clone();
    for picker in &state.audio_pickers {
        if picker.custom {
            draft.values[picker.field] = read_text(picker.editor).trim().into();
        }
    }
    draft
}

pub(super) fn action(state: &mut SettingsState, field: usize, action: usize, detail: LPARAM) {
    let Some(index) = state.audio_pickers.iter().position(|p| p.field == field) else {
        return;
    };
    if action == 3 {
        let p = &state.audio_pickers[index];
        if detail.0 != p.button.0 as isize && detail.0 != p.list.0 as isize {
            state.audio_pickers[index].close();
        }
        return;
    }
    if action == 2 || action == 5 || action == 6 {
        let p = &mut state.audio_pickers[index];
        p.close();
        if action == 6 {
            p.custom = false;
            unsafe {
                let _ = ShowWindow(p.editor, SW_HIDE);
            }
        }
        unsafe {
            let focus = if action == 5 {
                GetNextDlgTabItem(
                    state.hwnd,
                    Some(if p.custom { p.editor } else { p.button }),
                    detail.0 != 0,
                )
                .unwrap_or(p.button)
            } else {
                p.button
            };
            let _ = SetFocus(Some(focus));
        }
        return;
    }
    if state.saving || state.active_group != 2 || !state.audio_draft.enabled(field) {
        return;
    }
    if action == 4 {
        state.audio_draft = draft(state);
        let open = !state.audio_pickers[index].open;
        close_all(state);
        if let Some(p) = &mut state.microphone {
            p.set_open(false, state.language, state.dpi);
        }
        state.audio_pickers[index].open = open;
        refresh(state);
        if open {
            unsafe {
                let _ = SetFocus(Some(state.audio_pickers[index].list));
            }
        }
    } else if action == 1 && state.audio_pickers[index].open {
        let p = &state.audio_pickers[index];
        let row = unsafe { SendMessageW(p.list, LB_GETCURSEL, None, None) }.0 as usize;
        let Some(value) = p.rows.get(row).cloned() else {
            return;
        };
        state.audio_draft = draft(state);
        state.audio_pickers[index].close();
        let editor = state.audio_pickers[index].editor;
        if let Some(value) = value {
            // Selecting the current row must also preserve aliases and non-presets.
            let current = &state.audio_draft.values[field];
            let same = if field == 4 {
                canonical(&value) == canonical(current)
            } else {
                value.eq_ignore_ascii_case(current)
            };
            let pcm_depth_change = field == 1
                && state.audio_draft.codec() == "pcm"
                && !state.audio_draft.values[4].eq_ignore_ascii_case(&format!("pcm_s{value}le"))
                && !(value == "16" && state.audio_draft.values[4].eq_ignore_ascii_case("pcm"));
            if !same || pcm_depth_change {
                state.audio_draft.select(field, value);
            }
            state.audio_pickers[index].custom = false;
            // An explicit upstream change may have replaced an incompatible custom value.
            for p in &state.audio_pickers {
                if p.custom {
                    unsafe {
                        let _ = SetWindowTextW(
                            p.editor,
                            PCWSTR(wide(&state.audio_draft.values[p.field]).as_ptr()),
                        );
                    }
                }
            }
            refresh(state);
            unsafe {
                let _ = SetFocus(Some(state.audio_pickers[index].button));
            }
        } else {
            state.audio_pickers[index].custom = true;
            unsafe {
                let _ = SetWindowTextW(
                    editor,
                    PCWSTR(wide(&state.audio_draft.values[field]).as_ptr()),
                );
            }
            refresh(state);
            unsafe {
                let _ = SetFocus(Some(editor));
                SendMessageW(editor, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            }
        }
    }
}

unsafe extern "system" fn control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    field: usize,
    parent: usize,
) -> LRESULT {
    let parent = HWND(parent as *mut c_void);
    let id = unsafe { GetDlgCtrlID(hwnd) } as usize;
    let list = id == ID_LIST_BASE + field;
    let editor = id == 0x6410 + field;
    let post = |action: usize, detail: LPARAM| unsafe {
        let _ = PostMessageW(
            Some(parent),
            WM_ACTION,
            WPARAM((field << 8) | action),
            detail,
        );
    };
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        if list && [VK_RETURN.0, VK_SPACE.0, VK_ESCAPE.0, VK_TAB.0].contains(&key) {
            post(
                if key == VK_TAB.0 {
                    5
                } else if key == VK_ESCAPE.0 {
                    2
                } else {
                    1
                },
                LPARAM((unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) as isize),
            );
            return LRESULT(0);
        }
        if !list && !editor && [VK_DOWN.0, VK_RETURN.0].contains(&key) {
            post(4, LPARAM(0));
            return LRESULT(0);
        }
        if editor && key == VK_ESCAPE.0 {
            post(6, LPARAM(0));
            return LRESULT(0);
        }
        if !list && key == VK_TAB.0 {
            post(
                5,
                LPARAM((unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0) as isize),
            );
            return LRESULT(0);
        }
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if list && message == WM_LBUTTONUP {
        let hit = unsafe { SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(lparam)) }.0 as usize;
        if hit >> 16 == 0 {
            post(1, LPARAM(0));
        }
    } else if list && message == WM_KILLFOCUS {
        post(3, LPARAM(wparam.0 as isize));
    }
    result
}
