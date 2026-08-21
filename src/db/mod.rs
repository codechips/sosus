//! SQLite connection setup and the process-wide single-writer boundary.

mod models;
mod queries;
mod schema;

use std::{
    path::{Path, PathBuf},
    sync::{OnceLock, mpsc},
    thread::{self, JoinHandle},
};

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

#[allow(unused_imports)]
pub use models::{Chat, ChatTurn, ChatTurnSource, Citation, PipelineStage, Segment, Summary, Word};
pub use models::{Meeting, NewMeeting, NewPassage, Passage};
pub use schema::LATEST_SCHEMA_VERSION;

static SQLITE_VEC_REGISTRATION: OnceLock<i32> = OnceLock::new();

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("could not open SQLite database at {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not format migration timestamp")]
    MigrationTimestamp(#[from] time::error::Format),
    #[error("database schema version {found} is newer than supported version {supported}")]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("sqlite-vec auto-extension registration failed with SQLite code {0}")]
    VecRegistration(i32),
    #[error("could not spawn database writer thread")]
    WriterSpawn(#[source] std::io::Error),
    #[error("database writer is no longer available")]
    WriterUnavailable,
    #[error("database writer thread panicked")]
    WriterPanicked,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum WriteCommand {
    InsertMeeting(NewMeeting),
    DeleteMeeting(i64),
    InsertPassage(NewPassage),
    UpdatePassageText {
        id: i64,
        text: String,
    },
    DeletePassage(i64),
    #[cfg(test)]
    SeedTestGraph {
        meeting_id: i64,
        passage_id: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteResult {
    Inserted(i64),
    Deleted(bool),
    Updated(bool),
    #[cfg(test)]
    Done,
}

enum WriterMessage {
    #[allow(dead_code)]
    Execute {
        command: Box<WriteCommand>,
        reply: mpsc::Sender<Result<WriteResult, DatabaseError>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct DatabaseWriter {
    sender: mpsc::Sender<WriterMessage>,
}

impl DatabaseWriter {
    #[allow(dead_code)]
    pub fn execute(&self, command: WriteCommand) -> Result<WriteResult, DatabaseError> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(WriterMessage::Execute {
                command: Box::new(command),
                reply,
            })
            .map_err(|_| DatabaseError::WriterUnavailable)?;
        response
            .recv()
            .map_err(|_| DatabaseError::WriterUnavailable)?
    }
}

pub struct DatabaseReader {
    connection: Connection,
}

impl DatabaseReader {
    #[allow(dead_code)]
    pub fn meeting(&self, id: i64) -> Result<Option<Meeting>, DatabaseError> {
        Ok(queries::meeting(&self.connection, id)?)
    }

    #[allow(dead_code)]
    pub fn passage(&self, id: i64) -> Result<Option<Passage>, DatabaseError> {
        Ok(queries::passage(&self.connection, id)?)
    }

    pub fn connection_settings(&self) -> Result<(bool, String), DatabaseError> {
        Ok(queries::connection_settings(&self.connection)?)
    }
}

pub struct Database {
    path: PathBuf,
    writer: DatabaseWriter,
    writer_thread: Option<JoinHandle<()>>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        register_sqlite_vec()?;
        let path = path.as_ref().to_path_buf();
        let mut connection = open_connection(&path)?;
        queries::configure_connection(&connection, false)?;
        schema::migrate(&mut connection)?;

        let (sender, receiver) = mpsc::channel();
        let writer_thread = thread::Builder::new()
            .name("sosus-db-writer".to_owned())
            .spawn(move || writer_loop(connection, receiver))
            .map_err(DatabaseError::WriterSpawn)?;

        Ok(Self {
            path,
            writer: DatabaseWriter { sender },
            writer_thread: Some(writer_thread),
        })
    }

    #[allow(dead_code)]
    pub fn writer(&self) -> DatabaseWriter {
        self.writer.clone()
    }

    pub fn reader(&self) -> Result<DatabaseReader, DatabaseError> {
        let connection = open_connection(&self.path)?;
        queries::configure_connection(&connection, true)?;
        Ok(DatabaseReader { connection })
    }

    pub fn shutdown(mut self) -> Result<(), DatabaseError> {
        self.stop_writer()
    }

    fn stop_writer(&mut self) -> Result<(), DatabaseError> {
        let Some(writer_thread) = self.writer_thread.take() else {
            return Ok(());
        };
        self.writer
            .sender
            .send(WriterMessage::Shutdown)
            .map_err(|_| DatabaseError::WriterUnavailable)?;
        writer_thread
            .join()
            .map_err(|_| DatabaseError::WriterPanicked)
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = self.stop_writer();
    }
}

fn open_connection(path: &Path) -> Result<Connection, DatabaseError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| DatabaseError::Open {
        path: path.to_path_buf(),
        source,
    })
}

fn register_sqlite_vec() -> Result<(), DatabaseError> {
    let result = *SQLITE_VEC_REGISTRATION.get_or_init(|| {
        // sqlite-vec exposes its SQLite entrypoint without the full C signature.
        // SQLite's auto-extension API requires that entrypoint shape, as shown by
        // sqlite-vec's own rusqlite integration test.
        let entrypoint = unsafe {
            std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(sqlite_vec::sqlite3_vec_init as *const ())
        };
        // SAFETY: `entrypoint` is sqlite-vec's static extension initializer and
        // remains valid for the process lifetime.
        unsafe { rusqlite::ffi::sqlite3_auto_extension(Some(entrypoint)) }
    });

    if result == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(DatabaseError::VecRegistration(result))
    }
}

fn writer_loop(connection: Connection, receiver: mpsc::Receiver<WriterMessage>) {
    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Execute { command, reply } => {
                let _ = reply.send(execute_write(&connection, *command));
            }
            WriterMessage::Shutdown => break,
        }
    }
}

