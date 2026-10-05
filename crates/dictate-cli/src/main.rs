use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{ArgAction, Parser};
use dictate_core::Config;
use dictate_core::embedded_ffmpeg::EmbeddedFfmpegConverter;
use dictate_core::runtime::{Runtime, run_file_mode_with_cancellation};

#[derive(Debug, Parser)]
#[command(
    name = "dictate-cli",
    version,
    about = "Dictate for Windows: transcribe audio or rewrite selected text, then insert the result.",
    disable_help_flag = false
)]
struct Arguments {
    /// Path to a JSON configuration file.
    #[arg(long, value_name = "PATH", help_heading = "General")]
    config: Option<PathBuf>,

    /// Existing audio file to transcode and transcribe.
    #[arg(long, value_name = "PATH", help_heading = "General")]
    file: Option<PathBuf>,

    /// Output text path for --file mode.
    #[arg(long, value_name = "PATH", help_heading = "General")]
    output: Option<PathBuf>,

    /// ASR HTTP endpoint URL.
    #[arg(long, value_name = "URL", help_heading = "Audio API")]
    api_endpoint: Option<String>,
    /// Bearer token sent with the ASR request.
    #[arg(long, value_name = "TOKEN", help_heading = "Audio API")]
    token: Option<String>,
    /// Model multipart field.
    #[arg(long, value_name = "MODEL", help_heading = "Audio API")]
    model: Option<String>,
    /// Language multipart field.
    #[arg(long, value_name = "LANGUAGE", help_heading = "Audio API")]
    language: Option<String>,
    /// Prompt multipart field.
    #[arg(long, value_name = "TEXT", help_heading = "Audio API")]
    prompt: Option<String>,
    /// JSONPath selecting exactly one string, number or boolean (default: $.text).
    #[arg(long, value_name = "PATH", help_heading = "Audio API")]
    text_path: Option<String>,
    /// Stringified JSON object with extra multipart fields.
    #[arg(long, value_name = "JSON", help_heading = "Audio API")]
    extra_config: Option<String>,

    /// List active microphones and their stable IDs, then exit without loading config.
    #[arg(long, help_heading = "Audio Record")]
    list_input_devices: bool,
    /// Microphone endpoint ID, or "default" to follow the system default (this run only).
    #[arg(long, value_name = "ID|default", help_heading = "Audio Record")]
    input_device: Option<String>,

    /// Detect speech and trim original input (default false); no system FFmpeg required.
    #[arg(long, action = ArgAction::Set, value_name = "BOOL", help_heading = "Audio Record")]
    enable_vad: Option<bool>,
    /// Shared padding at internal cuts, in milliseconds (0-1000).
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=1000), help_heading = "Audio Record")]
    vad_padding_ms: Option<u32>,
    /// Speech start score threshold (0.5-1.0, default 0.6).
    #[arg(long, value_parser = parse_vad_start_threshold, help_heading = "Audio Record")]
    vad_start_threshold: Option<f64>,

    /// Audio encoder or compatible alias.
    #[arg(long, value_name = "CODEC", help_heading = "Audio Record")]
    codecs: Option<String>,
    /// Output audio container.
    #[arg(long, value_name = "FORMAT", help_heading = "Audio Record")]
    container: Option<String>,
    /// Final upload channel count; capture follows the microphone format.
    #[arg(long, value_name = "N", help_heading = "Audio Record")]
    channels: Option<i32>,
    /// Final upload sample rate in Hz; capture follows the microphone format.
    #[arg(long, alias = "rate", value_name = "HZ", help_heading = "Audio Record")]
    sampling_rate: Option<i32>,
    /// Conversion sample depth in bits.
    #[arg(long, value_name = "BITS", help_heading = "Audio Record")]
    sampling_rate_depth: Option<i32>,
    /// Audio bitrate in kbps.
    #[arg(long, value_name = "KBPS", help_heading = "Audio Record")]
    bit_rate: Option<i32>,

    /// Per-request client timeout in seconds.
    #[arg(long, value_name = "SECONDS", help_heading = "Network")]
    request_timeout: Option<i32>,
    /// Maximum request attempts, including the first request.
    #[arg(long, value_name = "N", help_heading = "Network")]
    max_retry: Option<i32>,
    /// Initial exponential-backoff delay in seconds.
    #[arg(long, value_name = "SECONDS", help_heading = "Network")]
    retry_base_delay: Option<f64>,
    /// Enable HTTP/2 negotiation.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Network"
    )]
    enable_http2: Option<bool>,
    /// Verify TLS certificates.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Network"
    )]
    verify_ssl: Option<bool>,

    /// Start/stop recording hotkey.
    #[arg(long, value_name = "HOTKEY", help_heading = "Audio Hotkeys")]
    start_key: Option<String>,
    /// Pause/resume recording hotkey.
    #[arg(long, value_name = "HOTKEY", help_heading = "Audio Hotkeys")]
    pause_key: Option<String>,
    /// Cancels a recording/request or retries the latest completed recording.
    #[arg(long, value_name = "HOTKEY", help_heading = "Audio Hotkeys")]
    cancel_or_retry_key: Option<String>,
    /// Use the low-level keyboard hook instead of RegisterHotKey.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Audio Hotkeys"
    )]
    hotkey_hook: Option<bool>,
    /// Use Unicode output instead of clipboard paste; Rewrite input still uses Copy.
    #[arg(long, value_name = "BOOL", action = ArgAction::Set, help_heading = "Audio Hotkeys")]
    use_sendinput: Option<bool>,
    /// Milliseconds to wait before Ctrl+V output or Ctrl+C Rewrite input.
    #[arg(long, value_name = "MS", help_heading = "Audio Hotkeys")]
    clipboard_write_delay: Option<u64>,
    /// Milliseconds to wait before restoring the clipboard after paste or Rewrite input.
    #[arg(long, value_name = "MS", help_heading = "Audio Hotkeys")]
    clipboard_restore_delay: Option<u64>,

    /// Directory used for temporary and retained cache files.
    #[arg(long, value_name = "PATH", help_heading = "Cache")]
    cache_dir: Option<String>,
    /// Retain audio and successful response cache files.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Cache"
    )]
    keep_cache: Option<bool>,
    /// Write [request failed] after all audio request attempts fail.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Cache"
    )]
    request_failed_notification: Option<bool>,

    /// Enable FFmpeg conversion diagnostics.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Debug"
    )]
    ffmpeg_debug: Option<bool>,
    /// Enable recording diagnostics.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Debug"
    )]
    record_debug: Option<bool>,
    /// Enable hotkey diagnostics.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Debug"
    )]
    hotkey_debug: Option<bool>,
    /// Enable ASR upload diagnostics.
    #[arg(
        long,
        value_name = "BOOL",
        action = ArgAction::Set,
        help_heading = "Debug"
    )]
    upload_debug: Option<bool>,
}

