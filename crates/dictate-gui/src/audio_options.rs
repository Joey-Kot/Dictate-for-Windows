//! GUI presets only. Loading never normalizes a user's configuration.
use dictate_core::Config;

pub const KEYS: [&str; 6] = [
    "CHANNELS",
    "SAMPLING_RATE_DEPTH",
    "SAMPLING_RATE",
    "BIT_RATE",
    "CODECS",
    "CONTAINER",
];
pub const RATES: &[i32] = &[
    7350, 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    176400, 192000,
];
pub const BITRATES: &[i32] = &[
    6, 8, 10, 12, 16, 20, 24, 28, 32, 40, 44, 48, 50, 56, 60, 64, 80, 96, 100, 112, 120, 128, 140,
    144, 160, 180, 190, 192, 200, 224, 240, 250, 256, 288, 320, 352, 384, 448, 500, 510, 512, 576,
    640,
];
// GUI output combinations target the embedded FFmpeg 8.1 build.
pub const CODECS: &[(&str, &str)] = &[
    ("opus", "Opus"),
    ("mp3", "MP3"),
    ("aac", "AAC"),
    ("vorbis", "Vorbis"),
    ("flac", "FLAC"),
    ("alac", "ALAC"),
    ("ac3", "AC-3"),
    ("eac3", "E-AC-3"),
    ("mp2", "MP2"),
    ("adpcm", "ADPCM (MS)"),
    ("amr", "AMR-NB"),
    ("amr_wb", "AMR-WB"),
    ("speex", "Speex"),
    ("wavpack", "WavPack"),
    ("wmav1", "WMA v1"),
    ("wmav2", "WMA v2"),
    ("pcm_s8", "PCM 8-bit (signed)"),
    ("pcm_alaw", "PCM A-law"),
    ("pcm_mulaw", "PCM μ-law"),
    ("pcm", "PCM"),
    ("pcm_f32le", "PCM Float 32-bit"),
    ("pcm_f64le", "PCM Float 64-bit"),
    ("pcm_s64le", "PCM 64-bit (LE)"),
    ("pcm_s16be", "PCM 16-bit (BE)"),
    ("pcm_s24be", "PCM 24-bit (BE)"),
    ("pcm_s32be", "PCM 32-bit (BE)"),
    ("pcm_f32be", "PCM Float 32-bit (BE)"),
    ("pcm_f64be", "PCM Float 64-bit (BE)"),
];

#[derive(Clone, Debug)]
pub struct AudioDraft {
    pub values: [String; 6],
}

pub fn canonical(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "libspeex" => "speex".into(),
        "libvo_amrwbenc" => "amr_wb".into(),
        "libopencore_amrnb" => "amr".into(),
        "libopus" => "opus".into(),
        "libvorbis" | "vorb" => "vorbis".into(),
        "pcm_s16le" | "pcm_s24le" | "pcm_s32le" => "pcm".into(),
        value => value.into(),
    }
}

impl AudioDraft {
    pub fn new(config: &Config) -> Self {
        Self {
            values: [
                config.channels.to_string(),
                config.sampling_rate_depth.to_string(),
                config.sampling_rate.to_string(),
                config.bit_rate.to_string(),
                config.codecs.clone(),
                config.container.clone(),
            ],
        }
    }

    pub fn codec(&self) -> String {
        canonical(&self.values[4])
    }

