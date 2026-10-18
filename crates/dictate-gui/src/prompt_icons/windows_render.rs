use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::path::Path;

use dictate_core::rewrite::PromptIcon;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, ID2D1Bitmap, ID2D1RenderTarget,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HDC, HGDIOBJ,
    SelectObject,
};

use super::{
    CacheKey, IconCache, IconDraft, IconPixels, MAX_CACHE_ENTRIES, MAX_ICON_SIDE, is_staged,
};

pub struct IconRenderer {
    pixels: IconCache,
    bitmaps: HashMap<CacheKey, ID2D1Bitmap>,
}

impl IconRenderer {
    pub fn new(config_path: &Path) -> Self {
        Self {
            pixels: IconCache::new(config_path),
            bitmaps: HashMap::new(),
        }
    }

    /// Call when the owning D2D render target is replaced (including device loss).
    pub fn discard_device_resources(&mut self) {
        self.bitmaps.clear();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        target: &ID2D1RenderTarget,
        icon: &PromptIcon,
        rect: D2D_RECT_F,
        dpi: u32,
        dark: bool,
        draft: Option<&IconDraft>,
    ) -> windows::core::Result<()> {
        let size = (((rect.right - rect.left).min(rect.bottom - rect.top) * dpi as f32 / 96.0)
            .round() as u32)
            .clamp(1, MAX_ICON_SIDE);
        let scale = dpi.max(1) as f32 / 96.0;
        let left = (((rect.left + rect.right) * 0.5 * scale - size as f32 * 0.5).round()) / scale;
        let top = (((rect.top + rect.bottom) * 0.5 * scale - size as f32 * 0.5).round()) / scale;
        // Keep bitmap texels aligned with physical monitor pixels at fractional
        // scaling (e.g. 30 DIP becomes 38 physical pixels at 125%).
        let rect = D2D_RECT_F {
            left,
            top,
            right: left + size as f32 / scale,
            bottom: top + size as f32 / scale,
        };
        let key = CacheKey {
            icon: icon.clone(),
            size,
            staged: is_staged(icon, draft),
        };
        if !self.bitmaps.contains_key(&key) {
            let pixels = self.pixels.pixels(icon, size, dark, draft);
            let bitmap = unsafe {
                target.CreateBitmap(
                    D2D_SIZE_U {
                        width: size,
                        height: size,
                    },
                    Some(pixels.bgra.as_ptr().cast()),
                    size * 4,
                    &D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: dpi as f32,
                        dpiY: dpi as f32,
                    },
                )?
            };
            if self.bitmaps.len() >= MAX_CACHE_ENTRIES {
                self.bitmaps.clear();
            }
            self.bitmaps.insert(key.clone(), bitmap);
        }
        if let Some(bitmap) = self.bitmaps.get(&key) {
            unsafe {
                target.DrawBitmap(
                    bitmap,
                    Some(&rect),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        }
        Ok(())
    }
}

/// Draw an already scaled preview onto a native owner-drawn control at physical
/// pixel coordinates. The caller supplies the control's painted background.
pub fn draw_gdi(hdc: HDC, x: i32, y: i32, pixels: &IconPixels) -> windows::core::Result<()> {
    unsafe {
        let memory = CreateCompatibleDC(Some(hdc));
        if memory.is_invalid() {
            return Err(windows::core::Error::from_win32());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pixels.size as i32,
                biHeight: -(pixels.size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        {
            Ok(bitmap) => bitmap,
            Err(error) => {
                let _ = DeleteDC(memory);
                return Err(error);
            }
        };
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(memory);
            return Err(windows::core::Error::from_win32());
        }
        std::ptr::copy_nonoverlapping(pixels.bgra.as_ptr(), bits.cast(), pixels.bgra.len());
        let previous = SelectObject(memory, HGDIOBJ(bitmap.0));
        let success = AlphaBlend(
            hdc,
            x,
            y,
            pixels.size as i32,
            pixels.size as i32,
            memory,
            0,
            0,
            pixels.size as i32,
            pixels.size as i32,
            BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            },
        );
        let _ = SelectObject(memory, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
        success.ok()
    }
}
