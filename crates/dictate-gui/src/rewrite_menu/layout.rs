#[cfg(not(windows))]
use crate::dropdown_scrollbar::Metrics;
#[cfg(windows)]
use crate::settings::ScrollbarMetrics as Metrics;

pub(super) const WIDTH: f32 = 248.0;
pub(super) const ROW: f32 = 40.0;
pub(super) const PADDING: f32 = 4.0;
pub(super) const FONT_SIZE: f32 = 14.0;
pub(super) const ICON_LEFT: f32 = 16.0;
pub(super) const ICON_SIZE: f32 = 22.0;
pub(super) const TITLE_LEFT: f32 = 50.0;
pub(super) const SCROLLBAR_INSET: f32 = 8.0;
pub(super) const SCROLLBAR_WIDTH: f32 = 4.0;
pub(super) const SCROLLBAR_SCALE: f32 = 100.0;
const SCROLLBAR_GUTTER: f32 = 16.0;
const SCROLLBAR_OUTER_GAP: f32 = 6.0;
const MIN_THUMB: f32 = 24.0;
// Match DWMWCP_ROUND's standard flyout corner. A larger foreground radius
// would leave a differently shaped system Acrylic backing visible underneath.
pub(super) const RADIUS: f32 = 8.0;
const MAX_ROWS: usize = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    pub width: f32,
    pub height: f32,
    pub row: f32,
    pub visible: usize,
    pub count: usize,
}

impl Layout {
    pub fn new(count: usize, work_width: i32, work_height: i32, dpi: u32) -> Self {
        let scale = 96.0 / dpi.max(1) as f32;
        let width = WIDTH.min((work_width.max(1) as f32 * scale).max(1.0));
        let available = (work_height.max(1) as f32 * scale - PADDING * 2.0).max(1.0);
        let visible = count
            .min(MAX_ROWS)
            .min((available / ROW).floor().max(1.0) as usize);
        let row = ROW.min(available);
        Self {
            width,
            height: if count == 0 {
                0.0
            } else {
                visible as f32 * row + PADDING * 2.0
            },
            row,
            visible,
            count,
        }
    }

    pub fn max_offset(self) -> usize {
        self.count.saturating_sub(self.visible)
    }

    pub fn has_scrollbar(self) -> bool {
        self.max_offset() > 0
    }

    pub fn content_right(self) -> f32 {
        self.width
            - if self.has_scrollbar() {
                SCROLLBAR_GUTTER
            } else {
                PADDING * 2.0
            }
    }

    pub fn hit(self, x: f32, y: f32, offset: usize) -> Option<usize> {
        if x < PADDING || x >= self.content_right() || y < PADDING || y >= self.height - PADDING {
            return None;
        }
        let row = ((y - PADDING) / self.row).floor() as usize;
        let index = offset.min(self.max_offset()) + row;
        (row < self.visible && index < self.count).then_some(index)
    }

    pub fn hit_scrollbar(self, x: f32, y: f32) -> bool {
        self.has_scrollbar()
            && x >= self.content_right()
            && x < self.width - PADDING
            && y >= SCROLLBAR_INSET
            && y < self.height - SCROLLBAR_INSET
    }

    pub fn scrollbar_left(self) -> f32 {
        self.width - SCROLLBAR_OUTER_GAP - SCROLLBAR_WIDTH
    }

    pub fn scrollbar(self, offset: usize) -> Metrics {
        Metrics::with_page(
            ((self.height - SCROLLBAR_INSET * 2.0).max(1.0) * SCROLLBAR_SCALE).round() as i32,
            self.visible as i32,
            self.count.min(i32::MAX as usize) as i32,
            offset.min(i32::MAX as usize) as i32,
            (MIN_THUMB * SCROLLBAR_SCALE).round() as i32,
        )
    }

