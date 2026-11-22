//! Physical reading position for PDFs, independent of sentence and OCR state.

use crate::{pdf_page_ocr, Document, Result, Store};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, HashMap};

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_reading_positions (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535)
        );",
    )
    .map_err(database_error)
}

impl Store {
    pub fn pdf_reading_page(&self, id: &str) -> Result<Option<u16>> {
        self.connection
            .query_row(
                "SELECT page FROM pdf_reading_positions WHERE document_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)
    }

    pub fn set_pdf_reading_page(&mut self, id: &str, page: u16) -> Result<()> {
        self.edit_document(id, |document, tx| {
            set_page(tx, document, page, true)?;
            Ok(false)
        })
    }

    /// Enter the text of a physical page while retaining that page as the OCR focus.
    pub fn open_pdf_reading_page(&mut self, id: &str, revision: &str, page: u16) -> Result<Option<usize>> {
        self.focus_pdf_reading_page(id, Some(revision), page)
    }

    /// Opening a physical page does not require a text revision when the caller
    /// has no text selection to validate.
    pub fn focus_pdf_reading_page(
        &mut self, id: &str, revision: Option<&str>, page: u16,
    ) -> Result<Option<usize>> {
        let mut opened = None;
        self.edit_document(id, |document, tx| {
            if revision.is_some_and(|revision| document.current_revision_id.as_deref() != Some(revision)) {
                return Err("Текст PDF змінився. Відкрийте сторінку знову.".into());
            }
            set_page(tx, document, page, true)?;
            let (position, changed) = focus_page_text(document, page);
            opened = position;
            Ok(changed)
        })?;
        Ok(opened)
    }
}

pub(crate) fn focus_for_offset(db: &Connection, document: &Document, offset: usize) -> Result<()> {
    let page = if document.format != "pdf" {
        None
    } else if let Some(starts) = &document.pdf_page_starts {
        starts.iter().enumerate().rev()
            .find(|(_, start)| start.is_some_and(|start| start <= offset))
            .and_then(|(page, _)| u16::try_from(page).ok())
    } else {
        document.blocks.iter().find(|block| block.start_utf16 <= offset && offset < block.end_utf16)
            .and_then(|block| block.page.and_then(|page| u16::try_from(page).ok()))
            .or_else(|| document.ocr_lines.iter().flatten()
                .find(|line| line.source_span.is_some_and(|span| span.start_utf16 <= offset && offset < span.end_utf16))
                .map(|line| line.page))
    };
    if let Some(page) = page.filter(|&page| contains_page(document, page)) {
        set_page(db, document, page, false)?;
    }
    Ok(())
}

/// Move the sentence cursor into the page while retaining an overlapping selection.
pub(crate) fn focus_page_text(document: &mut Document, page: u16) -> (Option<usize>, bool) {
    let Some((start, end)) = page_bounds(document, page) else { return (None, false); };
    let overlaps = |sentence: &crate::Sentence| sentence.start_utf16 < end && sentence.end_utf16 > start;
    let position = if document.sentences.get(document.position).is_some_and(overlaps) {
        Some(document.position)
    } else {
        document.sentences.iter().position(overlaps)
    };
    let Some(position) = position else { return (None, false); };
    let retain_word = document.position == position && document.reading_token_utf16
        .is_some_and(|offset| {
            let absolute = document.sentences[position].start_utf16 + offset;
            start <= absolute && absolute < end
        });
    let changed = document.position != position || (!retain_word && document.reading_token_utf16.is_some());
    document.position = position;
    if !retain_word {
        document.reading_token_utf16 = None;
    }
    (Some(position), changed)
}

pub(crate) fn page_bounds(document: &Document, page: u16) -> Option<(usize, usize)> {
    if let Some(starts) = &document.pdf_page_starts {
        let index = usize::from(page);
        let start = *starts.get(index)?.as_ref()?;
        let end = starts[index + 1..].iter().flatten().copied().next()
            .unwrap_or_else(|| document.text.encode_utf16().count());
        return (start < end).then_some((start, end));
    }
    let mut blocks = document.blocks.iter().filter(|block| block.page == Some(usize::from(page)));
    if let Some(first) = blocks.next() {
        return Some((first.start_utf16, blocks.last().unwrap_or(first).end_utf16));
    }
    let mut spans = document.ocr_lines.iter().flatten()
        .filter(|line| line.page == page).filter_map(|line| line.source_span);
    let first = spans.next()?;
    Some((first.start_utf16, spans.last().unwrap_or(first).end_utf16))
}

fn set_page(db: &Connection, document: &Document, page: u16, force_schedule: bool) -> Result<()> {
    if !contains_page(document, page) {
        return Err("Сторінку PDF не знайдено".into());
    }
    let changed = db.execute(
        "INSERT INTO pdf_reading_positions (document_id,page) VALUES (?1,?2)
         ON CONFLICT(document_id) DO UPDATE SET page=excluded.page
         WHERE pdf_reading_positions.page<>excluded.page",
        params![document.id, page],
    ).map_err(database_error)?;
    if force_schedule || changed != 0 {
        pdf_page_ocr::schedule_window(db, document, page)?;
    }
    Ok(())
}

pub(crate) fn export(db: &Connection) -> Result<BTreeMap<String, u16>> {
    let mut query = db
        .prepare("SELECT document_id,page FROM pdf_reading_positions")
        .map_err(database_error)?;
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(database_error)?;
    rows.collect::<rusqlite::Result<_>>().map_err(database_error)
}

pub(crate) fn validate(
    positions: &BTreeMap<String, u16>,
    documents: &[Document],
) -> Result<()> {
    if positions.is_empty() {
        return Ok(());
    }
    let documents = index_documents(documents);
    if positions.iter().any(|(id, &page)| {
        !documents
            .get(id.as_str())
            .is_some_and(|document| contains_page(document, page))
    }) {
        return Err("Резервна копія містить недійсну позицію читання PDF".into());
    }
    Ok(())
}

pub(crate) fn restore(
    db: &Connection,
    positions: BTreeMap<String, u16>,
    documents: &[Document],
) -> Result<()> {
    if positions.is_empty() {
        return Ok(());
    }
    let documents = index_documents(documents);
    for (id, page) in positions {
        let archived = documents
            .get(id.as_str())
            .ok_or("У резервній копії немає документа для позиції читання PDF")?;
        let body: String = db
            .query_row(
                "SELECT body FROM documents WHERE source_hash=?1",
                [&archived.source_hash],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        let local: Document = serde_json::from_str(&body).map_err(database_error)?;
        if contains_page(&local, page) {
            db.execute(
                "INSERT INTO pdf_reading_positions (document_id,page) VALUES (?1,?2)
                 ON CONFLICT(document_id) DO NOTHING",
                params![local.id, page],
            )
            .map_err(database_error)?;
        }
    }
    Ok(())
}

fn index_documents(documents: &[Document]) -> HashMap<&str, &Document> {
    documents.iter().map(|document| (document.id.as_str(), document)).collect()
}

fn contains_page(document: &Document, page: u16) -> bool {
    document.format == "pdf"
        && usize::from(page)
            < document.pdf_page_starts.as_ref().map_or_else(
                || usize::from(document.ocr_pdf_pages.unwrap_or(0)),
                Vec::len,
            )
}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка позиції читання PDF. {error}")
}
