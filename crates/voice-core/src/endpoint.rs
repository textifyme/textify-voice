//! VAD trait + pure endpointing state machine (SPEC.md §3.1, §3.2; V1.1
//! acceptance: "PTT endpoint fires on key-up"; toggle mode uses "32
//! ms/512-sample chunks, 150-250 ms hangover").
//!
//! `silero` (the real VAD model) is a native/IO backend and out of scope for
//! this crate per the run's boundaries — [`Vad`] is the trait it will
//! implement, and [`EnergyVad`] is a deterministic stand-in used to drive the
//! state machine in tests.

/// 16 kHz mono, 32 ms per SPEC — 16_000 * 0.032 = 512 samples/frame.
pub const FRAME_SAMPLES_16K: usize = 512;

/// A voice-activity detector: given one frame of PCM, says whether it judges
/// the frame to contain speech. Implementations may be stateful (silero is);
/// callers must feed frames in order.
pub trait Vad {
    fn is_speech(&mut self, frame: &[i16]) -> bool;
}

/// Deterministic energy-threshold VAD used in tests and as the trait's
/// in-memory stand-in for silero. Not tuned for real audio — it exists so
/// the endpointing state machine below can be driven by synthetic frames
/// without pulling in `ort`.
#[derive(Debug, Clone, Copy)]
pub struct EnergyVad {
    pub threshold: i64,
}

impl EnergyVad {
    #[must_use]
    pub fn new(threshold: i64) -> Self {
        Self { threshold }
    }
}

impl Vad for EnergyVad {
    fn is_speech(&mut self, frame: &[i16]) -> bool {
        if frame.is_empty() {
            return false;
        }
        let sum_abs: i64 = frame.iter().map(|&s| i64::from(s).abs()).sum();
        let mean_abs = sum_abs / frame.len() as i64;
        mean_abs >= self.threshold
    }
}

/// SPEC.md §3.1: "PTT: key-up = endpoint (no VAD wait); toggle: silero VAD
/// endpointing (32 ms/512-sample chunks, 150-250 ms hangover)."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMode {
    /// Push-to-talk: the endpoint is the key-up event itself, full stop —
    /// the state machine never waits on VAD frames to decide.
    Ptt,
    /// Toggle-to-talk: the endpoint fires after `hangover_ms` of continuous
    /// non-speech following the last speech frame.
    Toggle { hangover_ms: u32 },
}

/// What happened as a result of feeding one frame (toggle mode) or one key
/// event (PTT mode) to the [`Endpointer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointEvent {
    /// Nothing state-changing happened.
    None,
    /// Speech was judged to have started (first speech frame after idle).
    SpeechStarted,
    /// The utterance has ended; the caller should stop feeding frames,
    /// finalize the ASR engine, and encode/replay the ring buffer.
    Endpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Speaking,
    /// Trailing silence after speech, accumulating toward the hangover
    /// threshold. `elapsed_ms` resets to 0 the instant speech resumes.
    TrailingSilence {
        elapsed_ms: u32,
    },
    Ended,
}

/// Pure endpointing state machine — no audio I/O, no VAD model inference; it
/// only consumes `bool` speech/silence judgments (from a [`Vad`] impl, or
/// synthetic values in tests) and `Ptt` key events, and emits
/// [`EndpointEvent`]s.
#[derive(Debug, Clone)]
pub struct Endpointer {
    mode: EndpointMode,
    state: State,
    /// Frame duration in ms, used to convert frame counts to hangover time.
    /// Fixed at the SPEC's 32 ms per V1.1.
    frame_ms: u32,
}

impl Endpointer {
    #[must_use]
    pub fn new(mode: EndpointMode) -> Self {
        Self {
            mode,
            state: State::Idle,
            frame_ms: 32,
        }
    }

    #[must_use]
    pub fn mode(&self) -> EndpointMode {
        self.mode
    }

    #[must_use]
    pub fn has_ended(&self) -> bool {
        matches!(self.state, State::Ended)
    }