    pub fn containers(&self) -> &'static [&'static str] {
        let rate = self.values[2].parse::<i32>().unwrap_or(16000);
        match self.codec().as_str() {
            "opus" => &["opus", "ogg", "webm", "mp4", "mkv", "mka"],
            "vorbis" => &["ogg", "webm", "mkv", "mka"],
            "aac" => &["m4a", "mp4", "aac", "wav", "flv", "mkv", "mka", "mov"],
            "mp3" if rate == 11025 => &["mp3", "wav", "avi", "flv", "mkv", "mka", "mpeg"],
            "mp3" if rate < 16000 => &["mp3", "wav", "avi", "mkv", "mka", "mpeg"],
            "mp3" if [22050, 44100, 48000].contains(&rate) => {
                &["mp3", "wav", "mp4", "avi", "flv", "mkv", "mka", "mpeg"]
            }
            "mp3" => &["mp3", "wav", "mp4", "avi", "mkv", "mka", "mpeg"],
            "alac" => &["m4a", "mp4", "mov"],
            "flac" => &["flac", "ogg", "mkv", "mka"],
            "ac3" => &["ac3", "m4a", "mp4", "wav", "avi", "mkv", "mka", "mpeg"],
            "eac3" => &["eac3", "mp4", "mkv", "mka"],
            "amr" => &["amr", "wav"],
            "amr_wb" => &["amr"],
            "speex" => &["spx", "ogg"],
            "wavpack" => &["wv"],
            "wmav1" | "wmav2" => &["wma", "asf"],
            "pcm_s8" => &["aiff", "s8"],
            "pcm_alaw" => &["wav", "alaw"],
            "pcm_mulaw" => &["wav", "mulaw"],
            "pcm_s64le" | "adpcm" => &["wav"],
            "mp2" => &["wav", "mp4", "mpeg"],
            "pcm" => match self.values[4].to_ascii_lowercase().as_str() {
                "pcm_s24le" => &["wav", "mp4", "mov", "s24le"],
                "pcm_s32le" => &["wav", "mp4", "mov", "s32le"],
                _ => &["wav", "mp4", "mov", "avi", "s16le"],
            },
            "pcm_f32le" => &["wav", "mp4", "f32le"],
            "pcm_f64le" => &["wav", "mp4", "f64le"],
            "pcm_s16be" => &["aiff", "mp4", "s16be"],
            "pcm_s24be" => &["aiff", "mp4", "s24be"],
            "pcm_s32be" => &["aiff", "mp4", "s32be"],
            "pcm_f32be" => &["aiff", "mp4", "f32be"],
            "pcm_f64be" => &["aiff", "mp4", "f64be"],
            _ => &[],
        }
    }

    pub fn rates(&self) -> Vec<i32> {
        RATES
            .iter()
            .copied()
            .filter(|rate| match self.codec().as_str() {
                "opus" => [8000, 12000, 16000, 24000, 48000].contains(rate),
                "mp3" => (8000..=48000).contains(rate),
                "mp2" => [16000, 22050, 24000, 32000, 44100, 48000].contains(rate),
                "ac3" | "eac3" => [32000, 44100, 48000].contains(rate),
                "amr" => *rate == 8000,
                "amr_wb" => *rate == 16000,
                "speex" => [8000, 16000, 32000].contains(rate),
                "wmav1" | "wmav2" => (8000..=48000).contains(rate),
                "aac" => *rate <= 96000,
                // The libvorbis setup tables only cover rates through 50 kHz.
                "vorbis" => (8000..=48000).contains(rate),
                _ => true,
            })
            .collect()
    }

    pub fn bitrates(&self) -> Vec<i32> {
        if self.codec() == "amr_wb" {
            return vec![7, 9, 13, 14, 16, 18, 20, 23, 24];
        }
        BITRATES
            .iter()
            .copied()
            .filter(|value| self.supports_bitrate(value))
            .collect()
    }

    fn supports_bitrate(&self, bitrate: &i32) -> bool {
        let rate = i64::from(self.values[2].parse::<i32>().unwrap_or(16000));
        let channels = self.values[0].parse::<i32>().unwrap_or(1).max(1);
        match self.codec().as_str() {
            "opus" => (6..=(256 * channels).min(510)).contains(bitrate),
            "mp3" if rate < 32000 => {
                [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160].contains(bitrate)
            }
            "mp3" => [
                32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
            ]
            .contains(bitrate),
            "mp2" if rate < 32000 => {
                [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160].contains(bitrate)
            }
            "mp2" => [
                32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
            ]
            .contains(bitrate),
            "ac3" => [
                32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576,
                640,
            ]
            .contains(bitrate),
            "eac3" => *bitrate >= 32 && i64::from(*bitrate) <= 128 * rate / 1000,
            "aac" => *bitrate >= 8 && i64::from(*bitrate) <= 6 * rate * i64::from(channels) / 1000,
            // libvorbis nominal bitrate bounds vary with rate and channel count.
            "vorbis" => {
                let (low, high) = match (rate, channels == 2) {
                    (0..=8999, true) => (6, 32),
                    (0..=8999, false) => (8, 42),
                    (9000..=14999, true) => (8, 44),
                    (9000..=14999, false) => (12, 50),
                    (15000..=18999, true) => (12, 86),
                    (15000..=18999, false) => (16, 100),
                    (19000..=25999, true) => (15, 86),
                    (19000..=25999, false) => (16, 90),
                    (26000..=39999, true) => (18, 190),
                    (26000..=39999, false) => (30, 190),
                    (_, true) => (23, 250),
                    (_, false) => (32, 240),
                };
                *bitrate >= low * channels && *bitrate <= high * channels
            }
            // AMR selects its nearest supported mode; the config stores integer kbps.
            "amr" => [6, 8, 10, 12].contains(bitrate),
            "amr_wb" => [7, 9, 13, 14, 16, 18, 20, 23, 24].contains(bitrate),
            "speex" => (6..=44).contains(bitrate),
            "wmav1" | "wmav2" => (24..=320).contains(bitrate),
            _ => false,
        }
    }

    pub fn enabled(&self, field: usize) -> bool {
        match field {
            1 => self.codec() == "pcm",
            3 => dictate_core::converter::ffmpeg_codec_for(&self.values[4])
                .is_some_and(|(_, bitrate)| bitrate),
            _ => true,
        }
    }

    pub fn options(&self, field: usize) -> Vec<String> {
        match field {
            0 => {
                if matches!(self.codec().as_str(), "amr" | "amr_wb") {
                    vec!["1".into()]
                } else {
                    vec!["1".into(), "2".into()]
                }
            }
            // pcm_u8 is not an accepted core codec; signed 8-bit cannot be muxed as WAV.
            1 => vec!["16".into(), "24".into(), "32".into()],
            2 => self.rates().iter().map(ToString::to_string).collect(),
            3 => self.bitrates().iter().map(ToString::to_string).collect(),
            4 => CODECS.iter().map(|(value, _)| (*value).into()).collect(),
            5 => self
                .containers()
                .iter()
                .map(|value| (*value).into())
                .collect(),
            _ => vec![],
        }
    }

    /// Only explicit user selections trigger dependent changes. Custom entries and
    /// untouched configurations are never silently rounded to a preset.
    pub fn select(&mut self, field: usize, value: String) {
        self.values[field] = value;
        if field == 4 {
            if self.codec() == "pcm" {
                if !["16", "24", "32"].contains(&self.values[1].as_str()) {
                    self.values[1] = "16".into();
                }
                self.map_pcm();
            }
            let max_channels = match self.codec().as_str() {
                "amr" | "amr_wb" => 1,
                "mp3" | "mp2" | "adpcm" | "speex" | "wmav1" | "wmav2" => 2,
                "ac3" | "eac3" => 6,
                _ => 8,
            };
            if !self.values[0]
                .parse::<i32>()
                .is_ok_and(|n| (1..=max_channels).contains(&n))
            {
                self.values[0] = "1".into();
            }
            self.keep_or_default(5, self.containers().first().copied().unwrap_or("opus"));
            // PCM/lossless encoders also accept positive rates outside our presets.
            if !self.codec().starts_with("pcm")
                && !["flac", "alac", "adpcm", "wavpack"].contains(&self.codec().as_str())
            {
                self.keep_or_default(2, "16000");
            }
        }
        if field == 1 && self.codec() == "pcm" {
            self.map_pcm();
        }
        if field == 2 || (field == 1 && self.codec() == "pcm") {
            self.keep_or_default(5, self.containers().first().copied().unwrap_or("opus"));
        }
        if [0, 2, 4].contains(&field) && self.enabled(3) {
            self.keep_or_default(3, "128");
        }
    }

    fn map_pcm(&mut self) {
        self.values[4] = format!("pcm_s{}le", self.values[1]);
    }

    fn keep_or_default(&mut self, field: usize, default: &str) {
        let options = self.options(field);
        // Continuous bitrate encoders also accept values between our presets.
        if field == 3
            && self.values[field]
                .parse::<i32>()
                .is_ok_and(|value| self.supports_bitrate(&value))
        {
            return;
        }
        if !options
            .iter()
            .any(|v| v.eq_ignore_ascii_case(&self.values[field]))
            && let Some(value) = options
                .iter()
                .find(|v| v.as_str() == default)
                .or(options.first())
        {
            self.values[field] = value.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_preserves_non_presets_and_aliases() {
        assert_eq!(KEYS.len(), 6);
        let config = Config {
            channels: 6,
            sampling_rate: 12345,
            bit_rate: 123,
            codecs: "LIBOPUS".into(),
            container: "OGG".into(),
            ..Config::default()
        };
        let draft = AudioDraft::new(&config);
        assert_eq!(draft.values, ["6", "16", "12345", "123", "LIBOPUS", "OGG"]);
        assert_eq!(draft.codec(), "opus");
    }

    #[test]
    fn pcm_selection_maps_depth_but_loading_does_not_rewrite() {
        let mut draft = AudioDraft::new(&Config::default());
        draft.select(4, "pcm".into());
        draft.select(1, "24".into());
        assert_eq!(draft.values[4], "pcm_s24le");
        assert_eq!(draft.values[5], "wav");
        assert!(!draft.enabled(3));
        let config = Config {
            codecs: "pcm_s24le".into(),
            sampling_rate_depth: 16,
            ..Config::default()
        };
        assert_eq!(AudioDraft::new(&config).values[1], "16");
    }

    #[test]
    fn explicit_codec_and_rate_changes_reconcile_dependents() {
        let mut draft = AudioDraft::new(&Config::default());
        draft.select(4, "ac3".into());
        assert_eq!(draft.values[2], "32000");
        assert_eq!(draft.values[5], "ac3");
        draft.select(4, "mp3".into());
        draft.select(3, "320".into());
        draft.select(2, "16000".into());
        assert_eq!(draft.values[3], "128");
        draft.select(4, "opus".into());
        assert_eq!(draft.values[5], "opus");
        assert!(!draft.rates().contains(&44100));
    }

    #[test]
    fn defaults_and_existing_values_survive_config_roundtrip() {
        let defaults: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(
            AudioDraft::new(&defaults).values,
            ["1", "16", "16000", "128", "opus", "opus"]
        );
        let old: Config = serde_json::from_str(r#"{"CHANNELS":6,"SAMPLING_RATE":12345,"BIT_RATE":123,"CODECS":"libopus","CONTAINER":"ogg"}"#).unwrap();
        old.validate().unwrap();
        let roundtrip: Config =
            serde_json::from_value(serde_json::to_value(&old).unwrap()).unwrap();
        assert_eq!(
            AudioDraft::new(&old).values,
            AudioDraft::new(&roundtrip).values
        );
        let mut draft = AudioDraft::new(&old);
        draft.select(4, "pcm".into());
        assert_eq!(draft.values[0], "6");
        assert_eq!(draft.values[2], "12345");
        assert_eq!(draft.values[3], "123");
        draft.select(4, "pcm_s24be".into());
        assert_eq!(draft.values[2], "12345");
    }

    #[test]
    fn every_codec_has_valid_defaults_and_sorted_presets() {
        for (codec, _) in CODECS {
            let mut draft = AudioDraft::new(&Config::default());
            draft.select(4, (*codec).into());
            assert!(!draft.containers().is_empty(), "{codec}");
            assert!(draft.rates().windows(2).all(|w| w[0] < w[1]));
            assert!(draft.bitrates().windows(2).all(|w| w[0] < w[1]));
            let config = Config {
                channels: draft.values[0].parse().unwrap(),
                sampling_rate_depth: draft.values[1].parse().unwrap(),
                sampling_rate: draft.values[2].parse().unwrap(),
                bit_rate: draft.values[3].parse().unwrap(),
                codecs: draft.values[4].clone(),
                container: draft.values[5].clone(),
                ..Config::default()
            };
            config.validate().unwrap_or_else(|e| panic!("{codec}: {e}"));
            for container in draft.containers() {
                let saved = Config {
                    container: (*container).into(),
                    ..config.clone()
                };
                saved
                    .validate()
                    .unwrap_or_else(|e| panic!("{codec}/{container}: {e}"));
                dictate_core::converter::settings_for(&saved, 16000).unwrap();
            }
        }
    }

    #[test]
    fn ac3_codec_selection_reconciles_unsupported_channels_only() {
        for codec in ["ac3", "eac3"] {
            for channels in [6, 7, 8] {
                let config = Config {
                    codecs: "pcm_s16le".into(),
                    container: "wav".into(),
                    channels,
                    ..Config::default()
                };
                let mut draft = AudioDraft::new(&config);
                assert_eq!(draft.values[0], channels.to_string());
                draft.select(4, codec.into());
                assert_eq!(draft.values[0], if channels <= 6 { "6" } else { "1" });
                assert_eq!(draft.values[2], "32000");
                let existing = Config {
                    codecs: codec.into(),
                    ..config
                };
                assert_eq!(AudioDraft::new(&existing).values[0], channels.to_string());
            }
        }
    }

    #[test]
    fn new_codecs_and_raw_depth_changes_produce_compatible_settings() {
        let mut draft = AudioDraft::new(&Config::default());
        draft.select(0, "2".into());
        draft.select(4, "amr_wb".into());
        assert_eq!(draft.values[0], "1");
        assert_eq!(draft.values[2], "16000");
        assert_eq!(draft.values[5], "amr");
        assert!(draft.bitrates().contains(&draft.values[3].parse().unwrap()));
        draft.select(4, "speex".into());
        assert_eq!(draft.rates(), [8000, 16000, 32000]);
        assert_eq!(draft.values[5], "spx");
        draft.select(4, "pcm".into());
        draft.select(5, "s16le".into());
        draft.select(1, "24".into());
        assert_eq!(draft.values[4], "pcm_s24le");
        assert!(!draft.containers().contains(&"s16le"));
        assert!(draft.containers().contains(&draft.values[5].as_str()));
        for alias in ["libspeex", "libvo_amrwbenc", "libopencore_amrnb"] {
            let config = Config {
                codecs: alias.into(),
                ..Config::default()
            };
            assert_eq!(AudioDraft::new(&config).values[4], alias);
            assert!(CODECS.iter().any(|(name, _)| *name == canonical(alias)));
        }
    }

    /// Run with the project's embedded host FFmpeg, not a system FFmpeg executable.
    /// DICTATE_PRESET_CODECS can select encoders present in a smaller validation build.
    #[cfg(feature = "static-libav")]
    #[tokio::test]
    #[ignore = "requires an embedded FFmpeg build with the selected output encoders"]
    async fn embedded_presets_encode() {
        use dictate_core::converter::AudioConverter;
        use dictate_core::embedded_ffmpeg::EmbeddedFfmpegConverter;
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.wav");
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..16000 {
            writer
                .write_sample(((i as f64 * 0.17).sin() * 12000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
        let filter = std::env::var("DICTATE_PRESET_CODECS").ok();
        let token = tokio_util::sync::CancellationToken::new();
        let default_output = dir.path().join("default.opus");
        EmbeddedFfmpegConverter
            .convert(&token, &Config::default(), &input, &default_output, 16000)
            .await
            .unwrap();
        let bytes = std::fs::read(default_output).unwrap();
        assert_eq!(&bytes[..4], b"OggS");
        let header = bytes
            .windows(8)
            .position(|window| window == b"OpusHead")
            .unwrap();
        assert_eq!(bytes[header + 9], 1);
        assert_eq!(
            u32::from_le_bytes(bytes[header + 12..header + 16].try_into().unwrap()),
            16000
        );
        for depth in [16, 24, 32] {
            let mut draft = AudioDraft::new(&Config::default());
            draft.select(4, "pcm".into());
            draft.select(1, depth.to_string());
            let output = dir.path().join("pcm-depth.wav");
            let config = Config {
                codecs: draft.values[4].clone(),
                container: "wav".into(),
                sampling_rate_depth: depth,
                ..Config::default()
            };
            EmbeddedFfmpegConverter
                .convert(&token, &config, &input, &output, 16000)
                .await
                .unwrap();
            let spec = hound::WavReader::open(output).unwrap().spec();
            assert_eq!(spec.bits_per_sample, depth as u16);
            assert_eq!(spec.channels, 1);
            assert_eq!(spec.sample_rate, 16000);
        }
        // Raw output must contain samples only, with the selected width and byte order.
        for (codec, container, width) in [
            ("pcm_s8", "s8", 1),
            ("pcm_s16le", "s16le", 2),
            ("pcm_s16be", "s16be", 2),
            ("pcm_s24le", "s24le", 3),
            ("pcm_s32le", "s32le", 4),
            ("pcm_f32le", "f32le", 4),
            ("pcm_f64le", "f64le", 8),
            ("pcm_alaw", "alaw", 1),
            ("pcm_mulaw", "mulaw", 1),
        ] {
            if filter.is_some() {
                break; // Smaller host builds need not contain the new raw muxers.
            }
            let config = Config {
                codecs: codec.into(),
                container: container.into(),
                ..Config::default()
            };
            let output = dir.path().join(format!("raw.{container}"));
            EmbeddedFfmpegConverter
                .convert(&token, &config, &input, &output, 16000)
                .await
                .unwrap();
            let bytes = std::fs::read(output).unwrap();
            assert_eq!(bytes.len(), 16000 * width, "{codec}");
            if container == "s16le" || container == "s16be" {
                let samples: Vec<i16> = hound::WavReader::open(&input)
                    .unwrap()
                    .samples::<i16>()
                    .map(Result::unwrap)
                    .collect();
                let expected: Vec<u8> = samples
                    .iter()
                    .flat_map(|sample| {
                        if container == "s16le" {
                            sample.to_le_bytes()
                        } else {
                            sample.to_be_bytes()
                        }
                    })
                    .collect();
                assert_eq!(bytes, expected, "{codec}");
            }
        }
        let mut count = 0;
        for (codec, _) in CODECS
            .iter()
            .chain([("pcm_s24le", ""), ("pcm_s32le", "")].iter())
        {
            if filter
                .as_ref()
                .is_some_and(|filter| !filter.split(',').any(|v| v == *codec))
            {
                continue;
            }
            let mut draft = AudioDraft::new(&Config::default());
            draft.select(4, (*codec).into());
            for channels in draft.options(0) {
                draft.select(0, channels);
                for rate in draft.rates() {
                    draft.select(2, rate.to_string());
                    let rates = draft.bitrates();
                    let mut bitrates = vec![draft.values[3].parse::<i32>().unwrap()];
                    if let (Some(low), Some(high)) = (rates.first(), rates.last()) {
                        bitrates.extend([*low, *high]);
                    }
                    bitrates.sort_unstable();
                    bitrates.dedup();
                    for bitrate in bitrates {
                        for container in draft.containers() {
                            let output = dir.path().join(format!("encoded.{container}"));
                            let config = Config {
                                channels: draft.values[0].parse().unwrap(),
                                sampling_rate: rate,
                                bit_rate: bitrate,
                                codecs: draft.values[4].clone(),
                                container: (*container).into(),
                                ..Config::default()
                            };
                            EmbeddedFfmpegConverter.convert(&token, &config, &input, &output, 16000).await
                                .unwrap_or_else(|e| panic!("{codec}/{container}, {} ch, {rate} Hz, {bitrate} kbps: {e}", config.channels));
                            assert!(std::fs::metadata(&output).unwrap().len() > 0);
                            count += 1;
                        }
                    }
                }
            }
        }
        assert!(count > 0, "no encoders selected");
        eprintln!("Validated {count} embedded audio preset combinations");
    }
}
