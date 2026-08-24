//! File-only, size-rotated logging with field-level payload redaction.

use std::error::Error as StdError;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use thiserror::Error;
use tracing::field::Field;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::{Writer, debug_fn};

use crate::paths::{PathError, create_private_dir_all, open_private_append_file};

pub const LOG_FILE_NAME: &str = "sosus.log";
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
pub const RETAINED_LOG_FILES: usize = 5;

/// Install the process-wide file-only tracing subscriber.
pub fn initialize(log_dir: &Path) -> Result<(), LoggingError> {
    let writer = RotatingFile::new(
        log_dir.join(LOG_FILE_NAME),
        MAX_LOG_BYTES,
        RETAINED_LOG_FILES,
    )?;
    let fields = debug_fn(format_redacted_field);

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(writer)
        .fmt_fields(fields)
        .try_init()
        .map_err(LoggingError::Subscriber)
}

/// File writer shared by tracing events. A whole formatted event rotates and writes atomically.
#[derive(Debug, Clone)]
struct RotatingFile {
    inner: Arc<Mutex<FileState>>,
    active_path: PathBuf,
    max_bytes: u64,
    retained_files: usize,
}

#[derive(Debug)]
struct FileState {
    file: File,
    bytes: u64,
}

impl RotatingFile {
    fn new(
        active_path: PathBuf,
        max_bytes: u64,
        retained_files: usize,
    ) -> Result<Self, LoggingError> {
        if max_bytes == 0 || retained_files == 0 {
            return Err(LoggingError::InvalidRotation);
        }
        if let Some(log_dir) = active_path.parent() {
            create_private_dir_all(log_dir)?;
        }
        restrict_existing_logs(&active_path, retained_files)?;

        if active_path
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= max_bytes)
        {
            rotate_files(&active_path, retained_files)?;
        }
        let file = open_private_append_file(&active_path)?;
        let bytes = file.metadata()?.len();

        Ok(Self {
            inner: Arc::new(Mutex::new(FileState { file, bytes })),
            active_path,
            max_bytes,
            retained_files,
        })
    }

    fn append_record(&self, record: &[u8]) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("log writer lock poisoned"))?;
        let record_len = u64::try_from(record.len())
            .map_err(|_| io::Error::other("log record length does not fit in u64"))?;

        if state.bytes > 0 && state.bytes.saturating_add(record_len) > self.max_bytes {
            state.file.flush()?;
            rotate_files(&self.active_path, self.retained_files)?;
            state.file = open_private_append_file(&self.active_path)
                .map_err(|error| io::Error::other(error.to_string()))?;
            state.bytes = 0;
        }

        state.file.write_all(record)?;
        state.file.flush()?;
        state.bytes = state.bytes.saturating_add(record_len);
        Ok(())
    }
}

