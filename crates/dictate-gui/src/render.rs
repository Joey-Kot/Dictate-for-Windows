use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F,
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL,
    D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LINE_JOIN_ROUND,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget,
    ID2D1Factory, ID2D1GeometrySink, ID2D1PathGeometry, ID2D1RenderTarget, ID2D1SolidColorBrush,
    ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, ULW_ALPHA, UpdateLayeredWindow};
use windows::core::w;
use windows_numerics::{Matrix3x2, Vector2};

use dictate_core::runtime::{Event, State};

use crate::i18n::Language;

pub const BUTTON_SIZE: i32 = 32;
pub const BUTTON_GAP: i32 = 6;
pub const PANEL_CORNER_RADIUS: i32 = 10;
const RETRY_BUTTON_SCALE: f32 = 0.75;
pub const FULL_WIDTH: i32 = 222;
pub const FULL_HEIGHT: i32 = 94;
pub const MINIMAL_WIDTH: i32 = 170;
pub const MINIMAL_HEIGHT: i32 = 46;

pub struct Renderer {
    factory: ID2D1Factory,
    target: Option<ID2D1DCRenderTarget>,
    surface: Option<LayeredSurface>,
    text_format: IDWriteTextFormat,
    stroke_style: ID2D1StrokeStyle,
    gear_geometry: ID2D1PathGeometry,
    retry_geometry: ID2D1PathGeometry,
    full_panel_geometry: ID2D1PathGeometry,
    full_panel_border_geometry: ID2D1PathGeometry,
    minimal_panel_geometry: ID2D1PathGeometry,
    minimal_panel_border_geometry: ID2D1PathGeometry,
    dpi: u32,
    window_scale: f32,
}

