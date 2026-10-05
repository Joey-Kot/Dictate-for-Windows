//! Half-open positions measured in frames per channel, never interleaved samples.
use crate::converter::ConvertError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInterval {
    pub start_frame: u64,
    pub end_frame: u64,
}

fn failure() -> ConvertError {
    ConvertError::Failed {
        message: "audio interval arithmetic overflow or invalid rate".into(),
    }
}

fn scale(value: u64, rate: u32, divisor: u32, ceil: bool) -> Result<u64, ConvertError> {
    if divisor == 0 || rate == 0 {
        return Err(failure());
    }
    let n = u128::from(value) * u128::from(rate);
    let n = if ceil {
        n.div_ceil(u128::from(divisor))
    } else {
        n / u128::from(divisor)
    };
    u64::try_from(n).map_err(|_| failure())
}

/// Normalize detector coordinates, map to the source, then share internal padding.
pub fn prepare_intervals(
    intervals: Vec<AudioInterval>,
    source_rate: u32,
    total_frames: u64,
    padding_ms: u32,
) -> Result<Vec<AudioInterval>, ConvertError> {
    if padding_ms > 1000 || source_rate == 0 {
        return Err(failure());
    }
    let mut raw = intervals;
    raw.retain(|i| i.start_frame < i.end_frame);
    raw.sort_unstable_by_key(|i| i.start_frame);
    let mut merged: Vec<AudioInterval> = Vec::new();
    merged.try_reserve(raw.len()).map_err(|_| failure())?;
    for i in raw {
        if let Some(last) = merged.last_mut()
            && i.start_frame <= last.end_frame
        {
            last.end_frame = last.end_frame.max(i.end_frame);
            continue;
        }
        merged.push(i);
    }
    for i in &mut merged {
        i.start_frame = scale(i.start_frame, source_rate, 16000, false)?.min(total_frames);
        i.end_frame = scale(i.end_frame, source_rate, 16000, true)?.min(total_frames);
    }
    merged.retain(|i| i.start_frame < i.end_frame);
    let padding = scale(u64::from(padding_ms), source_rate, 1000, false)?;
    // Split in milliseconds first, so odd milliseconds belong to the next segment.
    let after = scale(u64::from(padding_ms / 2), source_rate, 1000, false)?;
    let before = padding - after;
    let count = merged.len();
    for (index, i) in merged.iter_mut().enumerate() {
        i.start_frame = i
            .start_frame
            .saturating_sub(if index == 0 { padding } else { before });
        let tail = if index + 1 == count { padding } else { after };
        i.end_frame =
            (u128::from(i.end_frame) + u128::from(tail)).min(u128::from(total_frames)) as u64;
    }
    let mut out: Vec<AudioInterval> = Vec::new();
    out.try_reserve(merged.len()).map_err(|_| failure())?;
    for i in merged {
        if let Some(last) = out.last_mut()
            && i.start_frame <= last.end_frame
        {
            last.end_frame = last.end_frame.max(i.end_frame);
            continue;
        }
        out.push(i);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn i(a: u64, b: u64) -> AudioInterval {
        AudioInterval {
            start_frame: a,
            end_frame: b,
        }
    }
    #[test]
    fn shared_padding_and_edges() {
        assert_eq!(
            prepare_intervals(vec![i(1600, 3200), i(6400, 8000)], 48000, 30000, 100).unwrap(),
            vec![i(0, 12000), i(16800, 28800)]
        );
        for p in [0, 1, 100, 999, 1000] {
            let result =
                prepare_intervals(vec![i(0, 100), i(10000, 16000)], 16000, 16000, p).unwrap();
            assert_eq!(result[0].start_frame, 0);
            assert_eq!(result.last().unwrap().end_frame, 16000);
        }
        assert_eq!(
            prepare_intervals(vec![i(100, 200), i(216, 300)], 16000, 1000, 1).unwrap(),
            vec![i(84, 316)]
        );
        assert_eq!(
            prepare_intervals(vec![i(100, 200), i(217, 300)], 16000, 1000, 1).unwrap(),
            vec![i(84, 200), i(201, 316)]
        );
    }
    #[test]
    fn mapping_normalization_and_overflow() {
        assert_eq!(
            prepare_intervals(vec![i(3, 4), i(1, 3), i(9, 9)], 44100, 100, 0).unwrap(),
            vec![i(2, 12)]
        );
        assert!(prepare_intervals(vec![i(u64::MAX - 1, u64::MAX)], 48000, u64::MAX, 0).is_err());
        assert_eq!(
            prepare_intervals(vec![i(0, 1)], 16000, u64::MAX, 1000).unwrap(),
            vec![i(0, 16001)]
        );
        let many = (0..5000).map(|n| i(n * 1000, n * 1000 + 10)).collect();
        assert_eq!(
            prepare_intervals(many, 16000, 5_000_000, 0).unwrap().len(),
            5000
        );
    }
}
