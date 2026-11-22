use crate::{ocr, Result, Sentence, Store};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use sha2::{Digest, Sha256};
use rusqlite::{params, Connection, OptionalExtension};

mod annotations;
mod find;
mod ocr_edit;
mod speech_resume;
mod translation;
mod word_break;
pub use translation::DetachedPdfTranslation;
pub use ocr_edit::DetachedPdfOcrPage;
pub use find::{DetachedPdfFindCursor, DetachedPdfFindPage};
pub use annotations::{DetachedPdfAnnotationChange, DetachedPdfAnnotationItem};
pub(crate) use annotations::has_reviewed_target;

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_detached_pages (
            document_id TEXT NOT NULL REFERENCES file_originals(document_id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535),
            ocr_language TEXT,
            text TEXT NOT NULL,
            lines TEXT,
            PRIMARY KEY(document_id, page)
        );
        CREATE TABLE IF NOT EXISTS pdf_detached_positions (
            document_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            sentence INTEGER NOT NULL CHECK(sentence >= 0),
            PRIMARY KEY(document_id,page),
            FOREIGN KEY(document_id,page) REFERENCES pdf_detached_pages(document_id,page) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS pdf_detached_speech_resume (
            document_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            body TEXT NOT NULL,
            PRIMARY KEY(document_id,page),
            FOREIGN KEY(document_id,page) REFERENCES pdf_detached_pages(document_id,page) ON DELETE CASCADE
        );",
    )
    .map_err(database_error)?;
    annotations::initialize(db)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfSentence {
    pub sentence: Sentence,
    pub position: usize,
    pub total: usize,
    pub language: String,
    pub bookmarked: bool,
    pub note: Option<String>,
    pub reviewed_translations: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_resume: Option<crate::SpeechResume>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_text: Option<String>,
}

pub(crate) struct CachedSentences {
    pub(crate) language: String,
    text: String,
    saved: usize,
    pub(crate) sentences: Vec<Sentence>,
    pub(crate) revision: String,
}

impl Store {
    pub fn detached_pdf_sentence(
        &self, id: &str, page: u16, requested: Option<usize>,
    ) -> Result<Option<DetachedPdfSentence>> {
        self.select_detached_pdf_sentence(id, page, requested, None)
    }

