//! Non-interactive command dispatch.

use std::{
    io::{self, IsTerminal},
    path::PathBuf,
};

use anyhow::{Context, bail};
use clap::{CommandFactory, Parser, Subcommand};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::time::{Duration, MissedTickBehavior};

use crate::{audio, config, db, logging, paths};

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
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Tui) => run_explicit_tui(&cli).await,
        Some(Command::Record) => run_record(&cli).await,
        None if io::stdin().is_terminal() && io::stdout().is_terminal() => run_tui(&cli).await,
        None => print_help(),
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
