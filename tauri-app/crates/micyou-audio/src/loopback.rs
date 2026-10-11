/*
 * MicYou — Turns your Android device into a high-quality PC microphone.
 * Copyright (C) 2026 LanRhyme <https://github.com/MicYou-Dev/MicYou>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 */

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, RingBuffer};
use ringbuf::HeapRb;

use crate::AecFailure;

const RING_BUF_SEC: usize = 2;
const TARGET_RATE: u32 = 48000;
const MAX_REFERENCE_LAG_SAMPLES: usize = TARGET_RATE as usize * 300 / 1000;
const CAPTURE_JOIN_TIMEOUT: Duration = Duration::from_millis(500);
const CAPTURE_JOIN_POLL: Duration = Duration::from_millis(10);

/// Cross-platform speaker loopback capture for AEC far-end reference.
///
/// - Windows: WASAPI loopback on default render device (no virtual device needed)
/// - macOS: cpal input from BlackHole
/// - Linux: pw-record on the default physical playback sink monitor
pub struct LoopbackCapture {
    active: Arc<AtomicBool>,
    buffer: Arc<Mutex<HeapRb<f32>>>,
    failure: Arc<Mutex<Option<AecFailure>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    reference: Arc<Mutex<ReferenceState>>,
}

#[derive(Default, Clone, Debug)]
pub struct ReferenceState {
    pub generation: u64,
    pub desired_endpoint_id: Option<String>,
    pub actual_endpoint_id: Option<String>,
    pub ready: bool,
}

impl LoopbackCapture {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
            buffer: Arc::new(Mutex::new(HeapRb::new(TARGET_RATE as usize * RING_BUF_SEC))),
            failure: Arc::new(Mutex::new(None)),
            thread: Mutex::new(None),
            reference: Arc::new(Mutex::new(ReferenceState::default())),
        }
    }

    pub fn start(&self) -> Result<(), AecFailure> {
        if self.active.load(Ordering::Relaxed) {
            return Ok(());
        }

        // A previous stop may have requested shutdown while the capture
        // thread is still winding down. Join it before reusing the shared
        // state; otherwise it can observe the new `true` flag and keep
        // running alongside the new capture thread.
        if !self.join_capture_thread(CAPTURE_JOIN_TIMEOUT) {
            log::error!(
                "[Loopback] Previous capture thread did not stop within {:?}",
                CAPTURE_JOIN_TIMEOUT
            );
            set_failure(&self.failure, AecFailure::ReferenceLost);
            return Err(AecFailure::ReferenceLost);
        }

        *lock(&self.failure) = None;
        lock(&self.buffer).clear();
        self.active.store(true, Ordering::Relaxed);
        let (generation, desired) = {
            let mut reference = lock(&self.reference);
            reference.generation = reference.generation.wrapping_add(1);
            reference.ready = false;
            reference.actual_endpoint_id = None;
            (reference.generation, reference.desired_endpoint_id.clone())
        };
        let reference = self.reference.clone();

        let active = self.active.clone();
        let buffer = self.buffer.clone();
        let failure = self.failure.clone();

        let spawned = std::thread::Builder::new()
            .name("SpeakerLoopback".into())
            .spawn(move || {
                #[cfg(target_os = "windows")]
                wasapi_loopback_thread(
                    active,
                    buffer,
                    failure,
                    desired,
                    reference.clone(),
                    generation,
                );
                #[cfg(target_os = "linux")]
                pipewire_loopback_thread(active, buffer, failure);
                #[cfg(target_os = "macos")]
                cpal_capture_thread(active, buffer, failure);
                #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
                {
                    set_failure(&failure, AecFailure::ReferenceLost);
                    active.store(false, Ordering::Relaxed);
                }
                clear_reference_generation(&reference, generation);
            });

        match spawned {
            Ok(thread) => {
                *lock(&self.thread) = Some(thread);
                Ok(())
            }
            Err(error) => {
                log::error!("[Loopback] Failed to spawn capture thread: {error}");
                self.active.store(false, Ordering::Relaxed);
                set_failure(&self.failure, AecFailure::ReferenceLost);
                Err(AecFailure::ReferenceLost)
            }
        }
    }

    pub fn stop(&self) {
        self.active.store(false, Ordering::Relaxed);
        {
            let mut reference = lock(&self.reference);
            reference.ready = false;
            reference.actual_endpoint_id = None;
        }

        // Give the capture stream a bounded window to release its device. A
        // timed-out handle remains stored, so start() cannot overlap a new
        // capture with a stuck old one and can reap it after it eventually exits.
        if !self.join_capture_thread(CAPTURE_JOIN_TIMEOUT) {
            log::warn!(
                "[Loopback] Capture thread is still stopping after {:?}",
                CAPTURE_JOIN_TIMEOUT
            );
        }
    }

    pub fn reference_state(&self) -> ReferenceState {
        lock(&self.reference).clone()
    }

    pub fn start_for_endpoint(&self, endpoint: String) -> Result<(), AecFailure> {
        let changed = lock(&self.reference).desired_endpoint_id.as_ref() != Some(&endpoint);
        if changed {
            self.stop();
            lock(&self.reference).desired_endpoint_id = Some(endpoint);
        }
        self.start()
    }

    pub fn set_desired_endpoint(&self, endpoint: Option<String>) {
        lock(&self.reference).desired_endpoint_id = endpoint;
    }

    pub fn capture_thread_stopped(&self) -> bool {
        self.join_capture_thread(Duration::ZERO)
    }

    fn join_capture_thread(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let finished = {
                let mut thread = lock(&self.thread);
                match thread.as_ref() {
                    None => return true,
                    Some(handle) if handle.is_finished() => thread.take(),
                    Some(_) => None,
                }
            };

            if let Some(handle) = finished {
                if handle.join().is_err() {
                    log::warn!("[Loopback] Capture thread panicked while stopping");
                }
                return true;
            }

            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(CAPTURE_JOIN_POLL);
        }
    }

    /// Stop capture and discard stream-local state before a new transport session.
    pub fn reset_session(&self) {
        self.stop();
        *lock(&self.failure) = None;
        lock(&self.buffer).clear();
    }

    /// Read exactly `n_samples` from the loopback timeline, consuming available
    /// samples and padding an underrun with silence. Advancing the timeline even
    /// while capture is starting prevents old reference samples from being paired
    /// with later near-end frames.
    pub fn read(&self, n_samples: usize) -> Vec<f32> {
        let mut buf = lock(&self.buffer);

        // If the near-end stream paused while playback continued, discard old
        // reference audio instead of preserving a permanent multi-second lag.
        let stale_samples = buf
            .occupied_len()
            .saturating_sub(MAX_REFERENCE_LAG_SAMPLES + n_samples);
        buf.skip(stale_samples);

        // Missing reference audio reads as silence.
        let mut out = vec![0.0; n_samples];
        buf.pop_slice(&mut out);
        out
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Takes the most recent capture failure so one failed attempt is handled once.
    pub fn take_failure_reason(&self) -> Option<AecFailure> {
        lock(&self.failure).take()
    }
}

