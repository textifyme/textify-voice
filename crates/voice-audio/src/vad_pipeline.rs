//! Wires voice-core's [`Endpointer`] state machine to a plain energy VAD
//! for toggle mode.
//!
//! This is explicitly the **energy-threshold VAD** (`voice_core::EnergyVad`,
//! an RMS-over-frame comparison against a fixed threshold), not silero or
//! any learned model — silero/`ort` are out of scope for this MVP unit.
//! It is honest, deterministic, and good enough to endpoint clear
//! speech-vs-silence in a quiet room; it will be noisier in loud
//! environments than a real VAD would be. Framing follows SPEC.md's 32 ms /
//! 512-sample chunks (`voice_core::FRAME_SAMPLES_16K`) with a
//! caller-configurable 150-250 ms hangover, both enforced by
//! `voice_core::Endpointer` itself — this module only supplies the framing
//! and the VAD judgment per frame.

use voice_core::endpoint::FRAME_SAMPLES_16K;
use voice_core::{EndpointEvent, EndpointMode, Endpointer, EnergyVad, Vad};

/// Feeds arbitrary-length 16 kHz mono `i16` sample chunks (as delivered by
/// live capture or replayed from a file) into fixed 512-sample frames, runs
/// the energy VAD + `Endpointer` on each complete frame, and reports
/// events. Buffers any trailing partial frame across calls so callers never
/// need to worry about capture callback buffer sizes aligning to 512.
pub struct ToggleCapturePipeline {
    endpointer: Endpointer,
    vad: EnergyVad,
    frame_accum: Vec<i16>,
}

impl ToggleCapturePipeline {
    #[must_use]
    pub fn new(vad_threshold: i64, hangover_ms: u32) -> Self {
        Self {
            endpointer: Endpointer::new(EndpointMode::Toggle { hangover_ms }),
            vad: EnergyVad::new(vad_threshold),
            frame_accum: Vec::with_capacity(FRAME_SAMPLES_16K),
        }
    }

    /// Push newly captured 16 kHz mono `i16` samples. Returns the sequence
    /// of endpoint events produced by however many complete 512-sample
    /// frames this call unlocked, in order. Stops processing further frames
    /// within this call as soon as `Endpoint` fires — a caller should stop
    /// feeding audio and finalize once it sees `EndpointEvent::Endpoint` in
    /// the returned list.
    pub fn push_samples(&mut self, samples: &[i16]) -> Vec<EndpointEvent> {
        self.frame_accum.extend_from_slice(samples);
        let mut events = Vec::new();
        while self.frame_accum.len() >= FRAME_SAMPLES_16K {
            let frame: Vec<i16> = self.frame_accum.drain(..FRAME_SAMPLES_16K).collect();
            let is_speech = self.vad.is_speech(&frame);
            let event = self.endpointer.process_frame(is_speech);
            let ended = event == EndpointEvent::Endpoint;
            events.push(event);
            if ended {
                break;
            }
        }
        events
    }

    #[must_use]
    pub fn has_ended(&self) -> bool {
        self.endpointer.has_ended()
    }

    /// Number of samples buffered but not yet enough to form a full frame.
    #[must_use]
    pub fn pending_sample_count(&self) -> usize {
        self.frame_accum.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud_frame() -> Vec<i16> {
        vec![3000; FRAME_SAMPLES_16K]
    }

    fn quiet_frame() -> Vec<i16> {
        vec![0; FRAME_SAMPLES_16K]
    }

    #[test]
    fn buffers_partial_frames_across_calls_until_512_samples() {
        let mut pipeline = ToggleCapturePipeline::new(500, 200);
        // Feed 200 samples, well short of one 512-sample frame.
        let events = pipeline.push_samples(&vec![3000i16; 200]);
        assert!(events.is_empty(), "no full frame yet, no events expected");
        assert_eq!(pipeline.pending_sample_count(), 200);

        // Feed the remaining 312 to complete the first frame.
        let events = pipeline.push_samples(&vec![3000i16; 312]);
        assert_eq!(events, vec![EndpointEvent::SpeechStarted]);
        assert_eq!(pipeline.pending_sample_count(), 0);
    }

    #[test]
    fn odd_sized_chunks_still_frame_correctly_at_512() {
        // Simulate a capture callback delivering irregular buffer sizes
        // (typical of real audio backends) rather than neat 512 multiples.
        let mut pipeline = ToggleCapturePipeline::new(500, 200);
        let mut total_events = Vec::new();
        let mut all_samples = Vec::new();
        all_samples.extend(loud_frame());
        all_samples.extend(loud_frame());
        // 7 silent frames (224ms) to cross the 200ms hangover.
        for _ in 0..7 {
            all_samples.extend(quiet_frame());
        }

        // Deliver in irregular chunk sizes: 100, 700, 333, remainder.
        let chunk_sizes = [100usize, 700, 333, 900, 1500];
        let mut i = 0;
        let mut c = 0;
        while i < all_samples.len() {
            let size = chunk_sizes[c % chunk_sizes.len()].min(all_samples.len() - i);
            total_events.extend(pipeline.push_samples(&all_samples[i..i + size]));
            i += size;
            c += 1;
        }

        assert!(pipeline.has_ended(), "should have endpointed on hangover");
        assert_eq!(
            total_events.first(),
            Some(&EndpointEvent::SpeechStarted)
        );
        assert_eq!(total_events.last(), Some(&EndpointEvent::Endpoint));
    }

    #[test]
    fn all_silence_never_endpoints() {
        let mut pipeline = ToggleCapturePipeline::new(500, 200);
        let events = pipeline.push_samples(&vec![0i16; FRAME_SAMPLES_16K * 20]);
        assert!(!pipeline.has_ended());
        assert!(events.iter().all(|e| *e == EndpointEvent::None));
    }

    #[test]
    fn stops_emitting_events_after_endpoint_within_same_call() {
        // Even if a single push_samples call contains far more than enough
        // audio to endpoint, no frames past the endpoint should be
        // processed (the caller is expected to stop feeding audio once it
        // sees Endpoint, but a defensive pipeline should not silently keep
        // running the VAD past the ended state either).
        let mut pipeline = ToggleCapturePipeline::new(500, 200);
        let mut samples = Vec::new();
        samples.extend(loud_frame());
        for _ in 0..7 {
            samples.extend(quiet_frame());
        }
        // Extra audio after the endpoint should be ignored within this call.
        samples.extend(loud_frame());
        samples.extend(loud_frame());

        let events = pipeline.push_samples(&samples);
        assert_eq!(events.last(), Some(&EndpointEvent::Endpoint));
        assert!(pipeline.has_ended());
        // The trailing two loud frames (1024 samples) should still be
        // sitting unconsumed in frame_accum since we broke out of the loop.
        assert_eq!(pipeline.pending_sample_count(), FRAME_SAMPLES_16K * 2);
    }
}
