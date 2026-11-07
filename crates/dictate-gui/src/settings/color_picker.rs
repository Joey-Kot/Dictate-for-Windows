use std::cell::Cell;
use std::rc::Rc;

use dictate_core::config::{WindowColors, WindowTheme};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, SetDIBitsToDevice,
};
use windows::Win32::UI::Controls::{
    EM_SETLIMITTEXT, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    TBM_GETCHANNELRECT, TBM_GETTHUMBRECT, TBM_SETPOS, TBM_SETRANGE, TBS_NOTICKS, TRACKBAR_CLASSW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN,
    VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_CLICK, GetNextDlgTabItem, GetParent, SWP_NOZORDER, WM_APP, WM_CANCELMODE, WM_CHAR,
    WM_HSCROLL, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_SETFOCUS,
    WM_USER,
};

use super::*;
use crate::window_color::{ColorDrafts, ColorField, Hsv};

const ID_SWATCH_BASE: usize = 0x6130;
const ID_RESET_BASE: usize = 0x6134;
const ID_HEX: usize = 0x6138;
const ID_THEME: usize = 0x6139;
const ID_THEME_ITEM_BASE: usize = 0x613A;
const SUBCLASS: usize = ID_SWATCH_BASE;
const FOCUS_SLIDER: u32 = WM_APP + 38;
const CHECK_FOCUS: u32 = WM_APP + 39;
const CHECK_THEME_FOCUS: u32 = WM_APP + 40;
const THEMES: [WindowTheme; 3] = [WindowTheme::Dark, WindowTheme::Light, WindowTheme::Custom];
// commctrl.h defines TBM_GETPOS as WM_USER. The bindings omit it.
const TBM_GETPOS: u32 = WM_USER;
const PANEL_WIDTH: i32 = 240;
const PANEL_HEIGHT: i32 = 276;
const HEX_FRAME: RECT = RECT {
    left: 56,
    top: 239,
    right: 224,
    bottom: 266,
};

struct Row {
    label: HWND,
    swatch: HWND,
    reset: HWND,
}

pub(super) struct Picker {
    theme_label: HWND,
    theme_button: HWND,
    theme_panel: HWND,
    theme_items: Vec<HWND>,
    theme_open: Cell<bool>,
    rows: Vec<Row>,
    brightness_label: HWND,
    panel: HWND,
    wheel: HWND,
    slider: HWND,
    hex_label: HWND,
    hex: HWND,
    dpi: Cell<u32>,
    active: Cell<Option<ColorField>>,
    draft: RefCell<ColorDrafts>,
    updating_hex: Cell<bool>,
    wheel_bitmap: RefCell<(i32, Vec<u32>)>,
}

