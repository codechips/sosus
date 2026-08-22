//! The sole home for application SQL.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::asr::TranscriptResult;

use super::models::{Meeting, NewMeeting, NewPassage, Passage};

const CONNECTION_PRAGMAS: &str = "
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
";

const READER_PRAGMA: &str = "PRAGMA query_only = ON;";
const FOREIGN_KEYS_STATE: &str = "PRAGMA foreign_keys;";
const JOURNAL_MODE_STATE: &str = "PRAGMA journal_mode;";

const CREATE_SCHEMA_VERSION: &str = "
CREATE TABLE IF NOT EXISTS schema_version (
  version INTEGER NOT NULL,
  applied_at TEXT NOT NULL
);
";

const CURRENT_SCHEMA_VERSION: &str = "SELECT MAX(version) FROM schema_version;";
const RECORD_SCHEMA_VERSION: &str =
    "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2);";

pub(super) const MIGRATION_V1: &str = "
CREATE TABLE meetings (
  id            INTEGER PRIMARY KEY,
  started_at    TEXT    NOT NULL,
  ended_at      TEXT,
  title         TEXT,
  duration_s    REAL    NOT NULL DEFAULT 0,
  language      TEXT    NOT NULL DEFAULT '',
  audio_path    TEXT,
  audio_owned   INTEGER NOT NULL,
  source        TEXT    NOT NULL,
  speaker_count INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT    NOT NULL
);

CREATE TABLE pipeline_stages (
  meeting_id        INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  stage             TEXT    NOT NULL CHECK(stage IN ('transcribe', 'diarize', 'summarize', 'export', 'index')),
  status            TEXT    NOT NULL CHECK(status IN ('pending', 'running', 'completed', 'failed', 'cancelled', 'skipped')),
  attempt           INTEGER NOT NULL DEFAULT 0,
  input_fingerprint TEXT    NOT NULL DEFAULT '',
  implementation_id TEXT    NOT NULL DEFAULT '',
  started_at        TEXT,
  completed_at      TEXT,
  error_code        TEXT,
  PRIMARY KEY(meeting_id, stage)
);

CREATE TABLE segments (
  id         INTEGER PRIMARY KEY,
  meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  idx        INTEGER NOT NULL,
  start_s    REAL    NOT NULL,
  end_s      REAL    NOT NULL,
  speaker    TEXT,
  text       TEXT    NOT NULL
);
CREATE UNIQUE INDEX idx_segments_meeting ON segments(meeting_id, idx);

CREATE TABLE words (
  id         INTEGER PRIMARY KEY,
  segment_id INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
  start_s    REAL    NOT NULL,
  end_s      REAL    NOT NULL,
  text       TEXT    NOT NULL,
  score      REAL    NOT NULL DEFAULT 0,
  speaker    TEXT
);

CREATE TABLE summaries (
  id         INTEGER PRIMARY KEY,
  meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  template   TEXT    NOT NULL,
  body       TEXT    NOT NULL,
  model      TEXT    NOT NULL,
  created_at TEXT    NOT NULL
);

CREATE TABLE passages (
  id          INTEGER PRIMARY KEY,
  meeting_id  INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  start_s     REAL    NOT NULL,
  end_s       REAL    NOT NULL,
  speakers    TEXT    NOT NULL DEFAULT '',
  text        TEXT    NOT NULL,
  token_count INTEGER NOT NULL
);
CREATE INDEX idx_passages_meeting ON passages(meeting_id);

CREATE VIRTUAL TABLE passages_fts USING fts5(
  text, content='passages', content_rowid='id', tokenize='unicode61'
);

CREATE VIRTUAL TABLE passage_vec USING vec0(
  passage_id INTEGER PRIMARY KEY,
  meeting_id INTEGER,
  embedding FLOAT[384]
);

CREATE TRIGGER passages_ai AFTER INSERT ON passages BEGIN
  INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER passages_ad AFTER DELETE ON passages BEGIN
  INSERT INTO passages_fts(passages_fts, rowid, text)
    VALUES ('delete', old.id, old.text);
  DELETE FROM passage_vec WHERE passage_id = old.id;
END;

