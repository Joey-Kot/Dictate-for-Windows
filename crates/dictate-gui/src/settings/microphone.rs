use std::sync::mpsc::{self, Receiver};

use dictate_core::audio_devices::{InputDevice, list_input_devices};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{GetDC, GetTextExtentPoint32W, ReleaseDC};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::*;

pub(super) const ID_MICROPHONE: usize = 0x6106;
pub(super) const ID_MICROPHONE_LIST: usize = 0x6107;
pub(super) const WM_MICROPHONE_ACTION: u32 = WM_APP + 26;
pub(super) const MICROPHONE_TIMER: usize = 0x6108;

struct Row {
    label: String,
    id: Option<String>,
    name: String,
}

pub(super) struct MicrophonePicker {
    pub button: HWND,
    pub list: HWND,
    panel: HWND,
    vertical_scrollbar: HWND,
    horizontal_scrollbar: HWND,
    pub open: bool,
    pub selected_id: String,
    pub selected_name: String,
    devices: Vec<InputDevice>,
    rows: Vec<Row>,
    pending: Option<Receiver<Result<Vec<InputDevice>, String>>>,
    loaded: bool,
    error: Option<String>,
    parent: HWND,
    text_width: i32,
}

impl MicrophonePicker {
    pub fn create(
        state: &SettingsState,
        instance: windows::Win32::Foundation::HMODULE,
    ) -> Result<Self, String> {
        let button = create_child(
            state,
            w!("BUTTON"),
            "",
            WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
            EDIT_LEFT,
            FIELD_TOP,
            EDIT_WIDTH,
            FIELD_HEIGHT,
            ID_MICROPHONE,
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
            EDIT_LEFT,
            FIELD_TOP + 40,
            EDIT_WIDTH,
            6 * 36,
            ID_MICROPHONE_LIST,
            instance,
        )?;
        apply_dark_theme(list);
        dropdown::track_hover(list, true)?;
        unsafe {
            SetParent(list, Some(panel)).map_err(|error| error.to_string())?;
            if !SetWindowSubclass(list, Some(list_proc), 1, state.hwnd.0 as usize).as_bool()
                || !SetWindowSubclass(button, Some(button_proc), 1, state.hwnd.0 as usize).as_bool()
            {
                return Err("Unable to initialize microphone dropdown".into());
            }
            SendMessageW(
                list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(platform::scale(36, state.dpi) as isize)),
            );
        }
        let vertical_scrollbar = dropdown_scrollbar::create(state, panel, list, instance)?;
        let horizontal_scrollbar =
            dropdown_scrollbar::create_horizontal(state, panel, list, instance)?;
        let config = state.runtime.config();
        let mut picker = Self {
            button,
            list,
            panel,
            vertical_scrollbar,
            horizontal_scrollbar,
            parent: state.hwnd,
            open: false,
            selected_id: config.input_device,
            selected_name: config.input_device_name,
            devices: Vec::new(),
            rows: Vec::new(),
            pending: None,
            loaded: false,
            error: None,
            text_width: 0,
        };
        picker.rebuild(state.language, state.dpi);
        picker.refresh(state.language, state.dpi);
        Ok(picker)
    }

    pub fn refresh(&mut self, language: Language, dpi: u32) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.error = None;
        std::thread::spawn(move || {
            let _ = tx.send(list_input_devices());
        });
        unsafe {
            SetTimer(Some(self.parent), MICROPHONE_TIMER, 100, None);
        }
        self.rebuild(language, dpi);
    }

    pub fn poll(&mut self, language: Language, dpi: u32) {
        let Some(receiver) = &self.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("Device enumeration stopped".into()),
        };
        self.pending = None;
        unsafe {
            let _ = KillTimer(Some(self.parent), MICROPHONE_TIMER);
        }
        match result {
            Ok(devices) => {
                self.devices = devices;
                self.loaded = true;
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
            }
        }
        self.rebuild(language, dpi);
    }

    pub fn set_open(&mut self, open: bool, language: Language, dpi: u32) {
        self.open = open;
        if open {
            self.refresh(language, dpi);
        }
        unsafe {
            let _ = ShowWindow(self.panel, if open { SW_SHOW } else { SW_HIDE });
            if open {
                let _ = SetWindowPos(
                    self.panel,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                let _ = SetFocus(Some(self.list));
            }
            let _ = InvalidateRect(Some(self.button), None, true);
        }
    }

    pub fn select(&mut self, language: Language, dpi: u32) {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0 as usize;
        if let Some(row) = self.rows.get(index)
            && let Some(id) = &row.id
        {
            self.selected_id = id.clone();
            self.selected_name = row.name.clone();
            self.set_open(false, language, dpi);
            self.rebuild(language, dpi);
            unsafe {
                let _ = SetFocus(Some(self.button));
            }
        }
    }

    pub fn rebuild(&mut self, language: Language, dpi: u32) {
        let highlighted = if self.open {
            let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, None, None) }.0 as usize;
            self.rows.get(index).and_then(|row| row.id.clone())
        } else {
            None
        };
        let mut label = language.text("microphone_default").to_string();
        if let Some(device) = self.devices.iter().find(|d| d.is_default) {
            label.push_str(&format!(" — {}", device.name));
        }
        self.rows = vec![Row {
            label,
            id: Some(String::new()),
            name: String::new(),
        }];
        for device in &self.devices {
            let duplicate = self
                .devices
                .iter()
                .filter(|d| d.name == device.name)
                .count()
                > 1;
            let label = if duplicate {
                // Put the endpoint identifier first so same-name rows remain distinguishable.
                let suffix = device
                    .id
                    .rsplit('{')
                    .next()
                    .unwrap_or(&device.id)
                    .trim_end_matches('}');
                format!("[{suffix}] {}", device.name)
            } else {
                device.name.clone()
            };
            self.rows.push(Row {
                label,
                id: Some(device.id.clone()),
                name: device.name.clone(),
            });
        }
        let selected = self
            .rows
            .iter()
            .position(|r| r.id.as_deref() == Some(&self.selected_id));
        if !self.selected_id.is_empty() && selected.is_none() {
            let name = if self.selected_name.is_empty() {
                &self.selected_id
            } else {
                &self.selected_name
            };
            let status = if self.loaded && self.error.is_none() {
                "microphone_unavailable"
            } else {
                "microphone_checking"
            };
            self.rows.push(Row {
                label: format!("{} — {name}", language.text(status)),
                id: None,
                name: String::new(),
            });
        }
        let selected_index = selected.unwrap_or(self.rows.len() - 1);
        let button_text = self.rows[selected_index].label.clone();
        if let Some(index) = selected
            && index > 0
        {
            self.selected_name = self.rows[index].name.clone();
        }
        if self.pending.is_some() {
            self.rows.push(Row {
                label: language.text("microphone_checking").into(),
                id: None,
                name: String::new(),
            });
        } else if let Some(error) = &self.error {
            self.rows.push(Row {
                label: format!("{}: {error}", language.text("microphone_error")),
                id: None,
                name: String::new(),
            });
        } else if self.loaded && self.devices.is_empty() {
            self.rows.push(Row {
                label: language.text("microphone_none").into(),
                id: None,
                name: String::new(),
            });
        }
        unsafe {
            let dc = GetDC(Some(self.list));
            let font = SendMessageW(self.list, WM_GETFONT, None, None);
            let old = SelectObject(dc, HGDIOBJ(font.0 as *mut c_void));
            self.text_width = self
                .rows
                .iter()
                .map(|row| {
                    let text: Vec<u16> = row.label.encode_utf16().collect();
                    let mut size = SIZE::default();
                    let _ = GetTextExtentPoint32W(dc, &text, &mut size);
                    size.cx
                })
                .max()
                .unwrap_or(0)
                + platform::scale(28, dpi);
            SelectObject(dc, old);
            ReleaseDC(Some(self.list), dc);
            let _ = SetWindowTextW(self.button, PCWSTR(wide(&button_text).as_ptr()));
            SendMessageW(self.list, WM_SETREDRAW, Some(WPARAM(0)), None);
            SendMessageW(self.list, LB_RESETCONTENT, None, None);
            for row in &self.rows {
                SendMessageW(
                    self.list,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(wide(&row.label).as_ptr() as isize)),
                );
            }
            let highlighted_index = highlighted
                .and_then(|id| {
                    self.rows
                        .iter()
                        .position(|row| row.id.as_ref() == Some(&id))
                })
                .unwrap_or(selected_index);
            SendMessageW(
                self.list,
                LB_SETCURSEL,
                Some(WPARAM(highlighted_index)),
                None,
            );
            SendMessageW(
                self.list,
                LB_SETITEMHEIGHT,
                Some(WPARAM(0)),
                Some(LPARAM(platform::scale(36, dpi) as isize)),
            );
            SendMessageW(self.list, WM_SETREDRAW, Some(WPARAM(1)), None);
            self.position_list(dpi);
            // WM_SETREDRAW(TRUE) can restore WS_VISIBLE even for a closed list.
            let _ = ShowWindow(self.list, SW_SHOW);
            let _ = ShowWindow(self.panel, if self.open { SW_SHOW } else { SW_HIDE });
            let _ = InvalidateRect(Some(self.list), None, true);
            let _ = InvalidateRect(Some(self.button), None, true);
        }
    }

    fn position_list(&self, dpi: u32) {
        let mut bounds = RECT::default();
        let _ = unsafe { GetWindowRect(self.button, &mut bounds) };
        let vertical = self.rows.len() > 8;
        let available = bounds.right
            - bounds.left
            - platform::scale(dropdown::PADDING * 2, dpi)
            - if vertical {
                dropdown_scrollbar::thickness(dpi)
            } else {
                0
            };
        let horizontal = self.text_width > available;
        // Sum physical row heights, rather than rounding the combined logical height.
        let list_height =
            self.rows.len().min(8) as i32 * platform::scale(dropdown::ROW_HEIGHT, dpi);
        let height = list_height
            + if horizontal {
                dropdown_scrollbar::thickness(dpi)
            } else {
                0
            };
        let width = dropdown::position_pixels(self.panel, self.button, height, dpi);
        let width = dropdown_scrollbar::position(
            self.vertical_scrollbar,
            width,
            list_height,
            dpi,
            vertical,
        );
        dropdown_scrollbar::position_horizontal(
            self.horizontal_scrollbar,
            width,
            list_height,
            dpi,
            self.text_width,
        );
        unsafe {
            let _ = SetWindowPos(
                self.list,
                Some(HWND_TOP),
                platform::scale(dropdown::PADDING, dpi),
                platform::scale(dropdown::PADDING, dpi),
                width,
                list_height,
                SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(self.list), None, false);
        }
    }

    pub fn draw(&self, state: &SettingsState, item: &DRAWITEMSTRUCT) {
        let Some(row) = self.rows.get(item.itemID as usize) else {
            return;
        };
        let selected = row.id.as_deref() == Some(&self.selected_id);
        let focused = item.itemState.0 & ODS_SELECTED.0 != 0;
        unsafe {
            let hot = dropdown::hovered_row(self.list, item.itemID);
            let mut row_rect = item.rcItem;
            let mut client = RECT::default();
            let _ = GetClientRect(self.list, &mut client);
            row_rect.left = 0;
            row_rect.right = client.right;
            dropdown::draw_row(
                item.hDC,
                row_rect,
                selected,
                (focused || hot) && row.id.is_some(),
                state.dpi,
            );
            let mut rect = item.rcItem;
            rect.left = platform::scale(14, state.dpi)
                - dropdown_scrollbar::horizontal_offset(self.horizontal_scrollbar);
            rect.right = rect.left + self.text_width;
            SetTextColor(
                item.hDC,
                if row.id.is_none() {
                    rgb(110, 126, 130)
                } else if selected {
                    rgb(231, 250, 246)
                } else {
                    rgb(190, 205, 208)
                },
            );
            SetBkMode(item.hDC, TRANSPARENT);
            let old = SelectObject(item.hDC, HGDIOBJ(state.font.0));
            DrawTextW(
                item.hDC,
                &mut wide(&row.label)[..row.label.encode_utf16().count()],
                &mut rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
            SelectObject(item.hDC, old);
        }
    }
}

