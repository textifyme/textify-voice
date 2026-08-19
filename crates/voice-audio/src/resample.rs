//! Sample-rate conversion to the pipeline's fixed 16 kHz mono target.
//!
//! This is a windowed-sinc (bandlimited) resampler: a Blackman-windowed
//! sinc kernel evaluated at each output position. For downsampling the
//! sinc's cutoff is scaled down to the output Nyquist frequency, which
//! doubles as the anti-aliasing low-pass filter — a plain linear
//! interpolator would alias badly on e.g. 44.1 kHz -> 16 kHz. This is not a
//! commercial-grade resampler (no SIMD, fixed kernel width, no dithering)
//! but it is a correct, deterministic, dependency-free implementation
//! adequate for speech-bandwidth MVP audio. If quality ever becomes a
//! problem, swap this module for `rubato` without touching callers — both
//! [`resample_to_16k`] and [`StreamingResampler`] are the only entry points.

use std::collections::VecDeque;

/// Kernel half-width in output-adjacent input samples. 16 gives a
/// reasonably sharp transition band while staying cheap enough for
/// real-time use in the live-capture callback.
const HALF_WIDTH: i64 = 16;

const PI: f64 = std::f64::consts::PI;

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Blackman window evaluated at offset `x` from the kernel center, over a
/// support of `[-half_width, half_width]`.
fn blackman(x: f64, half_width: f64) -> f64 {
    if x.abs() > half_width {
        return 0.0;
    }
    let n = (x + half_width) / (2.0 * half_width); // 0..=1
    0.42 - 0.5 * (2.0 * PI * n).cos() + 0.08 * (4.0 * PI * n).cos()
}

/// Evaluate the resampled signal at continuous input-domain position `t`,
/// reading from `buffer` whose element `0` corresponds to absolute input
/// index `base_offset`. Indices outside `[base_offset, base_offset +
/// buffer.len())` contribute zero (silence padding at stream edges).
fn eval_at(buffer: &[f32], base_offset: i64, t: f64, cutoff: f64) -> f32 {
    let center = t.floor() as i64;
    let mut acc = 0.0f64;
    for k in -HALF_WIDTH..=HALF_WIDTH {
        let idx = center + k;
        let local = idx - base_offset;
        if local < 0 || local as usize >= buffer.len() {
            continue;
        }
        let x = t - idx as f64;
        let w = blackman(x, HALF_WIDTH as f64);
        let s = sinc(x * cutoff) * cutoff;
        acc += f64::from(buffer[local as usize]) * s * w;
    }
    acc as f32
}

/// Streaming windowed-sinc resampler: feed input in arbitrary-sized chunks
/// (as a live-capture callback delivers them) and get output samples back
/// incrementally, with correct kernel context carried across chunk
/// boundaries (no boundary artifacts from treating each callback buffer in
/// isolation). [`resample_to_16k`] is a thin one-shot wrapper over this
/// same struct, so file decode and live capture share one code path exactly
/// as required.
pub struct StreamingResampler {
    ratio: f64, // out_rate / in_rate
    cutoff: f64,
    identity: bool,
    buffer: VecDeque<f32>,
    /// Absolute input-sample index of `buffer[0]`.
    buffer_base: i64,
    /// Total input samples ever pushed (absolute index one past the last).
    total_input: i64,
    /// Absolute output-sample index of the next sample to produce.
    next_output: i64,
}

impl StreamingResampler {
    #[must_use]
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let ratio = f64::from(out_rate) / f64::from(in_rate);
        let cutoff = ratio.min(1.0);
        Self {
            ratio,
            cutoff,
            identity: in_rate == out_rate,
            buffer: VecDeque::new(),
            buffer_base: 0,
            total_input: 0,
            next_output: 0,
        }
    }

    /// Push a chunk of native-rate mono f32 samples (already downmixed) and
    /// get back however many 16 kHz output samples that unlocks. Safe to
    /// call with any chunk size, including 1 sample or thousands.
    pub fn push(&mut self, chunk: &[f32]) -> Vec<f32> {
        if self.identity {
            return chunk.to_vec();
        }
        self.buffer.extend(chunk.iter().copied());
        self.total_input += chunk.len() as i64;
        self.drain_ready(false)
    }

    /// Signal end of input: produce any remaining output whose kernel
    /// window still overlaps real (non-padded) samples, then reset.
    /// Subsequent `push` calls start a fresh stream.
    pub fn flush(&mut self) -> Vec<f32> {
        if self.identity {
            return Vec::new();
        }
        let out = self.drain_ready(true);
        self.buffer.clear();
        self.buffer_base = self.total_input;
        out
    }

    fn drain_ready(&mut self, flushing: bool) -> Vec<f32> {
        let mut out = Vec::new();
        if self.total_input == 0 {
            // Nothing has ever been pushed: there is no real data for any
            // output position's kernel window to overlap, even under
            // flush()'s relaxed bound.
            return out;
        }
        loop {
            let t = self.next_output as f64 / self.ratio;
            let bound = if flushing {
                // Allow output positions whose window still overlaps at
                // least one real sample.
                (self.total_input - 1) + HALF_WIDTH
            } else {
                (self.total_input - 1) - HALF_WIDTH
            };
            if t.floor() as i64 > bound {
                break;
            }
            let base: Vec<f32> = self.buffer.iter().copied().collect();
            let sample = eval_at(&base, self.buffer_base, t, self.cutoff);
            out.push(sample);
            self.next_output += 1;
        }
        // Trim history we no longer need: keep from (next t's floor -
        // HALF_WIDTH) onward.
        let next_t = self.next_output as f64 / self.ratio;
        let keep_from = next_t.floor() as i64 - HALF_WIDTH;
        while self.buffer_base < keep_from && !self.buffer.is_empty() {
            self.buffer.pop_front();
            self.buffer_base += 1;
        }
        out
    }
}