impl Picker {
    pub fn create(
        state: &mut SettingsState,
        y: i32,
        colors: WindowColors,
        theme: WindowTheme,
        instance: HMODULE,
    ) -> Result<Rc<Self>, String> {
        let classes = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        if !unsafe { InitCommonControlsEx(&classes) }.as_bool() {
            return Err("Unable to initialize the brightness slider".into());
        }
        let layout_start = state.layouts.borrow().len();
        let mut panel = HWND::default();
        let result = (|| {
            let theme_label = create_label(
                state,
                state.language.text("window_theme"),
                CONTENT_LEFT,
                y,
                LABEL_WIDTH,
                FIELD_HEIGHT,
                instance,
            )?;
            let theme_button = create_child(
                state,
                w!("BUTTON"),
                state.language.text(theme_label_key(theme)),
                WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                EDIT_LEFT,
                y,
                EDIT_WIDTH,
                FIELD_HEIGHT,
                ID_THEME,
                instance,
            )?;
            let theme_panel = dropdown::create_panel(state, instance)?;
            let mut theme_items = Vec::with_capacity(THEMES.len());
            for (index, theme) in THEMES.into_iter().enumerate() {
                let item = create_child(
                    state,
                    w!("BUTTON"),
                    state.language.text(theme_label_key(theme)),
                    WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    0,
                    0,
                    EDIT_WIDTH,
                    dropdown::ROW_HEIGHT,
                    ID_THEME_ITEM_BASE + index,
                    instance,
                )?;
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SetParent(item, Some(theme_panel))
                        .map_err(|error| error.to_string())?;
                }
                dropdown::track_hover(item, false)?;
                theme_items.push(item);
            }
            let mut rows = Vec::with_capacity(ColorField::ALL.len());
            let style = WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32);
            for (index, field) in ColorField::ALL.into_iter().enumerate() {
                let top = y + (index as i32 + 1) * 42;
                let text = state.language.text(field.label_key());
                let label =
                    create_label(state, text, CONTENT_LEFT, top, 250, FIELD_HEIGHT, instance)?;
                let swatch = create_child(
                    state,
                    w!("BUTTON"),
                    text,
                    style,
                    452,
                    top + 2,
                    112,
                    30,
                    ID_SWATCH_BASE + index,
                    instance,
                )?;
                let reset = create_child(
                    state,
                    w!("BUTTON"),
                    state.language.text("advanced_audio_reset"),
                    style,
                    576,
                    top + 2,
                    104,
                    30,
                    ID_RESET_BASE + index,
                    instance,
                )?;
                dropdown::track_hover(reset, false)?;
                rows.push(Row {
                    label,
                    swatch,
                    reset,
                });
            }
            panel = dropdown::create_panel_for(state.hwnd, state.dpi, instance)?;
            let child = |class, text: &str, style, id: usize| {
                let text = wide(text);
                unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        class,
                        PCWSTR(text.as_ptr()),
                        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | style,
                        0,
                        0,
                        1,
                        1,
                        Some(panel),
                        (id != 0).then_some(HMENU(id as *mut c_void)),
                        Some(instance.into()),
                        None,
                    )
                }
                .map_err(|error| error.to_string())
            };
            let wheel = child(w!("STATIC"), "", WINDOW_STYLE::default(), 0)?;
            let brightness_label = child(
                w!("STATIC"),
                state.language.text("window_color_brightness"),
                WINDOW_STYLE(SS_CENTERIMAGE_STYLE),
                0,
            )?;
            let slider = child(
                TRACKBAR_CLASSW,
                "",
                WS_TABSTOP | WINDOW_STYLE(TBS_NOTICKS),
                0,
            )?;
            let hex_label = child(w!("STATIC"), "HEX", WINDOW_STYLE(SS_CENTERIMAGE_STYLE), 0)?;
            let hex = child(
                w!("EDIT"),
                "",
                WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                ID_HEX,
            )?;
            let picker = Rc::new(Self {
                theme_label,
                theme_button,
                theme_panel,
                theme_items,
                theme_open: Cell::new(false),
                rows,
                brightness_label,
                panel,
                wheel,
                slider,
                hex_label,
                hex,
                dpi: Cell::new(state.dpi),
                active: Cell::new(None),
                draft: RefCell::new(ColorDrafts::new(colors, theme)),
                updating_hex: Cell::new(false),
                wheel_bitmap: RefCell::default(),
            });
            for hwnd in [panel, wheel, slider, hex, theme_button, theme_panel]
                .into_iter()
                .chain(picker.theme_items.iter().copied())
                .chain(picker.rows.iter().map(|row| row.swatch))
            {
                attach(&picker, hwnd)?;
            }
            apply_dark_theme(hex);
            unsafe {
                SendMessageW(
                    slider,
                    TBM_SETRANGE,
                    Some(WPARAM(1)),
                    Some(LPARAM(255 << 16)),
                );
                SendMessageW(hex, EM_SETLIMITTEXT, Some(WPARAM(32)), None);
            }
            Ok(picker)
        })();
        match result {
            Ok(picker) => {
                for hwnd in [picker.theme_label, picker.theme_button] {
                    state.control_groups.insert(hwnd.0 as usize, "Display");
                }
                state
                    .localized_controls
                    .push((picker.theme_label, "window_theme"));
                for (item, theme) in picker.theme_items.iter().zip(THEMES) {
                    state
                        .localized_controls
                        .push((*item, theme_label_key(theme)));
                }
                for (field, row) in ColorField::ALL.into_iter().zip(&picker.rows) {
                    for hwnd in [row.label, row.swatch, row.reset] {
                        state.control_groups.insert(hwnd.0 as usize, "Display");
                    }
                    state.localized_controls.extend([
                        (row.label, field.label_key()),
                        (row.swatch, field.label_key()),
                        (row.reset, "advanced_audio_reset"),
                    ]);
                }
                state
                    .localized_controls
                    .push((picker.brightness_label, "window_color_brightness"));
                Ok(picker)
            }
            Err(error) => {
                let created = state.layouts.borrow_mut().split_off(layout_start);
                unsafe {
                    if !panel.is_invalid() {
                        let _ = DestroyWindow(panel);
                    }
                    for (hwnd, _) in created {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                Err(error)
            }
        }
    }

    pub fn values(&self) -> Result<WindowColors, ColorField> {
        self.draft.borrow().values()
    }

    pub fn theme(&self) -> WindowTheme {
        self.draft.borrow().theme()
    }

    pub fn buttons(&self) -> impl Iterator<Item = &HWND> {
        std::iter::once(&self.theme_button)
            .chain(self.theme_items.iter())
            .chain(self.rows.iter().flat_map(|row| [&row.swatch, &row.reset]))
    }

    pub fn theme_is_open(&self) -> bool {
        self.theme_open.get()
    }

    pub fn refresh_language(&self, language: Language) {
        let text = wide(language.text(theme_label_key(self.theme())));
        unsafe {
            let _ = SetWindowTextW(self.theme_button, PCWSTR(text.as_ptr()));
            let _ = InvalidateRect(Some(self.theme_button), None, false);
        }
    }

    pub fn close_theme(&self) {
        if self.theme_open.replace(false) {
            unsafe {
                let _ = ShowWindow(self.theme_panel, SW_HIDE);
                let _ = InvalidateRect(Some(self.theme_button), None, false);
            }
        }
    }

    fn toggle_theme(&self, dpi: u32) {
        if self.theme_open.get() {
            self.close_theme();
            return;
        }
        self.close(false);
        let width = dropdown::position(
            self.theme_panel,
            self.theme_button,
            THEMES.len() as i32 * dropdown::ROW_HEIGHT,
            dpi,
        );
        self.theme_open.set(true);
        unsafe {
            for (index, item) in self.theme_items.iter().enumerate() {
                let _ = SetWindowPos(
                    *item,
                    Some(HWND_TOP),
                    platform::scale(dropdown::PADDING, dpi),
                    platform::scale(dropdown::PADDING + index as i32 * dropdown::ROW_HEIGHT, dpi),
                    width,
                    platform::scale(dropdown::ROW_HEIGHT, dpi),
                    SWP_NOACTIVATE,
                );
                let _ = ShowWindow(*item, SW_SHOW);
                let _ = InvalidateRect(Some(*item), None, false);
            }
            let _ = ShowWindow(self.theme_panel, SW_SHOW);
            let _ = InvalidateRect(Some(self.theme_button), None, false);
        }
    }

    fn select_theme(&self, theme: WindowTheme, language: Language) {
        self.close_theme();
        self.draft.borrow_mut().select_theme(theme);
        self.refresh_language(language);
        for field in ColorField::ALL {
            self.repaint_swatch(field);
        }
        unsafe {
            let _ = SetFocus(Some(self.theme_button));
        }
    }

    fn swatch(&self, field: ColorField) -> HWND {
        self.rows[field as usize].swatch
    }

    fn toggle(&self, field: ColorField, dpi: u32, font: HFONT) {
        let was_open = self.active.get() == Some(field);
        self.close(false);
        if was_open {
            return;
        }
        self.dpi.set(dpi);
        for (hwnd, x, y, width, height) in [
            (self.wheel, 32, 10, 176, 176),
            (self.brightness_label, 16, 190, 208, 18),
            (self.slider, 13, 210, 214, 24),
            (self.hex_label, 16, 239, 36, 27),
            (self.hex, HEX_FRAME.left + 8, HEX_FRAME.top + 4, 152, 19),
        ] {
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    platform::scale(x, dpi),
                    platform::scale(y, dpi),
                    platform::scale(width, dpi),
                    platform::scale(height, dpi),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            set_font(hwnd, font);
        }
        dropdown::position_with_width(
            self.panel,
            self.swatch(field),
            PANEL_WIDTH,
            PANEL_HEIGHT - 2 * dropdown::PADDING,
            dpi,
        );
        self.active.set(Some(field));
        self.load_editor(field);
        unsafe {
            let _ = ShowWindow(self.panel, SW_SHOW);
            let _ = PostMessageW(Some(self.panel), FOCUS_SLIDER, WPARAM(0), LPARAM(0));
            let _ = InvalidateRect(Some(self.swatch(field)), None, false);
        }
    }

    pub fn close(&self, return_focus: bool) {
        let Some(field) = self.active.take() else {
            return;
        };
        unsafe {
            if [self.wheel, self.slider].contains(&GetCapture()) {
                let _ = ReleaseCapture();
            }
            if return_focus {
                let _ = SetFocus(Some(self.swatch(field)));
            }
            let _ = ShowWindow(self.panel, SW_HIDE);
            let _ = InvalidateRect(Some(self.swatch(field)), None, false);
        }
    }

    fn write_hex(&self, hex: &str) {
        let text = wide(hex);
        self.updating_hex.set(true);
        unsafe {
            let _ = SetWindowTextW(self.hex, PCWSTR(text.as_ptr()));
        }
        self.updating_hex.set(false);
    }

    fn sync_slider(&self, value: u8) {
        unsafe {
            SendMessageW(
                self.slider,
                TBM_SETPOS,
                Some(WPARAM(0)),
                Some(LPARAM(value.into())),
            );
        }
    }

    fn load_editor(&self, field: ColorField) {
        let (hex, value) = {
            let draft = self.draft.borrow();
            (draft.hex(field), draft.selection(field).value)
        };
        self.write_hex(&hex);
        self.sync_slider(value);
        self.repaint_hex_frame();
        self.repaint_picker();
    }

    fn reset(&self, field: ColorField) {
        self.draft.borrow_mut().reset(field);
        self.repaint_swatch(field);
        if let Some(active) = self.active.get()
            && (active == field || (active == ColorField::Font && field == ColorField::Background))
        {
            self.load_editor(active);
        }
    }

    fn valid_hex(&self) -> bool {
        self.active
            .get()
            .is_none_or(|field| self.draft.borrow().is_valid(field))
    }

    fn update_selection(&self, selected: Hsv) {
        let Some(field) = self.active.get() else {
            return;
        };
        let color = selected.rgb();
        let (changed, was_valid) = {
            let mut draft = self.draft.borrow_mut();
            let changed = draft.selection(field).rgb() != color;
            let was_valid = draft.is_valid(field);
            draft.set_selection(field, selected);
            (changed, was_valid)
        };
        if changed || !was_valid {
            self.write_hex(&color.to_string());
            self.repaint_swatch(field);
        }
        if !was_valid {
            self.repaint_hex_frame();
        }
        self.repaint_picker();
    }

    fn update_from_hex(&self) {
        if self.updating_hex.get() {
            return;
        }
        let Some(field) = self.active.get() else {
            return;
        };
        let text = read_text(self.hex);
        let (was_valid, valid, value) = {
            let mut draft = self.draft.borrow_mut();
            let was_valid = draft.is_valid(field);
            draft.edit_hex(field, text);
            (
                was_valid,
                draft.is_valid(field),
                draft.selection(field).value,
            )
        };
        if valid {
            self.sync_slider(value);
            self.repaint_picker();
            self.repaint_swatch(field);
        }
        if was_valid != valid {
            self.repaint_hex_frame();
            unsafe {
                let _ = InvalidateRect(Some(self.hex), None, false);
            }
        }
    }

    fn repaint_swatch(&self, field: ColorField) {
        unsafe {
            let _ = InvalidateRect(Some(self.swatch(field)), None, false);
            if field == ColorField::Background {
                let _ = InvalidateRect(Some(self.swatch(ColorField::Font)), None, false);
            }
        }
    }

    fn repaint_picker(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.wheel), None, false);
            let _ = InvalidateRect(Some(self.slider), None, false);
        }
    }

    fn repaint_hex_frame(&self) {
        unsafe {
            let rect = scaled_rect(HEX_FRAME, self.dpi.get());
            let _ = InvalidateRect(Some(self.panel), Some(&rect), false);
        }
    }

    fn panel_message(&self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        unsafe {
            match message {
                FOCUS_SLIDER => {
                    if let Some(field) = self.active.get()
                        && GetFocus() == self.swatch(field)
                    {
                        let _ = SetFocus(Some(self.slider));
                    }
                }
                CHECK_FOCUS => {
                    if let Some(field) = self.active.get()
                        && ![self.swatch(field), self.slider, self.hex].contains(&GetFocus())
                    {
                        self.close(false);
                    }
                }
                WM_COMMAND if wparam.0 & 0xffff == ID_HEX => {
                    if (wparam.0 >> 16) as u32 == EN_CHANGE {
                        self.update_from_hex();
                    }
                }
                WM_HSCROLL if lparam.0 == self.slider.0 as isize => {
                    if let Some(field) = self.active.get() {
                        let mut selection = self.draft.borrow().selection(field);
                        let value = SendMessageW(self.slider, TBM_GETPOS, None, None).0 as u8;
                        if selection.value != value {
                            selection.value = value;
                            self.update_selection(selection);
                        }
                    }
                }
                WM_PAINT => paint_buffered(self.panel, |dc, client| {
                    fill_color(dc, client, dropdown::background());
                    let border = if !self.valid_hex() {
                        rgb(219, 123, 111)
                    } else if GetFocus() == self.hex {
                        rgb(82, 192, 176)
                    } else {
                        rgb(48, 65, 70)
                    };
                    rounded_box(
                        dc,
                        scaled_rect(HEX_FRAME, self.dpi.get()),
                        rgb(26, 35, 39),
                        border,
                        platform::scale(7, self.dpi.get()),
                    );
                }),
                WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
                    let dc = HDC(wparam.0 as *mut c_void);
                    let edit = lparam.0 == self.hex.0 as isize;
                    let background = if edit {
                        rgb(26, 35, 39)
                    } else {
                        dropdown::background()
                    };
                    let text = if edit && !self.valid_hex() {
                        rgb(255, 156, 141)
                    } else {
                        rgb(218, 230, 232)
                    };
                    SetBkColor(dc, background);
                    SetTextColor(dc, text);
                    SetDCBrushColor(dc, background);
                    return Some(LRESULT(GetStockObject(DC_BRUSH).0 as isize));
                }
                _ => return None,
            }
        }
        Some(LRESULT(0))
    }

    fn wheel_bounds(&self) -> (i32, i32) {
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.wheel, &mut client);
        }
        let inset = platform::scale(8, self.dpi.get());
        (inset, (client.right.min(client.bottom) - 2 * inset).max(0))
    }

    fn wheel_message(&self, message: u32, lparam: LPARAM) -> Option<LRESULT> {
        unsafe {
            match message {
                WM_NCHITTEST => return Some(LRESULT(HTCLIENT as isize)),
                WM_ERASEBKGND => return Some(LRESULT(1)),
                WM_PAINT => self.paint_wheel(),
                WM_CANCELMODE => {
                    if GetCapture() == self.wheel {
                        let _ = ReleaseCapture();
                    }
                }
                WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP => {
                    let field = self.active.get()?;
                    let (inset, side) = self.wheel_bounds();
                    if side == 0 {
                        return Some(LRESULT(0));
                    }
                    let radius = side as f32 / 2.0;
                    let center = inset as f32 + radius;
                    let x = lparam.0 as i16 as f32 + 0.5 - center;
                    let y = (lparam.0 >> 16) as i16 as f32 + 0.5 - center;
                    if message == WM_LBUTTONDOWN && x.hypot(y) <= radius {
                        let _ = SetFocus(Some(self.slider));
                        SetCapture(self.wheel);
                    }
                    if GetCapture() == self.wheel {
                        let value = self.draft.borrow().selection(field).value;
                        self.update_selection(Hsv::from_wheel_pointer(x, y, radius, value));
                        if message == WM_LBUTTONUP {
                            let _ = ReleaseCapture();
                        }
                    }
                }
                _ => return None,
            }
        }
        Some(LRESULT(0))
    }

    fn paint_wheel(&self) {
        paint_buffered(self.wheel, |dc, client| unsafe {
            let background = dropdown::background();
            fill_color(dc, client, background);
            let Some(field) = self.active.get() else {
                return;
            };
            let (inset, side) = self.wheel_bounds();
            if side > 0 {
                let mut bitmap = self.wheel_bitmap.borrow_mut();
                if bitmap.0 != side {
                    *bitmap = (side, wheel_pixels(side, background));
                }
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: side,
                        biHeight: -side,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                SetDIBitsToDevice(
                    dc,
                    inset,
                    inset,
                    side as u32,
                    side as u32,
                    0,
                    0,
                    0,
                    side as u32,
                    bitmap.1.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                );
                let selection = self.draft.borrow().selection(field);
                let (x, y) = selection.wheel_position();
                let radius = side as f32 / 2.0;
                let x = inset + ((1.0 + x) * radius).round() as i32;
                let y = inset + ((1.0 + y) * radius).round() as i32;
                let outer = platform::scale(6, self.dpi.get());
                let inner = platform::scale(4, self.dpi.get());
                let circle = |r| RECT {
                    left: x - r,
                    top: y - r,
                    right: x + r,
                    bottom: y + r,
                };
                rounded_box(
                    dc,
                    circle(outer),
                    rgb(246, 249, 250),
                    rgb(12, 16, 18),
                    outer,
                );
                let [red, green, blue] = selection.rgb().0;
                let color = rgb(red, green, blue);
                rounded_box(dc, circle(inner), color, color, inner);
            }
        });
    }

    fn paint_slider(&self) {
        paint_buffered(self.slider, |dc, client| unsafe {
            fill_color(dc, client, dropdown::background());
            let mut channel = RECT::default();
            let mut thumb = RECT::default();
            SendMessageW(
                self.slider,
                TBM_GETCHANNELRECT,
                None,
                Some(LPARAM(&mut channel as *mut RECT as isize)),
            );
            SendMessageW(
                self.slider,
                TBM_GETTHUMBRECT,
                None,
                Some(LPARAM(&mut thumb as *mut RECT as isize)),
            );
            let dpi = self.dpi.get();
            let middle = (client.top + client.bottom) / 2;
            let track_radius = platform::scale(3, dpi);
            let track = RECT {
                left: channel.left,
                right: channel.right,
                top: middle - track_radius,
                bottom: middle + track_radius,
            };
            let center = (thumb.left + thumb.right) / 2;
            rounded_box(dc, track, rgb(36, 50, 55), rgb(54, 72, 78), track_radius);
            let filled = RECT {
                right: center.min(track.right),
                ..track
            };
            if filled.right > filled.left {
                rounded_box(
                    dc,
                    filled,
                    rgb(56, 107, 103),
                    rgb(72, 137, 129),
                    track_radius,
                );
            }
            let radius = platform::scale(7, dpi);
            let knob = RECT {
                left: center - radius,
                right: center + radius,
                top: middle - radius,
                bottom: middle + radius,
            };
            let focused = GetFocus() == self.slider;
            rounded_box(
                dc,
                knob,
                if focused {
                    rgb(140, 225, 210)
                } else {
                    rgb(112, 203, 191)
                },
                rgb(24, 42, 45),
                radius,
            );
        });
    }
}

