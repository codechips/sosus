//! Centralized filesystem path resolution and secure file creation.

use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use etcetera::{AppStrategy, AppStrategyArgs};
use thiserror::Error;
use time::OffsetDateTime;

const APP_NAME: &str = "sosus";
const CONFIG_FILE_NAME: &str = "config.toml";
const DATABASE_FILE_NAME: &str = "sosus.db";
const DEFAULT_OUTPUT_SUFFIX: &str = "sosus/recordings";

/// Every durable path used by sosus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    home_dir: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
    model_dir: PathBuf,
    database_file: PathBuf,
    output_dir: PathBuf,
}

impl AppPaths {
    /// Resolve the documented defaults, including the two supported environment overrides.
    #[allow(dead_code)]
    pub fn discover(output_override: Option<&Path>) -> Result<Self, PathError> {
        let config_override = env::var_os("SOSUS_CONFIG").map(PathBuf::from);
        let data_override = env::var_os("SOSUS_DATA_DIR").map(PathBuf::from);

        Self::resolve(
            config_override.as_deref(),
            data_override.as_deref(),
            output_override,
        )
    }

    /// Resolve defaults with explicit invocation overrides.
    ///
    /// Callers should apply CLI-over-environment precedence before passing values here.
    pub fn resolve(
        config_override: Option<&Path>,
        data_override: Option<&Path>,
        output_override: Option<&Path>,
    ) -> Result<Self, PathError> {
        let strategy = etcetera::app_strategy::Xdg::new(AppStrategyArgs {
            top_level_domain: "dev".to_owned(),
            author: APP_NAME.to_owned(),
            app_name: APP_NAME.to_owned(),
        })?;

        let config_file = config_override.map_or_else(
            || strategy.config_dir().join(CONFIG_FILE_NAME),
            Path::to_path_buf,
        );

        Ok(Self::from_roots(
            strategy.home_dir().to_path_buf(),
            config_file,
            data_override.map_or_else(|| strategy.data_dir(), Path::to_path_buf),
            output_override,
        ))
    }

    fn from_roots(
        home_dir: PathBuf,
        config_file: PathBuf,
        data_dir: PathBuf,
        output_override: Option<&Path>,
    ) -> Self {
        let config_dir = config_file
            .parent()
            .map_or_else(PathBuf::new, Path::to_path_buf);
        let output_dir = output_override.map_or_else(
            || home_dir.join(DEFAULT_OUTPUT_SUFFIX),
            |path| expand_home(path, &home_dir),
        );

        Self {
            home_dir,
            config_dir,
            config_file,
            log_dir: data_dir.join("logs"),
            model_dir: data_dir.join("models"),
            database_file: data_dir.join(DATABASE_FILE_NAME),
            data_dir,
            output_dir,
        }
    }

    /// Create the application-owned base directories with private permissions.
    ///
    /// Existing directories are left untouched, including a user-selected output directory.
    pub fn ensure_base_directories(&self) -> Result<(), PathError> {
        for path in [
            &self.config_dir,
            &self.data_dir,
            &self.log_dir,
            &self.model_dir,
            &self.output_dir,
        ] {
            create_private_dir_all(path)?;
        }
        Ok(())
    }

    /// Atomically reserve a unique meeting folder for the supplied local start time.
    #[allow(dead_code)]
    pub fn create_meeting_dir(&self, started_at: OffsetDateTime) -> Result<PathBuf, PathError> {
        create_meeting_dir(&self.output_dir, started_at)
    }

    #[allow(dead_code)]
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    #[allow(dead_code)]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    #[allow(dead_code)]
    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    pub fn database_file(&self) -> &Path {
        &self.database_file
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }
}

/// Expand the documented `~` output prefix without interpreting shell syntax.
pub fn expand_home(path: &Path, home_dir: &Path) -> PathBuf {
    if path == Path::new("~") {
        return home_dir.to_path_buf();
    }

    path.strip_prefix("~/")
        .map_or_else(|_| path.to_path_buf(), |suffix| home_dir.join(suffix))
}

