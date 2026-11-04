//! Caller-owned file transcription shared by CLI file mode and GUI batches.
//! Initialization never sweeps the scratch root: other files may still be running.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::asr::{AsrError, Transcription};
use crate::audio_api::{AudioApiClient, UploadLimiter};
use crate::cache;
use crate::converter::{AudioConverter, ConvertError, prepare_audio_for_upload};
use crate::runtime::RuntimeError;
use crate::segmented_upload::prepare_segmented_upload;

#[derive(Clone, Copy)]
pub(crate) enum OutputPolicy {
    Overwrite,
    Unique,
}

pub(crate) struct FileTranscriber {
    config: Config,
    converter: Arc<dyn AudioConverter>,
    client: AudioApiClient,
    scratch_root: PathBuf,
    limiter: Option<UploadLimiter>,
}

impl FileTranscriber {
    pub(crate) fn new(
        config: Config,
        converter: Arc<dyn AudioConverter>,
        scratch_root: PathBuf,
        limiter: Option<UploadLimiter>,
    ) -> Result<Self, RuntimeError> {
        let client = AudioApiClient::new(config.clone())?;
        Ok(Self {
            config,
            converter,
            client,
            scratch_root,
            limiter,
        })
    }

    pub(crate) async fn transcribe(
        &self,
        input: &Path,
        output: &Path,
        cancellation: &CancellationToken,
        policy: OutputPolicy,
    ) -> Result<PathBuf, RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(ConvertError::Canceled.into());
        }
        let metadata = std::fs::metadata(input).map_err(|source| RuntimeError::InputFile {
            path: input.to_owned(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(RuntimeError::InputFile {
                path: input.to_owned(),
                source: std::io::Error::other("not a regular file"),
            });
        }
        let _operation = crate::debug_log::operation_scope_if_none("file");
        let workspace = tempfile::Builder::new()
            .prefix("RecordTemp_")
            .tempdir_in(&self.scratch_root)
            .map_err(|source| RuntimeError::InputFile {
                path: self.scratch_root.clone(),
                source,
            })?;
        if self.config.enable_segmented_upload && !self.client.is_realtime_workflow() {
            return self
                .segmented(input, output, workspace.path(), cancellation, policy)
                .await;
        }
        let config = file_mode_preparation_config(
            &self.config,
            self.client.realtime_audio_stream().as_ref(),
        );
        let temporary = workspace
            .path()
            .join(format!("prepared.{}", config.container_extension()));
        prepare_audio_for_upload(
            self.converter.as_ref(),
            cancellation,
            &config,
            input,
            &temporary,
            config.sampling_rate,
        )
        .await?;
        let _permit = match &self.limiter {
            Some(limiter) => Some(limiter.acquire(cancellation).await?),
            None => None,
        };
        let transcription = self
            .client
            .transcribe_with_retry_prepare(cancellation, &temporary, || async {
                if !config.enable_vad {
                    return Ok(());
                }
                prepare_audio_for_upload(
                    self.converter.as_ref(),
                    cancellation,
                    &config,
                    input,
                    &temporary,
                    config.sampling_rate,
                )
                .await
                .map_err(AsrError::from)
            })
            .await;
        drop(_permit);
        let transcription = match transcription {
            Ok(value) => value,
            Err(error) => {
                if !cancellation.is_cancelled() && !error.is_canceled() {
                    cache::handle_cache(
                        &self.config,
                        None,
                        Some(&temporary),
                        error.is_text_extraction_error(),
                        error.last_response(),
                    );
                }
                return Err(error.into());
            }
        };
        finish_file_mode_output(
            &self.config,
            &temporary,
            output.to_owned(),
            transcription,
            cancellation,
            policy,
        )
    }

    async fn segmented(
        &self,
        input: &Path,
        output: &Path,
        scratch: &Path,
        cancellation: &CancellationToken,
        policy: OutputPolicy,
    ) -> Result<PathBuf, RuntimeError> {
        let prepared = prepare_segmented_upload(
            self.converter.as_ref(),
            cancellation,
            &self.config,
            input,
            scratch,
            None,
            None,
        )
        .await?;
        let concurrency = self.config.max_upload_concurrency as usize;
        let result = match &self.limiter {
            Some(limiter) => {
                self.client
                    .transcribe_segments_strict_limited(
                        cancellation,
                        &prepared.files,
                        concurrency,
                        limiter,
                    )
                    .await
            }
            None => {
                self.client
                    .transcribe_segments_strict(cancellation, &prepared.files, concurrency)
                    .await
            }
        };
        let transcription = match result {
            Ok(value) => value,
            Err(error) => {
                prepared.remove_files();
                return Err(error.into());
            }
        };
        let result = write_transcript(output, &transcription.text, cancellation, policy);
        cache::handle_segmented_cache(&self.config, None, &prepared.directory, result.is_ok());
        result
    }
}