impl Renderer {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let write_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text_format = write_factory.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
                w!("en-us"),
            )?;
            text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let stroke_style = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            let gear_geometry = create_gear_geometry(&factory)?;
            let retry_geometry = create_retry_geometry(&factory)?;
            let full_panel_geometry = create_continuous_rounded_rect(
                &factory,
                0.0,
                0.0,
                FULL_WIDTH as f32,
                FULL_HEIGHT as f32,
                PANEL_CORNER_RADIUS as f32,
            )?;
            let full_panel_border_geometry = create_continuous_rounded_rect(
                &factory,
                0.5,
                0.5,
                FULL_WIDTH as f32 - 0.5,
                FULL_HEIGHT as f32 - 0.5,
                PANEL_CORNER_RADIUS as f32 - 0.5,
            )?;
            let minimal_panel_geometry = create_continuous_rounded_rect(
                &factory,
                0.0,
                0.0,
                MINIMAL_WIDTH as f32,
                MINIMAL_HEIGHT as f32,
                PANEL_CORNER_RADIUS as f32,
            )?;
            let minimal_panel_border_geometry = create_continuous_rounded_rect(
                &factory,
                0.5,
                0.5,
                MINIMAL_WIDTH as f32 - 0.5,
                MINIMAL_HEIGHT as f32 - 0.5,
                PANEL_CORNER_RADIUS as f32 - 0.5,
            )?;
            Ok(Self {
                factory,
                target: None,
                surface: None,
                text_format,
                stroke_style,
                gear_geometry,
                retry_geometry,
                full_panel_geometry,
                full_panel_border_geometry,
                minimal_panel_geometry,
                minimal_panel_border_geometry,
                dpi: 96,
                window_scale: 1.0,
            })
        }
    }

    pub fn discard_device_resources(&mut self) {
        self.target = None;
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi.max(96);
        if let Some(target) = &self.target {
            self.apply_dpi(target);
        }
    }

    pub fn set_window_scale(&mut self, window_scale: f32) {
        self.window_scale = window_scale;
        if let Some(target) = &self.target {
            self.apply_dpi(target);
        }
    }

    fn apply_dpi(&self, target: &ID2D1RenderTarget) {
        let effective_dpi = self.dpi as f32 * self.window_scale;
        unsafe { target.SetDpi(effective_dpi, effective_dpi) };
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self
            .surface
            .as_ref()
            .is_some_and(|surface| surface.width != width || surface.height != height)
        {
            self.surface = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        hwnd: HWND,
        event: &Event,
        minimal: bool,
        rounded: bool,
        language: Language,
        _pressed_button: Option<i32>,
        hover_lifts: &[f32; 5],
        animation_time: f32,
        opacity: u8,
    ) -> windows::core::Result<()> {
        self.ensure_target()?;
        self.ensure_surface(hwnd)?;
        let target = self.target.as_ref().unwrap();
        let surface = self.surface.as_ref().unwrap();
        unsafe {
            target.BindDC(
                surface.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: surface.width as i32,
                    bottom: surface.height as i32,
                },
            )?;
            target.BeginDraw();
            target.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));

            let panel_color = rgb8(18, 22, 25);
            let button_color = rgb8(39, 45, 48);
            let hover_color = rgb8(49, 57, 61);
            let disabled_color = rgb8(27, 32, 35);
            let active_color = rgb8(197, 50, 50);
            let panel = target.CreateSolidColorBrush(&panel_color, None)?;
            let panel_border = target.CreateSolidColorBrush(&rgb8(42, 45, 48), None)?;
            let border = target.CreateSolidColorBrush(&rgb8(65, 70, 73), None)?;
            let disabled_border = target.CreateSolidColorBrush(&rgb8(37, 42, 45), None)?;
            let active_border = target.CreateSolidColorBrush(&rgb8(239, 108, 108), None)?;
            let normal = target.CreateSolidColorBrush(&rgb8(217, 227, 229), None)?;
            let muted = target.CreateSolidColorBrush(&rgb8(102, 108, 110), None)?;
            let warning = target.CreateSolidColorBrush(&rgb8(240, 208, 115), None)?;
            let danger = target.CreateSolidColorBrush(&rgb8(255, 156, 141), None)?;
            let accent = target.CreateSolidColorBrush(&rgb8(168, 215, 208), None)?;
            let white = target.CreateSolidColorBrush(&rgb8(255, 255, 255), None)?;
            let handle = target.CreateSolidColorBrush(&rgb8(65, 69, 71), None)?;
            let status_brush = target.CreateSolidColorBrush(&rgb8(174, 189, 192), None)?;
            let wave_factor = if event.state == State::Paused {
                0.72
            } else {
                1.0
            };
            let wave_primary =
                target.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.96 * wave_factor), None)?;
            let wave_secondary =
                target.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 0.60 * wave_factor), None)?;

            let size = if minimal {
                (MINIMAL_WIDTH, MINIMAL_HEIGHT)
            } else {
                (FULL_WIDTH, FULL_HEIGHT)
            };
            if rounded {
                let (panel_geometry, panel_border_geometry) = if minimal {
                    (
                        &self.minimal_panel_geometry,
                        &self.minimal_panel_border_geometry,
                    )
                } else {
                    (&self.full_panel_geometry, &self.full_panel_border_geometry)
                };
                target.FillGeometry(panel_geometry, &panel, None::<&ID2D1Brush>);
                target.DrawGeometry(panel_border_geometry, &panel_border, 1.0, None);
            } else {
                fill_rounded_f(
                    target,
                    &panel,
                    &panel_border,
                    D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: size.0 as f32,
                        bottom: size.1 as f32,
                    },
                    0.0,
                );
            }

            if !minimal {
                fill_rounded_f(
                    target,
                    &handle,
                    &handle,
                    D2D_RECT_F {
                        left: 90.0,
                        top: 9.0,
                        right: 132.0,
                        bottom: 13.0,
                    },
                    if rounded { 3.0 } else { 0.0 },
                );
            }

            let count = if minimal { 4 } else { 5 };
            let retry_button = button_shows_retry(event);
            for index in 0..count {
                let active_record =
                    index == 0 && matches!(event.state, State::Recording | State::Paused);
                let is_disabled = button_is_disabled(index, event);
                let hover_progress = (-hover_lifts[index as usize]).clamp(0.0, 1.0);
                let base_color = if active_record {
                    active_color
                } else if is_disabled {
                    disabled_color
                } else {
                    button_color
                };
                let fill_color = if is_disabled {
                    base_color
                } else {
                    mix(base_color, hover_color, hover_progress)
                };
                let fill = target.CreateSolidColorBrush(&fill_color, None)?;
                let outline = if active_record && !is_disabled {
                    &active_border
                } else if is_disabled {
                    &disabled_border
                } else {
                    &border
                };
                let rect = button_rect_f(index, minimal, hover_lifts[index as usize]);
                fill_rounded_f(
                    target,
                    &fill,
                    outline,
                    rect,
                    if rounded { 8.0 } else { 0.0 },
                );
                let brush = if is_disabled {
                    &muted
                } else if active_record {
                    &white
                } else if index == 1 {
                    &warning
                } else if index == 2 && retry_button {
                    &accent
                } else if index == 2 {
                    &danger
                } else if index >= 3 {
                    &accent
                } else {
                    &normal
                };
                draw_icon(
                    target,
                    &self.stroke_style,
                    &self.gear_geometry,
                    &self.retry_geometry,
                    index,
                    rect,
                    brush,
                    &wave_primary,
                    &wave_secondary,
                    event.state,
                    retry_button,
                    minimal,
                    animation_time,
                );
            }

            if !minimal {
                let status = if !event.error.is_empty() {
                    event.error.as_str()
                } else if event.message.is_empty() {
                    language.text("idle")
                } else {
                    language.text(&event.message)
                };
                let utf16: Vec<u16> = status.encode_utf16().collect();
                target.DrawText(
                    &utf16,
                    &self.text_format,
                    &D2D_RECT_F {
                        left: 10.0,
                        top: 57.0,
                        right: 212.0,
                        bottom: 75.0,
                    },
                    &status_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            if let Err(error) = target.EndDraw(None, None) {
                self.discard_device_resources();
                return Err(error);
            }
            surface.present(hwnd, opacity)?;
        }
        Ok(())
    }

    fn ensure_target(&mut self) -> windows::core::Result<()> {
        if self.target.is_some() {
            return Ok(());
        }
        unsafe {
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: self.dpi as f32,
                dpiY: self.dpi as f32,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            self.target = Some(self.factory.CreateDCRenderTarget(&properties)?);
            self.apply_dpi(self.target.as_ref().unwrap());
        }
        Ok(())
    }

    fn ensure_surface(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client)? };
        let width = (client.right - client.left).max(1) as u32;
        let height = (client.bottom - client.top).max(1) as u32;
        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != width || surface.height != height)
        {
            self.surface = Some(LayeredSurface::new(width, height)?);
        }
        Ok(())
    }
}

