//! Non-interactive command dispatch.

use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use anyhow::{Context, bail};
use clap::{CommandFactory, Parser, Subcommand};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::time::{Duration, MissedTickBehavior};

use crate::{asr, audio, config, diarize, export, logging, models, paths, pipeline};

#[derive(Debug, Parser)]
#[command(
    name = "sosus",
    version,
    about = "Local-first meeting recording and transcription for macOS"
)]
struct Cli {
    /// Use this configuration file for the invocation.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Override the model and log directory.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Override the meeting artifact directory.
    #[arg(long, global = true, value_name = "PATH")]
    output_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectedSpeakerCount {
    Auto,
    Exact(usize),
}

impl ExpectedSpeakerCount {
    fn bounds(self) -> (usize, usize) {
        match self {
            Self::Auto => (0, 0),
            Self::Exact(count) => (count, count),
        }
    }
}

impl FromStr for ExpectedSpeakerCount {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        let count = value
            .parse::<usize>()
            .map_err(|_| "expected `auto` or a positive number".to_owned())?;
        if count == 0 {
            return Err("expected `auto` or a positive number".to_owned());
        }
        Ok(Self::Exact(count))
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the interactive terminal interface.
    Tui,
    /// Record system audio and the default microphone until Ctrl+C.
    Record,
    /// Transcribe an existing audio or video file with the configured ASR backend.
    Transcribe {
        /// Audio or video file to transcribe.
        file: PathBuf,
        /// Override the configured transcription backend.
        #[arg(long)]
        backend: Option<asr::TranscriptionBackend>,
        /// Override the language; omit for automatic detection.
        #[arg(long)]
        language: Option<String>,
        /// Override the physical-core thread default.
        #[arg(long)]
        threads: Option<usize>,
        /// Skip speaker diarization for this invocation.
        #[arg(long)]
        no_diarize: bool,
        /// Expected speaker count: `auto` or an exact positive number.
        #[arg(long, value_name = "auto|N", conflicts_with_all = ["min_speakers", "max_speakers"])]
        speakers: Option<ExpectedSpeakerCount>,
        /// Require at least this many speakers (zero = auto).
        #[arg(long, value_name = "N")]
        min_speakers: Option<usize>,
        /// Allow at most this many speakers (zero = auto).
        #[arg(long, value_name = "N")]
        max_speakers: Option<usize>,
    },
    /// Import one existing audio or video file into the meeting archive.
    Import {
        /// Audio or video file to import.
        file: PathBuf,
    },
    /// Recover interrupted work and resume processing a saved meeting.
    Resume {
        /// Meeting folder or recording file to resume.
        meeting: PathBuf,
    },
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Tui) => run_explicit_tui(&cli).await,
        Some(Command::Record) => run_record(&cli).await,
        Some(Command::Transcribe {
            ref file,
            backend,
            ref language,
            threads,
            no_diarize,
            speakers,
            min_speakers,
            max_speakers,
        }) => {
            let (min_speakers, max_speakers) = match speakers {
                Some(expected) => {
                    let (min, max) = expected.bounds();
                    (Some(min), Some(max))
                }
                None => (min_speakers, max_speakers),
            };
            run_transcribe(
                &cli,
                TranscribeInvocation {
                    file,
                    backend,
                    language: language.clone(),
                    threads,
                    no_diarize,
                    min_speakers,
                    max_speakers,
                    existing_meeting: None,
                    resume_state: None,
                    resume_transcript: None,
                },
            )
            .await
        }
        Some(Command::Resume { ref meeting }) => run_resume(&cli, meeting).await,
        Some(Command::Import { ref file }) => {
            run_transcribe(
                &cli,
                TranscribeInvocation {
                    file,
                    backend: None,
                    language: None,
                    threads: None,
                    no_diarize: false,
                    min_speakers: None,
                    max_speakers: None,
                    existing_meeting: None,
                    resume_state: None,
                    resume_transcript: None,
                },
            )
            .await
        }
        None if io::stdin().is_terminal() && io::stdout().is_terminal() => run_tui(&cli).await,
        None => print_help(),
    }
}

