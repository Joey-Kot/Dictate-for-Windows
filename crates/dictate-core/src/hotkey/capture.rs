//! Platform-independent keyboard recorder and stable config/display conversion.
use super::ParsedHotkey;

pub fn modifier(vk: u32) -> u32 {
    match vk {
        0x10 | 0xa0 | 0xa1 => 4,
        0x11 | 0xa2 | 0xa3 => 2,
        0x12 | 0xa4 | 0xa5 => 1,
        _ => 0,
    }
}

pub fn allowed(vk: u32) -> bool {
    (0x08..=0xfe).contains(&vk)
        && modifier(vk) == 0
        && !matches!(vk, 0x08 | 0x09 | 0x0d | 0x13 | 0x14 | 0x21..=0x28
            | 0x2c..=0x2e | 0x5b..=0x5d | 0x90 | 0x91 | 0xe7)
}

const NAMES: &[(u32, &str, &str)] = &[
    // Legacy config keys remain readable, but are rejected by the GUI recorder.
    (0x08, "backspace", "Backspace"),
    (0x09, "tab", "Tab"),
    (0x0d, "enter", "Enter"),
    (0x13, "pause", "Pause"),
    (0x14, "capslock", "Caps Lock"),
    (0x21, "pageup", "Page Up"),
    (0x22, "pagedown", "Page Down"),
    (0x23, "end", "End"),
    (0x24, "home", "Home"),
    (0x25, "left", "Left"),
    (0x26, "up", "Up"),
    (0x27, "right", "Right"),
    (0x28, "down", "Down"),
    (0x2c, "printscreen", "Print Screen"),
    (0x2d, "insert", "Insert"),
    (0x2e, "delete", "Delete"),
    (0x90, "numlock", "Num Lock"),
    (0x91, "scrolllock", "Scroll Lock"),
    (0x1b, "esc", "Esc"),
    (0x20, "space", "Space"),
    (0x6a, "multiply", "Num *"),
    (0x6b, "add", "Num +"),
    (0x6c, "separator", "Num Separator"),
    (0x6d, "subtract", "Num -"),
    (0x6e, "decimal", "Num Decimal"),
    (0x6f, "divide", "Num /"),
    (0xba, "semicolon", ";"),
    (0xbb, "equals", "="),
    (0xbc, "comma", ","),
    (0xbd, "hyphen", "-"),
    (0xbe, "period", "."),
    (0xbf, "slash", "/"),
    (0xc0, "backtick", "`"),
    (0xdb, "leftbracket", "["),
    (0xdc, "backslash", "\\"),
    (0xdd, "rightbracket", "]"),
    (0xde, "quote", "'"),
    (0xe2, "oem102", "OEM 102"),
    (0xa6, "browserback", "Browser Back"),
    (0xa7, "browserforward", "Browser Forward"),
    (0xa8, "browserrefresh", "Browser Refresh"),
    (0xa9, "browserstop", "Browser Stop"),
    (0xaa, "browsersearch", "Browser Search"),
    (0xab, "browserfavorites", "Browser Favorites"),
    (0xac, "browserhome", "Browser Home"),
    (0xad, "volumemute", "Volume Mute"),
    (0xae, "volumedown", "Volume Down"),
    (0xaf, "volumeup", "Volume Up"),
    (0xb0, "medianext", "Media Next"),
    (0xb1, "mediaprevious", "Media Previous"),
    (0xb2, "mediastop", "Media Stop"),
    (0xb3, "mediaplaypause", "Media Play/Pause"),
    (0xb4, "launchmail", "Launch Mail"),
    (0xb5, "launchmedia", "Launch Media"),
    (0xb6, "launchapp1", "Launch App 1"),
    (0xb7, "launchapp2", "Launch App 2"),
];