/// Recursively create missing directories as `0700` while preserving existing modes.
pub fn create_private_dir_all(path: &Path) -> Result<(), PathError> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }

    if let Some(parent) = path.parent()
        && parent != path
    {
        create_private_dir_all(parent)?;
    }

    let mut builder = DirBuilder::new();
    match builder.mode(0o700).create(path) {
        Ok(()) => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Create a new sensitive file as `0600`, failing rather than replacing an existing file.
#[allow(dead_code)]
pub fn create_private_file(path: &Path) -> Result<File, PathError> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Create a sensitive file if absent or restrict an existing file to `0600`.
///
/// The file's contents are never truncated or otherwise modified.
pub fn ensure_private_file(path: &Path) -> Result<(), PathError> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Open an application-owned sensitive file for append, enforcing `0600`.
pub(crate) fn open_private_append_file(path: &Path) -> Result<File, PathError> {
    ensure_private_file(path)?;
    let file = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    Ok(file)
}

#[allow(dead_code)]
fn create_meeting_dir(root: &Path, started_at: OffsetDateTime) -> Result<PathBuf, PathError> {
    create_private_dir_all(root)?;
    let base_name = format!(
        "{:04}-{:02}-{:02}_{:02}{:02}",
        started_at.year(),
        u8::from(started_at.month()),
        started_at.day(),
        started_at.hour(),
        started_at.minute()
    );

    for suffix in 1_u64.. {
        let name = if suffix == 1 {
            base_name.clone()
        } else {
            format!("{base_name}_{suffix}")
        };
        let candidate = root.join(name);
        let mut builder = DirBuilder::new();
        match builder.mode(0o700).create(&candidate) {
            Ok(()) => {
                fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }

    Err(PathError::MeetingNamesExhausted)
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("could not resolve the user home directory")]
    Home(#[from] etcetera::HomeDirError),
    #[error("filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("could not allocate a unique meeting directory name")]
    #[allow(dead_code)]
    MeetingNamesExhausted,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use time::{Date, Month, Time, UtcOffset};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "sosus-paths-test-{}-{sequence}",
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

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn resolves_documented_default_layout() {
        let temp = TempDir::new();
        let paths = AppPaths::from_roots(
            temp.0.clone(),
            temp.0.join(".config/sosus/config.toml"),
            temp.0.join(".local/share/sosus"),
            None,
        );

        assert_eq!(
            paths.config_file(),
            temp.0.join(".config/sosus/config.toml")
        );
        assert_eq!(paths.model_dir(), temp.0.join(".local/share/sosus/models"));
        assert_eq!(paths.log_dir(), temp.0.join(".local/share/sosus/logs"));
        assert_eq!(
            paths.database_file(),
            temp.0.join(".local/share/sosus/sosus.db")
        );
        assert_eq!(paths.output_dir(), temp.0.join("sosus/recordings"));
    }

    #[test]
    fn expands_only_a_leading_home_component() {
        let home = Path::new("/Users/tester");

        assert_eq!(expand_home(Path::new("~"), home), home);
        assert_eq!(
            expand_home(Path::new("~/sosus/recordings"), home),
            Path::new("/Users/tester/sosus/recordings")
        );
        assert_eq!(
            expand_home(Path::new("archive/~/recordings"), home),
            Path::new("archive/~/recordings")
        );
        assert_eq!(
            expand_home(Path::new("~another/recordings"), home),
            Path::new("~another/recordings")
        );
    }

    #[test]
    fn creates_private_directories_and_preserves_existing_mode() {
        let temp = TempDir::new();
        let existing = temp.0.join("existing-output");
        fs::create_dir(&existing).unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o750)).unwrap();

        create_private_dir_all(&existing).unwrap();
        let nested = existing.join("sosus-owned/meetings");
        create_private_dir_all(&nested).unwrap();

        assert_eq!(mode(&existing), 0o750);
        assert_eq!(mode(&existing.join("sosus-owned")), 0o700);
        assert_eq!(mode(&nested), 0o700);
    }

    #[test]
    fn creates_and_restricts_sensitive_files_without_changing_contents() {
        let temp = TempDir::new();
        let new_file = temp.0.join("private/new.txt");
        ensure_private_file(&new_file).unwrap();
        assert_eq!(mode(&new_file), 0o600);

        let existing_file = temp.0.join("existing.txt");
        fs::write(&existing_file, b"existing").unwrap();
        fs::set_permissions(&existing_file, fs::Permissions::from_mode(0o644)).unwrap();
        ensure_private_file(&existing_file).unwrap();
        assert_eq!(mode(&existing_file), 0o600);
        assert_eq!(fs::read(&existing_file).unwrap(), b"existing");
    }

    #[test]
    fn private_file_helpers_reject_symbolic_links() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new();
        let target = temp.0.join("target.txt");
        let link = temp.0.join("link.txt");
        fs::write(&target, b"private").unwrap();
        symlink(&target, &link).unwrap();

        assert!(ensure_private_file(&link).is_err());
        assert!(open_private_append_file(&link).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"private");
    }

    #[test]
    fn meeting_directories_are_collision_safe_and_private() {
        let temp = TempDir::new();
        let date = Date::from_calendar_date(2026, Month::August, 21).unwrap();
        let time = Time::from_hms(14, 30, 59).unwrap();
        let started_at = date
            .with_time(time)
            .assume_offset(UtcOffset::from_hms(2, 0, 0).unwrap());

        let first = create_meeting_dir(&temp.0, started_at).unwrap();
        let second = create_meeting_dir(&temp.0, started_at).unwrap();
        let third = create_meeting_dir(&temp.0, started_at).unwrap();

        assert_eq!(first.file_name().unwrap(), "2026-08-21_1430");
        assert_eq!(second.file_name().unwrap(), "2026-08-21_1430_2");
        assert_eq!(third.file_name().unwrap(), "2026-08-21_1430_3");
        assert_eq!(mode(&first), 0o700);
        assert_eq!(mode(&second), 0o700);
        assert_eq!(mode(&third), 0o700);
    }

    #[test]
    fn concurrent_meeting_directory_reservations_are_unique() {
        let temp = TempDir::new();
        let root = Arc::new(temp.0.clone());
        let date = Date::from_calendar_date(2026, Month::August, 21).unwrap();
        let started_at = date
            .with_time(Time::from_hms(14, 30, 0).unwrap())
            .assume_offset(UtcOffset::UTC);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = Arc::clone(&root);
                std::thread::spawn(move || create_meeting_dir(&root, started_at).unwrap())
            })
            .collect();
        let paths: HashSet<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(paths.len(), 8);
        assert!(paths.iter().all(|path| mode(path) == 0o700));
    }
}