impl Arguments {
    fn has_config_override(&self) -> bool {
        self.api_endpoint.is_some()
            || self.input_device.is_some()
            || self.token.is_some()
            || self.model.is_some()
            || self.language.is_some()
            || self.prompt.is_some()
            || self.text_path.is_some()
            || self.extra_config.is_some()
            || self.codecs.is_some()
            || self.container.is_some()
            || self.channels.is_some()
            || self.sampling_rate.is_some()
            || self.sampling_rate_depth.is_some()
            || self.bit_rate.is_some()
            || self.request_timeout.is_some()
            || self.max_retry.is_some()
            || self.retry_base_delay.is_some()
            || self.enable_http2.is_some()
            || self.verify_ssl.is_some()
            || self.start_key.is_some()
            || self.pause_key.is_some()
            || self.cancel_or_retry_key.is_some()
            || self.hotkey_hook.is_some()
            || self.use_sendinput.is_some()
            || self.clipboard_write_delay.is_some()
            || self.clipboard_restore_delay.is_some()
            || self.cache_dir.is_some()
            || self.keep_cache.is_some()
            || self.request_failed_notification.is_some()
            || self.ffmpeg_debug.is_some()
            || self.enable_vad.is_some()
            || self.vad_padding_ms.is_some()
            || self.vad_start_threshold.is_some()
            || self.record_debug.is_some()
            || self.hotkey_debug.is_some()
            || self.upload_debug.is_some()
            || self.output.is_some()
    }

    fn apply(self, config: &mut Config) {
        if let Some(id) = self.input_device {
            config.input_device = if id == "default" { String::new() } else { id };
            config.input_device_name.clear();
        }
        set(&mut config.api_endpoint, self.api_endpoint);
        set(&mut config.token, self.token);
        set(&mut config.model, self.model);
        set(&mut config.language, self.language);
        set(&mut config.prompt, self.prompt);
        set(&mut config.text_path, self.text_path);
        set(&mut config.extra_config, self.extra_config);
        set(&mut config.codecs, self.codecs);
        set(&mut config.container, self.container);
        set(&mut config.channels, self.channels);
        set(&mut config.sampling_rate, self.sampling_rate);
        set(&mut config.sampling_rate_depth, self.sampling_rate_depth);
        set(&mut config.bit_rate, self.bit_rate);
        set(&mut config.request_timeout, self.request_timeout);
        set(&mut config.max_retry, self.max_retry);
        set(&mut config.retry_base_delay, self.retry_base_delay);
        set(&mut config.enable_http2, self.enable_http2);
        set(&mut config.verify_ssl, self.verify_ssl);
        set(&mut config.start_key, self.start_key);
        set(&mut config.pause_key, self.pause_key);
        set(&mut config.cancel_or_retry_key, self.cancel_or_retry_key);
        set(&mut config.hotkey_hook, self.hotkey_hook);
        set(&mut config.use_sendinput, self.use_sendinput);
        set(
            &mut config.clipboard_write_delay,
            self.clipboard_write_delay,
        );
        set(
            &mut config.clipboard_restore_delay,
            self.clipboard_restore_delay,
        );
        set(&mut config.cache_dir, self.cache_dir);
        set(&mut config.keep_cache, self.keep_cache);
        set(
            &mut config.request_failed_notification,
            self.request_failed_notification,
        );
        set(&mut config.ffmpeg_debug, self.ffmpeg_debug);
        set(&mut config.enable_vad, self.enable_vad);
        set(&mut config.vad_padding_ms, self.vad_padding_ms);
        set(&mut config.vad_start_threshold, self.vad_start_threshold);
        set(&mut config.record_debug, self.record_debug);
        set(&mut config.hotkey_debug, self.hotkey_debug);
        set(&mut config.upload_debug, self.upload_debug);
    }
}

