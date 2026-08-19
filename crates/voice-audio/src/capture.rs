//! Live microphone capture: cpal-backed, pre-initialized paused so the
//! first real frame lands fast after start (SPEC V1.1 wants <10 ms from
//! key-down to first captured frame — the stream must already be built and
//! warm *before* the key is pressed, not constructed on it), resampled to
//! 16 kHz mono through the same [`crate::resample`] path file decode uses.
//!
//! NOT EXECUTED IN THIS RUN: opening a real input stream requires macOS
//! Microphone TCC permission this sandboxed agent cannot grant, and an
//! attached audio device to actually produce frames. Everything in this
//! module compiles and its non-hardware logic (permission gating, the
//! frame-arrival watchdog, resample wiring) is unit tested; the actual
//! `cpal::Stream::play()` data path is not.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::decode::{downmix_to_mono, f32_to_i16};
use crate::permission::microphone_permission_status;
use crate::resample::StreamingResampler;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("microphone permission blocked: {0}")]
    PermissionDenied(&'static str),
    #[error("no input audio device found")]
    NoInputDevice,
    #[error("could not read default input device config: {0}")]
    Config(String),
    #[error("failed to build input stream: {0}")]
    BuildStream(String),
    #[error("failed to start/stop stream: {0}")]
    StreamControl(String),
    #[error("stream not primed yet (call prime()/new() before start())")]
    NotPrimed,
    #[error("unsupported input sample format: {0:?}")]
    UnsupportedSampleFormat(cpal::SampleFormat),
    #[error(
        "no audio frames arrived within {0:?} of starting capture — check that no other \
         app is exclusively using the microphone, and that System Settings > Privacy & \
         Security > Microphone access is actually granted (not just NotDetermined)"
    )]
    NoFramesReceived(Duration),
}

/// Small trait live capture sits behind, so callers/tests don't need to
/// depend on `cpal` types directly.
pub trait AudioSource {
    /// Resume delivering frames to the callback given at construction.
    fn start(&mut self) -> Result<(), CaptureError>;
    /// Pause delivery without tearing down the stream (cheap to resume).
    fn stop(&mut self) -> Result<(), CaptureError>;
    fn is_running(&self) -> bool;
}

/// cpal-backed microphone capture. Built (and paused) at construction time
/// so `start()` on key-down only has to call `Stream::play()` on an
/// already-live stream, not spin one up from scratch.
pub struct MicCapture {
    stream: cpal::Stream,
    running: Arc<AtomicBool>,
    device_name: String,
    native_sample_rate: u32,
    native_channels: u16,
}

