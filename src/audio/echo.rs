//! Conservative acoustic echo suppression using the captured system signal as a reference.
//!
//! The microphone can hear physical speakers even though the same signal is already available
//! losslessly from the system-audio tap.  This canceller estimates a single delayed reference
//! path only when correlation is strong; when there is no confident match, it passes the
//! microphone through unchanged.  That makes headphones and unrelated speech no-ops.

const SAMPLE_RATE: usize = 48_000;
const ANALYSIS_DECIMATION: usize = 48;
const ANALYSIS_HISTORY: usize = 2_000;
const ANALYSIS_WINDOW: usize = 600;
const MAX_DELAY_MILLIS: usize = 250;
const ESTIMATE_INTERVAL: usize = 250;
const MIN_CORRELATION: f32 = 0.65;
const MAX_GAIN: f32 = 1.5;

/// Removes confidently identified delayed system-audio bleed from microphone samples.
pub struct EchoCanceller {
    reference_history: Vec<f32>,
    reference_index: usize,
    system_analysis: Vec<f32>,
    microphone_analysis: Vec<f32>,
    analysis_index: usize,
    analysis_count: usize,
    analysis_accumulator: (f32, f32),
    analysis_frames: usize,
    frames_since_estimate: usize,
    delay_samples: usize,
    gain: f32,
    active: bool,
}

impl Default for EchoCanceller {
    fn default() -> Self {
        let max_delay_samples = SAMPLE_RATE * MAX_DELAY_MILLIS / 1_000;
        Self {
            reference_history: vec![0.0; max_delay_samples + 1],
            reference_index: 0,
            system_analysis: vec![0.0; ANALYSIS_HISTORY],
            microphone_analysis: vec![0.0; ANALYSIS_HISTORY],
            analysis_index: 0,
            analysis_count: 0,
            analysis_accumulator: (0.0, 0.0),
            analysis_frames: 0,
            frames_since_estimate: 0,
            delay_samples: 0,
            gain: 0.0,
            active: false,
        }
    }
}

impl EchoCanceller {
    /// Process one aligned system/microphone sample pair.
    pub fn process(&mut self, system: f32, microphone: f32) -> f32 {
        self.reference_history[self.reference_index] = system;
        let delayed_index = (self.reference_index + self.reference_history.len()
            - self.delay_samples)
            % self.reference_history.len();
        let cancelled = if self.active {
            microphone - self.gain * self.reference_history[delayed_index]
        } else {
            microphone
        };
        self.reference_index = (self.reference_index + 1) % self.reference_history.len();

        self.analysis_accumulator.0 += system;
        self.analysis_accumulator.1 += microphone;
        self.analysis_frames += 1;
        if self.analysis_frames == ANALYSIS_DECIMATION {
            self.push_analysis_frame(
                self.analysis_accumulator.0 / ANALYSIS_DECIMATION as f32,
                self.analysis_accumulator.1 / ANALYSIS_DECIMATION as f32,
            );
            self.analysis_accumulator = (0.0, 0.0);
            self.analysis_frames = 0;
            self.frames_since_estimate += 1;
            if self.frames_since_estimate >= ESTIMATE_INTERVAL {
                self.estimate_path();
                self.frames_since_estimate = 0;
            }
        }

        cancelled
    }

    fn push_analysis_frame(&mut self, system: f32, microphone: f32) {
        self.system_analysis[self.analysis_index] = system;
        self.microphone_analysis[self.analysis_index] = microphone;
        self.analysis_index = (self.analysis_index + 1) % ANALYSIS_HISTORY;
        self.analysis_count = (self.analysis_count + 1).min(ANALYSIS_HISTORY);
    }

    fn estimate_path(&mut self) {
        let max_delay = MAX_DELAY_MILLIS.min(ANALYSIS_HISTORY - ANALYSIS_WINDOW - 1);
        if self.analysis_count < ANALYSIS_WINDOW + max_delay {
            return;
        }

        let mut best = None;
        for delay in 0..=max_delay {
            let (dot, system_energy, microphone_energy) = self.correlation(delay);
            if system_energy <= f32::EPSILON || microphone_energy <= f32::EPSILON {
                continue;
            }
            let correlation = dot / (system_energy * microphone_energy).sqrt();
            if best.is_none_or(|(best_correlation, _)| correlation > best_correlation) {
                best = Some((correlation, (delay, dot / system_energy)));
            }
        }

        let Some((correlation, (delay, gain))) = best else {
            self.active = false;
            return;
        };
        if correlation >= MIN_CORRELATION && gain.abs() <= MAX_GAIN {
            self.delay_samples = delay * ANALYSIS_DECIMATION;
            self.gain = gain;
            self.active = true;
        } else {
            self.active = false;
        }
    }

    fn correlation(&self, delay: usize) -> (f32, f32, f32) {
        let mut dot = 0.0;
        let mut system_energy = 0.0;
        let mut microphone_energy = 0.0;
        for offset in 0..ANALYSIS_WINDOW {
            let microphone_index =
                (self.analysis_index + ANALYSIS_HISTORY - 1 - offset) % ANALYSIS_HISTORY;
            let system_index = (microphone_index + ANALYSIS_HISTORY - delay) % ANALYSIS_HISTORY;
            let system = self.system_analysis[system_index];
            let microphone = self.microphone_analysis[microphone_index];
            dot += system * microphone;
            system_energy += system * system;
            microphone_energy += microphone * microphone;
        }
        (dot, system_energy, microphone_energy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_a_confident_delayed_system_echo() {
        let mut canceller = EchoCanceller::default();
        let delay = 96 * ANALYSIS_DECIMATION;
        let mut reference = vec![0.0; delay];
        let mut reference_index = 0;
        let mut state = 1_u32;
        let mut raw_energy = 0.0;
        let mut cancelled_energy = 0.0;

        for frame in 0..SAMPLE_RATE * 3 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let system = ((state >> 8) as i16 as f32) / i16::MAX as f32;
            let microphone = 0.45 * reference[reference_index];
            reference[reference_index] = system;
            reference_index = (reference_index + 1) % reference.len();
            let cancelled = canceller.process(system, microphone);
            if frame > SAMPLE_RATE * 2 {
                raw_energy += microphone * microphone;
                cancelled_energy += cancelled * cancelled;
            }
        }

        assert!(
            cancelled_energy < raw_energy * 0.3,
            "raw={raw_energy}, cancelled={cancelled_energy}, active={}, delay={}, gain={}",
            canceller.active,
            canceller.delay_samples,
            canceller.gain
        );
    }

    #[test]
    fn leaves_uncorrelated_microphone_audio_unchanged() {
        let mut canceller = EchoCanceller::default();
        let mut state = 7_u32;
        let mut difference = 0.0;

        for frame in 0..SAMPLE_RATE * 3 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let system = ((state >> 8) as i16 as f32) / i16::MAX as f32;
            let microphone = (frame as f32 * 0.013).sin() * 0.5;
            let cancelled = canceller.process(system, microphone);
            if frame > SAMPLE_RATE * 2 {
                difference += (cancelled - microphone).abs();
            }
        }

        assert_eq!(difference, 0.0);
    }
}