struct LayeredSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    width: u32,
    height: u32,
}

impl LayeredSurface {
    fn new(width: u32, height: u32) -> windows::core::Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(windows::core::Error::from_win32());
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut::<c_void>();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(bitmap) => bitmap,
                Err(error) => {
                    let _ = DeleteDC(dc);
                    return Err(error);
                }
            };
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            if previous.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
                return Err(windows::core::Error::from_win32());
            }
            Ok(Self {
                dc,
                bitmap,
                previous,
                width,
                height,
            })
        }
    }

    unsafe fn present(&self, hwnd: HWND, opacity: u8) -> windows::core::Result<()> {
        unsafe { self.present_size(hwnd, opacity, self.width, self.height) }
    }

    unsafe fn present_size(
        &self,
        hwnd: HWND,
        opacity: u8,
        width: u32,
        height: u32,
    ) -> windows::core::Result<()> {
        let source = POINT { x: 0, y: 0 };
        let size = SIZE {
            cx: width as i32,
            cy: height as i32,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                None,
                Some(&size),
                Some(self.dc),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

thread_local! {
    static ROUNDED_SHAPES: RefCell<Option<RoundedShapeRenderer>> = const { RefCell::new(None) };
}

/// Paint only the shape through Direct2D. GDI text and native child controls keep
/// their own pixel-aligned rendering, viewport origin and clipping region.
pub fn draw_rounded_box(
    hdc: HDC,
    rect: RECT,
    fill: COLORREF,
    border: COLORREF,
    radius: i32,
) -> windows::core::Result<()> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    with_rounded_shapes(|renderer| {
        let surface = renderer.paint(width as u32, height as u32, fill, border, radius)?;
        unsafe {
            AlphaBlend(
                hdc,
                rect.left,
                rect.top,
                width,
                height,
                surface.dc,
                0,
                0,
                width,
                height,
                BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: u8::MAX,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                },
            )
            .ok()
        }
    })
}

/// Present a layered panel with genuine transparent, antialiased outer corners.
pub fn paint_rounded_panel(hwnd: HWND, fill: COLORREF, radius: i32) -> windows::core::Result<()> {
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client)? };
    let width = client.right - client.left;
    let height = client.bottom - client.top;
    if width <= 0 || height <= 0 {
        return Ok(());
    }
    with_rounded_shapes(|renderer| {
        let surface = renderer.paint(width as u32, height as u32, fill, fill, radius)?;
        // The scratch bitmap can be larger after painting another control. Only
        // the panel's actual dimensions belong in UpdateLayeredWindow's size.
        unsafe { surface.present_size(hwnd, u8::MAX, width as u32, height as u32) }
    })
}

fn with_rounded_shapes(
    paint: impl FnOnce(&mut RoundedShapeRenderer) -> windows::core::Result<()>,
) -> windows::core::Result<()> {
    ROUNDED_SHAPES.with(|cache| {
        let Ok(mut cache) = cache.try_borrow_mut() else {
            // Presenting a layered window can synchronously dispatch messages.
            // A nested paint uses its own resources instead of borrowing again.
            return paint(&mut RoundedShapeRenderer::new()?);
        };
        if cache.is_none() {
            *cache = Some(RoundedShapeRenderer::new()?);
        }
        let result = paint(cache.as_mut().unwrap());
        if result.is_err() {
            // A failed EndDraw must never copy stale pixels. Recreate all
            // dependent resources on the next paint, including after device loss.
            *cache = None;
        }
        result
    })
}

struct RoundedShapeRenderer {
    target: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    surface: Option<LayeredSurface>,
}

impl RoundedShapeRenderer {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let target = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                // Callers already pass physical pixels scaled for their window.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            })?;
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let brush = target.CreateSolidColorBrush(&rgb8(0, 0, 0), None)?;
            Ok(Self {
                target,
                brush,
                surface: None,
            })
        }
    }

    fn paint(
        &mut self,
        width: u32,
        height: u32,
        fill: COLORREF,
        border: COLORREF,
        radius: i32,
    ) -> windows::core::Result<&LayeredSurface> {
        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width < width || surface.height < height)
        {
            let (capacity_width, capacity_height) = self
                .surface
                .as_ref()
                .map(|surface| (surface.width.max(width), surface.height.max(height)))
                .unwrap_or((width, height));
            self.surface = Some(LayeredSurface::new(capacity_width, capacity_height)?);
        }
        let surface = self.surface.as_ref().unwrap();
        let radius = (radius.max(0) as f32).min(width.min(height) as f32 / 2.0);
        unsafe {
            self.target.BindDC(
                surface.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
            )?;
            self.target.BeginDraw();
            self.target.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
            self.brush.SetColor(&colorref(border));
            self.target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.0,
                        top: 0.0,
                        right: width as f32,
                        bottom: height as f32,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &self.brush,
            );
            // Keep the existing one-physical-pixel border. A single outer fill
            // prevents repeatedly blending the same antialiased silhouette.
            if fill != border && width > 2 && height > 2 {
                self.brush.SetColor(&colorref(fill));
                self.target.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: 1.0,
                            top: 1.0,
                            right: width as f32 - 1.0,
                            bottom: height as f32 - 1.0,
                        },
                        radiusX: (radius - 1.0).max(0.0),
                        radiusY: (radius - 1.0).max(0.0),
                    },
                    &self.brush,
                );
            }
            self.target.EndDraw(None, None)?;
        }
        Ok(surface)
    }
}

