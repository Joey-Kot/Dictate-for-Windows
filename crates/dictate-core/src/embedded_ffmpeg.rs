use crate::{
    Config,
    converter::{AudioConverter, ConvertError, SegmentAnalysis, SegmentExportRequest, paths_equal},
};
use async_trait::async_trait;
use std::path::Path;
#[cfg(feature = "static-libav")]
use std::path::PathBuf;
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

    async fn analyze_segments(
        &self,
        cancellation: &CancellationToken,
        config: &Config,
        input: &Path,
        min_pause_ms: u32,
    ) -> Result<SegmentAnalysis, ConvertError> {
        if cancellation.is_cancelled() {
            return Err(ConvertError::Canceled);
        }
        #[cfg(not(feature = "static-libav"))]
        {
            let _ = (config, input, min_pause_ms);
            Err(ConvertError::LibAvUnavailable)
        }
        #[cfg(feature = "static-libav")]
        {
            let (config, input, token) =
                (config.clone(), input.to_path_buf(), cancellation.clone());
            tokio::task::spawn_blocking(move || {
                native::analyze_segments(&token, &config, &input, min_pause_ms)
            })
            .await
            .map_err(|e| ConvertError::Failed {
                message: e.to_string(),
            })?
        }
    }

    async fn export_segments(
        &self,
        cancellation: &CancellationToken,
        request: SegmentExportRequest<'_>,
    ) -> Result<(), ConvertError> {
        if cancellation.is_cancelled() {
            return Err(ConvertError::Canceled);
        }
        if request
            .outputs
            .iter()
            .any(|output| paths_equal(request.input, output))
        {
            return Err(ConvertError::SamePath);
        }
        #[cfg(not(feature = "static-libav"))]
        {
            let _ = request;
            Err(ConvertError::LibAvUnavailable)
        }
        #[cfg(feature = "static-libav")]
        {
            let SegmentExportRequest {
                config,
                input,
                outputs,
                intervals,
                expected_source_rate,
                expected_source_frames,
            } = request;
            let (config, input, outputs, intervals, token) = (
                config.clone(),
                input.to_path_buf(),
                outputs.to_vec(),
                intervals.to_vec(),
                cancellation.clone(),
            );
            tokio::task::spawn_blocking(move || {
                native::export_segments(
                    &token,
                    &config,
                    &input,
                    &outputs,
                    &intervals,
                    expected_source_rate,
                    expected_source_frames,
                )
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
    use crate::{
        audio_intervals::prepare_intervals,
        converter::{SourceFrameInterval, settings_for},
        vad::Vad,
    };
    use std::ffi::{CStr, CString, c_char, c_void};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Interval {
        start_frame: i64,
        end_frame: i64,
    }
    type Cancel = extern "C" fn(*mut c_void) -> i32;
    type Samples = extern "C" fn(*mut c_void, *const i16, i32) -> i32;
    static CONVERT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    static LOG_INIT: std::sync::Once = std::sync::Once::new();
    unsafe extern "C" {
        fn dictate_ffmpeg_set_log_callback(callback: extern "C" fn(*const c_char) -> i32);
        fn dictate_ffmpeg_convert(
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
        fn dictate_ffmpeg_analyze_silence(
            input: *const c_char,
            min_pause_ms: i64,
            debug: i32,
            cancel: Cancel,
            cancel_context: *mut c_void,
            intervals: *mut *mut Interval,
            interval_count: *mut usize,
            source_rate: *mut i32,
            source_frames: *mut i64,
            error: *mut c_char,
            error_size: i32,
        ) -> i32;
        fn dictate_ffmpeg_free_intervals(intervals: *mut Interval);
        fn dictate_ffmpeg_export_segments(
            input: *const c_char,
            output_paths: *const *const c_char,
            intervals: *const Interval,
            interval_count: usize,
            codec: *const c_char,
            channels: i32,
            rate: i32,
            bitrate: i32,
            has_bitrate: i32,
            format: *const c_char,
            expected_source_rate: i32,
            expected_source_frames: i64,
            debug: i32,
            cancel: Cancel,
            cancel_context: *mut c_void,
            error: *mut c_char,
            error_size: i32,
        ) -> i32;
    }

    struct NativeIntervals(*mut Interval);

    impl Drop for NativeIntervals {
        fn drop(&mut self) {
            unsafe { dictate_ffmpeg_free_intervals(self.0) };
        }
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

    fn check_native_result(
        token: &CancellationToken,
        result: i32,
        error: &[c_char],
    ) -> Result<(), ConvertError> {
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
    }

    fn validate_segment_export(
        outputs: &[PathBuf],
        intervals: &[SourceFrameInterval],
        expected_source_rate: u32,
        expected_source_frames: u64,
    ) -> Result<(), ConvertError> {
        if expected_source_rate == 0
            || expected_source_frames == 0
            || outputs.is_empty()
            || outputs.len() != intervals.len()
        {
            return Err(failed("invalid segmented export plan"));
        }
        let mut paths = std::collections::HashSet::with_capacity(outputs.len());
        for output in outputs {
            if output.exists() {
                return Err(failed("segmented output path already exists"));
            }
            // Windows paths are case-insensitive. Rejecting the same spelling
            // on other hosts as well keeps a batch from overwriting itself.
            if !paths.insert(output.to_string_lossy().to_ascii_lowercase()) {
                return Err(failed("segmented output paths must be unique"));
            }
        }
        let mut cursor = 0_u64;
        for interval in intervals {
            if interval.start_frame != cursor
                || interval.end_frame <= interval.start_frame
                || interval.end_frame > expected_source_frames
            {
                return Err(failed(
                    "segmented export plan must be a contiguous source-frame partition",
                ));
            }
            cursor = interval.end_frame;
        }
        if cursor != expected_source_frames {
            return Err(failed(
                "segmented export plan does not cover the source frames",
            ));
        }
        Ok(())
    }

    pub(super) fn analyze_segments(
        token: &CancellationToken,
        config: &Config,
        input: &Path,
        min_pause_ms: u32,
    ) -> Result<SegmentAnalysis, ConvertError> {
        // libav logging is global, including calls from codec worker threads.
        let _conversion = CONVERT_LOCK.lock();
        if token.is_cancelled() {
            return Err(ConvertError::Canceled);
        }
        LOG_INIT.call_once(|| unsafe { dictate_ffmpeg_set_log_callback(log_line) });
        let input = string(input.to_string_lossy().as_bytes())?;
        let token_context = token as *const CancellationToken as *mut c_void;
        let mut error = [0 as c_char; 4096];
        let mut native_intervals = std::ptr::null_mut();
        let mut interval_count = 0_usize;
        let mut source_rate = 0_i32;
        let mut source_frames = 0_i64;
        let started = std::time::Instant::now();
        let result = unsafe {
            dictate_ffmpeg_analyze_silence(
                input.as_ptr(),
                i64::from(min_pause_ms),
                i32::from(config.ffmpeg_debug),
                canceled,
                token_context,
                &mut native_intervals,
                &mut interval_count,
                &mut source_rate,
                &mut source_frames,
                error.as_mut_ptr(),
                4096,
            )
        };
        let native_intervals = NativeIntervals(native_intervals);
        check_native_result(token, result, &error)?;
        let source_rate = u32::try_from(source_rate).map_err(failed)?;
        let source_frames = u64::try_from(source_frames).map_err(failed)?;
        let raw = if interval_count == 0 {
            &[][..]
        } else {
            if native_intervals.0.is_null() {
                return Err(failed("libav returned a null silence interval list"));
            }
            unsafe { std::slice::from_raw_parts(native_intervals.0, interval_count) }
        };
        let mut silence_intervals = Vec::new();
        silence_intervals.try_reserve(raw.len()).map_err(failed)?;
        let mut previous_end = 0_u64;
        for interval in raw {
            let start_frame = u64::try_from(interval.start_frame).map_err(failed)?;
            let end_frame = u64::try_from(interval.end_frame).map_err(failed)?;
            if end_frame <= start_frame || start_frame < previous_end || end_frame > source_frames {
                return Err(failed(
                    "libav returned invalid silence source-frame intervals",
                ));
            }
            silence_intervals.push(SourceFrameInterval {
                start_frame,
                end_frame,
            });
            previous_end = end_frame;
        }
        if config.ffmpeg_debug {
            crate::debug_log::write(
                crate::debug_log::Category::Ffmpeg,
                format_args!(
                    "[segmented-upload] analyzed source rate={source_rate} frames={source_frames} pauses={} min_pause={}ms elapsed={:?}",
                    silence_intervals.len(),
                    min_pause_ms.max(1),
                    started.elapsed(),
                ),
            );
        }
        Ok(SegmentAnalysis {
            source_rate,
            source_frames,
            silence_intervals,
        })
    }

    pub(super) fn export_segments(
        token: &CancellationToken,
        config: &Config,
        input: &Path,
        outputs: &[PathBuf],
        intervals: &[SourceFrameInterval],
        expected_source_rate: u32,
        expected_source_frames: u64,
    ) -> Result<(), ConvertError> {
        // A single source decode drives every segment, so this lock also keeps
        // libav's process-global logging configuration deterministic.
        let _conversion = CONVERT_LOCK.lock();
        if token.is_cancelled() {
            return Err(ConvertError::Canceled);
        }
        validate_segment_export(
            outputs,
            intervals,
            expected_source_rate,
            expected_source_frames,
        )?;
        LOG_INIT.call_once(|| unsafe { dictate_ffmpeg_set_log_callback(log_line) });
        let source_rate = i32::try_from(expected_source_rate).map_err(failed)?;
        let source_frames = i64::try_from(expected_source_frames).map_err(failed)?;
        let settings = settings_for(config, source_rate)?;
        let input = string(input.to_string_lossy().as_bytes())?;
        let codec = string(settings.ffmpeg_codec)?;
        let format = string(settings.sample_format)?;
        let mut output_strings = Vec::new();
        output_strings.try_reserve(outputs.len()).map_err(failed)?;
        for output in outputs {
            output_strings.push(string(output.to_string_lossy().as_bytes())?);
        }
        let output_paths: Vec<*const c_char> =
            output_strings.iter().map(|path| path.as_ptr()).collect();
        let mut native_intervals = Vec::new();
        native_intervals
            .try_reserve(intervals.len())
            .map_err(failed)?;
        for interval in intervals {
            native_intervals.push(Interval {
                start_frame: i64::try_from(interval.start_frame).map_err(failed)?,
                end_frame: i64::try_from(interval.end_frame).map_err(failed)?,
            });
        }
        let token_context = token as *const CancellationToken as *mut c_void;
        let mut error = [0 as c_char; 4096];
        let started = std::time::Instant::now();
        let result = unsafe {
            dictate_ffmpeg_export_segments(
                input.as_ptr(),
                output_paths.as_ptr(),
                native_intervals.as_ptr(),
                native_intervals.len(),
                codec.as_ptr(),
                settings.channels,
                settings.sample_rate,
                settings.bitrate,
                i32::from(settings.codec_has_bitrate),
                format.as_ptr(),
                source_rate,
                source_frames,
                i32::from(config.ffmpeg_debug),
                canceled,
                token_context,
                error.as_mut_ptr(),
                4096,
            )
        };
        let result = check_native_result(token, result, &error);
        if result.is_err() {
            for output in outputs {
                let _ = std::fs::remove_file(output);
            }
        }
        if config.ffmpeg_debug {
            crate::debug_log::write(
                crate::debug_log::Category::Ffmpeg,
                format_args!(
                    "[segmented-upload] exported segments={} elapsed={:?}",
                    outputs.len(),
                    started.elapsed(),
                ),
            );
        }
        result
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
        LOG_INIT.call_once(|| unsafe { dictate_ffmpeg_set_log_callback(log_line) });
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
                dictate_ffmpeg_convert(
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
            dictate_ffmpeg_convert(
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