fn clear_reference_generation(reference: &Mutex<ReferenceState>, generation: u64) {
    let mut state = lock(reference);
    if state.generation == generation {
        state.ready = false;
        state.actual_endpoint_id = None;
    }
}

impl Default for LoopbackCapture {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn set_failure(failure: &Mutex<Option<AecFailure>>, reason: AecFailure) {
    *lock(failure) = Some(reason);
}

// ─── Helper: downmix + resample + push to buffer ─────────────────────────

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn push_to_buffer(
    data: &[f32],
    channels: usize,
    _device_rate: u32,
    resampler: &Option<Arc<Mutex<crate::engine::RubatoResampler>>>,
    buffer: &Mutex<HeapRb<f32>>,
) {
    // Downmix to mono
    let mono: Vec<f32> = if channels > 1 {
        data.chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        data.to_vec()
    };

    // Resample to 48kHz if needed
    let resampled = if let Some(ref r) = resampler {
        if let Ok(mut resampler) = r.lock() {
            let mut out = Vec::new();
            resampler.resample(&mono, 1, &mut out);
            out
        } else {
            mono
        }
    } else {
        mono
    };

    if let Ok(mut buf) = buffer.lock() {
        for &s in &resampled {
            buf.push_overwrite(s);
        }
    }
}

// ─── Windows: WASAPI loopback on default render device ────────────────────
//
// The trick: get the default render (speaker) device, then initialize its
// IAudioClient with Direction::Capture + ShareMode::Shared.  The wasapi crate
// automatically adds AUDCLNT_STREAMFLAGS_LOOPBACK in this combination
// (see api.rs line 832-835).

#[cfg(target_os = "windows")]
fn wasapi_loopback_thread(
    active: Arc<AtomicBool>,
    buffer: Arc<Mutex<HeapRb<f32>>>,
    failure: Arc<Mutex<Option<AecFailure>>>,
    desired: Option<String>,
    reference: Arc<Mutex<ReferenceState>>,
    generation: u64,
) {
    use wasapi::*;
    use windows::{
        core::PCWSTR,
        Win32::{
            Media::Audio::*,
            System::Com::{CoCreateInstance, CLSCTX_ALL},
        },
    };

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let _apartment = crate::stereo_loopback::Apartment::enter()?;

        let enumerator = DeviceEnumerator::new()?;
        let device = if let Some(desired) = desired {
            enumerator.get_device(&desired)?
        } else {
            enumerator.get_default_device(&Direction::Render)?
        };
        let actual_id = device.get_id()?;
        let device_name = device.get_friendlyname().unwrap_or_default();
        log::info!("[Loopback] WASAPI: capturing from '{}'", device_name);

        let format_client = device.get_iaudioclient()?;
        let mix_format = format_client.get_mixformat()?;
        let channels = mix_format.get_nchannels() as usize;
        let device_rate = mix_format.get_samplespersec();
        if channels == 0
            || mix_format.get_bitspersample() != 32
            || mix_format.get_subformat()? != SampleType::Float
        {
            return Err("unsupported loopback mix format".into());
        }

        log::info!(
            "[Loopback] WASAPI device format: {}Hz {}ch",
            device_rate,
            channels
        );

        let native_enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let id: Vec<u16> = actual_id.encode_utf16().chain(Some(0)).collect();
        let native_device = unsafe { native_enumerator.GetDevice(PCWSTR(id.as_ptr()))? };
        let audio_client: IAudioClient = unsafe { native_device.Activate(CLSCTX_ALL, None)? };
        unsafe {
            audio_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                200_000,
                0,
                mix_format.as_waveformatex_ref(),
                None,
            )?;
        }
        let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService()? };
        let max_frames = unsafe { audio_client.GetBufferSize()? } as usize;

        // Create resampler if device is not 48kHz
        let resampler = if device_rate != TARGET_RATE {
            match crate::engine::RubatoResampler::new(device_rate, TARGET_RATE, 1) {
                Ok(r) => Some(Arc::new(Mutex::new(r))),
                Err(e) => {
                    log::error!("[Loopback] Failed to create resampler: {}", e);
                    None
                }
            }
        } else {
            None
        };

        unsafe {
            audio_client.Start()?;
        }
        {
            let mut state = lock(&reference);
            if state.generation != generation || !active.load(Ordering::Relaxed) {
                unsafe {
                    audio_client.Stop()?;
                }
                return Ok(());
            }
            state.actual_endpoint_id = Some(actual_id);
            state.ready = true;
        }
        log::info!("[Loopback] WASAPI loopback started");

        let bytes_per_frame = mix_format.get_blockalign() as usize;
        let mut total_frames: u64 = 0;

        while active.load(Ordering::Relaxed) {
            let available = unsafe { capture_client.GetNextPacketSize()? };
            if available == 0 {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let mut ptr = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            unsafe {
                capture_client.GetBuffer(&mut ptr, &mut frames, &mut flags, None, None)?;
            }
            let bytes = unsafe {
                crate::stereo_loopback::copy_capture_packet(
                    ptr,
                    frames as usize,
                    max_frames,
                    bytes_per_frame,
                    flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0,
                )
            };
            unsafe {
                capture_client.ReleaseBuffer(frames)?;
            }
            let bytes = bytes?;
            if !bytes.is_empty() {
                let slice = bytes.as_slice();
                let f32_samples: Vec<f32> = slice
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect();

                if !f32_samples.is_empty() {
                    push_to_buffer(&f32_samples, channels, device_rate, &resampler, &buffer);
                    total_frames += (f32_samples.len() / channels) as u64;
                }
            }
        }

        unsafe {
            audio_client.Stop()?;
        }
        log::info!(
            "[Loopback] WASAPI loopback stopped, {} frames captured",
            total_frames
        );
        Ok(())
    })();

    if let Err(e) = result {
        log::error!("[Loopback] WASAPI error: {}", e);
        if active.load(Ordering::Relaxed) {
            set_failure(&failure, AecFailure::ReferenceLost);
        }
    }
    active.store(false, Ordering::Relaxed);
}

