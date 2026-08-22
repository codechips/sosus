//! Non-interactive command dispatch.

use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, bail};
use clap::{CommandFactory, Parser, Subcommand};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::time::{Duration, MissedTickBehavior};

use crate::{asr, audio, config, db, diarize, logging, models, paths};

#[derive(Debug, Parser)]
#[command(
    name = "sosus",
    version,
    about = "Local-first meeting intelligence for macOS"
)]
struct Cli {
    /// Use this configuration file for the invocation.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Override the model, database, and log directory.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Override the meeting artifact directory.
    #[arg(long, global = true, value_name = "PATH")]
    output_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
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
        /// Require at least this many speakers (zero = auto).
        #[arg(long, value_name = "N")]
        min_speakers: Option<usize>,
        /// Allow at most this many speakers (zero = auto).
        #[arg(long, value_name = "N")]
        max_speakers: Option<usize>,
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
            min_speakers,
            max_speakers,
        }) => {
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
                },
            )
            .await
        }
        None if io::stdin().is_terminal() && io::stdout().is_terminal() => run_tui(&cli).await,
        None => print_help(),
    }
}

struct TranscribeInvocation<'a> {
    file: &'a Path,
    backend: Option<asr::TranscriptionBackend>,
    language: Option<String>,
    threads: Option<usize>,
    no_diarize: bool,
    min_speakers: Option<usize>,
    max_speakers: Option<usize>,
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
    eprintln!("Preparing {}...", capabilities.display_name);
    let model_progress = ConsoleModelProgress::new();
    let model_dir =
        models::ensure_asr_model(capabilities.id, app_paths.model_dir(), &model_progress)
            .await
            .context("could not prepare the transcription model")?;
    eprintln!();

    let audio = asr::decode_audio_file(file).context("could not decode the input file")?;
    let thread_count = match effective.effective.transcription.threads {
        0 => num_cpus::get_physical().max(1),
        configured => configured,
    };
    let mut transcriber = asr::create_transcriber(backend);
    transcriber
        .prepare(&asr::PrepareOptions {
            model_dir,
            threads: thread_count,
        })
        .context("could not initialize transcription")?;
    let language = (!effective.effective.transcription.language.is_empty())
        .then(|| effective.effective.transcription.language.clone());
    eprintln!("Transcribing {:.1}s of audio...", audio.duration_seconds());
    let result = transcriber
        .transcribe(
            &audio,
            &asr::TranscribeOptions {
                language,
                vocabulary: Vec::new(),
                words_required: true,
            },
            &ConsoleAsrProgress,
        )
        .context("transcription failed")?;

    let mut result = result;
    let mut speaker_count = 0_usize;
    if effective.effective.diarization.enabled {
        eprintln!("Preparing speaker diarization models...");
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
                    eprintln!(
                        "warning: diarization model preparation failed; transcript is undiarized: {error}"
                    );
                    None
                }
            },
            Err(error) => {
                eprintln!(
                    "warning: diarization model preparation failed; transcript is undiarized: {error}"
                );
                None
            }
        };
        if let Some((segmentation_dir, embedding_dir)) = model_dirs {
            eprintln!();
            eprintln!("Diarizing {:.1}s of audio...", audio.duration_seconds());
            let diarization = diarize::Diarizer::prepare(&diarize::DiarizationOptions {
                segmentation_dir,
                embedding_dir,
                threads: thread_count,
                min_speakers: effective.effective.diarization.min_speakers,
                max_speakers: effective.effective.diarization.max_speakers,
            });
            match diarization {
                Ok(mut diarizer) => match diarizer.process(&audio, &ConsoleDiarizationProgress) {
                    Ok(diarization) => {
                        let assignment =
                            diarize::assign_speakers(&mut result, &diarization.turns, false);
                        speaker_count = assignment.speaker_count;
                        eprintln!(
                            "Diarization complete: {} speaker(s).",
                            assignment.speaker_count
                        );
                    }
                    Err(error) => {
                        eprintln!("warning: diarization failed; transcript is undiarized: {error}")
                    }
                },
                Err(error) => {
                    eprintln!("warning: diarization failed; transcript is undiarized: {error}")
                }
            }
        }
    }

    for segment in &result.segments {
        if let Some(speaker) = &segment.speaker {
            println!("{speaker}: {}", segment.text);
        } else {
            println!("{}", segment.text);
        }
    }

    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let timestamp = now.format(&Rfc3339)?;
    match db::Database::open(app_paths.database_file()) {
        Ok(database) => {
            let save = database
                .writer()
                .execute(db::WriteCommand::InsertMeeting(db::NewMeeting {
                    started_at: timestamp.clone(),
                    ended_at: Some(timestamp.clone()),
                    title: None,
                    duration_s: audio.duration_seconds(),
                    language: result.language.clone(),
                    audio_path: Some(file.to_string_lossy().into_owned()),
                    audio_owned: false,
                    source: "file".to_owned(),
                    speaker_count: i64::try_from(speaker_count).unwrap_or(i64::MAX),
                    created_at: timestamp,
                }));
            match save {
                Ok(db::WriteResult::Inserted(meeting_id)) => {
                    let transcript_save =
                        database
                            .writer()
                            .execute(db::WriteCommand::InsertTranscript {
                                meeting_id,
                                transcript: result.clone(),
                                speaker_count,
                            });
                    if let Err(error) = transcript_save {
                        eprintln!("warning: transcript was not saved to the database: {error}");
                    } else {
                        eprintln!("Saved meeting {meeting_id} to the database.");
                    }
                }
                Ok(other) => eprintln!("warning: unexpected database result: {other:?}"),
                Err(error) => {
                    eprintln!("warning: transcript was not saved to the database: {error}")
                }
            }
            if let Err(error) = database.shutdown() {
                eprintln!("warning: database shutdown failed: {error}");
            }
        }
        Err(error) => eprintln!("warning: transcript was not saved to the database: {error}"),
    }
    Ok(())
}

