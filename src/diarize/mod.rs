//! Offline speaker diarization.

mod assign;
mod sherpa;

pub use assign::{DiarizationTurn, assign_speakers, split_segments_by_speaker};
pub use sherpa::{DiarizationOptions, DiarizationStage, Diarizer, ProgressSink};
