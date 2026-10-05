use std::marker::PhantomData;
use std::rc::Rc;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToStringAlloc,
};
use windows::Win32::System::Com::*;
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{PCWSTR, PWSTR};

use super::{CaptureFormat, InputDevice};
use crate::recorder::AudioStream;

struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(error)?;
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
struct Property(PROPVARIANT);
impl Drop for Property {
    fn drop(&mut self) {
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}
fn error(e: windows::core::Error) -> String {
    e.to_string()
}

fn take_string(value: PWSTR) -> Result<String, String> {
    let result = unsafe { value.to_string() }.map_err(|e| e.to_string());
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    result
}

fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(error)
}

fn describe(device: &IMMDevice, default: Option<&str>) -> Result<InputDevice, String> {
    let id = take_string(unsafe { device.GetId() }.map_err(error)?)?;
    let name = (|| {
        let store = unsafe { device.OpenPropertyStore(STGM_READ) }.map_err(error)?;
        let value = Property(unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.map_err(error)?);
        take_string(unsafe { PropVariantToStringAlloc(&value.0) }.map_err(error)?)
    })()
    .unwrap_or_else(|_: String| id.clone());
    Ok(InputDevice {
        is_default: default == Some(id.as_str()),
        id,
        name,
    })
}

pub(super) fn list_devices() -> Result<Vec<InputDevice>, String> {
    let _apartment = Apartment::new()?;
    let enumerator = enumerator()?;
    let default = unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }
        .ok()
        .and_then(|d| unsafe { d.GetId() }.ok())
        .and_then(|id| take_string(id).ok());
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }.map_err(error)?;
    let mut devices = Vec::new();
    for index in 0..unsafe { collection.GetCount() }.map_err(error)? {
        let device = unsafe { collection.Item(index) }.map_err(error)?;
        // An endpoint can disappear between enumeration and property lookup.
        if let Ok(info) = describe(&device, default.as_deref()) {
            devices.push(info);
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(devices)
}

fn device_format(device: &IMMDevice) -> Result<CaptureFormat, String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.map_err(error)?;
    let value = Property(unsafe { store.GetValue(&PKEY_AudioEngine_DeviceFormat) }.map_err(error)?);
    unsafe {
        let inner = &value.0.Anonymous.Anonymous;
        if inner.vt != VT_BLOB {
            return Err("device format is not a blob".into());
        }
        let blob = &inner.Anonymous.blob;
        if blob.pBlobData.is_null() || blob.cbSize < 18 {
            return Err("empty device format".into());
        }
        CaptureFormat::from_wave_format(std::slice::from_raw_parts(
            blob.pBlobData,
            blob.cbSize as usize,
        ))
    }
}

fn mix_format(client: &IAudioClient) -> Result<CaptureFormat, String> {
    let pointer = unsafe { client.GetMixFormat() }.map_err(error)?;
    if pointer.is_null() {
        return Err("empty audio engine format".into());
    }
    let result = unsafe {
        let size = 18 + usize::from((*pointer).cbSize);
        CaptureFormat::from_wave_format(std::slice::from_raw_parts(pointer.cast(), size))
    };
    unsafe { CoTaskMemFree(Some(pointer.cast())) };
    result
}

pub(super) fn open(id: &str) -> Result<Box<dyn AudioStream>, String> {
    let apartment = Apartment::new()?;
    let enumerator = enumerator()?;
    let device = if id.is_empty() {
        unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }
            .map_err(|e| format!("System default microphone unavailable: {e}"))?
    } else {
        let wide: Vec<_> = id.encode_utf16().chain(Some(0)).collect();
        unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
            .map_err(|e| format!("Selected microphone unavailable ({id}): {e}"))?
    };
    if unsafe { device.GetState() }.map_err(error)? != DEVICE_STATE_ACTIVE {
        return Err(format!("Selected microphone unavailable ({id})"));
    }
    // A config/CLI ID must refer to a capture endpoint, never a playback device.
    use windows::core::Interface;
    let endpoint: IMMEndpoint = device.cast().map_err(error)?;
    if unsafe { endpoint.GetDataFlow() }.map_err(error)? != eCapture {
        return Err("Selected device is not a microphone input".into());
    }
    let info = describe(&device, None)?;
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(error)?;
    let preferred = device_format(&device);
    let (format, fallback) = match preferred {
        Ok(format)
            if {
                let mut closest = std::ptr::null_mut();
                let result = unsafe {
                    client.IsFormatSupported(
                        AUDCLNT_SHAREMODE_SHARED,
                        format.wave_format.as_ptr().cast(),
                        Some(&mut closest),
                    )
                };
                unsafe { CoTaskMemFree(Some(closest.cast())) };
                // S_FALSE supplies a closest match and does not accept our exact format.
                result.0 == 0
            } =>
        {
            (format, false)
        }
        _ => (mix_format(&client)?, true),
    };
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            0,
            1_000_000,
            0,
            format.wave_format.as_ptr().cast(),
            None,
        )
    }
    .map_err(error)?;
    let capture = unsafe { client.GetService::<IAudioCaptureClient>() }.map_err(error)?;
    Ok(Box::new(Stream {
        capture,
        client,
        format,
        info,
        fallback,
        _apartment: apartment,
    }))
}

// All COM objects are created, used and dropped on the recorder thread.
// Field order keeps the apartment alive until after the interfaces are released.
struct Stream {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    format: CaptureFormat,
    info: InputDevice,
    fallback: bool,
    _apartment: Apartment,
}
impl AudioStream for Stream {
    fn format(&self) -> &CaptureFormat {
        &self.format
    }
    fn description(&self) -> String {
        format!(
            "{} ({}) engine_format_fallback={}",
            self.info.name, self.info.id, self.fallback
        )
    }
    fn start(&mut self) -> Result<(), String> {
        // Discard buffered samples from before a pause.
        unsafe { self.client.Reset() }.map_err(error)?;
        unsafe { self.client.Start() }.map_err(error)
    }
    fn stop(&mut self) -> Result<(), String> {
        unsafe { self.client.Stop() }.map_err(error)
    }
    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn read(&mut self, buffer: &mut Vec<u8>) -> Result<(), String> {
        buffer.clear();
        if unsafe { self.capture.GetNextPacketSize() }.map_err(error)? == 0 {
            return Ok(());
        }
        let mut pointer = std::ptr::null_mut();
        let mut frames = 0;
        let mut flags = 0;
        unsafe {
            self.capture
                .GetBuffer(&mut pointer, &mut frames, &mut flags, None, None)
        }
        .map_err(error)?;
        let size = frames as usize * usize::from(self.format.block_align);
        let result = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
            buffer.resize(
                size,
                if self.format.bits_per_sample == 8 {
                    128
                } else {
                    0
                },
            );
            Ok(())
        } else if pointer.is_null() {
            Err("capture returned a null packet".into())
        } else {
            buffer.extend_from_slice(unsafe { std::slice::from_raw_parts(pointer, size) });
            Ok(())
        };
        unsafe { self.capture.ReleaseBuffer(frames) }.map_err(error)?;
        result
    }
}
