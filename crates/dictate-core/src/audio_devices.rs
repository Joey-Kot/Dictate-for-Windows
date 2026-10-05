//! Shared input-device discovery and capture format metadata for GUI and CLI.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// The exact PCM format used by the capture stream. `wave_format` preserves
/// WAVEFORMATEXTENSIBLE valid bits and channel mask in the temporary WAV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub valid_bits: u16,
    pub is_float: bool,
    pub block_align: u16,
    pub(crate) wave_format: Vec<u8>,
}

impl CaptureFormat {
    pub fn from_wave_format(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 18 {
            return Err("incomplete capture format".into());
        }
        let word = |i| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let dword = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let tag = word(0);
        let channels = word(2);
        let sample_rate = dword(4);
        let block_align = word(12);
        let bits_per_sample = word(14);
        let size = 18 + usize::from(word(16));
        if size > bytes.len() {
            return Err("incomplete extended capture format".into());
        }
        let (encoding, valid_bits) = if tag == 0xfffe {
            if size < 40 || bytes[28..40] != [0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113] {
                return Err("unsupported capture subtype".into());
            }
            (dword(24), word(18))
        } else {
            (u32::from(tag), bits_per_sample)
        };
        if !matches!(encoding, 1 | 3)
            || channels == 0
            || sample_rate == 0
            || !matches!(bits_per_sample, 8 | 16 | 24 | 32)
            || valid_bits == 0
            || valid_bits > bits_per_sample
            || (encoding == 3 && (bits_per_sample != 32 || valid_bits != 32))
            || u32::from(block_align) != u32::from(channels) * u32::from(bits_per_sample / 8)
            || sample_rate.checked_mul(u32::from(block_align)) != Some(dword(8))
        {
            return Err("unsupported or invalid PCM capture format".into());
        }
        Ok(Self {
            sample_rate,
            channels,
            bits_per_sample,
            valid_bits,
            is_float: encoding == 3,
            block_align,
            wave_format: bytes[..size].to_vec(),
        })
    }
}

/// Lists active capture endpoints only. No API configuration is required.
pub fn list_input_devices() -> Result<Vec<InputDevice>, String> {
    #[cfg(windows)]
    {
        wasapi::list_devices()
    }
    #[cfg(not(windows))]
    {
        Err("Microphone capture is only available on Windows".into())
    }
}

/// Queries the format that would be used for this input (empty ID = default).
/// Resolves the endpoint afresh and does not start recording.
pub fn input_device_format(device_id: &str) -> Result<CaptureFormat, String> {
    use crate::recorder::AudioBackend;
    Ok(SystemAudioBackend.open_stream(device_id)?.format().clone())
}

#[cfg(test)]
pub(crate) fn test_capture_format(
    rate: u32,
    channels: u16,
    bits: u16,
    valid: u16,
    float: bool,
) -> CaptureFormat {
    let mut bytes = Vec::new();
    let align = channels * (bits / 8);
    bytes.extend_from_slice(&0xfffe_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
    bytes.extend_from_slice(&align.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    bytes.extend_from_slice(&22_u16.to_le_bytes());
    bytes.extend_from_slice(&valid.to_le_bytes());
    bytes.extend_from_slice(&(if channels == 1 { 4_u32 } else { 3_u32 }).to_le_bytes());
    bytes.extend_from_slice(&(if float { 3_u32 } else { 1_u32 }).to_le_bytes());
    bytes.extend_from_slice(&[0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
    CaptureFormat::from_wave_format(&bytes).unwrap()
}

pub(crate) struct SystemAudioBackend;

impl crate::recorder::AudioBackend for SystemAudioBackend {
    fn open_stream(
        &self,
        device_id: &str,
    ) -> Result<Box<dyn crate::recorder::AudioStream>, String> {
        #[cfg(windows)]
        {
            wasapi::open(device_id)
        }
        #[cfg(not(windows))]
        {
            let _ = device_id;
            Err("Microphone capture is only available on Windows".into())
        }
    }
}

#[cfg(windows)]
mod wasapi;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_or_unsupported_driver_formats() {
        let format = test_capture_format(48000, 2, 32, 24, false);
        assert_eq!(format.valid_bits, 24);
        for length in [0, 16, 18, 39] {
            assert!(CaptureFormat::from_wave_format(&format.wave_format[..length]).is_err());
        }
        for (offset, value) in [(2, 0), (12, 1), (18, 33), (24, 9), (28, 1)] {
            let mut bytes = format.wave_format.clone();
            bytes[offset] = value;
            assert!(
                CaptureFormat::from_wave_format(&bytes).is_err(),
                "offset={offset}"
            );
        }
    }
}