CREATE TRIGGER passages_au AFTER UPDATE OF text ON passages BEGIN
  INSERT INTO passages_fts(passages_fts, rowid, text)
    VALUES ('delete', old.id, old.text);
  INSERT INTO passages_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TABLE chats (
  id         INTEGER PRIMARY KEY,
  scope_meeting_id INTEGER REFERENCES meetings(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL
);

CREATE TABLE chat_turns (
  id         INTEGER PRIMARY KEY,
  chat_id    INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
  role       TEXT    NOT NULL,
  content    TEXT    NOT NULL,
  created_at TEXT    NOT NULL
);

CREATE TABLE chat_turn_sources (
  chat_turn_id INTEGER NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
  meeting_id   INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  PRIMARY KEY(chat_turn_id, meeting_id)
);

CREATE TABLE citations (
  id           INTEGER PRIMARY KEY,
  chat_turn_id INTEGER NOT NULL REFERENCES chat_turns(id) ON DELETE CASCADE,
  passage_id   INTEGER NOT NULL REFERENCES passages(id) ON DELETE CASCADE,
  quote        TEXT    NOT NULL,
  verified     INTEGER NOT NULL
);

CREATE TRIGGER meetings_chat_cleanup_bd BEFORE DELETE ON meetings BEGIN
  DELETE FROM chats
  WHERE id IN (
    SELECT DISTINCT ct.chat_id
    FROM chat_turns ct
    JOIN chat_turn_sources src ON src.chat_turn_id = ct.id
    WHERE src.meeting_id = old.id
  );
END;
";

pub(super) fn configure_connection(connection: &Connection, reader: bool) -> rusqlite::Result<()> {
    connection.execute_batch(CONNECTION_PRAGMAS)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    if reader {
        connection.execute_batch(READER_PRAGMA)?;
    }
    Ok(())
}

pub(super) fn connection_settings(connection: &Connection) -> rusqlite::Result<(bool, String)> {
    let foreign_keys = connection.query_row(FOREIGN_KEYS_STATE, [], |row| row.get::<_, i64>(0))?;
    let journal_mode = connection.query_row(JOURNAL_MODE_STATE, [], |row| row.get(0))?;
    Ok((foreign_keys == 1, journal_mode))
}

pub(super) fn create_schema_version(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(CREATE_SCHEMA_VERSION)
}

pub(super) fn current_schema_version(connection: &Connection) -> rusqlite::Result<Option<i64>> {
    connection.query_row(CURRENT_SCHEMA_VERSION, [], |row| row.get(0))
}

pub(super) fn apply_v1(transaction: &Transaction<'_>, applied_at: &str) -> rusqlite::Result<()> {
    transaction.execute_batch(MIGRATION_V1)?;
    transaction.execute(RECORD_SCHEMA_VERSION, params![1_i64, applied_at])?;
    Ok(())
}

const INSERT_MEETING: &str = "
INSERT INTO meetings (
  started_at, ended_at, title, duration_s, language, audio_path,
  audio_owned, source, speaker_count, created_at
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);
";
const DELETE_MEETING: &str = "DELETE FROM meetings WHERE id = ?1;";
#[allow(dead_code)]
const SELECT_MEETING: &str = "
SELECT id, started_at, ended_at, title, duration_s, language, audio_path,
       audio_owned, source, speaker_count, created_at
FROM meetings WHERE id = ?1;
";

pub(super) fn insert_meeting(
    connection: &Connection,
    meeting: &NewMeeting,
) -> rusqlite::Result<i64> {
    connection.execute(
        INSERT_MEETING,
        params![
            meeting.started_at,
            meeting.ended_at,
            meeting.title,
            meeting.duration_s,
            meeting.language,
            meeting.audio_path,
            i64::from(meeting.audio_owned),
            meeting.source,
            meeting.speaker_count,
            meeting.created_at,
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

pub(super) fn insert_transcript(
    connection: &Connection,
    meeting_id: i64,
    transcript: &TranscriptResult,
    speaker_count: usize,
) -> rusqlite::Result<()> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "UPDATE meetings SET speaker_count = ?2 WHERE id = ?1",
        params![meeting_id, i64::try_from(speaker_count).unwrap_or(i64::MAX)],
    )?;
    for (index, segment) in transcript.segments.iter().enumerate() {
        transaction.execute(
            "INSERT INTO segments (meeting_id, idx, start_s, end_s, speaker, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                meeting_id,
                i64::try_from(index).unwrap_or(i64::MAX),
                segment.start_seconds,
                segment.end_seconds,
                segment.speaker,
                segment.text,
            ],
        )?;
        let segment_id = transaction.last_insert_rowid();
        for word in &segment.words {
            transaction.execute(
                "INSERT INTO words (segment_id, start_s, end_s, text, score, speaker) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    segment_id,
                    word.start_seconds,
                    word.end_seconds,
                    word.text,
                    f64::from(word.score),
                    word.speaker,
                ],
            )?;
        }
    }
    transaction.commit()
}

