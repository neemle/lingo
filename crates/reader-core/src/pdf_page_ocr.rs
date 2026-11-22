//! On-demand OCR for PDFs whose original file was already imported.

use crate::{
    document_layout, document_revisions, file_originals, ocr, pdf_layers, Document, Result, Store,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::collections::BTreeSet;

const OCR_LEASE_SECONDS: i64 = 120;
const OCR_RETRY_DELAY_SECONDS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PdfReadPageState {
    TextLayer,
    Recognized,
    Empty,
    NeedsOcr,
    NeedsTextLayer,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfReadPage {
    pub page: u16,
    pub state: PdfReadPageState,
    pub text: String,
    pub span: Option<ocr::OcrSpan>,
    pub refresh_pending: bool,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfOcrRequest {
    pub page: u16,
    pub token: String,
    pub expires_at: i64,
    pub language: String,
    pub read_text_layer: bool,
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_ocr_completed_pages (
            document_id TEXT NOT NULL REFERENCES file_originals(document_id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535),
            PRIMARY KEY(document_id, page)
        );
        CREATE TABLE IF NOT EXISTS pdf_ocr_requests (
            document_id TEXT NOT NULL REFERENCES file_originals(document_id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535),
            priority INTEGER NOT NULL,
            token TEXT,
            available_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(document_id, page)
        );",
    )
    .map_err(database_error)
}

fn needed_pages(
    db: &Connection,
    document: &Document,
    pages: &[String],
    center: u16,
) -> Result<Vec<u16>> {
    if usize::from(center) >= pages.len() {
        return Err("Сторінку PDF не знайдено".into());
    }
    let completed = completed_pages_between(
        db, &document.id, center.saturating_sub(1), center.saturating_add(1),
    )?;
    let window = [Some(center), center.checked_sub(1), center.checked_add(1)];
    Ok(window
        .into_iter()
        .flatten()
        .filter(|&page| {
            pages
                .get(usize::from(page))
                .is_some_and(|text| text.trim().is_empty())
                && (should_refresh(document, page)
                    || (completed.binary_search(&page).is_err() && !has_ocr_lines(document, page)))
        })
        .collect())
}

fn should_refresh(document: &Document, page: u16) -> bool {
    if !document.pdf_ocr_refresh_pages.contains(&page) {
        return false;
    }
    if document.ocr_lines.iter().flatten().any(|line| line.page == page && line.correction.is_some()) {
        return false;
    }
    if document.bookmarks.is_empty() && document.notes.is_empty()
        && document.speech_text_overrides.is_empty() && document.reviewed_translations.is_empty()
    {
        return true;
    }
    let Some((start, end)) = crate::pdf_reading_position::page_bounds(document, page) else {
        return true;
    };
    !document.sentences.iter().any(|sentence| {
        sentence.start_utf16 < end && sentence.end_utf16 > start
            && (document.bookmarks.contains(&sentence.id)
                || document.notes.contains_key(&sentence.id)
                || document.speech_text_overrides.contains_key(&sentence.id)
                || document.reviewed_translations.values().any(|entries| entries.contains_key(&sentence.id)))
    })
}

pub(crate) fn refresh_after_language_change(db: &Connection, document: &mut Document) -> Result<()> {
    if document.format != "pdf" {
        return Ok(());
    }
    let file_backed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM file_originals WHERE document_id=?1)",
        [&document.id],
        |row| row.get(0),
    ).map_err(database_error)?;
    if !file_backed {
        return Ok(());
    }
    document.pdf_ocr_refresh_pages.extend(completed_pages(db, &document.id)?);
    document.pdf_ocr_refresh_pages.extend(document.ocr_lines.iter().flatten().map(|line| line.page));

    // Leases taken before the language change must not write old-language results.
    db.execute("DELETE FROM pdf_ocr_requests WHERE document_id=?1", [&document.id])
        .map_err(database_error)?;
    let focused: Option<u16> = db.query_row(
        "SELECT page FROM pdf_reading_positions WHERE document_id=?1", [&document.id], |row| row.get(0),
    ).optional().map_err(database_error)?;
    if let Some(page) = focused {
        schedule_window(db, document, page)?;
    }
    Ok(())
}

