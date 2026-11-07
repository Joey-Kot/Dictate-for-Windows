use std::f32::consts::TAU;

use dictate_core::config::{WindowColor, WindowColors, WindowTheme};

pub(crate) fn status_text_color(background: WindowColor) -> WindowColor {
    contrasting_foreground(background, WindowColor([174, 189, 192]))
}

pub(crate) fn contrasting_foreground(
    background: WindowColor,
    preferred: WindowColor,
) -> WindowColor {
    let luminance = |color: WindowColor| {
        let [red, green, blue] = color.0.map(|channel| {
            let channel = f32::from(channel) / 255.0;
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        });
        0.2126 * red + 0.7152 * green + 0.0722 * blue
    };
    let background = luminance(background);
    let foreground = luminance(preferred);
    if (background.max(foreground) + 0.05) / (background.min(foreground) + 0.05) >= 4.5 {
        preferred
    } else if background >= 0.175 {
        WindowColor([0; 3])
    } else {
        WindowColor([255; 3])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorField {
    Background,
    Button,
    Font,
    Border,
}

impl ColorField {
    pub const ALL: [Self; 4] = [Self::Background, Self::Button, Self::Font, Self::Border];

    #[cfg(windows)]
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Background => "WINDOW_BACKGROUND_COLOR",
            Self::Button => "WINDOW_BUTTON_COLOR",
            Self::Font => "WINDOW_FONT_COLOR",
            Self::Border => "WINDOW_BORDER_COLOR",
        }
    }
}

pub(crate) struct ColorDrafts {
    theme: WindowTheme,
    selection: [Hsv; ColorField::ALL.len()],
    invalid_hex: [Option<String>; ColorField::ALL.len()],
    auto_font: bool,
}

impl ColorDrafts {
    pub fn new(colors: WindowColors, theme: WindowTheme) -> Self {
        Self {
            theme,
            selection: [
                colors.background.into(),
                colors.button.into(),
                colors
                    .font
                    .unwrap_or_else(|| status_text_color(colors.background))
                    .into(),
                colors.border.into(),
            ],
            invalid_hex: Default::default(),
            auto_font: colors.font.is_none(),
        }
    }

    pub fn theme(&self) -> WindowTheme {
        self.theme
    }

    pub fn select_theme(&mut self, theme: WindowTheme) {
        match theme {
            WindowTheme::Dark => *self = Self::new(WindowColors::DEFAULT, theme),
            WindowTheme::Light => *self = Self::new(WindowColors::LIGHT, theme),
            WindowTheme::Custom => self.theme = theme,
        }
    }

    pub fn selection(&self, field: ColorField) -> Hsv {
        if field == ColorField::Font && self.auto_font {
            status_text_color(self.selection[ColorField::Background as usize].rgb()).into()
        } else {
            self.selection[field as usize]
        }
    }

    pub fn set_selection(&mut self, field: ColorField, selection: Hsv) {
        self.selection[field as usize] = selection;
        self.invalid_hex[field as usize] = None;
        if field == ColorField::Font {
            self.auto_font = false;
        }
    }

    pub fn edit_hex(&mut self, field: ColorField, text: String) {
        if let Ok(color) = text.parse::<WindowColor>() {
            self.set_selection(field, color.into());
        } else {
            self.invalid_hex[field as usize] = Some(text);
        }
    }

    pub fn hex(&self, field: ColorField) -> String {
        self.invalid_hex[field as usize]
            .clone()
            .unwrap_or_else(|| self.selection(field).rgb().to_string())
    }

    pub fn is_valid(&self, field: ColorField) -> bool {
        self.invalid_hex[field as usize].is_none()
    }

    pub fn reset(&mut self, field: ColorField) {
        let colors = match self.theme {
            WindowTheme::Dark => WindowColors::DEFAULT,
            WindowTheme::Light | WindowTheme::Custom => WindowColors::LIGHT,
        };
        let defaults = Self::new(colors, self.theme);
        self.set_selection(field, defaults.selection(field));
        if field == ColorField::Font {
            self.auto_font = defaults.auto_font;
        }
    }

