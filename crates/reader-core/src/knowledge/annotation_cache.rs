use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::Result;

const RETAINED_ANNOTATIONS: u32 = 2048;
const MAX_ANNOTATION_BYTES: usize = 65536;

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS linguistic_annotations (
            id INTEGER PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            cache_key BLOB NOT NULL,
            annotation_blob TEXT NOT NULL,
            UNIQUE(document_id, cache_key)
        );
        CREATE TABLE IF NOT EXISTS model_hints (
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            cache_key BLOB NOT NULL,
            annotation_blob TEXT NOT NULL,
            PRIMARY KEY(document_id, cache_key)
        ) WITHOUT ROWID;",
    )
    .map_err(|error| error.to_string())
}

pub(super) fn read(db: &Connection, document_id: &str, key: &[u8]) -> Option<Value> {
    read_value(
        db,
        "SELECT annotation_blob FROM linguistic_annotations WHERE document_id=?1 AND cache_key=?2",
        document_id,
        key,
    )
}

pub(crate) fn read_model_hint<T: serde::de::DeserializeOwned>(
    db: &Connection,
    document_id: &str,
    key: &[u8],
) -> Result<Option<T>> {
    let body: Option<String> = db
        .query_row(
            "SELECT annotation_blob FROM model_hints WHERE document_id=?1 AND cache_key=?2",
            params![document_id, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Не вдалося прочитати кеш підказок. {error}"))?;
    Ok(body.and_then(|body| serde_json::from_str(&body).ok()))
}

fn read_value(db: &Connection, query: &str, document_id: &str, key: &[u8]) -> Option<Value> {
    let body: Option<String> = db
        .query_row(query, params![document_id, key], |row| row.get(0))
        .optional()
        .ok()?;
    serde_json::from_str(&body?).ok()
}

pub(super) fn write(db: &Connection, document_id: &str, key: &[u8], annotation: &Value) {
    let body = annotation.to_string();
    if body.len() > MAX_ANNOTATION_BYTES {
        return;
    }

    // Derived data: a locked database or a concurrently deleted document
    // should not prevent returning the freshly computed analysis.
    let _ = save(db, document_id, key, &body);
}

pub(crate) fn write_model_hint(
    db: &Connection,
    document_id: &str,
    key: &[u8],
    hint: &impl serde::Serialize,
) -> Result<()> {
    let body = serde_json::to_string(hint).map_err(|error| error.to_string())?;
    if body.len() > MAX_ANNOTATION_BYTES {
        return Err("Підказка завелика для збереження.".into());
    }
    db.execute(
        "INSERT INTO model_hints (document_id, cache_key, annotation_blob) VALUES (?1, ?2, ?3)
         ON CONFLICT(document_id, cache_key) DO UPDATE SET annotation_blob=excluded.annotation_blob",
        params![document_id, key, body],
    )
    .map_err(|error| format!("Не вдалося зберегти підказку. {error}"))?;
    Ok(())
}

fn save(db: &Connection, document_id: &str, key: &[u8], body: &str) -> rusqlite::Result<()> {
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO linguistic_annotations (document_id, cache_key, annotation_blob)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(document_id, cache_key) DO UPDATE SET annotation_blob=excluded.annotation_blob",
        params![document_id, key, body],
    )?;
    tx.execute(
        "DELETE FROM linguistic_annotations WHERE id < (
            SELECT id FROM linguistic_annotations ORDER BY id DESC LIMIT 1 OFFSET ?1
        )",
        [RETAINED_ANNOTATIONS - 1],
    )?;
    tx.commit()
}
