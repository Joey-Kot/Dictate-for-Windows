//! A fresh detector per input; streaming 16 kHz mono signed PCM, 256 samples/frame.
use crate::{audio_intervals::AudioInterval, converter::ConvertError};

const FRAME: usize = 256;
const CONTINUE_THRESHOLD: f32 = 0.5;
const START_FRAMES: u64 = 3;
const LOOKBACK_FRAMES: u64 = 6;
const MIN_VOICE_FRAMES: u64 = 4;
const END_SILENCE_FRAMES: u64 = 10;

pub struct Vad {
    start_threshold: f32,
    detector: earshot::Detector,
    pending: [i16; FRAME],
    used: usize,
    position: u64,
    candidate: Option<u64>,
    voiced: u64,
    consecutive: u64,
    active: bool,
    last_voice_end: u64,
    intervals: Vec<AudioInterval>,
}
impl Default for Vad {
    fn default() -> Self {
        Self::new(crate::config::DEFAULT_VAD_START_THRESHOLD).expect("valid default VAD threshold")
    }
}
impl Vad {
    pub fn new(start_threshold: f64) -> Result<Self, crate::config::ConfigError> {
        crate::config::validate_vad_start_threshold(start_threshold)?;
        Ok(Self {
            start_threshold: start_threshold as f32,
            detector: earshot::Detector::default(),
            pending: [0; FRAME],
            used: 0,
            position: 0,
            candidate: None,
            voiced: 0,
            consecutive: 0,
            active: false,
            last_voice_end: 0,
            intervals: Vec::new(),
        })
    }
}
impl Vad {
    pub fn push(&mut self, mut samples: &[i16]) -> Result<(), ConvertError> {
        while !samples.is_empty() {
            let n = (FRAME - self.used).min(samples.len());
            self.pending[self.used..self.used + n].copy_from_slice(&samples[..n]);
            self.used += n;
            samples = &samples[n..];
            if self.used == FRAME {
                let score = self.detector.predict_i16(&self.pending);
                self.accept(score, FRAME as u64)?;
                self.used = 0;
            }
        }
        Ok(())
    }
    fn accept(&mut self, score: f32, samples: u64) -> Result<(), ConvertError> {
        let start = self.position;
        self.position = self
            .position
            .checked_add(samples)
            .ok_or_else(|| ConvertError::Failed {
                message: "VAD position overflow".into(),
            })?;
        if score >= CONTINUE_THRESHOLD {
            let candidate = self.candidate.get_or_insert(start);
            self.voiced += 1;
            self.last_voice_end = self.position;
            if !self.active {
                // Include the confirmation frames in the six-frame lookback.
                // All candidate frames meet the continuation threshold, so
                // trimming the window also caps its voice count.
                *candidate =
                    (*candidate).max(start.saturating_sub((LOOKBACK_FRAMES - 1) * FRAME as u64));
                self.voiced = self.voiced.min(LOOKBACK_FRAMES);
                self.consecutive = if score >= self.start_threshold {
                    self.consecutive + 1
                } else {
                    0
                };
                self.active = self.consecutive >= START_FRAMES;
            }
        } else {
            self.consecutive = 0;
            if !self.active {
                self.candidate = None;
                self.voiced = 0;
            } else if self.position - self.last_voice_end >= END_SILENCE_FRAMES * FRAME as u64 {
                self.close()?;
            }
        }
        Ok(())
    }
    fn close(&mut self) -> Result<(), ConvertError> {
        if self.active && self.voiced >= MIN_VOICE_FRAMES {
            self.intervals
                .try_reserve(1)
                .map_err(|_| ConvertError::Failed {
                    message: "VAD interval allocation failed".into(),
                })?;
            self.intervals.push(AudioInterval {
                start_frame: self.candidate.unwrap(),
                end_frame: self.last_voice_end,
            });
        }
        self.candidate = None;
        self.voiced = 0;
        self.consecutive = 0;
        self.active = false;
        Ok(())
    }
    pub fn finish(mut self) -> Result<Vec<AudioInterval>, ConvertError> {
        if self.used > 0 {
            self.pending[self.used..].fill(0);
            let score = self.detector.predict_i16(&self.pending);
            self.accept(score, self.used as u64)?;
        }
        self.close()?;
        Ok(self.intervals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_and_partial_tail() {
        let mut vad = Vad::default();
        for _ in 0..100 {
            vad.push(&[0; 317]).unwrap();
        }
        assert!(vad.finish().unwrap().is_empty());
    }
    #[test]
    fn configured_threshold_changes_startup() {
        for (threshold, expected) in [(0.6, 1), (0.9, 0), (0.99, 0)] {
            let mut vad = Vad::new(threshold).unwrap();
            for _ in 0..4 {
                vad.accept(0.6, FRAME as u64).unwrap();
            }
            assert_eq!(vad.finish().unwrap().len(), expected);
        }
        assert!(Vad::new(0.0).is_err());
        assert!(Vad::new(f64::NAN).is_err());
    }

    #[test]
    fn start_threshold_boundaries_are_inclusive() {
        for threshold in [0.5, 1.0] {
            let mut vad = Vad::new(threshold).unwrap();
            for _ in 0..4 {
                vad.accept(threshold as f32, FRAME as u64).unwrap();
            }
            assert_eq!(
                vad.finish().unwrap(),
                vec![AudioInterval {
                    start_frame: 0,
                    end_frame: 4 * FRAME as u64,
                }]
            );
        }
        assert!(Vad::new(0.499).is_err());
        assert!(Vad::new(1.001).is_err());
    }
    #[test]
    fn hysteresis_flush_and_short_bursts() {
        let mut vad = Vad::default();
        vad.accept(0.6, 256).unwrap();
        vad.accept(0.0, 256).unwrap();
        for _ in 0..4 {
            vad.accept(0.6, 256).unwrap();
        }
        for _ in 0..5 {
            vad.accept(0.0, 256).unwrap();
        }
        vad.accept(0.5, 17).unwrap();
        assert_eq!(
            vad.finish().unwrap(),
            vec![AudioInterval {
                start_frame: 512,
                end_frame: 2833
            }]
        );
        let mut vad = Vad::default();
        vad.accept(0.6, 17).unwrap();
        assert!(vad.finish().unwrap().is_empty());
    }

    #[test]
    fn startup_requires_three_consecutive_high_scores() {
        let mut vad = Vad::default();
        for score in [0.5, 0.59, 0.6, 0.6, 0.59, 0.6, 0.6] {
            vad.accept(score, FRAME as u64).unwrap();
        }
        assert!(vad.finish().unwrap().is_empty());
    }

    #[test]
    fn lookback_is_bounded_and_weak_speech_continues() {
        let mut vad = Vad::default();
        for _ in 0..20 {
            vad.accept(0.5, FRAME as u64).unwrap();
        }
        for _ in 0..3 {
            vad.accept(0.6, FRAME as u64).unwrap();
        }
        assert_eq!(vad.voiced, LOOKBACK_FRAMES);
        for _ in 0..2 {
            vad.accept(0.5, FRAME as u64).unwrap();
        }
        for _ in 0..END_SILENCE_FRAMES - 1 {
            vad.accept(0.49, FRAME as u64).unwrap();
        }
        assert!(vad.intervals.is_empty());
        vad.accept(0.49, FRAME as u64).unwrap();
        assert_eq!(
            vad.finish().unwrap(),
            vec![AudioInterval {
                start_frame: 17 * FRAME as u64,
                end_frame: 25 * FRAME as u64,
            }]
        );
    }

    #[test]
    fn silence_resets_candidate_and_three_frames_are_discarded() {
        let mut vad = Vad::default();
        for score in [0.5, 0.6, 0.6, 0.49, 0.6, 0.6, 0.6] {
            vad.accept(score, FRAME as u64).unwrap();
        }
        assert!(vad.finish().unwrap().is_empty());
    }

    #[test]
    fn partial_tail_counts_as_one_voice_frame_with_real_endpoint() {
        let mut vad = Vad::default();
        for _ in 0..3 {
            vad.accept(0.6, FRAME as u64).unwrap();
        }
        vad.accept(0.5, 17).unwrap();
        assert_eq!(
            vad.finish().unwrap(),
            vec![AudioInterval {
                start_frame: 0,
                end_frame: 3 * FRAME as u64 + 17,
            }]
        );
    }
}
