use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::hotkey;

pub const DEFAULT_VAD_START_THRESHOLD: f64 = 0.6;

pub fn validate_vad_start_threshold(value: f64) -> Result<(), ConfigError> {
    if !(0.5..=1.0).contains(&value) {
        return Err(ConfigError::Invalid(
            "invalid VAD_START_THRESHOLD (allowed 0.5..=1.0)".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(non_snake_case)]
pub struct Config {
    #[serde(rename = "REWRITE")]
    pub rewrite: crate::rewrite::RewriteConfig,
    #[serde(rename = "API_ENDPOINT")]
    pub api_endpoint: String,
    #[serde(rename = "TOKEN")]
    pub token: String,
    #[serde(rename = "MODEL")]
    pub model: String,
    #[serde(rename = "LANGUAGE")]
    pub language: String,
    #[serde(rename = "PROMPT")]
    pub prompt: String,
    #[serde(rename = "TEXT_PATH")]
    pub text_path: String,
    #[serde(rename = "ExtraConfig")]
    pub extra_config: String,
    #[serde(rename = "OPACITY")]
    pub opacity: f64,
    #[serde(rename = "WINDOW_SCALE")]
    pub window_scale: f64,
    /// Empty means follow the system default input endpoint at recording start.
    #[serde(rename = "INPUT_DEVICE")]
    pub input_device: String,
    /// Display-only cache; never used to resolve an endpoint.
    #[serde(rename = "INPUT_DEVICE_NAME")]
    pub input_device_name: String,
    #[serde(rename = "CHANNELS")]
    pub channels: i32,
    #[serde(rename = "SAMPLING_RATE")]
    pub sampling_rate: i32,
    #[serde(rename = "ENABLE_VAD")]
    pub enable_vad: bool,
    #[serde(rename = "VAD_PADDING_MS")]
    pub vad_padding_ms: u32,
    #[serde(rename = "VAD_START_THRESHOLD")]
    pub vad_start_threshold: f64,
    #[serde(rename = "SAMPLING_RATE_DEPTH")]
    pub sampling_rate_depth: i32,
    #[serde(rename = "BIT_RATE")]
    pub bit_rate: i32,
    #[serde(rename = "CODECS")]
    pub codecs: String,
    #[serde(rename = "CONTAINER")]
    pub container: String,
    #[serde(rename = "REQUEST_TIMEOUT")]
    pub request_timeout: i32,
    #[serde(rename = "MAX_RETRY")]
    pub max_retry: i32,
    #[serde(rename = "RETRY_BASE_DELAY")]
    pub retry_base_delay: f64,
    #[serde(rename = "ENABLE_HTTP2")]
    pub enable_http2: bool,
    #[serde(rename = "VERIFY_SSL")]
    pub verify_ssl: bool,
    #[serde(rename = "HOTKEY_HOOK")]
    pub hotkey_hook: bool,
    #[serde(rename = "USE_SENDINPUT")]
    pub use_sendinput: bool,
    #[serde(rename = "START_KEY")]
    pub start_key: String,
    #[serde(rename = "PAUSE_KEY")]
    pub pause_key: String,
    #[serde(rename = "CANCEL_OR_RETRY_KEY")]
    pub cancel_or_retry_key: String,
    #[serde(rename = "CLIPBOARD_WRITE_DELAY")]
    pub clipboard_write_delay: u64,
    #[serde(rename = "CLIPBOARD_RESTORE_DELAY")]
    pub clipboard_restore_delay: u64,
    #[serde(rename = "CACHE_DIR")]
    pub cache_dir: String,
    #[serde(rename = "KEEP_CACHE")]
    pub keep_cache: bool,
    #[serde(rename = "REQUEST_FAILED_NOTIFICATION")]
    pub request_failed_notification: bool,
    #[serde(rename = "FFMPEG_DEBUG")]
    pub ffmpeg_debug: bool,
    #[serde(rename = "RECORD_DEBUG")]
    pub record_debug: bool,
    #[serde(rename = "HOTKEY_DEBUG")]
    pub hotkey_debug: bool,
    #[serde(rename = "UPLOAD_DEBUG")]
    pub upload_debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            rewrite: crate::rewrite::RewriteConfig::default(),
            api_endpoint: String::new(),
            token: String::new(),
            model: String::new(),
            language: String::new(),
            prompt: String::new(),
            text_path: "$.text".into(),
            extra_config: String::new(),
            opacity: 1.0,
            window_scale: 1.0,
            input_device: String::new(),
            input_device_name: String::new(),
            channels: 1,
            sampling_rate: 16_000,
            enable_vad: false,
            vad_padding_ms: 100,
            vad_start_threshold: DEFAULT_VAD_START_THRESHOLD,
            sampling_rate_depth: 16,
            bit_rate: 128,
            codecs: "opus".into(),
            container: "opus".into(),
            request_timeout: 60,
            max_retry: 3,
            retry_base_delay: 0.5,
            enable_http2: true,
            verify_ssl: true,
            hotkey_hook: true,
            use_sendinput: false,
            start_key: "ctrl+alt+q".into(),
            pause_key: "ctrl+alt+s".into(),
            cancel_or_retry_key: "alt+esc".into(),
            clipboard_write_delay: 80,
            clipboard_restore_delay: 120,
            cache_dir: String::new(),
            keep_cache: false,
            request_failed_notification: false,
            ffmpeg_debug: false,
            record_debug: false,
            hotkey_debug: true,
            upload_debug: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config '{path}': {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config '{path}': {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write config '{path}': {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        // Persist a complete sibling file atomically so a failed save cannot
        // truncate the last working configuration.
        let write = || -> Result<(), std::io::Error> {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist(path).map_err(|error| error.error)?;
            Ok(())
        };
        write().map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.rewrite.validate().map_err(ConfigError::Invalid)?;
        crate::additional_parameters::parse(&self.extra_config)
            .map_err(|error| ConfigError::Invalid(format!("Extra config: {error}")))?;
        validate_vad_start_threshold(self.vad_start_threshold)?;
        crate::jsonpath::parse_text_path(&self.text_path)
            .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        if self.vad_padding_ms > 1000 {
            return Err(ConfigError::Invalid(
                "invalid VAD_PADDING_MS (allowed 0..=1000 ms)".into(),
            ));
        }
        if !self.opacity.is_finite()
            || !(0.1..=1.0).contains(&self.opacity)
            || !is_opacity_step_aligned(self.opacity)
        {
            return Err(ConfigError::Invalid(format!(
                "invalid OPACITY: {} (allowed 0.10..1.00 in steps of 0.01)",
                self.opacity
            )));
        }
        if !self.window_scale.is_finite()
            || !(0.3..=2.0).contains(&self.window_scale)
            || !is_tenth_step_aligned(self.window_scale)
        {
            return Err(ConfigError::Invalid(format!(
                "invalid WINDOW_SCALE: {} (allowed 0.3..2.0 in steps of 0.1)",
                self.window_scale
            )));
        }
        if !(1..=8).contains(&self.channels) {
            return Err(ConfigError::Invalid(format!(
                "invalid Channels: {} (allowed 1..8)",
                self.channels
            )));
        }
        if self.sampling_rate <= 0 {
            return Err(ConfigError::Invalid(format!(
                "invalid SAMPLING_RATE: {} (must be > 0)",
                self.sampling_rate
            )));
        }
        if ![8, 16, 24, 32].contains(&self.sampling_rate_depth) {
            return Err(ConfigError::Invalid(format!(
                "invalid SAMPLING_RATE_DEPTH: {} (allowed: 8,16,24,32)",
                self.sampling_rate_depth
            )));
        }
        if self.bit_rate <= 0 {
            return Err(ConfigError::Invalid(format!(
                "invalid BIT_RATE: {} (must be > 0)",
                self.bit_rate
            )));
        }
        hotkey::validate_config(self).map_err(|error| ConfigError::Invalid(error.to_string()))?;

        let codecs: HashSet<&str> = [
            "opus",
            "libopus",
            "wavpack",
            "aac",
            "ac3",
            "eac3",
            "mp3",
            "mp2",
            "mp1",
            "flac",
            "alac",
            "pcm",
            "vorbis",
            "libvorbis",
            "vorb",
            "adpcm",
            "amr",
            "amr_wb",
            "libvo_amrwbenc",
            "libopencore_amrnb",
            "speex",
            "libspeex",
            "wmav1",
            "wmav2",
            "pcm_alaw",
            "pcm_mulaw",
            "pcm_f32be",
            "pcm_f32le",
            "pcm_f64be",
            "pcm_f64le",
            "pcm_s16be",
            "pcm_s16le",
            "pcm_s24be",
            "pcm_s24le",
            "pcm_s32be",
            "pcm_s32le",
            "pcm_s64be",
            "pcm_s64le",
            "pcm_s8",
        ]
        .into_iter()
        .collect();
        if !codecs.contains(self.codecs.to_ascii_lowercase().as_str()) {
            return Err(ConfigError::Invalid(format!(
                "invalid CODECS: {}",
                self.codecs
            )));
        }

        let containers: HashSet<&str> = [
            "wav", "ac3", "ac4", "ogg", "oga", "mp3", "flac", "eac3", "aac", "m4a", "mp4", "opus",
            "webm", "s8", "s16be", "s16le", "s24be", "s24le", "s32be", "s32le", "f32be", "f32le",
            "f64be", "f64le", "spx", "amr", "avi", "flv", "mkv", "mka", "mov", "mpg", "mpeg",
            "asf", "wma", "aif", "aiff", "afc", "aifc", "wv", "alaw", "mulaw",
        ]
        .into_iter()
        .collect();
        if !containers.contains(self.container.to_ascii_lowercase().as_str()) {
            return Err(ConfigError::Invalid(format!(
                "invalid CONTAINER: {}",
                self.container
            )));
        }
        Ok(())
    }

    pub fn container_extension(&self) -> String {
        container_extension(&self.container)
    }
}

fn is_opacity_step_aligned(opacity: f64) -> bool {
    let hundredths = opacity * 100.0;
    (hundredths - hundredths.round()).abs() < 1e-9
}

fn is_tenth_step_aligned(value: f64) -> bool {
    let tenths = value * 10.0;
    (tenths - tenths.round()).abs() < 1e-9
}

pub fn container_extension(container: &str) -> String {
    let lower = container.to_ascii_lowercase();
    if lower.is_empty() {
        "ogg".into()
    } else {
        lower
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn microphone_config_is_backward_compatible_and_keeps_offline_selection() {
        let old: super::Config = serde_json::from_str(r#"{"CHANNELS":2}"#).unwrap();
        assert!(old.input_device.is_empty());
        assert!(old.input_device_name.is_empty());
        let config = super::Config {
            input_device: "offline-endpoint-id".into(),
            input_device_name: "USB microphone".into(),
            ..old
        };
        config.validate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        config.save(&path).unwrap();
        assert_eq!(super::Config::load(path).unwrap(), config);
    }
    #[test]
    fn vad_defaults_roundtrip_and_bounds() {
        let old: super::Config = serde_json::from_str("{}").unwrap();
        assert!(!old.enable_vad);
        assert_eq!(old.vad_padding_ms, 100);
        assert_eq!(old.vad_start_threshold, 0.6);
        for threshold in [0.5, 0.6, 1.0] {
            let config: super::Config = serde_json::from_value(serde_json::json!({
                "VAD_START_THRESHOLD": threshold
            }))
            .unwrap();
            config.validate().unwrap();
            assert_eq!(
                serde_json::from_str::<super::Config>(&serde_json::to_string(&config).unwrap())
                    .unwrap(),
                config
            );
        }
        for threshold in [0.499, 1.001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let config = super::Config {
                vad_start_threshold: threshold,
                ..Default::default()
            };
            assert!(
                config
                    .validate()
                    .unwrap_err()
                    .to_string()
                    .contains("VAD_START_THRESHOLD")
            );
        }
        for padding in [0, 1000] {
            let config: super::Config = serde_json::from_value(serde_json::json!({
                "ENABLE_VAD": true, "VAD_PADDING_MS": padding
            }))
            .unwrap();
            config.validate().unwrap();
            assert_eq!(
                serde_json::to_value(config).unwrap()["VAD_PADDING_MS"],
                padding
            );
        }
        assert!(serde_json::from_str::<super::Config>(r#"{"VAD_PADDING_MS":-1}"#).is_err());
        let config: super::Config = serde_json::from_str(r#"{"VAD_PADDING_MS":1001}"#).unwrap();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("VAD_PADDING_MS")
        );
    }
    use super::*;

    #[test]
    fn old_notification_is_ignored_and_not_written_back() {
        let input = r#"{"NOTIFICATION":true,"MODEL":"whisper"}"#;
        let cfg: Config = serde_json::from_str(input).unwrap();
        assert_eq!(cfg.model, "whisper");
        let output: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert!(!output.contains_key("NOTIFICATION"));
        assert!(output.contains_key("REQUEST_FAILED_NOTIFICATION"));
    }

    #[test]
    fn missing_and_unknown_fields_are_compatible() {
        let cfg: Config = serde_json::from_str(r#"{"UNKNOWN":1,"CHANNELS":2}"#).unwrap();
        assert_eq!(cfg.channels, 2);
        assert_eq!(cfg.text_path, "$.text");
        assert_eq!(cfg.codecs, "opus");
        assert_eq!(cfg.clipboard_write_delay, 80);
        assert_eq!(cfg.clipboard_restore_delay, 120);
        assert!(!cfg.use_sendinput);
        let enabled: Config = serde_json::from_str(r#"{"USE_SENDINPUT":true}"#).unwrap();
        assert!(enabled.use_sendinput);
        assert_eq!(
            serde_json::to_value(enabled).unwrap()["USE_SENDINPUT"],
            true
        );
        assert_eq!(cfg.opacity, 1.0);
        assert_eq!(cfg.window_scale, 1.0);
    }

    #[test]
    fn validates_equivalent_hotkeys() {
        let cfg = Config {
            pause_key: "ALT + CONTROL + Q".into(),
            ..Config::default()
        };
        assert!(
            cfg.validate()
                .unwrap_err()
                .to_string()
                .contains("duplicates START_KEY")
        );
    }

    #[test]
    fn validates_audio_ranges_and_case_insensitive_formats() {
        let mut cfg = Config {
            codecs: "MP3".into(),
            container: "M4A".into(),
            ..Config::default()
        };
        cfg.validate().unwrap();
        cfg.channels = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validates_opacity_range_and_step() {
        let mut cfg = Config {
            opacity: 0.1,
            ..Config::default()
        };
        cfg.validate().unwrap();
        cfg.opacity = 0.67;
        cfg.validate().unwrap();
        cfg.opacity = 0.09;
        assert!(cfg.validate().unwrap_err().to_string().contains("OPACITY"));
        cfg.opacity = 1.01;
        assert!(cfg.validate().unwrap_err().to_string().contains("OPACITY"));
        cfg.opacity = 0.105;
        assert!(cfg.validate().unwrap_err().to_string().contains("OPACITY"));
    }

    #[test]
    fn validates_window_scale_range_and_step() {
        let mut cfg = Config {
            window_scale: 0.3,
            ..Config::default()
        };
        cfg.validate().unwrap();
        cfg.window_scale = 2.0;
        cfg.validate().unwrap();
        cfg.window_scale = 0.2;
        assert!(
            cfg.validate()
                .unwrap_err()
                .to_string()
                .contains("WINDOW_SCALE")
        );
        cfg.window_scale = 2.1;
        assert!(
            cfg.validate()
                .unwrap_err()
                .to_string()
                .contains("WINDOW_SCALE")
        );
        cfg.window_scale = 1.05;
        assert!(
            cfg.validate()
                .unwrap_err()
                .to_string()
                .contains("WINDOW_SCALE")
        );
    }

    #[test]
    fn round_trip_default_has_no_removed_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        Config::default().save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        let raw: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert!(!raw.contains_key("NOTIFICATION"));
        assert!(!raw.contains_key("CANCEL_KEY"));
        assert!(raw.contains_key("CANCEL_OR_RETRY_KEY"));
        assert_eq!(raw.get("OPACITY"), Some(&serde_json::json!(1.0)));
        assert_eq!(raw.get("WINDOW_SCALE"), Some(&serde_json::json!(1.0)));
    }

    #[test]
    fn legacy_config_and_rewrite_roundtrip_preserve_defaults_and_stable_prompt_ids() {
        let old: Config =
            serde_json::from_str(r#"{"MODEL":"audio-model","START_KEY":"ctrl+alt+q"}"#).unwrap();
        old.validate().unwrap();
        assert_eq!(old.rewrite, crate::rewrite::RewriteConfig::default());
        let mut config = old;
        config.rewrite.prompts.push(crate::rewrite::RewritePrompt {
            title: "Polish".into(),
            prompt: "Polish the text".into(),
            hotkey: "ctrl+alt+w".into(),
            extra_config: r#"{"nested":{"array":[null,{"keep":true}]}}"#.into(),
            ..Default::default()
        });
        config.validate().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        config
            .rewrite
            .prompts
            .push(config.rewrite.prompts[0].clone());
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("Duplicate prompt ID")
        );
        config.rewrite.prompts.pop();
        config.rewrite.prompts[0].extra_config = "[]".into();
        assert!(config.validate().is_err());
    }
}
