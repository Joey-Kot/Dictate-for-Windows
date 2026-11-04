//! Pixel layout shared by status painting and spinner-only invalidation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusContent {
    pub icon_left: i32,
    pub icon_top: i32,
    pub icon_size: i32,
    pub text_left: i32,
    pub text_right: i32,
}

impl StatusContent {
    pub fn centered(badge: [i32; 4], text_width: i32, dpi: u32) -> Self {
        let scale = |value: i32| ((i64::from(value) * i64::from(dpi.max(96)) + 48) / 96) as i32;
        let [left, top, right, bottom] = badge;
        let icon_size = scale(16)
            .min((right - left).max(0))
            .min((bottom - top).max(0));
        let padding = scale(8);
        let gap = scale(7).min((right - left - icon_size).max(0));
        let text_width = text_width
            .max(0)
            .min((right - left - padding * 2 - icon_size - gap).max(0));
        let content_width = icon_size + gap + text_width;
        let icon_left = left + (right - left - content_width) / 2;
        let text_left = icon_left + icon_size + gap;
        Self {
            icon_left,
            icon_top: top + (bottom - top - icon_size) / 2,
            icon_size,
            text_left,
            text_right: text_left + text_width,
        }
    }

    pub fn icon_rect(self) -> [i32; 4] {
        [
            self.icon_left,
            self.icon_top,
            self.icon_left + self.icon_size,
            self.icon_top + self.icon_size,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_and_label_are_centered_together_for_different_widths_and_dpi() {
        for dpi in [96, 120, 144, 192] {
            let scale = |value: i32| (value * dpi as i32 + 48) / 96;
            let badge = [scale(330), scale(41), scale(482), scale(67)];
            // Different English/localized labels and a label requiring truncation.
            for width in [26, 39, 61, 73, 84, 240] {
                let layout = StatusContent::centered(badge, scale(width), dpi);
                assert!((layout.icon_left + layout.text_right - badge[0] - badge[2]).abs() <= 1);
                assert!((layout.icon_top * 2 + layout.icon_size - badge[1] - badge[3]).abs() <= 1);
                assert!(layout.icon_left >= badge[0] + scale(8) - 1);
                assert!(layout.text_right <= badge[2] - scale(8) + 1);
                assert!(layout.icon_left + layout.icon_size < layout.text_left);
                assert!(layout.text_right >= layout.text_left);
            }
        }
    }

    #[test]
    fn animation_damage_fits_inside_the_badge_and_excludes_label_pixels() {
        let badge = [500, 12, 804, 64];
        let layout = StatusContent::centered(badge, 146, 192);
        let [left, top, right, bottom] = layout.icon_rect();
        assert!(left > badge[0] && top > badge[1]);
        assert!(right < layout.text_left && bottom < badge[3]);
        assert!(left + right >= badge[0] * 2);
    }
}
