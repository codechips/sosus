//! Loudness equalization for diarization input.
//!
//! Speaker embeddings degrade sharply for quiet speech: a voice recorded
//! 20 dB below another (a person sitting across the room from the
//! microphone, or a remote participant mixed low) produces embeddings that
//! collapse toward the noise floor, and clustering then lumps both voices
//! into one speaker. ASR does not share this weakness because log-mel
//! features are level-robust, so only the diarizer input is equalized and
//! the recording itself is never modified.

use crate::asr::Audio16kMono;

const WINDOW_SECONDS: f64 = 0.5;
/// -20 dBFS, a typical speech production level.
const TARGET_RMS: f32 = 0.1;
/// -65 dBFS. Windows below this are treated as silence and left untouched
/// so the pauses between words are not amplified into embedding noise.
const GATE_RMS: f32 = 0.000_562;
/// +40 dB ceiling keeps pathological near-silent windows bounded.
const MAX_GAIN: f32 = 100.0;

/// Equalize per-window loudness so quiet and loud voices produce comparably
/// scaled speaker embeddings.
///
/// Each half-second window is scaled toward [`TARGET_RMS`], the gain is
/// ramped linearly across the window to avoid clicks at boundaries, and a
/// tanh soft limiter bounds transients instead of hard clipping them.
pub fn normalize_levels(audio: &Audio16kMono) -> Audio16kMono {
    let samples = audio.samples();
    let window = (WINDOW_SECONDS * f64::from(Audio16kMono::SAMPLE_RATE)) as usize;
    let mut output = Vec::with_capacity(samples.len());
    let mut previous_gain = 1.0_f32;
    for chunk in samples.chunks(window.max(1)) {
        let rms = (chunk.iter().map(|sample| sample * sample).sum::<f32>()
            / chunk.len() as f32)
            .sqrt();
        let gain = if rms < GATE_RMS {
            1.0
        } else {
            (TARGET_RMS / rms).min(MAX_GAIN)
        };
        let length = chunk.len() as f32;
        for (index, sample) in chunk.iter().enumerate() {
            let ramped = previous_gain + (gain - previous_gain) * (index as f32 / length);
            output.push((sample * ramped).tanh());
        }
        previous_gain = gain;
    }
    Audio16kMono::new(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn sine(amplitude: f32, seconds: f64) -> Vec<f32> {
        let count = (seconds * f64::from(Audio16kMono::SAMPLE_RATE)) as usize;
        (0..count)
            .map(|index| {
                amplitude * (index as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin()
            })
            .collect()
    }

    #[test]
    fn quiet_speech_is_amplified_to_the_target_level() {
        // -46 dBFS sine, comparable to the quiet voice in a real meeting.
        let audio = Audio16kMono::new(sine(0.005, 2.0));
        let normalized = normalize_levels(&audio);
        // Skip the first window: it ramps up from unity gain.
        let settled = &normalized.samples()[8_000..];
        let level = 20.0 * rms(settled).log10();
        assert!((-23.0..=-17.0).contains(&level), "level was {level} dBFS");
    }

    #[test]
    fn silence_is_not_amplified() {
        let audio = Audio16kMono::new(vec![0.000_1; 16_000]);
        let normalized = normalize_levels(&audio);
        assert!(rms(normalized.samples()) < 0.000_2);
    }

    #[test]
    fn unequal_voices_converge_to_comparable_levels() {
        // Two voices 26 dB apart, alternating every two seconds.
        let mut samples = sine(0.1, 2.0);
        samples.extend(sine(0.005, 2.0));
        let normalized = normalize_levels(&Audio16kMono::new(samples));
        let loud = rms(&normalized.samples()[8_000..32_000]);
        let quiet = rms(&normalized.samples()[40_000..64_000]);
        let difference = 20.0 * (loud / quiet).log10().abs();
        assert!(difference < 3.0, "voices still {difference:.1} dB apart");
    }

    #[test]
    fn sample_count_is_preserved() {
        let audio = Audio16kMono::new(sine(0.02, 1.3));
        assert_eq!(
            normalize_levels(&audio).samples().len(),
            audio.samples().len()
        );
    }

    #[test]
    fn output_is_bounded_by_the_soft_limiter() {
        let audio = Audio16kMono::new(vec![0.9; 24_000]);
        let normalized = normalize_levels(&audio);
        assert!(normalized.samples().iter().all(|sample| sample.abs() < 1.0));
    }
}
