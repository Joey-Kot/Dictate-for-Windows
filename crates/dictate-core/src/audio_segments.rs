//! Non-destructive planning for bounded, complete-audio uploads.
//!
//! The silence detector supplies source-frame intervals that have already met
//! the caller's minimum-pause requirement.  This module deliberately does not
//! inspect samples, apply VAD padding, or remove silence.  It only turns those
//! candidate pauses into a stable plan whose half-open ranges cover every
//! source frame exactly once.

use thiserror::Error;

/// A half-open range measured in frames per channel on the original audio
/// timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceInterval {
    pub(crate) start_frame: u64,
    pub(crate) end_frame: u64,
}

impl SourceInterval {
    pub(crate) const fn new(start_frame: u64, end_frame: u64) -> Self {
        Self {
            start_frame,
            end_frame,
        }
    }

    pub(crate) const fn len(self) -> u64 {
        self.end_frame.saturating_sub(self.start_frame)
    }
}

/// A frozen partition of one original recording.
///
/// `source_rate` is carried with the plan so a retry can validate and export
/// the same source-frame boundaries without re-running silence analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentPlan {
    pub(crate) source_rate: u32,
    pub(crate) total_frames: u64,
    /// The strict source-frame upper bound used when this plan was created.
    /// It is part of the retry contract: later setting changes must not make
    /// an already-recorded plan invalid or silently change its boundaries.
    pub(crate) max_segment_frames: u64,
    pub(crate) segments: Vec<SourceInterval>,
}

impl SegmentPlan {
    /// Verify the invariants expected by a later exporter or retry path.
    pub(crate) fn validate(&self) -> Result<(), SegmentPlanError> {
        if self.source_rate == 0 {
            return Err(SegmentPlanError::InvalidSourceRate);
        }
        if self.max_segment_frames == 0 {
            return Err(SegmentPlanError::InvalidMaximumLength);
        }
        if self.total_frames == 0 || self.segments.is_empty() {
            return Err(SegmentPlanError::InvalidPlan);
        }

        let mut cursor = 0;
        for segment in &self.segments {
            if segment.start_frame != cursor
                || segment.end_frame <= segment.start_frame
                || segment.len() > self.max_segment_frames
            {
                return Err(SegmentPlanError::InvalidPlan);
            }
            cursor = segment.end_frame;
        }
        if cursor != self.total_frames {
            return Err(SegmentPlanError::InvalidPlan);
        }
        Ok(())
    }
}

/// Whether the normalized silence input described a completely silent source
/// or yielded a plan that can be exported and uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentPlanOutcome {
    /// The source has no frames, or qualifying silence covers `[0, total)`.
    NoSpeech,
    /// Every source frame belongs to exactly one output segment.
    Plan(SegmentPlan),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum SegmentPlanError {
    #[error("source sample rate must be positive")]
    InvalidSourceRate,
    #[error("maximum segment length must be positive")]
    InvalidMaximumLength,
    #[error("segment plan allocation failed")]
    AllocationFailed,
    #[error("segment plan is not a contiguous source-audio partition")]
    InvalidPlan,
}

/// Builds a continuous upload plan from already-qualified source-frame
/// silences.
///
/// The algorithm follows the split-mode max-span behavior while preserving
/// every original frame:
///
/// * malformed, overlapping, adjacent, and out-of-source silence reports are
///   normalized before planning;
/// * if normalized silence covers the complete source, no requestable segment
///   exists and [`SegmentPlanOutcome::NoSpeech`] is returned;
/// * before each strict maximum-length limit, the latest silence that starts
///   after the current cursor is preferred, with its *end* as the boundary;
/// * a pause that crosses the strict limit is cut at the limit;
/// * a leading or already-in-progress silence is never used to create a short
///   artificial segment; when necessary, ordinary maximum-length hard cuts
///   split it instead;
/// * when no usable pause precedes the limit, the algorithm hard-cuts exactly
///   at the limit.
///
/// `silence_intervals` must already be filtered by the configured minimum
/// pause duration.  This planner intentionally has no VAD or threshold policy.
pub(crate) fn build_segment_plan(
    source_rate: u32,
    total_frames: u64,
    max_segment_frames: u64,
    silence_intervals: &[SourceInterval],
) -> Result<SegmentPlanOutcome, SegmentPlanError> {
    if source_rate == 0 {
        return Err(SegmentPlanError::InvalidSourceRate);
    }
    if max_segment_frames == 0 {
        return Err(SegmentPlanError::InvalidMaximumLength);
    }
    if total_frames == 0 {
        return Ok(SegmentPlanOutcome::NoSpeech);
    }

    let silences = normalize_silences(total_frames, silence_intervals)?;
    if source_is_fully_silent(total_frames, &silences) {
        return Ok(SegmentPlanOutcome::NoSpeech);
    }

    let mut segments = Vec::new();
    let mut cursor = 0;
    while cursor < total_frames {
        let limit = cursor.saturating_add(max_segment_frames).min(total_frames);
        let end = if limit == total_frames {
            // A final range that already fits must not be split merely because
            // it contains an otherwise usable pause.  This matches split
            // mode's max-span grouping behavior.
            total_frames
        } else {
            choose_break(cursor, limit, &silences).unwrap_or(limit)
        };

        debug_assert!(cursor < end && end <= limit);
        segments
            .try_reserve(1)
            .map_err(|_| SegmentPlanError::AllocationFailed)?;
        segments.push(SourceInterval::new(cursor, end));
        cursor = end;
    }

    let plan = SegmentPlan {
        source_rate,
        total_frames,
        max_segment_frames,
        segments,
    };
    plan.validate()?;
    Ok(SegmentPlanOutcome::Plan(plan))
}

