//! Acrylic for a popup that must never activate. The managed transient-window
//! backdrop falls back to a solid color when inactive; use the compositor's
//! accent material instead, without altering the HWND's activation policy.
//!
//! Accent policy is an undocumented extension of the dynamically exported
//! SetWindowCompositionAttribute API. Keep it isolated, check the return value,
//! and let the caller draw a solid theme surface if unavailable. This path is
//! gated on Windows 11's DWM rounded-window support by Theme::apply.
use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{BOOL, s, w};

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    tint: u32,
    animation: u32,
}

#[repr(C)]
struct AttributeData {
    attribute: u32,
    data: *const c_void,
    size: usize,
}

type SetAttribute = unsafe extern "system" fn(HWND, *const AttributeData) -> BOOL;

/// A successful return means the compositor accepted the material, not that
/// Windows desktop visual validation has been performed.
pub(super) fn apply(hwnd: HWND, tint: Option<[u8; 4]>) -> bool {
    static SET_ATTRIBUTE: OnceLock<Option<SetAttribute>> = OnceLock::new();
    let setter = SET_ATTRIBUTE.get_or_init(|| unsafe {
        let module = GetModuleHandleW(w!("user32.dll")).ok()?;
        let proc = GetProcAddress(module, s!("SetWindowCompositionAttribute"))?;
        Some(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            SetAttribute,
        >(proc))
    });
    let Some(setter) = setter else {
        return false;
    };
    let policy = AccentPolicy {
        state: if tint.is_some() { 4 } else { 0 }, // Acrylic blur / disabled.
        // Acrylic uses 0, not the blur-behind tint/border flags. Extra border
        // flags can leak a rectangular material frame outside rounded popups.
        flags: 0,
        tint: tint.map_or(0, |[r, g, b, a]| {
            // Convert theme RGBA to native AABBGGRR; the compositor supplies
            // live background blur and Acrylic noise beneath this tint.
            (u32::from(a) << 24) | u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
        }),
        animation: 0,
    };
    let attribute = AttributeData {
        attribute: 19, // WCA_ACCENT_POLICY
        data: (&policy as *const AccentPolicy).cast(),
        size: size_of::<AccentPolicy>(),
    };
    unsafe { setter(hwnd, &attribute).as_bool() }
}