    pub fn place(self, pointer: (i32, i32), work: Bounds, dpi: u32) -> Bounds {
        let scale = dpi.max(1) as f32 / 96.0;
        let width = ((self.width * scale).round() as i32).min((work.right - work.left).max(1));
        let height = ((self.height * scale).round() as i32).min((work.bottom - work.top).max(1));
        let gap = (6.0 * scale).round() as i32;
        let x = if pointer.0.saturating_add(gap).saturating_add(width) <= work.right {
            pointer.0.saturating_add(gap)
        } else {
            pointer.0.saturating_sub(gap).saturating_sub(width)
        }
        .clamp(work.left, (work.right - width).max(work.left));
        let y = if pointer.1.saturating_add(gap).saturating_add(height) <= work.bottom {
            pointer.1.saturating_add(gap)
        } else {
            pointer.1.saturating_sub(gap).saturating_sub(height)
        }
        .clamp(work.top, (work.bottom - height).max(work.top));
        Bounds {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rows_without_empty_or_partial_eighth_row() {
        for count in [0, 1, 6, 7, 8, 1000] {
            let layout = Layout::new(count, 1920, 1080, 96);
            assert_eq!(layout.visible, count.min(7));
            assert_eq!(layout.has_scrollbar(), count > 7);
            assert_eq!(
                layout.height,
                if count == 0 {
                    0.0
                } else {
                    count.min(7) as f32 * 40.0 + 8.0
                }
            );
            if count > 0 {
                assert_eq!(
                    layout.hit(100.0, 5.0, layout.max_offset()),
                    Some(count - layout.visible)
                );
                assert_eq!(
                    layout.hit(100.0, layout.height - 4.1, layout.max_offset()),
                    Some(count - 1)
                );
            }
            assert_eq!(layout.hit(100.0, layout.height - 3.0, 0), None);
        }
    }

    #[test]
    fn scrollbar_has_its_own_gutter_and_maps_to_last_row() {
        let layout = Layout::new(90, 1920, 1080, 144);
        let content_right = layout.content_right();
        let track_center = layout.scrollbar_left() + SCROLLBAR_WIDTH / 2.0;
        assert_eq!(layout.hit(content_right - 0.1, 40.0, 0), Some(0));
        assert_eq!(layout.hit(content_right, 40.0, 0), None);
        assert!(!layout.hit_scrollbar(content_right - 0.1, 40.0));
        assert!(layout.hit_scrollbar(content_right, 40.0));
        assert_eq!(layout.hit(track_center, 40.0, 0), None);
        assert!(layout.hit_scrollbar(track_center, 40.0));
        assert!(!layout.hit_scrollbar(layout.width - PADDING, 40.0));
        assert!(!layout.hit_scrollbar(track_center, SCROLLBAR_INSET - 0.1));
        assert!(!layout.hit_scrollbar(track_center, layout.height - SCROLLBAR_INSET));
        assert!(!Layout::new(7, 1920, 1080, 144).hit_scrollbar(track_center, 40.0));
        let metrics = layout.scrollbar(83);
        assert_eq!(metrics.height, 27200);
        assert_eq!(metrics.index_at(metrics.height - metrics.thumb), 83);
        assert_eq!(layout.hit(100.0, 44.1, 83), Some(84));
        assert_eq!(layout.hit(100.0, 283.0, 83), Some(89));
    }

    #[test]
    fn compact_scrollbar_drag_maps_back_to_prompt_offsets() {
        for count in [8, 90, 1000] {
            let layout = Layout::new(count, 1920, 1080, 96);
            for offset in [0, layout.max_offset() / 2, layout.max_offset()] {
                let metrics = layout.scrollbar(offset);
                assert_eq!(metrics.index_at(metrics.top) as usize, offset);
                assert!(metrics.top + metrics.thumb <= metrics.height);
            }
        }
    }

    #[test]
    fn placement_obeys_negative_workareas_and_mixed_dpi() {
        let work = Bounds {
            left: -1920,
            top: -300,
            right: 0,
            bottom: 740,
        };
        for dpi in [96, 120, 144, 192, 288] {
            let layout = Layout::new(100, 1920, 1040, dpi);
            for pointer in [(-1919, -299), (-1, 739), (-1000, 400)] {
                let placed = layout.place(pointer, work, dpi);
                assert!(placed.left >= work.left && placed.right <= work.right);
                assert!(placed.top >= work.top && placed.bottom <= work.bottom);
                assert_eq!(
                    placed.right - placed.left,
                    (248.0 * dpi as f32 / 96.0).round() as i32
                );
                assert_eq!(
                    placed.bottom - placed.top,
                    (288.0 * dpi as f32 / 96.0).round() as i32
                );
            }
        }
    }

    #[test]
    fn short_workarea_keeps_whole_rows_and_allows_reaching_every_item() {
        let layout = Layout::new(8, 500, 320, 144);
        assert_eq!(layout.visible, 5);
        assert_eq!(layout.height, 208.0);
        assert_eq!(layout.hit(100.0, 180.0, 3), Some(7));
    }
}
