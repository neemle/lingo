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
        );
        CREATE TABLE IF NOT EXISTS background_translation_progress (
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            target TEXT NOT NULL,
            revision_id TEXT NOT NULL,
            cursor INTEGER NOT NULL,
            incomplete INTEGER NOT NULL,
            resume_at INTEGER,
            PRIMARY KEY(document_id, target)
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

        let result = self.translate_document_paragraph(&document, selected, &target, true, crate::translation::PageBoundaries::Canonical);
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationWarmStep {
    next: Option<usize>,
    incomplete: bool,
    #[serde(skip)]
    translated_page: Option<u16>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTranslationProgress {
    // Zero means the next step is ready. No deadline means the document is complete.
    resume_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translated_page: Option<u16>,
}

impl Store {
    pub fn advance_background_translation(
        &mut self,
        id: &str,
        revision: &str,
        target: &str,
    ) -> Result<BackgroundTranslationProgress> {
        valid_language(target)?;
        let done = BackgroundTranslationProgress {
            resume_at: None,
            translated_page: None,
        };
        if self.reader_preferences()?.background_translation != Some(true) {
            return Ok(done);
        }
        let Some(_lock) = crate::file_originals::try_worker_lock(
            &self.file_original_directory,
            ".background-translation.lock",
        )?
        else {
            return Ok(BackgroundTranslationProgress {
                resume_at: Some(crate::now() + 2),
                ..done
            });
        };
        let document = self.translation_document(id)?;
        if document.current_revision_id.as_deref() != Some(revision) || document.language == target
        {
            return Ok(done);
        }
        let (cursor, incomplete, resume_at) = self
            .connection
            .query_row(
                "SELECT cursor,incomplete,resume_at FROM background_translation_progress
             WHERE document_id=?1 AND revision_id=?2 AND target=?3",
                params![id, revision, target],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?
            .unwrap_or((0, false, Some(0)));
        let now = crate::now();
        if resume_at.is_none_or(|deadline| deadline > now) {
            return Ok(BackgroundTranslationProgress { resume_at, ..done });
        }
        let (cursor, incomplete, resume_at, translated_page) = if cursor == 0
            && !self
                .translation_readiness(&document.language, target)?
                .ready
        {
            (0, false, Some(now + 30), None)
        } else {
            let step = self.warm_document_translation_step(&document, target, cursor as usize)?;
            match step.next {
                Some(next) => (
                    next,
                    incomplete || step.incomplete,
                    Some(0),
                    step.translated_page,
                ),
                None if incomplete || document.format == "pdf" => {
                    (0, false, Some(crate::now() + 300), None)
                }
                None => (0, false, None, None),
            }
        };
        self.connection.execute(
            "INSERT INTO background_translation_progress(document_id,target,revision_id,cursor,incomplete,resume_at)
             SELECT id,?2,?3,?4,?5,?6 FROM documents
             WHERE id=?1 AND json_extract(body,'$.currentRevisionId')=?3
             ON CONFLICT(document_id,target) DO UPDATE SET revision_id=excluded.revision_id,
               cursor=excluded.cursor,incomplete=excluded.incomplete,resume_at=excluded.resume_at",
            params![id, target, revision, i64::try_from(cursor).map_err(database_error)?, incomplete, resume_at],
        ).map_err(database_error)?;
        Ok(BackgroundTranslationProgress {
            resume_at,
            translated_page,
        })
    }

    pub fn warm_translation_step(
        &mut self,
        id: &str,
        revision: &str,
        target: &str,
        start: usize,
    ) -> Result<TranslationWarmStep> {
        let done = TranslationWarmStep {
            next: None,
            incomplete: false,
            translated_page: None,
        };
        if self.reader_preferences()?.background_translation != Some(true) {
            return Ok(done);
        }
        let document = self.translation_document(id)?;
        if document.current_revision_id.as_deref() != Some(revision) || document.language == target
        {
            return Ok(done);
        }
        self.warm_document_translation_step(&document, target, start)
    }

    fn warm_document_translation_step(
        &mut self,
        document: &Document,
        target: &str,
        start: usize,
    ) -> Result<TranslationWarmStep> {
        let done = TranslationWarmStep {
            next: None,
            incomplete: false,
            translated_page: None,
        };
        let page_count = document
            .pdf_page_starts
            .as_ref()
            .map(Vec::len)
            .or(document.ocr_pdf_pages.map(usize::from))
            .unwrap_or(0);
        if document.format == "pdf" && page_count > 0 {
            let canonical = (start..page_count).find(|&page| {
                u16::try_from(page).ok().is_some_and(|page| {
                    crate::pdf_reading_position::page_bounds(document, page).is_some()
                })
            });
            let detached: Option<u16> = self
                .connection
                .query_row(
                    "SELECT min(page) FROM pdf_detached_pages WHERE document_id=?1 AND page>=?2",
                    params![document.id, i64::try_from(start).unwrap_or(i64::MAX)],
                    |row| row.get(0),
                )
                .map_err(database_error)?;
            let Some(page) = canonical.into_iter().chain(detached.map(usize::from)).min() else {
                return Ok(done);
            };
            let rows = self.pdf_page_translation_with_priority(
                &document.id,
                page as u16,
                target,
                false,
                true,
            );
            let translated_page = rows
                .as_ref()
                .ok()
                .filter(|rows| rows.iter().any(|row| row.text.is_some()))
                .map(|_| page as u16);
            return Ok(TranslationWarmStep {
                next: Some(page + 1),
                incomplete: rows.map_or(true, |rows| rows.iter().any(|row| row.text.is_none())),
                translated_page,
            });
        }
        if start >= document.sentences.len() {
            return Ok(done);
        }
        let translated = self.translate_document_paragraph(
            document,
            start,
            target,
            true,
            crate::translation::PageBoundaries::Canonical,
        );
        let Ok(translated) = translated else {
            return Ok(TranslationWarmStep {
                next: Some(start + 1),
                incomplete: true,
                translated_page: None,
            });
        };
        let next = document.sentences[start..]
            .iter()
            .take(16)
            .take_while(|sentence| {
                translated
                    .units
                    .iter()
                    .any(|unit| unit.sentence_ids.contains(&sentence.id))
            })
            .count()
            .max(1)
            + start;
        Ok(TranslationWarmStep {
            next: Some(next),
            incomplete: translated
                .units
                .iter()
                .any(|unit| unit.translation.is_none()),
            translated_page: None,
        })
    }
}