    pub fn select_detached_pdf_sentence(
        &self, id: &str, page: u16, requested: Option<usize>, expected: Option<&str>,
    ) -> Result<Option<DetachedPdfSentence>> {
        let Some(CachedSentences { language, saved, sentences, .. }) = self.detached_pdf_sentences(id, page)? else {
            return Ok(None);
        };
        let total = sentences.len();
        let position = requested.unwrap_or(saved).min(total - 1);
        let speech_text = annotations::speech_text(&self.connection, id, page, sentences[position].start_utf16)?;
        let checkpoint = speech_resume::read(&self.connection, id, page)?;
        let speech_resume = checkpoint.as_ref()
            .filter(|checkpoint| checkpoint.position == position && speech_resume::matches_original(checkpoint, &sentences))
            .and_then(|checkpoint| checkpoint.for_sentences(&sentences, &language, |_| speech_text.as_deref()));
        let sentence = sentences.into_iter().nth(position).ok_or("Речення PDF не знайдено.")?;
        if expected.is_some_and(|expected| sentence.id != expected) {
            return Err("Речення в результатах пошуку змінилося.".into());
        }
        if requested.is_some() {
            self.connection.execute(
                "INSERT INTO pdf_detached_positions (document_id,page,sentence)
                 SELECT document_id,page,?3 FROM pdf_detached_pages WHERE document_id=?1 AND page=?2
                 ON CONFLICT(document_id,page) DO UPDATE SET sentence=excluded.sentence",
                params![id, page, position],
            ).map_err(database_error)?;
        }
        let (bookmarked, note) = annotations::read(&self.connection, id, page, sentence.start_utf16)?;
        let reviewed_translations = annotations::reviewed(&self.connection, id, page, sentence.start_utf16)?;
        Ok(Some(DetachedPdfSentence { sentence, position, total, language, bookmarked, note, reviewed_translations, speech_resume, speech_text }))
    }
    pub(crate) fn detached_pdf_speech_source(
        &self, id: &str, page: u16, sentence_id: &str, next: bool,
    ) -> Result<Option<(String, crate::speech::SpeechSource)>> {
        let CachedSentences { language, saved, sentences, .. } = self.detached_pdf_sentences(id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let position = saved.min(sentences.len() - 1);
        if sentences[position].id != sentence_id {
            return Err("Позиція читання сторінки PDF змінилася.".into());
        }
        let Some(sentence) = sentences.into_iter().nth(position + usize::from(next)) else { return Ok(None); };
        let text = annotations::speech_text(&self.connection, id, page, sentence.start_utf16)?
            .unwrap_or(sentence.text);
        Ok(Some((sentence.id, crate::speech::SpeechSource { language, text })))
    }

    pub(crate) fn detached_pdf_sentences(&self, id: &str, page: u16) -> Result<Option<CachedSentences>> {
        let result: Option<(String, String, usize)> = self.connection.query_row(
            "SELECT p.text, json_extract(d.body, '$.language'), coalesce(c.sentence,0)
             FROM pdf_detached_pages p JOIN documents d ON d.id=p.document_id
             LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
             WHERE p.document_id=?1 AND p.page=?2",
            params![id, page],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(database_error)?;
        let Some((text, language, saved)) = result else { return Ok(None); };
        let revision = page_revision(&text);
        let sentences = crate::segment(&text, &sentence_prefix(id, page, &revision));
        Ok((!sentences.is_empty()).then_some(CachedSentences { language, text, saved, sentences, revision }))
    }
}

pub(crate) fn has_local_edits(db: &Connection, id: &str, page: u16) -> Result<bool> {
    if annotations::has_annotations(db, id, page)? {
        return Ok(true);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_detached_pages, json_each(lines)
         WHERE document_id=?1 AND page=?2 AND json_type(value, '$.correction')='text')",
        params![id, page], |row| row.get(0),
    ).map_err(database_error)
}

/// A present row with no language contains a native text layer.
pub(crate) fn source_language(db: &Connection, id: &str, page: u16) -> Result<Option<Option<String>>> {
    db.query_row(
        "SELECT ocr_language FROM pdf_detached_pages WHERE document_id=?1 AND page=?2",
        params![id, page],
        |row| row.get(0),
    )
    .optional()
    .map_err(database_error)
}

pub(crate) fn read(db: &Connection, id: &str, page: u16) -> Result<Option<(String, Option<String>)>> {
    db.query_row(
        "SELECT text,ocr_language FROM pdf_detached_pages WHERE document_id=?1 AND page=?2",
        params![id, page],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(database_error)
}

pub(crate) fn write_text_layer(db: &Connection, id: &str, page: u16, text: &str) -> Result<()> {
    write(db, id, page, text, None, None)
}

pub(crate) fn write_ocr(
    db: &Connection, id: &str, page: u16, language: &str, lines: &[ocr::OcrLine], page_count: u16,
) -> Result<()> {
    if lines.is_empty() {
        db.execute(
            "UPDATE pdf_detached_pages SET ocr_language=?3 WHERE document_id=?1 AND page=?2",
            params![id, page, language],
        )
        .map_err(database_error)?;
        return Ok(());
    }
    let text = ocr::recognized_lines(lines, page_count)?;
    let payload = serde_json::to_string(lines).map_err(database_error)?;
    write(db, id, page, &text, Some(language), Some(&payload))
}

fn write(
    db: &Connection, id: &str, page: u16, text: &str, language: Option<&str>, lines: Option<&str>,
) -> Result<()> {
    db.execute(
        "INSERT INTO pdf_detached_pages (document_id, page, ocr_language, text, lines)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(document_id,page) DO UPDATE SET
            ocr_language=excluded.ocr_language, text=excluded.text, lines=excluded.lines",
        params![id, page, language, text, lines],
    )
    .map_err(database_error)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ArchivedPage {
    pub page: u16,
    pub text: String,
    pub language: Option<String>,
    pub lines: Option<Vec<ocr::OcrLine>>,
    pub position: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<annotations::ArchivedAnnotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speech_resume: Option<speech_resume::ArchivedSpeechResume>,
}

impl ArchivedPage {
    pub(crate) fn validate(&self, document: &crate::Document, page_count: u16) -> Result<()> {
        if self.page >= page_count || self.text.trim().is_empty()
            || self.text.len() > crate::MAX_IMPORT || self.text.contains('\0')
            || document.pdf_page_starts.as_ref().and_then(|starts| starts.get(usize::from(self.page)))
                != Some(&None)
            || self.position.is_some_and(|position| !has_sentence(&self.text, position))
        {
            return Err("Некоректна окрема сторінка PDF у резервній копії.".into());
        }

        if !self.annotations.is_empty() {
            let sentences = crate::segment(&self.text, "");
            if self.annotations.windows(2).any(|pair| pair[0].start_utf16 >= pair[1].start_utf16) {
                return Err("Некоректний порядок приміток до сторінки PDF.".into());
            }
            for item in &self.annotations {
                if (!item.bookmarked && item.note.is_none() && item.translations.is_empty() && item.speech_text.is_none())
                    || sentences.binary_search_by_key(&item.start_utf16, |sentence| sentence.start_utf16).is_err()
                {
                    return Err("Примітка до сторінки PDF не має речення.".into());
                }
                if let Some(note) = &item.note {
                    annotations::validate_note(note)?;
                    if note.trim().is_empty() {
                        return Err("Порожня нотатка до сторінки PDF.".into());
                    }
                }
                if let Some(text) = &item.speech_text {
                    crate::validate_speech_text(text)?;
                    if text.trim().is_empty() {
                        return Err("Порожній текст озвучення PDF у резервній копії.".into());
                    }
                }
                for (target, text) in &item.translations {
                    crate::valid_language(target)?;
                    crate::validate_translation(text)?;
                    if target == &document.language || text.trim().is_empty() {
                        return Err("Некоректний переклад речення PDF у резервній копії.".into());
                    }
                }
            }
        }
        if let Some(checkpoint) = &self.speech_resume {
            let prefix = sentence_prefix(&document.id, self.page, &page_revision(&self.text));
            let sentences = crate::segment(&self.text, &prefix);
            if !speech_resume::matches_original(checkpoint, &sentences)
                || checkpoint.for_sentences(&sentences, &document.language,
                    |start| annotations::archived_speech_text(&self.annotations, start)).is_none()
            {
                return Err("Некоректна позиція озвучення PDF у резервній копії.".into());
            }
        }
        match (&self.language, &self.lines) {
            (None, None) => Ok(()),
            (Some(language), Some(lines)) if lines.iter().all(|line| line.page == self.page) =>
            {
                crate::valid_language(language)?;
                if ocr::recognized_lines(lines, page_count)? != self.text {
                    return Err("Текст сторінки PDF не відповідає збереженому OCR.".into());
                }
                Ok(())
            }
            _ => Err("Некоректне джерело тексту сторінки PDF.".into()),
        }
    }
}

fn has_sentence(text: &str, position: usize) -> bool {
    text.split_sentence_bounds().filter(|part| !part.trim().is_empty()).nth(position).is_some()
}

pub(crate) fn has_cached_pages(db: &Connection, id: &str) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_detached_pages WHERE document_id=?1)",
        [id],
        |row| row.get(0),
    )
    .map_err(database_error)
}

