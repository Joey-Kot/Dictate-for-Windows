//! Shared themed scrollbars for settings lists, multiline text fields, and viewports.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    height: i32,
    thumb: i32,
    top: i32,
    max: i32,
    page: i32,
}

impl Metrics {
    fn new(height: i32, row: i32, count: i32, index: i32, min_thumb: i32) -> Self {
        Self::with_page(height, height / row.max(1), count, index, min_thumb)
    }

    fn with_page(height: i32, page: i32, count: i32, index: i32, min_thumb: i32) -> Self {
        let height = height.max(1);
        let page = page.max(1);
        let max = (count - page).max(0);
        let thumb = ((i64::from(height) * i64::from(page) / i64::from(count.max(1))) as i32)
            .clamp(min_thumb.min(height), height);
        let top = if max == 0 {
            0
        } else {
            ((height - thumb) as i64 * index.clamp(0, max) as i64 / max as i64) as i32
        };
        Self {
            height,
            thumb,
            top,
            max,
            page,
        }
    }

    fn index_at(self, y: i32) -> i32 {
        let travel = self.height - self.thumb;
        if travel == 0 {
            0
        } else {
            ((y.clamp(0, travel) as i64 * self.max as i64 + travel as i64 / 2) / travel as i64)
                as i32
        }
    }
}

#[cfg(windows)]
pub(crate) use control::*;

