//! sherpa-onnx diarization implementation.

use std::path::{Path, PathBuf};

use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig,
};
use thiserror::Error;

use crate::asr::Audio16kMono;

use super::DiarizationTurn;

const SEGMENTATION_FILE: &str = "model.int8.onnx";
const EMBEDDING_FILE: &str = "3dspeaker.onnx";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiarizationStage {
    Segmentation,
    Embedding,
    Clustering,
}

pub trait ProgressSink: Sync {
    fn report(&self, stage: DiarizationStage, complete: bool);
}

impl<F> ProgressSink for F
where
    F: Fn(DiarizationStage, bool) + Sync,
{
    fn report(&self, stage: DiarizationStage, complete: bool) {
        self(stage, complete);
    }
}

#[derive(Clone, Debug)]
pub struct DiarizationOptions {
    pub segmentation_dir: PathBuf,
    pub embedding_dir: PathBuf,
    pub threads: usize,
    pub min_speakers: usize,
    pub max_speakers: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiarizationResult {
    pub turns: Vec<DiarizationTurn>,
    pub speaker_count: usize,
}

pub struct Diarizer {
    engine: OfflineSpeakerDiarization,
    config: OfflineSpeakerDiarizationConfig,
    min_speakers: usize,
    max_speakers: usize,
}

impl Diarizer {
    pub fn prepare(options: &DiarizationOptions) -> Result<Self, DiarizationError> {
        validate_bounds(options.min_speakers, options.max_speakers)?;
        let segmentation_model = model_path(&options.segmentation_dir, SEGMENTATION_FILE)?;
        let embedding_model = model_path(&options.embedding_dir, EMBEDDING_FILE)?;
        let threads = i32::try_from(options.threads.max(1)).unwrap_or(i32::MAX);
        let config = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(segmentation_model.to_string_lossy().into_owned()),
                },
                num_threads: threads,
                ..Default::default()
            },
            embedding: SpeakerEmbeddingExtractorConfig {
                model: Some(embedding_model.to_string_lossy().into_owned()),
                num_threads: threads,
                ..Default::default()
            },
            clustering: FastClusteringConfig {
                num_clusters: forced_cluster_count(options.min_speakers, options.max_speakers),
                ..Default::default()
            },
            ..Default::default()
        };
        let engine = OfflineSpeakerDiarization::create(&config).ok_or(DiarizationError::Create)?;
        if engine.sample_rate() != Audio16kMono::SAMPLE_RATE as i32 {
            return Err(DiarizationError::SampleRate(engine.sample_rate()));
        }
        Ok(Self {
            engine,
            config,
            min_speakers: options.min_speakers,
            max_speakers: options.max_speakers,
        })
    }

    pub fn process(
        &mut self,
        audio: &Audio16kMono,
        progress: &dyn ProgressSink,
    ) -> Result<DiarizationResult, DiarizationError> {
        // sherpa's Rust API exposes one blocking call for the combined pipeline;
        // keep the three named stages visible without inventing false fractions.
        progress.report(DiarizationStage::Segmentation, false);
        progress.report(DiarizationStage::Embedding, false);
        progress.report(DiarizationStage::Clustering, false);
        let mut result = self.process_once(audio)?;
        progress.report(DiarizationStage::Segmentation, true);
        progress.report(DiarizationStage::Embedding, true);
        progress.report(DiarizationStage::Clustering, true);

        let count = result.speaker_count;
        let bounded = bounded_cluster_count(count, self.min_speakers, self.max_speakers);
        if bounded != count {
            self.config.clustering.num_clusters = bounded as i32;
            self.engine.set_config(&self.config);
            result = self.process_once(audio)?;
        }
        Ok(result)
    }

    fn process_once(&self, audio: &Audio16kMono) -> Result<DiarizationResult, DiarizationError> {
        let result = self
            .engine
            .process(audio.samples())
            .ok_or(DiarizationError::Process)?;
        let speaker_count = usize::try_from(result.num_speakers()).unwrap_or(0);
        let turns = result
            .sort_by_start_time()
            .into_iter()
            .map(|segment| DiarizationTurn {
                start_seconds: f64::from(segment.start),
                end_seconds: f64::from(segment.end),
                cluster_id: segment.speaker,
            })
            .collect();
        Ok(DiarizationResult {
            turns,
            speaker_count,
        })
    }
}

fn model_path(directory: &Path, filename: &str) -> Result<PathBuf, DiarizationError> {
    let path = directory.join(filename);
    if path.is_file() {
        Ok(path)
    } else {
        Err(DiarizationError::MissingModel(path))
    }
}

fn validate_bounds(min: usize, max: usize) -> Result<(), DiarizationError> {
    if min != 0 && max != 0 && min > max {
        Err(DiarizationError::InvalidBounds { min, max })
    } else {
        Ok(())
    }
}

fn forced_cluster_count(min: usize, max: usize) -> i32 {
    if min != 0 && min == max {
        i32::try_from(min).unwrap_or(i32::MAX)
    } else {
        -1
    }
}

fn bounded_cluster_count(count: usize, min: usize, max: usize) -> usize {
    if min != 0 && count < min {
        min
    } else if max != 0 && count > max {
        max
    } else {
        count
    }
}

#[derive(Debug, Error)]
pub enum DiarizationError {
    #[error("speaker diarization model file is missing: {0}")]
    MissingModel(PathBuf),
    #[error("could not initialize sherpa-onnx speaker diarization")]
    Create,
    #[error("sherpa-onnx speaker diarization expects 16 kHz but reported {0} Hz")]
    SampleRate(i32),
    #[error("sherpa-onnx speaker diarization failed to process the audio")]
    Process,
    #[error("invalid speaker bounds: min_speakers={min}, max_speakers={max}")]
    InvalidBounds { min: usize, max: usize },
}
