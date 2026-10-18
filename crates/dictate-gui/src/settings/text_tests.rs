//! Native regression coverage for the optional-hotkey Clear -> Save -> paint path.
//! These tests run on Windows; cross compilation alone does not execute GDI.
use super::*;
use dictate_core::rewrite::RewritePrompt;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DT_RIGHT, DeleteDC, GetBkMode, GetCurrentObject, GetPixel, GetTextColor,
    OBJ_FONT, SYSTEM_FONT,
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

impl Drop for TestDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
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
        hotkeys::attach_optional(field, "ctrl+numpad1".into(), Language::English, parent.0)
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