#[cfg(windows)]
mod control {
    use super::Metrics;
    use crate::settings::*;
    use windows::Win32::Graphics::Gdi::{GetDC, GetTextMetricsW, ReleaseDC, TEXTMETRICW};
    use windows::Win32::UI::Controls::{
        EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_GETRECT, EM_LINESCROLL, EM_REPLACESEL,
        EM_SCROLL, EM_SCROLLCARET, EM_SETRECTNP, EM_SETSEL, WM_MOUSELEAVE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_LEFT, VK_RIGHT,
    };
    use windows::Win32::UI::Shell::{
        DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    const WHEEL: u32 = WM_APP + 28;
    const UPDATE_DPI: u32 = WM_APP + 29;
    const SET_OFFSET: u32 = WM_APP + 30;
    // Viewport owners report their content height and current scroll offset in
    // physical pixels. SET_OFFSET asks the owner to relayout its own children.
    pub(crate) const VIEWPORT_GET_EXTENT: u32 = WM_APP + 31;
    pub(crate) const VIEWPORT_GET_OFFSET: u32 = WM_APP + 32;
    pub(crate) const VIEWPORT_SET_OFFSET: u32 = WM_APP + 33;

    #[derive(Clone, Copy)]
    struct State {
        control: HWND,
        dpi: u32,
        drag: Option<i32>,
        hot: bool,
        wheel: i32,
        horizontal: bool,
        multiline: bool,
        viewport: bool,
        viewport_background: COLORREF,
        extent: i32,
        offset: i32,
    }

    pub(crate) fn create(
        state: &SettingsState,
        panel: HWND,
        list: HWND,
        instance: windows::Win32::Foundation::HMODULE,
    ) -> Result<HWND, String> {
        create_axis(state, panel, list, instance, false)
    }

    pub(crate) fn create_horizontal(
        state: &SettingsState,
        panel: HWND,
        list: HWND,
        instance: windows::Win32::Foundation::HMODULE,
    ) -> Result<HWND, String> {
        create_axis(state, panel, list, instance, true)
    }

    fn create_axis(
        state: &SettingsState,
        panel: HWND,
        list: HWND,
        instance: windows::Win32::Foundation::HMODULE,
        horizontal: bool,
    ) -> Result<HWND, String> {
        let bar = create_child(state, w!("STATIC"), "", WS_CHILD, 0, 0, 1, 1, 0, instance)?;
        unsafe {
            SetParent(bar, Some(panel)).map_err(|e| e.to_string())?;
        }
        attach_bar(bar, list, state.dpi, horizontal, false, None)?;
        Ok(bar)
    }

    pub(crate) fn attach_edit(edit: HWND, dpi: u32) -> Result<(), String> {
        let bar = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!(""),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                Some(edit),
                None,
                None,
                None,
            )
        }
        .map_err(|error| error.to_string())?;
        attach_bar(bar, edit, dpi, false, true, None)?;
        unsafe {
            layout_edit(edit, bar);
        }
        Ok(())
    }

    /// The caller handles the VIEWPORT_* messages, positioning, and visibility.
    /// No subclass is installed on the viewport itself.
    pub(crate) fn attach_viewport(viewport: HWND, dpi: u32) -> Result<HWND, String> {
        attach_viewport_with_background(viewport, dpi, rgb(16, 22, 25))
    }

    pub(crate) fn attach_viewport_with_background(
        viewport: HWND,
        dpi: u32,
        background: COLORREF,
    ) -> Result<HWND, String> {
        let bar = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!(""),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                Some(viewport),
                None,
                None,
                None,
            )
        }
        .map_err(|error| error.to_string())?;
        attach_bar(bar, viewport, dpi, false, false, Some(background))?;
        Ok(bar)
    }

    pub(crate) fn edit_at_bottom(edit: HWND) -> bool {
        unsafe {
            let mut bar = 0;
            let mut data = 0;
            if !GetWindowSubclass(edit, Some(edit_proc), 5, Some(&mut bar)).as_bool()
                || !GetWindowSubclass(HWND(bar as *mut c_void), Some(bar_proc), 1, Some(&mut data))
                    .as_bool()
            {
                return true;
            }
            let state = *(data as *const State);
            index(state) >= metrics(HWND(bar as *mut c_void), state).max
        }
    }

    fn attach_bar(
        bar: HWND,
        control: HWND,
        dpi: u32,
        horizontal: bool,
        multiline: bool,
        viewport_background: Option<COLORREF>,
    ) -> Result<(), String> {
        unsafe {
            let viewport = viewport_background.is_some();
            let data = Box::into_raw(Box::new(State {
                control,
                dpi,
                drag: None,
                hot: false,
                wheel: 0,
                horizontal,
                multiline,
                viewport,
                viewport_background: viewport_background.unwrap_or_else(dropdown::background),
                extent: 0,
                offset: 0,
            }));
            if !SetWindowSubclass(bar, Some(bar_proc), 1, data as usize).as_bool() {
                drop(Box::from_raw(data));
                let _ = DestroyWindow(bar);
                return Err("Unable to initialize settings scrollbar".into());
            }
            if !viewport
                && !SetWindowSubclass(
                    control,
                    if multiline {
                        Some(edit_proc)
                    } else {
                        Some(list_proc)
                    },
                    if multiline {
                        5
                    } else if horizontal {
                        4
                    } else {
                        3
                    },
                    bar.0 as usize,
                )
                .as_bool()
            {
                let _ = DestroyWindow(bar);
                return Err("Unable to synchronize settings scrollbar".into());
            }
        }
        Ok(())
    }

    pub(crate) fn thickness(dpi: u32) -> i32 {
        platform::scale(14, dpi)
    }

    pub(crate) fn horizontal_offset(bar: HWND) -> i32 {
        unsafe {
            let mut data = 0;
            if GetWindowSubclass(bar, Some(bar_proc), 1, Some(&mut data)).as_bool() {
                (*(data as *const State)).offset
            } else {
                0
            }
        }
    }

    pub(crate) fn position_horizontal(bar: HWND, width: i32, height: i32, dpi: u32, extent: i32) {
        unsafe {
            SendMessageW(
                bar,
                UPDATE_DPI,
                Some(WPARAM(dpi as usize)),
                Some(LPARAM(extent as isize)),
            );
            let _ = SetWindowPos(
                bar,
                Some(HWND_TOP),
                platform::scale(8, dpi),
                platform::scale(8, dpi) + height,
                width,
                thickness(dpi),
                SWP_NOACTIVATE,
            );
            // Clamp the offset when devices, scale, or available width change.
            SendMessageW(
                bar,
                SET_OFFSET,
                Some(WPARAM(horizontal_offset(bar) as usize)),
                None,
            );
            let _ = ShowWindow(bar, if extent > width { SW_SHOW } else { SW_HIDE });
        }
    }

    pub(crate) fn position(bar: HWND, width: i32, height: i32, dpi: u32, visible: bool) -> i32 {
        let gutter = if visible { thickness(dpi) } else { 0 };
        let left = platform::scale(8, dpi) + width - gutter;
        let top = platform::scale(8, dpi);
        position_rect(
            bar,
            RECT {
                left,
                top,
                right: left + gutter.max(1),
                bottom: top + height,
            },
            dpi,
        );
        unsafe {
            let _ = ShowWindow(bar, if visible { SW_SHOW } else { SW_HIDE });
        }
        width - gutter
    }

    /// Position a vertical bar in parent-client pixels; the caller owns visibility.
    pub(crate) fn position_rect(bar: HWND, rect: RECT, dpi: u32) {
        unsafe {
            SendMessageW(bar, UPDATE_DPI, Some(WPARAM(dpi as usize)), None);
            let _ = SetWindowPos(
                bar,
                Some(HWND_TOP),
                rect.left,
                rect.top,
                (rect.right - rect.left).max(1),
                (rect.bottom - rect.top).max(1),
                SWP_NOACTIVATE,
            );
        }
    }

    unsafe fn metrics(hwnd: HWND, state: State) -> Metrics {
        unsafe {
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            if state.horizontal {
                return Metrics::new(
                    rect.right,
                    1,
                    state.extent,
                    state.offset,
                    platform::scale(24, state.dpi),
                );
            }
            if state.viewport {
                let mut client = RECT::default();
                let _ = GetClientRect(state.control, &mut client);
                return Metrics::with_page(
                    rect.bottom,
                    client.bottom,
                    SendMessageW(state.control, VIEWPORT_GET_EXTENT, None, None).0 as i32,
                    index(state),
                    platform::scale(24, state.dpi),
                );
            }
            if state.multiline {
                let mut format = RECT::default();
                SendMessageW(
                    state.control,
                    EM_GETRECT,
                    None,
                    Some(LPARAM(&mut format as *mut RECT as isize)),
                );
                let dc = GetDC(Some(state.control));
                let font = SendMessageW(state.control, WM_GETFONT, None, None);
                let old = SelectObject(dc, HGDIOBJ(font.0 as *mut c_void));
                let mut text = TEXTMETRICW::default();
                let measured = GetTextMetricsW(dc, &mut text).as_bool();
                SelectObject(dc, old);
                ReleaseDC(Some(state.control), dc);
                let line_height = if measured {
                    text.tmHeight.max(1)
                } else {
                    platform::scale(18, state.dpi)
                };
                return Metrics::with_page(
                    rect.bottom,
                    (format.bottom - format.top) / line_height,
                    SendMessageW(state.control, EM_GETLINECOUNT, None, None).0 as i32,
                    index(state),
                    platform::scale(24, state.dpi),
                );
            }
            Metrics::new(
                rect.bottom,
                SendMessageW(state.control, LB_GETITEMHEIGHT, Some(WPARAM(0)), None).0 as i32,
                SendMessageW(state.control, LB_GETCOUNT, None, None).0 as i32,
                index(state),
                platform::scale(24, state.dpi),
            )
        }
    }

    unsafe fn scroll(hwnd: HWND, state: State, index: i32) {
        unsafe {
            let m = metrics(hwnd, state);
            if state.horizontal {
                SendMessageW(
                    hwnd,
                    SET_OFFSET,
                    Some(WPARAM(index.clamp(0, m.max) as usize)),
                    None,
                );
                return;
            }
            if state.viewport {
                SendMessageW(
                    state.control,
                    VIEWPORT_SET_OFFSET,
                    Some(WPARAM(index.clamp(0, m.max) as usize)),
                    None,
                );
            } else if state.multiline {
                let top = SendMessageW(state.control, EM_GETFIRSTVISIBLELINE, None, None).0 as i32;
                SendMessageW(
                    state.control,
                    EM_LINESCROLL,
                    None,
                    Some(LPARAM((index.clamp(0, m.max) - top) as isize)),
                );
            } else {
                SendMessageW(
                    state.control,
                    LB_SETTOPINDEX,
                    Some(WPARAM(index.clamp(0, m.max) as usize)),
                    None,
                );
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }

    unsafe fn layout_edit(edit: HWND, bar: HWND) {
        unsafe {
            let dpi = platform::window_dpi(edit);
            SendMessageW(bar, UPDATE_DPI, Some(WPARAM(dpi as usize)), None);
            let mut client = RECT::default();
            let _ = GetClientRect(edit, &mut client);
            let gutter = thickness(dpi);
            // Reserve a stable gutter so showing the thumb cannot reflow the text.
            // Reset first: repeated font/DPI changes must not shrink the rectangle.
            SendMessageW(edit, EM_SETRECTNP, None, None);
            let mut format = RECT::default();
            SendMessageW(
                edit,
                EM_GETRECT,
                None,
                Some(LPARAM(&mut format as *mut RECT as isize)),
            );
            format.right = (format.right - gutter).max(format.left + 1);
            SendMessageW(
                edit,
                EM_SETRECTNP,
                None,
                Some(LPARAM(&format as *const RECT as isize)),
            );
            let _ = SetWindowPos(
                bar,
                Some(HWND_TOP),
                (client.right - gutter).max(0),
                0,
                gutter,
                client.bottom.max(1),
                SWP_NOACTIVATE,
            );
            let _ = InvalidateRect(Some(edit), None, false);
            sync_edit(bar);
        }
    }

    unsafe fn sync_edit(bar: HWND) {
        unsafe {
            let mut data = 0;
            if !GetWindowSubclass(bar, Some(bar_proc), 1, Some(&mut data)).as_bool() {
                return;
            }
            let state = *(data as *const State);
            let visible = metrics(bar, state).max > 0;
            let shown = GetWindowLongPtrW(bar, GWL_STYLE) as u32 & WS_VISIBLE.0 != 0;
            if visible != shown {
                let _ = ShowWindow(bar, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            let _ = InvalidateRect(Some(bar), None, false);
        }
    }

    unsafe extern "system" fn edit_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        data: usize,
    ) -> LRESULT {
        unsafe {
            let bar = HWND(data as *mut c_void);
            if message == WM_NCDESTROY {
                let _ = RemoveWindowSubclass(hwnd, Some(edit_proc), id);
                return DefSubclassProc(hwnd, message, wparam, lparam);
            }
            if message == WM_MOUSEWHEEL {
                return SendMessageW(bar, WHEEL, Some(wparam), Some(lparam));
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if matches!(message, WM_SIZE | WM_SETFONT) {
                layout_edit(hwnd, bar);
            } else if matches!(
                message,
                WM_KEYDOWN
                    | WM_CHAR
                    | WM_VSCROLL
                    | WM_TIMER
                    | WM_MOUSEMOVE
                    | WM_LBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_SETTEXT
                    | WM_CUT
                    | WM_PASTE
                    | WM_CLEAR
                    | WM_UNDO
                    | WM_PAINT
                    | EM_REPLACESEL
                    | EM_SETSEL
                    | EM_SCROLL
                    | EM_LINESCROLL
                    | EM_SCROLLCARET
            ) {
                sync_edit(bar);
            }
            result
        }
    }

    unsafe extern "system" fn list_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        id: usize,
        data: usize,
    ) -> LRESULT {
        unsafe {
            let bar = HWND(data as *mut c_void);
            let horizontal = id == 4;
            if horizontal && message == WM_MOUSEHWHEEL {
                return SendMessageW(bar, message, Some(wparam), Some(lparam));
            }
            if horizontal
                && message == WM_KEYDOWN
                && [VK_LEFT.0 as usize, VK_RIGHT.0 as usize].contains(&wparam.0)
            {
                return SendMessageW(bar, message, Some(wparam), Some(lparam));
            }
            if message == WM_MOUSEWHEEL && (!horizontal || wparam.0 & 4 != 0) {
                return SendMessageW(bar, WHEEL, Some(wparam), Some(lparam));
            }
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            if matches!(
                message,
                WM_KEYDOWN
                    | WM_CHAR
                    | WM_VSCROLL
                    | WM_TIMER
                    | WM_MOUSEMOVE
                    | WM_SIZE
                    | LB_SETTOPINDEX
                    | LB_SETCURSEL
                    | LB_RESETCONTENT
                    | LB_ADDSTRING
                    | LB_SETITEMHEIGHT
            ) {
                let _ = InvalidateRect(Some(bar), None, false);
            }
            if message == WM_NCDESTROY {
                let _ = RemoveWindowSubclass(hwnd, Some(list_proc), id);
            }
            result
        }
    }

    unsafe extern "system" fn bar_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        data: usize,
    ) -> LRESULT {
        unsafe {
            let ptr = data as *mut State;
            // Do not hold a mutable reference across synchronous window messages.
            let state = *ptr;
            match message {
                WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
                WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
                UPDATE_DPI => {
                    (*ptr).dpi = wparam.0 as u32;
                    (*ptr).extent = lparam.0 as i32;
                    return LRESULT(0);
                }
                SET_OFFSET => {
                    (*ptr).offset = (wparam.0 as i32).clamp(0, metrics(hwnd, state).max);
                    let _ = InvalidateRect(Some(state.control), None, false);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                WM_KEYDOWN if state.horizontal => {
                    let step = platform::scale(24, state.dpi);
                    let delta = if wparam.0 == VK_LEFT.0 as usize {
                        -step
                    } else {
                        step
                    };
                    scroll(hwnd, state, state.offset + delta);
                    return LRESULT(0);
                }
                WM_ERASEBKGND => return LRESULT(1),
                WM_PAINT => {
                    let m = metrics(hwnd, state);
                    let mut paint = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut paint);
                    let mut rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rect);
                    SetDCBrushColor(
                        dc,
                        if state.viewport {
                            state.viewport_background
                        } else if state.multiline {
                            rgb(26, 35, 39)
                        } else {
                            dropdown::background()
                        },
                    );
                    FillRect(dc, &rect, HBRUSH(GetStockObject(DC_BRUSH).0));
                    if m.max > 0 {
                        rect.left += platform::scale(4, state.dpi);
                        rect.right -= platform::scale(3, state.dpi);
                        rect.top = m.top;
                        rect.bottom = m.top + m.thumb;
                        if state.horizontal {
                            rect = RECT {
                                left: m.top,
                                right: m.top + m.thumb,
                                top: platform::scale(4, state.dpi),
                                bottom: thickness(state.dpi) - platform::scale(3, state.dpi),
                            };
                        }
                        let color = if state.drag.is_some() {
                            rgb(113, 210, 195)
                        } else if state.hot {
                            rgb(116, 147, 148)
                        } else {
                            rgb(70, 96, 99)
                        };
                        rounded_box(dc, rect, color, color, platform::scale(4, state.dpi));
                    }
                    let _ = EndPaint(hwnd, &paint);
                    return LRESULT(0);
                }
                WM_LBUTTONDOWN => {
                    let y = axis_point(state, lparam);
                    let m = metrics(hwnd, state);
                    if m.max > 0 {
                        if y >= m.top && y < m.top + m.thumb {
                            (*ptr).drag = Some(y - m.top);
                            SetCapture(hwnd);
                        } else {
                            let top = index(state);
                            scroll(hwnd, state, top + if y < m.top { -m.page } else { m.page });
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    return LRESULT(0);
                }
                WM_MOUSEMOVE => {
                    if !state.hot {
                        (*ptr).hot = true;
                        let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        });
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    if let Some(offset) = state.drag {
                        let y = axis_point(state, lparam);
                        scroll(hwnd, state, metrics(hwnd, state).index_at(y - offset));
                    }
                    return LRESULT(0);
                }
                WM_LBUTTONUP | WM_CAPTURECHANGED | WM_CANCELMODE => {
                    (*ptr).drag = None;
                    if state.drag.is_some() && message != WM_CAPTURECHANGED {
                        let _ = ReleaseCapture();
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                WM_MOUSELEAVE => {
                    (*ptr).hot = false;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }
                WM_SHOWWINDOW if wparam.0 == 0 => {
                    (*ptr).drag = None;
                    (*ptr).hot = false;
                    (*ptr).wheel = 0;
                    if state.drag.is_some() {
                        let _ = ReleaseCapture();
                    }
                }
                WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WHEEL => {
                    let direction = if message == WM_MOUSEHWHEEL { -1 } else { 1 };
                    let delta = direction * (wparam.0 >> 16) as i16 as i32 + state.wheel;
                    (*ptr).wheel = delta % 120;
                    let mut lines: u32 = 3;
                    let _ = SystemParametersInfoW(
                        if state.horizontal {
                            SPI_GETWHEELSCROLLCHARS
                        } else {
                            SPI_GETWHEELSCROLLLINES
                        },
                        0,
                        Some(&mut lines as *mut u32 as *mut c_void),
                        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                    );
                    let m = metrics(hwnd, state);
                    let step = if lines == u32::MAX {
                        m.page
                    } else {
                        let unit = if state.horizontal {
                            platform::scale(12, state.dpi) as u32
                        } else if state.viewport {
                            platform::scale(18, state.dpi) as u32
                        } else {
                            1
                        };
                        lines.saturating_mul(unit).min(m.page as u32) as i32
                    };
                    let top = index(state);
                    scroll(hwnd, state, top - delta / 120 * step);
                    return LRESULT(0);
                }
                WM_NCDESTROY => {
                    let _ = RemoveWindowSubclass(hwnd, Some(bar_proc), 1);
                    drop(Box::from_raw(ptr));
                }
                _ => {}
            }
            DefSubclassProc(hwnd, message, wparam, lparam)
        }
    }

    fn axis_point(state: State, point: LPARAM) -> i32 {
        (if state.horizontal {
            point.0
        } else {
            point.0 >> 16
        }) as i16 as i32
    }

    unsafe fn index(state: State) -> i32 {
        if state.horizontal {
            state.offset
        } else {
            unsafe {
                SendMessageW(
                    state.control,
                    if state.viewport {
                        VIEWPORT_GET_OFFSET
                    } else if state.multiline {
                        EM_GETFIRSTVISIBLELINE
                    } else {
                        LB_GETTOPINDEX
                    },
                    None,
                    None,
                )
                .0 as i32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_endpoints_and_drag_clamping_at_multiple_scales() {
        for scale in [1, 2, 3] {
            for count in [7, 19, 40] {
                let m = Metrics::new(216 * scale, 36 * scale, count, 0, 24 * scale);
                assert_eq!(m.page, 6);
                assert_eq!(m.top, 0);
                assert_eq!(m.index_at(-100), 0);
                assert_eq!(m.index_at(m.height), count - 6);
                let end = Metrics::new(m.height, 36 * scale, count, count, 24 * scale);
                assert_eq!(end.top + end.thumb, end.height);
                assert_eq!(end.index_at(end.top), end.max);
            }
        }
    }

    #[test]
    fn short_or_empty_lists_have_no_scroll_range() {
        for count in 0..=6 {
            let m = Metrics::new(216, 36, count, 0, 24);
            assert_eq!(m.max, 0);
            assert_eq!(m.thumb, m.height);
            assert_eq!(m.index_at(100), 0);
        }
    }

    #[test]
    fn horizontal_pixel_range_tracks_long_text_and_viewport_resizing() {
        for width in [320, 400, 480, 640] {
            let start = Metrics::new(width, 1, 1200, 0, 24);
            assert_eq!(start.page, width);
            assert_eq!(start.max, 1200 - width);
            assert_eq!(start.index_at(start.height), start.max);
            let end = Metrics::new(width, 1, 1200, 1200, 24);
            assert_eq!(end.top + end.thumb, width);
            assert_eq!(end.index_at(end.top), 1200 - width);
            let shortened = Metrics::new(width, 1, 200, end.max, 24);
            assert_eq!(shortened.max, 0);
            assert_eq!(shortened.top, 0);
            assert_eq!(shortened.thumb, width);
        }
    }

    #[test]
    fn text_scroll_range_uses_visible_lines_independently_of_track_height() {
        for (height, minimum_thumb) in [(120, 24), (180, 36), (240, 48)] {
            let m = Metrics::with_page(height, 6, 40, 0, minimum_thumb);
            assert_eq!(m.index_at(-100), 0);
            assert_eq!(m.index_at(height + 100), 34);
            let end = Metrics::with_page(height, 6, 40, 34, minimum_thumb);
            assert_eq!(end.top + end.thumb, height);

            // Rewrapping or deleting text clamps an old position to the new end.
            let reflow = Metrics::with_page(height, 8, 12, 34, minimum_thumb);
            assert_eq!(reflow.index_at(reflow.top), 4);
            let short = Metrics::with_page(height, 8, 4, 34, minimum_thumb);
            assert_eq!((short.max, short.top, short.thumb), (0, 0, height));
        }
    }

    #[test]
    fn viewport_pixel_scroll_range_tracks_page_size_and_clamps_after_resize() {
        for dpi in [96, 120, 144, 192] {
            let pixels = |logical: i32| logical * dpi / 96;
            // The track can be inset without reducing the visible content page.
            let track = pixels(480);
            let page = pixels(500);
            let extent = pixels(1300);
            let start = Metrics::with_page(track, page, extent, 0, pixels(24));
            assert_eq!(start.page, page);
            assert_eq!(start.max, extent - page);
            assert_eq!(start.index_at(-100), 0);
            assert_eq!(start.index_at(track + 100), extent - page);
            let end = Metrics::with_page(track, page, extent, extent, pixels(24));
            assert_eq!(end.top + end.thumb, track);
            assert_eq!(end.index_at(end.top), extent - page);

            // A taller viewport or shorter content clamps an existing offset.
            let resized = Metrics::with_page(track, pixels(1100), extent, end.max, pixels(24));
            assert_eq!(resized.index_at(resized.top), pixels(200));
            let fits = Metrics::with_page(track, extent, extent, end.max, pixels(24));
            assert_eq!((fits.max, fits.top, fits.thumb), (0, 0, track));
        }
    }
}
