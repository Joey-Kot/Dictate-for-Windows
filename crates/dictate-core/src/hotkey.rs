use std::collections::HashMap;
use std::fmt;

use thiserror::Error;

const MOD_ALT: u32 = 0x0001;
const MOD_CTRL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;

pub mod capture;

static CAPTURE_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Suppress application actions while the GUI owns keyboard input, including
/// releases drained after focus leaves the recorder.
pub fn set_capture_active(active: bool) {
    CAPTURE_ACTIVE.store(active, std::sync::atomic::Ordering::Release);
}

#[cfg(windows)]
fn capture_active() -> bool {
    CAPTURE_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParsedHotkey {
    pub modifiers: u32,
    pub virtual_key: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HotkeyError {
    #[error("empty key")]
    Empty,
    #[error("empty key token")]
    EmptyKey,
    #[error("empty modifier token")]
    EmptyModifier,
    #[error("unsupported modifier token: {0}")]
    UnsupportedModifier(String),
    #[error("duplicate modifier token: {0}")]
    DuplicateModifier(String),
    #[error("unsupported key token: {0}")]
    UnsupportedKey(String),
    #[error("invalid {name} {spec:?}: {reason}")]
    InvalidBinding {
        name: &'static str,
        spec: String,
        reason: String,
    },
    #[error("{name} {spec:?} duplicates {previous_name} {previous_spec:?}")]
    DuplicateBinding {
        name: &'static str,
        spec: String,
        previous_name: &'static str,
        previous_spec: String,
    },
    #[error("hotkeys are only available on Windows")]
    UnsupportedPlatform,
    #[error("{0}")]
    Registration(String),
}

pub fn validate_bindings(
    start: &str,
    pause: &str,
    cancel_or_retry: &str,
) -> Result<(), HotkeyError> {
    let bindings = [
        ("START_KEY", start),
        ("PAUSE_KEY", pause),
        ("CANCEL_OR_RETRY_KEY", cancel_or_retry),
    ];
    let mut seen: HashMap<ParsedHotkey, (&'static str, String)> = HashMap::new();
    for (name, spec) in bindings {
        let parsed = parse_hotkey(spec).map_err(|error| HotkeyError::InvalidBinding {
            name,
            spec: spec.into(),
            reason: error.to_string(),
        })?;
        if let Some((previous_name, previous_spec)) = seen.get(&parsed) {
            return Err(HotkeyError::DuplicateBinding {
                name,
                spec: spec.into(),
                previous_name,
                previous_spec: previous_spec.clone(),
            });
        }
        seen.insert(parsed, (name, spec.into()));
    }
    Ok(())
}

pub fn parse_hotkey(spec: &str) -> Result<ParsedHotkey, HotkeyError> {
    if spec.trim().is_empty() {
        return Err(HotkeyError::Empty);
    }
    let parts: Vec<String> = spec
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect();
    let key = parts.last().ok_or(HotkeyError::EmptyKey)?;
    if key.is_empty() {
        return Err(HotkeyError::EmptyKey);
    }

    let mut modifiers = 0;
    for token in &parts[..parts.len() - 1] {
        if token.is_empty() {
            return Err(HotkeyError::EmptyModifier);
        }
        let value = match token.as_str() {
            "alt" | "menu" => MOD_ALT,
            "ctrl" | "control" => MOD_CTRL,
            "shift" => MOD_SHIFT,
            "win" | "meta" | "super" => MOD_WIN,
            _ => return Err(HotkeyError::UnsupportedModifier(token.clone())),
        };
        if modifiers & value != 0 {
            return Err(HotkeyError::DuplicateModifier(token.clone()));
        }
        modifiers |= value;
    }

    let virtual_key = key_to_virtual_key(key)?;
    Ok(ParsedHotkey {
        modifiers,
        virtual_key,
    })
}

/// Existing audio bindings retain their matching semantics. New rewrite
/// bindings must not introduce any ambiguous hook match for the same key.
pub fn validate_config(config: &crate::Config) -> Result<(), HotkeyError> {
    validate_bindings(
        &config.start_key,
        &config.pause_key,
        &config.cancel_or_retry_key,
    )?;
    let mut bindings = vec![
        ("Start".to_owned(), parse_hotkey(&config.start_key)?),
        ("Pause".to_owned(), parse_hotkey(&config.pause_key)?),
        (
            "Cancel / Retry".to_owned(),
            parse_hotkey(&config.cancel_or_retry_key)?,
        ),
    ];
    for prompt in &config.rewrite.prompts {
        let parsed = parse_hotkey(&prompt.hotkey)?;
        for (name, previous) in &bindings {
            if *previous == parsed
                || (config.hotkey_hook && previous.virtual_key == parsed.virtual_key)
            {
                return Err(HotkeyError::Registration(format!(
                    "Hotkey conflict: {} / {name}",
                    prompt.title
                )));
            }
        }
        bindings.push((prompt.title.clone(), parsed));
    }
    // RegisterHotKey consumes even injected Ctrl+C, which Rewrite uses to copy
    // the selection. The low-level hook ignores injected input instead.
    if !config.hotkey_hook
        && !config.rewrite.prompts.is_empty()
        && let Some((name, _)) = bindings.iter().find(|(_, binding)| {
            binding.modifiers == MOD_CTRL && binding.virtual_key == u32::from(b'C')
        })
    {
        return Err(HotkeyError::Registration(format!(
            "Hotkey {name} uses Ctrl+C, which is reserved for copying Rewrite input in RegisterHotKey mode; choose another hotkey or enable hook mode"
        )));
    }
    Ok(())
}

fn key_to_virtual_key(key: &str) -> Result<u32, HotkeyError> {
    if let Some(value) = key
        .strip_prefix("vk_")
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .filter(|value| (1..=254).contains(value))
    {
        return Ok(value);
    }
    if let Some(value) = capture::named_key(key) {
        return Ok(value);
    }
    let bytes = key.as_bytes();
    if bytes.len() == 1 {
        let byte = bytes[0];
        if byte.is_ascii_lowercase() {
            return Ok(u32::from(byte.to_ascii_uppercase()));
        }
        return Ok(u32::from(byte.to_ascii_uppercase()));
    }
    match key {
        "esc" | "escape" => return Ok(0x1B),
        "space" => return Ok(0x20),
        "enter" | "return" => return Ok(0x0D),
        _ => {}
    }
    if let Some(number) = key
        .strip_prefix('f')
        .and_then(|number| number.parse::<u32>().ok())
        && (1..=24).contains(&number)
    {
        return Ok(0x70 + number - 1);
    }
    let value = match key {
        "numpad0" | "num0" | "kp0" => 0x60,
        "numpad1" | "num1" | "kp1" => 0x61,
        "numpad2" | "num2" | "kp2" => 0x62,
        "numpad3" | "num3" | "kp3" => 0x63,
        "numpad4" | "num4" | "kp4" => 0x64,
        "numpad5" | "num5" | "kp5" => 0x65,
        "numpad6" | "num6" | "kp6" => 0x66,
        "numpad7" | "num7" | "kp7" => 0x67,
        "numpad8" | "num8" | "kp8" => 0x68,
        "numpad9" | "num9" | "kp9" => 0x69,
        "add" | "plus" | "kpadd" => 0x6B,
        "subtract" | "minus" | "kpsubtract" => 0x6D,
        "tab" => 0x09,
        "backspace" => 0x08,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        _ => return Err(HotkeyError::UnsupportedKey(key.into())),
    };
    Ok(value)
}

pub struct HotkeyRegistration {
    stop: Option<Box<dyn FnOnce(bool) + Send + 'static>>,
}

impl fmt::Debug for HotkeyRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HotkeyRegistration")
            .finish_non_exhaustive()
    }
}

impl HotkeyRegistration {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn new(stop: impl FnOnce(bool) + Send + 'static) -> Self {
        Self {
            stop: Some(Box::new(stop)),
        }
    }

    pub fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            stop(false);
        }
    }

    pub fn stop_and_wait(mut self) {
        if let Some(stop) = self.stop.take() {
            stop(true);
        }
    }
}

