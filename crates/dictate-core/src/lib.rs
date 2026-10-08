pub mod additional_parameters;
pub mod advanced_audio;
pub mod asr;
pub mod audio_api;
pub mod cache;
pub mod clipboard;
pub mod config;
pub mod converter;
pub mod debug_log;
pub mod hotkey;
pub mod jsonpath;
pub mod keyboard;
mod network;
pub mod recorder;
pub mod rewrite;
pub mod runtime;
pub(crate) mod segmented_upload;
pub mod selection;
pub mod text_input;

pub use config::Config;

pub mod audio_devices;
pub mod audio_intervals;
pub(crate) mod audio_segments;
mod capture_wav;
pub mod embedded_ffmpeg;
pub mod vad;