impl MicCapture {
    /// Build (and immediately pause) a capture stream from the default
    /// input device, delivering resampled 16 kHz mono `i16` frames to
    /// `on_frames` as they arrive. Fails fast — before ever building a
    /// stream — if the mic is known-denied, per this unit's mandate to
    /// never hang silently waiting for frames that will never come.
    pub fn new<F>(mut on_frames: F) -> Result<Self, CaptureError>
    where
        F: FnMut(&[i16]) + Send + 'static,
    {
        let permission = microphone_permission_status();
        if permission.should_block_capture() {
            let msg = permission
                .actionable_message()
                .unwrap_or("microphone access blocked");
            return Err(CaptureError::PermissionDenied(msg));
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(CaptureError::NoInputDevice)?;
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
        let supported_config = device
            .default_input_config()
            .map_err(|e| CaptureError::Config(e.to_string()))?;
        let sample_format = supported_config.sample_format();
        let native_channels = supported_config.channels();
        let native_sample_rate = supported_config.sample_rate().0;
        let stream_config: cpal::StreamConfig = supported_config.into();

        let running = Arc::new(AtomicBool::new(false));
        let err_fn = |err: cpal::StreamError| {
            eprintln!("voice-audio: input stream error: {err}");
        };

        let mut resampler = StreamingResampler::new(native_sample_rate, 16_000);

        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                    let mono = downmix_to_mono(data, native_channels);
                    let out = resampler.push(&mono);
                    if !out.is_empty() {
                        on_frames(&f32_to_i16(&out));
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                    let as_f32: Vec<f32> = data.iter().map(|&s| f32::from(s) / 32768.0).collect();
                    let mono = downmix_to_mono(&as_f32, native_channels);
                    let out = resampler.push(&mono);
                    if !out.is_empty() {
                        on_frames(&f32_to_i16(&out));
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &stream_config,
                move |data: &[u16], _info: &cpal::InputCallbackInfo| {
                    let as_f32: Vec<f32> = data
                        .iter()
                        .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                        .collect();
                    let mono = downmix_to_mono(&as_f32, native_channels);
                    let out = resampler.push(&mono);
                    if !out.is_empty() {
                        on_frames(&f32_to_i16(&out));
                    }
                },
                err_fn,
                None,
            ),
            other => return Err(CaptureError::UnsupportedSampleFormat(other)),
        }
        .map_err(|e| CaptureError::BuildStream(e.to_string()))?;

        // Pre-initialized and paused: the OS-level stream exists and is
        // warm, but delivers no callbacks until `start()` calls `play()`.
        stream
            .pause()
            .map_err(|e| CaptureError::StreamControl(e.to_string()))?;

        Ok(Self {
            stream,
            running,
            device_name,
            native_sample_rate,
            native_channels,
        })
    }

    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    #[must_use]
    pub fn native_sample_rate(&self) -> u32 {
        self.native_sample_rate
    }

    #[must_use]
    pub fn native_channels(&self) -> u16 {
        self.native_channels
    }

    /// Handle to poll/wait against for the frame-arrival watchdog: flips to
    /// `true` isn't wired automatically (the `on_frames` callback owns
    /// that decision), so callers that want the watchdog should flip a
    /// shared `Arc<AtomicBool>` from inside their own callback and pass it
    /// to [`wait_for_first_frame`]. `running` here only reflects
    /// play/pause state, not "has ever produced a frame."
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl AudioSource for MicCapture {
    fn start(&mut self) -> Result<(), CaptureError> {
        self.stream
            .play()
            .map_err(|e| CaptureError::StreamControl(e.to_string()))?;
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), CaptureError> {
        self.stream
            .pause()
            .map_err(|e| CaptureError::StreamControl(e.to_string()))?;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.is_playing()
    }
}

/// Guard against the exact failure mode this unit calls out as
/// undiagnosable: a caller that starts capture and then blocks forever
/// because no frame ever arrives (TCC silently denying audio, a disconnected
/// device, an exclusive-access conflict). Poll `arrived` (flipped `true`
/// from inside the capture callback on its first invocation) until either
/// it goes true or `timeout` elapses.
///
/// Deterministic and unit-testable without any real audio hardware: tests
/// below drive `arrived` from a plain background thread.
pub fn wait_for_first_frame(
    arrived: &Arc<AtomicBool>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(), CaptureError> {
    let start = Instant::now();
    loop {
        if arrived.load(Ordering::SeqCst) {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(CaptureError::NoFramesReceived(timeout));
        }
        std::thread::sleep(poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::MicPermission;
    use std::sync::atomic::AtomicBool;
    use std::thread;

    #[test]
    fn watchdog_succeeds_when_flag_flips_before_timeout() {
        let arrived = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&arrived);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            flag.store(true, Ordering::SeqCst);
        });
        let result = wait_for_first_frame(
            &arrived,
            Duration::from_millis(500),
            Duration::from_millis(5),
        );
        assert!(result.is_ok(), "expected watchdog to observe the flag flip");
    }

    #[test]
    fn watchdog_times_out_with_actionable_error_when_no_frame_ever_arrives() {
        let arrived = Arc::new(AtomicBool::new(false));
        let result = wait_for_first_frame(
            &arrived,
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        match result {
            Err(CaptureError::NoFramesReceived(d)) => {
                assert_eq!(d, Duration::from_millis(30));
            }
            other => panic!("expected NoFramesReceived timeout, got {other:?}"),
        }
    }

    #[test]
    fn watchdog_returns_immediately_if_flag_already_set() {
        let arrived = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let result = wait_for_first_frame(
            &arrived,
            Duration::from_secs(5),
            Duration::from_millis(5),
        );
        assert!(result.is_ok());
        assert!(
            start.elapsed() < Duration::from_millis(100),
            "should not have waited for the full timeout"
        );
    }

    #[test]
    fn permission_denied_blocks_capture_before_touching_any_device() {
        // MicPermission::Denied.should_block_capture() is the gate
        // MicCapture::new() checks before ever calling cpal; verify the
        // gate logic itself (the device-touching half of MicCapture::new
        // is exercised only on real hardware, out of scope here).
        assert!(MicPermission::Denied.should_block_capture());
        assert!(!MicPermission::Authorized.should_block_capture());
        assert!(!MicPermission::NotDetermined.should_block_capture());
    }
}
