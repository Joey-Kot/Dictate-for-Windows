//! Shared, platform-independent validation for both settings save paths.
use dictate_core::Config;
use dictate_core::hotkey::parse_hotkey;

use crate::i18n::Language;

#[cfg(windows)]
pub const KEYS: [&str; 3] = ["START_KEY", "PAUSE_KEY", "CANCEL_OR_RETRY_KEY"];

/// Use the same conflict rules in both draft editors and their final save.
pub fn validate_draft(config: &Config, language: Language) -> Result<(), String> {
    let mut bindings = vec![
        (
            language.text("START_KEY").to_owned(),
            config.start_key.as_str(),
        ),
        (
            language.text("PAUSE_KEY").to_owned(),
            config.pause_key.as_str(),
        ),
        (
            language.text("CANCEL_OR_RETRY_KEY").to_owned(),
            config.cancel_or_retry_key.as_str(),
        ),
    ];
    if config.rewrite.menu_enabled && !config.rewrite.menu_key.trim().is_empty() {
        bindings.push((
            language.text("rewrite_menu_key").to_owned(),
            &config.rewrite.menu_key,
        ));
    }
    bindings.extend(
        config
            .rewrite
            .prompts
            .iter()
            .filter(|prompt| !prompt.hotkey.trim().is_empty())
            .map(|prompt| (prompt.title.clone(), prompt.hotkey.as_str())),
    );
    for (index, (name, spec)) in bindings.iter().enumerate() {
        let Ok(current) = parse_hotkey(spec) else {
            continue;
        };
        for (previous_name, previous_spec) in &bindings[..index] {
            if let Ok(previous) = parse_hotkey(previous_spec)
                && current == previous
            {
                return Err(format!(
                    "{}: {name} / {previous_name}",
                    language.text("hotkey_duplicate")
                ));
            }
        }
    }
    dictate_core::hotkey::validate_config_draft(config).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_core::rewrite::RewritePrompt;

    fn shared_key_config() -> Config {
        let mut config = Config {
            start_key: "ctrl+shift+q".into(),
            pause_key: "ctrl+alt+q".into(),
            cancel_or_retry_key: "shift+alt+q".into(),
            ..Config::default()
        };
        config.rewrite.menu_enabled = true;
        config.rewrite.menu_key = "ctrl+shift+alt+q".into();
        for hotkey in ["ctrl+q", "shift+q", "alt+q", "q", "win+q", ""] {
            config.rewrite.prompts.push(RewritePrompt {
                title: format!("Prompt {hotkey}"),
                prompt: "Rewrite".into(),
                hotkey: hotkey.into(),
                ..Default::default()
            });
        }
        config
    }

    #[test]
    fn both_save_paths_allow_distinct_audio_prompt_and_menu_chords() {
        for hook in [false, true] {
            for language in Language::ALL {
                let mut config = shared_key_config();
                config.hotkey_hook = hook;
                // Main settings and Edit prompt use this same entry point.
                validate_draft(&config, language).unwrap();
                config.validate().unwrap();
                // Draft editing still permits an enabled menu awaiting its key.
                config.rewrite.menu_key.clear();
                validate_draft(&config, language).unwrap();
                // A disabled menu does not reserve the stored combination.
                config.rewrite.menu_enabled = false;
                config.rewrite.menu_key = "ctrl+q".into();
                validate_draft(&config, language).unwrap();
            }
        }
    }

    #[test]
    fn both_modes_reject_normalized_duplicates_across_all_action_kinds() {
        for hook in [false, true] {
            for duplicate in 0..6 {
                let mut config = shared_key_config();
                config.hotkey_hook = hook;
                match duplicate {
                    0 => config.pause_key = "SHIFT + CONTROL + Q".into(),
                    1 => config.rewrite.prompts[1].hotkey = "CONTROL + Q".into(),
                    2 => config.rewrite.prompts[0].hotkey = "SHIFT + CONTROL + Q".into(),
                    3 => config.rewrite.menu_key = "CONTROL + Q".into(),
                    4 => config.rewrite.menu_key = "SHIFT + CONTROL + Q".into(),
                    _ => config.start_key = "CONTROL + Q".into(),
                }
                assert!(validate_draft(&config, Language::English).is_err());
            }
        }
    }
}