async fn run_resume(cli: &Cli, meeting: &Path) -> anyhow::Result<()> {
    let defaults = paths::AppPaths::resolve(None, None, cli.output_dir.as_deref())?;
    let environment = config::EnvironmentOverrides::from_process();
    let invocation = config::ConfigOverrides {
        config_path: cli.config.clone(),
        data_dir: cli.data_dir.clone(),
        output_dir: cli.output_dir.clone(),
        ..config::ConfigOverrides::default()
    };
    let effective = config::load_effective(
        defaults.config_file(),
        defaults.data_dir(),
        &environment,
        &invocation,
    )?;
    let app_paths = paths::AppPaths::resolve(
        Some(&effective.locations.config_path),
        Some(&effective.locations.data_dir),
        Some(&effective.effective.output.dir),
    )?;
    app_paths.ensure_base_directories()?;
    let audio_path = if meeting.is_dir() {
        meeting.join("recording.wav")
    } else {
        meeting.to_path_buf()
    };
    if !audio_path.is_file() {
        bail!("recording file was not found: {}", audio_path.display());
    }
    let meeting_dir = audio_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let state_path = meeting_dir.join(pipeline::PipelineState::STATE_FILE);
    let resume_state = if state_path.is_file() {
        let mut state = pipeline::PipelineState::load(&meeting_dir).with_context(|| {
            format!(
                "could not load pipeline state from {}",
                state_path.display()
            )
        })?;
        let recovered = state.recover_interrupted();
        if recovered > 0 {
            state.save(&meeting_dir)?;
        }
        Some(state)
    } else {
        None
    };
    let resume_transcript = match &resume_state {
        Some(state)
            if state
                .stage(pipeline::Stage::Transcribe)
                .is_some_and(|stage| stage.status == pipeline::StageStatus::Completed) =>
        {
            let path = meeting_dir.join(INTERMEDIATE_TRANSCRIPT);
            Some(export::read_transcript_json(&path).with_context(|| {
                format!(
                    "could not load the saved intermediate transcript from {}",
                    path.display()
                )
            })?)
        }
        _ => None,
    };
    eprintln!("Resuming from {}.", audio_path.display());
    run_transcribe(
        cli,
        TranscribeInvocation {
            file: &audio_path,
            backend: None,
            language: None,
            threads: None,
            no_diarize: false,
            min_speakers: None,
            max_speakers: None,
            existing_meeting: Some(meeting_dir),
            resume_state,
            resume_transcript,
        },
    )
    .await
}

struct TranscribeInvocation<'a> {
    file: &'a Path,
    backend: Option<asr::TranscriptionBackend>,
    language: Option<String>,
    threads: Option<usize>,
    no_diarize: bool,
    min_speakers: Option<usize>,
    max_speakers: Option<usize>,
    existing_meeting: Option<PathBuf>,
    resume_state: Option<pipeline::PipelineState>,
    resume_transcript: Option<asr::TranscriptResult>,
}

fn select_artifact_dir(
    existing_meeting: Option<PathBuf>,
    app_paths: &paths::AppPaths,
    started: OffsetDateTime,
) -> Result<PathBuf, paths::PathError> {
    match existing_meeting {
        Some(meeting_dir) => Ok(meeting_dir),
        None => app_paths.create_meeting_dir(started),
    }
}