fn colorref(value: COLORREF) -> D2D1_COLOR_F {
    rgb8(value.0 as u8, (value.0 >> 8) as u8, (value.0 >> 16) as u8)
}

pub struct RoundedOutlineRenderer {
    target: ID2D1DCRenderTarget,
    surface: Option<LayeredSurface>,
    geometry: ID2D1PathGeometry,
    dpi: u32,
}

impl RoundedOutlineRenderer {
    pub fn new(width: f32, height: f32, radius: f32, dpi: u32) -> windows::core::Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&properties)?;
            let geometry = create_continuous_rounded_rect(
                &factory,
                0.5,
                0.5,
                width - 0.5,
                height - 0.5,
                radius - 0.5,
            )?;
            Ok(Self {
                target,
                surface: None,
                geometry,
                dpi,
            })
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi.max(96);
        unsafe { self.target.SetDpi(self.dpi as f32, self.dpi as f32) };
        self.surface = None;
    }

    pub fn paint(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client)? };
        let width = (client.right - client.left).max(1) as u32;
        let height = (client.bottom - client.top).max(1) as u32;
        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != width || surface.height != height)
        {
            self.surface = Some(LayeredSurface::new(width, height)?);
        }
        let surface = self.surface.as_ref().unwrap();
        unsafe {
            self.target.BindDC(
                surface.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
            )?;
            self.target.BeginDraw();
            self.target.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
            let border = self.target.CreateSolidColorBrush(&rgb8(42, 45, 48), None)?;
            self.target.DrawGeometry(&self.geometry, &border, 1.5, None);
            self.target.EndDraw(None, None)?;
            surface.present(hwnd, u8::MAX)
        }
    }
}

pub fn continuous_rounded_rect_polygon(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
    segments_per_corner: usize,
) -> Vec<(f32, f32)> {
    let (shoulder, near_corner) = continuous_corner_parameters(left, top, right, bottom, radius);
    let mut points = Vec::with_capacity(segments_per_corner * 4 + 4);
    points.push((left + shoulder, top));
    points.push((right - shoulder, top));
    append_cubic_points(
        &mut points,
        (right - shoulder, top),
        (right - near_corner, top),
        (right, top + near_corner),
        (right, top + shoulder),
        segments_per_corner,
    );
    points.push((right, bottom - shoulder));
    append_cubic_points(
        &mut points,
        (right, bottom - shoulder),
        (right, bottom - near_corner),
        (right - near_corner, bottom),
        (right - shoulder, bottom),
        segments_per_corner,
    );
    points.push((left + shoulder, bottom));
    append_cubic_points(
        &mut points,
        (left + shoulder, bottom),
        (left + near_corner, bottom),
        (left, bottom - near_corner),
        (left, bottom - shoulder),
        segments_per_corner,
    );
    points.push((left, top + shoulder));
    append_cubic_points(
        &mut points,
        (left, top + shoulder),
        (left, top + near_corner),
        (left + near_corner, top),
        (left + shoulder, top),
        segments_per_corner,
    );
    points
}

fn append_cubic_points(
    points: &mut Vec<(f32, f32)>,
    start: (f32, f32),
    control1: (f32, f32),
    control2: (f32, f32),
    end: (f32, f32),
    segments: usize,
) {
    for step in 1..=segments.max(1) {
        let t = step as f32 / segments.max(1) as f32;
        let inverse = 1.0 - t;
        points.push((
            inverse.powi(3) * start.0
                + 3.0 * inverse.powi(2) * t * control1.0
                + 3.0 * inverse * t.powi(2) * control2.0
                + t.powi(3) * end.0,
            inverse.powi(3) * start.1
                + 3.0 * inverse.powi(2) * t * control1.1
                + 3.0 * inverse * t.powi(2) * control2.1
                + t.powi(3) * end.1,
        ));
    }
}

pub fn button_rect(index: i32, minimal: bool) -> RECT {
    let count = if minimal { 4 } else { 5 };
    let total = count * BUTTON_SIZE + (count - 1) * BUTTON_GAP;
    let width = if minimal { MINIMAL_WIDTH } else { FULL_WIDTH };
    let left = (width - total) / 2 + index * (BUTTON_SIZE + BUTTON_GAP);
    let top = if minimal { 7 } else { 20 };
    RECT {
        left,
        top,
        right: left + BUTTON_SIZE,
        bottom: top + BUTTON_SIZE,
    }
}

pub fn hit_test_button(x: i32, y: i32, minimal: bool) -> Option<i32> {
    let count = if minimal { 4 } else { 5 };
    (0..count).find(|index| {
        let rect = button_rect(*index, minimal);
        x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
    })
}

pub fn button_shows_retry(event: &Event) -> bool {
    event.state == State::Idle && event.retry_available
}

pub fn button_is_disabled(index: i32, event: &Event) -> bool {
    let state = event.state;
    (index == 1 && !matches!(state, State::Recording | State::Paused))
        || (index == 2
            && !matches!(
                state,
                State::Recording | State::Paused | State::Uploading | State::Rewriting
            )
            && !button_shows_retry(event))
        || (index == 0 && matches!(state, State::Uploading | State::Rewriting))
}