fn parse_vad_start_threshold(value: &str) -> Result<f64, String> {
    let threshold = value.parse::<f64>().map_err(|error| error.to_string())?;
    dictate_core::config::validate_vad_start_threshold(threshold)
        .map_err(|error| error.to_string())?;
    Ok(threshold)
}

fn set<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("[main] {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    if arguments.list_input_devices {
        let devices = dictate_core::audio_devices::list_input_devices()?;
        if devices.is_empty() {
            println!("No active microphone inputs found.");
        }
        for device in devices {
            println!(
                "{}{}\n  {}",
                device.name,
                if device.is_default {
                    " [system default]"
                } else {
                    ""
                },
                device.id
            );
        }
        return Ok(());
    }
    let explicit_config = arguments.config.clone();
    let file = arguments.file.clone();
    let output = arguments.output.clone();
    let has_override = arguments.has_config_override();

    let mut config = if let Some(path) = explicit_config {
        Config::load(path)?
    } else if Path::new("config.json").exists() {
        Config::load("config.json")?
    } else if !has_override {
        Config::default().save("config.json")?;
        let path = std::env::current_dir()?.join("config.json");
        println!(
            "[main] default config created at {}. Please edit it and re-run.",
            path.display()
        );
        return Ok(());
    } else {
        Config::default()
    };

    arguments.apply(&mut config);
    config.validate()?;
    let converter = Arc::new(EmbeddedFfmpegConverter);

    if let Some(file) = file {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let signal_token = cancellation.clone();
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_token.cancel();
            }
        });
        let result = run_file_mode_with_cancellation(
            config,
            converter,
            &file,
            output.as_deref(),
            cancellation,
        )
        .await;
        signal.abort();
        match result {
            Ok(path) => println!("[main] transcription written to {}", path.display()),
            Err(dictate_core::runtime::RuntimeError::Convert(
                dictate_core::converter::ConvertError::NoSpeech,
            )) => println!("[main] No speech detected"),
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }

    let runtime = Runtime::new(config, converter)?;
    runtime.enable_retry_buffer();
    runtime.set_event_handler(Some(Arc::new(|event| {
        if event.error.is_empty() {
            println!("[state] {}: {}", event.state, event.message);
        } else {
            println!(
                "[state] {}: {} ({})",
                event.state, event.message, event.error
            );
        }
    })));
    runtime.start_hotkeys()?;
    println!("[main] ready. Use hotkeys to start/stop/pause/cancel or retry.");
    tokio::signal::ctrl_c().await?;
    runtime.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn microphone_override_and_explicit_default_are_session_only() {
        use clap::Parser;
        for (args, expected) in [
            (vec!["dictate-cli"], "saved-endpoint"),
            (
                vec!["dictate-cli", "--input-device", "another-endpoint"],
                "another-endpoint",
            ),
            (vec!["dictate-cli", "--input-device", "default"], ""),
        ] {
            let mut config = dictate_core::Config {
                input_device: "saved-endpoint".into(),
                input_device_name: "Saved mic".into(),
                ..Default::default()
            };
            let original = config.clone();
            let arguments = super::Arguments::try_parse_from(args).unwrap();
            let overridden = arguments.input_device.is_some();
            assert_eq!(arguments.has_config_override(), overridden);
            arguments.apply(&mut config);
            assert_eq!(config.input_device, expected);
            assert_eq!(original.input_device, "saved-endpoint");
            assert_eq!(
                config.input_device_name,
                if overridden { "" } else { "Saved mic" }
            );
        }
        let listing =
            super::Arguments::try_parse_from(["dictate-cli", "--list-input-devices"]).unwrap();
        assert!(listing.list_input_devices);
        assert!(!listing.has_config_override());
    }
    #[test]
    fn vad_override_and_range() {
        use clap::Parser;
        let args = super::Arguments::try_parse_from([
            "dictate-cli",
            "--enable-vad",
            "false",
            "--vad-padding-ms",
            "0",
        ])
        .unwrap();
        let mut config = dictate_core::Config {
            enable_vad: true,
            ..Default::default()
        };
        args.apply(&mut config);
        assert!(!config.enable_vad);
        assert_eq!(config.vad_padding_ms, 0);
        for value in ["-1", "1001"] {
            assert!(
                super::Arguments::try_parse_from(["dictate-cli", "--vad-padding-ms", value])
                    .is_err()
            );
        }
        assert!(super::Arguments::try_parse_from(["dictate-cli", "--enable-vad"]).is_err());
    }
    #[test]
    fn vad_start_threshold_override_and_range() {
        use clap::Parser;
        for value in ["0.5", "0.6", "1.0"] {
            let args =
                super::Arguments::try_parse_from(["dictate-cli", "--vad-start-threshold", value])
                    .unwrap();
            assert!(args.has_config_override());
            let mut config = dictate_core::Config::default();
            args.apply(&mut config);
            assert_eq!(config.vad_start_threshold, value.parse::<f64>().unwrap());
            config.validate().unwrap();
        }
        for value in ["0", "0.499", "1.001", "NaN", "inf", "invalid"] {
            assert!(
                super::Arguments::try_parse_from(["dictate-cli", "--vad-start-threshold", value,])
                    .is_err()
            );
        }
        let mut config = dictate_core::Config {
            vad_start_threshold: 0.8,
            ..Default::default()
        };
        super::Arguments::try_parse_from(["dictate-cli"])
            .unwrap()
            .apply(&mut config);
        assert_eq!(config.vad_start_threshold, 0.8);
    }
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn sendinput_override_is_bidirectional_and_optional() {
        let mut config = Config {
            use_sendinput: true,
            ..Config::default()
        };
        Arguments::try_parse_from(["dictate-cli"])
            .unwrap()
            .apply(&mut config);
        assert!(config.use_sendinput);
        for value in ["false", "true"] {
            let args =
                Arguments::try_parse_from(["dictate-cli", "--use-sendinput", value]).unwrap();
            assert!(args.has_config_override());
            args.apply(&mut config);
            assert_eq!(config.use_sendinput, value == "true");
        }
    }

    #[test]
    fn standard_boolean_values_and_alias_apply() {
        let arguments = Arguments::try_parse_from([
            "dictate-cli",
            "--verify-ssl",
            "false",
            "--enable-http2",
            "true",
            "--hotkey-hook",
            "false",
            "--clipboard-write-delay",
            "25",
            "--clipboard-restore-delay",
            "75",
            "--rate",
            "22050",
        ])
        .unwrap();
        let mut config = Config::default();
        arguments.apply(&mut config);
        assert!(!config.verify_ssl);
        assert!(config.enable_http2);
        assert!(!config.hotkey_hook);
        assert_eq!(config.clipboard_write_delay, 25);
        assert_eq!(config.clipboard_restore_delay, 75);
        assert_eq!(config.sampling_rate, 22_050);
    }

    #[test]
    fn cancel_or_retry_key_option_replaces_cancel_key() {
        let arguments =
            Arguments::try_parse_from(["dictate-cli", "--cancel-or-retry-key", "ctrl+alt+r"])
                .unwrap();
        let mut config = Config::default();
        arguments.apply(&mut config);
        assert_eq!(config.cancel_or_retry_key, "ctrl+alt+r");
        assert!(Arguments::try_parse_from(["dictate-cli", "--cancel-key", "alt+esc"]).is_err());
    }

    #[test]
    fn help_is_grouped_like_gui_settings() {
        let mut command = Arguments::command();
        let help = command.render_long_help().to_string();
        let mut previous = 0;
        for heading in [
            "General:",
            "Audio API:",
            "Audio Record:",
            "Network:",
            "Audio Hotkeys:",
            "Cache:",
            "Debug:",
        ] {
            let position = help
                .find(heading)
                .unwrap_or_else(|| panic!("missing help heading {heading:?}\n{help}"));
            assert!(
                position >= previous,
                "help heading order is incorrect\n{help}"
            );
            previous = position;
        }
    }

    #[test]
    fn removed_notification_argument_is_rejected() {
        let error =
            Arguments::try_parse_from(["dictate-cli", "--notification", "true"]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unexpected argument '--notification'")
        );
    }

    #[test]
    fn single_dash_legacy_argument_is_rejected() {
        assert!(Arguments::try_parse_from(["dictate-cli", "-token", "secret"]).is_err());
    }
}
