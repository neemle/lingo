//! On-demand OCR for PDFs whose original file was already imported.

use crate::{document_layout, document_revisions, ocr, pdf_layers, Document, Result, Store};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PdfReadPageState {
    TextLayer,
    Recognized,
    Empty,
    NeedsOcr,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfReadPage {
    pub page: u16,
    pub state: PdfReadPageState,
    pub text: String,
    pub span: Option<ocr::OcrSpan>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfReadWindow {
    pub center: u16,
    pub page_count: usize,
    pub pages: Vec<PdfReadPage>,
    pub context: String,
    /// Canonical source covered by the normalized context.
    pub context_span: Option<ocr::OcrSpan>,
}

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
    completed_pages_between(db, id, 0, u16::MAX)
}

fn completed_pages_between(db: &Connection, id: &str, first: u16, last: u16) -> Result<Vec<u16>> {
    let mut query = db
        .prepare(
            "SELECT page FROM pdf_ocr_completed_pages
             WHERE document_id=?1 AND page BETWEEN ?2 AND ?3 ORDER BY page",
        )
        .map_err(database_error)?;
    let pages = query
        .query_map(params![id, first, last], |row| row.get(0))
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
    /// Provide enough text for the active page and both immediate neighbors.
    pub fn pdf_read_window(&self, id: &str, center: u16) -> Result<PdfReadWindow> {
        let (body, file_backed): (String, bool) = self
            .connection
            .query_row(
                "SELECT body, EXISTS(SELECT 1 FROM file_originals WHERE document_id=?1)
                 FROM documents WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(database_error)?;
        let document: Document = serde_json::from_str(&body).map_err(database_error)?;
        let starts = document
            .pdf_page_starts
            .as_deref()
            .filter(|_| document.format == "pdf")
            .ok_or("У документа немає розмітки сторінок PDF")?;
        let index = usize::from(center);
        if index >= starts.len() {
            return Err("Сторінку PDF не знайдено".into());
        }

        let completed = completed_pages_between(
            &self.connection, id, center.saturating_sub(1), center.saturating_add(1),
        )?;
        let first = index.saturating_sub(1);
        let last = (index + 1).min(starts.len() - 1);
        let pages = (first..=last)
            .map(|page| reading_page(&document, starts, &completed, page, file_backed))
            .collect::<Result<Vec<_>>>()?;

        let context_span = reading_context(&pages, index - first);
        let context = context_span.map_or_else(String::new, |span| {
            crate::translation::normalize_ocr_input(
                &document,
                &document.text[span.start_utf8..span.end_utf8],
                span.start_utf16,
            )
        });
        Ok(PdfReadWindow {
            center,
            page_count: starts.len(),
            pages,
            context,
            context_span,
        })
    }

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
        let completed = completed_pages_between(
            &self.connection, id, center.saturating_sub(1), center.saturating_add(1),
        )?;
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
            reflow_pdf_document(tx, document, pages)?;
            updated = Some(document.clone());
            Ok(true)
        })?;
        updated.ok_or_else(|| "Не вдалося оновити сторінку PDF".to_owned())
    }
}