fn button_rect_f(index: i32, minimal: bool, lift: f32) -> D2D_RECT_F {
    let rect = button_rect(index, minimal);
    D2D_RECT_F {
        left: rect.left as f32,
        top: rect.top as f32 + lift,
        right: rect.right as f32,
        bottom: rect.bottom as f32 + lift,
    }
}

fn create_continuous_rounded_rect(
    factory: &ID2D1Factory,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> windows::core::Result<ID2D1PathGeometry> {
    // Extend each corner's shoulder beyond a circular arc while preserving the
    // same diagonal inset. The longer tangent makes the straight-to-curve
    // transition gradual instead of changing curvature at a single point.
    let (shoulder, near_corner) = continuous_corner_parameters(left, top, right, bottom, radius);

    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.BeginFigure(v(left + shoulder, top), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(v(right - shoulder, top));
        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
            point1: v(right - near_corner, top),
            point2: v(right, top + near_corner),
            point3: v(right, top + shoulder),
        });
        sink.AddLine(v(right, bottom - shoulder));
        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
            point1: v(right, bottom - near_corner),
            point2: v(right - near_corner, bottom),
            point3: v(right - shoulder, bottom),
        });
        sink.AddLine(v(left + shoulder, bottom));
        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
            point1: v(left + near_corner, bottom),
            point2: v(left, bottom - near_corner),
            point3: v(left, bottom - shoulder),
        });
        sink.AddLine(v(left, top + shoulder));
        sink.AddBezier(&D2D1_BEZIER_SEGMENT {
            point1: v(left, top + near_corner),
            point2: v(left + near_corner, top),
            point3: v(left + shoulder, top),
        });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geometry)
    }
}