fn execute_write(
    connection: &Connection,
    command: WriteCommand,
) -> Result<WriteResult, DatabaseError> {
    match command {
        WriteCommand::InsertMeeting(meeting) => Ok(WriteResult::Inserted(queries::insert_meeting(
            connection, &meeting,
        )?)),
        WriteCommand::DeleteMeeting(id) => Ok(WriteResult::Deleted(queries::delete_meeting(
            connection, id,
        )?)),
        WriteCommand::InsertPassage(passage) => Ok(WriteResult::Inserted(queries::insert_passage(
            connection, &passage,
        )?)),
        WriteCommand::UpdatePassageText { id, text } => Ok(WriteResult::Updated(
            queries::update_passage_text(connection, id, &text)?,
        )),
        WriteCommand::DeletePassage(id) => Ok(WriteResult::Deleted(queries::delete_passage(
            connection, id,
        )?)),
        #[cfg(test)]
        WriteCommand::SeedTestGraph {
            meeting_id,
            passage_id,
        } => {
            queries::seed_cascade_graph(connection, meeting_id, passage_id)?;
            queries::insert_zero_vector(connection, passage_id, meeting_id)?;
            Ok(WriteResult::Done)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
        thread,
    };

    use super::*;

    static TEMP_DB_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempDatabasePath(PathBuf);

    impl TempDatabasePath {
        fn new() -> Self {
            let sequence = TEMP_DB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sosus-db-test-{}-{sequence}.sqlite3",
                std::process::id()
            ));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDatabasePath {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let _ = fs::remove_file(self.0.with_extension("sqlite3-wal"));
            let _ = fs::remove_file(self.0.with_extension("sqlite3-shm"));
        }
    }

    fn sample_meeting() -> NewMeeting {
        NewMeeting {
            started_at: "2026-08-21T10:00:00+02:00".to_owned(),
            ended_at: None,
            title: None,
            duration_s: 0.0,
            language: "en".to_owned(),
            audio_path: Some("/tmp/meeting.wav".to_owned()),
            audio_owned: true,
            source: "recording".to_owned(),
            speaker_count: 0,
            created_at: "2026-08-21T10:00:00+02:00".to_owned(),
        }
    }

    fn passage_for(meeting_id: i64, text: &str) -> NewPassage {
        NewPassage {
            meeting_id,
            start_s: 0.0,
            end_s: 1.0,
            speakers: "Speaker 1".to_owned(),
            text: text.to_owned(),
            token_count: 2,
        }
    }

    fn inserted_id(result: WriteResult) -> i64 {
        match result {
            WriteResult::Inserted(id) => id,
            other => panic!("expected inserted id, got {other:?}"),
        }
    }

    #[test]
    fn creates_exact_schema_and_migration_is_idempotent() {
        let path = TempDatabasePath::new();
        let database = Database::open(path.path()).unwrap();
        let reader = database.reader().unwrap();
        let settings = reader.connection_settings().unwrap();
        assert!(settings.0);
        assert_eq!(settings.1, "wal");
        assert_eq!(
            queries::current_schema_version(&reader.connection).unwrap(),
            Some(LATEST_SCHEMA_VERSION)
        );
        assert_eq!(
            queries::schema_version_row_count(&reader.connection).unwrap(),
            1
        );
        assert!(queries::named_table_exists(&reader.connection, "meetings").unwrap());
        assert!(queries::named_table_exists(&reader.connection, "passages_fts").unwrap());
        assert!(queries::named_table_exists(&reader.connection, "passage_vec").unwrap());
        assert_eq!(
            queries::table_column_count(&reader.connection, "meetings").unwrap(),
            11
        );
        let object_count = queries::user_schema_object_count(&reader.connection).unwrap();
        drop(reader);
        database.shutdown().unwrap();

        let reopened = Database::open(path.path()).unwrap();
        let reader = reopened.reader().unwrap();
        assert_eq!(
            queries::schema_version_row_count(&reader.connection).unwrap(),
            1
        );
        assert_eq!(
            queries::user_schema_object_count(&reader.connection).unwrap(),
            object_count
        );
        reopened.shutdown().unwrap();
    }

    #[test]
    fn meeting_delete_cascades_through_relational_fts_and_vector_data() {
        let path = TempDatabasePath::new();
        let database = Database::open(path.path()).unwrap();
        let writer = database.writer();
        let meeting_id = inserted_id(
            writer
                .execute(WriteCommand::InsertMeeting(sample_meeting()))
                .unwrap(),
        );
        let passage_id = inserted_id(
            writer
                .execute(WriteCommand::InsertPassage(passage_for(
                    meeting_id,
                    "cascade phrase",
                )))
                .unwrap(),
        );
        assert_eq!(
            writer
                .execute(WriteCommand::SeedTestGraph {
                    meeting_id,
                    passage_id,
                })
                .unwrap(),
            WriteResult::Done
        );
        assert_eq!(
            writer
                .execute(WriteCommand::DeleteMeeting(meeting_id))
                .unwrap(),
            WriteResult::Deleted(true)
        );

        let reader = database.reader().unwrap();
        for table in [
            "meetings",
            "pipeline_stages",
            "segments",
            "words",
            "summaries",
            "passages",
            "chats",
            "chat_turns",
            "chat_turn_sources",
            "citations",
        ] {
            assert_eq!(
                queries::count_rows(&reader.connection, table).unwrap(),
                0,
                "{table}"
            );
        }
        assert_eq!(
            queries::fts_match_count(&reader.connection, "cascade").unwrap(),
            0
        );
        assert_eq!(
            queries::vector_count(&reader.connection, passage_id).unwrap(),
            0
        );
    }

    #[test]
    fn passage_triggers_track_insert_update_and_delete() {
        let path = TempDatabasePath::new();
        let database = Database::open(path.path()).unwrap();
        let writer = database.writer();
        let meeting_id = inserted_id(
            writer
                .execute(WriteCommand::InsertMeeting(sample_meeting()))
                .unwrap(),
        );
        let passage_id = inserted_id(
            writer
                .execute(WriteCommand::InsertPassage(passage_for(
                    meeting_id,
                    "alpha phrase",
                )))
                .unwrap(),
        );
        writer
            .execute(WriteCommand::SeedTestGraph {
                meeting_id,
                passage_id,
            })
            .unwrap();

        let reader = database.reader().unwrap();
        assert_eq!(
            queries::fts_match_count(&reader.connection, "alpha").unwrap(),
            1
        );
        assert_eq!(
            queries::vector_count(&reader.connection, passage_id).unwrap(),
            1
        );
        writer
            .execute(WriteCommand::UpdatePassageText {
                id: passage_id,
                text: "beta phrase".to_owned(),
            })
            .unwrap();
        assert_eq!(
            reader.passage(passage_id).unwrap().unwrap().text,
            "beta phrase"
        );
        assert_eq!(
            queries::fts_match_count(&reader.connection, "alpha").unwrap(),
            0
        );
        assert_eq!(
            queries::fts_match_count(&reader.connection, "beta").unwrap(),
            1
        );
        assert_eq!(
            queries::vector_count(&reader.connection, passage_id).unwrap(),
            1
        );
        writer
            .execute(WriteCommand::DeletePassage(passage_id))
            .unwrap();
        assert_eq!(
            queries::fts_match_count(&reader.connection, "beta").unwrap(),
            0
        );
        assert_eq!(
            queries::vector_count(&reader.connection, passage_id).unwrap(),
            0
        );
    }

    #[test]
    fn concurrent_readers_progress_while_one_writer_commits() {
        let path = TempDatabasePath::new();
        let database = Database::open(path.path()).unwrap();
        let writer = database.writer();
        let meeting_id = inserted_id(
            writer
                .execute(WriteCommand::InsertMeeting(sample_meeting()))
                .unwrap(),
        );
        let barrier = Arc::new(Barrier::new(5));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let reader = database.reader().unwrap();
            let barrier = Arc::clone(&barrier);
            readers.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..100 {
                    assert_eq!(reader.meeting(meeting_id).unwrap().unwrap().id, meeting_id);
                }
            }));
        }

        barrier.wait();
        for idx in 0..100 {
            let result = writer
                .execute(WriteCommand::InsertPassage(passage_for(
                    meeting_id,
                    &format!("passage {idx}"),
                )))
                .unwrap();
            assert!(matches!(result, WriteResult::Inserted(_)));
        }
        for reader in readers {
            reader.join().unwrap();
        }
    }
}
