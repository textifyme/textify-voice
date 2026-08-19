//! Manual verification tool: decode the real fixture audio and print
//! sample counts, duration, and peak/RMS amplitude, proving the decoded
//! PCM is real, non-silent, and correctly scaled — not a stub. Run with:
//!
//!   cargo run -p voice-audio --bin decode_report

use std::path::Path;

fn main() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio");
    for name in ["short-5s.wav", "ref-3min.wav"] {
        let path = fixtures_dir.join(name);
        match voice_audio::decode_wav_file(&path) {
            Ok(samples) => {
                let stats = voice_audio::compute_stats(&samples);
                println!(
                    "{name}: sample_count={} duration_s={:.6} peak_amplitude={} rms_amplitude={:.2}",
                    stats.sample_count, stats.duration_s, stats.peak_amplitude, stats.rms_amplitude
                );
            }
            Err(e) => {
                eprintln!("{name}: DECODE FAILED: {e}");
                std::process::exit(1);
            }
        }
    }
}
