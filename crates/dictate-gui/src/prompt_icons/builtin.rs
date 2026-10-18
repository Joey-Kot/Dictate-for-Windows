//! User-supplied two-tone vector artwork embedded in the executable.

use std::collections::HashMap;
use std::sync::OnceLock;

use dictate_core::rewrite::BuiltinPromptIcon;

use super::IconPixels;

fn artwork(icon: BuiltinPromptIcon) -> &'static str {
    match icon {
        BuiltinPromptIcon::Chat => include_str!("../../../../assets/prompt-icons/chat.svg"),
        BuiltinPromptIcon::Messages => include_str!("../../../../assets/prompt-icons/messages.svg"),
        BuiltinPromptIcon::Translate => {
            include_str!("../../../../assets/prompt-icons/translate.svg")
        }
        BuiltinPromptIcon::Eye => include_str!("../../../../assets/prompt-icons/eye.svg"),
        BuiltinPromptIcon::Feather => include_str!("../../../../assets/prompt-icons/feather.svg"),
        BuiltinPromptIcon::Document => {
            include_str!("../../../../assets/prompt-icons/document.svg")
        }
        BuiltinPromptIcon::Shield => include_str!("../../../../assets/prompt-icons/shield.svg"),
        BuiltinPromptIcon::PieChart => {
            include_str!("../../../../assets/prompt-icons/pie-chart.svg")
        }
        BuiltinPromptIcon::Tags => include_str!("../../../../assets/prompt-icons/tags.svg"),
        BuiltinPromptIcon::TextCard => {
            include_str!("../../../../assets/prompt-icons/text-card.svg")
        }
        BuiltinPromptIcon::Box => include_str!("../../../../assets/prompt-icons/box.svg"),
        BuiltinPromptIcon::Flask => include_str!("../../../../assets/prompt-icons/flask.svg"),
        BuiltinPromptIcon::Globe => include_str!("../../../../assets/prompt-icons/globe.svg"),
        BuiltinPromptIcon::Cookie => include_str!("../../../../assets/prompt-icons/cookie.svg"),
        BuiltinPromptIcon::Window => include_str!("../../../../assets/prompt-icons/window.svg"),
        BuiltinPromptIcon::Plug => include_str!("../../../../assets/prompt-icons/plug.svg"),
        BuiltinPromptIcon::Settings => include_str!("../../../../assets/prompt-icons/settings.svg"),
        BuiltinPromptIcon::Layers => include_str!("../../../../assets/prompt-icons/layers.svg"),
        BuiltinPromptIcon::Key => include_str!("../../../../assets/prompt-icons/key.svg"),
        BuiltinPromptIcon::Network => include_str!("../../../../assets/prompt-icons/network.svg"),
        BuiltinPromptIcon::Hierarchy => {
            include_str!("../../../../assets/prompt-icons/hierarchy.svg")
        }
        BuiltinPromptIcon::BarChart => {
            include_str!("../../../../assets/prompt-icons/bar-chart.svg")
        }
        BuiltinPromptIcon::Microphone => {
            include_str!("../../../../assets/prompt-icons/microphone.svg")
        }
        BuiltinPromptIcon::Fingerprint => {
            include_str!("../../../../assets/prompt-icons/fingerprint.svg")
        }
        BuiltinPromptIcon::Layout => include_str!("../../../../assets/prompt-icons/layout.svg"),
    }
}

pub(super) fn pixels(icon: Option<BuiltinPromptIcon>, size: u32) -> IconPixels {
    // Parse trusted embedded vectors once. Monitor DPI changes only rasterize to
    // a new physical size; light and dark themes share the original two colors.
    static TREES: OnceLock<HashMap<BuiltinPromptIcon, resvg::usvg::Tree>> = OnceLock::new();
    let trees = TREES.get_or_init(|| {
        BuiltinPromptIcon::ALL
            .into_iter()
            .map(|icon| {
                let tree =
                    resvg::usvg::Tree::from_str(artwork(icon), &resvg::usvg::Options::default())
                        .expect("embedded prompt icons are valid SVG");
                (icon, tree)
            })
            .collect()
    });
    let tree = &trees[&icon.unwrap_or(BuiltinPromptIcon::Chat)];
    let bounds = tree.root().abs_stroke_bounding_box();
    // The supplied 1024-unit viewBoxes contain different amounts of whitespace.
    // Fit their actual artwork, preserving aspect ratio and every path. A clear
    // physical-pixel margin protects antialiased edges at the smallest menu size.
    let canvas = size as f32;
    let margin = (canvas * 0.04).max(1.0).min(canvas * 0.25);
    let scale = (canvas - margin * 2.0) / bounds.width().max(bounds.height());
    let transform = resvg::tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (canvas - bounds.width() * scale) * 0.5 - bounds.x() * scale,
        (canvas - bounds.height() * scale) * 0.5 - bounds.y() * scale,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("valid icon size");
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let mut bgra = pixmap.take();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    IconPixels { size, bgra }
}