fn paint_buffered(hwnd: HWND, draw: impl FnOnce(HDC, RECT)) {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_ok() && client.right > 0 && client.bottom > 0 {
            dropdown::with_buffered_dc(dc, client, |buffer| draw(buffer, client));
        }
        let _ = EndPaint(hwnd, &paint);
    }
}

fn wheel_pixels(side: i32, background: COLORREF) -> Vec<u32> {
    let [r, g, b, _] = background.0.to_le_bytes();
    let background = [r, g, b];
    let radius = side as f32 / 2.0;
    let axis = |coordinate| (coordinate as f32 + 0.5 - radius) / radius;
    let mut pixels = Vec::with_capacity(side as usize * side as usize);
    for row in 0..side {
        for column in 0..side {
            let (x, y) = (axis(column), axis(row));
            let coverage = ((1.0 - x.hypot(y)) * radius + 0.5).clamp(0.0, 1.0);
            let [r, g, b] = if coverage == 0.0 {
                background
            } else {
                // Keep hue and saturation visible even when the brightness is zero.
                let color = Hsv::from_wheel(x, y, 255).rgb().0;
                std::array::from_fn(|i| {
                    (f32::from(background[i]) * (1.0 - coverage) + f32::from(color[i]) * coverage)
                        .round() as u8
                })
            };
            pixels.push(u32::from_be_bytes([0, r, g, b]));
        }
    }
    pixels
}

