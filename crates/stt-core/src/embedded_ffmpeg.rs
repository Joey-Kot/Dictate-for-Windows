use crate::{
    Config,
    converter::{AudioConverter, ConvertError, paths_equal},
};
use async_trait::async_trait;
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
pub struct EmbeddedFfmpegConverter;

#[async_trait]
impl AudioConverter for EmbeddedFfmpegConverter {
    async fn convert(
        &self,
        cancellation: &CancellationToken,
        config: &Config,
        input: &Path,
        output: &Path,
        source_rate: i32,
    ) -> Result<(), ConvertError> {
        if cancellation.is_cancelled() {
            return Err(ConvertError::Canceled);
        }
        if paths_equal(input, output)
            || (input.exists()
                && output.exists()
                && input.canonicalize().ok() == output.canonicalize().ok())
        {
            return Err(ConvertError::SamePath);
        }
        #[cfg(not(feature = "static-libav"))]
        {
            let _ = (config, source_rate);
            Err(ConvertError::LibAvUnavailable)
        }
        #[cfg(feature = "static-libav")]
        {
            let (config, input, output, token) = (
                config.clone(),
                input.to_path_buf(),
                output.to_path_buf(),
                cancellation.clone(),
            );
            // The worker owns the token and callback contexts until C has fully unwound.
            // Await it even after cancellation, so cleanup never races an open output.
            tokio::task::spawn_blocking(move || {
                native::prepare(&token, &config, &input, &output, source_rate)
            })
            .await
            .map_err(|e| ConvertError::Failed {
                message: e.to_string(),
            })?
        }
    }
}

