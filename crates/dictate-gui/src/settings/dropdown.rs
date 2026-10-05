//! Shared antialiased menu surface for the settings dropdown selectors.
use super::*;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateRectRgn, CreateRoundRectRgn,
    DeleteDC, SRCCOPY, SetViewportOrgEx,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetCursorPos, GetParent, IsWindow, LB_GETITEMRECT, LB_ITEMFROMPOINT,
    LB_RESETCONTENT, LB_SETTOPINDEX, WM_CHAR, WM_KEYDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCDESTROY, WM_VSCROLL,
};

pub(super) const PADDING: i32 = 8;
pub(super) const ROW_HEIGHT: i32 = 36;

struct PanelSurface {
    hwnd: HWND,
    dpi: u32,
    painted_size: Option<(i32, i32, u32)>,
}

impl PanelSurface {
    unsafe fn sync(&mut self, panel: HWND) {
        unsafe {
            // Test the child's own visible bit: a temporarily hidden ancestor
            // must not leave the surface hidden when that ancestor reappears.
            if GetWindowLongPtrW(panel, GWL_STYLE) as u32 & WS_VISIBLE.0 == 0 {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                return;
            }
            let mut bounds = RECT::default();
            let Ok(parent) = GetParent(panel) else {
                self.fallback(panel);
                return;
            };
            if GetWindowRect(panel, &mut bounds).is_err() {
                self.fallback(panel);
                return;
            }
            let mut origin = POINT {
                x: bounds.left,
                y: bounds.top,
            };
            if !ScreenToClient(parent, &mut origin).as_bool() {
                self.fallback(panel);
                return;
            }
            let width = bounds.right - bounds.left;
            let height = bounds.bottom - bounds.top;
            let padding = platform::scale(PADDING, self.dpi);
            if width <= padding * 2 || height <= padding * 2 {
                self.fallback(panel);
                return;
            }
            // The alpha surface sits immediately behind the native content, but
            // above the settings fields that the expanded menu overlaps.
            if SetWindowPos(
                self.hwnd,
                Some(panel),
                origin.x,
                origin.y,
                width,
                height,
                SWP_NOACTIVATE,
            )
            .is_err()
            {
                self.fallback(panel);
                return;
            }
            let size = (width, height, self.dpi);
            if self.painted_size != Some(size) {
                let radius = platform::scale(12, self.dpi);
                if crate::render::paint_rounded_panel(self.hwnd, background(), radius).is_err() {
                    self.fallback(panel);
                    return;
                }
                // Every list/button/scrollbar is inside this padding. Keep its
                // normal GDI drawing in an opaque rectangle; the alpha surface
                // supplies the entire curved edge without a hard window region.
                let region = CreateRectRgn(padding, padding, width - padding, height - padding);
                if region.is_invalid() {
                    self.fallback(panel);
                    return;
                }
                if SetWindowRgn(panel, Some(region), true) == 0 {
                    let _ = DeleteObject(HGDIOBJ(region.0));
                    self.fallback(panel);
                    return;
                }
                self.painted_size = Some(size);
            }
            // SetWindowPos preserves the z-order while showing the surface.
            if SetWindowPos(
                self.hwnd,
                Some(panel),
                0,
                0,
                0,
                0,
                SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW,
            )
            .is_err()
            {
                self.fallback(panel);
            }
        }
    }

    unsafe fn fallback(&mut self, panel: HWND) {
        unsafe {
            self.painted_size = None;
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let mut client = RECT::default();
            if GetClientRect(panel, &mut client).is_ok() {
                let radius = platform::scale(12, self.dpi);
                let region = CreateRoundRectRgn(
                    0,
                    0,
                    client.right + 1,
                    client.bottom + 1,
                    radius * 2,
                    radius * 2,
                );
                if !region.is_invalid() {
                    if SetWindowRgn(panel, Some(region), true) != 0 {
                        return;
                    }
                    let _ = DeleteObject(HGDIOBJ(region.0));
                }
            }
            // Never leave an old inset region with no alpha surface behind it.
            let _ = SetWindowRgn(panel, None, true);
        }
    }
}

pub(super) fn background() -> COLORREF {
    rgb(23, 31, 35)
}

pub(super) fn track_hover(hwnd: HWND, list: bool) -> Result<(), String> {
    // Bit zero identifies list controls; remaining bits store hovered index + 1.
    if unsafe { SetWindowSubclass(hwnd, Some(hover_proc), 2, usize::from(list)) }.as_bool() {
        Ok(())
    } else {
        Err("Unable to initialize dropdown hover tracking".into())
    }
}

pub(super) fn hovered(hwnd: HWND) -> bool {
    let mut hot = 0;
    unsafe {
        GetWindowSubclass(hwnd, Some(hover_proc), 2, Some(&mut hot)).as_bool() && hot >> 1 != 0
    }
}

