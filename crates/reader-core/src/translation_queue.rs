use crate::{translation::ParagraphTranslation, valid_language, Document, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

const RETRY_SECONDS: i64 = 15;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranslationWindowAddress {
    sentence_id: String,
    revision_id: String,
    target: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationWindowRequest {
    pub sentence_id: String,
    pub revision_id: String,
    pub target: String,
    /// Retry deadline in Unix seconds, or zero when there is no scheduled backoff.
    pub available_at: i64,
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS translation_windows (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            sentence_id TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            source TEXT NOT NULL,
            target TEXT NOT NULL,
            lease TEXT,
            available_at INTEGER NOT NULL DEFAULT 0
        );",
    )
    .map_err(database_error)
}

impl Store {
    pub fn schedule_translation_window(
        &mut self,
        id: &str,
        sentence_id: &str,
        revision: &str,
        target: &str,
    ) -> Result<()> {
        valid_language(target)?;
        self.edit_document(id, |document, tx| {
            if document.language == target {
                return Err("Мова перекладу має відрізнятися від мови джерела".into());
            }
            if document.current_revision_id.as_deref() != Some(revision)
                || !document.sentences.iter().any(|sentence| sentence.id == sentence_id)
            {
                return Err("Запит перекладу стосується застарілого тексту.".into());
            }
            tx.execute(
                "INSERT INTO translation_windows (document_id,sentence_id,revision_id,source,target)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(document_id) DO UPDATE SET
                    sentence_id=excluded.sentence_id, revision_id=excluded.revision_id,
                    source=excluded.source, target=excluded.target, lease=NULL, available_at=0
                 WHERE translation_windows.sentence_id<>excluded.sentence_id
                    OR translation_windows.revision_id<>excluded.revision_id
                    OR translation_windows.source<>excluded.source
                    OR translation_windows.target<>excluded.target",
                params![id, sentence_id, revision, document.language, target],
            )
            .map_err(database_error)?;
            Ok(false)
        })
    }

    pub fn pending_translation_window(&self, id: &str) -> Result<Option<TranslationWindowRequest>> {
        self.connection.query_row(
            "SELECT w.sentence_id,w.revision_id,w.target,
                    CASE WHEN w.lease IS NULL THEN w.available_at ELSE 0 END
             FROM translation_windows w JOIN documents d ON d.id=w.document_id
             WHERE w.document_id=?1 AND w.revision_id=json_extract(d.body,'$.currentRevisionId')
               AND w.source=json_extract(d.body,'$.language')",
            [id],
            |row| Ok(TranslationWindowRequest {
                sentence_id: row.get(0)?,
                revision_id: row.get(1)?,
                target: row.get(2)?,
                available_at: row.get(3)?,
            }),
        )
        .optional()
        .map_err(database_error)
    }

    pub fn cancel_translation_window(&self, id: &str) -> Result<bool> {
        self.connection
            .execute("DELETE FROM translation_windows WHERE document_id=?1", [id])
            .map(|affected| affected != 0)
            .map_err(database_error)
    }

    pub(crate) fn run_translation_window(
        &mut self, id: &str, expected: Option<&TranslationWindowAddress>,
    ) -> Result<Option<ParagraphTranslation>> {
        let Some(_process_lock) = crate::file_originals::try_worker_lock(
            &self.file_original_directory, ".translation-window.lock",
        )? else {
            return Ok(None);
        };
        // With the worker lock held, a leftover lease cannot belong to a live worker.
        let tx = self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let now = crate::now();
        let job: Option<(String, String, String)> = tx.query_row(
            "SELECT w.sentence_id,w.target,d.body FROM translation_windows w
             JOIN documents d ON d.id=w.document_id
             WHERE w.document_id=?1 AND (w.lease IS NOT NULL OR w.available_at<=?2)
               AND w.revision_id=json_extract(d.body,'$.currentRevisionId')
               AND w.source=json_extract(d.body,'$.language')
               AND (?3 IS NULL OR (w.sentence_id=?3 AND w.revision_id=?4 AND w.target=?5))",
            params![
                id, now,
                expected.map(|request| request.sentence_id.as_str()),
                expected.map(|request| request.revision_id.as_str()),
                expected.map(|request| request.target.as_str()),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(database_error)?;
        let Some((sentence_id, target, body)) = job else {
            return Ok(None);
        };
        let document: Document = serde_json::from_str(&body).map_err(database_error)?;
        let selected = document.sentences.iter().position(|sentence| sentence.id == sentence_id)
            .ok_or("Речення для фонового перекладу не знайдено.")?;
        let lease = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "UPDATE translation_windows SET lease=?2,available_at=0 WHERE document_id=?1",
            params![id, lease],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;

        let result = self.translate_document_paragraph(&document, selected, &target, true);
        let complete = result.as_ref().is_ok_and(|window| {
            window.units.iter().all(|unit| unit.translation.is_some())
        });
        let tx = self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        // Superseded requests may have populated reusable cache entries.
        // Only the current lease may deliver a window to the caller.
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM translation_windows w JOIN documents d ON d.id=w.document_id
             WHERE w.document_id=?1 AND w.lease=?2
               AND w.revision_id=json_extract(d.body,'$.currentRevisionId')
               AND w.source=json_extract(d.body,'$.language'))",
            params![id, lease],
            |row| row.get(0),
        )
        .map_err(database_error)?;
        if complete || !current {
            tx.execute(
                "DELETE FROM translation_windows WHERE document_id=?1 AND lease=?2",
                params![id, lease],
            )
            .map_err(database_error)?;
        } else {
            tx.execute(
                "UPDATE translation_windows SET lease=NULL,available_at=?3
                 WHERE document_id=?1 AND lease=?2",
                params![id, lease, crate::now() + RETRY_SECONDS],
            )
            .map_err(database_error)?;
        }
        tx.commit().map_err(database_error)?;
        if current {
            result.map(|window| complete.then_some(window))
        } else {
            Ok(None)
        }
    }

}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка черги перекладу. {error}")
}