fn normalize_silences(
    total_frames: u64,
    silence_intervals: &[SourceInterval],
) -> Result<Vec<SourceInterval>, SegmentPlanError> {
    let mut normalized = Vec::new();
    normalized
        .try_reserve(silence_intervals.len())
        .map_err(|_| SegmentPlanError::AllocationFailed)?;
    for interval in silence_intervals {
        let start_frame = interval.start_frame.min(total_frames);
        let end_frame = interval.end_frame.min(total_frames);
        if start_frame < end_frame {
            normalized.push(SourceInterval::new(start_frame, end_frame));
        }
    }
    normalized.sort_unstable_by_key(|interval| (interval.start_frame, interval.end_frame));

    let mut merged: Vec<SourceInterval> = Vec::new();
    merged
        .try_reserve(normalized.len())
        .map_err(|_| SegmentPlanError::AllocationFailed)?;
    for interval in normalized {
        if let Some(previous) = merged.last_mut()
            && interval.start_frame <= previous.end_frame
        {
            previous.end_frame = previous.end_frame.max(interval.end_frame);
        } else {
            merged.push(interval);
        }
    }
    Ok(merged)
}

fn source_is_fully_silent(total_frames: u64, silences: &[SourceInterval]) -> bool {
    total_frames == 0
        || matches!(
            silences,
            [SourceInterval {
                start_frame: 0,
                end_frame,
            }] if *end_frame == total_frames
        )
}