pub(super) fn delete_meeting(connection: &Connection, id: i64) -> rusqlite::Result<bool> {
    Ok(connection.execute(DELETE_MEETING, params![id])? == 1)
}

#[allow(dead_code)]
pub(super) fn meeting(connection: &Connection, id: i64) -> rusqlite::Result<Option<Meeting>> {
    connection
        .query_row(SELECT_MEETING, params![id], |row| {
            Ok(Meeting {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                title: row.get(3)?,
                duration_s: row.get(4)?,
                language: row.get(5)?,
                audio_path: row.get(6)?,
                audio_owned: row.get::<_, i64>(7)? == 1,
                source: row.get(8)?,
                speaker_count: row.get(9)?,
                created_at: row.get(10)?,
            })
        })
        .optional()
}

const INSERT_PASSAGE: &str = "
INSERT INTO passages (meeting_id, start_s, end_s, speakers, text, token_count)
VALUES (?1, ?2, ?3, ?4, ?5, ?6);
";
const UPDATE_PASSAGE_TEXT: &str = "UPDATE passages SET text = ?2 WHERE id = ?1;";
const DELETE_PASSAGE: &str = "DELETE FROM passages WHERE id = ?1;";
#[allow(dead_code)]
const SELECT_PASSAGE: &str = "
SELECT id, meeting_id, start_s, end_s, speakers, text, token_count
FROM passages WHERE id = ?1;
";

pub(super) fn insert_passage(
    connection: &Connection,
    passage: &NewPassage,
) -> rusqlite::Result<i64> {
    connection.execute(
        INSERT_PASSAGE,
        params![
            passage.meeting_id,
            passage.start_s,
            passage.end_s,
            passage.speakers,
            passage.text,
            passage.token_count,
        ],
    )?;
    Ok(connection.last_insert_rowid())
}

pub(super) fn update_passage_text(
    connection: &Connection,
    id: i64,
    text: &str,
) -> rusqlite::Result<bool> {
    Ok(connection.execute(UPDATE_PASSAGE_TEXT, params![id, text])? == 1)
}

pub(super) fn delete_passage(connection: &Connection, id: i64) -> rusqlite::Result<bool> {
    Ok(connection.execute(DELETE_PASSAGE, params![id])? == 1)
}

#[allow(dead_code)]
pub(super) fn passage(connection: &Connection, id: i64) -> rusqlite::Result<Option<Passage>> {
    connection
        .query_row(SELECT_PASSAGE, params![id], |row| {
            Ok(Passage {
                id: row.get(0)?,
                meeting_id: row.get(1)?,
                start_s: row.get(2)?,
                end_s: row.get(3)?,
                speakers: row.get(4)?,
                text: row.get(5)?,
                token_count: row.get(6)?,
            })
        })
        .optional()
}

#[cfg(test)]
const COUNT_SCHEMA_VERSION_ROWS: &str = "SELECT COUNT(*) FROM schema_version;";
#[cfg(test)]
const COUNT_USER_SCHEMA_OBJECTS: &str = "
SELECT COUNT(*) FROM sqlite_schema
WHERE name NOT LIKE 'sqlite_%' AND name NOT LIKE '%_shadow';
";
#[cfg(test)]
const INSERT_SEGMENT: &str = "
INSERT INTO segments (meeting_id, idx, start_s, end_s, text)
VALUES (?1, 0, 0, 1, 'segment');
";
#[cfg(test)]
const INSERT_WORD: &str = "
INSERT INTO words (segment_id, start_s, end_s, text)
VALUES (?1, 0, 1, 'word');
";
#[cfg(test)]
const INSERT_SUMMARY: &str = "
INSERT INTO summaries (meeting_id, template, body, model, created_at)
VALUES (?1, 'meeting', 'summary', 'test', '2026-08-21T10:00:00Z');
";
#[cfg(test)]
const INSERT_PIPELINE_STAGE: &str = "
INSERT INTO pipeline_stages (meeting_id, stage, status)
VALUES (?1, 'transcribe', 'pending');
";
#[cfg(test)]
const INSERT_CHAT: &str = "
INSERT INTO chats (scope_meeting_id, created_at)
VALUES (NULL, '2026-08-21T10:00:00Z');
";
#[cfg(test)]
const INSERT_CHAT_TURN: &str = "
INSERT INTO chat_turns (chat_id, role, content, created_at)
VALUES (?1, 'assistant', 'answer', '2026-08-21T10:00:00Z');
";
#[cfg(test)]
const INSERT_CHAT_TURN_SOURCE: &str = "
INSERT INTO chat_turn_sources (chat_turn_id, meeting_id) VALUES (?1, ?2);
";
#[cfg(test)]
const INSERT_CITATION: &str = "
INSERT INTO citations (chat_turn_id, passage_id, quote, verified)
VALUES (?1, ?2, 'text', 1);
";
#[cfg(test)]
const INSERT_VECTOR: &str = "
INSERT INTO passage_vec (passage_id, meeting_id, embedding)
VALUES (?1, ?2, ?3);
";
#[cfg(test)]
const COUNT_TABLE: &str = "SELECT COUNT(*) FROM pragma_table_info(?1);";
#[cfg(test)]
const COUNT_NAMED_TABLE: &str =
    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1;";
