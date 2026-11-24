use crate::{translation::{ParagraphTranslation, TranslationBatchError}, valid_language, Document, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

const RETRY_SECONDS: i64 = 15;
const INTERACTIVE_RETRY_SECONDS: i64 = 2;
const PDF_OCR_POLL_SECONDS: i64 = 15;
const PAGE_TRANSLATION_REVISION: u32 = 3;

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
    let tx = rusqlite::Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(database_error)?;
    tx.execute_batch(
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
            ocr_generation INTEGER NOT NULL DEFAULT 0,
            route_identity TEXT,
            PRIMARY KEY(document_id, target)
        );",
    )
    .map_err(database_error)?;
    let has_generation: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('background_translation_progress')
         WHERE name='ocr_generation')",
            [],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if !has_generation {
        tx.execute_batch(
            "ALTER TABLE background_translation_progress ADD COLUMN ocr_generation INTEGER NOT NULL DEFAULT 0;"
        ).map_err(database_error)?;
    }
    let has_route_identity: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('background_translation_progress')
             WHERE name='route_identity')",
            [],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if !has_route_identity {
        tx.execute_batch("ALTER TABLE background_translation_progress ADD COLUMN route_identity TEXT;")
            .map_err(database_error)?;
    }
    tx.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS background_translation_new_pdf_text
         AFTER INSERT ON pdf_detached_pages WHEN length(NEW.text) > 0
         BEGIN
             UPDATE background_translation_progress
             SET cursor=min(cursor,max(0,NEW.page-1)), resume_at=0, ocr_generation=ocr_generation+1
             WHERE document_id=NEW.document_id;
         END;
         CREATE TRIGGER IF NOT EXISTS background_translation_changed_pdf_text
         AFTER UPDATE OF text ON pdf_detached_pages WHEN OLD.text <> NEW.text
         BEGIN
             UPDATE background_translation_progress
             SET cursor=min(cursor,max(0,NEW.page-1)), resume_at=0, ocr_generation=ocr_generation+1
             WHERE document_id=NEW.document_id;
         END;
         CREATE TRIGGER IF NOT EXISTS background_translation_changed_pdf_layout
         AFTER UPDATE OF lines,ocr_language ON pdf_detached_pages
         WHEN OLD.text = NEW.text AND (OLD.lines IS NOT NEW.lines OR OLD.ocr_language IS NOT NEW.ocr_language)
         BEGIN
             UPDATE background_translation_progress
             SET cursor=min(cursor,max(0,NEW.page-1)), resume_at=0, ocr_generation=ocr_generation+1
             WHERE document_id=NEW.document_id;
         END;",
    )
    .map_err(database_error)?;
    tx.commit().map_err(database_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_checkpoint_reopens_when_its_route_disappears() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let (id, revision) = {
            let mut store = Store::open(&path).unwrap();
            let document = store.import("reader.txt", b"Bonjour le monde.", "fr").unwrap();
            store.update_reader_preference(crate::reader_preferences::ReaderPreferenceChange::BackgroundTranslation { value: true }).unwrap();
            let revision = document.current_revision_id.unwrap();
            store.connection.execute(
                "INSERT INTO background_translation_progress(document_id,target,revision_id,cursor,incomplete,resume_at,route_identity)
                 VALUES (?1,'uk',?2,8,0,NULL,'previously installed route')",
                params![document.id, revision],
            ).unwrap();
            (document.id, revision)
        };

        let mut store = Store::open(&path).unwrap();
        let progress = store.advance_background_translation(&id, &revision, "uk").unwrap();
        assert!(progress.missing_packages.is_some());
        let deadline = progress.resume_at.unwrap();
        let cursor: u32 = store.connection.query_row(
            "SELECT cursor FROM background_translation_progress WHERE document_id=?1 AND target='uk'",
            [&id], |row| row.get(0),
        ).unwrap();
        assert_eq!(cursor, 0);
        assert_eq!(store.advance_background_translation(&id, &revision, "uk").unwrap().resume_at, Some(deadline));
    }

    #[test]
    fn source_language_change_restarts_progress_and_rejects_an_old_step() {
        use std::cell::{Cell, RefCell};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let mut worker = Store::open(&path).unwrap();
        let document = worker.import("reader.txt", b"Ein Schiff kommt an.", "fr").unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        worker.update_reader_preference(
            crate::reader_preferences::ReaderPreferenceChange::BackgroundTranslation { value: true },
        ).unwrap();
        worker.connection.execute(
            "INSERT INTO background_translation_progress
                (document_id,target,revision_id,cursor,incomplete,resume_at,ocr_generation,route_identity)
             VALUES (?1,'uk',?2,8,1,0,4,'')",
            params![document.id, revision],
        ).unwrap();

        let editor = RefCell::new(Store::open(&path).unwrap());
        let invocations = Cell::new(0);
        let old_step = worker.advance_background_translation_until_canceled(
            &document.id, revision, "uk", &|| {
                let count = invocations.get() + 1;
                invocations.set(count);
                if count == 2 {
                    editor.borrow_mut().set_document_language(&document.id, "de").unwrap();
                }
                false
            },
        ).unwrap();
        assert!(invocations.get() >= 2);
        assert_eq!(old_step.resume_at, Some(0));
        assert!(old_step.missing_packages.is_none());

        let checkpoint = worker.connection.query_row(
            "SELECT cursor,incomplete,resume_at,ocr_generation,route_identity
             FROM background_translation_progress WHERE document_id=?1 AND target='uk' AND revision_id=?2",
            params![document.id, revision],
            |row| Ok((row.get::<_, u32>(0)?, row.get::<_, bool>(1)?, row.get::<_, i64>(2)?,
                      row.get::<_, i64>(3)?, row.get::<_, Option<String>>(4)?)),
        ).unwrap();
        assert_eq!(checkpoint, (0, false, 0, 5, None));
        let next_step = worker.advance_background_translation(&document.id, revision, "uk").unwrap();
        assert!(next_step.resume_at.is_some_and(|due| due > 0));
        assert!(next_step.missing_packages.is_some());
        assert_eq!(worker.snapshot().unwrap().documents[0].language, "de");
    }
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
            let delay = if matches!(&result, Err(TranslationBatchError::Deferred { .. })) {
                INTERACTIVE_RETRY_SECONDS
            } else {
                RETRY_SECONDS
            };
            tx.execute(
                "UPDATE translation_windows SET lease=NULL,available_at=?3
                 WHERE document_id=?1 AND lease=?2",
                params![id, lease, crate::now() + delay],
            )
            .map_err(database_error)?;
        }
        tx.commit().map_err(database_error)?;
        if current {
            result.map(|window| complete.then_some(window)).map_err(String::from)
        } else {
            Ok(None)
        }
    }

}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка черги перекладу. {error}")
}