/// Chooses the latest natural boundary for one non-final segment.
///
/// A silence that began on or before `cursor` is intentionally ignored.  It
/// is either leading silence or the remainder of the pause used by the
/// previous segment; choosing it would create a short silence-only segment.
fn choose_break(cursor: u64, limit: u64, silences: &[SourceInterval]) -> Option<u64> {
    let mut selected = None;
    for silence in silences {
        if silence.end_frame <= cursor {
            continue;
        }
        if silence.start_frame <= cursor {
            continue;
        }
        if silence.start_frame > limit {
            break;
        }

        if silence.end_frame >= limit {
            // The strict limit itself is inside the pause (or at its end), so
            // it is both the closest valid boundary and the hard upper bound.
            return Some(limit);
        }
        selected = Some(silence.end_frame);
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_RATE: u32 = 48_000;

    fn interval(start_frame: u64, end_frame: u64) -> SourceInterval {
        SourceInterval::new(start_frame, end_frame)
    }

    fn plan(
        total_frames: u64,
        max_segment_frames: u64,
        silence_intervals: &[SourceInterval],
    ) -> SegmentPlan {
        match build_segment_plan(
            SOURCE_RATE,
            total_frames,
            max_segment_frames,
            silence_intervals,
        )
        .unwrap()
        {
            SegmentPlanOutcome::Plan(plan) => plan,
            SegmentPlanOutcome::NoSpeech => panic!("expected a requestable segment plan"),
        }
    }

    fn assert_segments(plan: &SegmentPlan, expected: &[(u64, u64)], max_segment_frames: u64) {
        assert_eq!(plan.source_rate, SOURCE_RATE);
        assert_eq!(plan.max_segment_frames, max_segment_frames);
        assert_eq!(
            plan.segments,
            expected
                .iter()
                .map(|&(start, end)| interval(start, end))
                .collect::<Vec<_>>()
        );
        plan.validate().unwrap();
    }

    #[test]
    fn rejects_invalid_rate_and_maximum_length() {
        assert_eq!(
            build_segment_plan(0, 100, 10, &[]),
            Err(SegmentPlanError::InvalidSourceRate)
        );
        assert_eq!(
            build_segment_plan(SOURCE_RATE, 100, 0, &[]),
            Err(SegmentPlanError::InvalidMaximumLength)
        );
    }

    #[test]
    fn empty_input_and_full_silence_return_no_speech() {
        assert_eq!(
            build_segment_plan(SOURCE_RATE, 0, 10, &[]).unwrap(),
            SegmentPlanOutcome::NoSpeech
        );
        assert_eq!(
            build_segment_plan(
                SOURCE_RATE,
                100,
                10,
                &[interval(50, 100), interval(0, 25), interval(25, 50)],
            )
            .unwrap(),
            SegmentPlanOutcome::NoSpeech
        );
    }

    #[test]
    fn no_silence_hard_cuts_the_complete_timeline() {
        let result = plan(23, 5, &[]);
        assert_segments(&result, &[(0, 5), (5, 10), (10, 15), (15, 20), (20, 23)], 5);
    }

    #[test]
    fn a_pause_before_the_limit_uses_its_end_as_the_boundary() {
        let result = plan(180, 100, &[interval(40, 60)]);
        assert_segments(&result, &[(0, 60), (60, 160), (160, 180)], 100);
    }

    #[test]
    fn latest_pause_wins_when_multiple_pauses_fit() {
        let result = plan(180, 100, &[interval(20, 30), interval(70, 90)]);
        assert_segments(&result, &[(0, 90), (90, 180)], 100);
    }

    #[test]
    fn a_pause_crossing_the_strict_limit_cuts_at_the_limit() {
        let result = plan(200, 100, &[interval(90, 120)]);
        assert_segments(&result, &[(0, 100), (100, 200)], 100);
    }

    #[test]
    fn a_fitting_final_range_is_not_split_for_an_internal_pause() {
        let result = plan(100, 100, &[interval(40, 60)]);
        assert_segments(&result, &[(0, 100)], 100);
    }

    #[test]
    fn leading_and_trailing_silence_do_not_create_artificial_short_segments() {
        let result = plan(
            160,
            100,
            &[interval(0, 10), interval(60, 70), interval(150, 160)],
        );
        assert_segments(&result, &[(0, 70), (70, 160)], 100);
    }

    #[test]
    fn a_long_silence_continuing_after_a_cut_uses_stable_hard_cuts() {
        let result = plan(260, 60, &[interval(10, 200)]);
        assert_segments(
            &result,
            &[(0, 60), (60, 120), (120, 180), (180, 240), (240, 260)],
            60,
        );
    }

    #[test]
    fn normalizes_unsorted_overlapping_adjacent_and_out_of_source_reports() {
        let result = plan(
            250,
            100,
            &[
                interval(190, 300),
                interval(40, 60),
                interval(20, 40),
                interval(60, 80),
                interval(88, 88),
                interval(180, 190),
            ],
        );
        assert_segments(&result, &[(0, 80), (80, 180), (180, 250)], 100);
    }

    #[test]
    fn malformed_reports_are_ignored_without_leaving_gaps() {
        let result = plan(
            35,
            10,
            &[interval(9, 9), interval(20, 10), interval(99, 100)],
        );
        assert_segments(&result, &[(0, 10), (10, 20), (20, 30), (30, 35)], 10);
    }

    #[test]
    fn plan_validation_rejects_gaps_overlaps_and_oversized_ranges() {
        let invalid = SegmentPlan {
            source_rate: SOURCE_RATE,
            total_frames: 20,
            max_segment_frames: 10,
            segments: vec![interval(0, 10), interval(11, 20)],
        };
        assert_eq!(invalid.validate(), Err(SegmentPlanError::InvalidPlan));

        let invalid = SegmentPlan {
            source_rate: SOURCE_RATE,
            total_frames: 20,
            max_segment_frames: 10,
            segments: vec![interval(0, 11), interval(11, 20)],
        };
        assert_eq!(invalid.validate(), Err(SegmentPlanError::InvalidPlan));
    }
}