pub(super) fn hovered_row(hwnd: HWND, index: u32) -> bool {
    let mut hot = 0;
    unsafe {
        GetWindowSubclass(hwnd, Some(hover_proc), 2, Some(&mut hot)).as_bool()
            && hot >> 1 == index as usize + 1
    }
}

fn invalidate_hover(hwnd: HWND, list: bool, row: usize) {
    if row == 0 {
        return;
    }
    unsafe {
        if list {
            let mut rect = RECT::default();
            if SendMessageW(
                hwnd,
                LB_GETITEMRECT,
                Some(WPARAM(row - 1)),
                Some(LPARAM(&mut rect as *mut RECT as isize)),
            )
            .0 >= 0
            {
                let _ = InvalidateRect(Some(hwnd), Some(&rect), false);
            }
        } else {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

unsafe extern "system" fn hover_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        let result = DefSubclassProc(hwnd, message, wparam, lparam);
        if matches!(
            message,
            WM_MOUSEMOVE
                | WM_MOUSELEAVE
                | WM_MOUSEWHEEL
                | WM_VSCROLL
                | LB_RESETCONTENT
                | LB_SETTOPINDEX
                | WM_KEYDOWN
                | WM_CHAR
        ) && (data & 1 != 0 || !matches!(message, WM_KEYDOWN | WM_CHAR))
        {
            let list = data & 1 != 0;
            let old = data >> 1;
            let mut row = 0;
            if !matches!(message, WM_MOUSELEAVE | LB_RESETCONTENT) {
                if list {
                    let mut cursor = POINT::default();
                    if GetCursorPos(&mut cursor).is_ok()
                        && ScreenToClient(hwnd, &mut cursor).as_bool()
                    {
                        let point = LPARAM(
                            ((cursor.y as u16 as u32) << 16 | cursor.x as u16 as u32) as isize,
                        );
                        let hit =
                            SendMessageW(hwnd, LB_ITEMFROMPOINT, None, Some(point)).0 as usize;
                        if hit >> 16 == 0 {
                            row = (hit & 0xffff) + 1;
                        }
                    }
                } else {
                    row = 1;
                }
            }
            if message == WM_MOUSEMOVE && old == 0 {
                let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                });
            }
            if old != row {
                let _ =
                    SetWindowSubclass(hwnd, Some(hover_proc), 2, (row << 1) | usize::from(list));
                invalidate_hover(hwnd, list, old);
                invalidate_hover(hwnd, list, row);
            }
        }
        result
    }
}

/// Compose background, highlight and text offscreen before copying the row.
unsafe fn draw_buffered(parent: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
        let rect = item.rcItem;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return LRESULT(1);
        }
        let dc = CreateCompatibleDC(Some(item.hDC));
        let bitmap = CreateCompatibleBitmap(item.hDC, width, height);
        if dc.0.is_null() || bitmap.0.is_null() {
            let _ = DeleteDC(dc);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            return SendMessageW(parent, WM_DRAWITEM, Some(wparam), Some(lparam));
        }
        let old = SelectObject(dc, HGDIOBJ(bitmap.0));
        let _ = SetViewportOrgEx(dc, -rect.left, -rect.top, None);
        // Empty owner-draw list notifications may have no row content.
        SetDCBrushColor(dc, background());
        FillRect(dc, &rect, HBRUSH(GetStockObject(DC_BRUSH).0));
        let mut buffered = *item;
        buffered.hDC = dc;
        let result = SendMessageW(
            parent,
            WM_DRAWITEM,
            Some(wparam),
            Some(LPARAM(&buffered as *const DRAWITEMSTRUCT as isize)),
        );
        let _ = BitBlt(
            item.hDC,
            rect.left,
            rect.top,
            width,
            height,
            Some(dc),
            rect.left,
            rect.top,
            SRCCOPY,
        );
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);
        result
    }
}

pub(super) fn create_panel(
    state: &SettingsState,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let panel = create_panel_for(state.hwnd, state.dpi, instance)?;
    set_font(panel, state.font);
    state.layouts.borrow_mut().push((
        panel,
        RECT {
            left: 0,
            top: 0,
            right: EDIT_WIDTH,
            bottom: 1,
        },
    ));
    Ok(panel)
}

pub(super) fn create_panel_for(
    parent: HWND,
    dpi: u32,
    instance: windows::Win32::Foundation::HMODULE,
) -> Result<HWND, String> {
    let panel = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!(""),
            WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
            0,
            0,
            platform::scale(EDIT_WIDTH, dpi),
            platform::scale(1, dpi),
            Some(parent),
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| error.to_string())?;
    let surface = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE,
            w!("STATIC"),
            w!(""),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            Some(parent),
            None,
            Some(instance.into()),
            None,
        )
    };
    let surface = match surface {
        Ok(surface) => surface,
        Err(error) => {
            unsafe {
                let _ = DestroyWindow(panel);
            }
            return Err(error.to_string());
        }
    };
    let data = Box::into_raw(Box::new(RefCell::new(PanelSurface {
        hwnd: surface,
        dpi,
        painted_size: None,
    })));
    unsafe {
        if !SetWindowSubclass(surface, Some(surface_proc), 1, 0).as_bool()
            || !SetWindowSubclass(panel, Some(panel_proc), 1, data as usize).as_bool()
        {
            drop(Box::from_raw(data));
            let _ = DestroyWindow(surface);
            let _ = DestroyWindow(panel);
            return Err("Unable to initialize dropdown panel".into());
        }
    }
    Ok(panel)
}

