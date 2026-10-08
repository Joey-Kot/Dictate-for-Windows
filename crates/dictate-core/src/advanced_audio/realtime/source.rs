//! Audio sources shared by recorded replay and live microphone sessions.
//!
//! The recorder remains the owner of the complete local WAV. A live source
//! only receives its non-blocking packet tee and converts bounded packets on
//! a separate async task, so a slow network can never block capture.

use std::collections::VecDeque;
use std::sync::mpsc::TryRecvError;

use async_trait::async_trait;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::audio_devices::CaptureFormat;
use crate::recorder::{CapturePacket, CapturePacketReceiver};

use super::super::schema::RealtimeAudioStream;

/// One target-format websocket audio payload plus its media timeline length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioChunk {
    pub bytes: Vec<u8>,
    pub duration_ms: u64,
}

/// Bounded source used by a realtime session. Sources never expose partial
/// transcript data; they only produce the declared audio format.
#[async_trait]
pub trait RealtimeChunkSource: Send {
    async fn next_chunk(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<AudioChunk>, RealtimeSourceError>;

    /// Recorded replay is produced as fast as it can be decoded, so the
    /// session must apply media-timeline pacing before it sends each chunk.
    /// Live capture packets already arrive on that timeline; sleeping again
    /// after each live chunk would make the sender fall behind the recorder.
    fn requires_realtime_pacing(&self) -> bool {
        true
    }
}

/// Replays already recorded audio. Pacing is deliberately applied by the
/// session, after chunk conversion, rather than by an ffmpeg `-re` flag.
pub struct RecordedReplaySource {
    chunks: VecDeque<AudioChunk>,
}

impl RecordedReplaySource {
    pub fn from_wav(
        path: impl AsRef<std::path::Path>,
        target: &RealtimeAudioStream,
    ) -> Result<Self, RealtimeSourceError> {
        if !target.codec.eq_ignore_ascii_case("pcm_s16le") {
            return Err(RealtimeSourceError::UnsupportedCodec(target.codec.clone()));
        }
        let mut reader = hound::WavReader::open(path).map_err(RealtimeSourceError::WavOpen)?;
        let spec = reader.spec();
        if spec.channels == 0 || spec.sample_rate == 0 {
            return Err(RealtimeSourceError::InvalidWav);
        }
        let samples = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(RealtimeSourceError::WavRead)?,
            hound::SampleFormat::Int => {
                let scale = (1_i64 << (spec.bits_per_sample.saturating_sub(1))).max(1) as f32;
                reader
                    .samples::<i32>()
                    .map(|sample| sample.map(|sample| sample as f32 / scale))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(RealtimeSourceError::WavRead)?
            }
        };
        let native = NativePcm {
            sample_rate: spec.sample_rate,
            channels: spec.channels,
            samples,
        };
        let mut chunker = PcmChunker::new(target)?;
        chunker.push_native(native)?;
        Ok(Self {
            chunks: chunker.finish()?,
        })
    }
}

#[async_trait]
impl RealtimeChunkSource for RecordedReplaySource {
    async fn next_chunk(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<AudioChunk>, RealtimeSourceError> {
        if cancellation.is_cancelled() {
            return Err(RealtimeSourceError::Canceled);
        }
        Ok(self.chunks.pop_front())
    }
}

/// Converts recorder packets taken from the bounded optional tee. If the tee
/// reaches capacity, this source fails rather than producing a transcript
/// with a missing audio interval; the Runtime keeps recording locally and
/// replays the complete WAV after Stop.
pub struct LiveChunkSource {
    receiver: CapturePacketReceiver,
    target: RealtimeAudioStream,
    chunker: Option<PcmChunker>,
    pending: VecDeque<AudioChunk>,
    disconnected: bool,
}

impl LiveChunkSource {
    pub fn new(receiver: CapturePacketReceiver, target: RealtimeAudioStream) -> Self {
        Self {
            receiver,
            target,
            chunker: None,
            pending: VecDeque::new(),
            disconnected: false,
        }
    }