pub(crate) fn visit_cached_pages(
    db: &Connection, id: &str, mut visit: impl FnMut(ArchivedPage) -> Result<()>,
) -> Result<()> {
    let mut query = db.prepare(
        "SELECT p.page,p.text,p.ocr_language,p.lines,c.sentence,json_extract(d.body,'$.language')
         FROM pdf_detached_pages p
         JOIN documents d ON d.id=p.document_id
         LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
         WHERE p.document_id=?1 ORDER BY p.page",
    ).map_err(database_error)?;
    let mut rows = query.query([id]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let encoded: Option<String> = row.get(3).map_err(database_error)?;
        let text: String = row.get(1).map_err(database_error)?;
        let position: Option<usize> = row.get(4).map_err(database_error)?;
        let page = row.get(0).map_err(database_error)?;
        let language: String = row.get(5).map_err(database_error)?;
        let annotations = annotations::list(db, id, page)?;
        let checkpoint = speech_resume::read(db, id, page)?.filter(|checkpoint| {
            let prefix = sentence_prefix(id, page, &page_revision(&text));
            let sentences = crate::segment(&text, &prefix);
            speech_resume::matches_original(checkpoint, &sentences)
                && checkpoint.for_sentences(&sentences, &language,
                    |start| annotations::archived_speech_text(&annotations, start)).is_some()
        });
        visit(ArchivedPage {
            page,
            speech_resume: checkpoint,
            annotations,
            position: position.filter(|&position| has_sentence(&text, position)),
            text,
            language: row.get(2).map_err(database_error)?,
            lines: encoded.as_deref().map(serde_json::from_str).transpose().map_err(database_error)?,
        })?;
    }
    Ok(())
}