pub(super) fn named_key(name: &str) -> Option<u32> {
    NAMES
        .iter()
        .find(|(_, token, _)| *token == name)
        .map(|(vk, _, _)| *vk)
        .or(match name {
            ";" => Some(0xba),
            "=" => Some(0xbb),
            "," => Some(0xbc),
            "-" => Some(0xbd),
            "." => Some(0xbe),
            "/" => Some(0xbf),
            "`" => Some(0xc0),
            "[" => Some(0xdb),
            "\\" => Some(0xdc),
            "]" => Some(0xdd),
            "'" => Some(0xde),
            _ => None,
        })
}

pub fn format(key: ParsedHotkey, display: bool) -> String {
    let mut parts = Vec::new();
    for (mask, name, label) in [
        (2, "ctrl", "Ctrl"),
        (4, "shift", "Shift"),
        (1, "alt", "Alt"),
        (8, "win", "Win"),
    ] {
        if key.modifiers & mask != 0 {
            parts.push(if display { label } else { name }.to_string());
        }
    }
    let vk = key.virtual_key;
    if vk != 0 {
        parts.push(match vk {
            0x30..=0x39 | 0x41..=0x5a => {
                let text = char::from_u32(vk).unwrap().to_string();
                if display {
                    text
                } else {
                    text.to_ascii_lowercase()
                }
            }
            0x70..=0x87 => format!("{}{}", if display { "F" } else { "f" }, vk - 0x6f),
            0x60..=0x69 => format!("{}{}", if display { "Num " } else { "numpad" }, vk - 0x60),
            _ => NAMES
                .iter()
                .find(|(code, _, _)| *code == vk)
                .map(|(_, name, label)| if display { *label } else { *name }.to_string())
                .unwrap_or_else(|| format!("vk_{vk:02x}")),
        });
    }
    parts.join(if display { " + " } else { "+" })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    Preview(ParsedHotkey),
    Complete(ParsedHotkey),
    Invalid,
    Waiting,
}

pub struct Recorder {
    down: [bool; 256],
    candidate: Option<ParsedHotkey>,
    invalid: bool,
    priming: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            down: [false; 256],
            candidate: None,
            invalid: false,
            priming: false,
        }
    }
}

impl Recorder {
    // Do not turn the Tab/Shift+Tab used to enter the control into a binding.
    pub fn seed(&mut self, keys: impl IntoIterator<Item = u32>) {
        for vk in keys {
            if vk < 256 {
                self.down[vk as usize] = true;
                self.priming = true;
            }
        }
    }

    pub fn event(&mut self, vk: u32, pressed: bool) -> Update {
        if vk >= 256 {
            return Update::Invalid;
        }
        if self.down[vk as usize] == pressed {
            return self.preview();
        }
        self.down[vk as usize] = pressed;
        if self.priming {
            self.priming = self.down.iter().any(|down| *down);
            return Update::Waiting;
        }
        if pressed {
            if modifier(vk) == 0 {
                if !allowed(vk) || self.candidate.is_some() {
                    self.invalid = true;
                } else {
                    self.candidate = Some(ParsedHotkey {
                        modifiers: self.modifiers(),
                        virtual_key: vk,
                    });
                }
            } else if let Some(candidate) = &mut self.candidate {
                candidate.modifiers |= modifier(vk);
            }
        }
        if !self.down.iter().any(|down| *down) {
            let candidate = self.candidate.take();
            let invalid = std::mem::take(&mut self.invalid);
            if invalid {
                return Update::Invalid;
            }
            return candidate.map(Update::Complete).unwrap_or(Update::Waiting);
        }
        self.preview()
    }

    fn modifiers(&self) -> u32 {
        self.down
            .iter()
            .enumerate()
            .filter(|(_, down)| **down)
            .fold(0, |mask, (vk, _)| mask | modifier(vk as u32))
    }

