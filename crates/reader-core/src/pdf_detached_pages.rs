use crate::{ocr, Result, Sentence, Store};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use sha2::{Digest, Sha256};
use rusqlite::{params, Connection, OptionalExtension};

mod annotations;
mod find;
pub use find::{DetachedPdfFindCursor, DetachedPdfFindPage};
pub use annotations::{DetachedPdfAnnotationChange, DetachedPdfAnnotationItem};
pub(crate) use annotations::has_annotations;

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
}

pub(crate) struct CachedSentences {
    pub(crate) language: String,
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
        Ok(Some(DetachedPdfSentence { sentence, position, total, language, bookmarked, note }))
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
        Ok(sentences.into_iter().nth(position + usize::from(next)).map(|sentence| {
            (sentence.id, crate::speech::SpeechSource { language, text: sentence.text })
        }))
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
        Ok((!sentences.is_empty()).then_some(CachedSentences { language, saved, sentences, revision }))
    }
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
                if (!item.bookmarked && item.note.is_none())
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
            }
        }
        match (&self.language, &self.lines) {
            (None, None) => Ok(()),
            (Some(language), Some(lines))
                if lines.iter().all(|line| line.page == self.page && line.correction.is_none()) =>
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
        "SELECT p.page,p.text,p.ocr_language,p.lines,c.sentence
         FROM pdf_detached_pages p
         LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
         WHERE p.document_id=?1 ORDER BY p.page",
    ).map_err(database_error)?;
    let mut rows = query.query([id]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let encoded: Option<String> = row.get(3).map_err(database_error)?;
        let text: String = row.get(1).map_err(database_error)?;
        let position: Option<usize> = row.get(4).map_err(database_error)?;
        let page = row.get(0).map_err(database_error)?;
        visit(ArchivedPage {
            page,
            annotations: annotations::list(db, id, page)?,
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
