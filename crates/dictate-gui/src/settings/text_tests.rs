//! Native regression coverage for settings text and buffered row painting.
//! These tests run on Windows; cross compilation alone does not execute GDI.
use super::*;
use dictate_core::rewrite::RewritePrompt;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DT_RIGHT, DeleteDC, GetBkMode, GetCurrentObject,
    GetDC, GetPixel, GetTextColor, HBITMAP, IntersectClipRect, OBJ_FONT, ReleaseDC, SYSTEM_FONT,
    SelectClipRgn,
};

struct TestWindow(HWND);

impl Drop for TestWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.0);
        }
    }
}

struct TestDc(HDC);

struct TestBitmap {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Drop for TestBitmap {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
        }
    }
}

impl Drop for TestDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

#[test]
fn buffered_row_is_presented_once_and_respects_spinner_damage_clipping() {
    unsafe {
        let screen = GetDC(None);
        let dc = TestDc(CreateCompatibleDC(Some(screen)));
        let bitmap = CreateCompatibleBitmap(screen, 340, 110);
        ReleaseDC(None, screen);
        assert!(!dc.0.is_invalid() && !bitmap.is_invalid());
        let _bitmap = TestBitmap {
            dc: dc.0,
            bitmap,
            previous: SelectObject(dc.0, HGDIOBJ(bitmap.0)),
        };
        let before = rgb(11, 27, 39);
        let after = rgb(47, 183, 119);
        fill_color(
            dc.0,
            RECT {
                left: 0,
                top: 0,
                right: 340,
                bottom: 110,
            },
            before,
        );
        IntersectClipRect(dc.0, 220, 40, 240, 60);
        let item = DRAWITEMSTRUCT {
            hDC: dc.0,
            rcItem: RECT {
                left: 10,
                top: 30,
                right: 310,
                bottom: 90,
            },
            ..Default::default()
        };
        dropdown::with_buffered_item(&item, |buffered| {
            assert_ne!(buffered.hDC, item.hDC);
            fill_color(buffered.hDC, buffered.rcItem, after);
            assert_eq!(
                GetPixel(item.hDC, 230, 50),
                before,
                "row construction must not reach the visible DC"
            );
            LRESULT(1)
        });
        SelectClipRgn(dc.0, None);
        assert_eq!(GetPixel(dc.0, 230, 50), after);
        assert_eq!(
            GetPixel(dc.0, 20, 40),
            before,
            "filename pixels outside animation damage must stay unchanged"
        );
        assert_eq!(GetPixel(dc.0, 320, 100), before);
    }
}

#[test]
fn cleared_prompt_hotkey_saves_and_paints_without_touching_gdi() {
    unsafe {
        let parent = TestWindow(
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                320,
                64,
                None,
                None,
                None,
                None,
            )
            .unwrap(),
        );
        let field = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WS_CHILD,
            0,
            0,
            160,
            32,
            Some(parent.0),
            None,
            None,
            None,
        )
        .unwrap();
        hotkeys::attach_optional(
            field,
            "ctrl+numpad1".into(),
            Language::English,
            parent.0,
            false,
        )
        .unwrap();
        hotkeys::clear(field);
        let mut config = Config::default();
        config.rewrite.prompts.push(RewritePrompt {
            title: "Menu only prompt".into(),
            prompt: "Rewrite the selected text".into(),
            hotkey: hotkeys::value(field).unwrap(),
            ..Default::default()
        });
        config.rewrite.validate_prompts().unwrap();
        hotkeys::validate_draft(&config, Language::English).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        config.save(&path).unwrap();
        let saved = Config::load(&path).unwrap();
        let hotkey = &saved.rewrite.prompts[0].hotkey;
        assert!(hotkey.is_empty());

        // Use a real memory DC so removing the empty-text guards reaches the
        // same DrawTextW call used when the settings prompt list repaints.
        let dc = TestDc(CreateCompatibleDC(None));
        assert!(!dc.0.is_invalid());
        let font = HFONT(GetStockObject(SYSTEM_FONT).0);
        let before = (
            GetBkMode(dc.0),
            GetTextColor(dc.0),
            GetCurrentObject(dc.0, OBJ_FONT),
            GetPixel(dc.0, 0, 0),
        );
        let rect = RECT {
            left: 0,
            top: 0,
            right: 160,
            bottom: 32,
        };
        draw_text_line(dc.0, font, hotkey, rgb(150, 180, 180), rect, DT_RIGHT);
        draw_text_wrapped(dc.0, font, hotkey, rgb(150, 180, 180), rect);
        let after = (
            GetBkMode(dc.0),
            GetTextColor(dc.0),
            GetCurrentObject(dc.0, OBJ_FONT),
            GetPixel(dc.0, 0, 0),
        );
        assert_eq!(after, before);
    }
}