    fn preview(&self) -> Update {
        if self.invalid {
            Update::Invalid
        } else {
            Update::Preview(self.candidate.unwrap_or(ParsedHotkey {
                modifiers: self.modifiers(),
                virtual_key: 0,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::parse_hotkey;

    #[test]
    fn every_recordable_key_round_trips() {
        for vk in (0..256).filter(|vk| allowed(*vk)) {
            for modifiers in 0..8 {
                let key = ParsedHotkey {
                    modifiers,
                    virtual_key: vk,
                };
                assert_eq!(parse_hotkey(&format(key, false)).unwrap(), key);
            }
        }
        assert_eq!(parse_hotkey("ctrl+;").unwrap().virtual_key, 0xba);
        assert_ne!(parse_hotkey("1").unwrap(), parse_hotkey("numpad1").unwrap());
    }

    #[test]
    fn records_on_final_release_and_ignores_repeat() {
        let mut r = Recorder::default();
        r.event(0xa2, true);
        r.event(0xa4, true);
        r.event(0x53, true);
        r.event(0x53, true);
        assert!(matches!(r.event(0xa2, false), Update::Preview(_)));
        r.event(0x53, false);
        assert_eq!(
            r.event(0xa4, false),
            Update::Complete(parse_hotkey("ctrl+alt+s").unwrap())
        );
        r.event(0x70, true);
        assert_eq!(
            r.event(0x70, false),
            Update::Complete(parse_hotkey("f1").unwrap())
        );
    }

    #[test]
    fn forbidden_keys_poison_whole_chord() {
        for vk in [
            0x08, 0x09, 0x0d, 0x13, 0x14, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2c,
            0x2d, 0x2e, 0x5b, 0x5c, 0x5d, 0x90, 0x91,
        ] {
            assert!(!allowed(vk));
            let mut r = Recorder::default();
            r.event(vk, true);
            r.event(0x41, true);
            r.event(vk, false);
            assert_eq!(r.event(0x41, false), Update::Invalid);
            r.event(0x1b, true);
            assert_eq!(
                r.event(0x1b, false),
                Update::Complete(parse_hotkey("esc").unwrap())
            );
        }
    }

    #[test]
    fn rejects_multiple_keys_and_pure_modifiers_and_entry_keys() {
        let mut r = Recorder::default();
        r.event(0x41, true);
        r.event(0x42, true);
        r.event(0x41, false);
        assert_eq!(r.event(0x42, false), Update::Invalid);
        r.event(0xa0, true);
        assert_eq!(r.event(0xa0, false), Update::Waiting);
        r.seed([0x09, 0xa0]);
        r.event(0x09, false);
        assert_eq!(r.event(0xa0, false), Update::Waiting);
    }

    #[test]
    fn escape_and_symbols_keep_modifiers_in_either_release_order() {
        for (modifier, vk, spec) in [(0xa5, 0x1b, "alt+esc"), (0xa1, 0xbb, "shift+equals")] {
            for release_modifier_first in [false, true] {
                let mut r = Recorder::default();
                r.event(modifier, true);
                r.event(vk, true);
                let (first, last) = if release_modifier_first {
                    (modifier, vk)
                } else {
                    (vk, modifier)
                };
                assert!(matches!(r.event(first, false), Update::Preview(_)));
                assert_eq!(
                    r.event(last, false),
                    Update::Complete(parse_hotkey(spec).unwrap())
                );
            }
        }
    }

    #[test]
    fn focus_entry_waits_for_held_keys_and_discard_resets_candidate() {
        let mut r = Recorder::default();
        r.seed([0xa0, 9]);
        r.event(0x41, true);
        r.event(9, false);
        r.event(0xa0, false);
        assert_eq!(r.event(0x41, false), Update::Waiting);
        r.event(0xa2, true);
        r.event(0x41, true);
        // Losing focus discards the recorder, keeping only the committed field value.
        r = Recorder::default();
        r.event(0x70, true);
        assert_eq!(
            r.event(0x70, false),
            Update::Complete(parse_hotkey("f1").unwrap())
        );
    }
}
