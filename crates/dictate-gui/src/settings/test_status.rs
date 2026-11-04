//! A fixed-size, buffered child window owns test-icon animation and its timer.
use super::*;
use std::time::Instant;
use windows::Win32::UI::Shell::{
    DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SWP_NOZORDER, SWP_SHOWWINDOW, SetTimer, WM_NCDESTROY, WM_SIZE, WM_TIMER,
};

use crate::test_status_icons::{self, StatusIcon};

const SUBCLASS: usize = 0x7453;
const TIMER: usize = 1;

struct IconState {
    icon: Option<StatusIcon>,
    started: Instant,
    timer_running: bool,
}

pub(super) fn create(parent: HWND) -> Result<HWND, String> {
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("STATIC"),
            w!(""),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            1,
            1,
            Some(parent),
            None,
            None,
            None,
        )
    }
    .map_err(|error| error.to_string())?;
    let pointer = Box::into_raw(Box::new(IconState {
        icon: None,
        started: Instant::now(),
        timer_running: false,
    }));
    if !unsafe { SetWindowSubclass(hwnd, Some(icon_proc), SUBCLASS, pointer as usize) }.as_bool() {
        unsafe {
            drop(Box::from_raw(pointer));
            let _ = DestroyWindow(hwnd);
        }
        return Err("Unable to initialize test status icon".into());
    }
    Ok(hwnd)
}

fn state_pointer(hwnd: HWND) -> Option<*mut IconState> {
    let mut data = 0;
    unsafe { GetWindowSubclass(hwnd, Some(icon_proc), SUBCLASS, Some(&mut data)) }
        .as_bool()
        .then_some(data as *mut IconState)
}

pub(super) fn show(hwnd: HWND, status: TestResultState, rect: RECT) {
    let icon = match status {
        TestResultState::Idle => {
            hide(hwnd);
            return;
        }
        TestResultState::Testing => StatusIcon::Processing,
        TestResultState::Succeeded => StatusIcon::Success,
        TestResultState::Failed => StatusIcon::Failed,
    };
    let Some(pointer) = state_pointer(hwnd) else {
        return;
    };
    unsafe {
        if (*pointer).icon != Some(icon) {
            (*pointer).icon = Some(icon);
            (*pointer).started = Instant::now();
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
        );
        sync_timer(hwnd, &mut *pointer);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub(super) fn hide(hwnd: HWND) {
    if let Some(pointer) = state_pointer(hwnd) {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER);
            (*pointer).timer_running = false;
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn sync_timer(hwnd: HWND, state: &mut IconState) {
    let running =
        state.icon == Some(StatusIcon::Processing) && unsafe { IsWindowVisible(hwnd) }.as_bool();
    if running && !state.timer_running {
        state.timer_running = unsafe {
            SetTimer(
                Some(hwnd),
                TIMER,
                test_status_icons::FRAME_INTERVAL_MS,
                None,
            ) != 0
        };
    } else if !running && state.timer_running {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER);
        }
        state.timer_running = false;
    }
}

unsafe extern "system" fn icon_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    data: usize,
) -> LRESULT {
    match message {
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            paint_icon(dc, rect, &*(data as *const IconState));
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER if wparam.0 == TIMER => {
            let state = unsafe { &mut *(data as *mut IconState) };
            sync_timer(hwnd, state);
            if state.timer_running {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => unsafe {
            let _ = KillTimer(Some(hwnd), TIMER);
            let _ = RemoveWindowSubclass(hwnd, Some(icon_proc), SUBCLASS);
            drop(Box::from_raw(data as *mut IconState));
            DefSubclassProc(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

unsafe fn paint_icon(dc: HDC, rect: RECT, state: &IconState) {
    let item = DRAWITEMSTRUCT {
        hDC: dc,
        rcItem: rect,
        ..Default::default()
    };
    unsafe {
        dropdown::with_buffered_item(&item, |item| {
            fill_color(item.hDC, rect, rgb(16, 22, 25));
            if let Some(icon) = state.icon {
                let size = (rect.right - rect.left).min(rect.bottom - rect.top).max(1) as u32;
                let pixels = test_status_icons::pixels(icon, size, state.started.elapsed());
                let x = rect.left + (rect.right - rect.left - pixels.size as i32) / 2;
                let y = rect.top + (rect.bottom - rect.top - pixels.size as i32) / 2;
                let _ = crate::prompt_icons::draw_gdi(item.hDC, x, y, &pixels);
            }
            LRESULT(1)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{GetUpdateRect, ValidateRect};

    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn animation_updates_only_the_icon_without_changing_focus_or_bounds() {
        unsafe {
            let parent = TestWindow(
                CreateWindowExW(
                    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP | WS_CLIPCHILDREN,
                    0,
                    0,
                    160,
                    100,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let _ = ShowWindow(parent.0, SW_SHOWNOACTIVATE);
            let focus = GetFocus();
            let icon = create(parent.0).unwrap();
            let rect = RECT {
                left: 10,
                top: 12,
                right: 40,
                bottom: 42,
            };
            show(icon, TestResultState::Testing, rect);
            let pointer = state_pointer(icon).unwrap();
            assert!((*pointer).timer_running);
            let started = (*pointer).started;
            let mut before = RECT::default();
            GetWindowRect(icon, &mut before).unwrap();
            assert!(ValidateRect(Some(parent.0), None).as_bool());
            assert!(ValidateRect(Some(icon), None).as_bool());
            SendMessageW(icon, WM_TIMER, Some(WPARAM(TIMER)), None);
            assert!(GetUpdateRect(icon, None, false).as_bool());
            assert!(!GetUpdateRect(parent.0, None, false).as_bool());
            let mut after = RECT::default();
            GetWindowRect(icon, &mut after).unwrap();
            assert_eq!(
                [before.left, before.top, before.right, before.bottom],
                [after.left, after.top, after.right, after.bottom]
            );
            assert_eq!(GetFocus(), focus);

            hide(icon);
            assert!(!(*pointer).timer_running && !IsWindowVisible(icon).as_bool());
            show(icon, TestResultState::Testing, rect);
            assert!((*pointer).timer_running);
            assert_eq!((*pointer).started, started);
            for status in [TestResultState::Succeeded, TestResultState::Failed] {
                show(icon, status, rect);
                assert!(!(*pointer).timer_running);
                assert!(IsWindowVisible(icon).as_bool());
            }
            show(icon, TestResultState::Idle, rect);
            assert!(!(*pointer).timer_running && !IsWindowVisible(icon).as_bool());
        }
    }
}