pub(crate) fn file_mode_preparation_config(
    config: &Config,
    realtime_stream: Option<&crate::advanced_audio::schema::RealtimeAudioStream>,
) -> Config {
    let Some(stream) = realtime_stream else {
        return config.clone();
    };
    let mut preparation = config.clone();
    preparation.codecs = "pcm".into();
    preparation.container = "wav".into();
    preparation.channels = i32::from(stream.channels);
    preparation.sampling_rate = stream.sample_rate as i32;
    preparation.sampling_rate_depth = 16;
    preparation
}

pub(crate) fn finish_file_mode_output(
    config: &Config,
    temporary: &Path,
    output: PathBuf,
    transcription: Transcription,
    cancellation: &CancellationToken,
    policy: OutputPolicy,
) -> Result<PathBuf, RuntimeError> {
    let result = write_transcript(&output, &transcription.text, cancellation, policy);
    if cancellation.is_cancelled() {
        let _ = std::fs::remove_file(temporary);
    } else {
        cache::handle_cache(
            config,
            None,
            Some(temporary),
            true,
            &transcription.raw_response,
        );
    }
    result
}

fn write_transcript(
    output: &Path,
    text: &str,
    cancellation: &CancellationToken,
    policy: OutputPolicy,
) -> Result<PathBuf, RuntimeError> {
    let fail = |source| RuntimeError::OutputFile {
        path: output.to_owned(),
        source,
    };
    if cancellation.is_cancelled() {
        return Err(ConvertError::Canceled.into());
    }
    if matches!(policy, OutputPolicy::Overwrite) {
        std::fs::write(output, text).map_err(fail)?;
        return Ok(output.to_owned());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(fail)?;
    temporary.write_all(text.as_bytes()).map_err(fail)?;
    temporary.as_file().sync_all().map_err(fail)?;
    let mut suffix = 1_u64;
    loop {
        if cancellation.is_cancelled() {
            return Err(ConvertError::Canceled.into());
        }
        let target = if suffix == 1 {
            output.to_owned()
        } else {
            let mut name = OsString::from(output.file_stem().unwrap_or_default());
            name.push(format!(" ({suffix}).txt"));
            parent.join(name)
        };
        match temporary.persist_noclobber(&target) {
            Ok(_) => return Ok(target),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                temporary = error.file;
                suffix += 1;
            }
            Err(error) => return Err(fail(error.error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_outputs_preserve_existing_text_and_cancellation_leaves_no_partial_file() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("会议.txt");
        std::fs::write(&output, "existing").unwrap();
        let token = CancellationToken::new();
        let second = write_transcript(&output, "new", &token, OutputPolicy::Unique).unwrap();
        assert_eq!(second.file_name().unwrap(), "会议 (2).txt");
        assert_eq!(std::fs::read_to_string(output).unwrap(), "existing");
        token.cancel();
        assert!(write_transcript(&second, "canceled", &token, OutputPolicy::Unique).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
