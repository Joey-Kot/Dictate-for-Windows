//! Shared SVG status artwork for settings tests and batch transcription.
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::prompt_icons::IconPixels;

pub const FRAME_INTERVAL_MS: u32 = 20;
const LOOP_MS: u128 = 1200;
const FRAME_COUNT: u8 = (LOOP_MS / FRAME_INTERVAL_MS as u128) as u8;
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 256;
const MAX_ICON_SIDE: u32 = 256;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusIcon {
    Processing,
    Success,
    Failed,
    BatchCompleted,
    BatchFailed,
}

fn artwork(icon: StatusIcon) -> &'static str {
    match icon {
        StatusIcon::Processing => include_str!("../../../assets/status-icons/Processing.svg"),
        StatusIcon::Success => include_str!("../../../assets/status-icons/Success1.svg"),
        StatusIcon::Failed => include_str!("../../../assets/status-icons/Failed1.svg"),
        StatusIcon::BatchCompleted => include_str!("../../../assets/status-icons/Success2.svg"),
        StatusIcon::BatchFailed => include_str!("../../../assets/status-icons/Failed2.svg"),
    }
}

fn frame_index(elapsed: Duration) -> u8 {
    // Use wall-clock elapsed time, so delayed Windows timers never slow down
    // the supplied animation. 60 frames cover exactly one 1.2-second loop.
    ((elapsed.as_nanos() % (LOOP_MS * 1_000_000)) * FRAME_COUNT as u128 / (LOOP_MS * 1_000_000))
        as u8
}

#[derive(Default)]
struct FrameCache {
    frames: HashMap<(StatusIcon, u32, u8), Arc<IconPixels>>,
    bytes: usize,
}

impl FrameCache {
    fn pixels(&mut self, icon: StatusIcon, size: u32, elapsed: Duration) -> Arc<IconPixels> {
        let size = size.clamp(1, MAX_ICON_SIDE);
        let frame = if icon == StatusIcon::Processing {
            frame_index(elapsed)
        } else {
            0
        };
        let key = (icon, size, frame);
        if let Some(pixels) = self.frames.get(&key) {
            return Arc::clone(pixels);
        }
        let pixels = Arc::new(render(icon, size, frame));
        if self.frames.len() >= MAX_CACHE_ENTRIES
            || self.bytes + pixels.bgra.len() > MAX_CACHE_BYTES
        {
            self.frames.clear();
            self.bytes = 0;
        }
        self.bytes += pixels.bgra.len();
        self.frames.insert(key, Arc::clone(&pixels));
        pixels
    }
}

pub fn pixels(icon: StatusIcon, size: u32, elapsed: Duration) -> Arc<IconPixels> {
    static CACHE: OnceLock<parking_lot::Mutex<FrameCache>> = OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .pixels(icon, size, elapsed)
}