async fn run_transcribe(
    cli: &Cli,
    invocation_args: TranscribeInvocation<'_>,
) -> anyhow::Result<()> {
    let TranscribeInvocation {
        file,
        backend,
        language,
        threads,
        no_diarize,
        min_speakers,
        max_speakers,
        existing_meeting,
        resume_state,
        resume_transcript,
    } = invocation_args;
    let invocation = config::ConfigOverrides {
        config_path: cli.config.clone(),
        data_dir: cli.data_dir.clone(),
        output_dir: cli.output_dir.clone(),
        backend,
        language,
        threads,
        diarization_enabled: no_diarize.then_some(false),
        min_speakers,
        max_speakers,
        ..config::ConfigOverrides::default()
    };
    let defaults = paths::AppPaths::resolve(None, None, cli.output_dir.as_deref())?;
    let environment = config::EnvironmentOverrides::from_process();
    let effective = config::load_effective(
        defaults.config_file(),
        defaults.data_dir(),
        &environment,
        &invocation,
    )?;
    let app_paths = paths::AppPaths::resolve(
        Some(&effective.locations.config_path),
        Some(&effective.locations.data_dir),
        Some(&effective.effective.output.dir),
    )?;
    app_paths.ensure_base_directories()?;
    logging::initialize(app_paths.log_dir())?;
    for warning in &effective.warnings {
        eprintln!("warning: {warning}");
    }

    let backend = effective.effective.transcription.backend;
    let capabilities = backend.capabilities();
    let thread_count = match effective.effective.transcription.threads {
        0 => num_cpus::get_physical().max(1),
        configured => configured,
    };
    let started = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let started_text = started.format(&Rfc3339)?;
    let artifact_dir = select_artifact_dir(existing_meeting, &app_paths, started)?;
    let input_fingerprint = file_fingerprint(file)?;
    let mut skipped = vec![pipeline::Stage::Summarize, pipeline::Stage::Index];
    if !effective.effective.diarization.enabled {
        skipped.push(pipeline::Stage::Diarize);
    }
    let mut pipeline_state = resume_state.unwrap_or_else(|| pipeline::PipelineState::new(&skipped));
    let intermediate_path = artifact_dir.join(INTERMEDIATE_TRANSCRIPT);
    let mut result = resume_transcript;
    let mut audio = None;

    if result.is_none() {
        pipeline_state.begin(
            pipeline::Stage::Transcribe,
            &format!("{input_fingerprint}:{}", capabilities.id),
            capabilities.id,
            &started_text,
        )?;
        persist_pipeline(&pipeline_state, &artifact_dir)?;
        let model_progress = ConsoleModelProgress::new();
        eprintln!("Preparing transcription...");
        let model_dir = models::ensure_asr_model(
            capabilities.id,
            &effective.effective.transcription.model,
            app_paths.model_dir(),
            &model_progress,
        )
        .await
        .context("could not prepare the transcription model")?;
        model_progress.finish();
        eprintln!("Reading recording...");
        let decoded = asr::decode_audio_file(file).context("could not decode the input file")?;
        let mut transcriber = asr::create_transcriber(backend);
        eprintln!("Loading transcriber...");
        if let Err(error) = transcriber.prepare(&asr::PrepareOptions {
            model_dir,
            threads: thread_count,
        }) {
            pipeline_state.fail(pipeline::Stage::Transcribe, "initialization")?;
            persist_pipeline(&pipeline_state, &artifact_dir)?;
            return Err(anyhow::anyhow!(error)).context("could not initialize transcription");
        }
        let language = (!effective.effective.transcription.language.is_empty())
            .then(|| effective.effective.transcription.language.clone());
        eprintln!(
            "Transcribing {:.1}s of audio...",
            decoded.duration_seconds()
        );
        match transcriber.transcribe(
            &decoded,
            &asr::TranscribeOptions {
                language,
                vocabulary: Vec::new(),
                words_required: false,
            },
            &ConsoleAsrProgress,
        ) {
            Ok(transcript) => result = Some(transcript),
            Err(error) => {
                pipeline_state.fail(pipeline::Stage::Transcribe, "runtime")?;
                persist_pipeline(&pipeline_state, &artifact_dir)?;
                return Err(anyhow::anyhow!(error)).context("transcription failed");
            }
        }
        export::write_transcript_json(
            &intermediate_path,
            result.as_ref().expect("transcript set"),
        )?;
        pipeline_state.complete(pipeline::Stage::Transcribe, &started_text)?;
        persist_pipeline(&pipeline_state, &artifact_dir)?;
        audio = Some(decoded);
    } else {
        eprintln!("Using saved transcription; skipping Whisper.");
    }

    let mut result = result.expect("transcript is set after transcription");
    let diarization_status = pipeline_state
        .stage(pipeline::Stage::Diarize)
        .map(|stage| stage.status);
    if effective.effective.diarization.enabled
        && !matches!(
            diarization_status,
            Some(pipeline::StageStatus::Completed | pipeline::StageStatus::Skipped)
        )
    {
        pipeline_state.begin(
            pipeline::Stage::Diarize,
            &format!("{input_fingerprint}:diarization"),
            "sherpa-onnx-diarization-1",
            &started_text,
        )?;
        persist_pipeline(&pipeline_state, &artifact_dir)?;
        if audio.is_none() {
            eprintln!("Reading recording for diarization...");
            audio = Some(asr::decode_audio_file(file).context("could not decode the input file")?);
        }
        let audio = audio.as_ref().expect("audio decoded for diarization");
        let model_progress = ConsoleModelProgress::new();
        eprintln!("Preparing diarization...");
        let model_dirs = match models::ensure_diarization_model(
            "diarization-segmentation",
            app_paths.model_dir(),
            &model_progress,
        )
        .await
        {
            Ok(segmentation_dir) => match models::ensure_diarization_model(
                "diarization-embedding",
                app_paths.model_dir(),
                &model_progress,
            )
            .await
            {
                Ok(embedding_dir) => Some((segmentation_dir, embedding_dir)),
                Err(error) => {
                    pipeline_state.skip(pipeline::Stage::Diarize)?;
                    persist_pipeline(&pipeline_state, &artifact_dir)?;
                    eprintln!(
                        "warning: diarization model preparation failed; transcript is undiarized: {error}"
                    );
                    None
                }
            },
            Err(error) => {
                pipeline_state.skip(pipeline::Stage::Diarize)?;
                persist_pipeline(&pipeline_state, &artifact_dir)?;
                eprintln!(
                    "warning: diarization model preparation failed; transcript is undiarized: {error}"
                );
                None
            }
        };
        model_progress.finish();
        if let Some((segmentation_dir, embedding_dir)) = model_dirs {
            eprintln!("Diarizing {:.1}s of audio...", audio.duration_seconds());
            let diarization = diarize::Diarizer::prepare(&diarize::DiarizationOptions {
                segmentation_dir,
                embedding_dir,
                threads: thread_count,
                min_speakers: effective.effective.diarization.min_speakers,
                max_speakers: effective.effective.diarization.max_speakers,
            });
            match diarization {
                Ok(mut diarizer) => match diarizer.process(audio, &ConsoleDiarizationProgress) {
                    Ok(diarization) => {
                        let assign_words =
                            matches!(capabilities.word_timestamps, asr::WordTimestamps::Native);
                        let assignment =
                            diarize::assign_speakers(&mut result, &diarization.turns, assign_words);
                        if assign_words {
                            diarize::split_segments_by_speaker(&mut result);
                        }
                        export::write_transcript_json(&intermediate_path, &result)?;
                        pipeline_state.complete(pipeline::Stage::Diarize, &started_text)?;
                        persist_pipeline(&pipeline_state, &artifact_dir)?;
                        eprintln!(
                            "Diarization complete: {} speaker(s).",
                            assignment.speaker_count
                        );
                    }
                    Err(error) => {
                        pipeline_state.skip(pipeline::Stage::Diarize)?;
                        persist_pipeline(&pipeline_state, &artifact_dir)?;
                        eprintln!("warning: diarization failed; transcript is undiarized: {error}")
                    }
                },
                Err(error) => {
                    pipeline_state.skip(pipeline::Stage::Diarize)?;
                    persist_pipeline(&pipeline_state, &artifact_dir)?;
                    eprintln!("warning: diarization failed; transcript is undiarized: {error}")
                }
            }
        }
    }

    if pipeline_state
        .stage(pipeline::Stage::Export)
        .is_some_and(|stage| stage.status == pipeline::StageStatus::Completed)
    {
        eprintln!("Transcript artifacts are already complete.");
        return Ok(());
    }
    pipeline_state.begin(
        pipeline::Stage::Export,
        &format!("{input_fingerprint}:markdown-v1"),
        "markdown-export-v1",
        &started_text,
    )?;
    persist_pipeline(&pipeline_state, &artifact_dir)?;
    eprintln!("Saving transcript...");
    for segment in &result.segments {
        if let Some(speaker) = &segment.speaker {
            println!("{speaker}: {}", segment.text);
        } else {
            println!("{}", segment.text);
        }
    }

    {
        let transcript_path = artifact_dir.join("transcript.md");
        match export::write_transcript(&transcript_path, &result) {
            Ok(()) => eprintln!("Saved transcript: {}", transcript_path.display()),
            Err(error) => eprintln!(
                "warning: transcript artifact was not saved to {}: {error}",
                transcript_path.display()
            ),
        }
        if effective.effective.output.json {
            let json_path = artifact_dir.join("transcript.json");
            match export::write_transcript_json(&json_path, &result) {
                Ok(()) => eprintln!("Saved transcript JSON: {}", json_path.display()),
                Err(error) => eprintln!(
                    "warning: transcript JSON was not saved to {}: {error}",
                    json_path.display()
                ),
            }
        }
    }
    pipeline_state.complete(pipeline::Stage::Export, &started_text)?;
    persist_pipeline(&pipeline_state, &artifact_dir)?;

    Ok(())
}