    pub fn values(&self) -> Result<WindowColors, ColorField> {
        if let Some(field) = ColorField::ALL
            .into_iter()
            .find(|field| !self.is_valid(*field))
        {
            return Err(field);
        }
        Ok(WindowColors {
            background: self.selection(ColorField::Background).rgb(),
            button: self.selection(ColorField::Button).rgb(),
            font: (!self.auto_font).then(|| self.selection(ColorField::Font).rgb()),
            border: self.selection(ColorField::Border).rgb(),
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Hsv {
    hue: f32,
    saturation: f32,
    pub value: u8,
}

impl Hsv {
    // Red is at the right edge. Hue runs clockwise in screen coordinates.
    pub fn from_wheel(x: f32, y: f32, value: u8) -> Self {
        Self {
            hue: (y.atan2(x) / TAU).rem_euclid(1.0),
            saturation: x.hypot(y).min(1.0),
            value,
        }
    }

    pub fn from_wheel_pointer(x: f32, y: f32, radius: f32, value: u8) -> Self {
        // On an even-sized wheel, the center lies between four pixels.
        if x.hypot(y) <= 1.0 {
            Self::from_wheel(0.0, 0.0, value)
        } else {
            Self::from_wheel(x / radius, y / radius, value)
        }
    }

    pub fn wheel_position(self) -> (f32, f32) {
        let (sin, cos) = (self.hue * TAU).sin_cos();
        (cos * self.saturation, sin * self.saturation)
    }

    pub fn rgb(self) -> WindowColor {
        let value = f32::from(self.value);
        let chroma = value * self.saturation;
        let sector = self.hue * 6.0;
        let middle = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
        let channels = match sector as u8 {
            0 => [chroma, middle, 0.0],
            1 => [middle, chroma, 0.0],
            2 => [0.0, chroma, middle],
            3 => [0.0, middle, chroma],
            4 => [middle, 0.0, chroma],
            _ => [chroma, 0.0, middle],
        };
        WindowColor(channels.map(|channel| (channel + value - chroma).round() as u8))
    }
}

impl From<WindowColor> for Hsv {
    fn from(color: WindowColor) -> Self {
        let [red, green, blue] = color.0.map(f32::from);
        let max = red.max(green).max(blue);
        let delta = max - red.min(green).min(blue);
        let sector = if delta == 0.0 {
            0.0
        } else if max == red {
            (green - blue) / delta
        } else if max == green {
            2.0 + (blue - red) / delta
        } else {
            4.0 + (red - green) / delta
        };
        Self {
            hue: (sector / 6.0).rem_euclid(1.0),
            saturation: if max == 0.0 { 0.0 } else { delta / max },
            value: max as u8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_round_trip_through_the_picker() {
        assert_eq!(Hsv::from(WindowColor::DEFAULT).rgb(), WindowColor::DEFAULT);
        for red in (0..=255).step_by(17) {
            for green in (0..=255).step_by(17) {
                for blue in (0..=255).step_by(17) {
                    let original = WindowColor([red, green, blue]);
                    let selection = Hsv::from(original);
                    assert_eq!(selection.rgb(), original);
                    let (x, y) = selection.wheel_position();
                    assert_eq!(Hsv::from_wheel(x, y, selection.value).rgb(), original);
                }
            }
        }
    }

    #[test]
    fn brightness_keeps_hue_and_saturation_at_zero() {
        let mut selected = Hsv::from_wheel(-2.0, 0.0, 0);
        assert_eq!(selected.rgb(), WindowColor([0, 0, 0]));
        selected.value = 90;
        assert_eq!(selected.rgb(), WindowColor([0, 90, 90]));
        assert_eq!(Hsv::from_wheel(0.0, 0.0, 255).rgb(), WindowColor([255; 3]));
    }

    #[test]
    fn pointer_can_select_neutral_colors_at_odd_and_even_wheel_centers() {
        for radius in [94.0_f32, 94.5] {
            let nearest_pixel = radius.floor() + 0.5 - radius;
            for value in [128, 255] {
                let selected = Hsv::from_wheel_pointer(nearest_pixel, nearest_pixel, radius, value);
                assert_eq!(selected.rgb(), WindowColor([value; 3]));
                assert_eq!(selected.wheel_position(), (0.0, 0.0));
            }
        }
        assert_ne!(
            Hsv::from_wheel_pointer(2.0, 0.0, 94.0, 255).rgb(),
            WindowColor([255; 3])
        );
    }

    #[test]
    fn status_color_stays_readable_on_light_and_dark_backgrounds() {
        assert_eq!(
            status_text_color(WindowColor::DEFAULT),
            WindowColor([174, 189, 192])
        );
        assert_eq!(
            status_text_color(WindowColor([255; 3])),
            WindowColor([0; 3])
        );
        assert_eq!(
            status_text_color(WindowColor([100; 3])),
            WindowColor([255; 3])
        );
    }

    #[test]
    fn resets_and_invalid_hex_are_independent_across_color_fields() {
        let mut draft = ColorDrafts::new(WindowColors::DEFAULT, WindowTheme::Dark);
        draft.edit_hex(ColorField::Background, "FFFFFF".into());
        draft.edit_hex(ColorField::Button, "FF6600".into());
        draft.edit_hex(ColorField::Border, "123".into());
        assert_eq!(draft.hex(ColorField::Border), "123");
        assert_eq!(draft.values(), Err(ColorField::Border));
        assert_eq!(draft.hex(ColorField::Font), "#000000");

        draft.edit_hex(ColorField::Font, "0080FF".into());
        draft.reset(ColorField::Border);
        assert_eq!(
            draft.values().unwrap(),
            WindowColors {
                background: WindowColor([255; 3]),
                button: WindowColor([255, 102, 0]),
                font: Some(WindowColor([0, 128, 255])),
                border: WindowColors::DEFAULT.border,
            }
        );

        draft.reset(ColorField::Font);
        assert_eq!(draft.hex(ColorField::Font), "#000000");
        assert_eq!(draft.values().unwrap().font, None);
        draft.reset(ColorField::Background);
        assert_eq!(draft.hex(ColorField::Font), "#AEBDC0");
        assert_eq!(draft.hex(ColorField::Button), "#FF6600");
    }

    #[test]
    fn choosing_presets_replaces_colors_but_edits_keep_the_selected_theme() {
        let mut draft = ColorDrafts::new(WindowColors::DEFAULT, WindowTheme::Dark);
        draft.edit_hex(ColorField::Background, "#123456".into());
        draft.edit_hex(ColorField::Button, "bad".into());
        assert_eq!(draft.theme(), WindowTheme::Dark);

        draft.select_theme(WindowTheme::Light);
        assert_eq!(draft.theme(), WindowTheme::Light);
        assert_eq!(draft.values(), Ok(WindowColors::LIGHT));
        assert_eq!(draft.hex(ColorField::Background), "#E9FFF6");
        assert_eq!(draft.hex(ColorField::Button), "#D3E8FF");
        assert_eq!(draft.hex(ColorField::Font), "#000000");
        assert_eq!(draft.hex(ColorField::Border), "#E9FFF6");

        draft.edit_hex(ColorField::Font, "#123456".into());
        assert_eq!(draft.theme(), WindowTheme::Light);
        draft.reset(ColorField::Font);
        assert_eq!(draft.values(), Ok(WindowColors::LIGHT));

        draft.select_theme(WindowTheme::Dark);
        assert_eq!(draft.theme(), WindowTheme::Dark);
        assert_eq!(draft.values(), Ok(WindowColors::DEFAULT));
        draft.edit_hex(ColorField::Border, "#123456".into());
        draft.select_theme(WindowTheme::Dark);
        assert_eq!(draft.values(), Ok(WindowColors::DEFAULT));
    }

    #[test]
    fn custom_keeps_current_edits_and_each_reset_uses_light() {
        let mut draft = ColorDrafts::new(WindowColors::DEFAULT, WindowTheme::Dark);
        draft.edit_hex(ColorField::Background, "#ABCDEF".into());
        draft.edit_hex(ColorField::Button, "invalid".into());
        draft.select_theme(WindowTheme::Custom);
        assert_eq!(draft.theme(), WindowTheme::Custom);
        assert_eq!(draft.hex(ColorField::Background), "#ABCDEF");
        assert_eq!(draft.hex(ColorField::Button), "invalid");

        draft.reset(ColorField::Button);
        assert_eq!(draft.hex(ColorField::Button), "#D3E8FF");
        assert_eq!(draft.hex(ColorField::Background), "#ABCDEF");
        for field in [ColorField::Background, ColorField::Font, ColorField::Border] {
            draft.reset(field);
        }
        assert_eq!(draft.theme(), WindowTheme::Custom);
        assert_eq!(draft.values(), Ok(WindowColors::LIGHT));
    }
}