/// Returns the inner width, anchored to the button's actual bounds at any DPI.
pub(super) fn position(panel: HWND, button: HWND, height: i32, dpi: u32) -> i32 {
    position_pixels(panel, button, platform::scale(height, dpi), dpi)
}

pub(super) fn position_pixels(panel: HWND, button: HWND, height: i32, dpi: u32) -> i32 {
    unsafe {
        let mut data = 0;
        if GetWindowSubclass(panel, Some(panel_proc), 1, Some(&mut data)).as_bool() {
            let state = &*(data as *const RefCell<PanelSurface>);
            state.borrow_mut().dpi = dpi;
        }
        let mut bounds = RECT::default();
        let Ok(parent) = windows::Win32::UI::WindowsAndMessaging::GetParent(panel) else {
            return 0;
        };
        if GetWindowRect(button, &mut bounds).is_err() {
            return 0;
        }
        let mut origin = POINT {
            x: bounds.left,
            y: bounds.bottom,
        };
        if !ScreenToClient(parent, &mut origin).as_bool() {
            return 0;
        }
        let width = bounds.right - bounds.left;
        let height = height + platform::scale(PADDING * 2, dpi);
        let gap = platform::scale(6, dpi);
        let mut client = RECT::default();
        let _ = GetClientRect(parent, &mut client);
        // Audio fields near the footer need to open upward. Keep the entire menu
        // inside the settings content area instead of clipping its final rows.
        let below = origin.y + gap;
        let top = if below + height > client.bottom - platform::scale(FOOTER_HEIGHT, dpi) {
            (origin.y - (bounds.bottom - bounds.top) - gap - height)
                .max(platform::scale(HEADER_HEIGHT, dpi))
        } else {
            below
        };
        let _ = SetWindowPos(
            panel,
            Some(HWND_TOP),
            origin.x,
            top,
            width,
            height,
            SWP_NOACTIVATE,
        );
        let _ = InvalidateRect(Some(panel), None, true);
        (width - platform::scale(PADDING * 2, dpi)).max(1)
    }
}

pub(super) fn draw_row(hdc: HDC, rect: RECT, selected: bool, hot: bool, dpi: u32) {
    unsafe {
        SetDCBrushColor(hdc, background());
        FillRect(hdc, &rect, HBRUSH(GetStockObject(DC_BRUSH).0));
        if selected || hot {
            let color = if selected {
                rgb(40, 78, 73)
            } else {
                rgb(33, 49, 51)
            };
            let mut inset = rect;
            inset.top += platform::scale(2, dpi);
            inset.bottom -= platform::scale(2, dpi);
            rounded_box(hdc, inset, color, color, platform::scale(8, dpi));
        }
    }
}

unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::WM_MEASUREITEM;
    unsafe {
        match message {
            WM_NCDESTROY => {
                let _ = RemoveWindowSubclass(hwnd, Some(panel_proc), 1);
                let state = Box::from_raw(data as *mut RefCell<PanelSurface>).into_inner();
                if IsWindow(Some(state.hwnd)).as_bool() {
                    let _ = DestroyWindow(state.hwnd);
                }
                return DefSubclassProc(hwnd, message, wparam, lparam);
            }
            WM_WINDOWPOSCHANGED => {
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                let state = &*(data as *const RefCell<PanelSurface>);
                // SetWindowRgn may reenter this message while syncing.
                if let Ok(mut state) = state.try_borrow_mut() {
                    state.sync(hwnd);
                }
                return result;
            }
            WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
            WM_DRAWITEM => {
                if let Ok(parent) = GetParent(hwnd) {
                    return draw_buffered(parent, wparam, lparam);
                }
            }
            WM_COMMAND | WM_MEASUREITEM => {
                if let Ok(parent) = GetParent(hwnd) {
                    return SendMessageW(parent, message, Some(wparam), Some(lparam));
                }
            }
            WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
                let dc = HDC(wparam.0 as *mut c_void);
                SetBkColor(dc, background());
                SetDCBrushColor(dc, background());
                return LRESULT(GetStockObject(DC_BRUSH).0 as isize);
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                SetDCBrushColor(dc, background());
                FillRect(dc, &paint.rcPaint, HBRUSH(GetStockObject(DC_BRUSH).0));
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            _ => {}
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

unsafe extern "system" fn surface_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        match message {
            // Menu padding consumes clicks just as the original panel did.
            // Pixels with zero alpha fall through via layered hit testing.
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}