unsafe extern "system" fn list_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    let parent = HWND(parent as *mut c_void);
    if message == WM_KEYDOWN {
        let key = wparam.0 as u16;
        if key == VK_RETURN.0 || key == VK_SPACE.0 || key == VK_ESCAPE.0 || key == VK_TAB.0 {
            unsafe {
                let _ = PostMessageW(
                    Some(parent),
                    WM_MICROPHONE_ACTION,
                    WPARAM(if key == VK_RETURN.0 || key == VK_SPACE.0 {
                        1
                    } else if key == VK_TAB.0 {
                        5
                    } else {
                        2
                    }),
                    LPARAM((GetKeyState(VK_SHIFT.0 as i32) < 0) as isize),
                );
            }
            return LRESULT(0);
        }
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if message == WM_LBUTTONUP {
        // Do not select a row when clicking empty space or outside during capture.
        let hit = unsafe { SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(lparam)) }.0 as usize;
        if hit >> 16 == 0 {
            unsafe {
                let _ = PostMessageW(Some(parent), WM_MICROPHONE_ACTION, WPARAM(1), LPARAM(0));
            }
        }
    } else if message == WM_KILLFOCUS {
        unsafe {
            let _ = PostMessageW(
                Some(parent),
                WM_MICROPHONE_ACTION,
                WPARAM(3),
                LPARAM(wparam.0 as isize),
            );
        }
    }
    result
}

unsafe extern "system" fn button_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    if message == WM_KEYDOWN && (wparam.0 == VK_DOWN.0 as usize || wparam.0 == VK_RETURN.0 as usize)
    {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(parent as *mut c_void)),
                WM_MICROPHONE_ACTION,
                WPARAM(4),
                LPARAM(0),
            );
        }
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
