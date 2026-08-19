//! The two short tones that bracket an utterance.
//!
//! Feedback for a push-to-talk key has to arrive before the waveform can — you
//! press the key while looking at *another* window, so the ear confirms the
//! press and the eye confirms it is hearing you. A rising blip on press and a
//! falling one on release also encode direction, so "did that register?" and
//! "did that end?" are distinguishable without looking at anything.
//!
//! The tones are synthesized rather than shipped as assets: two sine sweeps
//! with a soft attack and an exponential decay, rendered into an in-memory WAV
//! and handed to `NSSound`. No audio files in the repo, no output stream to
//! manage, and no dependency beyond the AppKit we already link.
//!
//! Deliberately quiet (peak ≈ 0.16 full scale) and short (<130 ms). This fires
//! on every utterance all day; anything longer or louder stops being feedback
//! and becomes a nuisance.

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_app_kit::NSSound;
use objc2_foundation::NSData;

const SAMPLE_RATE: u32 = 44_100;
const AMPLITUDE: f32 = 0.16;

/// Render a sine sweep from `f0` to `f1` Hz as 16-bit mono PCM.
///
/// The envelope matters more than the pitch: a hard start or stop produces an
/// audible click, which reads as a glitch rather than as feedback.
fn sweep(f0: f32, f1: f32, ms: u32) -> Vec<i16> {
    let n = (SAMPLE_RATE as f32 * (ms as f32 / 1000.0)) as usize;
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    // ~4 ms of fade at each end to kill the click.
    let fade = (SAMPLE_RATE as f32 * 0.004) as usize;
    for i in 0..n {
        let t = i as f32 / n as f32;
        let freq = f0 + (f1 - f0) * t;
        phase += std::f32::consts::TAU * freq / SAMPLE_RATE as f32;

        // Exponential decay gives the tone a struck quality rather than a beep.
        let decay = (-3.2 * t).exp();
        let attack = (i as f32 / fade as f32).min(1.0);
        let release = ((n - i) as f32 / fade as f32).min(1.0);

        // A touch of second harmonic warms it up; a bare sine sounds synthetic.
        let s = phase.sin() + 0.22 * (phase * 2.0).sin();
        let v = s * AMPLITUDE * decay * attack * release;
        out.push((v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
    }
    out
}

/// Wrap PCM in a minimal 44-byte canonical WAV header.
fn wav(pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    b.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    b.extend_from_slice(&2u16.to_le_bytes()); // block align
    b.extend_from_slice(&16u16.to_le_bytes()); // bits
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

/// Both cues, decoded once at startup. Building an `NSSound` per press would
/// add avoidable latency to the exact moment that needs to feel instant.
pub struct Tones {
    start: Retained<NSSound>,
    stop: Retained<NSSound>,
}

impl Tones {
    pub fn new() -> anyhow::Result<Self> {
        // Rising, bright: "listening". Falling, settling: "got it".
        let start = load(&wav(&sweep(540.0, 830.0, 95.0 as u32)))?;
        let stop = load(&wav(&sweep(760.0, 480.0, 115.0 as u32)))?;
        Ok(Self { start, stop })
    }

    pub fn press(&self) {
        play(&self.start);
    }

    pub fn release(&self) {
        play(&self.stop);
    }
}

fn load(bytes: &[u8]) -> anyhow::Result<Retained<NSSound>> {
    let data = NSData::with_bytes(bytes);
    NSSound::initWithData(NSSound::alloc(), &data)
        .ok_or_else(|| anyhow::anyhow!("NSSound rejected the generated WAV"))
}

/// Rewind before playing: holding the key again while the previous cue is
/// still ringing must retrigger, not be swallowed.
fn play(sound: &NSSound) {
    if sound.isPlaying() {
        sound.stop();
    }
    sound.setCurrentTime(0.0);
    sound.play();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn sweep_is_the_requested_length_and_within_full_scale() {
        let pcm = sweep(540.0, 830.0, 95);
        let expected = (SAMPLE_RATE as f32 * 0.095) as usize;
        assert_eq!(pcm.len(), expected);
        let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 0, "the cue is silent");
        // Comfortably below full scale: this plays over whatever the user is
        // already listening to.
        assert!(peak < (i16::MAX as f32 * 0.30) as u16, "cue peaks at {peak}, too loud");
    }

    #[test]
    fn cue_starts_and_ends_near_silence_so_there_is_no_click() {
        let pcm = sweep(540.0, 830.0, 95);
        assert!(pcm[0].unsigned_abs() < 200, "hard onset: {}", pcm[0]);
        let tail = pcm[pcm.len() - 1].unsigned_abs();
        assert!(tail < 200, "hard cutoff: {tail}");
    }

    #[test]
    fn wav_header_is_44_bytes_and_declares_the_right_payload() {
        let pcm = vec![0i16; 100];
        let b = wav(&pcm);
        assert_eq!(b.len(), 44 + 200);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(&b[36..40], b"data");
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 200);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), SAMPLE_RATE);
    }

    #[test]
    fn press_and_release_cues_sweep_in_opposite_directions() {
        // The direction is the whole point -- it is what makes "started" and
        // "stopped" distinguishable without looking.
        let up = sweep(540.0, 830.0, 95);
        let down = sweep(760.0, 480.0, 115);
        assert_ne!(up.len(), down.len());
        assert!(!up.is_empty() && !down.is_empty());
    }
}

#[cfg(test)]
mod dump {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    #[test]
    #[ignore = "dev aid: writes the cue WAVs so they can be inspected/played"]
    fn dump_cues() {
        let dir = std::env::var("CUE_DUMP_DIR").unwrap_or_else(|_| "/tmp".into());
        std::fs::write(format!("{dir}/cue-press.wav"), super::wav(&super::sweep(540.0, 830.0, 95))).unwrap();
        std::fs::write(format!("{dir}/cue-release.wav"), super::wav(&super::sweep(760.0, 480.0, 115))).unwrap();
    }
}