fn continuous_corner_parameters(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> (f32, f32) {
    let shoulder = (radius * 1.5)
        .min((right - left) / 2.0)
        .min((bottom - top) / 2.0);
    let diagonal_inset = radius * (1.0 - std::f32::consts::FRAC_1_SQRT_2);
    let near_corner = ((8.0 * diagonal_inset - shoulder) / 3.0).clamp(0.0, shoulder);
    (shoulder, near_corner)
}

unsafe fn fill_rounded_f(
    target: &ID2D1RenderTarget,
    fill: &ID2D1SolidColorBrush,
    border: &ID2D1SolidColorBrush,
    rect: D2D_RECT_F,
    radius: f32,
) {
    let rounded = D2D1_ROUNDED_RECT {
        rect,
        radiusX: radius,
        radiusY: radius,
    };
    unsafe {
        target.FillRoundedRectangle(&rounded, fill);
        target.DrawRoundedRectangle(&rounded, border, 1.0, None);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_icon(
    target: &ID2D1RenderTarget,
    stroke_style: &ID2D1StrokeStyle,
    gear_geometry: &ID2D1PathGeometry,
    retry_geometry: &ID2D1PathGeometry,
    index: i32,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
    wave_primary: &ID2D1SolidColorBrush,
    wave_secondary: &ID2D1SolidColorBrush,
    state: State,
    retry_button: bool,
    minimal: bool,
    animation_time: f32,
) {
    let cx = (rect.left + rect.right) / 2.0;
    let cy = (rect.top + rect.bottom) / 2.0;
    unsafe {
        match index {
            0 => draw_microphone(
                target,
                stroke_style,
                cx,
                cy,
                brush,
                wave_primary,
                wave_secondary,
                state,
                animation_time,
            ),
            1 if state == State::Paused => {
                draw_polyline(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    &[
                        v(cx - 3.0, cy - 5.25),
                        v(cx - 3.0, cy + 5.25),
                        v(cx + 5.25, cy),
                        v(cx - 3.0, cy - 5.25),
                    ],
                );
            }
            1 => {
                draw_line(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    cx - 3.0,
                    cy - 5.25,
                    cx - 3.0,
                    cy + 5.25,
                );
                draw_line(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    cx + 3.0,
                    cy - 5.25,
                    cx + 3.0,
                    cy + 5.25,
                );
            }
            2 if retry_button => draw_retry_icon(target, retry_geometry, cx, cy, brush),
            2 => {
                draw_line(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    cx - 4.5,
                    cy - 4.5,
                    cx + 4.5,
                    cy + 4.5,
                );
                draw_line(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    cx + 4.5,
                    cy - 4.5,
                    cx - 4.5,
                    cy + 4.5,
                );
            }
            3 if minimal => {
                draw_line(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    cx - 5.25,
                    cy,
                    cx + 5.25,
                    cy,
                );
                draw_line(
                    target,
                    stroke_style,
                    brush,
                    1.5,
                    cx,
                    cy - 5.25,
                    cx,
                    cy + 5.25,
                );
            }
            3 => draw_gear(target, stroke_style, gear_geometry, cx, cy, brush),
            _ => {
                draw_line(target, stroke_style, brush, 1.5, cx - 4.5, cy, cx + 4.5, cy);
            }
        }
    }
}

unsafe fn draw_retry_icon(
    target: &ID2D1RenderTarget,
    geometry: &ID2D1PathGeometry,
    cx: f32,
    cy: f32,
    brush: &ID2D1SolidColorBrush,
) {
    const VIEWBOX_SIZE: f32 = 1024.0;
    const ICON_SIZE: f32 = 18.0 * RETRY_BUTTON_SCALE;

    let scale = ICON_SIZE / VIEWBOX_SIZE;
    unsafe {
        let mut original = Matrix3x2::default();
        target.GetTransform(&mut original);
        target.SetTransform(&Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: cx - ICON_SIZE / 2.0,
            M32: cy - ICON_SIZE / 2.0,
        });
        target.FillGeometry(geometry, brush, None::<&ID2D1Brush>);
        target.SetTransform(&original);
    }
}

fn create_retry_geometry(factory: &ID2D1Factory) -> windows::core::Result<ID2D1PathGeometry> {
    // This uses the supplied SVG's 1024 × 1024 viewBox coordinates so its
    // two solid, opposing arrows retain their original proportions.
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;

        sink.BeginFigure(v(177.724_63, 124.579_94), D2D1_FIGURE_BEGIN_FILLED);
        let mut point = v(177.724_63, 124.579_94);
        path_arc_absolute(
            &sink, &mut point, 511.832_12, 511.832_12, false, true, 931.448_6, 805.623_8,
        );
        path_line_absolute(&sink, &mut point, 768.071_8, 511.729_77);
        path_line_absolute(&sink, &mut point, 921.723_8, 511.729_77);
        path_arc_absolute(
            &sink, &mut point, 409.465_7, 409.465_7, false, false, 228.703_11, 216.300_25,
        );
        path_line_absolute(&sink, &mut point, 177.724_63, 124.477_57);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);

        sink.BeginFigure(v(846.791_56, 899.186_65), D2D1_FIGURE_BEGIN_FILLED);
        point = v(846.791_56, 899.186_65);
        path_arc_absolute(
            &sink, &mut point, 511.832_12, 511.832_12, false, true, 92.965_23, 218.040_48,
        );
        path_line_absolute(&sink, &mut point, 256.444_4, 511.832_12);
        path_line_absolute(&sink, &mut point, 102.792_41, 511.832_12);
        path_arc_absolute(
            &sink, &mut point, 409.465_7, 409.465_7, false, false, 795.813_1, 807.261_6,
        );
        path_line_absolute(&sink, &mut point, 846.791_56, 899.186_65);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);

        sink.Close()?;
        Ok(geometry)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_microphone(
    target: &ID2D1RenderTarget,
    stroke_style: &ID2D1StrokeStyle,
    cx: f32,
    cy: f32,
    brush: &ID2D1SolidColorBrush,
    wave_primary: &ID2D1SolidColorBrush,
    wave_secondary: &ID2D1SolidColorBrush,
    state: State,
    animation_time: f32,
) {
    let body = D2D_RECT_F {
        left: cx - 2.25,
        top: cy - 6.75,
        right: cx + 2.25,
        bottom: cy + 1.5,
    };
    unsafe {
        if matches!(state, State::Recording | State::Paused) {
            target.PushAxisAlignedClip(&body, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let first_drift = if state == State::Recording {
                triangle_wave(animation_time, 1.15) * 1.6
            } else {
                0.0
            };
            let second_drift = if state == State::Recording {
                triangle_wave(animation_time, 1.55) * 1.6
            } else {
                0.0
            };
            draw_mic_wave(
                target,
                stroke_style,
                wave_primary,
                cx,
                cy - 2.7,
                first_drift,
            );
            draw_mic_wave(
                target,
                stroke_style,
                wave_secondary,
                cx,
                cy - 0.75,
                second_drift,
            );
            target.PopAxisAlignedClip();
        }

        target.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: body,
                radiusX: 2.25,
                radiusY: 2.25,
            },
            brush,
            1.5,
            stroke_style,
        );

        let mut previous = v(cx - 5.25, cy - 0.75);
        for step in 1..=12 {
            let angle = std::f32::consts::PI - std::f32::consts::PI * step as f32 / 12.0;
            let point = v(cx + angle.cos() * 5.25, cy - 0.75 + angle.sin() * 5.25);
            target.DrawLine(previous, point, brush, 1.5, stroke_style);
            previous = point;
        }
        draw_line(
            target,
            stroke_style,
            brush,
            1.5,
            cx,
            cy + 4.5,
            cx,
            cy + 6.75,
        );
    }
}

unsafe fn draw_mic_wave(
    target: &ID2D1RenderTarget,
    stroke_style: &ID2D1StrokeStyle,
    brush: &ID2D1SolidColorBrush,
    cx: f32,
    base_y: f32,
    drift: f32,
) {
    let mut previous = None;
    for step in 0..=32 {
        let x = -6.0 + step as f32 * 12.0 / 32.0 + drift;
        let phase = (x - drift + 5.25) * std::f32::consts::TAU / 4.5;
        let point = v(cx + x, base_y - phase.sin() * 0.6);
        if let Some(start) = previous {
            unsafe { target.DrawLine(start, point, brush, 1.125, stroke_style) };
        }
        previous = Some(point);
    }
}