fn attach(picker: &Rc<Picker>, hwnd: HWND) -> Result<(), String> {
    let pointer = Rc::into_raw(picker.clone());
    if !unsafe { SetWindowSubclass(hwnd, Some(control_proc), SUBCLASS, pointer as usize) }.as_bool()
    {
        unsafe {
            drop(Rc::from_raw(pointer));
        }
        return Err("Unable to initialize the floating window color picker".into());
    }
    Ok(())
}

fn field_for_id(id: usize, base: usize) -> Option<ColorField> {
    id.checked_sub(base)
        .and_then(|index| ColorField::ALL.get(index).copied())
}

fn theme_label_key(theme: WindowTheme) -> &'static str {
    match theme {
        WindowTheme::Dark => "window_theme_dark",
        WindowTheme::Light => "window_theme_light",
        WindowTheme::Custom => "window_theme_custom",
    }
}

pub(super) fn is_theme_button(id: usize) -> bool {
    id == ID_THEME
}

pub(super) fn theme_item(id: usize) -> Option<WindowTheme> {
    id.checked_sub(ID_THEME_ITEM_BASE)
        .and_then(|index| THEMES.get(index).copied())
}

pub(super) fn is_reset(id: usize) -> bool {
    field_for_id(id, ID_RESET_BASE).is_some()
}