/// Keep a local page and cursor when present. A cursor only transfers to identical text.
pub(crate) fn adopt_cached_page(db: &Connection, document: &crate::Document, page: ArchivedPage) -> Result<()> {
    if document.pdf_page_starts.as_ref().and_then(|starts| starts.get(usize::from(page.page)))
        != Some(&None)
    {
        return Ok(());
    }
    if let Some((text, _)) = read(db, &document.id, page.page)? {
        if text != page.text {
            return Ok(());
        }
    } else {
        let payload = page.lines.as_ref().map(serde_json::to_string).transpose().map_err(database_error)?;
        write(db, &document.id, page.page, &page.text, page.language.as_deref(), payload.as_deref())?;
        db.execute(
            "DELETE FROM pdf_ocr_completed_pages WHERE document_id=?1 AND page=?2",
            params![document.id, page.page],
        ).map_err(database_error)?;
        db.execute(
            "DELETE FROM pdf_ocr_requests WHERE document_id=?1 AND page=?2",
            params![document.id, page.page],
        ).map_err(database_error)?;
    }
    annotations::merge(db, &document.id, page.page, &page.annotations)?;
    if let Some(checkpoint) = page.speech_resume {
        let prefix = sentence_prefix(&document.id, page.page, &page_revision(&page.text));
        let sentences = crate::segment(&page.text, &prefix);
        let annotations = annotations::list(db, &document.id, page.page)?;
        let speech_text = |start| annotations::archived_speech_text(&annotations, start);
        let local_valid = speech_resume::read(db, &document.id, page.page)?.is_some_and(|local| {
            speech_resume::matches_original(&local, &sentences)
                && local.for_sentences(&sentences, &document.language, speech_text).is_some()
        });
        if !local_valid {
            if let Some(checkpoint) = speech_resume::rebind(checkpoint, &sentences, &document.language, speech_text) {
                speech_resume::write(db, &document.id, page.page, &checkpoint)?;
            }
        }
    }
    if let Some(position) = page.position {
        db.execute(
            "INSERT OR IGNORE INTO pdf_detached_positions (document_id,page,sentence) VALUES (?1,?2,?3)",
            params![document.id, page.page, position],
        ).map_err(database_error)?;
    }
    Ok(())
}

fn page_revision(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn sentence_prefix(document_id: &str, page: u16, revision: &str) -> String {
    format!("{document_id}:{page}:{revision}")
}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка кешу сторінки PDF. {error}")
}