/// One-shot convenience wrapper: resample an entire buffer of native-rate
/// mono f32 samples to 16 kHz. Bit-exact passthrough when `in_rate ==
/// 16_000` (no filtering applied, matching what `feed_pcm` already expects
/// from a 16 kHz source).
#[must_use]
pub fn resample_to_16k(input: &[f32], in_rate: u32) -> Vec<f32> {
    let mut r = StreamingResampler::new(in_rate, 16_000);
    let mut out = r.push(input);
    out.extend(r.flush());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_passthrough_is_bit_exact_at_16k() {
        let input: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = resample_to_16k(&input, 16_000);
        assert_eq!(out, input);
    }

    #[test]
    fn downsample_44100_to_16000_preserves_duration() {
        let duration_s = 1.0;
        let n = (44_100.0 * duration_s) as usize;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 440.0 * i as f64 / 44_100.0).sin() as f32)
            .collect();
        let out = resample_to_16k(&input, 44_100);
        let expected = (16_000.0 * duration_s) as usize;
        // Allow a small tolerance from edge-window truncation.
        assert!(
            out.len().abs_diff(expected) < 50,
            "expected ~{expected} samples, got {}",
            out.len()
        );
    }

    #[test]
    fn downsample_preserves_approximate_frequency_via_zero_crossings() {
        // 440 Hz sine at 44.1 kHz for 1 second has ~880 zero crossings.
        // After resampling to 16 kHz it should still have ~880 (+/-
        // tolerance from windowing at the edges), proving the tone survived
        // the anti-aliasing filter rather than being destroyed or aliased
        // into a different frequency.
        let n = 44_100;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 440.0 * i as f64 / 44_100.0).sin() as f32)
            .collect();
        let out = resample_to_16k(&input, 44_100);
        let crossings = out
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        assert!(
            (800..960).contains(&crossings),
            "expected ~880 zero crossings for a 440Hz tone, got {crossings}"
        );
    }

    #[test]
    fn upsample_8000_to_16000_doubles_sample_count() {
        let n = 8000;
        let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = resample_to_16k(&input, 8_000);
        assert!(
            out.len().abs_diff(16_000) < 50,
            "expected ~16000 samples, got {}",
            out.len()
        );
    }

    #[test]
    fn streaming_matches_batch_within_tolerance() {
        let n = 20_000;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 300.0 * i as f64 / 44_100.0).sin() as f32)
            .collect();
        let batch = resample_to_16k(&input, 44_100);

        // Feed in small, unevenly-sized chunks to exercise cross-callback
        // continuity (the case that matters for the live-capture path).
        let mut r = StreamingResampler::new(44_100, 16_000);
        let mut streamed = Vec::new();
        let mut i = 0;
        let chunk_sizes = [37, 101, 256, 480, 13];
        let mut c = 0;
        while i < input.len() {
            let size = chunk_sizes[c % chunk_sizes.len()].min(input.len() - i);
            streamed.extend(r.push(&input[i..i + size]));
            i += size;
            c += 1;
        }
        streamed.extend(r.flush());

        assert!(
            streamed.len().abs_diff(batch.len()) <= 2,
            "streamed len {} vs batch len {}",
            streamed.len(),
            batch.len()
        );
        let n_cmp = streamed.len().min(batch.len());
        let max_diff = (0..n_cmp)
            .map(|i| (streamed[i] - batch[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_diff < 0.01,
            "streaming vs batch resample diverged: max_diff={max_diff}"
        );
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let out = resample_to_16k(&[], 44_100);
        assert!(out.is_empty());
    }
}
