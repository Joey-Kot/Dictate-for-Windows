use std::ffi::c_void;
use std::mem::{size_of, size_of_val};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::{
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::{PCWSTR, w};

use super::backdrop;

#[derive(Clone, Copy)]
pub(super) struct Theme {
    pub dark: bool,
    pub high_contrast: bool,
    pub transparency: bool,
    pub surface: D2D1_COLOR_F,
    pub foreground: D2D1_COLOR_F,
    pub separator: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    pub hover: D2D1_COLOR_F,
    pub pressed: D2D1_COLOR_F,
    pub track: D2D1_COLOR_F,
    pub thumb: D2D1_COLOR_F,
}

pub(super) fn color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

fn registry_flag(name: PCWSTR, default: bool) -> bool {
    let mut value = u32::from(default);
    let mut bytes = size_of_val(&value) as u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut bytes),
        );
    }
    value != 0
}

impl Theme {
    pub fn read() -> Self {
        let mut high = HIGHCONTRASTW {
            cbSize: size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                high.cbSize,
                Some((&mut high as *mut HIGHCONTRASTW).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
        }
        let high_contrast = high.dwFlags.contains(HCF_HIGHCONTRASTON);
        let dark = !registry_flag(w!("AppsUseLightTheme"), true);
        let mut power = SYSTEM_POWER_STATUS::default();
        let battery_saver =
            unsafe { GetSystemPowerStatus(&mut power) }.is_ok() && power.SystemStatusFlag == 1;
        let mut result = Self {
            dark,
            high_contrast,
            transparency: registry_flag(w!("EnableTransparency"), true)
                && !high_contrast
                && !battery_saver,
            surface: if dark {
                color(32, 34, 39, 1.0)
            } else {
                color(247, 247, 247, 1.0)
            },
            foreground: if dark {
                color(245, 246, 249, 1.0)
            } else {
                color(16, 18, 22, 1.0)
            },
            separator: if dark {
                color(222, 227, 236, 0.12)
            } else {
                color(76, 89, 110, 0.10)
            },
            border: if dark {
                color(230, 235, 245, 0.24)
            } else {
                color(255, 255, 255, 0.78)
            },
            hover: if dark {
                color(255, 255, 255, 0.055)
            } else {
                color(255, 255, 255, 0.22)
            },
            pressed: if dark {
                color(255, 255, 255, 0.085)
            } else {
                color(44, 58, 79, 0.065)
            },
            track: if dark {
                color(210, 220, 235, 0.12)
            } else {
                color(80, 99, 125, 0.08)
            },
            thumb: if dark {
                color(215, 225, 240, 0.34)
            } else {
                color(80, 99, 125, 0.31)
            },
        };
        if high_contrast {
            let system_color = |index| {
                let c = unsafe { GetSysColor(index) };
                color(c as u8, (c >> 8) as u8, (c >> 16) as u8, 1.0)
            };
            result.surface = system_color(COLOR_WINDOW);
            result.foreground = system_color(COLOR_WINDOWTEXT);
            result.separator = result.foreground;
            result.border = result.foreground;
            result.track = result.surface;
            result.thumb = result.foreground;
            result.hover = system_color(COLOR_HIGHLIGHT);
            result.pressed = result.hover;
        }
        result
    }

    pub fn hovered_foreground(self) -> D2D1_COLOR_F {
        if self.high_contrast {
            let c = unsafe { GetSysColor(COLOR_HIGHLIGHTTEXT) };
            color(c as u8, (c >> 8) as u8, (c >> 16) as u8, 1.0)
        } else {
            self.foreground
        }
    }

    /// Configure the compositor without activating the popup. Its live blur
    /// must still be checked on Windows; an API return is not visual evidence.
    pub fn apply(self, hwnd: HWND) -> bool {
        unsafe {
            let dark = i32::from(self.dark);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark as *const i32).cast(),
                size_of_val(&dark) as u32,
            );
            let corner = DWMWCP_ROUND;
            let rounded_window_supported = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corner as *const _ as *const c_void).cast(),
                size_of_val(&corner) as u32,
            )
            .is_ok();
            let border = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                (&border as *const u32).cast(),
                size_of_val(&border) as u32,
            );
            // Never stack the inactive managed backdrop below the accent
            // material: that opaque gray surface would hide the real desktop.
            let managed_backdrop = DWMSBT_NONE;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&managed_backdrop as *const _ as *const c_void).cast(),
                size_of_val(&managed_backdrop) as u32,
            );
            // The swapchain already supplies per-pixel alpha. Extending the
            // nonclient frame over the whole client can cover accent blur with
            // a flat frame fill; retain only DWM's outer rounding and shadow.
            let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS::default());
            let use_acrylic = self.transparency
                && rounded_window_supported
                && DwmIsCompositionEnabled().is_ok_and(|enabled| enabled.as_bool());
            let tint = use_acrylic.then_some(if self.dark {
                [32, 34, 39, 0xCC] // Keep the accepted dark tint at 80% opacity.
            } else {
                [247, 247, 247, 0xB3] // About 70% opacity reveals more backdrop.
            });
            let accepted = backdrop::apply(hwnd, tint);
            if use_acrylic && !accepted {
                // Remove any previous accent after a theme/compositor change
                // before the renderer switches to its fully opaque fallback.
                let _ = backdrop::apply(hwnd, None);
            }
            use_acrylic && accepted
        }
    }
}