#[cfg(feature = "static-libav")]
mod native {
    #[cfg(test)]
    mod tests {
        include!("embedded_ffmpeg_tests.rs");
    }
    use super::*;
    use crate::{audio_intervals::prepare_intervals, converter::settings_for, vad::Vad};
    use std::ffi::{CStr, CString, c_char, c_void};
    #[repr(C)]
    struct Interval {
        start_frame: i64,
        end_frame: i64,
    }
    type Cancel = extern "C" fn(*mut c_void) -> i32;
    type Samples = extern "C" fn(*mut c_void, *const i16, i32) -> i32;
    static CONVERT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    static LOG_INIT: std::sync::Once = std::sync::Once::new();
    unsafe extern "C" {
        fn stt_ffmpeg_set_log_callback(callback: extern "C" fn(*const c_char) -> i32);
        fn stt_ffmpeg_convert(
            input: *const c_char,
            output: *const c_char,
            codec: *const c_char,
            channels: i32,
            rate: i32,
            bitrate: i32,
            has_bitrate: i32,
            format: *const c_char,
            debug: i32,
            intervals: *const Interval,
            count: usize,
            enabled: i32,
            cancel: Cancel,
            cancel_context: *mut c_void,
            samples: Option<Samples>,
            samples_context: *mut c_void,
            source_rate: *mut i32,
            source_frames: *mut i64,
            error: *mut c_char,
            error_size: i32,
        ) -> i32;
    }
    extern "C" fn log_line(message: *const c_char) -> i32 {
        if message.is_null() {
            return 0;
        }
        std::panic::catch_unwind(|| {
            let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
            i32::from(crate::debug_log::forward(
                crate::debug_log::Category::Ffmpeg,
                &message,
            ))
        })
        .unwrap_or(0)
    }
    extern "C" fn canceled(context: *mut c_void) -> i32 {
        i32::from(unsafe { &*context.cast::<CancellationToken>() }.is_cancelled())
    }
    struct Analysis {
        vad: Vad,
        error: Option<ConvertError>,
    }
    extern "C" fn samples(context: *mut c_void, data: *const i16, count: i32) -> i32 {
        let state = unsafe { &mut *context.cast::<Analysis>() };
        // Never unwind across the C boundary, including a detector assertion.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state
                .vad
                .push(unsafe { std::slice::from_raw_parts(data, count as usize) })
        })) {
            Ok(Ok(())) => 0,
            Ok(Err(e)) => {
                state.error = Some(e);
                1
            }
            Err(_) => {
                state.error = Some(failed("VAD callback panicked"));
                1
            }
        }
    }
    fn failed(message: impl ToString) -> ConvertError {
        ConvertError::Failed {
            message: message.to_string(),
        }
    }
    fn string(value: impl AsRef<[u8]>) -> Result<CString, ConvertError> {
        CString::new(value.as_ref()).map_err(failed)
    }
    pub(super) fn prepare(
        token: &CancellationToken,
        config: &Config,
        input: &Path,
        output: &Path,
        source_rate: i32,
    ) -> Result<(), ConvertError> {
        // libav logging is global, including calls from codec worker threads.
        let _conversion = CONVERT_LOCK.lock();
        if token.is_cancelled() {
            return Err(ConvertError::Canceled);
        }
        LOG_INIT.call_once(|| unsafe { stt_ffmpeg_set_log_callback(log_line) });
        let settings = settings_for(config, source_rate)?;
        let input = string(input.to_string_lossy().as_bytes())?;
        let output = string(output.to_string_lossy().as_bytes())?;
        let codec = string(settings.ffmpeg_codec)?;
        let format = string(settings.sample_format)?;
        let mut error = [0 as c_char; 4096];
        let mut rate = 0;
        let mut frames = 0;
        let mut intervals = Vec::new();
        let token_context = token as *const CancellationToken as *mut c_void;
        let check = |result: i32, error: &[c_char]| {
            if token.is_cancelled() {
                Err(ConvertError::Canceled)
            } else if result < 0 {
                let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
                Err(failed(if message.is_empty() {
                    format!("libav failed: {result}")
                } else {
                    message.into_owned()
                }))
            } else {
                Ok(())
            }
        };
        if config.enable_vad {
            let start = std::time::Instant::now();
            let mut analysis = Analysis {
                vad: Vad::new(config.vad_start_threshold)
                    .map_err(|error| failed(error.to_string()))?,
                error: None,
            };
            let result = unsafe {
                stt_ffmpeg_convert(
                    input.as_ptr(),
                    std::ptr::null(),
                    codec.as_ptr(),
                    1,
                    16000,
                    0,
                    0,
                    format.as_ptr(),
                    i32::from(config.ffmpeg_debug),
                    std::ptr::null(),
                    0,
                    0,
                    canceled,
                    token_context,
                    Some(samples),
                    (&mut analysis as *mut Analysis).cast(),
                    &mut rate,
                    &mut frames,
                    error.as_mut_ptr(),
                    4096,
                )
            };
            if let Some(e) = analysis.error {
                return Err(e);
            }
            check(result, &error)?;
            let raw = analysis.vad.finish()?;
            let raw_count = raw.len();
            let normalized = prepare_intervals(
                raw,
                u32::try_from(rate).map_err(failed)?,
                u64::try_from(frames).map_err(failed)?,
                config.vad_padding_ms,
            )?;
            intervals.try_reserve(normalized.len()).map_err(failed)?;
            for i in normalized {
                intervals.push(Interval {
                    start_frame: i64::try_from(i.start_frame).map_err(failed)?,
                    end_frame: i64::try_from(i.end_frame).map_err(failed)?,
                });
            }
            if config.ffmpeg_debug {
                let kept: i64 = intervals.iter().map(|i| i.end_frame - i.start_frame).sum();
                crate::debug_log::write(
                    crate::debug_log::Category::Ffmpeg,
                    format_args!(
                        "[vad] rate={rate} duration={:.3}s raw_intervals={raw_count} intervals={} padding={}ms frames={frames} kept={kept} ratio={:.3} elapsed={:?}",
                        frames as f64 / f64::from(rate),
                        intervals.len(),
                        config.vad_padding_ms,
                        kept as f64 / (frames.max(1) as f64),
                        start.elapsed()
                    ),
                );
            }
            if intervals.is_empty() {
                return Err(ConvertError::NoSpeech);
            }
        }
        error.fill(0);
        let start = std::time::Instant::now();
        let result = unsafe {
            stt_ffmpeg_convert(
                input.as_ptr(),
                output.as_ptr(),
                codec.as_ptr(),
                settings.channels,
                settings.sample_rate,
                settings.bitrate,
                i32::from(settings.codec_has_bitrate),
                format.as_ptr(),
                i32::from(config.ffmpeg_debug),
                intervals.as_ptr(),
                intervals.len(),
                i32::from(config.enable_vad),
                canceled,
                token_context,
                None,
                std::ptr::null_mut(),
                &mut rate,
                &mut frames,
                error.as_mut_ptr(),
                4096,
            )
        };
        let result = check(result, &error);
        if result.is_err() {
            let _ = std::fs::remove_file(Path::new(output.to_str().map_err(failed)?));
        }
        if config.ffmpeg_debug {
            crate::debug_log::write(
                crate::debug_log::Category::Ffmpeg,
                format_args!("[ffmpeg] crop/transcode elapsed={:?}", start.elapsed()),
            );
        }
        result
    }
}
