//! Preparation of complete-audio segmented upload artifacts.
//!
//! This module deliberately sits between audio conversion and protocol
//! execution.  It knows how to analyze and export a stable source-frame plan,
//! but it does not know whether the eventual client is Legacy or Advanced.

use std::path::{Path, PathBuf};

use tokio_util::sync::CancellationToken;

use crate::Config;
use crate::audio_segments::{
    SegmentPlan, SegmentPlanError, SegmentPlanOutcome, SourceInterval, build_segment_plan,
};
use crate::cache;
use crate::converter::{AudioConverter, ConvertError, SourceFrameInterval};

/// Files for one generated upload batch.
///
/// The caller owns cleanup of `directory` after the network batch reaches a
/// terminal state.  Keeping all generated files in that one directory makes
/// cancellation and failed-attempt cleanup atomic at the directory level.
#[derive(Debug)]
pub(crate) struct PreparedSegmentedUpload {
    pub(crate) directory: PathBuf,
    pub(crate) files: Vec<PathBuf>,
}

impl PreparedSegmentedUpload {
    pub(crate) fn remove_files(&self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// Analyze (unless a retry supplies a frozen plan) and export independent,
/// complete media files for every plan range.
///
/// `temporary_root` must be a runtime-owned scratch location.  The source is
/// never removed here: interactive Runtime and --file callers have different
/// ownership rules for it.
pub(crate) async fn prepare_segmented_upload(
    converter: &dyn AudioConverter,
    cancellation: &CancellationToken,
    config: &Config,
    input: &Path,
    temporary_root: &Path,
    frozen_plan: Option<&SegmentPlan>,
    on_new_plan: Option<&(dyn Fn(&SegmentPlan) + Send + Sync)>,
) -> Result<PreparedSegmentedUpload, ConvertError> {
    if cancellation.is_cancelled() {
        return Err(ConvertError::Canceled);
    }

    let directory = cache::temporary_attempt_directory(temporary_root).map_err(|error| {
        ConvertError::Failed {
            message: format!("could not create segmented upload workspace: {error}"),
        }
    })?;
    let result = prepare_segmented_upload_in_directory(
        converter,
        cancellation,
        config,
        input,
        &directory,
        frozen_plan,
        on_new_plan,
    )
    .await;
    match result {
        Ok(files) => Ok(PreparedSegmentedUpload { directory, files }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&directory);
            Err(error)
        }
    }
}

async fn prepare_segmented_upload_in_directory(
    converter: &dyn AudioConverter,
    cancellation: &CancellationToken,
    config: &Config,
    input: &Path,
    directory: &Path,
    frozen_plan: Option<&SegmentPlan>,
    on_new_plan: Option<&(dyn Fn(&SegmentPlan) + Send + Sync)>,
) -> Result<Vec<PathBuf>, ConvertError> {
    let plan = match frozen_plan {
        Some(plan) => {
            plan.validate().map_err(invalid_frozen_plan)?;
            plan.clone()
        }
        None => {
            let analysis = converter
                .analyze_segments(cancellation, config, input, config.min_upload_pause_ms)
                .await?;
            let max_segment_frames = u64::from(config.max_upload_segment_seconds)
                .checked_mul(u64::from(analysis.source_rate))
                .ok_or_else(|| ConvertError::Failed {
                    message: "maximum segmented upload duration is too large".into(),
                })?;
            let mut silences = Vec::new();
            silences
                .try_reserve(analysis.silence_intervals.len())
                .map_err(|_| ConvertError::Failed {
                    message: "could not allocate segmented upload plan".into(),
                })?;
            for interval in analysis.silence_intervals {
                silences.push(SourceInterval::new(
                    interval.start_frame,
                    interval.end_frame,
                ));
            }
            match build_segment_plan(
                analysis.source_rate,
                analysis.source_frames,
                max_segment_frames,
                &silences,
            )
            .map_err(|error| ConvertError::Failed {
                message: format!("could not build segmented upload plan: {error}"),
            })? {
                SegmentPlanOutcome::NoSpeech => return Err(ConvertError::NoSpeech),
                SegmentPlanOutcome::Plan(plan) => {
                    // A retry needs stable source-frame boundaries even when
                    // exporting this newly planned batch fails.  Notify the
                    // caller before the first await below, after every plan
                    // invariant has been checked by the planner.
                    if let Some(on_new_plan) = on_new_plan {
                        on_new_plan(&plan);
                    }
                    plan
                }
            }
        }
    };
    if cancellation.is_cancelled() {
        return Err(ConvertError::Canceled);
    }

    let mut files = Vec::new();
    let mut intervals = Vec::new();
    files
        .try_reserve(plan.segments.len())
        .map_err(|_| ConvertError::Failed {
            message: "could not allocate segmented upload output list".into(),
        })?;
    intervals
        .try_reserve(plan.segments.len())
        .map_err(|_| ConvertError::Failed {
            message: "could not allocate segmented upload interval list".into(),
        })?;
    let extension = config.container_extension();
    for (index, segment) in plan.segments.iter().enumerate() {
        files.push(directory.join(format!("segment-{index:06}.{extension}")));
        intervals.push(SourceFrameInterval {
            start_frame: segment.start_frame,
            end_frame: segment.end_frame,
        });
    }

    // Segment planning uses silence only for choosing boundaries.  The
    // exporter must preserve every source frame, so the old Earshot VAD
    // trimming branch is deliberately disabled for this conversion.
    let mut export_config = config.clone();
    export_config.enable_vad = false;
    converter
        .export_segments(
            cancellation,
            &export_config,
            input,
            &files,
            &intervals,
            plan.source_rate,
            plan.total_frames,
        )
        .await?;
    Ok(files)
}

fn invalid_frozen_plan(error: SegmentPlanError) -> ConvertError {
    ConvertError::Failed {
        message: format!("invalid frozen segmented upload plan: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use parking_lot::Mutex;

    use super::*;
    use crate::converter::SegmentAnalysis;

    #[derive(Default)]
    struct Calls {
        analyses: usize,
        export_enable_vad: Vec<bool>,
        exports: Vec<Vec<SourceFrameInterval>>,
    }

    struct TestConverter {
        analysis: SegmentAnalysis,
        calls: Arc<Mutex<Calls>>,
        export_error: bool,
    }

    #[async_trait]
    impl AudioConverter for TestConverter {
        async fn convert(
            &self,
            _: &CancellationToken,
            _: &Config,
            _: &Path,
            _: &Path,
            _: i32,
        ) -> Result<(), ConvertError> {
            unreachable!("segmented preparation must not call single-file conversion")
        }

        async fn analyze_segments(
            &self,
            _: &CancellationToken,
            _: &Config,
            _: &Path,
            _: u32,
        ) -> Result<SegmentAnalysis, ConvertError> {
            self.calls.lock().analyses += 1;
            Ok(self.analysis.clone())
        }

        async fn export_segments(
            &self,
            _: &CancellationToken,
            config: &Config,
            _: &Path,
            outputs: &[PathBuf],
            intervals: &[SourceFrameInterval],
            _: u32,
            _: u64,
        ) -> Result<(), ConvertError> {
            let mut calls = self.calls.lock();
            calls.export_enable_vad.push(config.enable_vad);
            calls.exports.push(intervals.to_vec());
            drop(calls);
            if self.export_error {
                return Err(ConvertError::Failed {
                    message: "simulated segment export failure".into(),
                });
            }
            for output in outputs {
                std::fs::write(output, b"segment").map_err(|error| ConvertError::Failed {
                    message: error.to_string(),
                })?;
            }
            Ok(())
        }
    }

    fn config() -> Config {
        Config {
            enable_segmented_upload: true,
            enable_vad: true,
            container: "wav".into(),
            max_upload_segment_seconds: 1,
            min_upload_pause_ms: 700,
            max_upload_concurrency: 1,
            ..Config::default()
        }
    }

    fn converter(calls: Arc<Mutex<Calls>>) -> TestConverter {
        TestConverter {
            analysis: SegmentAnalysis {
                source_rate: 10,
                source_frames: 25,
                silence_intervals: vec![SourceFrameInterval {
                    start_frame: 8,
                    end_frame: 10,
                }],
            },
            calls,
            export_error: false,
        }
    }

    #[tokio::test]
    async fn preparation_preserves_every_frame_disables_vad_and_reuses_the_frozen_plan() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("source.wav");
        std::fs::write(&input, b"source").unwrap();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let converter = converter(calls.clone());
        let cancellation = CancellationToken::new();
        let planned = Arc::new(Mutex::new(None));
        let observer = {
            let planned = planned.clone();
            move |plan: &SegmentPlan| *planned.lock() = Some(plan.clone())
        };

        let prepared = prepare_segmented_upload(
            &converter,
            &cancellation,
            &config(),
            &input,
            root.path(),
            None,
            Some(&observer),
        )
        .await
        .unwrap();
        assert_eq!(prepared.files.len(), 3);
        let plan = planned.lock().clone().expect("new plan must be reported");
        assert_eq!(
            plan.segments,
            vec![
                SourceInterval::new(0, 10),
                SourceInterval::new(10, 20),
                SourceInterval::new(20, 25),
            ]
        );
        assert!(prepared.files.iter().all(|path| path.is_file()));
        assert_eq!(calls.lock().analyses, 1);
        assert_eq!(calls.lock().export_enable_vad, vec![false]);
        prepared.remove_files();

        let mut changed = config();
        changed.max_upload_segment_seconds = 2;
        changed.min_upload_pause_ms = 1;
        changed.max_upload_concurrency = 4;
        let retried = prepare_segmented_upload(
            &converter,
            &cancellation,
            &changed,
            &input,
            root.path(),
            Some(&plan),
            None,
        )
        .await
        .unwrap();
        assert_eq!(calls.lock().analyses, 1);
        assert_eq!(
            calls.lock().exports,
            vec![
                vec![
                    SourceFrameInterval {
                        start_frame: 0,
                        end_frame: 10,
                    },
                    SourceFrameInterval {
                        start_frame: 10,
                        end_frame: 20,
                    },
                    SourceFrameInterval {
                        start_frame: 20,
                        end_frame: 25,
                    },
                ],
                vec![
                    SourceFrameInterval {
                        start_frame: 0,
                        end_frame: 10,
                    },
                    SourceFrameInterval {
                        start_frame: 10,
                        end_frame: 20,
                    },
                    SourceFrameInterval {
                        start_frame: 20,
                        end_frame: 25,
                    },
                ],
            ]
        );
        assert_eq!(calls.lock().export_enable_vad, vec![false, false]);
        retried.remove_files();
    }

    #[tokio::test]
    async fn full_silence_returns_no_speech_and_removes_its_attempt_directory() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("source.wav");
        std::fs::write(&input, b"source").unwrap();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let converter = TestConverter {
            analysis: SegmentAnalysis {
                source_rate: 10,
                source_frames: 25,
                silence_intervals: vec![SourceFrameInterval {
                    start_frame: 0,
                    end_frame: 25,
                }],
            },
            calls: calls.clone(),
            export_error: false,
        };

        let error = prepare_segmented_upload(
            &converter,
            &CancellationToken::new(),
            &config(),
            &input,
            root.path(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ConvertError::NoSpeech));
        assert_eq!(calls.lock().analyses, 1);
        assert!(
            std::fs::read_dir(root.path())
                .unwrap()
                .all(|entry| entry.unwrap().path() == input)
        );
    }

    #[tokio::test]
    async fn newly_built_plan_is_reported_before_export_failure() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("source.wav");
        std::fs::write(&input, b"source").unwrap();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let converter = TestConverter {
            analysis: SegmentAnalysis {
                source_rate: 10,
                source_frames: 25,
                silence_intervals: vec![SourceFrameInterval {
                    start_frame: 8,
                    end_frame: 10,
                }],
            },
            calls,
            export_error: true,
        };
        let reported = Arc::new(Mutex::new(Vec::new()));
        let observer = {
            let reported = reported.clone();
            move |plan: &SegmentPlan| reported.lock().push(plan.clone())
        };

        let error = prepare_segmented_upload(
            &converter,
            &CancellationToken::new(),
            &config(),
            &input,
            root.path(),
            None,
            Some(&observer),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ConvertError::Failed { .. }));
        assert_eq!(reported.lock().len(), 1);
        assert!(
            std::fs::read_dir(root.path())
                .unwrap()
                .all(|entry| entry.unwrap().path() == input)
        );
    }
}