const INTERMEDIATE_TRANSCRIPT: &str = ".transcript-work.json";

fn persist_pipeline(state: &pipeline::PipelineState, meeting_dir: &Path) -> anyhow::Result<()> {
    state
        .save(meeting_dir)
        .context("could not save pipeline state")
}

fn file_fingerprint(path: &Path) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("could not inspect input file {}", path.display()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    Ok(format!(
        "{}:{}:{}",
        path.display(),
        metadata.len(),
        modified
    ))
}

struct ConsoleModelProgress {
    last_percent: AtomicU64,
    downloaded: AtomicBool,
}

impl ConsoleModelProgress {
    fn new() -> Self {
        Self {
            last_percent: AtomicU64::new(u64::MAX),
            downloaded: AtomicBool::new(false),
        }
    }

    fn finish(&self) {
        if self.downloaded.swap(false, Ordering::Relaxed) {
            eprintln!();
        }
    }
}

impl models::ModelProgressSink for ConsoleModelProgress {
    fn report(&self, progress: models::DownloadProgress<'_>) {
        self.downloaded.store(true, Ordering::Relaxed);
        let percent = progress
            .model_bytes
            .saturating_mul(100)
            .checked_div(progress.model_total.max(1))
            .unwrap_or(0)
            .min(100);
        if self.last_percent.swap(percent, Ordering::Relaxed) != percent {
            eprint!(
                "\rDownloading {}: {:>3}% ({})",
                progress.model, percent, progress.file
            );
        }
    }
}

