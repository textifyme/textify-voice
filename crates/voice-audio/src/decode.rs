//! WAV file decode to the pipeline's fixed format: 16 kHz mono `i16` PCM,
//! exactly what `LocalAsr::feed_pcm` and `PcmRingBuffer` expect.
//!
//! Handles arbitrary input sample rate, channel count, and bit depth (8/16/
//! 24/32-bit integer PCM, or 32-bit float) by normalizing to `f32` in
//! `[-1.0, 1.0]`, downmixing to mono, and resampling through
//! [`crate::resample::resample_to_16k`].

use std::path::Path;

use crate::resample::resample_to_16k;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("failed to open/parse WAV file: {0}")]
    Wav(String),
    #[error("unsupported sample format: {0}")]
    Unsupported(String),
}

impl From<hound::Error> for DecodeError {
    fn from(e: hound::Error) -> Self {
        DecodeError::Wav(e.to_string())
    }
}

/// Summary stats proving decoded audio is real, non-silent, and correctly
/// scaled — used both by tests and by the `decode_report` bin for manual
/// verification.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioStats {
    pub sample_count: usize,
    pub duration_s: f64,
    pub peak_amplitude: i16,
    pub rms_amplitude: f64,
}

#[must_use]
pub fn compute_stats(samples: &[i16]) -> AudioStats {
    let sample_count = samples.len();
    let duration_s = sample_count as f64 / 16_000.0;
    let peak_amplitude = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as i16;
    let rms_amplitude = if sample_count == 0 {
        0.0
    } else {
        let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (sum_sq / sample_count as f64).sqrt()
    };
    AudioStats {
        sample_count,
        duration_s,
        peak_amplitude,
        rms_amplitude,
    }
}

/// Decode a WAV file to 16 kHz mono `i16` PCM, resampling and downmixing as
/// needed. This is the file-path entry point; live capture
/// (`crate::capture`) resamples through the identical
/// `StreamingResampler`/`resample_to_16k` code, not a separate
/// implementation.
pub fn decode_wav_file(path: impl AsRef<Path>) -> Result<Vec<i16>, DecodeError> {
    let mut reader = hound::WavReader::open(path.as_ref())
        .map_err(|e| DecodeError::Wav(format!("{}: {e}", path.as_ref().display())))?;
    let spec = reader.spec();

    let mono_f32: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(DecodeError::Unsupported(format!(
                    "{}-bit integer PCM",
                    spec.bits_per_sample
                )));
            }
            let max_val = (1i64 << (spec.bits_per_sample - 1)) as f32;
            let raw: Vec<f32> = reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max_val))
                .collect::<Result<_, _>>()?;
            downmix_to_mono(&raw, spec.channels)
        }
        hound::SampleFormat::Float => {
            let raw: Vec<f32> = reader.samples::<f32>().collect::<Result<_, _>>()?;
            downmix_to_mono(&raw, spec.channels)
        }
    };

    let resampled = resample_to_16k(&mono_f32, spec.sample_rate);
    Ok(f32_to_i16(&resampled))
}

/// Average interleaved multi-channel samples down to mono. No-op (clone)
/// for `channels <= 1`.
#[must_use]
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let ch = usize::from(channels);
    interleaved
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

/// Convert `f32` samples in `[-1.0, 1.0]` to `i16` PCM, clamping
/// out-of-range values instead of wrapping (a resampler overshoot near a
/// hard clip should saturate, not roll over to the opposite sign).
#[must_use]
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/audio")
            .join(name)
    }

    #[test]
    fn decodes_short_5s_wav_non_silent_and_correctly_scaled() {
        let samples = decode_wav_file(fixture("short-5s.wav")).expect("decode short-5s.wav");
        let stats = compute_stats(&samples);
        println!("short-5s.wav: {stats:?}");
        // Reference transcript duration is ~3.88s.
        assert!(
            (stats.duration_s - 3.878).abs() < 0.05,
            "duration {} not close to 3.878s",
            stats.duration_s
        );
        assert!(
            stats.peak_amplitude > 5000,
            "peak {} too low to be real speech",
            stats.peak_amplitude
        );
        assert!(
            stats.rms_amplitude > 500.0,
            "rms {} too low to be real speech",
            stats.rms_amplitude
        );
    }

    #[test]
    fn decodes_ref_3min_wav_non_silent_and_correctly_scaled() {
        let samples = decode_wav_file(fixture("ref-3min.wav")).expect("decode ref-3min.wav");
        let stats = compute_stats(&samples);
        println!("ref-3min.wav: {stats:?}");
        assert!(
            (stats.duration_s - 121.363).abs() < 0.1,
            "duration {} not close to 121.363s",
            stats.duration_s
        );
        assert!(
            stats.peak_amplitude > 5000,
            "peak {} too low to be real speech",
            stats.peak_amplitude
        );
        assert!(
            stats.rms_amplitude > 500.0,
            "rms {} too low to be real speech",
            stats.rms_amplitude
        );
    }

    #[test]
    fn downmix_stereo_averages_channels() {
        // L=1.0, R=-1.0 -> mono 0.0; L=0.5, R=0.5 -> mono 0.5.
        let interleaved = vec![1.0, -1.0, 0.5, 0.5];
        let mono = downmix_to_mono(&interleaved, 2);
        assert_eq!(mono, vec![0.0, 0.5]);
    }

    #[test]
    fn downmix_mono_is_noop() {
        let mono_in = vec![0.1, 0.2, 0.3];
        assert_eq!(downmix_to_mono(&mono_in, 1), mono_in);
    }

    #[test]
    fn f32_to_i16_scales_and_clamps() {
        assert_eq!(f32_to_i16(&[0.0]), vec![0]);
        assert_eq!(f32_to_i16(&[1.0]), vec![32767]);
        assert_eq!(f32_to_i16(&[-1.0]), vec![-32767]);
        // Overshoot from resampler ringing must clamp, not wrap.
        assert_eq!(f32_to_i16(&[1.5]), vec![32767]);
        assert_eq!(f32_to_i16(&[-1.5]), vec![-32767]);
    }

    #[test]
    fn decode_missing_file_is_a_clean_error_not_a_panic() {
        let err = decode_wav_file("/nonexistent/path/does-not-exist.wav").unwrap_err();
        assert!(matches!(err, DecodeError::Wav(_)));
    }

    #[test]
    fn decode_resamples_and_downmixes_a_synthetic_44100_stereo_wav() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("voice_audio_test_{}.wav", std::process::id()));
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for i in 0..44_100u32 {
                let t = i as f64 / 44_100.0;
                let v = (2.0 * std::f64::consts::PI * 440.0 * t).sin();
                let s = (v * 20000.0) as i16;
                writer.write_sample(s).unwrap(); // left
                writer.write_sample(s).unwrap(); // right (identical -> downmix should equal same value)
            }
            writer.finalize().unwrap();
        }

        let samples = decode_wav_file(&path).expect("decode synthetic wav");
        let stats = compute_stats(&samples);
        std::fs::remove_file(&path).ok();

        // 1 second of 44.1kHz audio resampled to 16kHz should be ~16000 samples.
        assert!(
            stats.sample_count.abs_diff(16_000) < 50,
            "sample_count {} not close to 16000",
            stats.sample_count
        );
        assert!(stats.peak_amplitude > 10000, "tone should be loud");
    }
}