unsafe fn draw_gear(
    target: &ID2D1RenderTarget,
    stroke_style: &ID2D1StrokeStyle,
    geometry: &ID2D1PathGeometry,
    cx: f32,
    cy: f32,
    brush: &ID2D1SolidColorBrush,
) {
    unsafe {
        let mut original = Matrix3x2::default();
        target.GetTransform(&mut original);
        target.SetTransform(&Matrix3x2 {
            M11: 0.75,
            M12: 0.0,
            M21: 0.0,
            M22: 0.75,
            M31: cx - 9.0,
            M32: cy - 9.0,
        });
        target.DrawGeometry(geometry, brush, 2.0, stroke_style);
        target.SetTransform(&original);
    }
}

fn create_gear_geometry(factory: &ID2D1Factory) -> windows::core::Result<ID2D1PathGeometry> {
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;

        sink.BeginFigure(v(12.0, 15.5), D2D1_FIGURE_BEGIN_HOLLOW);
        let mut point = v(12.0, 15.5);
        path_arc_relative(&sink, &mut point, 3.5, 3.5, true, false, 0.0, -7.0);
        path_arc_relative(&sink, &mut point, 3.5, 3.5, false, false, 0.0, 7.0);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);

        sink.BeginFigure(v(19.4, 15.0), D2D1_FIGURE_BEGIN_HOLLOW);
        point = v(19.4, 15.0);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 0.3, 1.9);
        path_line_relative(&sink, &mut point, 0.1, 0.1);
        path_arc_relative(&sink, &mut point, 2.0, 2.0, true, true, -2.8, 2.8);
        path_line_relative(&sink, &mut point, -0.1, -0.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -1.9, -0.3);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -1.0, 1.6);
        path_line_relative(&sink, &mut point, 0.0, 0.3);
        path_arc_relative(&sink, &mut point, 2.0, 2.0, true, true, -4.0, 0.0);
        path_line_absolute(&sink, &mut point, 10.0, 21.0);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -1.1, -1.6);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -1.8, 0.3);
        path_line_relative(&sink, &mut point, -0.1, 0.1);
        path_arc_absolute(&sink, &mut point, 2.0, 2.0, true, true, 4.2, 17.0);
        path_line_relative(&sink, &mut point, 0.1, -0.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 0.3, -1.9);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -1.6, -1.0);
        path_line_absolute(&sink, &mut point, 2.7, 14.0);
        path_arc_relative(&sink, &mut point, 2.0, 2.0, true, true, 0.0, -4.0);
        path_line_absolute(&sink, &mut point, 3.0, 10.0);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 1.6, -1.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -0.3, -1.8);
        path_line_absolute(&sink, &mut point, 4.2, 7.0);
        path_arc_absolute(&sink, &mut point, 2.0, 2.0, true, true, 7.0, 4.2);
        path_line_relative(&sink, &mut point, 0.1, 0.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 1.9, 0.3);
        path_line_absolute(&sink, &mut point, 9.0, 4.6);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 1.0, -1.6);
        path_line_relative(&sink, &mut point, 0.0, -0.3);
        path_arc_relative(&sink, &mut point, 2.0, 2.0, true, true, 4.0, 0.0);
        path_line_absolute(&sink, &mut point, 14.0, 3.0);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 1.1, 1.6);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 1.8, -0.3);
        path_line_relative(&sink, &mut point, 0.1, -0.1);
        path_arc_absolute(&sink, &mut point, 2.0, 2.0, true, true, 19.8, 7.0);
        path_line_relative(&sink, &mut point, -0.1, 0.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -0.3, 1.9);
        path_line_relative(&sink, &mut point, 0.0, 0.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, 1.6, 1.0);
        path_line_relative(&sink, &mut point, 0.3, 0.0);
        path_arc_relative(&sink, &mut point, 2.0, 2.0, true, true, 0.0, 4.0);
        path_line_absolute(&sink, &mut point, 21.0, 14.1);
        path_arc_relative(&sink, &mut point, 1.7, 1.7, false, false, -1.6, 0.9);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geometry)
    }
}

unsafe fn path_line_relative(
    sink: &ID2D1GeometrySink,
    point: &mut Vector2,
    delta_x: f32,
    delta_y: f32,
) {
    point.X += delta_x;
    point.Y += delta_y;
    unsafe { sink.AddLine(*point) };
}

unsafe fn path_line_absolute(sink: &ID2D1GeometrySink, point: &mut Vector2, x: f32, y: f32) {
    *point = v(x, y);
    unsafe { sink.AddLine(*point) };
}