pub(super) fn command(state: &mut SettingsState, id: usize, notification: u32) -> bool {
    if notification != BN_CLICKED || state.saving {
        return false;
    }
    if is_theme_button(id) {
        set_language_dropdown(state, false);
        if let Some(picker) = &state.color_picker {
            picker.toggle_theme(state.dpi);
        }
        return true;
    }
    if let Some(theme) = theme_item(id) {
        if let Some(picker) = &state.color_picker {
            picker.select_theme(theme, state.language);
        }
        return true;
    }
    if let Some(field) = field_for_id(id, ID_SWATCH_BASE) {
        if state.language_open {
            set_language_dropdown(state, false);
        }
        if let Some(picker) = &state.color_picker {
            picker.close_theme();
            picker.toggle(field, state.dpi, state.font);
        }
        return true;
    }
    if let Some(field) = field_for_id(id, ID_RESET_BASE) {
        if let Some(picker) = &state.color_picker {
            picker.close_theme();
            picker.reset(field);
        }
        return true;
    }
    false
}

pub(super) fn handles_escape(hwnd: HWND) -> bool {
    let mut data = 0;
    unsafe {
        GetWindowSubclass(hwnd, Some(control_proc), SUBCLASS, Some(&mut data)).as_bool()
            && ((*(data as *const Picker)).active.get().is_some()
                || (*(data as *const Picker)).theme_open.get())
    }
}