/// Bring in previously unseen OCR pages for the same file-backed PDF.
pub(crate) fn merge_backup_cache(
    db: &Connection,
    incoming: &Document,
    incoming_pages: &[String],
    completed: &[u16],
) -> Result<()> {
    let matched: Option<(String, String)> = db
        .query_row(
            "SELECT documents.body, file_originals.pdf_page_text
             FROM documents JOIN file_originals ON file_originals.document_id=documents.id
             WHERE documents.source_hash=?1",
            [&incoming.source_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    let Some((body, page_text)) = matched else {
        return Ok(());
    };
    let mut local: Document = serde_json::from_str(&body).map_err(database_error)?;
    let pages: Vec<String> = serde_json::from_str(&page_text).map_err(database_error)?;
    if local.language != incoming.language || pages.as_slice() != incoming_pages {
        return Ok(());
    }

    for &page in completed {
        mark_completed(db, &local.id, page)?;
    }
    let added: Vec<_> = incoming
        .ocr_lines
        .iter()
        .flatten()
        .filter(|line| !has_ocr_lines(&local, line.page))
        .cloned()
        .collect();
    if added.is_empty() {
        return Ok(());
    }

    let lines = local.ocr_lines.get_or_insert_with(Vec::new);
    lines.extend(added);
    lines.sort_by_key(|line| line.page);
    reflow_pdf_document(db, &mut local, pages)?;
    let body = serde_json::to_string(&local).map_err(database_error)?;
    db.execute(
        "UPDATE documents SET body=?1 WHERE id=?2",
        params![body, local.id],
    )
    .map_err(database_error)?;
    Ok(())
}

fn reflow_pdf_document(db: &Connection, document: &mut Document, pages: Vec<String>) -> Result<()> {
    let lines = document.ocr_lines.as_deref().unwrap_or_default();
    let (text, starts) = pdf_layers::merge_page_text(pages, lines)?;
    let parsed = document_layout::from_transcript(text, Some(starts));
    let changed = parsed.text != document.text;
    document.speech_resume = Store::read_speech_resume(db, &document.id)?;
    ocr::update_transcript_with_page_starts(
        document,
        parsed.text,
        parsed.pdf_page_starts.as_deref(),
    )?;
    document.pdf_page_starts = parsed.pdf_page_starts;
    document.blocks = parsed.blocks;
    ocr::refresh_provenance(document);
    if changed {
        document_revisions::append(document, db)?;
    }
    if let Some(resume) = document.speech_resume.take() {
        Store::write_speech_resume(db, &document.id, &resume)?;
    }
    Store::discard_changed_speech_resume(db, document)
}

fn reading_page(
    document: &Document,
    starts: &[Option<usize>],
    completed: &[u16],
    index: usize,
    file_backed: bool,
) -> Result<PdfReadPage> {
    let page = u16::try_from(index).map_err(database_error)?;
    let mut result = PdfReadPage {
        page,
        state: PdfReadPageState::Unavailable,
        text: String::new(),
        span: None,
    };
    let Some(start_utf16) = starts[index] else {
        result.state = if completed.binary_search(&page).is_ok() {
            PdfReadPageState::Empty
        } else if file_backed {
            PdfReadPageState::NeedsOcr
        } else {
            PdfReadPageState::Unavailable
        };
        return Ok(result);
    };

    let invalid_span = "Межі сторінки PDF пошкоджені";
    let text = &document.text;
    let start_utf8 = ocr::byte_at_utf16(text, start_utf16).ok_or(invalid_span)?;
    let end_utf8 = match starts[index + 1..].iter().flatten().next() {
        Some(next) => ocr::byte_at_utf16(text, *next).ok_or(invalid_span)?,
        None => text.len(),
    };
    let content = text.get(start_utf8..end_utf8).ok_or(invalid_span)?.trim_end();
    result.state = if has_ocr_lines(document, page) {
        PdfReadPageState::Recognized
    } else {
        PdfReadPageState::TextLayer
    };
    result.text = content.to_owned();
    result.span = Some(ocr::OcrSpan {
        start_utf8,
        end_utf8: start_utf8 + content.len(),
        start_utf16,
        end_utf16: start_utf16 + content.encode_utf16().count(),
    });
    Ok(result)
}

fn reading_context(pages: &[PdfReadPage], center: usize) -> Option<ocr::OcrSpan> {
    let current = pages[center].span?;
    let start = center
        .checked_sub(1)
        .and_then(|index| pages[index].span)
        .unwrap_or(current);
    let end = pages
        .get(center + 1)
        .and_then(|page| page.span)
        .unwrap_or(current);
    Some(ocr::OcrSpan {
        start_utf8: start.start_utf8,
        end_utf8: end.end_utf8,
        start_utf16: start.start_utf16,
        end_utf16: end.end_utf16,
    })
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