fn render(icon: StatusIcon, size: u32, frame: u8) -> IconPixels {
    // resvg is a static renderer. Resolve the supplied rotate animation to a
    // static group transform per frame, parsing each frame only once. Keeping
    // the transform on the original group preserves the background and opacity
    // compositing exactly, including antialiased dot edges.
    static TREES: [OnceLock<resvg::usvg::Tree>; FRAME_COUNT as usize + 4] =
        [const { OnceLock::new() }; FRAME_COUNT as usize + 4];
    let index = match icon {
        StatusIcon::Processing => frame as usize,
        StatusIcon::Success => FRAME_COUNT as usize,
        StatusIcon::Failed => FRAME_COUNT as usize + 1,
        StatusIcon::BatchCompleted => FRAME_COUNT as usize + 2,
        StatusIcon::BatchFailed => FRAME_COUNT as usize + 3,
    };
    let tree = TREES[index].get_or_init(|| {
        let source = if icon == StatusIcon::Processing {
            let rotation = frame as f32 * 360.0 / FRAME_COUNT as f32;
            artwork(icon).replacen(
                "<g ",
                &format!("<g transform=\"rotate({rotation} 64 64)\" "),
                1,
            )
        } else {
            artwork(icon).to_owned()
        };
        resvg::usvg::Tree::from_str(&source, &resvg::usvg::Options::default())
            .expect("embedded test-status artwork is valid SVG")
    });
    let scale = size as f32 / 128.0;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("bounded status icon size");
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let mut bgra = pixmap.take();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    IconPixels { size, bgra }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplied_artwork_keeps_colors_transparency_and_dpi_sizes() {
        for (icon, background) in [
            (StatusIcon::Processing, [0x37, 0x29, 0x1f, 255]),
            (StatusIcon::Success, [0x5e, 0xc5, 0x22, 255]),
            (StatusIcon::Failed, [0x44, 0x44, 0xef, 255]),
            (StatusIcon::BatchCompleted, [0, 0, 0, 0]),
            (StatusIcon::BatchFailed, [0, 0, 0, 0]),
        ] {
            for size in [16, 20, 24, 25, 30, 32, 40, 48, 60, 128] {
                let pixels = pixels(icon, size, Duration::ZERO);
                assert_eq!(pixels.size, size);
                assert_eq!(pixels.bgra.len(), (size * size * 4) as usize);
                assert_eq!(&pixels.bgra[..4], &[0, 0, 0, 0]);
                let index = ((size / 6 * size + size / 2) * 4) as usize;
                assert_eq!(&pixels.bgra[index..index + 4], &background);
                assert!(
                    pixels
                        .bgra
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .all(|p| p[..3].iter().all(|v| *v <= p[3]))
                );
            }
        }
        for (icon, x, y, color) in [
            (StatusIcon::BatchCompleted, 55, 86, [0x5e, 0xc5, 0x22, 255]),
            (StatusIcon::BatchFailed, 44, 44, [0x44, 0x44, 0xef, 255]),
        ] {
            let pixels = pixels(icon, 128, Duration::ZERO);
            let index = (y * 128 + x) * 4;
            assert_eq!(&pixels.bgra[index..index + 4], &color);
        }
    }

    #[test]
    fn spinner_matches_svg_group_rotation_and_keeps_background_fixed() {
        let first = render(StatusIcon::Processing, 128, 0);
        for (milliseconds, degrees) in [(0, 0), (300, 90), (600, 180), (900, 270), (1200, 0)] {
            let actual = pixels(
                StatusIcon::Processing,
                128,
                Duration::from_millis(milliseconds),
            );
            let source = artwork(StatusIcon::Processing).replace(
                "<g fill=\"#ffffff\">",
                &format!("<g fill=\"#ffffff\" transform=\"rotate({degrees} 64 64)\">"),
            );
            let tree =
                resvg::usvg::Tree::from_str(&source, &resvg::usvg::Options::default()).unwrap();
            let mut expected = resvg::tiny_skia::Pixmap::new(128, 128).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut expected.as_mut(),
            );
            let mut expected = expected.take();
            for pixel in expected.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let differences: Vec<_> = actual
                .bgra
                .iter()
                .zip(&expected)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| (i, *a, *b))
                .collect();
            assert!(
                differences.is_empty(),
                "at {milliseconds} ms: {} channel differences, first: {:?}, max: {:?}",
                differences.len(),
                &differences[..differences.len().min(12)],
                differences.iter().map(|(_, a, b)| a.abs_diff(*b)).max(),
            );
            for y in 0..128 {
                for x in 0..128 {
                    if !(32..96).contains(&x) || !(32..96).contains(&y) {
                        let i = (y * 128 + x) * 4;
                        assert_eq!(&actual.bgra[i..i + 4], &first.bgra[i..i + 4]);
                    }
                }
            }
        }
        assert_ne!(
            pixels(StatusIcon::Processing, 128, Duration::from_millis(300)).bgra,
            first.bgra
        );
    }

    #[test]
    fn delayed_ticks_keep_period_and_reuse_bounded_frames() {
        assert_eq!(frame_index(Duration::from_millis(1199)), 59);
        assert_eq!(frame_index(Duration::from_millis(1200)), 0);
        assert_eq!(frame_index(Duration::from_millis(1500)), 15);
        let mut cache = FrameCache::default();
        let first = cache.pixels(StatusIcon::Processing, 40, Duration::from_millis(300));
        let next = cache.pixels(StatusIcon::Processing, 40, Duration::from_millis(1500));
        assert!(Arc::ptr_eq(&first, &next));
        let success = cache.pixels(StatusIcon::Success, 40, Duration::ZERO);
        assert!(Arc::ptr_eq(
            &success,
            &cache.pixels(StatusIcon::Success, 40, Duration::from_secs(9))
        ));
        for frame in 0..FRAME_COUNT {
            cache.pixels(
                StatusIcon::Processing,
                MAX_ICON_SIDE,
                Duration::from_millis(frame as u64 * 20),
            );
            assert!(cache.bytes <= MAX_CACHE_BYTES);
            assert!(cache.frames.len() <= MAX_CACHE_ENTRIES);
        }
    }
}