pub(crate) fn revise_pdf_checkpoint(
    db: &Connection, id: &str, previous: &str, revision: &str, page: u16,
) -> Result<()> {
    db.execute(
        "UPDATE background_translation_progress
         SET revision_id=?3,cursor=min(cursor,?4),resume_at=0,ocr_generation=ocr_generation+1
         WHERE document_id=?1 AND revision_id=?2",
        params![id, previous, revision, page.saturating_sub(1)],
    ).map_err(database_error)?;
    Ok(())
}

pub(crate) fn revisit_reviewed_translation(
    db: &Connection, id: &str, target: &str, cursor: usize,
) -> Result<()> {
    db.execute(
        "UPDATE background_translation_progress
         SET cursor=min(cursor,?3),resume_at=0,ocr_generation=ocr_generation+1
         WHERE document_id=?1 AND target=?2",
        params![id, target, i64::try_from(cursor).map_err(database_error)?],
    ).map_err(database_error)?;
    Ok(())
}

pub(crate) fn restart_after_source_language_change(db: &Connection, id: &str) -> Result<()> {
    db.execute(
        "UPDATE background_translation_progress
         SET cursor=0,incomplete=0,resume_at=0,route_identity=NULL,ocr_generation=ocr_generation+1
         WHERE document_id=?1",
        [id],
    )
    .map_err(database_error)?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationWarmStep {
    next: Option<usize>,
    incomplete: bool,
    #[serde(skip)]
    deferred: bool,
    #[serde(skip)]
    translated_page: Option<u16>,
    #[serde(skip)]
    translated_sentence_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTranslationProgress {
    // Zero means the next step is ready. No deadline means no work is scheduled.
    resume_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translated_page: Option<u16>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    translated_sentence_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_packages: Option<Vec<crate::translation::TranslationPair>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_pdf_text_page: Option<u16>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    waiting_for_pdf_ocr: bool,
}

impl Store {
    pub fn advance_background_translation(
        &mut self,
        id: &str,
        revision: &str,
        target: &str,
    ) -> Result<BackgroundTranslationProgress> {
        self.advance_background_translation_until_canceled(id, revision, target, &|| false)
    }

    pub(crate) fn advance_background_translation_until_canceled(
        &mut self, id: &str, revision: &str, target: &str, canceled: &dyn Fn() -> bool,
    ) -> Result<BackgroundTranslationProgress> {
        valid_language(target)?;
        let mut progress = BackgroundTranslationProgress {
            resume_at: None,
            translated_page: None,
            translated_sentence_ids: Vec::new(),
            missing_packages: None,
            pending_pdf_text_page: None,
            waiting_for_pdf_ocr: false,
        };
        let canceled_error = || "Фоновий переклад скасовано.".to_owned();
        if canceled() { return Err(canceled_error()); }
        if self.reader_preferences()?.background_translation != Some(true) {
            return Ok(progress);
        }
        let Some(_lock) = crate::file_originals::try_worker_lock(
            &self.file_original_directory,
            ".background-translation.lock",
        )?
        else {
            return Ok(BackgroundTranslationProgress {
                resume_at: Some(crate::now() + 2),
                ..progress
            });
        };
        let source: Option<(String, bool)> = self.connection.query_row(
            "SELECT json_extract(body,'$.language'),
                    json_extract(body,'$.format')='pdf' OR coalesce(json_array_length(body,'$.ocrLines'),0)>0
             FROM documents
             WHERE id=?1 AND json_extract(body,'$.currentRevisionId')=?2",
            params![id, revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(database_error)?;
        let Some((source, positioned)) = source.filter(|(source, _)| source != target) else {
            return Ok(progress);
        };
        let (mut route_identity, missing_packages) = self.background_translation_route(&source, target)?;
        if positioned && missing_packages.is_none() {
            route_identity = serde_json::to_string(&(PAGE_TRANSLATION_REVISION, route_identity))
                .map_err(database_error)?;
        }
        let now = crate::now();
        // Reserve before inference so concurrent OCR can invalidate the exact checkpoint.
        // A different route uses different cache keys, even for an unchanged document revision.
        self.connection.execute(
            "INSERT INTO background_translation_progress(document_id,target,revision_id,cursor,incomplete,resume_at,route_identity)
             SELECT id,?2,?3,0,0,0,?4 FROM documents
             WHERE id=?1 AND json_extract(body,'$.currentRevisionId')=?3
               AND json_extract(body,'$.language')=?5
             ON CONFLICT(document_id,target) DO UPDATE SET revision_id=excluded.revision_id,
               cursor=0,incomplete=0,resume_at=0,route_identity=excluded.route_identity
             WHERE background_translation_progress.revision_id<>excluded.revision_id
                OR background_translation_progress.route_identity IS NOT excluded.route_identity",
            params![id, target, revision, route_identity, source],
        ).map_err(database_error)?;
        let checkpoint = self.connection.query_row(
                "SELECT cursor,incomplete,resume_at,ocr_generation FROM background_translation_progress
             WHERE document_id=?1 AND revision_id=?2 AND target=?3",
                params![id, revision, target],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        let Some((cursor, incomplete, resume_at, generation)) = checkpoint else {
            return Ok(progress);
        };
        // A review can change without a text revision. Read the body after the
        // checkpoint so an older body cannot be committed under a newer generation.
        let document = self.translation_document(id)?;
        if document.current_revision_id.as_deref() != Some(revision) || document.language != source {
            return Ok(BackgroundTranslationProgress { resume_at: Some(0), ..progress });
        }
        if resume_at.is_none() {
            progress.waiting_for_pdf_ocr = document.format == "pdf"
                && self.pdf_has_unprocessed_pages(&document)?;
            return Ok(progress);
        }
        if resume_at.is_some_and(|deadline| deadline > now) {
            return Ok(BackgroundTranslationProgress { resume_at, missing_packages, ..progress });
        }
        let (cursor, incomplete, resume_at) = if missing_packages.is_some() {
            (0, false, Some(now + 30))
        } else {
            let step = self.warm_document_translation_step(&document, target, cursor as usize, canceled)?;
            progress.translated_page = step.translated_page;
            progress.translated_sentence_ids = step.translated_sentence_ids;
            if step.deferred {
                (cursor as usize, incomplete, Some(crate::now() + INTERACTIVE_RETRY_SECONDS))
            } else if let Some(next) = step.next {
                (next, incomplete || step.incomplete, Some(0))
            } else {
                if document.format == "pdf" {
                    progress.pending_pdf_text_page = self.next_background_pdf_text_page(id)?;
                }
                if progress.pending_pdf_text_page.is_some() {
                    (cursor as usize, incomplete, Some(crate::now() + PDF_OCR_POLL_SECONDS))
                } else if incomplete {
                    (0, false, Some(crate::now() + 300))
                } else {
                    progress.waiting_for_pdf_ocr = document.format == "pdf"
                        && self.pdf_has_unprocessed_pages(&document)?;
                    (cursor as usize, false, None)
                }
            }
        };
        // A superseded step may already have cached complete units. Keep its old
        // checkpoint so the next run can reuse those units and finish the rest.
        if canceled() { return Err(canceled_error()); }
        let saved = self.connection.execute(
            "UPDATE background_translation_progress SET cursor=?4,incomplete=?5,resume_at=?6
             WHERE document_id=?1 AND target=?2 AND revision_id=?3 AND ocr_generation=?7
               AND EXISTS(SELECT 1 FROM documents WHERE id=?1 AND json_extract(body,'$.currentRevisionId')=?3
                          AND json_extract(body,'$.language')=?8)",
            params![id, target, revision, i64::try_from(cursor).map_err(database_error)?, incomplete, resume_at, generation, source],
        ).map_err(database_error)?;
        if saved == 0 {
            progress.resume_at = Some(0);
            progress.translated_page = None;
            progress.translated_sentence_ids.clear();
            progress.pending_pdf_text_page = None;
            progress.waiting_for_pdf_ocr = false;
            return Ok(progress);
        }
        progress.resume_at = resume_at;
        progress.missing_packages = missing_packages;
        Ok(progress)
    }

    fn next_background_pdf_text_page(&self, id: &str) -> Result<Option<u16>> {
        self.connection.query_row(
            "SELECT min(j.value) FROM documents d, json_each(d.body,'$.pdfPendingTextPages') j
             WHERE d.id=?1 AND NOT EXISTS (
                 SELECT 1 FROM pdf_detached_pages p WHERE p.document_id=d.id AND p.page=j.value
             )",
            [id], |row| row.get(0),
        ).map_err(database_error)
    }

    fn pdf_has_unprocessed_pages(&self, document: &Document) -> Result<bool> {
        let page_count = document.pdf_page_starts.as_ref().map(Vec::len)
            .or(document.ocr_pdf_pages.map(usize::from)).unwrap_or(0);
        let mut recognized = vec![false; page_count];
        let mut query = self.connection.prepare_cached(
            "SELECT page FROM pdf_ocr_completed_pages WHERE document_id=?1
             UNION ALL SELECT page FROM pdf_detached_pages WHERE document_id=?1",
        ).map_err(database_error)?;
        let pages = query.query_map([&document.id], |row| row.get::<_, u16>(0))
            .map_err(database_error)?;
        for page in pages {
            if let Some(done) = recognized.get_mut(usize::from(page.map_err(database_error)?)) {
                *done = true;
            }
        }
        Ok(recognized.into_iter().enumerate().any(|(page, done)| {
            !done && u16::try_from(page).ok().is_none_or(|page| {
                crate::pdf_reading_position::page_bounds(document, page).is_none()
            })
        }))
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
            deferred: false,
            translated_page: None,
            translated_sentence_ids: Vec::new(),
        };
        if self.reader_preferences()?.background_translation != Some(true) {
            return Ok(done);
        }
        let document = self.translation_document(id)?;
        if document.current_revision_id.as_deref() != Some(revision) || document.language == target
        {
            return Ok(done);
        }
        self.warm_document_translation_step(&document, target, start, &|| false)
    }

    fn warm_document_translation_step(
        &mut self,
        document: &Document,
        target: &str,
        start: usize,
        canceled: &dyn Fn() -> bool,
    ) -> Result<TranslationWarmStep> {
        let canceled_error = || "Фоновий переклад скасовано.".to_owned();
        if canceled() { return Err(canceled_error()); }
        let done = TranslationWarmStep {
            next: None,
            incomplete: false,
            deferred: false,
            translated_page: None,
            translated_sentence_ids: Vec::new(),
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
            return self.warm_positioned_translation_page(document, target, page as u16, start, page + 1, canceled);
        }
        if start >= document.sentences.len() {
            // OCR photos also have a text-only view. Warm its sentences before the page layout.
            if start == document.sentences.len() && document.format != "pdf" &&
                document.ocr_lines.as_ref().is_some_and(|lines| !lines.is_empty())
            {
                return self.warm_positioned_translation_page(document, target, 0, start, start + 1, canceled);
            }
            return Ok(done);
        }
        let translated = match self.translate_document_paragraph_until_canceled(
            document,
            start,
            target,
            true,
            crate::translation::PageBoundaries::Canonical,
            canceled,
        ) {
            _ if canceled() => return Err(canceled_error()),
            Ok(translated) => translated,
            Err(TranslationBatchError::Deferred { .. }) => {
                let routes = self.translation_cache_routes(&document.language, target)?;
                let cache = routes.cached_window(self, document, start, target, crate::translation::PageBoundaries::Canonical)?;
                return Ok(TranslationWarmStep {
                    next: Some(start),
                    incomplete: true,
                    deferred: true,
                    translated_sentence_ids: translated_sentence_ids(cache.paragraph),
                    ..done
                });
            }
            Err(TranslationBatchError::Failed(_)) => return Ok(TranslationWarmStep {
                next: Some(start + 1),
                incomplete: true,
                ..done
            }),
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
        let incomplete = translated.units.iter().any(|unit| unit.translation.is_none());
        Ok(TranslationWarmStep {
            next: Some(next),
            incomplete,
            translated_sentence_ids: translated_sentence_ids(translated),
            ..done
        })
    }

    fn warm_positioned_translation_page(
        &mut self, document: &Document, target: &str, page: u16, start: usize, next: usize, canceled: &dyn Fn() -> bool,
    ) -> Result<TranslationWarmStep> {
        let id = &document.id;
        let rows = self.pdf_page_translation_result_interruptible(id, page, target, false, true, None, canceled);
        if canceled() { return Err("Фоновий переклад скасовано.".into()); }
        let layout_deferred = matches!(&rows, Err(TranslationBatchError::Deferred { .. }));
        let rows = if layout_deferred {
            self.pdf_page_translation_result(id, page, target, true, true)
        } else { rows };
        // Layout can split one reading sentence into independent visual blocks.
        let split = document.format == "pdf" && rows.as_ref().is_ok_and(|rows|
            rows.windows(2).any(|pair| pair[0].source_sentence_id == pair[1].source_sentence_id));
        let reflow = if split && !layout_deferred {
            self.warm_pdf_reflow_page(document, page, target, canceled)
        } else {
            Ok(Vec::new())
        };
        if canceled() { return Err("Фоновий переклад скасовано.".into()); }
        let deferred = layout_deferred || matches!(&reflow, Err(TranslationBatchError::Deferred { .. }));
        let translated_page = (
            rows.as_ref().is_ok_and(|rows| rows.iter().any(|row| row.text.is_some())) ||
            reflow.as_ref().is_ok_and(|units| units.iter().any(|unit| unit.translation.is_some()))
        ).then_some(page);
        Ok(TranslationWarmStep {
            next: Some(if deferred { start } else { next }),
            incomplete: rows.map_or(true, |rows| rows.iter().any(|row| row.text.is_none())) ||
                reflow.map_or(true, |units| units.iter().any(|unit| unit.translation.is_none())),
            deferred,
            translated_page,
            translated_sentence_ids: Vec::new(),
        })
    }
}

fn translated_sentence_ids(paragraph: ParagraphTranslation) -> Vec<String> {
    paragraph.units.into_iter()
        .filter(|unit| !unit.reviewed && unit.translation.is_some())
        .flat_map(|unit| unit.sentence_ids)
        .collect()
}
