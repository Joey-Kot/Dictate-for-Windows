//! Premultiplied DirectComposition surface. No GDI background brush,
//! WS_EX_LAYERED, color key, or UpdateLayeredWindow participates in this path.
use dictate_core::rewrite::RewritePrompt;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, w};
use windows_numerics::Vector2;

use super::layout::{
    FONT_SIZE, ICON_LEFT, ICON_SIZE, Layout, PADDING, RADIUS, SCROLLBAR_INSET, SCROLLBAR_SCALE,
    SCROLLBAR_WIDTH, TITLE_LEFT,
};
use super::theme::{Theme, color};
use crate::prompt_icons::IconRenderer;

pub(super) struct Surface {
    // Keep the device and visual tree alive as long as their swapchain.
    _d3d: ID3D11Device,
    _composition: IDCompositionDevice,
    _root: IDCompositionTarget,
    _visual: IDCompositionVisual,
    swapchain: IDXGISwapChain1,
    target: ID2D1RenderTarget,
    write: IDWriteFactory,
    format: IDWriteTextFormat,
}

impl Surface {
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: u32) -> windows::core::Result<Self> {
        unsafe {
            let mut device = None;
            let create = |driver, output: &mut Option<ID3D11Device>| {
                D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(output),
                    None,
                    None,
                )
            };
            if create(D3D_DRIVER_TYPE_HARDWARE, &mut device).is_err() {
                create(D3D_DRIVER_TYPE_WARP, &mut device)?;
            }
            let d3d = device.ok_or_else(windows::core::Error::from_win32)?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let adapter = dxgi.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let swapchain = factory.CreateSwapChainForComposition(
                &d3d,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: width.max(1),
                    Height: height.max(1),
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                    AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                    ..Default::default()
                },
                None,
            )?;
            let composition: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let root = composition.CreateTargetForHwnd(hwnd, true)?;
            let visual = composition.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            root.SetRoot(&visual)?;
            composition.Commit()?;
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let buffer: IDXGISurface = swapchain.GetBuffer(0)?;
            let target = d2d.CreateDxgiSurfaceRenderTarget(
                &buffer,
                &D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: dpi as f32,
                    dpiY: dpi as f32,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                },
            )?;
            // Grayscale antialiasing is correct for translucent surfaces. The
            // swapchain is physical-sized at native monitor DPI, never enlarged.
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = write.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                FONT_SIZE,
                w!("en-us"),
            )?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let ellipsis = write.CreateEllipsisTrimmingSign(&format)?;
            format.SetTrimming(
                &DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    ..Default::default()
                },
                &ellipsis,
            )?;
            Ok(Self {
                _d3d: d3d,
                _composition: composition,
                _root: root,
                _visual: visual,
                swapchain,
                target,
                write,
                format,
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        layout: Layout,
        prompts: &[RewritePrompt],
        offset: usize,
        hover: Option<usize>,
        pressed: Option<usize>,
        dpi: u32,
        theme: Theme,
        acrylic_requested: bool,
        icons: &mut IconRenderer,
    ) -> windows::core::Result<()> {
        unsafe {
            let rt = &self.target;
            rt.BeginDraw();
            rt.Clear(Some(&color(0, 0, 0, 0.0)));
            let panel = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: layout.width,
                    bottom: layout.height,
                },
                radiusX: RADIUS,
                radiusY: RADIUS,
            };
            let brush = rt.CreateSolidColorBrush(&theme.surface, None)?;
            if !acrylic_requested {
                // The accent material already owns the theme tint and live blur.
                // Keep the foreground transparent instead of stacking another
                // full-panel fill over it. Text/icons never enter the blur pass.
                rt.FillRoundedRectangle(&panel, &brush);
            }
            for row in 0..layout.visible {
                let index = offset + row;
                let Some(prompt) = prompts.get(index) else {
                    break;
                };
                let top = PADDING + row as f32 * layout.row;
                let highlighted = hover == Some(index);
                if highlighted || pressed == Some(index) {
                    brush.SetColor(&if pressed == Some(index) {
                        theme.pressed
                    } else {
                        theme.hover
                    });
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: D2D_RECT_F {
                                left: PADDING,
                                top: top + 2.0,
                                right: layout.content_right(),
                                bottom: top + layout.row - 2.0,
                            },
                            radiusX: 6.0,
                            radiusY: 6.0,
                        },
                        &brush,
                    );
                }
                let icon_top = top + (layout.row - ICON_SIZE) / 2.0;
                icons.draw(
                    rt,
                    &prompt.icon,
                    D2D_RECT_F {
                        left: ICON_LEFT,
                        top: icon_top,
                        right: ICON_LEFT + ICON_SIZE,
                        bottom: icon_top + ICON_SIZE,
                    },
                    dpi,
                    theme.dark,
                    None,
                )?;
                brush.SetColor(&if highlighted {
                    theme.hovered_foreground()
                } else {
                    theme.foreground
                });
                let title: Vec<u16> = prompt.title.encode_utf16().collect();
                let text = self.write.CreateTextLayout(
                    &title,
                    &self.format,
                    (layout.content_right() - TITLE_LEFT - 6.0).max(1.0),
                    layout.row,
                )?;
                rt.DrawTextLayout(
                    Vector2 {
                        X: TITLE_LEFT,
                        Y: top,
                    },
                    &text,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
                if row + 1 < layout.visible && index + 1 < prompts.len() {
                    brush.SetColor(&theme.separator);
                    // A single physical pixel prevents fuzzy doubled rules at
                    // fractional monitor scales.
                    let scale = dpi as f32 / 96.0;
                    let y = ((top + layout.row) * scale).floor() / scale + 0.5 / scale;
                    rt.DrawLine(
                        Vector2 { X: 8.0, Y: y },
                        Vector2 {
                            X: layout.content_right(),
                            Y: y,
                        },
                        &brush,
                        1.0 / scale,
                        None,
                    );
                }
            }
            if layout.has_scrollbar() {
                let metrics = layout.scrollbar(offset);
                let left = layout.scrollbar_left();
                let mut bar = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left,
                        top: SCROLLBAR_INSET,
                        right: left + SCROLLBAR_WIDTH,
                        bottom: layout.height - SCROLLBAR_INSET,
                    },
                    radiusX: SCROLLBAR_WIDTH / 2.0,
                    radiusY: SCROLLBAR_WIDTH / 2.0,
                };
                brush.SetColor(&theme.track);
                rt.FillRoundedRectangle(&bar, &brush);
                bar.rect.top += metrics.top as f32 / SCROLLBAR_SCALE;
                bar.rect.bottom = bar.rect.top + metrics.thumb as f32 / SCROLLBAR_SCALE;
                brush.SetColor(&theme.thumb);
                rt.FillRoundedRectangle(&bar, &brush);
            }
            brush.SetColor(&theme.border);
            rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: 0.5,
                        top: 0.5,
                        right: layout.width - 0.5,
                        bottom: layout.height - 0.5,
                    },
                    radiusX: RADIUS - 0.5,
                    radiusY: RADIUS - 0.5,
                },
                &brush,
                1.0,
                None,
            );
            rt.EndDraw(None, None)?;
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()
        }
    }
}