// ─── Linux: PipeWire capture from the physical playback sink ──────────────

#[cfg(target_os = "linux")]
fn pipewire_loopback_thread(
    active: Arc<AtomicBool>,
    buffer: Arc<Mutex<HeapRb<f32>>>,
    failure: Arc<Mutex<Option<AecFailure>>>,
) {
    use std::io::Read;
    use std::process::{Command, Stdio};

    // Capture the monitor of the current default playback sink. MicYouVirtualMic
    // contains MicYou's own microphone output and is therefore not a valid AEC
    // far-end reference.
    let mut child = match Command::new("pw-record")
        .args([
            "--target",
            "@DEFAULT_AUDIO_SINK@",
            "--properties",
            "{ stream.capture.sink = true node.name = MicYouAecCapture }",
            "--latency",
            "10ms",
            "--rate",
            "48000",
            "--channels",
            "1",
            "--format",
            "f32",
            "--raw",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            log::error!("[Loopback] Failed to start pw-record: {}", error);
            set_failure(&failure, AecFailure::PipeWireUnavailable);
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    let Some(mut stdout) = child.stdout.take() else {
        log::error!("[Loopback] pw-record stdout is unavailable");
        let _ = child.kill();
        let _ = child.wait();
        set_failure(&failure, AecFailure::ReferenceLost);
        active.store(false, Ordering::Relaxed);
        return;
    };

    let reader_active = active.clone();
    let reader_buffer = buffer.clone();
    let reader_failure = failure.clone();
    let reader = std::thread::Builder::new()
        .name("PipeWireReferenceReader".into())
        .spawn(move || {
            let mut read_buffer = [0_u8; 4096];
            let mut pending = Vec::with_capacity(read_buffer.len() + 3);

            loop {
                match stdout.read(&mut read_buffer) {
                    Ok(0) => {
                        if reader_active.load(Ordering::Relaxed) {
                            set_failure(&reader_failure, AecFailure::ReferenceLost);
                            reader_active.store(false, Ordering::Relaxed);
                        }
                        break;
                    }
                    Ok(count) => {
                        pending.extend_from_slice(&read_buffer[..count]);
                        let complete_bytes = pending.len() / 4 * 4;
                        if complete_bytes == 0 {
                            continue;
                        }

                        if let Ok(mut ring) = reader_buffer.lock() {
                            for bytes in pending[..complete_bytes].as_chunks::<4>().0 {
                                ring.push_overwrite(f32::from_ne_bytes(*bytes));
                            }
                        }
                        pending.drain(..complete_bytes);
                    }
                    Err(error) => {
                        if reader_active.load(Ordering::Relaxed) {
                            log::error!("[Loopback] Failed to read PipeWire reference: {}", error);
                            set_failure(&reader_failure, AecFailure::ReferenceLost);
                            reader_active.store(false, Ordering::Relaxed);
                        }
                        break;
                    }
                }
            }
        });

    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            log::error!("[Loopback] Failed to spawn PipeWire reader: {}", error);
            let _ = child.kill();
            let _ = child.wait();
            set_failure(&failure, AecFailure::ReferenceLost);
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    log::info!("[Loopback] PipeWire default playback monitor started");
    while active.load(Ordering::Relaxed) {
        match child.try_wait() {
            Ok(Some(status)) => {
                log::error!("[Loopback] pw-record exited unexpectedly: {}", status);
                set_failure(&failure, AecFailure::ReferenceLost);
                active.store(false, Ordering::Relaxed);
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(error) => {
                log::error!("[Loopback] Failed to inspect pw-record: {}", error);
                set_failure(&failure, AecFailure::ReferenceLost);
                active.store(false, Ordering::Relaxed);
                break;
            }
        }
    }

    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = reader.join();
    active.store(false, Ordering::Relaxed);
    log::info!("[Loopback] PipeWire default playback monitor stopped");
}

// ─── macOS: cpal capture from BlackHole ──────────────────────────────────

#[cfg(target_os = "macos")]
fn cpal_capture_thread(
    active: Arc<AtomicBool>,
    buffer: Arc<Mutex<HeapRb<f32>>>,
    failure: Arc<Mutex<Option<AecFailure>>>,
) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();

    let device = {
        let mut found = None;
        if let Ok(devices) = host.input_devices() {
            'outer: for dev in devices {
                if let Some(name) = crate::device_name(&dev) {
                    let lower = name.to_lowercase();
                    let matches = lower.contains("blackhole");
                    if matches {
                        log::info!("[Loopback] Found virtual device: '{}'", name);
                        found = Some(dev);
                        break 'outer;
                    }
                }
            }
        }
        match found {
            Some(d) => d,
            None => {
                log::error!(
                    "[Loopback] No virtual audio device found. \
                     Install BlackHole (macOS) or start MicYou PipeWire routing (Linux)."
                );
                set_failure(&failure, AecFailure::VirtualSourceMissing);
                active.store(false, Ordering::Relaxed);
                return;
            }
        }
    };

    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            log::error!("[Loopback] Failed to get input config: {}", e);
            set_failure(&failure, AecFailure::ReferenceLost);
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    let channels = config.channels() as usize;
    let device_rate = config.sample_rate();
    let sample_format = config.sample_format();

    log::info!(
        "[Loopback] cpal capture started: {}Hz {}ch",
        device_rate,
        channels
    );

    let resampler = if device_rate != TARGET_RATE {
        match crate::engine::RubatoResampler::new(device_rate, TARGET_RATE, 1) {
            Ok(r) => Some(Arc::new(Mutex::new(r))),
            Err(e) => {
                log::error!("[Loopback] Failed to create resampler: {}", e);
                set_failure(&failure, AecFailure::ReferenceLost);
                None
            }
        }
    } else {
        None
    };

    let stream_active = active.clone();
    let stream_failure = failure.clone();
    let err_fn = move |err: cpal::Error| {
        // cpal repeats the callback while the device stays broken; only the
        // first error after the stream was active is worth reporting
        if stream_active.swap(false, Ordering::Relaxed) {
            log::error!("[Loopback] Stream error: {}", err);
            set_failure(&stream_failure, AecFailure::ReferenceLost);
        }
    };

    let buf_clone = buffer.clone();
    let active_clone = active.clone();
    let resampler_clone = resampler.clone();

    let stream_result = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push_to_buffer(data, channels, device_rate, &resampler_clone, &buf_clone);
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let f32_data: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                push_to_buffer(
                    &f32_data,
                    channels,
                    device_rate,
                    &resampler_clone,
                    &buf_clone,
                );
            },
            err_fn,
            None,
        ),
        // cpal 0.18 ranks I32 above I16 when picking the default format.
        cpal::SampleFormat::I32 => device.build_input_stream(
            config.into(),
            move |data: &[i32], _: &cpal::InputCallbackInfo| {
                let f32_data: Vec<f32> = data.iter().map(|&s| s as f32 / 2_147_483_648.0).collect();
                push_to_buffer(
                    &f32_data,
                    channels,
                    device_rate,
                    &resampler_clone,
                    &buf_clone,
                );
            },
            err_fn,
            None,
        ),
        fmt => {
            log::error!("[Loopback] Unsupported sample format: {:?}", fmt);
            set_failure(&failure, AecFailure::ReferenceLost);
            active.store(false, Ordering::Relaxed);
            return;
        }
    };

    match stream_result {
        Ok(stream) => {
            if let Err(e) = stream.play() {
                log::error!("[Loopback] Failed to start stream: {}", e);
                set_failure(&failure, AecFailure::ReferenceLost);
                active.store(false, Ordering::Relaxed);
                return;
            }

            while active_clone.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            drop(stream);
            log::info!("[Loopback] Stopped");
        }
        Err(e) => {
            log::error!("[Loopback] Failed to build stream: {}", e);
            set_failure(&failure, AecFailure::ReferenceLost);
            active.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Producer;

    #[test]
    fn stopped_reference_retains_desired_and_discards_actual() {
        let capture = LoopbackCapture::new();
        *lock(&capture.reference) = ReferenceState {
            generation: 2,
            desired_endpoint_id: Some("desired".into()),
            actual_endpoint_id: Some("actual".into()),
            ready: true,
        };
        capture.buffer.lock().unwrap().try_push(1.0).unwrap();
        capture.reset_session();
        let reference = capture.reference_state();
        assert_eq!(reference.desired_endpoint_id.as_deref(), Some("desired"));
        assert!(reference.actual_endpoint_id.is_none());
        assert!(!reference.ready);
        assert_eq!(capture.buffer.lock().unwrap().occupied_len(), 0);
    }

    #[test]
    fn old_generation_cleanup_does_not_clear_new_reference() {
        let reference = Mutex::new(ReferenceState {
            generation: 3,
            desired_endpoint_id: Some("new".into()),
            actual_endpoint_id: Some("new".into()),
            ready: true,
        });
        clear_reference_generation(&reference, 2);
        assert!(lock(&reference).ready);
        clear_reference_generation(&reference, 3);
        assert!(!lock(&reference).ready);
        assert_eq!(lock(&reference).desired_endpoint_id.as_deref(), Some("new"));
    }

    #[test]
    fn capture_failure_is_consumed_once() {
        let capture = LoopbackCapture::new();
        set_failure(&capture.failure, AecFailure::VirtualSourceMissing);

        assert_eq!(
            capture.take_failure_reason(),
            Some(AecFailure::VirtualSourceMissing)
        );
        assert_eq!(capture.take_failure_reason(), None);
    }

    #[test]
    fn reference_reads_follow_requested_near_end_frame_count() {
        let capture = LoopbackCapture::new();
        {
            let mut buffer = capture.buffer.lock().unwrap();
            for sample in 0..700 {
                buffer.try_push(sample as f32).unwrap();
            }
        }

        let reference = capture.read(660);
        assert_eq!(reference.len(), 660);
        assert_eq!(reference.first().copied(), Some(0.0));
        assert_eq!(reference.last().copied(), Some(659.0));
        let underrun = capture.read(480);
        assert_eq!(
            &underrun[..40],
            &(660..700).map(|sample| sample as f32).collect::<Vec<_>>()
        );
        assert!(underrun[40..].iter().all(|sample| *sample == 0.0));
        assert_eq!(capture.buffer.lock().unwrap().occupied_len(), 0);
    }

    #[test]
    fn reference_read_discards_excessive_lag() {
        let capture = LoopbackCapture::new();
        let sample_count = MAX_REFERENCE_LAG_SAMPLES + 960;
        {
            let mut buffer = capture.buffer.lock().unwrap();
            for sample in 0..sample_count {
                buffer.try_push(sample as f32).unwrap();
            }
        }

        let reference = capture.read(480);

        assert_eq!(reference.first(), Some(&480.0));
        assert_eq!(reference.last(), Some(&959.0));
        assert_eq!(
            capture.buffer.lock().unwrap().occupied_len(),
            MAX_REFERENCE_LAG_SAMPLES
        );
    }

    #[test]
    fn reset_session_clears_buffer_and_stale_failure() {
        let capture = LoopbackCapture::new();
        capture.buffer.lock().unwrap().try_push(1.0).unwrap();
        set_failure(&capture.failure, AecFailure::ReferenceLost);

        capture.reset_session();

        assert_eq!(capture.buffer.lock().unwrap().occupied_len(), 0);
        assert_eq!(capture.take_failure_reason(), None);
        assert!(!capture.is_active());
    }

    #[test]
    fn reference_underrun_advances_timeline_with_silence() {
        let capture = LoopbackCapture::new();
        {
            let mut buffer = capture.buffer.lock().unwrap();
            buffer.try_push(1.0).unwrap();
            buffer.try_push(2.0).unwrap();
        }

        let reference = capture.read(4);

        assert_eq!(reference, vec![1.0, 2.0, 0.0, 0.0]);
        assert_eq!(capture.buffer.lock().unwrap().occupied_len(), 0);
    }

    #[test]
    fn timed_out_join_keeps_handle_for_later_reap() {
        let capture = LoopbackCapture::new();
        let release = Arc::new(AtomicBool::new(false));
        let release_thread = release.clone();
        let handle = std::thread::spawn(move || {
            while !release_thread.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
        });
        *capture.thread.lock().unwrap() = Some(handle);

        assert!(!capture.join_capture_thread(Duration::ZERO));
        assert!(capture.thread.lock().unwrap().is_some());

        release.store(true, Ordering::Relaxed);
        assert!(capture.join_capture_thread(Duration::from_secs(1)));
        assert!(capture.thread.lock().unwrap().is_none());
    }

    #[test]
    fn changed_target_cannot_start_until_old_thread_exits() {
        let capture = LoopbackCapture::new();
        capture.set_desired_endpoint(Some("old".into()));
        let (release, wait) = std::sync::mpsc::channel::<()>();
        *lock(&capture.thread) = Some(std::thread::spawn(move || {
            let _ = wait.recv();
        }));
        let result = capture.start_for_endpoint("new".into());
        release.send(()).unwrap();
        assert_eq!(result, Err(AecFailure::ReferenceLost));
        assert_eq!(capture.reference_state().generation, 0);
        assert_eq!(
            capture.reference_state().desired_endpoint_id.as_deref(),
            Some("new")
        );
        assert!(capture.join_capture_thread(Duration::from_secs(1)));
    }
}