impl Drop for HotkeyRegistration {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop(false);
        }
    }
}

#[cfg(windows)]
mod windows_impl;

#[cfg(windows)]
pub use windows_impl::register;

#[cfg(not(windows))]
pub fn register(
    _start: &str,
    _pause: &str,
    _cancel_or_retry: &str,
    _prompts: &[crate::rewrite::RewritePrompt],
    _hook: bool,
    _handler: impl Fn(i32) + Send + Sync + 'static,
    _debug: bool,
) -> Result<HotkeyRegistration, HotkeyError> {
    Err(HotkeyError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_and_duplicate_modifiers() {
        assert!(matches!(
            parse_hotkey("textctrrl+q"),
            Err(HotkeyError::UnsupportedModifier(_))
        ));
        assert!(matches!(
            parse_hotkey("ctrl++q"),
            Err(HotkeyError::EmptyModifier)
        ));
        assert!(matches!(
            parse_hotkey("ctrl+control+q"),
            Err(HotkeyError::DuplicateModifier(_))
        ));
    }

    #[test]
    fn normalizes_aliases_and_rejects_equivalent_bindings() {
        let error = validate_bindings("ctrl+alt+q", "ALT + CONTROL + Q", "alt+esc")
            .unwrap_err()
            .to_string();
        assert!(error.contains("PAUSE_KEY"));
        assert!(error.contains("duplicates START_KEY"));
        validate_bindings("control+alt+Q", "shift+F12", "menu+escape").unwrap();
    }

    #[test]
    fn supports_key_aliases() {
        assert_eq!(parse_hotkey("kp1").unwrap().virtual_key, 0x61);
        assert_eq!(parse_hotkey("super+return").unwrap().modifiers, MOD_WIN);
        assert_eq!(parse_hotkey("F24").unwrap().virtual_key, 0x87);
    }

    #[test]
    fn rewrite_conflicts_cover_aliases_other_prompts_and_hook_modifier_overlap() {
        let mut config = crate::Config::default();
        config.rewrite.prompts.push(crate::rewrite::RewritePrompt {
            title: "One".into(),
            hotkey: "control+alt+Q".into(),
            ..Default::default()
        });
        for hook in [false, true] {
            config.hotkey_hook = hook;
            assert!(validate_config(&config).is_err());
        }
        config.rewrite.prompts[0].hotkey = "ctrl+shift+q".into();
        config.hotkey_hook = false;
        validate_config(&config).unwrap();
        config.hotkey_hook = true;
        assert!(validate_config(&config).is_err());
        config.rewrite.prompts[0].hotkey = "ctrl+alt+w".into();
        validate_config(&config).unwrap();
        config.rewrite.prompts.push(crate::rewrite::RewritePrompt {
            title: "Two".into(),
            hotkey: "alt+ctrl+w".into(),
            ..Default::default()
        });
        let error = validate_config(&config).unwrap_err().to_string();
        assert!(error.contains("One") && error.contains("Two"));
    }

    #[test]
    fn rewrite_reserves_copy_hotkey_only_in_register_hotkey_mode() {
        for spec in ["ctrl+c", " CONTROL + C ", "ctrl+vk_43", "Control + VK_0043"] {
            for binding in ["Start", "Pause", "Cancel / Retry", "Rewrite prompt"] {
                let mut config = crate::Config {
                    hotkey_hook: false,
                    ..Default::default()
                };
                config.rewrite.prompts.push(crate::rewrite::RewritePrompt {
                    title: "Rewrite prompt".into(),
                    hotkey: "ctrl+alt+w".into(),
                    ..Default::default()
                });
                match binding {
                    "Start" => config.start_key = spec.into(),
                    "Pause" => config.pause_key = spec.into(),
                    "Cancel / Retry" => config.cancel_or_retry_key = spec.into(),
                    _ => config.rewrite.prompts[0].hotkey = spec.into(),
                }

                let error = validate_config(&config).unwrap_err().to_string();
                assert!(error.contains(binding), "{error}");
                assert!(error.contains("Ctrl+C"), "{error}");
                assert!(
                    error.contains("reserved for copying Rewrite input"),
                    "{error}"
                );
                assert!(error.contains("RegisterHotKey mode"), "{error}");
                assert!(
                    error.contains("choose another hotkey or enable hook mode"),
                    "{error}"
                );

                config.hotkey_hook = true;
                validate_config(&config).unwrap();

                config.hotkey_hook = false;
                config.rewrite.prompts.clear();
                validate_config(&config).unwrap();
            }
        }
    }

    #[test]
    fn rewrite_copy_reservation_allows_other_modifier_combinations() {
        let mut config = crate::Config {
            hotkey_hook: false,
            ..Default::default()
        };
        config.rewrite.prompts.push(crate::rewrite::RewritePrompt {
            title: "Rewrite prompt".into(),
            ..Default::default()
        });
        for spec in ["c", "alt+c", "ctrl+alt+c", "ctrl+shift+c", "win+ctrl+c"] {
            config.rewrite.prompts[0].hotkey = spec.into();
            validate_config(&config).unwrap();
        }
    }
}