    /// Toggle mode only: feed one 32 ms/512-sample frame's speech judgment.
    /// Calling this in `Ptt` mode is a caller error but is handled
    /// defensively as a genuine no-op: SPEC.md §3.1/V1.1 says "PTT: key-up
    /// = endpoint (no VAD wait)," so VAD frames must never be able to
    /// endpoint a PTT utterance, not even accidentally. A caller that runs
    /// one shared frame loop for both modes (natural, since the VAD is
    /// already running to decide *when* to start capturing) must be able to
    /// call this every frame in PTT mode too without truncating dictation
    /// at the speaker's first inter-word pause — only `on_key_up` may end a
    /// PTT utterance. (An earlier version used `hangover_ms = 0` here,
    /// which made the first non-speech frame endpoint immediately — worse
    /// than a no-op, since it silently truncated PTT dictation.)
    pub fn process_frame(&mut self, is_speech: bool) -> EndpointEvent {
        let hangover_ms = match self.mode {
            EndpointMode::Toggle { hangover_ms } => hangover_ms,
            EndpointMode::Ptt => {
                // SpeechStarted is still useful telemetry/UI signal (e.g.
                // driving a "listening" indicator) and doesn't end
                // anything, so it's allowed through; silence must never do
                // anything in PTT mode — only `on_key_up` may endpoint.
                if is_speech && self.state == State::Idle {
                    self.state = State::Speaking;
                    return EndpointEvent::SpeechStarted;
                }
                return EndpointEvent::None;
            }
        };

        match self.state {
            State::Ended => EndpointEvent::None,
            State::Idle => {
                if is_speech {
                    self.state = State::Speaking;
                    EndpointEvent::SpeechStarted
                } else {
                    EndpointEvent::None
                }
            }
            State::Speaking => {
                if is_speech {
                    EndpointEvent::None
                } else {
                    self.accumulate_silence(0, hangover_ms)
                }
            }
            State::TrailingSilence { elapsed_ms } => {
                if is_speech {
                    // Speech resumed before hangover elapsed: cancel the
                    // countdown and go back to Speaking.
                    self.state = State::Speaking;
                    EndpointEvent::None
                } else {
                    self.accumulate_silence(elapsed_ms, hangover_ms)
                }
            }
        }
    }

    /// Shared silence-accumulation step used from both `Speaking` (prior
    /// elapsed = 0) and `TrailingSilence` (prior elapsed carried over).
    fn accumulate_silence(&mut self, prior_elapsed_ms: u32, hangover_ms: u32) -> EndpointEvent {
        let new_elapsed = prior_elapsed_ms + self.frame_ms;
        if new_elapsed >= hangover_ms {
            self.state = State::Ended;
            EndpointEvent::Endpoint
        } else {
            self.state = State::TrailingSilence {
                elapsed_ms: new_elapsed,
            };
            EndpointEvent::None
        }
    }

