use std::collections::HashMap;
use std::fmt;

use thiserror::Error;

const MOD_ALT: u32 = 0x0001;
const MOD_CTRL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;

pub const REWRITE_MENU_ACTION: i32 = 4;
pub const REWRITE_PROMPT_ACTION_BASE: i32 = 1000;

pub mod capture;

#[cfg(any(windows, test))]
mod hook_state;

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

/// Both hotkey backends match the complete modifier combination. Audio,
/// menu and prompt bindings conflict only when their normalized chords match.
pub fn validate_config(config: &crate::Config) -> Result<(), HotkeyError> {
    if config.rewrite.menu_enabled {
        parse_hotkey(&config.rewrite.menu_key).map_err(|error| HotkeyError::InvalidBinding {
            name: "Menu key",
            spec: config.rewrite.menu_key.clone(),
            reason: error.to_string(),
        })?;
    }
    validate_config_draft(config)
}

/// Validate existing bindings while a prompt editor updates the settings
/// draft. Enabling the menu before recording its key must not block editing.
pub fn validate_config_draft(config: &crate::Config) -> Result<(), HotkeyError> {
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
    let menu_binding = (config.rewrite.menu_enabled && !config.rewrite.menu_key.trim().is_empty())
        .then_some(("Menu key", config.rewrite.menu_key.as_str()));
    let prompt_bindings = config.rewrite.prompts.iter().filter_map(|prompt| {
        (!prompt.hotkey.trim().is_empty())
            .then_some((prompt.title.as_str(), prompt.hotkey.as_str()))
    });
    for (title, spec) in menu_binding.into_iter().chain(prompt_bindings) {
        let parsed = parse_hotkey(spec)?;
        for (name, previous) in &bindings {
            if *previous == parsed {
                return Err(HotkeyError::Registration(format!(
                    "Hotkey conflict: {} / {name}",
                    title
                )));
            }
        }
        bindings.push((title.to_owned(), parsed));
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

/// Build both Windows backends from the same sparse action table. Prompt
/// numbers use their original list index; skipping empty keys never shifts
/// the stable prompt-ID lookup captured by the runtime.
#[cfg_attr(not(windows), allow(dead_code))]
fn binding_definitions(
    config: &crate::Config,
    menu_available: bool,
) -> Result<Vec<(i32, String, ParsedHotkey)>, HotkeyError> {
    validate_config(config)?;
    let mut definitions = vec![
        (
            1,
            config.start_key.clone(),
            parse_hotkey(&config.start_key)?,
        ),
        (
            2,
            config.pause_key.clone(),
            parse_hotkey(&config.pause_key)?,
        ),
        (
            3,
            config.cancel_or_retry_key.clone(),
            parse_hotkey(&config.cancel_or_retry_key)?,
        ),
    ];
    if menu_available && config.rewrite.menu_enabled {
        definitions.push((
            REWRITE_MENU_ACTION,
            config.rewrite.menu_key.clone(),
            parse_hotkey(&config.rewrite.menu_key)?,
        ));
    }
    for (index, prompt) in config.rewrite.prompts.iter().enumerate() {
        if prompt.hotkey.trim().is_empty() {
            continue;
        }
        let id = i32::try_from(index)
            .ok()
            .and_then(|index| REWRITE_PROMPT_ACTION_BASE.checked_add(index))
            .ok_or_else(|| HotkeyError::Registration("Too many prompt hotkeys".into()))?;
        definitions.push((id, prompt.hotkey.clone(), parse_hotkey(&prompt.hotkey)?));
    }
    Ok(definitions)
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
    _config: &crate::Config,
    _menu_available: bool,
    _handler: impl Fn(i32) + Send + Sync + 'static,
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
    fn shared_key_dispatch_preserves_audio_menu_and_prompt_action_ids() {
        // Numeric order follows MOD_ALT, MOD_CTRL, MOD_SHIFT and MOD_WIN.
        let chords = [
            "q",
            "alt+q",
            "ctrl+q",
            "ctrl+alt+q",
            "shift+q",
            "alt+shift+q",
            "ctrl+shift+q",
            "ctrl+alt+shift+q",
            "win+q",
            "win+alt+q",
            "win+ctrl+q",
            "win+ctrl+alt+q",
            "win+shift+q",
            "win+alt+shift+q",
            "win+ctrl+shift+q",
            "win+ctrl+alt+shift+q",
        ];
        let mut config = crate::Config {
            start_key: chords[0].into(),
            pause_key: chords[1].into(),
            cancel_or_retry_key: chords[2].into(),
            ..Default::default()
        };
        config.rewrite.menu_enabled = true;
        config.rewrite.menu_key = chords[3].into();
        config.rewrite.prompts = chords[4..]
            .iter()
            .enumerate()
            .map(|(index, chord)| crate::rewrite::RewritePrompt {
                id: format!("prompt-{index}"),
                title: format!("Prompt {index}"),
                prompt: "Rewrite".into(),
                hotkey: (*chord).into(),
                ..Default::default()
            })
            .collect();
        for hook in [false, true] {
            config.hotkey_hook = hook;
            config.validate().unwrap();
            let definitions = binding_definitions(&config, true).unwrap();
            assert_eq!(definitions.len(), 16);
            let candidates: Vec<_> = definitions
                .iter()
                .map(|(id, _, binding)| {
                    assert_eq!(binding.virtual_key, u32::from(b'Q'));
                    (*id, binding.modifiers)
                })
                .collect();
            for modifiers in 0..16 {
                let id = match modifiers {
                    0..=2 => modifiers as i32 + 1,
                    3 => REWRITE_MENU_ACTION,
                    _ => REWRITE_PROMPT_ACTION_BASE + modifiers as i32 - 4,
                };
                assert_eq!(
                    hook_state::matching_action(&candidates, modifiers),
                    Some(id)
                );
            }
        }
    }

    #[test]
    fn rewrite_conflicts_cover_aliases_and_allow_distinct_modifier_combinations() {
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
        for hook in [false, true] {
            config.hotkey_hook = hook;
            validate_config(&config).unwrap();
        }
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

    #[test]
    fn optional_prompt_keys_preserve_sparse_action_ids_after_reordering() {
        let mut config = crate::Config::default();
        for (id, key) in [("a", ""), ("b", "ctrl+alt+w"), ("c", "  "), ("d", "alt+t")] {
            config.rewrite.prompts.push(crate::rewrite::RewritePrompt {
                id: id.into(),
                title: id.into(),
                hotkey: key.into(),
                ..Default::default()
            });
        }
        for expected in [[("b", 1001), ("d", 1003)], [("d", 1000), ("b", 1001)]] {
            let definitions = binding_definitions(&config, true).unwrap();
            let actual: Vec<_> = definitions
                .iter()
                .filter(|(id, _, _)| *id >= REWRITE_PROMPT_ACTION_BASE)
                .map(|(id, _, _)| {
                    let prompt =
                        &config.rewrite.prompts[(*id - REWRITE_PROMPT_ACTION_BASE) as usize];
                    (prompt.id.as_str(), *id)
                })
                .collect();
            assert_eq!(actual, expected);
            config.rewrite.prompts.swap(0, 3);
        }
        config.rewrite.prompts.remove(0);
        let definitions = binding_definitions(&config, true).unwrap();
        assert_eq!(definitions.last().unwrap().0, 1002);
        assert_eq!(config.rewrite.prompts[2].id, "d");
    }

    #[test]
    fn menu_key_is_required_only_at_final_validation_and_registered_only_for_gui() {
        let mut config = crate::Config::default();
        config.rewrite.menu_enabled = true;
        validate_config_draft(&config).unwrap();
        assert!(
            validate_config(&config)
                .unwrap_err()
                .to_string()
                .contains("Menu key")
        );
        config.rewrite.menu_key = "ctrl+alt+m".into();
        validate_config(&config).unwrap();
        assert!(
            binding_definitions(&config, true)
                .unwrap()
                .iter()
                .any(|(id, _, _)| *id == REWRITE_MENU_ACTION)
        );
        assert!(
            !binding_definitions(&config, false)
                .unwrap()
                .iter()
                .any(|(id, _, _)| *id == REWRITE_MENU_ACTION)
        );

        config.rewrite.menu_enabled = false;
        for key in ["ctrl+alt+q", "invalid key", "ctrl+c"] {
            config.rewrite.menu_key = key.into();
            validate_config(&config).unwrap();
            assert_eq!(binding_definitions(&config, true).unwrap().len(), 3);
            assert_eq!(config.rewrite.menu_key, key);
        }
    }

    #[test]
    fn menu_conflicts_include_exact_audio_prompt_chords_and_copy_protection() {
        let mut config = crate::Config::default();
        config.rewrite.menu_enabled = true;
        for hook in [false, true] {
            config.hotkey_hook = hook;
            config.rewrite.menu_key = "control+alt+q".into();
            assert!(validate_config_draft(&config).is_err());
            config.rewrite.menu_key = "ctrl+alt+m".into();
            config.rewrite.prompts = vec![crate::rewrite::RewritePrompt {
                title: "Prompt".into(),
                hotkey: "alt+control+m".into(),
                ..Default::default()
            }];
            let error = validate_config(&config).unwrap_err().to_string();
            assert!(error.contains("Prompt") && error.contains("Menu key"));
            config.rewrite.prompts[0].hotkey.clear();
            config.rewrite.menu_key = "ctrl+shift+q".into();
            validate_config(&config).unwrap();
        }
        config.hotkey_hook = false;
        config.rewrite.menu_key = "ctrl+c".into();
        assert!(
            validate_config(&config)
                .unwrap_err()
                .to_string()
                .contains("reserved for copying")
        );
        config.hotkey_hook = true;
        validate_config(&config).unwrap();
    }
}