#[cfg(test)]
const COUNT_ROWS_TEMPLATE: &str = "SELECT COUNT(*) FROM ";
#[cfg(test)]
const FTS_MATCH_COUNT: &str = "SELECT COUNT(*) FROM passages_fts WHERE passages_fts MATCH ?1;";
#[cfg(test)]
const VECTOR_COUNT: &str = "SELECT COUNT(*) FROM passage_vec WHERE passage_id = ?1;";

#[cfg(test)]
pub(super) fn schema_version_row_count(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row(COUNT_SCHEMA_VERSION_ROWS, [], |row| row.get(0))
}

#[cfg(test)]
pub(super) fn user_schema_object_count(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row(COUNT_USER_SCHEMA_OBJECTS, [], |row| row.get(0))
}

#[cfg(test)]
pub(super) fn named_table_exists(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    let count =
        connection.query_row(COUNT_NAMED_TABLE, params![name], |row| row.get::<_, i64>(0))?;
    Ok(count == 1)
}

#[cfg(test)]
pub(super) fn table_column_count(connection: &Connection, name: &str) -> rusqlite::Result<i64> {
    connection.query_row(COUNT_TABLE, params![name], |row| row.get(0))
}

#[cfg(test)]
pub(super) fn seed_cascade_graph(
    connection: &Connection,
    meeting_id: i64,
    passage_id: i64,
) -> rusqlite::Result<()> {
    connection.execute(INSERT_PIPELINE_STAGE, params![meeting_id])?;
    connection.execute(INSERT_SEGMENT, params![meeting_id])?;
    let segment_id = connection.last_insert_rowid();
    connection.execute(INSERT_WORD, params![segment_id])?;
    connection.execute(INSERT_SUMMARY, params![meeting_id])?;
    connection.execute(INSERT_CHAT, [])?;
    let chat_id = connection.last_insert_rowid();
    connection.execute(INSERT_CHAT_TURN, params![chat_id])?;
    let turn_id = connection.last_insert_rowid();
    connection.execute(INSERT_CHAT_TURN_SOURCE, params![turn_id, meeting_id])?;
    connection.execute(INSERT_CITATION, params![turn_id, passage_id])?;
    Ok(())
}

#[cfg(test)]
pub(super) fn insert_zero_vector(
    connection: &Connection,
    passage_id: i64,
    meeting_id: i64,
) -> rusqlite::Result<()> {
    let embedding = vec![0_u8; 384 * size_of::<f32>()];
    connection.execute(INSERT_VECTOR, params![passage_id, meeting_id, embedding])?;
    Ok(())
}

#[cfg(test)]
pub(super) fn count_rows(connection: &Connection, table: &str) -> rusqlite::Result<i64> {
    let allowed = [
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
    ];
    if !allowed.contains(&table) {
        return Err(rusqlite::Error::InvalidParameterName(table.to_owned()));
    }
    let sql = format!("{COUNT_ROWS_TEMPLATE}{table};");
    connection.query_row(&sql, [], |row| row.get(0))
}

#[cfg(test)]
pub(super) fn fts_match_count(connection: &Connection, query: &str) -> rusqlite::Result<i64> {
    connection.query_row(FTS_MATCH_COUNT, params![query], |row| row.get(0))
}

#[cfg(test)]
pub(super) fn vector_count(connection: &Connection, passage_id: i64) -> rusqlite::Result<i64> {
    connection.query_row(VECTOR_COUNT, params![passage_id], |row| row.get(0))
}