#[allow(clippy::too_many_arguments)]
unsafe fn path_arc_relative(
    sink: &ID2D1GeometrySink,
    point: &mut Vector2,
    radius_x: f32,
    radius_y: f32,
    large: bool,
    clockwise: bool,
    delta_x: f32,
    delta_y: f32,
) {
    let x = point.X + delta_x;
    let y = point.Y + delta_y;
    unsafe {
        path_arc_absolute(sink, point, radius_x, radius_y, large, clockwise, x, y);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn path_arc_absolute(
    sink: &ID2D1GeometrySink,
    point: &mut Vector2,
    radius_x: f32,
    radius_y: f32,
    large: bool,
    clockwise: bool,
    x: f32,
    y: f32,
) {
    *point = v(x, y);
    unsafe {
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: *point,
            size: D2D_SIZE_F {
                width: radius_x,
                height: radius_y,
            },
            rotationAngle: 0.0,
            sweepDirection: if clockwise {
                D2D1_SWEEP_DIRECTION_CLOCKWISE
            } else {
                D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
            },
            arcSize: if large {
                D2D1_ARC_SIZE_LARGE
            } else {
                D2D1_ARC_SIZE_SMALL
            },
        });
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_line(
    target: &ID2D1RenderTarget,
    stroke_style: &ID2D1StrokeStyle,
    brush: &ID2D1SolidColorBrush,
    width: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
) {
    unsafe { target.DrawLine(v(x1, y1), v(x2, y2), brush, width, stroke_style) };
}

unsafe fn draw_polyline(
    target: &ID2D1RenderTarget,
    stroke_style: &ID2D1StrokeStyle,
    brush: &ID2D1SolidColorBrush,
    width: f32,
    points: &[Vector2],
) {
    for pair in points.windows(2) {
        unsafe { target.DrawLine(pair[0], pair[1], brush, width, stroke_style) };
    }
}

fn triangle_wave(time: f32, duration: f32) -> f32 {
    let progress = (time / duration).rem_euclid(1.0);
    if progress < 0.5 {
        -1.0 + progress * 4.0
    } else {
        3.0 - progress * 4.0
    }
}

fn v(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rgb8(r: u8, g: u8, b: u8) -> D2D1_COLOR_F {
    color(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
}

fn mix(from: D2D1_COLOR_F, to: D2D1_COLOR_F, amount: f32) -> D2D1_COLOR_F {
    let amount = amount.clamp(0.0, 1.0);
    color(
        from.r + (to.r - from.r) * amount,
        from.g + (to.g - from.g) * amount,
        from.b + (to.b - from.b) * amount,
        from.a + (to.a - from.a) * amount,
    )
}

#[cfg(test)]
mod rounded_shape_tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{
        DC_BRUSH, DIBSECTION, FillRect, GdiFlush, GetObjectW, GetStockObject, GetViewportOrgEx,
        HBRUSH, IntersectClipRect, SetDCBrushColor, SetViewportOrgEx,
    };

    fn pixels(surface: &LayeredSurface) -> Vec<[u8; 4]> {
        unsafe {
            assert!(GdiFlush().as_bool());
            let mut section = DIBSECTION::default();
            assert_eq!(
                GetObjectW(
                    HGDIOBJ(surface.bitmap.0),
                    size_of::<DIBSECTION>() as i32,
                    Some((&mut section as *mut DIBSECTION).cast()),
                ),
                size_of::<DIBSECTION>() as i32,
            );
            let bytes = std::slice::from_raw_parts(
                section.dsBm.bmBits.cast::<u8>(),
                (surface.width * surface.height * 4) as usize,
            );
            bytes.as_chunks::<4>().0.to_vec()
        }
    }

    #[test]
    fn rounded_shape_clears_reused_surface_and_keeps_premultiplied_edges() {
        let mut renderer = RoundedShapeRenderer::new().unwrap();
        renderer
            .paint(48, 32, COLORREF(0x0000ff), COLORREF(0x0000ff), 0)
            .unwrap();
        let surface = renderer
            .paint(32, 24, COLORREF(0xa05018), COLORREF(0xa05018), 7)
            .unwrap();
        let pixels = pixels(surface);
        let at = |x: usize, y: usize| pixels[y * surface.width as usize + x];
        assert_eq!(at(0, 0), [0, 0, 0, 0]);
        assert_eq!(at(16, 12), [160, 80, 24, 255]);
        let edge = (0..7)
            .flat_map(|y| (0..7).map(move |x| at(x, y)))
            .filter(|pixel| pixel[3] > 0 && pixel[3] < 255)
            .collect::<Vec<_>>();
        assert!(
            !edge.is_empty(),
            "rounded edge must contain partial coverage"
        );
        for [b, g, r, a] in edge {
            assert!(b <= a && g <= a && r <= a);
        }
    }

    #[test]
    fn rounded_shape_blend_preserves_viewport_clip_and_background() {
        let destination = LayeredSurface::new(24, 20).unwrap();
        unsafe {
            SetDCBrushColor(destination.dc, COLORREF(0x1e140a));
            FillRect(
                destination.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: 24,
                    bottom: 20,
                },
                HBRUSH(GetStockObject(DC_BRUSH).0),
            );
            assert!(SetViewportOrgEx(destination.dc, -10, -20, None).as_bool());
            IntersectClipRect(destination.dc, 14, 20, 28, 40);
        }
        draw_rounded_box(
            destination.dc,
            RECT {
                left: 12,
                top: 22,
                right: 30,
                bottom: 38,
            },
            COLORREF(0xa05018),
            COLORREF(0xa05018),
            4,
        )
        .unwrap();
        let pixels = pixels(&destination);
        let at = |x: usize, y: usize| {
            let pixel = pixels[y * destination.width as usize + x];
            [pixel[0], pixel[1], pixel[2]]
        };
        assert_eq!(at(3, 10), [30, 20, 10], "left of clip must be preserved");
        assert_eq!(at(5, 10), [160, 80, 24]);
        assert_eq!(at(14, 10), [160, 80, 24]);
        assert_eq!(at(18, 10), [30, 20, 10], "right of clip must be preserved");
        assert_eq!(at(10, 19), [30, 20, 10], "outside shape must be preserved");
        let mut origin = POINT::default();
        unsafe { assert!(GetViewportOrgEx(destination.dc, &mut origin).as_bool()) };
        assert_eq!((origin.x, origin.y), (-10, -20));
    }
}
