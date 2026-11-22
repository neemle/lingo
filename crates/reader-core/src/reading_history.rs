use crate::{Document, Result, Store};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

const LATEST_TIMESTAMP: i64 = 253_402_300_799;
const RECENT_LIMIT: u32 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingSession {
    pub id: String,
    pub document_id: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub active_duration_seconds: i64,
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS reading_sessions (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            started_at INTEGER NOT NULL,
            ended_at INTEGER NOT NULL,
            active_duration_seconds INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS reading_sessions_recent
           ON reading_sessions(ended_at DESC, started_at DESC, id DESC);",
    )
    .map_err(error)
}

impl Store {
    pub fn checkpoint_reading(&self, session: ReadingSession) -> Result<()> {
        write(&self.connection, &session, &session.document_id)
    }

    pub fn recent_reading_sessions(&self) -> Result<Vec<ReadingSession>> {
        read(&self.connection, i64::from(RECENT_LIMIT))
    }
}

pub(crate) fn export(db: &Connection) -> Result<Vec<ReadingSession>> {
    read(db, -1)
}

pub(crate) fn restore(
    db: &Connection,
    sessions: Vec<ReadingSession>,
    documents: &[Document],
) -> Result<()> {
    if sessions.is_empty() {
        return Ok(());
    }
    let hashes: HashMap<_, _> = documents
        .iter()
        .map(|doc| (doc.id.as_str(), doc.source_hash.as_str()))
        .collect();
    let mut document_ids = HashMap::<&str, String>::new();
    let mut resolve = db
        .prepare_cached("SELECT id FROM documents WHERE source_hash=?1")
        .map_err(error)?;
    for session in &sessions {
        let local_id = match document_ids.get(session.document_id.as_str()) {
            Some(id) => id,
            None => {
                let hash = hashes
                    .get(session.document_id.as_str())
                    .ok_or("В архіві немає документа для запису історії читання.")?;
                let id = resolve.query_row([hash], |row| row.get(0)).map_err(error)?;
                document_ids.entry(&session.document_id).or_insert(id)
            }
        };
        write(db, session, local_id)?;
    }
    Ok(())
}

fn read(db: &Connection, limit: i64) -> Result<Vec<ReadingSession>> {
    let mut statement = db
        .prepare(
            "SELECT id,document_id,started_at,ended_at,active_duration_seconds FROM reading_sessions
             ORDER BY ended_at DESC, started_at DESC, id DESC LIMIT ?1",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([limit], |row| {
            Ok(ReadingSession {
                id: row.get(0)?,
                document_id: row.get(1)?,
                started_at: row.get(2)?,
                ended_at: row.get(3)?,
                active_duration_seconds: row.get(4)?,
            })
        })
        .map_err(error)?;
    rows.map(|row| row.map_err(error)).collect()
}

fn write(db: &Connection, session: &ReadingSession, document_id: &str) -> Result<()> {
    if session.id.len() != 36
        || !Uuid::parse_str(&session.id).is_ok_and(|id| id.to_string() == session.id)
        || !(0..=LATEST_TIMESTAMP).contains(&session.started_at)
        || !(session.started_at..=LATEST_TIMESTAMP).contains(&session.ended_at)
        || !(0..=session.ended_at - session.started_at).contains(&session.active_duration_seconds)
    {
        return Err("Некоректний запис історії читання.".into());
    }
    let changed = db.execute(
        "INSERT INTO reading_sessions(id,document_id,started_at,ended_at,active_duration_seconds)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(id) DO UPDATE SET
            ended_at=MAX(reading_sessions.ended_at,excluded.ended_at),
            active_duration_seconds=MAX(reading_sessions.active_duration_seconds,excluded.active_duration_seconds)
         WHERE reading_sessions.document_id=excluded.document_id AND reading_sessions.started_at=excluded.started_at",
        params![session.id,document_id,session.started_at,session.ended_at,session.active_duration_seconds],
    ).map_err(error)?;
    if changed == 0 {
        return Err("Конфлікт ідентифікаторів історії читання.".into());
    }
    Ok(())
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backup;

    #[test]
    fn reading_history_persists_checkpoints_and_merges_backups_by_document_content() {
        let root = tempfile::tempdir().unwrap();
        let original_path = root.path().join("original.db");
        let mut original = Store::open(&original_path).unwrap();
        let bytes = b"Bonjour. On commence la lecture.";
        let original_doc = original.import("book.txt", bytes, "fr").unwrap();
        let opened = ReadingSession {
            id: Uuid::new_v4().to_string(),
            document_id: original_doc.id,
            started_at: 1000,
            ended_at: 1000,
            active_duration_seconds: 0,
        };
        original.checkpoint_reading(opened.clone()).unwrap();
        let latest = ReadingSession {
            ended_at: 1100,
            active_duration_seconds: 15,
            ..opened.clone()
        };
        original.checkpoint_reading(latest.clone()).unwrap();
        original.checkpoint_reading(opened).unwrap();
        assert!(original
            .checkpoint_reading(ReadingSession {
                active_duration_seconds: 101,
                ..latest.clone()
            })
            .is_err());
        drop(original);
        let original = Store::open(&original_path).unwrap();
        assert_eq!(
            original.recent_reading_sessions().unwrap(),
            [latest.clone()]
        );

        let mut backup = serde_json::to_value(original.export().unwrap()).unwrap();
        let mut destination = Store::open(&root.path().join("destination.db")).unwrap();
        let local_doc = destination.import("renamed.txt", bytes, "fr").unwrap();
        assert_ne!(latest.document_id, local_doc.id);
        for _ in 0..2 {
            destination
                .restore(serde_json::from_value(backup.clone()).unwrap())
                .unwrap();
        }
        let restored = ReadingSession {
            document_id: local_doc.id,
            ..latest
        };
        assert_eq!(
            destination.recent_reading_sessions().unwrap(),
            [restored.clone()]
        );

        backup.as_object_mut().unwrap().remove("readingSessions");
        let legacy: Backup = serde_json::from_value(backup).unwrap();
        destination.restore(legacy).unwrap();
        assert_eq!(destination.recent_reading_sessions().unwrap(), [restored]);
    }
}
