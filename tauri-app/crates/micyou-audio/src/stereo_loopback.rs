//! 独立于 AEC 的电脑声音采集；回调必须非阻塞。
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub const PCM_FRAME_BYTES: usize = 3840;
#[derive(Clone, Debug)]
pub struct RenderEndpoint {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[cfg(windows)]
pub(crate) struct Apartment(bool);
#[cfg(windows)]
impl Apartment {
    pub(crate) fn enter() -> Result<Self, windows::core::Error> {
        wasapi::initialize_mta().ok()?;
        Ok(Self(true))
    }

    /// 只读枚举可复用 STA/MTA；采集仍必须通过 enter 建立 MTA。
    pub(crate) fn query() -> Result<Self, windows::core::Error> {
        use windows::Win32::Foundation::CO_E_NOTINITIALIZED;
        use windows::Win32::System::Com::{CoGetApartmentType, APTTYPE, APTTYPEQUALIFIER};
        let mut kind = APTTYPE::default();
        let mut qualifier = APTTYPEQUALIFIER::default();
        match unsafe { CoGetApartmentType(&mut kind, &mut qualifier) } {
            Ok(()) => Ok(Self(false)),
            Err(error) if error.code() == CO_E_NOTINITIALIZED => Self::enter(),
            Err(error) => Err(error),
        }
    }
}
#[cfg(windows)]
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            wasapi::deinitialize();
        }
    }
}

#[cfg(windows)]
pub fn list_render_endpoints() -> Result<Vec<RenderEndpoint>, String> {
    use wasapi::*;
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        let _apartment = Apartment::query()?;
        let e = DeviceEnumerator::new()?;
        let default = e.get_default_device(&Direction::Render)?.get_id()?;
        let mut result = Vec::new();
        for device in &e.get_device_collection(&Direction::Render)? {
            let device = device?;
            let id = device.get_id()?;
            result.push(RenderEndpoint {
                is_default: id == default,
                id,
                name: device.get_friendlyname()?,
            });
        }
        Ok(result)
    })();
    result.map_err(|e| e.to_string())
}
#[cfg(not(windows))]
pub fn list_render_endpoints() -> Result<Vec<RenderEndpoint>, String> {
    Err("unsupported_platform".into())
}

pub fn float_to_pcm(sample: f32) -> [u8; 2] {
    let finite = if sample.is_finite() { sample } else { 0.0 };
    ((finite * 32768.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes()
}

// 非静音指针必须来自尚未 ReleaseBuffer 的 WASAPI 包，覆盖 frames*block_align 字节。
#[cfg(any(windows, test))]
pub(crate) unsafe fn copy_capture_packet(
    ptr: *const u8,
    frames: usize,
    max_frames: usize,
    block_align: usize,
    silent: bool,
) -> Result<Vec<u8>, &'static str> {
    if frames > max_frames {
        return Err("capture buffer exceeds device limit");
    }
    if block_align == 0 {
        return Err("capture block alignment is zero");
    }
    let bytes = frames
        .checked_mul(block_align)
        .ok_or("capture buffer exceeds device limit")?;
    if bytes > isize::MAX as usize {
        return Err("capture buffer exceeds slice limit");
    }
    if silent {
        return Ok(vec![0; bytes]);
    }
    if frames == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err("capture buffer is null");
    }
    Ok(unsafe { std::slice::from_raw_parts(ptr, bytes) }.to_vec())
}