    /// PTT mode: key-up is the endpoint, full stop — no VAD wait regardless
    /// of what the most recent frame looked like. Safe to call from any
    /// state (including `Idle`, e.g. a key tap too short to produce a
    /// frame) and idempotent once ended.
    pub fn on_key_up(&mut self) -> EndpointEvent {
        if matches!(self.state, State::Ended) {
            return EndpointEvent::None;
        }
        self.state = State::Ended;
        EndpointEvent::Endpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptt_endpoints_immediately_on_key_up_mid_speech() {
        let mut ep = Endpointer::new(EndpointMode::Ptt);
        assert_eq!(ep.process_frame(true), EndpointEvent::SpeechStarted);
        assert_eq!(ep.process_frame(true), EndpointEvent::None);
        // Key-up fires the endpoint even though the VAD still thinks the
        // user is speaking — "no VAD wait" is the whole point of PTT.
        assert_eq!(ep.on_key_up(), EndpointEvent::Endpoint);
        assert!(ep.has_ended());
    }

    #[test]
    fn ptt_key_up_before_any_frame_still_ends_cleanly() {
        let mut ep = Endpointer::new(EndpointMode::Ptt);
        assert_eq!(ep.on_key_up(), EndpointEvent::Endpoint);
        assert!(ep.has_ended());
    }

    #[test]
    fn ptt_process_frame_is_a_true_no_op_and_does_not_endpoint_on_first_pause() {
        // MINOR regression: with the old `hangover_ms = 0` PTT handling,
        // accumulate_silence fired EndpointEvent::Endpoint on the very
        // first non-speech frame while the key was still held — truncating
        // dictation at the speaker's first inter-word pause. process_frame
        // must be a genuine no-op for silence in PTT mode; only
        // `on_key_up` may end the utterance.
        let mut ep = Endpointer::new(EndpointMode::Ptt);
        assert_eq!(ep.process_frame(true), EndpointEvent::SpeechStarted);
        // A pause mid-utterance (e.g. "get the... [pause] ...current
        // weather") must not endpoint while the key is still held.
        assert_eq!(ep.process_frame(false), EndpointEvent::None);
        assert!(!ep.has_ended(), "PTT must not endpoint on VAD silence");
        // Many silent frames in a row still must not endpoint.
        for _ in 0..20 {
            assert_eq!(ep.process_frame(false), EndpointEvent::None);
        }
        assert!(!ep.has_ended());
        // Speech resuming after the pause is also a no-op (not a state
        // transition worth reporting) — only key-up ends the utterance.
        assert_eq!(ep.process_frame(true), EndpointEvent::None);
        assert!(!ep.has_ended());
        // Only the key-up ends it, and it captures the full utterance
        // including everything spoken across the pause.
        assert_eq!(ep.on_key_up(), EndpointEvent::Endpoint);
        assert!(ep.has_ended());
    }

    #[test]
    fn ptt_process_frame_silence_before_any_speech_is_a_no_op() {
        let mut ep = Endpointer::new(EndpointMode::Ptt);
        assert_eq!(ep.process_frame(false), EndpointEvent::None);
        assert!(!ep.has_ended());
    }

    #[test]
    fn ptt_key_up_is_idempotent() {
        let mut ep = Endpointer::new(EndpointMode::Ptt);
        assert_eq!(ep.on_key_up(), EndpointEvent::Endpoint);
        assert_eq!(ep.on_key_up(), EndpointEvent::None);
    }

    #[test]
    fn toggle_mode_waits_for_hangover_before_endpointing() {
        // 200 ms hangover / 32 ms frames = needs >= ~7 silent frames.
        let mut ep = Endpointer::new(EndpointMode::Toggle { hangover_ms: 200 });
        assert_eq!(ep.process_frame(true), EndpointEvent::SpeechStarted);
        // 6 silent frames = 192ms, not enough yet.
        for _ in 0..6 {
            assert_eq!(ep.process_frame(false), EndpointEvent::None);
        }
        assert!(!ep.has_ended());
        // 7th silent frame crosses 200ms (224ms total elapsed).
        assert_eq!(ep.process_frame(false), EndpointEvent::Endpoint);
        assert!(ep.has_ended());
    }

    #[test]
    fn toggle_mode_resets_hangover_when_speech_resumes() {
        let mut ep = Endpointer::new(EndpointMode::Toggle { hangover_ms: 150 });
        ep.process_frame(true); // speaking
        ep.process_frame(false); // 32ms silence
        ep.process_frame(false); // 64ms silence
        assert_eq!(
            ep.process_frame(true),
            EndpointEvent::None,
            "speech resumed: countdown must cancel, not endpoint"
        );
        // Now silence must accumulate from zero again, not from 64ms.
        for _ in 0..3 {
            assert_eq!(ep.process_frame(false), EndpointEvent::None);
        }
        assert!(!ep.has_ended(), "only 96ms of fresh silence, below 150ms");
        assert_eq!(ep.process_frame(false), EndpointEvent::None); // 128ms
        assert_eq!(ep.process_frame(false), EndpointEvent::Endpoint); // 160ms
    }

    #[test]
    fn toggle_mode_hangover_within_spec_range_150_to_250ms() {
        const FRAME_MS: u32 = 32;
        for hangover_ms in [150u32, 200, 250] {
            let mut ep = Endpointer::new(EndpointMode::Toggle { hangover_ms });
            ep.process_frame(true);
            let frames_needed = hangover_ms.div_ceil(FRAME_MS);
            for _ in 0..frames_needed.saturating_sub(1) {
                assert_eq!(ep.process_frame(false), EndpointEvent::None);
            }
            assert_eq!(ep.process_frame(false), EndpointEvent::Endpoint);
        }
    }

    #[test]
    fn frame_size_matches_spec_32ms_512_samples_at_16khz() {
        // SPEC.md V1.1: "32 ms/512-sample chunks."
        assert_eq!(FRAME_SAMPLES_16K, 512);
        assert_eq!((FRAME_SAMPLES_16K as f64 / 16_000.0) * 1000.0, 32.0);
    }

    #[test]
    fn idle_silence_never_endpoints() {
        let mut ep = Endpointer::new(EndpointMode::Toggle { hangover_ms: 200 });
        for _ in 0..20 {
            assert_eq!(ep.process_frame(false), EndpointEvent::None);
        }
        assert!(!ep.has_ended());
    }

    #[test]
    fn energy_vad_is_deterministic_over_synthetic_frames() {
        let mut vad = EnergyVad::new(500);
        let quiet = vec![0i16; FRAME_SAMPLES_16K];
        let loud = vec![3000i16; FRAME_SAMPLES_16K];
        assert!(!vad.is_speech(&quiet));
        assert!(vad.is_speech(&loud));
    }
}