pub(crate) fn schedule_window(db: &Connection, document: &Document, center: u16) -> Result<()> {
    let Some(pages) = file_originals::native_pdf_page_text(db, &document.id)? else {
        return Ok(());
    };
    let pending = needed_pages(db, document, &pages, center)?;
    let mut retained = [-1; 3];
    for (slot, &page) in retained.iter_mut().zip(&pending) {
        *slot = i64::from(page);
    }
    db.execute(
        "DELETE FROM pdf_ocr_requests WHERE document_id=?1 AND page NOT IN (?2,?3,?4)",
        params![document.id, retained[0], retained[1], retained[2]],
    )
    .map_err(database_error)?;
    for (priority, page) in pending.into_iter().enumerate() {
        db.execute(
            "INSERT INTO pdf_ocr_requests (document_id,page,priority) VALUES (?1,?2,?3)
             ON CONFLICT(document_id,page) DO UPDATE SET priority=excluded.priority",
            params![document.id, page, priority as i64],
        )
        .map_err(database_error)?;
    }
    Ok(())
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
    remove_request(db, id, page)
}

fn remove_request(db: &Connection, id: &str, page: u16) -> Result<()> {
    db.execute(
        "DELETE FROM pdf_ocr_requests WHERE document_id=?1 AND page=?2",
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
        let body: String = self
            .connection
            .query_row("SELECT body FROM documents WHERE id=?1", [id], |row| row.get(0))
            .map_err(database_error)?;
        let document: Document = serde_json::from_str(&body).map_err(database_error)?;
        needed_pages(&self.connection, &document, &pages, center)
    }

    /// One worker per document can take the highest-priority available page.
    pub fn claim_pdf_ocr_page(&mut self, id: &str) -> Result<Option<PdfOcrRequest>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let now = crate::now();
        let page = tx
            .query_row(
                "SELECT page FROM pdf_ocr_requests
                 WHERE document_id=?1 AND available_at<=?2
                   AND NOT EXISTS (
                     SELECT 1 FROM pdf_ocr_requests
                     WHERE document_id=?1 AND token IS NOT NULL AND available_at>?2
                   )
                 ORDER BY priority LIMIT 1",
                params![id, now],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        let Some(page) = page else {
            return Ok(None);
        };
        let (language, read_text_layer) = tx.query_row(
            "SELECT json_extract(body, '$.language'),
                EXISTS(SELECT 1 FROM json_each(body, '$.pdfPendingTextPages') WHERE value=?2)
             FROM documents WHERE id=?1",
            params![id, page],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(database_error)?;
        let request = PdfOcrRequest {
            page,
            language,
            read_text_layer,
            token: uuid::Uuid::new_v4().to_string(),
            expires_at: now + OCR_LEASE_SECONDS,
        };
        tx.execute(
            "UPDATE pdf_ocr_requests SET token=?3, available_at=?4 WHERE document_id=?1 AND page=?2",
            params![id, page, request.token, request.expires_at],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(Some(request))
    }

    /// Release a failed page for retry while allowing available neighbors to proceed.
    pub fn retry_pdf_ocr_page(&self, id: &str, page: u16, token: &str) -> Result<bool> {
        self.release_pdf_ocr_page(id, page, token, OCR_RETRY_DELAY_SECONDS)
    }

    pub fn cancel_pdf_ocr_page(&self, id: &str, page: u16, token: &str) -> Result<bool> {
        self.release_pdf_ocr_page(id, page, token, 0)
    }

    fn release_pdf_ocr_page(&self, id: &str, page: u16, token: &str, retry_delay: i64) -> Result<bool> {
        let now = crate::now();
        self.connection
            .execute(
                "UPDATE pdf_ocr_requests SET token=NULL, available_at=?5
                 WHERE document_id=?1 AND page=?2 AND token=?3 AND available_at>?4",
                params![id, page, token, now, now + retry_delay],
            )
            .map(|count| count != 0)
            .map_err(database_error)
    }

    /// Store a page once, including an empty result. Existing corrections survive retries.
    pub fn cache_pdf_ocr_page(
        &mut self,
        id: &str,
        page: u16,
        lines: Vec<ocr::OcrLine>,
    ) -> Result<Document> {
        self.complete_pdf_ocr_page(id, page, lines, None)
    }

    pub fn complete_pdf_ocr_page(
        &mut self,
        id: &str,
        page: u16,
        lines: Vec<ocr::OcrLine>,
        token: Option<&str>,
    ) -> Result<Document> {
        self.complete_pdf_page(id, page, lines, token, None)
    }

    pub fn complete_pdf_page(
        &mut self, id: &str, page: u16, lines: Vec<ocr::OcrLine>, token: Option<&str>,
        text_layer: Option<String>,
    ) -> Result<Document> {
        if let Some(text) = &text_layer {
            if token.is_none() || !lines.is_empty() || text.trim().is_empty()
                || text.contains('\0') || text.len() > crate::MAX_IMPORT
            {
                return Err("Некоректний текстовий шар сторінки PDF.".into());
            }
        }
        if lines
            .iter()
            .any(|line| line.page != page || line.correction.is_some())
        {
            return Err("Результат OCR містить рядки іншої сторінки або виправлення".into());
        }

        let mut updated = None;
        self.edit_document(id, |document, tx| {
            if let Some(token) = token {
                let current: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM pdf_ocr_requests
                         WHERE document_id=?1 AND page=?2 AND token=?3 AND available_at>?4)",
                        params![id, page, token, crate::now()],
                        |row| row.get(0),
                    )
                    .map_err(database_error)?;
                if !current {
                    return Err("Запит OCR для цієї сторінки вже неактивний".into());
                }
            }
            let mut pages = file_originals::native_pdf_page_text(tx, id)?
                .ok_or("Оригінал PDF для розпізнавання недоступний")?;
            let needs_text = pages
                .get(usize::from(page))
                .ok_or("Сторінку PDF не знайдено")?
                .trim()
                .is_empty();
            if let Some(text) = text_layer {
                if !needs_text || !document.pdf_pending_text_pages.remove(&page) {
                    return Err("Сторінка PDF не очікує витягання тексту.".into());
                }
                pages[usize::from(page)] = text;
                let page_text = serde_json::to_string(&pages).map_err(database_error)?;
                tx.execute(
                    "UPDATE file_originals SET pdf_page_text=?2 WHERE document_id=?1",
                    params![id, page_text],
                ).map_err(database_error)?;
                remove_request(tx, id, page)?;
                update_focused_page(tx, document, page, pages)?;
                updated = Some(document.clone());
                return Ok(true);
            }
            let cached: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pdf_ocr_completed_pages WHERE document_id=?1 AND page=?2)",
                    params![id, page],
                    |row| row.get(0),
                )
                .map_err(database_error)?;
            let refresh = token.is_some() && should_refresh(document, page);
            if !needs_text || (!refresh && (cached || has_ocr_lines(document, page))) {
                remove_request(tx, id, page)?;
                updated = Some(document.clone());
                return Ok(false);
            }

            mark_completed(tx, id, page)?;
            let cleared_refresh = document.pdf_ocr_refresh_pages.remove(&page);
            let resolved_text_layer = document.pdf_pending_text_pages.remove(&page);
            // An empty rerun must not erase a previous OCR result.
            if lines.is_empty() {
                updated = Some(document.clone());
                return Ok(cleared_refresh || resolved_text_layer);
            }

            let existing = document.ocr_lines.get_or_insert_with(Vec::new);
            let insertion = existing.partition_point(|line| line.page < page);
            let end = if refresh {
                existing.partition_point(|line| line.page <= page)
            } else {
                insertion
            };
            existing.splice(insertion..end, lines);
            update_focused_page(tx, document, page, pages)?;
            updated = Some(document.clone());
            Ok(true)
        })?;
        updated.ok_or_else(|| "Не вдалося оновити сторінку PDF".to_owned())
    }
}