/// 所有 COM 对象仅在采集线程创建和释放。
#[cfg(windows)]
pub fn capture(
    endpoint: &str,
    stopped: Arc<AtomicBool>,
    mut check: impl FnMut() -> Result<(), String>,
    mut ready: impl FnMut(),
    mut frame: impl FnMut(Vec<u8>),
) -> Result<(), String> {
    use wasapi::{SampleType, WaveFormat};
    use windows::{
        core::PCWSTR,
        Win32::{
            Media::Audio::*,
            System::Com::{CoCreateInstance, CLSCTX_ALL},
        },
    };
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let _apartment = Apartment::enter()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let id: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let device = unsafe { enumerator.GetDevice(PCWSTR(id.as_ptr()))? };
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                200_000,
                0,
                format.as_waveformatex_ref(),
                None,
            )?;
        }
        let reader: IAudioCaptureClient = unsafe { client.GetService()? };
        // 由设备缓冲大小限制单次分配，不根据网络数据分配。
        let max_frames = unsafe { client.GetBufferSize()? } as usize;
        let mut pcm = Vec::with_capacity(PCM_FRAME_BYTES);
        check()?;
        unsafe {
            client.Start()?;
        }
        ready();
        let capture_result = (|| -> Result<(), Box<dyn std::error::Error>> {
            while !stopped.load(Ordering::Acquire) {
                check()?;
                while unsafe { reader.GetNextPacketSize()? } > 0 {
                    let mut ptr = std::ptr::null_mut();
                    let mut frames = 0;
                    let mut flags = 0;
                    unsafe {
                        reader.GetBuffer(&mut ptr, &mut frames, &mut flags, None, None)?;
                    }
                    // 静音包允许空指针，不能交给 wasapi 的无条件 slice 封装。
                    let bytes = unsafe {
                        copy_capture_packet(
                            ptr,
                            frames as usize,
                            max_frames,
                            8,
                            flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0,
                        )
                    };
                    unsafe {
                        reader.ReleaseBuffer(frames)?;
                    }
                    if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                        pcm.clear();
                    }
                    for sample in bytes?.chunks_exact(4) {
                        pcm.extend_from_slice(&float_to_pcm(f32::from_le_bytes(
                            sample.try_into()?,
                        )));
                        if pcm.len() == PCM_FRAME_BYTES {
                            check()?;
                            frame(std::mem::replace(
                                &mut pcm,
                                Vec::with_capacity(PCM_FRAME_BYTES),
                            ));
                        }
                    }
                    if stopped.load(Ordering::Acquire) {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(())
        })();
        let stop_result = unsafe { client.Stop() };
        capture_result?;
        stop_result?;
        Ok(())
    })();
    result.map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn capture(
    _: &str,
    _: Arc<AtomicBool>,
    _: impl FnMut() -> Result<(), String>,
    _: impl FnMut(),
    _: impl FnMut(Vec<u8>),
) -> Result<(), String> {
    Err("unsupported_platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn endpoint_queries_preserve_sta_and_mta_ownership() {
        for initialize in [wasapi::initialize_sta, wasapi::initialize_mta] {
            std::thread::spawn(move || {
                initialize().ok().unwrap();
                assert!(!crate::endpoint::default_render_endpoint_id().unwrap().is_empty());
                assert!(!list_render_endpoints().unwrap().is_empty());
                {
                    let apartment = Apartment::query().unwrap();
                    assert!(!apartment.0, "must not own the caller's COM initialization");
                }
                // guard 释放后仍须保留原 apartment，不能提前 CoUninitialize。
                let apartment = Apartment::query().unwrap();
                assert!(!apartment.0);
                drop(apartment);
                wasapi::deinitialize();
                let apartment = Apartment::query().unwrap();
                assert!(apartment.0, "uninitialized query must own a balanced MTA guard");
                drop(apartment);
                assert!(Apartment::query().unwrap().0);
            }).join().unwrap();
        }
    }

    #[test]
    fn capture_packets_handle_silence_null_and_device_bounds() {
        let ptr = std::ptr::null();
        assert_eq!(
            unsafe { copy_capture_packet(ptr, 2, 2, 8, true) }.unwrap(),
            vec![0; 16]
        );
        assert!(unsafe { copy_capture_packet(ptr, 2, 2, 8, false) }.is_err());
        assert!(unsafe { copy_capture_packet(ptr, 3, 2, 8, true) }.is_err());
        assert!(unsafe { copy_capture_packet(ptr, 1, 2, 0, true) }.is_err());
        assert_eq!(
            unsafe { copy_capture_packet(ptr, 2, 2, 64, true) }
                .unwrap()
                .len(),
            128
        );
        assert_eq!(
            unsafe { copy_capture_packet(ptr, 0, 2, 8, false) }.unwrap(),
            Vec::<u8>::new()
        );
        let samples = [0u8, 0, 128, 63, 0, 0, 128, 191];
        assert_eq!(
            unsafe { copy_capture_packet(samples.as_ptr(), 1, 2, 8, false) }.unwrap(),
            samples
        );
    }
    #[test]
    fn pcm_preserves_interleaved_stereo_and_sanitizes() {
        let bytes: Vec<_> = [1.0, -1.0, f32::NAN, f32::INFINITY]
            .into_iter()
            .flat_map(float_to_pcm)
            .collect();
        assert_eq!(bytes, [255, 127, 0, 128, 0, 0, 0, 0]);
        assert_eq!(48000 / 50 * 2 * 2, PCM_FRAME_BYTES);
    }
}