fn restrict_existing_logs(active_path: &Path, retained_files: usize) -> io::Result<()> {
    for suffix in 0..retained_files {
        let path = if suffix == 0 {
            active_path.to_path_buf()
        } else {
            rotated_path(active_path, suffix)
        };
        match fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl<'writer> MakeWriter<'writer> for RotatingFile {
    type Writer = EventWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        EventWriter {
            destination: self.clone(),
            buffer: Vec::new(),
        }
    }
}

struct EventWriter {
    destination: RotatingFile,
    buffer: Vec<u8>,
}

impl Write for EventWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.destination.append_record(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn rotate_files(active_path: &Path, retained_files: usize) -> io::Result<()> {
    if retained_files == 1 {
        match fs::remove_file(active_path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
    }

    let oldest = rotated_path(active_path, retained_files - 1);
    match fs::remove_file(oldest) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    for suffix in (1..retained_files - 1).rev() {
        let source = rotated_path(active_path, suffix);
        let destination = rotated_path(active_path, suffix + 1);
        match fs::rename(source, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    match fs::rename(active_path, rotated_path(active_path, 1)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rotated_path(active_path: &Path, suffix: usize) -> PathBuf {
    let mut name = active_path
        .file_name()
        .map_or_else(|| "sosus.log".into(), std::ffi::OsString::from);
    name.push(format!(".{suffix}"));
    active_path.with_file_name(name)
}

fn format_redacted_field(
    writer: &mut Writer<'_>,
    field: &Field,
    value: &dyn fmt::Debug,
) -> fmt::Result {
    let name = field.name();
    if is_safe_field(name) {
        write!(writer, " {name}={value:?}")
    } else {
        write!(writer, " {name}=[REDACTED]")
    }
}

fn is_safe_field(name: &str) -> bool {
    matches!(
        name,
        "attempt"
            | "backend"
            | "count"
            | "duration_ms"
            | "elapsed_ms"
            | "error_category"
            | "event"
            | "meeting_id"
            | "model_id"
            | "microphone_channels"
            | "microphone_sample_rate"
            | "size_bytes"
            | "stage"
            | "status"
            | "system_channels"
            | "system_sample_rate"
    )
}

#[derive(Debug, Error)]
pub enum LoggingError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("log rotation requires a non-zero size and retained-file count")]
    InvalidRotation,
    #[error("could not initialize logging")]
    Subscriber(#[source] Box<dyn StdError + Send + Sync + 'static>),
    #[error("log file operation failed")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sosus-logging-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn rotates_at_limit_and_retains_five_private_files() {
        let temp = TempDir::new();
        let active = temp.0.join(LOG_FILE_NAME);
        let writer = RotatingFile::new(active.clone(), 8, 5).unwrap();

        for value in 0..7 {
            writer
                .append_record(format!("item{value}\n").as_bytes())
                .unwrap();
        }

        for suffix in 0..5 {
            let path = if suffix == 0 {
                active.clone()
            } else {
                rotated_path(&active, suffix)
            };
            assert!(path.is_file(), "missing {}", path.display());
            let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(!rotated_path(&active, 5).exists());
        assert_eq!(fs::read_to_string(active).unwrap(), "item6\n");
    }

    #[test]
    fn restricts_preexisting_log_files() {
        let temp = TempDir::new();
        let active = temp.0.join(LOG_FILE_NAME);
        fs::write(&active, b"active").unwrap();
        fs::write(rotated_path(&active, 1), b"archive").unwrap();
        fs::set_permissions(&active, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(rotated_path(&active, 1), fs::Permissions::from_mode(0o666)).unwrap();

        RotatingFile::new(active.clone(), 1024, 5).unwrap();

        let active_mode = fs::metadata(&active).unwrap().permissions().mode() & 0o777;
        let archive_mode = fs::metadata(rotated_path(&active, 1))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(active_mode, 0o600);
        assert_eq!(archive_mode, 0o600);
    }

    #[test]
    fn redacts_messages_and_prohibited_payload_fields() {
        let temp = TempDir::new();
        let active = temp.0.join(LOG_FILE_NAME);
        let writer = RotatingFile::new(active.clone(), 1024, 5).unwrap();
        let fields = debug_fn(format_redacted_field);
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(writer)
            .fmt_fields(fields)
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                event = "stage_finished",
                stage = "transcribe",
                meeting_id = 42,
                transcript = "PROHIBITED_TRANSCRIPT",
                chat_question = "PROHIBITED_QUESTION",
                prompt = "PROHIBITED_PROMPT",
                "PROHIBITED_MESSAGE"
            );
        });

        let contents = fs::read_to_string(active).unwrap();
        assert!(contents.contains("stage_finished"));
        assert!(contents.contains("transcribe"));
        assert!(contents.contains("meeting_id=42"));
        assert!(!contents.contains("PROHIBITED_TRANSCRIPT"));
        assert!(!contents.contains("PROHIBITED_QUESTION"));
        assert!(!contents.contains("PROHIBITED_PROMPT"));
        assert!(!contents.contains("PROHIBITED_MESSAGE"));
        assert_eq!(contents.matches("[REDACTED]").count(), 4);
    }
}