struct ConsoleAsrProgress;

impl asr::ProgressSink for ConsoleAsrProgress {
    fn report(&self, _fraction: f32) {}
}

struct ConsoleDiarizationProgress;

impl diarize::ProgressSink for ConsoleDiarizationProgress {
    fn report(&self, _stage: diarize::DiarizationStage, _complete: bool) {}
}

async fn run_record(cli: &Cli) -> anyhow::Result<()> {
    let invocation = config::ConfigOverrides {
        config_path: cli.config.clone(),
        data_dir: cli.data_dir.clone(),
        output_dir: cli.output_dir.clone(),
        ..config::ConfigOverrides::default()
    };
    let defaults = paths::AppPaths::resolve(None, None, cli.output_dir.as_deref())?;
    let environment = config::EnvironmentOverrides::from_process();
    let effective = config::load_effective(
        defaults.config_file(),
        defaults.data_dir(),
        &environment,
        &invocation,
    )?;
    let app_paths = paths::AppPaths::resolve(
        Some(&effective.locations.config_path),
        Some(&effective.locations.data_dir),
        Some(&effective.effective.output.dir),
    )?;
    app_paths.ensure_base_directories()?;
    logging::initialize(app_paths.log_dir())?;

    for warning in &effective.warnings {
        eprintln!("warning: {warning}");
    }

    audio::ensure_capture_permissions()
        .await
        .context("recording permissions are required")?;

    let started_at = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let (meeting_dir, mut session) = audio::RecordingSession::start_new_meeting_with_mix_settings(
        &app_paths,
        started_at,
        audio::MixSettings::from_db(
            effective.effective.audio.system_gain_db,
            effective.effective.audio.mic_gain_db,
        ),
    )?;
    let formats = session.source_formats();
    tracing::info!(
        event = "recording_started",
        system_sample_rate = formats.system_sample_rate,
        system_channels = formats.system_channels,
        microphone_sample_rate = formats.microphone_sample_rate,
        microphone_channels = formats.microphone_channels,
    );

    println!("Recording system audio + microphone. Press Ctrl+C to stop.");
    println!("Meeting folder: {}", meeting_dir.display());

    let mut interval = tokio::time::interval(Duration::from_millis(10));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let recording_error = loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("could not listen for Ctrl+C")?;
                break None;
            }
            _ = interval.tick() => {
                if let Err(error) = session.pump() {
                    break Some(error);
                }
            }
        }
    };

    let outcome = session.finish().context("could not finalize recording")?;
    if let Some(error) = recording_error {
        return Err(error).context("recording stopped because system audio failed");
    }

    if let Err(error) = run_transcribe(
        cli,
        TranscribeInvocation {
            file: &outcome.path,
            backend: None,
            language: None,
            threads: None,
            no_diarize: false,
            min_speakers: None,
            max_speakers: None,
            existing_meeting: Some(
                outcome
                    .path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
            ),
            resume_state: None,
            resume_transcript: None,
        },
    )
    .await
    {
        eprintln!("warning: post-recording pipeline failed: {error:#}");
    }

    println!(
        "Saved recording: {} ({:.1}s)",
        outcome.path.display(),
        outcome.duration_seconds
    );
    if outcome.system_dropouts > 0 || outcome.microphone_dropouts > 0 {
        eprintln!(
            "warning: audio queue dropouts: system={}, microphone={}",
            outcome.system_dropouts, outcome.microphone_dropouts
        );
    }
    if outcome.microphone_failed {
        eprintln!(
            "warning: the microphone stream failed during recording; missing audio is silent"
        );
    }
    if (outcome.elapsed_seconds - outcome.duration_seconds).abs() > 1.0 {
        eprintln!(
            "warning: captured audio duration differs from wall time ({:.1}s vs {:.1}s)",
            outcome.duration_seconds, outcome.elapsed_seconds
        );
    }
    Ok(())
}

