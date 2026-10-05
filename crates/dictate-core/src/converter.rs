use async_trait::async_trait;
use std::path::Path;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionSettings {
    pub codec_key: String,
    pub ffmpeg_codec: String,
    pub codec_has_bitrate: bool,
    pub channels: i32,
    pub sample_rate: i32,
    pub bitrate: i32,
    pub depth: i32,
    pub sample_format: String,
}

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("No speech detected")]
    NoSpeech,
    #[error("conversion canceled")]
    Canceled,
    #[error("input and output paths must differ")]
    SamePath,
    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),
    #[error("ffmpeg failed: {message}")]
    Failed { message: String },
    #[error("libav conversion is unavailable in this build")]
    LibAvUnavailable,
}

#[async_trait]
pub trait AudioConverter: Send + Sync {
    async fn convert(
        &self,
        cancellation: &CancellationToken,
        config: &Config,
        input: &Path,
        output: &Path,
        source_rate: i32,
    ) -> Result<(), ConvertError>;
}

/// Shared front-end entry; implementations own analysis and final conversion.
pub async fn prepare_audio_for_upload(
    converter: &dyn AudioConverter,
    cancellation: &CancellationToken,
    config: &Config,
    input: &Path,
    output: &Path,
    source_rate: i32,
) -> Result<(), ConvertError> {
    converter
        .convert(cancellation, config, input, output, source_rate)
        .await
}

pub fn settings_for(config: &Config, source_rate: i32) -> Result<ConversionSettings, ConvertError> {
    let codec_key = config.codecs.to_ascii_lowercase();
    let (ffmpeg_codec, codec_has_bitrate) = ffmpeg_codec_for(&codec_key)
        .ok_or_else(|| ConvertError::UnsupportedCodec(config.codecs.clone()))?;
    let channels = if config.channels <= 0 {
        1
    } else {
        config.channels
    };
    let sample_rate = if config.sampling_rate <= 0 {
        source_rate
    } else {
        config.sampling_rate
    };
    let bitrate = if config.bit_rate <= 0 {
        128
    } else {
        config.bit_rate
    };
    let depth = if config.sampling_rate_depth == 0 {
        16
    } else {
        config.sampling_rate_depth
    };
    let sample_format = if ffmpeg_codec.starts_with("pcm_") {
        String::new()
    } else {
        sample_format_for_depth(depth).into()
    };
    Ok(ConversionSettings {
        codec_key,
        ffmpeg_codec: ffmpeg_codec.into(),
        codec_has_bitrate,
        channels,
        sample_rate,
        bitrate,
        depth,
        sample_format,
    })
}

pub fn sample_format_for_depth(depth: i32) -> &'static str {
    match depth {
        8 => "u8",
        16 => "s16",
        24 => "s24",
        32 => "s32",
        _ => "",
    }
}

pub fn ffmpeg_codec_for(codec: &str) -> Option<(&'static str, bool)> {
    match codec.to_ascii_lowercase().as_str() {
        "opus" | "libopus" => Some(("libopus", true)),
        "wavpack" => Some(("wavpack", false)),
        "aac" => Some(("aac", true)),
        "ac3" => Some(("ac3", true)),
        "eac3" => Some(("eac3", true)),
        "mp3" => Some(("libmp3lame", true)),
        "mp2" => Some(("mp2", true)),
        "mp1" => Some(("mp1", true)),
        "flac" => Some(("flac", false)),
        "alac" => Some(("alac", false)),
        "pcm" => Some(("pcm_s16le", false)),
        "vorbis" | "libvorbis" | "vorb" => Some(("libvorbis", true)),
        "adpcm" => Some(("adpcm_ms", false)),
        "amr" | "libopencore_amrnb" => Some(("libopencore_amrnb", true)),
        "amr_wb" | "libvo_amrwbenc" => Some(("libvo_amrwbenc", true)),
        "speex" | "libspeex" => Some(("libspeex", true)),
        "wmav1" => Some(("wmav1", true)),
        "wmav2" => Some(("wmav2", true)),
        "pcm_alaw" => Some(("pcm_alaw", false)),
        "pcm_mulaw" => Some(("pcm_mulaw", false)),
        value @ ("pcm_f32be" | "pcm_f32le" | "pcm_f64be" | "pcm_f64le" | "pcm_s16be"
        | "pcm_s16le" | "pcm_s24be" | "pcm_s24le" | "pcm_s32be" | "pcm_s32le"
        | "pcm_s64be" | "pcm_s64le" | "pcm_s8") => {
            // All arms are static string literals; return the canonical literal.
            match value {
                "pcm_f32be" => Some(("pcm_f32be", false)),
                "pcm_f32le" => Some(("pcm_f32le", false)),
                "pcm_f64be" => Some(("pcm_f64be", false)),
                "pcm_f64le" => Some(("pcm_f64le", false)),
                "pcm_s16be" => Some(("pcm_s16be", false)),
                "pcm_s16le" => Some(("pcm_s16le", false)),
                "pcm_s24be" => Some(("pcm_s24be", false)),
                "pcm_s24le" => Some(("pcm_s24le", false)),
                "pcm_s32be" => Some(("pcm_s32be", false)),
                "pcm_s32le" => Some(("pcm_s32le", false)),
                "pcm_s64be" => Some(("pcm_s64be", false)),
                "pcm_s64le" => Some(("pcm_s64le", false)),
                "pcm_s8" => Some(("pcm_s8", false)),
                _ => unreachable!(),
            }
        }
        _ => None,
    }
}

pub fn paths_equal(first: &Path, second: &Path) -> bool {
    first
        .to_string_lossy()
        .eq_ignore_ascii_case(&second.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_aliases_and_defaults() {
        let settings = settings_for(
            &Config {
                codecs: "MP3".into(),
                sampling_rate: 0,
                channels: 0,
                bit_rate: 0,
                sampling_rate_depth: 0,
                ..Config::default()
            },
            44_100,
        )
        .unwrap();
        assert_eq!(settings.ffmpeg_codec, "libmp3lame");
        assert_eq!(settings.channels, 1);
        assert_eq!(settings.sample_rate, 44_100);
        assert_eq!(settings.bitrate, 128);
        assert_eq!(settings.sample_format, "s16");
    }
}