fn update_focused_page(db: &Connection, document: &mut Document, page: u16, pages: Vec<String>) -> Result<()> {
    reflow_pdf_document(db, document, pages)?;
    let focused: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_reading_positions WHERE document_id=?1 AND page=?2)",
        params![document.id, page],
        |row| row.get(0),
    ).map_err(database_error)?;
    if focused {
        crate::pdf_reading_position::focus_page_text(document, page);
    }
    Ok(())
}

/// Bring in previously unseen OCR pages for the same file-backed PDF.
pub(crate) fn merge_backup_cache(
    db: &Connection,
    incoming: &Document,
    incoming_pages: &[String],
    completed: &[u16],
) -> Result<()> {
    merge_cached_pages(
        db, &incoming.source_hash, &incoming.language,
        incoming.ocr_lines.as_deref().unwrap_or_default(), incoming_pages, completed,
        &incoming.pdf_ocr_refresh_pages,
    ).map(|_| ())
}

pub(crate) fn merge_cached_pages(
    db: &Connection,
    hash: &str,
    language: &str,
    lines: &[ocr::OcrLine],
    incoming_pages: &[String],
    completed: &[u16],
    stale: &BTreeSet<u16>,
) -> Result<bool> {
    let matched: Option<(String, String)> = db
        .query_row(
            "SELECT documents.body, file_originals.pdf_page_text
             FROM documents JOIN file_originals ON file_originals.document_id=documents.id
             WHERE documents.source_hash=?1",
            [hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    let Some((body, page_text)) = matched else {
        return Ok(false);
    };
    let mut local: Document = serde_json::from_str(&body).map_err(database_error)?;
    let pages: Vec<String> = serde_json::from_str(&page_text).map_err(database_error)?;
    if local.language != language || pages.as_slice() != incoming_pages {
        return Ok(false);
    }

    let local_completed = completed_pages(db, &local.id)?;
    let mut adopted_pages = BTreeSet::new();
    for &page in completed {
        if local_completed.binary_search(&page).is_err() && !has_ocr_lines(&local, page) {
            adopted_pages.insert(page);
        }
        mark_completed(db, &local.id, page)?;
    }
    let added: Vec<_> = lines
        .iter()
        .filter(|line| !has_ocr_lines(&local, line.page))
        .cloned()
        .collect();
    for page in added.iter().map(|line| line.page).collect::<BTreeSet<_>>() {
        remove_request(db, &local.id, page)?;
        adopted_pages.insert(page);
    }
    if adopted_pages.is_empty() {
        return Ok(true);
    }
    for page in adopted_pages {
        local.pdf_pending_text_pages.remove(&page);
        if stale.contains(&page) {
            local.pdf_ocr_refresh_pages.insert(page);
        } else {
            local.pdf_ocr_refresh_pages.remove(&page);
        }
    }
    if !added.is_empty() {
        let lines = local.ocr_lines.get_or_insert_with(Vec::new);
        lines.extend(added);
        lines.sort_by_key(|line| line.page);
        reflow_pdf_document(db, &mut local, pages)?;
    }
    let body = serde_json::to_string(&local).map_err(database_error)?;
    db.execute(
        "UPDATE documents SET body=?1 WHERE id=?2",
        params![body, local.id],
    )
    .map_err(database_error)?;
    Ok(true)
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
        refresh_pending: file_backed && should_refresh(document, page),
    };
    let Some(start_utf16) = starts[index] else {
        result.state = if document.pdf_pending_text_pages.contains(&page) {
            PdfReadPageState::NeedsTextLayer
        } else if completed.binary_search(&page).is_ok() {
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
