//! On-demand OCR for PDFs whose original file was already imported.

use crate::{document_layout, document_revisions, ocr, pdf_layers, Document, Result, Store};
use rusqlite::{params, Connection};

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_ocr_completed_pages (
            document_id TEXT NOT NULL REFERENCES file_originals(document_id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535),
            PRIMARY KEY(document_id, page)
        );",
    )
    .map_err(database_error)
}

pub(crate) fn completed_pages(db: &Connection, id: &str) -> Result<Vec<u16>> {
    let mut query = db
        .prepare("SELECT page FROM pdf_ocr_completed_pages WHERE document_id=?1 ORDER BY page")
        .map_err(database_error)?;
    let pages = query
        .query_map([id], |row| row.get(0))
        .map_err(database_error)?;
    pages.collect::<rusqlite::Result<_>>().map_err(database_error)
}

pub(crate) fn mark_completed(db: &Connection, id: &str, page: u16) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO pdf_ocr_completed_pages (document_id,page) VALUES (?1,?2)",
        params![id, page],
    )
    .map_err(database_error)?;
    Ok(())
}

impl Store {
    /// Return the unrecognized pages in this window, starting with the requested page.
    pub fn pdf_ocr_needed_pages(&self, id: &str, center: u16) -> Result<Vec<u16>> {
        let Some(pages) = self.native_pdf_page_text(id)? else {
            return Ok(Vec::new());
        };
        if usize::from(center) >= pages.len() {
            return Err("Сторінку PDF не знайдено".into());
        }
        let body: String = self
            .connection
            .query_row("SELECT body FROM documents WHERE id=?1", [id], |row| row.get(0))
            .map_err(database_error)?;
        let document: Document = serde_json::from_str(&body).map_err(database_error)?;
        let completed = completed_pages(&self.connection, id)?;
        let window = [Some(center), center.checked_sub(1), center.checked_add(1)];
        Ok(window
            .into_iter()
            .flatten()
            .filter(|&page| {
                pages
                    .get(usize::from(page))
                    .is_some_and(|text| text.trim().is_empty())
                    && completed.binary_search(&page).is_err()
                    && !has_ocr_lines(&document, page)
            })
            .collect())
    }

    /// Store a page once, including an empty result. Existing corrections survive retries.
    pub fn cache_pdf_ocr_page(
        &mut self,
        id: &str,
        page: u16,
        lines: Vec<ocr::OcrLine>,
    ) -> Result<Document> {
        let pages = self
            .native_pdf_page_text(id)?
            .ok_or("Оригінал PDF для розпізнавання недоступний")?;
        let needs_text = pages
            .get(usize::from(page))
            .ok_or("Сторінку PDF не знайдено")?
            .trim()
            .is_empty();
        if lines
            .iter()
            .any(|line| line.page != page || line.correction.is_some())
        {
            return Err("Результат OCR містить рядки іншої сторінки або виправлення".into());
        }

        let mut updated = None;
        self.edit_document(id, |document, tx| {
            let cached: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pdf_ocr_completed_pages WHERE document_id=?1 AND page=?2)",
                    params![id, page],
                    |row| row.get(0),
                )
                .map_err(database_error)?;
            if cached || has_ocr_lines(document, page) || !needs_text {
                updated = Some(document.clone());
                return Ok(false);
            }

            mark_completed(tx, id, page)?;
            if lines.is_empty() {
                updated = Some(document.clone());
                return Ok(false);
            }

            let existing = document.ocr_lines.get_or_insert_with(Vec::new);
            let insertion = existing.partition_point(|line| line.page < page);
            existing.splice(insertion..insertion, lines);
            let (text, starts) = pdf_layers::merge_page_text(pages, existing)?;
            let parsed = document_layout::from_transcript(text, Some(starts));
            let changed = parsed.text != document.text;
            document.speech_resume = Self::read_speech_resume(tx, id)?;
            ocr::update_transcript_with_page_starts(
                document,
                parsed.text,
                parsed.pdf_page_starts.as_deref(),
            )?;
            document.pdf_page_starts = parsed.pdf_page_starts;
            document.blocks = parsed.blocks;
            ocr::refresh_provenance(document);
            if changed {
                document_revisions::append(document, tx)?;
            }
            if let Some(resume) = document.speech_resume.take() {
                Self::write_speech_resume(tx, id, &resume)?;
            }
            Self::discard_changed_speech_resume(tx, document)?;
            updated = Some(document.clone());
            Ok(true)
        })?;
        updated.ok_or_else(|| "Не вдалося оновити сторінку PDF".to_owned())
    }
}

fn has_ocr_lines(document: &Document, page: u16) -> bool {
    document
        .ocr_lines
        .as_deref()
        .is_some_and(|lines| lines.binary_search_by_key(&page, |line| line.page).is_ok())
}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка кешу OCR PDF. {error}")
}