struct ConsoleModelProgress {
    last_percent: AtomicU64,
}

impl ConsoleModelProgress {
    fn new() -> Self {
        Self {
            last_percent: AtomicU64::new(u64::MAX),
        }
    }
}

impl models::ModelProgressSink for ConsoleModelProgress {
    fn report(&self, progress: models::DownloadProgress<'_>) {
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
    fn report(&self, fraction: f32) {
        if fraction >= 1.0 {
            eprintln!("Transcription complete.");
        }
    }
}

struct ConsoleDiarizationProgress;

impl diarize::ProgressSink for ConsoleDiarizationProgress {
    fn report(&self, stage: diarize::DiarizationStage, complete: bool) {
        let name = match stage {
            diarize::DiarizationStage::Segmentation => "segmentation",
            diarize::DiarizationStage::Embedding => "embedding",
            diarize::DiarizationStage::Clustering => "clustering",
        };
        if complete {
            eprintln!("Diarization {name} complete.");
        } else {
            eprintln!("Diarization {name}...");
        }
    }
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
    paths::ensure_private_file(app_paths.database_file())?;

    for warning in &effective.warnings {
        eprintln!("warning: {warning}");
    }

    audio::ensure_capture_permissions()
        .await
        .context("recording permissions are required")?;

    let started_at = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let meeting_dir = app_paths.create_meeting_dir(started_at)?;
    let recording_path = meeting_dir.join("recording.wav");
    let mut session = audio::RecordingSession::start(&recording_path)?;
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

    let ended_at = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let started_at_text = started_at.format(&Rfc3339)?;
    let ended_at_text = ended_at.format(&Rfc3339)?;
    let database = db::Database::open(app_paths.database_file())?;
    let result = database
        .writer()
        .execute(db::WriteCommand::InsertMeeting(db::NewMeeting {
            started_at: started_at_text.clone(),
            ended_at: Some(ended_at_text),
            title: None,
            duration_s: outcome.duration_seconds,
            language: effective.effective.transcription.language.clone(),
            audio_path: Some(outcome.path.to_string_lossy().into_owned()),
            audio_owned: true,
            source: "recording".to_owned(),
            speaker_count: 0,
            created_at: started_at_text,
        }))?;
    let meeting_id = match result {
        db::WriteResult::Inserted(id) => id,
        other => bail!("unexpected database result while saving recording: {other:?}"),
    };
    database.shutdown().context("shut down database writer")?;

    println!(
        "Saved meeting {meeting_id}: {} ({:.1}s)",
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
    paths::ensure_private_file(app_paths.database_file())?;

    let database = db::Database::open(app_paths.database_file())?;
    let reader = database.reader()?;
    let (foreign_keys, journal_mode) = reader.connection_settings()?;
    tracing::info!(
        event = "startup",
        status = "ready",
        backend = ?effective.effective.transcription.backend,
        schema_version = db::LATEST_SCHEMA_VERSION,
        foreign_keys,
        journal_mode,
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
            database_writer: database.writer(),
            language: effective.effective.transcription.language,
        }),
    };
    let result = crate::tui::run(startup).await;
    let shutdown = database.shutdown().context("shut down database writer");
    result.and(shutdown)
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
    use clap::CommandFactory;

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