    fn accept_packet(&mut self, packet: CapturePacket) -> Result<(), RealtimeSourceError> {
        let (format, bytes) = packet.into_parts();
        let chunker = self.chunker.get_or_insert(PcmChunker::new(&self.target)?);
        chunker.push_capture(&format, &bytes)?;
        self.pending.extend(chunker.take_ready());
        Ok(())
    }

    fn finish(&mut self) -> Result<(), RealtimeSourceError> {
        if let Some(chunker) = &mut self.chunker {
            self.pending.extend(chunker.finish()?);
        }
        Ok(())
    }
}

#[async_trait]
impl RealtimeChunkSource for LiveChunkSource {
    async fn next_chunk(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<AudioChunk>, RealtimeSourceError> {
        loop {
            if cancellation.is_cancelled() {
                return Err(RealtimeSourceError::Canceled);
            }
            if self.receiver.backpressure_exceeded() {
                return Err(RealtimeSourceError::Backpressure);
            }
            if let Some(chunk) = self.pending.pop_front() {
                return Ok(Some(chunk));
            }
            if self.disconnected {
                return Ok(None);
            }
            match self.receiver.try_recv() {
                Ok(packet) => self.accept_packet(packet)?,
                Err(TryRecvError::Empty) => {
                    tokio::select! {
                        _ = cancellation.cancelled() => return Err(RealtimeSourceError::Canceled),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    self.finish()?;
                }
            }
        }
    }

    fn requires_realtime_pacing(&self) -> bool {
        false
    }
}

struct PcmChunker {
    target: RealtimeAudioStream,
    bytes_per_frame: usize,
    chunk_frame_numerator: u64,
    chunk_frame_remainder: u64,
    next_chunk_frames: usize,
    pending_bytes: Vec<u8>,
    ready: VecDeque<AudioChunk>,
    resampler: Option<IncrementalPcmResampler>,
    finished: bool,
}

impl PcmChunker {
    fn new(target: &RealtimeAudioStream) -> Result<Self, RealtimeSourceError> {
        if !target.codec.eq_ignore_ascii_case("pcm_s16le") {
            return Err(RealtimeSourceError::UnsupportedCodec(target.codec.clone()));
        }
        let bytes_per_frame = usize::from(target.channels)
            .checked_mul(std::mem::size_of::<i16>())
            .filter(|bytes| *bytes != 0)
            .ok_or(RealtimeSourceError::InvalidTargetFormat)?;
        if target.sample_rate == 0 {
            return Err(RealtimeSourceError::InvalidTargetFormat);
        }
        let chunk_frame_numerator = u64::from(target.sample_rate)
            .checked_mul(u64::from(target.chunk_duration_ms))
            .ok_or(RealtimeSourceError::InvalidTargetFormat)?;
        let mut chunker = Self {
            target: target.clone(),
            bytes_per_frame,
            chunk_frame_numerator,
            chunk_frame_remainder: 0,
            next_chunk_frames: 0,
            pending_bytes: Vec::new(),
            ready: VecDeque::new(),
            resampler: None,
            finished: false,
        };
        chunker.advance_chunk_size()?;
        Ok(chunker)
    }

    fn push_capture(
        &mut self,
        format: &CaptureFormat,
        bytes: &[u8],
    ) -> Result<(), RealtimeSourceError> {
        let native = decode_capture(format, bytes)?;
        self.push_native(native)
    }

    fn push_native(&mut self, native: NativePcm) -> Result<(), RealtimeSourceError> {
        if self.finished {
            return Err(RealtimeSourceError::InvalidCapturePacket);
        }
        let output = match &mut self.resampler {
            Some(resampler) => resampler.push(native, &self.target)?,
            None => {
                let mut resampler = IncrementalPcmResampler::new(&native)?;
                let output = resampler.push(native, &self.target)?;
                self.resampler = Some(resampler);
                output
            }
        };
        self.append_output(&output)?;
        Ok(())
    }

    fn append_output(&mut self, output: &[i16]) -> Result<(), RealtimeSourceError> {
        for sample in output {
            self.pending_bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.split_ready()
    }

    fn split_ready(&mut self) -> Result<(), RealtimeSourceError> {
        while self.pending_bytes.len() >= self.next_chunk_bytes()? {
            let bytes = self
                .pending_bytes
                .drain(..self.next_chunk_bytes()?)
                .collect();
            self.ready.push_back(AudioChunk {
                bytes,
                duration_ms: self.target.chunk_duration_ms.into(),
            });
            self.advance_chunk_size()?;
        }
        Ok(())
    }

    fn take_ready(&mut self) -> VecDeque<AudioChunk> {
        std::mem::take(&mut self.ready)
    }

    fn finish(&mut self) -> Result<VecDeque<AudioChunk>, RealtimeSourceError> {
        if !self.finished {
            if let Some(resampler) = &mut self.resampler {
                let output = resampler.finish(&self.target)?;
                self.append_output(&output)?;
            }
            self.split_ready()?;
            if !self.pending_bytes.is_empty() {
                let bytes = std::mem::take(&mut self.pending_bytes);
                let frames = bytes.len() / self.bytes_per_frame;
                let duration_ms = u64::try_from(frames)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(1000)
                    / u64::from(self.target.sample_rate);
                self.ready.push_back(AudioChunk { bytes, duration_ms });
            }
            self.finished = true;
        }
        Ok(self.take_ready())
    }

    fn next_chunk_bytes(&self) -> Result<usize, RealtimeSourceError> {
        self.next_chunk_frames
            .checked_mul(self.bytes_per_frame)
            .filter(|bytes| *bytes != 0)
            .ok_or(RealtimeSourceError::InvalidTargetFormat)
    }

    fn advance_chunk_size(&mut self) -> Result<(), RealtimeSourceError> {
        let frames_with_remainder = self
            .chunk_frame_remainder
            .checked_add(self.chunk_frame_numerator)
            .ok_or(RealtimeSourceError::InvalidTargetFormat)?;
        let frames = frames_with_remainder / 1000;
        self.chunk_frame_remainder = frames_with_remainder % 1000;
        self.next_chunk_frames = usize::try_from(frames)
            .ok()
            .filter(|frames| *frames != 0)
            .ok_or(RealtimeSourceError::InvalidTargetFormat)?;
        Ok(())
    }
}

struct NativePcm {
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
}

fn decode_capture(format: &CaptureFormat, bytes: &[u8]) -> Result<NativePcm, RealtimeSourceError> {
    if format.channels == 0
        || format.sample_rate == 0
        || format.block_align == 0
        || !bytes.len().is_multiple_of(usize::from(format.block_align))
    {
        return Err(RealtimeSourceError::InvalidCapturePacket);
    }
    let bytes_per_sample = usize::from(format.bits_per_sample / 8);
    let mut samples = Vec::with_capacity(bytes.len() / bytes_per_sample);
    for value in bytes.chunks_exact(bytes_per_sample) {
        let sample = if format.is_float {
            if value.len() != 4 {
                return Err(RealtimeSourceError::InvalidCapturePacket);
            }
            f32::from_le_bytes(value.try_into().expect("checked sample length"))
        } else {
            decode_integer_sample(value, format.valid_bits)?
        };
        samples.push(sample.clamp(-1.0, 1.0));
    }
    Ok(NativePcm {
        sample_rate: format.sample_rate,
        channels: format.channels,
        samples,
    })
}

fn decode_integer_sample(bytes: &[u8], valid_bits: u16) -> Result<f32, RealtimeSourceError> {
    let mut value = 0_i32;
    for (shift, byte) in bytes.iter().enumerate() {
        value |= i32::from(*byte) << (shift * 8);
    }
    let bit_count = bytes.len() * 8;
    if bit_count < i32::BITS as usize && bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
        value |= !0_i32 << bit_count;
    }
    let shift = bit_count.saturating_sub(usize::from(valid_bits));
    let value = value >> shift;
    let scale = (1_i64 << valid_bits.saturating_sub(1)).max(1) as f32;
    Ok(value as f32 / scale)
}

/// Stateful linear PCM resampler. The output frame position is an exact
/// rational value (`next_output * input_rate / output_rate`), rather than a
/// per-packet calculation. It keeps the boundary frame until its successor
/// arrives, so packet boundaries neither reset fractional phase nor create a
/// discontinuity in interpolation.
struct IncrementalPcmResampler {
    input_rate: u32,
    input_channels: u16,
    samples: Vec<f32>,
    buffer_start_frame: u64,
    buffer_offset_frames: usize,
    total_input_frames: u64,
    next_output_frame: u64,
}

impl IncrementalPcmResampler {
    fn new(native: &NativePcm) -> Result<Self, RealtimeSourceError> {
        validate_native(native)?;
        Ok(Self {
            input_rate: native.sample_rate,
            input_channels: native.channels,
            samples: Vec::new(),
            buffer_start_frame: 0,
            buffer_offset_frames: 0,
            total_input_frames: 0,
            next_output_frame: 0,
        })
    }

    fn push(
        &mut self,
        native: NativePcm,
        target: &RealtimeAudioStream,
    ) -> Result<Vec<i16>, RealtimeSourceError> {
        let input_frames = validate_native(&native)?;
        if native.sample_rate != self.input_rate || native.channels != self.input_channels {
            return Err(RealtimeSourceError::InvalidCapturePacket);
        }
        self.total_input_frames = self
            .total_input_frames
            .checked_add(u64::try_from(input_frames).unwrap_or(u64::MAX))
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        self.samples.extend(native.samples);
        self.drain(target, false)
    }

    fn finish(&mut self, target: &RealtimeAudioStream) -> Result<Vec<i16>, RealtimeSourceError> {
        self.drain(target, true)
    }

    fn drain(
        &mut self,
        target: &RealtimeAudioStream,
        final_input: bool,
    ) -> Result<Vec<i16>, RealtimeSourceError> {
        let target_rate = u64::from(target.sample_rate);
        if target_rate == 0 {
            return Err(RealtimeSourceError::InvalidTargetFormat);
        }
        let final_output_frames = if final_input {
            self.total_input_frames
                .checked_mul(target_rate)
                .ok_or(RealtimeSourceError::InvalidCapturePacket)?
                / u64::from(self.input_rate)
        } else {
            u64::MAX
        };
        let mut output = Vec::new();
        while self.next_output_frame < final_output_frames {
            let position_numerator = self
                .next_output_frame
                .checked_mul(u64::from(self.input_rate))
                .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
            let first = position_numerator / target_rate;
            if first >= self.total_input_frames {
                break;
            }
            let second = first
                .checked_add(1)
                .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
            if !final_input && second >= self.total_input_frames {
                break;
            }
            let second = if final_input {
                second.min(self.total_input_frames.saturating_sub(1))
            } else {
                second
            };
            let fraction = (position_numerator % target_rate) as f32 / target_rate as f32;
            self.append_interpolated_frame(&mut output, target, first, second, fraction)?;
            self.next_output_frame = self
                .next_output_frame
                .checked_add(1)
                .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        }
        self.discard_consumed_frames(target)?;
        Ok(output)
    }

    fn append_interpolated_frame(
        &self,
        output: &mut Vec<i16>,
        target: &RealtimeAudioStream,
        first: u64,
        second: u64,
        fraction: f32,
    ) -> Result<(), RealtimeSourceError> {
        let interpolate = |channel| -> Result<f32, RealtimeSourceError> {
            let before = self.sample_at(first, channel)?;
            let after = self.sample_at(second, channel)?;
            Ok(before + (after - before) * fraction)
        };
        if target.channels == 1 {
            let mut sum = 0.0;
            for channel in 0..usize::from(self.input_channels) {
                sum += interpolate(channel)?;
            }
            output.push(float_to_i16(sum / f32::from(self.input_channels)));
            return Ok(());
        }
        for channel in 0..usize::from(target.channels) {
            output.push(float_to_i16(interpolate(
                channel % usize::from(self.input_channels),
            )?));
        }
        Ok(())
    }

    fn sample_at(&self, frame: u64, channel: usize) -> Result<f32, RealtimeSourceError> {
        let relative_frame = frame
            .checked_sub(self.buffer_start_frame)
            .and_then(|frame| usize::try_from(frame).ok())
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        let frame_index = self
            .buffer_offset_frames
            .checked_add(relative_frame)
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        let sample_index = frame_index
            .checked_mul(usize::from(self.input_channels))
            .and_then(|index| index.checked_add(channel))
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        self.samples
            .get(sample_index)
            .copied()
            .ok_or(RealtimeSourceError::InvalidCapturePacket)
    }

    fn discard_consumed_frames(
        &mut self,
        target: &RealtimeAudioStream,
    ) -> Result<(), RealtimeSourceError> {
        let next_position = self
            .next_output_frame
            .checked_mul(u64::from(self.input_rate))
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?
            / u64::from(target.sample_rate);
        let available_frames = self
            .total_input_frames
            .saturating_sub(self.buffer_start_frame);
        let discarded_frames = next_position
            .saturating_sub(self.buffer_start_frame)
            .min(available_frames);
        let discarded_frames = usize::try_from(discarded_frames)
            .map_err(|_| RealtimeSourceError::InvalidCapturePacket)?;
        self.buffer_start_frame = self
            .buffer_start_frame
            .checked_add(u64::try_from(discarded_frames).unwrap_or(u64::MAX))
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        self.buffer_offset_frames = self
            .buffer_offset_frames
            .checked_add(discarded_frames)
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;

        let consumed_samples = self
            .buffer_offset_frames
            .checked_mul(usize::from(self.input_channels))
            .ok_or(RealtimeSourceError::InvalidCapturePacket)?;
        if consumed_samples >= 16 * 1024 && consumed_samples.saturating_mul(2) >= self.samples.len()
        {
            self.samples.drain(..consumed_samples);
            self.buffer_offset_frames = 0;
        }
        Ok(())
    }
}

fn validate_native(native: &NativePcm) -> Result<usize, RealtimeSourceError> {
    if native.channels == 0
        || native.sample_rate == 0
        || !native
            .samples
            .len()
            .is_multiple_of(usize::from(native.channels))
    {
        return Err(RealtimeSourceError::InvalidCapturePacket);
    }
    Ok(native.samples.len() / usize::from(native.channels))
}

fn float_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

#[derive(Debug, Error)]
pub enum RealtimeSourceError {
    #[error("realtime audio codec '{0}' is not supported; this build supports pcm_s16le")]
    UnsupportedCodec(String),
    #[error("realtime audio target format is invalid")]
    InvalidTargetFormat,
    #[error("captured audio packet does not match its declared PCM format")]
    InvalidCapturePacket,
    #[error("recorded replay input is not a valid WAV file")]
    InvalidWav,
    #[error("failed to open recorded WAV: {0}")]
    WavOpen(#[source] hound::Error),
    #[error("failed to read recorded WAV: {0}")]
    WavRead(#[source] hound::Error),
    #[error("realtime audio source was canceled")]
    Canceled,
    #[error("live realtime audio fell behind recorder capture")]
    Backpressure,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_devices::test_capture_format;
    use crate::recorder::capture_packet_channel;

    fn target() -> RealtimeAudioStream {
        RealtimeAudioStream {
            codec: "pcm_s16le".into(),
            sample_rate: 16_000,
            channels: 1,
            chunk_duration_ms: 10,
            pacing: super::super::super::schema::RealtimePacing::Realtime,
        }
    }

    fn target_with(sample_rate: u32, chunk_duration_ms: u32) -> RealtimeAudioStream {
        RealtimeAudioStream {
            sample_rate,
            chunk_duration_ms,
            ..target()
        }
    }

    fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(std::mem::size_of_val(samples));
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    fn join_chunks(chunks: VecDeque<AudioChunk>) -> Vec<u8> {
        chunks.into_iter().flat_map(|chunk| chunk.bytes).collect()
    }

    #[test]
    fn converts_stereo_capture_to_mono_pcm_chunks() {
        let format = test_capture_format(16_000, 2, 16, 16, false);
        let mut bytes = Vec::new();
        for _ in 0..160 {
            bytes.extend_from_slice(&i16::MAX.to_le_bytes());
            bytes.extend_from_slice(&0_i16.to_le_bytes());
        }
        let mut chunker = PcmChunker::new(&target()).unwrap();
        chunker.push_capture(&format, &bytes).unwrap();
        let chunks = chunker.finish().unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].bytes.len(), 320);
        assert_eq!(
            i16::from_le_bytes(chunks[0].bytes[..2].try_into().unwrap()),
            16_383
        );
    }

    #[test]
    fn incremental_resampling_matches_one_shot_across_arbitrary_capture_packets() {
        let format = test_capture_format(44_100, 1, 16, 16, false);
        let input_frames = 44_100 + 317;
        let samples = (0..input_frames)
            .map(|frame| ((frame * 7_919 % 65_536) as i32 - 32_768) as i16)
            .collect::<Vec<_>>();
        let bytes = pcm_bytes(&samples);

        let mut one_shot = PcmChunker::new(&target()).unwrap();
        one_shot.push_capture(&format, &bytes).unwrap();
        let one_shot = join_chunks(one_shot.finish().unwrap());

        let mut incremental = PcmChunker::new(&target()).unwrap();
        let packet_sizes = [1, 17, 311, 2, 701, 29, 5, 1_023, 43];
        let mut offset = 0;
        let mut packet = 0;
        while offset < input_frames {
            let frame_count = packet_sizes[packet % packet_sizes.len()].min(input_frames - offset);
            let start = offset * std::mem::size_of::<i16>();
            let end = (offset + frame_count) * std::mem::size_of::<i16>();
            incremental
                .push_capture(&format, &bytes[start..end])
                .unwrap();
            offset += frame_count;
            packet += 1;
        }
        let incremental = join_chunks(incremental.finish().unwrap());

        assert_eq!(incremental, one_shot);
        assert_eq!(
            incremental.len(),
            ((input_frames as u64 * 16_000) / 44_100) as usize * std::mem::size_of::<i16>()
        );
    }

    #[test]
    fn chunk_frame_remainder_preserves_nonintegral_target_timing() {
        let format = test_capture_format(11_025, 1, 16, 16, false);
        let samples = vec![i16::MAX; 441];
        let mut chunker = PcmChunker::new(&target_with(11_025, 10)).unwrap();
        chunker.push_capture(&format, &pcm_bytes(&samples)).unwrap();
        let chunks = chunker.finish().unwrap().into_iter().collect::<Vec<_>>();

        assert_eq!(chunks.len(), 4);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.bytes.len() / std::mem::size_of::<i16>())
                .collect::<Vec<_>>(),
            vec![110, 110, 110, 111]
        );
        assert!(chunks.iter().all(|chunk| chunk.duration_ms == 10));
        assert_eq!(
            chunks.iter().map(|chunk| chunk.bytes.len()).sum::<usize>(),
            samples.len() * std::mem::size_of::<i16>()
        );
    }

    #[test]
    fn only_recorded_replay_requires_session_pacing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("replay.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample(0_i16).unwrap();
        writer.finalize().unwrap();
        let replay = RecordedReplaySource::from_wav(&path, &target()).unwrap();

        let (_sink, receiver) = capture_packet_channel(1);
        let live = LiveChunkSource::new(receiver, target());

        assert!(replay.requires_realtime_pacing());
        assert!(!live.requires_realtime_pacing());
    }
}
