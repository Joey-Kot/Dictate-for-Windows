//! Lossless packet storage: no conversion to i16 and no channel-layout loss.
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::audio_devices::CaptureFormat;

pub(crate) struct CaptureWav {
    writer: BufWriter<File>,
    data_size: u32,
    data_start: u64,
    block_align: u16,
    input_block_align: u16,
    input_bytes: usize,
    stored_bytes: usize,
}

impl CaptureWav {
    pub fn create(path: &Path, format: &CaptureFormat) -> io::Result<Self> {
        // FFmpeg 7 interprets 24 valid bits in a 32-bit WAV container as
        // Cool Edit float. Remove whole low padding bytes losslessly instead.
        // WAVEFORMATEXTENSIBLE PCM valid bits are left-aligned in each sample.
        let input_bytes = usize::from(format.bits_per_sample / 8);
        let stored_bytes = usize::from(format.valid_bits.div_ceil(8));
        let block_align = format.channels * stored_bytes as u16;
        let mut wave_format = format.wave_format.clone();
        wave_format[8..12]
            .copy_from_slice(&(format.sample_rate * u32::from(block_align)).to_le_bytes());
        wave_format[12..14].copy_from_slice(&block_align.to_le_bytes());
        wave_format[14..16].copy_from_slice(&((stored_bytes * 8) as u16).to_le_bytes());
        let mut writer = BufWriter::new(File::create(path)?);
        writer.write_all(b"RIFF\0\0\0\0WAVEfmt ")?;
        writer.write_all(&(wave_format.len() as u32).to_le_bytes())?;
        writer.write_all(&wave_format)?;
        if !wave_format.len().is_multiple_of(2) {
            writer.write_all(&[0])?;
        }
        // A fact chunk is required for IEEE float and harmless for PCM.
        writer.write_all(b"fact\x04\0\0\0\0\0\0\0data\0\0\0\0")?;
        let data_start = writer.stream_position()?;
        Ok(Self {
            writer,
            data_size: 0,
            data_start,
            block_align,
            input_block_align: format.block_align,
            input_bytes,
            stored_bytes,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let new_size =
            u64::from(self.data_size) + (bytes.len() / self.input_bytes * self.stored_bytes) as u64;
        if !bytes
            .len()
            .is_multiple_of(usize::from(self.input_block_align))
        {
            return Err(io::Error::other(
                "capture packet contains an incomplete frame",
            ));
        }
        if new_size + self.data_start + 1 > u64::from(u32::MAX) {
            return Err(io::Error::other("recording exceeds WAV size limit"));
        }
        if self.input_bytes == self.stored_bytes {
            self.writer.write_all(bytes)?;
        } else {
            for sample in bytes.chunks_exact(self.input_bytes) {
                let packed = &sample[self.input_bytes - self.stored_bytes..];
                if self.stored_bytes == 1 {
                    // WAV 8-bit PCM is unsigned; wider input PCM is signed.
                    self.writer.write_all(&[packed[0] ^ 0x80])?;
                } else {
                    self.writer.write_all(packed)?;
                }
            }
        }
        self.data_size = new_size as u32;
        Ok(())
    }

    pub fn finalize(mut self) -> io::Result<()> {
        let padding = self.data_size % 2;
        if padding != 0 {
            self.writer.write_all(&[0])?;
        }
        self.writer.seek(SeekFrom::Start(4))?;
        self.writer
            .write_all(&((self.data_start - 8) as u32 + self.data_size + padding).to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(self.data_start - 12))?;
        self.writer
            .write_all(&(self.data_size / u32::from(self.block_align)).to_le_bytes())?;
        self.writer.seek(SeekFrom::Start(self.data_start - 4))?;
        self.writer.write_all(&self.data_size.to_le_bytes())?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_devices::test_capture_format;

    #[test]
    fn preserves_pcm_float_valid_bits_channel_mask_and_packet_bytes() {
        let dir = tempfile::tempdir().unwrap();
        for (bits, valid, float, channels) in [
            (8, 8, false, 1),
            (16, 16, false, 2),
            (24, 24, false, 2),
            (32, 32, false, 2),
            (32, 32, true, 2),
        ] {
            let format = test_capture_format(44100, channels, bits, valid, float);
            let path = dir.path().join("capture.wav");
            let data: Vec<u8> = (0..usize::from(format.block_align) * 3)
                .map(|n| n as u8)
                .collect();
            let mut writer = CaptureWav::create(&path, &format).unwrap();
            writer
                .write(&data[..usize::from(format.block_align)])
                .unwrap();
            writer
                .write(&data[usize::from(format.block_align)..])
                .unwrap();
            writer.finalize().unwrap();
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(&bytes[20..60], &format.wave_format);
            assert_eq!(&bytes[80..80 + data.len()], &data);
            assert_eq!(
                u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize,
                bytes.len() - 8
            );
            assert_eq!(u32::from_le_bytes(bytes[68..72].try_into().unwrap()), 3);
            assert_eq!(
                u32::from_le_bytes(bytes[76..80].try_into().unwrap()) as usize,
                data.len()
            );
            assert!(bytes.len().is_multiple_of(2));
        }
    }

    #[test]
    fn rejects_incomplete_frame_without_writing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.wav");
        let mut writer =
            CaptureWav::create(&path, &test_capture_format(48000, 2, 24, 24, false)).unwrap();
        assert!(writer.write(&[1, 2, 3]).is_err());
        writer.finalize().unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), 80);
    }

    #[test]
    fn packs_24_in_32_without_losing_precision_or_channel_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.wav");
        let format = test_capture_format(48000, 2, 32, 24, false);
        let values: [i32; 6] = [0x12345600, -256, i32::MIN, 0x7fffff00, 0, 256];
        let input: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut writer = CaptureWav::create(&path, &format).unwrap();
        writer.write(&input).unwrap();
        writer.finalize().unwrap();
        let mut reader = hound::WavReader::open(path).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 24);
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.duration(), 3);
        assert_eq!(
            reader
                .samples::<i32>()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            values.iter().map(|value| value >> 8).collect::<Vec<_>>()
        );
    }
}