async fn run_explicit_tui(cli: &Cli) -> anyhow::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("sosus tui requires an interactive terminal on stdin and stdout");
    }

    run_tui(cli).await
}

async fn run_tui(cli: &Cli) -> anyhow::Result<()> {
    let invocation = config::ConfigOverrides {
        config_path: cli.config.clone(),
        data_dir: cli.data_dir.clone(),
        output_dir: cli.output_dir.clone(),
        ..config::ConfigOverrides::default()
    };
    let defaults = paths::AppPaths::resolve(None, None, cli.output_dir.as_deref())?;
    let environment = config::EnvironmentOverrides::from_process();
    let effective = config::load_effective(
        defaults.config_file(),
        defaults.data_dir(),
        &environment,
        &invocation,
    )?;
    let app_paths = paths::AppPaths::resolve(
        Some(&effective.locations.config_path),
        Some(&effective.locations.data_dir),
        Some(&effective.effective.output.dir),
    )?;
    app_paths.ensure_base_directories()?;
    logging::initialize(app_paths.log_dir())?;
    tracing::info!(
        event = "startup",
        status = "ready",
        backend = ?effective.effective.transcription.backend,
    );

    let startup = crate::tui::Startup {
        archive_dir: app_paths.output_dir().display().to_string(),
        warnings: effective
            .warnings
            .into_iter()
            .map(|warning| warning.to_string())
            .collect(),
        recording: Some(crate::tui::RecordingStartup {
            app_paths: app_paths.clone(),
            mix_settings: audio::MixSettings::from_db(
                effective.effective.audio.system_gain_db,
                effective.effective.audio.mic_gain_db,
            ),
            config_path: effective.locations.config_path.clone(),
        }),
        settings: Some(crate::tui::SettingsStartup {
            config: effective.saved,
            config_path: effective.locations.config_path.clone(),
            fingerprint: config::fingerprint(&effective.locations.config_path)?,
            model_dir: app_paths.model_dir().to_path_buf(),
        }),
    };
    crate::tui::run(startup).await
}

fn print_help() -> anyhow::Result<()> {
    let mut command = Cli::command();
    command.print_help()?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use std::{env, fs};
    use time::OffsetDateTime;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn expected_speakers_accepts_auto_or_an_exact_count() {
        assert_eq!("auto".parse(), Ok(ExpectedSpeakerCount::Auto));
        assert_eq!("2".parse(), Ok(ExpectedSpeakerCount::Exact(2)));
        assert!("0".parse::<ExpectedSpeakerCount>().is_err());
        assert!("two".parse::<ExpectedSpeakerCount>().is_err());
        assert!(
            Cli::try_parse_from([
                "sosus",
                "transcribe",
                "meeting.wav",
                "--speakers",
                "2",
                "--min-speakers",
                "2",
            ])
            .is_err()
        );
    }

    #[test]
    fn resuming_uses_the_existing_meeting_without_creating_another_directory() {
        let root = env::temp_dir().join(format!(
            "sosus-cli-test-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let output_dir = root.join("recordings");
        let existing_meeting = root.join("existing-meeting");
        fs::create_dir_all(&existing_meeting).unwrap();
        let app_paths = paths::AppPaths::resolve(
            Some(&root.join("config.toml")),
            Some(&root.join("data")),
            Some(&output_dir),
        )
        .unwrap();

        let selected = select_artifact_dir(
            Some(existing_meeting.clone()),
            &app_paths,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        assert_eq!(selected, existing_meeting);
        assert!(!output_dir.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