pub(super) fn draw_swatch(item: &DRAWITEMSTRUCT) -> bool {
    let Some(field) = field_for_id(item.CtlID as usize, ID_SWATCH_BASE) else {
        return false;
    };
    let mut data = 0;
    unsafe {
        if GetWindowSubclass(item.hwndItem, Some(control_proc), SUBCLASS, Some(&mut data)).as_bool()
        {
            let picker = &*(data as *const Picker);
            let [r, g, b] = picker.draft.borrow().selection(field).rgb().0;
            let border =
                if picker.active.get() == Some(field) || item.itemState.0 & ODS_FOCUS.0 != 0 {
                    rgb(112, 215, 195)
                } else {
                    rgb(83, 98, 102)
                };
            let dpi = platform::window_dpi(item.hwndItem);
            dropdown::with_buffered_dc(item.hDC, item.rcItem, |dc| {
                fill_color(dc, item.rcItem, rgb(16, 22, 25));
                rounded_box(
                    dc,
                    item.rcItem,
                    rgb(r, g, b),
                    border,
                    platform::scale(7, dpi),
                );
            });
        }
    }
    true
}

unsafe extern "system" fn control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    data: usize,
) -> LRESULT {
    let pointer = data as *const Picker;
    unsafe {
        if message == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(control_proc), subclass);
            drop(Rc::from_raw(pointer));
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
        // Synchronous control notifications can reenter the picker. Hold a
        // reference even if another message destroys this particular control.
        Rc::increment_strong_count(pointer);
        let picker = Rc::from_raw(pointer);
        if hwnd == picker.theme_panel && message == CHECK_THEME_FOCUS {
            let focus = GetFocus();
            if focus != picker.theme_button && !picker.theme_items.contains(&focus) {
                picker.close_theme();
            }
            return LRESULT(0);
        }
        if hwnd == picker.panel
            && let Some(result) = picker.panel_message(message, wparam, lparam)
        {
            return result;
        }
        if hwnd == picker.wheel
            && let Some(result) = picker.wheel_message(message, lparam)
        {
            return result;
        }
        if hwnd == picker.slider {
            match message {
                WM_ERASEBKGND => return LRESULT(1),
                WM_PAINT => {
                    picker.paint_slider();
                    return LRESULT(0);
                }
                WM_SETFOCUS | WM_KILLFOCUS => {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                _ => {}
            }
        }
        if hwnd == picker.hex && matches!(message, WM_SETFOCUS | WM_KILLFOCUS) {
            picker.repaint_hex_frame();
        }
        let key = wparam.0 as u16;
        if message == WM_KEYDOWN {
            if hwnd == picker.theme_button || picker.theme_items.contains(&hwnd) {
                if key == VK_TAB.0 {
                    picker.close_theme();
                    let target = GetParent(picker.theme_button)
                        .and_then(|parent| {
                            GetNextDlgTabItem(
                                parent,
                                Some(picker.theme_button),
                                GetKeyState(VK_SHIFT.0 as i32) < 0,
                            )
                        })
                        .unwrap_or(picker.theme_button);
                    let _ = SetFocus(Some(target));
                    return LRESULT(0);
                }
                if picker.theme_open.get() && key == VK_ESCAPE.0 {
                    picker.close_theme();
                    let _ = SetFocus(Some(picker.theme_button));
                    return LRESULT(0);
                }
                if key == VK_RETURN.0 {
                    SendMessageW(hwnd, BM_CLICK, None, None);
                    return LRESULT(0);
                }
                if key == VK_UP.0 || key == VK_DOWN.0 {
                    if !picker.theme_open.get() {
                        SendMessageW(picker.theme_button, BM_CLICK, None, None);
                    }
                    if picker.theme_open.get() {
                        let index = picker.theme_items.iter().position(|item| *item == hwnd);
                        let index = match index {
                            Some(index) if key == VK_UP.0 => index.saturating_sub(1),
                            Some(index) => (index + 1).min(THEMES.len() - 1),
                            None => THEMES
                                .iter()
                                .position(|theme| *theme == picker.theme())
                                .unwrap_or(0),
                        };
                        let _ = SetFocus(Some(picker.theme_items[index]));
                    }
                    return LRESULT(0);
                }
            }
            if picker.active.get().is_some() {
                if key == VK_ESCAPE.0 || (key == VK_RETURN.0 && picker.valid_hex()) {
                    picker.close(true);
                    return LRESULT(0);
                }
                if key == VK_TAB.0 {
                    let target = if hwnd == picker.slider {
                        picker.hex
                    } else {
                        picker.slider
                    };
                    let _ = SetFocus(Some(target));
                    return LRESULT(0);
                }
            } else if key == VK_RETURN.0 && picker.rows.iter().any(|row| row.swatch == hwnd) {
                SendMessageW(hwnd, BM_CLICK, None, None);
                return LRESULT(0);
            }
        }
        if message == WM_CHAR && [VK_ESCAPE.0, VK_RETURN.0, VK_TAB.0].contains(&key) {
            return LRESULT(0);
        }
        if message == WM_KILLFOCUS && picker.active.get().is_some() {
            let _ = PostMessageW(Some(picker.panel), CHECK_FOCUS, WPARAM(0), LPARAM(0));
        }
        if message == WM_KILLFOCUS
            && picker.theme_open.get()
            && (hwnd == picker.theme_button || picker.theme_items.contains(&hwnd))
        {
            let _ = PostMessageW(
                Some(picker.theme_panel),
                CHECK_THEME_FOCUS,
                WPARAM(0),
                LPARAM(0),
            );
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}
