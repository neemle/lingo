//! On-demand OCR for PDFs whose original file was already imported.

use crate::{
    document_layout, document_revisions, file_originals, ocr, pdf_detached_pages, pdf_layers,
    Document, Result, Store,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::{collections::BTreeSet, fs::File};

const OCR_RETRY_DELAY_SECONDS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PdfReadPageState {
    TextLayer,
    Recognized,
    Empty,
    NeedsOcr,
    NeedsTextLayer,
    Detached,
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
    pub has_ocr: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<document_layout::DocumentBlock>,
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PdfTextOrderEdit {
    start_utf16: usize,
    end_utf16: usize,
    replacement: String,
}

fn repair_text_order(old: &str, changes: &[PdfTextOrderEdit]) -> Result<(String, Vec<ocr::TranscriptEdit>)> {
    let invalid = || "Некоректні виправлення порядку слів PDF.".to_owned();
    let mut cursor = 0;
    let mut offset = 0;
    let mut text = String::with_capacity(old.len());
    let mut edits = Vec::new();
    for change in changes {
        let skip = change.start_utf16.checked_sub(offset).ok_or_else(invalid)?;
        let length = change.end_utf16.checked_sub(change.start_utf16).ok_or_else(invalid)?;
        let start = cursor + ocr::byte_at_utf16(&old[cursor..], skip).ok_or_else(invalid)?;
        let end = start + ocr::byte_at_utf16(&old[start..], length).ok_or_else(invalid)?;
        let original = old.get(start..end).ok_or_else(invalid)?;
        let valid = if change.replacement.is_empty() {
            let left = old[..start].rsplit(char::is_whitespace).next().unwrap_or("");
            crate::pdf_text_blocks::ligature_boundary(left, original, &old[end..])
        } else {
            valid_pdf_order_edit(original, &change.replacement)
        };
        if !valid {
            return Err(invalid());
        }
        text.push_str(&old[cursor..start]);
        text.push_str(&change.replacement);
        edits.push(ocr::TranscriptEdit {
            start_utf16: change.start_utf16,
            old_len: length,
            new_len: change.replacement.encode_utf16().count(),
        });
        cursor = end;
        offset = change.end_utf16;
    }
    text.push_str(&old[cursor..]);
    Ok((text, edits))
}

fn valid_pdf_order_edit(original: &str, replacement: &str) -> bool {
    if (1..=3).contains(&original.len()) && original.bytes().all(|byte| byte.is_ascii_digit())
        && replacement.chars().map(|symbol| crate::document_layout::note_digit(symbol))
            .eq(original.bytes().map(|byte| Some((u32::from(byte - b'0'), true))))
    {
        return true;
    }
    if let Some((moved, restored)) = original.strip_suffix('à').zip(replacement.strip_prefix(" à ")) {
        return !restored.is_empty() && moved.trim() == restored
            && moved.starts_with(char::is_whitespace) && moved.ends_with(char::is_whitespace);
    }
    if replacement == "\n\n" {
        return !original.is_empty() && original.lines().count() <= 2 &&
            original.matches('\n').count() <= 1 && original.chars().all(char::is_whitespace);
    }
    if original.len() > 8192 || !(2..=49).contains(&original.lines().count()) ||
        replacement.trim() != replacement {
        return false;
    }
    let mut columns = replacement.split("\n\n");
    let height = columns.next().map_or(0, |text| text.lines().count());
    let middle = columns.next();
    let last = columns.next();
    let columns_valid = (2..=24).contains(&height)
        && match (middle, last, columns.next()) {
            (Some(right), None, None) => right.lines().count() == height,
            (Some(label), Some(right), None) => label.split_whitespace().count() == 1 && right.lines().count() == height,
            _ => false,
        };
    if !columns_valid { return false; }
    let mut before: Vec<_> = original.split_whitespace().collect();
    let mut after: Vec<_> = replacement.split_whitespace().collect();
    before.sort_unstable();
    after.sort_unstable();
    before == after
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfOcrRequest {
    pub page: u16,
    pub token: String,
    /// Zero indicates a claim tied to the live library handle.
    pub expires_at: i64,
    pub language: String,
    pub read_text_layer: bool,
}

pub(crate) struct ActiveClaim {
    document: String,
    page: u16,
    token: String,
    _worker_lock: File,
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
    let mut needed = Vec::with_capacity(3);
    for page in window.into_iter().flatten() {
        let Some(text) = pages.get(usize::from(page)) else { continue; };
        if !text.trim().is_empty() {
            if (completed.binary_search(&page).is_err() || should_refresh(document, page))
                && can_recover_text_layer(document, page, text) {
                needed.push(page);
            }
            continue;
        }
        let pending = match pdf_detached_pages::source_language(db, &document.id, page)? {
            Some(language) => language.as_deref().is_some_and(|value| value != document.language.as_str())
                && !pdf_detached_pages::has_local_edits(db, &document.id, page)?,
            None => should_refresh(document, page)
                || (completed.binary_search(&page).is_err() && !has_ocr_lines(document, page)),
        };
        if pending { needed.push(page); }
    }
    Ok(needed)
}

fn should_refresh(document: &Document, page: u16) -> bool {
    if !document.pdf_ocr_refresh_pages.contains(&page) {
        return false;
    }
    if document.ocr_lines.iter().flatten().any(|line| line.page == page && line.correction.is_some()) {
        return false;
    }
    !page_has_sentence_annotations(document, page)
}

fn can_recover_text_layer(document: &Document, page: u16, text: &str) -> bool {
    !has_ocr_lines(document, page)
        && pdf_layers::damaged_text_layer(text)
        && !page_has_sentence_annotations(document, page)
}

fn page_has_sentence_annotations(document: &Document, page: u16) -> bool {
    if document.bookmarks.is_empty() && document.notes.is_empty()
        && document.speech_text_overrides.is_empty() && document.reviewed_translations.is_empty()
    {
        return false;
    }
    let Some((start, end)) = crate::pdf_reading_position::page_bounds(document, page) else {
        return false;
    };
    document.sentences.iter().any(|sentence| {
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

fn release_request(db: &Connection, id: &str, page: u16, token: &str, delay: i64) -> Result<bool> {
    db.execute(
        "UPDATE pdf_ocr_requests SET token=NULL, available_at=?4
         WHERE document_id=?1 AND page=?2 AND token=?3",
        params![id, page, token, crate::now() + delay],
    )
    .map(|count| count != 0)
    .map_err(database_error)
}

impl Store {
    /// Resolve a deferred text-layer probe without rendering, running OCR or moving the reading position.
    pub fn cache_background_pdf_text_layer(
        &mut self, id: &str, revision: &str, page: u16, text: &str,
    ) -> Result<bool> {
        if text.len() > crate::MAX_IMPORT || text.contains('\0') {
            return Err("Некоректний текстовий шар сторінки PDF.".into());
        }
        if self.reader_preferences()?.background_translation != Some(true) {
            return Ok(false);
        }
        let Some(_lock) = file_originals::try_worker_lock(
            &self.file_original_directory, ".pdf-ocr-window.lock",
        )? else {
            return Ok(false);
        };
        let text = text.trim();
        let mut completed = false;
        self.edit_document(id, |document, tx| {
            if document.current_revision_id.as_deref() != Some(revision)
                || !document.pdf_pending_text_pages.remove(&page)
                || pdf_detached_pages::source_language(tx, id, page)?.is_some()
            {
                return Ok(false);
            }
            if !text.is_empty() && !pdf_layers::damaged_text_layer(text) {
                pdf_detached_pages::write_text_layer(tx, id, page, text)?;
                remove_request(tx, id, page)?;
            }
            // An unusable embedded layer leaves the page eligible for foreground OCR.
            tx.execute(
                "UPDATE background_translation_progress SET resume_at=0 WHERE document_id=?1",
                [id],
            ).map_err(database_error)?;
            completed = true;
            Ok(true)
        })?;
        Ok(completed)
    }

    pub fn unread_pdf_text_pages(&self, id: &str) -> Result<Vec<u16>> {
        let mut query = self.connection.prepare(
            "SELECT j.value FROM documents d, json_each(d.body,'$.pdfPendingTextPages') j
             WHERE d.id=?1 AND NOT EXISTS (
                 SELECT 1 FROM pdf_detached_pages p WHERE p.document_id=d.id AND p.page=j.value
             ) ORDER BY j.value",
        ).map_err(database_error)?;
        let rows = query.query_map([id], |row| row.get(0)).map_err(database_error)?;
        rows.collect::<rusqlite::Result<_>>().map_err(database_error)
    }

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
        let mut pages = Vec::with_capacity(last + 1 - first);
        for page in first..=last {
            let mut result = reading_page(&document, starts, &completed, page, file_backed)?;
            if file_backed && result.span.is_none() {
                if let Some((text, language)) = pdf_detached_pages::read(&self.connection, id, result.page)? {
                    let ocr = self.detached_pdf_ocr(id, result.page)?;
                    result.has_ocr = ocr.is_some();
                    let detached = document_layout::detached_pdf_document(text,
                        language.as_deref().unwrap_or(&document.language), ocr.map(|cached| cached.lines));
                    result.text = detached.text;
                    result.blocks = detached.blocks;
                    result.state = PdfReadPageState::Detached;
                    result.refresh_pending = language.is_some_and(|language| language != document.language.as_str())
                        && !pdf_detached_pages::has_local_edits(&self.connection, id, result.page)?;
                }
            }
            pages.push(result);
        }

        let context_span = reading_context(&pages, index - first);
        let context = context_span.map_or_else(|| {
            let current = &pages[index - first];
            if current.state == PdfReadPageState::Detached { current.text.clone() } else { String::new() }
        }, |span| {
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
        self.claim_pdf_ocr_page_for_window(id, None)
    }

    /// An asynchronous viewer may claim pages only while its physical window is active.
    pub fn claim_pdf_ocr_page_for_window(
        &mut self, id: &str, center: Option<u16>,
    ) -> Result<Option<PdfOcrRequest>> {
        self.claim_pdf_ocr_page_with_retry(id, center, false)
    }

    pub fn claim_pdf_ocr_page_with_retry(
        &mut self, id: &str, center: Option<u16>, retry_now: bool,
    ) -> Result<Option<PdfOcrRequest>> {
        if self.active_pdf_ocr.is_some() {
            return Ok(None);
        }
        let Some(worker_lock) = file_originals::try_worker_lock(
            &self.file_original_directory, ".pdf-ocr-window.lock",
        )? else {
            return Ok(None);
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let now = crate::now();
        if let Some(center) = center.filter(|_| retry_now) {
            tx.execute(
                "UPDATE pdf_ocr_requests SET available_at=0
                 WHERE document_id=?1 AND page BETWEEN ?2 - 1 AND ?2 + 1
                   AND token IS NULL AND available_at>?3
                   AND EXISTS (SELECT 1 FROM pdf_reading_positions
                               WHERE document_id=?1 AND page=?2)",
                params![id, center, now],
            ).map_err(database_error)?;
        }
        let page = tx
            .query_row(
                "SELECT page FROM pdf_ocr_requests
                 WHERE document_id=?1 AND (token IS NOT NULL OR available_at<=?2)
                   AND (?3 IS NULL OR (
                     page BETWEEN ?3 - 1 AND ?3 + 1
                     AND EXISTS (SELECT 1 FROM pdf_reading_positions
                                 WHERE document_id=?1 AND page=?3)
                   ))
                 ORDER BY priority LIMIT 1",
                params![id, now, center],
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
            expires_at: 0,
        };
        tx.execute(
            "UPDATE pdf_ocr_requests SET token=?3, available_at=0 WHERE document_id=?1 AND page=?2",
            params![id, page, request.token],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        self.active_pdf_ocr = Some(ActiveClaim {
            document: id.to_owned(), page, token: request.token.clone(), _worker_lock: worker_lock,
        });
        Ok(Some(request))
    }

    fn owns_pdf_ocr_claim(&self, id: &str, page: u16, token: &str) -> bool {
        self.active_pdf_ocr.as_ref().is_some_and(|claim|
            claim.document == id && claim.page == page && claim.token == token)
    }

    /// Release a failed page for retry while allowing available neighbors to proceed.
    pub fn retry_pdf_ocr_page(&mut self, id: &str, page: u16, token: &str) -> Result<bool> {
        self.release_pdf_ocr_page(id, page, token, OCR_RETRY_DELAY_SECONDS)
    }

    pub fn cancel_pdf_ocr_page(&mut self, id: &str, page: u16, token: &str) -> Result<bool> {
        self.release_pdf_ocr_page(id, page, token, 0)
    }

    fn release_pdf_ocr_page(&mut self, id: &str, page: u16, token: &str, retry_delay: i64) -> Result<bool> {
        if !self.owns_pdf_ocr_claim(id, page, token) {
            return Ok(false);
        }
        let result = release_request(&self.connection, id, page, token, retry_delay);
        self.active_pdf_ocr = None;
        result
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
        self.commit_pdf_page(id, page, lines, token, text_layer, None)
    }

    pub fn replace_pdf_text_layer(
        &mut self, id: &str, page: u16, revision: &str, language: &str, lines: Vec<ocr::OcrLine>,
    ) -> Result<Document> {
        if lines.is_empty() {
            return Err("Розпізнавання не знайшло тексту на сторінці.".into());
        }
        self.commit_pdf_page(id, page, lines, None, None, Some((revision, language)))
    }

    pub fn supplement_pdf_ocr_page(
        &mut self, id: &str, page: u16, revision: &str, language: &str,
        detached: bool, lines: Vec<ocr::OcrLine>,
    ) -> Result<bool> {
        if lines.is_empty() { return Ok(false); }
        if lines.iter().any(|line| line.page != page || line.correction.is_some()) {
            return Err("Результат OCR містить рядки іншої сторінки або виправлення.".into());
        }
        ocr::recognized_lines(&lines, page.checked_add(1).ok_or("Некоректна сторінка PDF.")?)?;
        if detached {
            return self.supplement_detached_pdf_ocr(id, page, revision, language, lines);
        }
        let mut updated = false;
        self.edit_document(id, |document, tx| {
            if document.current_revision_id.as_deref() != Some(revision) || document.language != language {
                return Err("Текст або мова PDF змінилися. Відкрийте сторінку знову.".into());
            }
            let pages = file_originals::native_pdf_page_text(tx, id)?.ok_or("Оригінал PDF недоступний.")?;
            let (start, _) = crate::pdf_reading_position::page_bounds(document, page)
                .ok_or("Розпізнаний текст сторінки недоступний.")?;
            let existing = document.ocr_lines.as_mut().ok_or("Розпізнаний текст сторінки недоступний.")?;
            let first = existing.partition_point(|line| line.page < page);
            let end = existing.partition_point(|line| line.page <= page);
            if first == end { return Err("Розпізнаний текст сторінки недоступний.".into()); }
            let Some((merged, mut edits)) = crate::ocr_layout::supplement_page(&existing[first..end], lines)?
                else { return Ok(false); };
            for edit in &mut edits { edit.start_utf16 += start; }
            existing.splice(first..end, merged);
            reflow_pdf_document(tx, document, pages, &edits, page)?;
            updated = true;
            Ok(true)
        })?;
        Ok(updated)
    }

    pub fn repair_stored_pdf_text_order(
        &mut self, id: &str, page: u16, revision: &str, reference: &str, changes: &[PdfTextOrderEdit],
    ) -> Result<bool> {
        if changes.is_empty() { return Ok(false); }
        if reference.len() > crate::MAX_IMPORT || reference.contains('\0') {
            return Err("Некоректний текстовий шар сторінки PDF.".into());
        }
        let (corrected, mut edits) = repair_text_order(reference, changes)?;
        let mut changed = false;
        self.edit_document(id, |document, tx| {
            if document.current_revision_id.as_deref() != Some(revision)
                || has_ocr_lines(document, page) {
                return Ok(false);
            }
            let Some(mut pages) = file_originals::native_pdf_page_text(tx, id)? else { return Ok(false); };
            let Some(old) = pages.get_mut(usize::from(page)) else { return Ok(false); };
            if old.trim().is_empty() {
                changed = pdf_detached_pages::repair_cached_text_order(tx, id, page, reference, &corrected, &edits)?;
                return Ok(false);
            }
            if old.trim() != reference { return Ok(false); }
            let Some((start, _)) = crate::pdf_reading_position::page_bounds(document, page) else { return Ok(false); };
            for edit in &mut edits { edit.start_utf16 += start; }
            *old = corrected;
            let page_text = serde_json::to_string(&pages).map_err(database_error)?;
            reflow_pdf_document(tx, document, pages, &edits, page)?;
            tx.execute("UPDATE file_originals SET pdf_page_text=?2 WHERE document_id=?1",
                params![id, page_text]).map_err(database_error)?;
            changed = true;
            Ok(true)
        })?;
        Ok(changed)
    }

    pub fn upgrade_stored_ocr_layout(&mut self, id: &str, page: u16, revision: &str, reference: &str, detached: bool) -> Result<bool> {
        if reference.len() > 96_000 || reference.is_empty() { return Ok(false); }
        if detached {
            return self.upgrade_detached_ocr_layout(id, page, revision, reference);
        }
        let mut updated = false;
        self.edit_document(id, |document, tx| {
            if document.current_revision_id.as_deref() != Some(revision) || !has_ocr_lines(document, page) {
                return Ok(false);
            }
            let Some(pages) = file_originals::native_pdf_page_text(tx, id)? else { return Ok(false); };
            let Some((start, _)) = crate::pdf_reading_position::page_bounds(document, page) else { return Ok(false); };
            let lines = document.ocr_lines.as_ref().expect("verified OCR lines");
            let first = lines.partition_point(|line| line.page < page);
            let end = lines.partition_point(|line| line.page <= page);
            let Some((rebuilt, _, mut edits)) = crate::ocr_layout::upgrade_cached_page(&lines[first..end], reference)
                else { return Ok(false); };
            for edit in &mut edits { edit.start_utf16 += start; }
            let resume = Store::read_speech_resume(tx, id)?;
            let protected = document.sentences.iter().any(|sentence| {
                crate::ocr_layout::intersects_edits(sentence.start_utf16, sentence.end_utf16, &edits)
                    && (document.bookmarks.contains(&sentence.id)
                    || document.notes.contains_key(&sentence.id)
                    || document.speech_text_overrides.contains_key(&sentence.id)
                    || document.reviewed_translations.values().any(|values| values.contains_key(&sentence.id))
                    || resume.as_ref().is_some_and(|resume| resume.sentence_id == sentence.id))
            });
            if protected || crate::dictionary_transfer::selection_overlaps_layout_edits(tx, id, None, revision, &edits)? {
                return Ok(false);
            }
            document.ocr_lines.as_mut().expect("verified OCR lines").splice(first..end, rebuilt);
            reflow_pdf_document(tx, document, pages, &edits, page)?;
            updated = true;
            Ok(true)
        })?;
        Ok(updated)
    }

    pub fn reconcile_pdf_ocr_page(
        &mut self, id: &str, page: u16, revision: &str, language: &str, reference: &str, detached: bool,
    ) -> Result<bool> {
        if reference.len() > crate::MAX_IMPORT || reference.contains('\0') {
            return Err("Некоректний текстовий шар сторінки PDF.".into());
        }
        let corpus = if reference.trim().is_empty() {
            Some(self.pdf_ocr_corpus(language)?.ok_or(
                "У цього скану немає текстового шару. Встановіть пакет аналізу мови, щоб звірити OCR.",
            )?)
        } else {
            None
        };
        let reconcile = |lines: &mut [ocr::OcrLine]| match &corpus {
            Some(db) => Self::repair_pdf_ocr_with_corpus(db, language, lines),
            None => Ok(reconcile_ocr_with_layer(reference, lines)),
        };
        if detached {
            return self.reconcile_detached_pdf_ocr(id, page, revision, language, reconcile);
        }
        let mut changed = false;
        self.edit_document(id, |document, tx| {
            if document.current_revision_id.as_deref() != Some(revision) || document.language != language {
                return Err("Текст або мова PDF змінилися. Відкрийте сторінку знову.".into());
            }
            let pages = file_originals::native_pdf_page_text(tx, id)?
                .ok_or("Оригінал PDF недоступний.")?;
            if usize::from(page) >= pages.len() || !has_ocr_lines(document, page) {
                return Err("Розпізнаний текст сторінки недоступний.".into());
            }
            let lines = document.ocr_lines.get_or_insert_with(Vec::new);
            let start = lines.partition_point(|line| line.page < page);
            let end = lines.partition_point(|line| line.page <= page);
            let page_lines = &mut lines[start..end];
            let previous: Vec<_> = page_lines.iter().map(|line| line.reading_text().to_owned()).collect();
            changed = reconcile(page_lines)?;
            if changed {
                let edits = ocr::line_transcript_edits(page_lines, &previous)?;
                reflow_pdf_document(tx, document, pages, &edits, page)?;
            }
            Ok(changed)
        })?;
        Ok(changed)
    }

    fn commit_pdf_page(
        &mut self, id: &str, page: u16, mut lines: Vec<ocr::OcrLine>, token: Option<&str>,
        text_layer: Option<String>, replaces_revision: Option<(&str, &str)>,
    ) -> Result<Document> {
        if token.is_some_and(|token| !self.owns_pdf_ocr_claim(id, page, token)) {
            return Err("Запит OCR для цієї сторінки вже неактивний".into());
        }
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

        let corrected_language = if !lines.is_empty() {
            let language: String = self.connection.query_row(
                "SELECT json_extract(body,'$.language') FROM documents WHERE id=?1", [id], |row| row.get(0),
            ).map_err(database_error)?;
            ocr::recognized_lines(&lines, page.checked_add(1).ok_or("Некоректна сторінка PDF.")?)?;
            crate::ocr_layout::reconstruct_blocks(&mut lines);
            if self.repair_pdf_ocr(&language, &mut lines).is_err() {
                lines.iter_mut().for_each(|line| line.correction = None);
            }
            Some(language)
        } else {
            None
        };

        let mut updated = None;
        self.edit_document(id, |document, tx| {
            if corrected_language.as_deref().is_some_and(|language| language != document.language) {
                lines.iter_mut().for_each(|line| line.correction = None);
            }
            if let Some(token) = token {
                let current: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM pdf_ocr_requests
                         WHERE document_id=?1 AND page=?2 AND token=?3)",
                        params![id, page, token],
                        |row| row.get(0),
                    )
                    .map_err(database_error)?;
                if !current {
                    return Err("Запит OCR для цієї сторінки вже неактивний".into());
                }
            }
            let mut pages = file_originals::native_pdf_page_text(tx, id)?
                .ok_or("Оригінал PDF для розпізнавання недоступний")?;
            let old_layer = pages
                .get(usize::from(page))
                .ok_or("Сторінку PDF не знайдено")?;
            let needs_text = old_layer.trim().is_empty();
            if replaces_revision.is_some_and(|(revision, language)| {
                document.current_revision_id.as_deref() != Some(revision)
                    || document.language != language
                    || needs_text || has_ocr_lines(document, page)
            }) {
                return Err("Текст або мова PDF змінилися. Відкрийте сторінку й повторіть OCR.".into());
            }
            let automatic_repair = token.is_some() && !needs_text &&
                can_recover_text_layer(document, page, old_layer);
            if replaces_revision.is_some() || automatic_repair {
                if automatic_repair {
                    let old_letters = old_layer.chars().filter(|ch| ch.is_alphabetic()).count();
                    let new_letters = lines.iter().flat_map(|line| line.reading_text().chars())
                        .filter(|ch| ch.is_alphabetic()).count();
                    if lines.is_empty() || new_letters * 3 < old_letters * 2 {
                        mark_completed(tx, id, page)?;
                        let cleared_refresh = document.pdf_ocr_refresh_pages.remove(&page);
                        updated = Some(document.clone());
                        return Ok(cleared_refresh);
                    }
                }
                reconcile_ocr_with_layer(old_layer, &mut lines);
                repair_ocr_drop_caps(old_layer, &mut lines);
                remove_spurious_ocr_note_punctuation(old_layer, &mut lines);
                pages[usize::from(page)].clear();
                let page_text = serde_json::to_string(&pages).map_err(database_error)?;
                tx.execute("UPDATE file_originals SET pdf_page_text=?2 WHERE document_id=?1",
                    params![id, page_text]).map_err(database_error)?;
                let existing = document.ocr_lines.get_or_insert_with(Vec::new);
                let insertion = existing.partition_point(|line| line.page < page);
                existing.splice(insertion..insertion, lines);
                mark_completed(tx, id, page)?;
                document.pdf_pending_text_pages.remove(&page);
                document.pdf_ocr_refresh_pages.remove(&page);
                update_focused_page(tx, document, page, pages)?;
                updated = Some(document.clone());
                return Ok(true);
            }
            let remaining = crate::MAX_IMPORT.saturating_sub(document.text.len());
            if let Some(previous_language) = pdf_detached_pages::source_language(tx, id, page)? {
                if let Some(token) = token.filter(|_| previous_language.as_deref()
                    .is_some_and(|previous| previous != document.language.as_str()))
                {
                    if !pdf_detached_pages::has_local_edits(tx, id, page)? {
                        if let Some(text) = text_layer {
                            pdf_detached_pages::write_text_layer(tx, id, page, text.trim())?;
                        } else if lines.is_empty() {
                            release_request(tx, id, page, token, OCR_RETRY_DELAY_SECONDS)?;
                            updated = Some(document.clone());
                            return Ok(false);
                        } else {
                            let page_count = u16::try_from(pages.len()).map_err(database_error)?;
                            pdf_detached_pages::write_ocr(tx, id, page, &document.language, &lines, page_count)?;
                        }
                    }
                }
                remove_request(tx, id, page)?;
                updated = Some(document.clone());
                return Ok(false);
            }
            if let Some(text) = text_layer {
                if !needs_text || !document.pdf_pending_text_pages.contains(&page) {
                    return Err("Сторінка PDF не очікує витягання тексту.".into());
                }
                if pdf_layers::damaged_text_layer(&text) {
                    // The next claim renders this page instead of reading its broken text layer again.
                    document.pdf_pending_text_pages.remove(&page);
                    tx.execute(
                        "UPDATE pdf_ocr_requests SET token=NULL, available_at=0 WHERE document_id=?1 AND page=?2",
                        params![id, page],
                    ).map_err(database_error)?;
                    updated = Some(document.clone());
                    return Ok(true);
                }
                if text.trim().len() + 2 > remaining {
                    pdf_detached_pages::write_text_layer(tx, id, page, text.trim())?;
                    remove_request(tx, id, page)?;
                    updated = Some(document.clone());
                    return Ok(false);
                }
                document.pdf_pending_text_pages.remove(&page);
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

            if !lines.is_empty() && !has_ocr_lines(document, page) {
                let bytes = 1 + lines.iter().map(ocr_page_line_bytes).sum::<usize>();
                if bytes > remaining {
                    let page_count = u16::try_from(pages.len()).map_err(database_error)?;
                    pdf_detached_pages::write_ocr(tx, id, page, &document.language, &lines, page_count)?;
                    remove_request(tx, id, page)?;
                    updated = Some(document.clone());
                    return Ok(false);
                }
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
        if token.is_some() {
            self.active_pdf_ocr = None;
        }
        updated.ok_or_else(|| "Не вдалося оновити сторінку PDF".to_owned())
    }
}

fn normalized_ocr_anchor(word: &str) -> String {
    use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
    word.nfd()
        .filter(|&ch| !is_combining_mark(ch))
        .flat_map(char::to_lowercase)
        .map(|ch| if ch == '’' { '\'' } else { ch })
        .collect()
}

fn remove_spurious_ocr_note_punctuation(reference: &str, lines: &mut [ocr::OcrLine]) {
    use crate::document_layout::{footnote_start, pdf_note_references};
    use crate::document_tokens::word_parts;

    const ANCHORS: usize = 4;
    let numbers: Vec<u32> = reference
        .lines()
        .map(str::trim)
        .filter(|line| footnote_start(line))
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect();
    let references = pdf_note_references(reference, &numbers);
    if references.is_empty() {
        return;
    }

    let mut source = String::with_capacity(reference.len());
    let mut notes = std::collections::BTreeMap::new();
    let mut offset = 0;
    for marker in references {
        source.push_str(&reference[offset..marker.start]);
        let punctuation = reference[marker.end..]
            .trim_start_matches([' ', '\u{00a0}', '\u{202f}'])
            .chars()
            .next()
            .filter(|ch| matches!(*ch, '?' | '!' | '*' | '\'' | '’'));
        notes.insert(source.len(), punctuation);
        offset = marker.end;
    }
    source.push_str(&reference[offset..]);
    let source_words: Vec<_> = word_parts(&source)
        .map(|(start, _, text)| {
            (
                normalized_ocr_anchor(text),
                notes.get(&(start + text.len())),
            )
        })
        .take(4097)
        .collect();
    if !(ANCHORS + 1..=4096).contains(&source_words.len()) {
        return;
    }

    for line in lines {
        let text = line.reading_text();
        let words: Vec<_> = word_parts(text)
            .map(|(start, _, word)| (start + word.len(), normalized_ocr_anchor(word)))
            .collect();
        let mut corrected = text.to_owned();
        for (index, &(end, _)) in words.iter().enumerate().rev() {
            let Some(mark @ ('?' | '!' | '*' | '\'' | '’')) = text[end..].chars().next() else {
                continue;
            };
            if text[end + mark.len_utf8()..].starts_with(char::is_alphanumeric) {
                continue;
            }
            let after = (words.len() - index - 1).min(ANCHORS - index.min(2));
            let before = ANCHORS - after;
            if before > index {
                continue;
            }
            let keys = words[index - before..=index + after]
                .iter()
                .map(|(_, key)| key);
            let mut matches = source_words
                .windows(ANCHORS + 1)
                .filter(|sample| sample.iter().map(|(key, _)| key).eq(keys.clone()))
                .map(|sample| sample[before].1);
            let removable = |suffix: Option<&Option<char>>| {
                suffix.is_some_and(|punctuation| *punctuation != Some(mark))
            };
            if matches.next().is_some_and(removable) && matches.all(removable) {
                corrected.replace_range(end..end + mark.len_utf8(), "");
            }
        }
        if corrected != text {
            line.correction = Some(corrected);
        }
    }
}
// OCR sometimes reads a two-line initial as part of both text lines.
fn repair_ocr_drop_caps(reference: &str, lines: &mut [ocr::OcrLine]) {
    use crate::document_tokens::word_parts;

    const ANCHORS: usize = 4;
    let reference: Vec<_> = word_parts(reference)
        .map(|(_, _, word)| (word, normalized_ocr_anchor(word)))
        .take(4097)
        .collect();
    if !(ANCHORS + 1..=4096).contains(&reference.len()) {
        return;
    }

    let replacement = |words: &[(usize, usize, &str)]| {
        let anchors: Vec<_> = words
            .iter()
            .take(ANCHORS)
            .map(|&(_, _, word)| normalized_ocr_anchor(word))
            .collect();
        if anchors.len() != ANCHORS {
            return None;
        }
        let mut matches = reference
            .windows(ANCHORS + 1)
            .filter(|window| window[1..].iter().map(|(_, key)| key).eq(&anchors))
            .map(|window| window[0].0);
        let candidate = matches.next()?;
        matches.all(|word| word == candidate).then_some(candidate)
    };

    for index in 0..lines.len().saturating_sub(1) {
        let (first, rest) = lines.split_at_mut(index + 1);
        let (initial, continuation) = (&mut first[index], &mut rest[0]);
        if initial.page != continuation.page
            || initial.height < continuation.height * 1.6
            || initial.height > continuation.height * 4.0
            || continuation.x < initial.x + continuation.height * 0.5
            || continuation.y < initial.y + continuation.height * 0.4
            || continuation.y > initial.y + initial.height
        {
            continue;
        }
        let opening = initial.reading_text();
        let words: Vec<_> = word_parts(opening).take(ANCHORS + 1).collect();
        if let Some(&(start, _, word)) = words.first() {
            let prefix = opening[..start].trim();
            let corrected = if matches!(prefix, "©" | "®" | "@" | "§") {
                replacement(&words)
                    .filter(|candidate| {
                        candidate.chars().count() <= 3
                            && candidate.starts_with(char::is_uppercase)
                            && candidate.chars().all(char::is_alphabetic)
                    })
                    .map(|candidate| format!("{candidate} {}", &opening[start..]))
            } else if prefix.is_empty()
                && word.chars().count() <= 2
                && word.chars().all(char::is_lowercase)
            {
                replacement(&words[1..])
                    .filter(|candidate| {
                        candidate.starts_with(char::is_uppercase)
                            && candidate.chars().skip(1).eq(word.chars())
                    })
                    .map(|candidate| format!("{candidate}{}", &opening[start + word.len()..]))
            } else {
                None
            };
            if let Some(corrected) = corrected {
                initial.correction = Some(corrected);
            }
        }

        let text = continuation.reading_text();
        let words: Vec<_> = word_parts(text).take(ANCHORS + 1).collect();
        if let Some(&(0, _, word)) = words.first() {
            if let Some(candidate) = replacement(&words[1..]).filter(|candidate| {
                candidate.chars().count() >= 3
                    && candidate.chars().all(char::is_lowercase)
                    && word.chars().skip(1).eq(candidate.chars())
            }) {
                continuation.correction = Some(format!("{candidate}{}", &text[word.len()..]));
            }
        }
    }
}
pub(crate) fn reconcile_ocr_with_layer(reference: &str, lines: &mut [ocr::OcrLine]) -> bool {
    use crate::document_tokens::word_parts;
    const CONTEXT: usize = 4;
    const MAX_LAYER_WORDS: usize = 4096;
    let layer: Vec<_> = word_parts(reference).map(|(_, _, word)| word)
        .take(MAX_LAYER_WORDS + 1).collect();
    if !(CONTEXT + 1..=MAX_LAYER_WORDS).contains(&layer.len()) {
        return false;
    }

    let mut changed = false;
    let mut remaining = MAX_LAYER_WORDS;
    for line in lines {
        let text = line.reading_text();
        let words: Vec<_> = word_parts(text).take(remaining + 1).collect();
        if words.len() > remaining { break; }
        remaining -= words.len();
        let unchanged = line.unchanged_word_starts();
        let mut corrected = text.to_owned();
        for (index, &(start, _, word)) in words.iter().enumerate().rev() {
            let after = (words.len() - index - 1).min(CONTEXT - index.min(2));
            let before = CONTEXT - after;
            if index < before || unchanged.as_ref().is_some_and(|starts| starts.binary_search(&start).is_err()) ||
                word.chars().count() < 4 ||
                !word.chars().all(char::is_alphabetic)
            {
                continue;
            }
            let neighbors = &words[index - before..=index + after];
            let mut matches = layer.windows(CONTEXT + 1).filter(|sample| {
                neighbors.iter().zip(*sample).enumerate().all(|(position, ((_, _, recognized), original))| {
                    position == before || recognized == original
                })
            }).map(|sample| sample[before]);
            let Some(candidate) = matches.next() else { continue; };
            if candidate != word && candidate.chars().all(char::is_alphabetic)
                && candidate.starts_with(char::is_uppercase) == word.starts_with(char::is_uppercase)
                && matches.all(|other| other == candidate) && ocr::differs_by_one_glyph(word, candidate)
            {
                corrected.replace_range(start..start + word.len(), candidate);
            }
        }
        if corrected != text {
            line.correction = Some(corrected);
            changed = true;
        }
    }
    changed
}

fn update_focused_page(db: &Connection, document: &mut Document, page: u16, pages: Vec<String>) -> Result<()> {
    reflow_pdf_document(db, document, pages, &[], page)?;
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

/// Cached page results from the same PDF original.
pub(crate) struct PdfPageCache<'a> {
    pub pages: &'a [String],
    pub lines: &'a [ocr::OcrLine],
    pub completed: &'a [u16],
    pub stale: &'a BTreeSet<u16>,
    /// Only native ZIP backups can carry deferred text-layer identities.
    pub pending: Option<&'a BTreeSet<u16>>,
}

pub(crate) fn merge_backup_cache(
    db: &Connection,
    incoming: &Document,
    incoming_pages: &[String],
    completed: &[u16],
) -> Result<()> {
    merge_cached_pages(db, &incoming.source_hash, &incoming.language, PdfPageCache {
        pages: incoming_pages,
        lines: incoming.ocr_lines.as_deref().unwrap_or_default(),
        completed,
        stale: &incoming.pdf_ocr_refresh_pages,
        pending: Some(&incoming.pdf_pending_text_pages),
    }).map(|_| ())
}

pub(crate) fn merge_cached_pages(
    db: &Connection,
    hash: &str,
    language: &str,
    cache: PdfPageCache<'_>,
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
    let mut pages: Vec<String> = serde_json::from_str(&page_text).map_err(database_error)?;
    if local.language != language || pages.len() != cache.pages.len() {
        return Ok(false);
    }

    let mut available = crate::MAX_IMPORT.saturating_sub(local.text.len());
    let mut adopted_pages = BTreeSet::new();
    let mut obsolete_requests = BTreeSet::new();
    let mut layers_changed = false;
    for (index, (local_text, incoming_text)) in pages.iter_mut().zip(cache.pages).enumerate() {
        let page = u16::try_from(index).map_err(database_error)?;
        if cache.pending.is_some_and(|pending| pending.contains(&page)) {
            continue;
        }
        if !local.pdf_pending_text_pages.contains(&page) {
            // Legacy checkpoint transfers must retain the exact layer mapping.
            if cache.pending.is_none() && local_text.as_str() != incoming_text.as_str() {
                return Ok(false);
            }
            continue;
        }

        if pdf_detached_pages::source_language(db, &local.id, page)?.is_some() {
            continue;
        }
        if !incoming_text.is_empty() {
            let bytes = incoming_text.trim().len() + 2;
            if bytes > available {
                if cache.pending.is_none() {
                    return Ok(false);
                }
                continue;
            }
            available -= bytes;
            local_text.clone_from(incoming_text);
            layers_changed = true;
            obsolete_requests.insert(page);
        }
        adopted_pages.insert(page);
    }

    let mut added_lines = Vec::new();
    let mut unadopted_results = BTreeSet::new();
    for lines in cache.lines.chunk_by(|a, b| a.page == b.page) {
        let page = lines[0].page;
        if !pages.get(usize::from(page)).is_some_and(|text| text.trim().is_empty())
            || has_ocr_lines(&local, page)
            || pdf_detached_pages::source_language(db, &local.id, page)?.is_some()
        {
            continue;
        }
        let bytes = 1 + lines.iter().map(ocr_page_line_bytes).sum::<usize>();
        if bytes > available {
            if cache.pending.is_none() {
                return Ok(false);
            }
            unadopted_results.insert(page);
            continue;
        }
        available -= bytes;
        obsolete_requests.insert(page);
        adopted_pages.insert(page);
        added_lines.extend_from_slice(lines);
    }

    for page in obsolete_requests {
        remove_request(db, &local.id, page)?;
    }
    let local_completed = completed_pages(db, &local.id)?;
    for &page in cache.completed {
        if unadopted_results.contains(&page)
            || pdf_detached_pages::source_language(db, &local.id, page)?.is_some()
            || !pages.get(usize::from(page)).is_some_and(|text| text.trim().is_empty())
        {
            continue;
        }
        if local_completed.binary_search(&page).is_err() && !has_ocr_lines(&local, page) {
            adopted_pages.insert(page);
        }
        mark_completed(db, &local.id, page)?;
    }
    if adopted_pages.is_empty() {
        return Ok(true);
    }
    let first_changed = *adopted_pages.first().expect("checked adopted pages");
    for page in adopted_pages {
        local.pdf_pending_text_pages.remove(&page);
        if cache.stale.contains(&page) {
            local.pdf_ocr_refresh_pages.insert(page);
        } else {
            local.pdf_ocr_refresh_pages.remove(&page);
        }
    }
    if layers_changed {
        let page_text = serde_json::to_string(&pages).map_err(database_error)?;
        db.execute(
            "UPDATE file_originals SET pdf_page_text=?2 WHERE document_id=?1",
            params![local.id, page_text],
        ).map_err(database_error)?;
    }
    let has_added_ocr = !added_lines.is_empty();
    if has_added_ocr {
        let lines = local.ocr_lines.get_or_insert_with(Vec::new);
        lines.extend(added_lines);
        lines.sort_by_key(|line| line.page);
    }
    if layers_changed || has_added_ocr {
        reflow_pdf_document(db, &mut local, pages, &[], first_changed)?;
    }
    let body = serde_json::to_string(&local).map_err(database_error)?;
    db.execute(
        "UPDATE documents SET body=?1 WHERE id=?2",
        params![body, local.id],
    )
    .map_err(database_error)?;
    Ok(true)
}

fn reflow_pdf_document(
    db: &Connection, document: &mut Document, pages: Vec<String>, edits: &[ocr::TranscriptEdit], first_changed: u16,
) -> Result<()> {
    let previous_revision = document.current_revision_id.clone();
    let lines = document.ocr_lines.as_deref().unwrap_or_default();
    let (text, starts) = pdf_layers::merge_page_text(pages, lines)?;
    let parsed = document_layout::from_transcript(text, Some(starts));
    let changed = parsed.text != document.text;
    document.speech_resume = Store::read_speech_resume(db, &document.id)?;
    let before = ocr::update_transcript_with_edits(
        document,
        parsed.text,
        parsed.pdf_page_starts.as_deref(),
        edits,
    )?;
    let old_pages = std::mem::replace(&mut document.pdf_page_starts, parsed.pdf_page_starts);
    document.blocks = parsed.blocks;
    ocr::refresh_provenance(document);
    if changed {
        document_revisions::append(document, db)?;
        crate::dictionary_transfer::reanchor_canonical_selections(
            db, previous_revision.as_deref(), document, &before, old_pages.as_deref(), edits,
        )?;
    }
    if let (Some(previous), Some(revision)) = (previous_revision.as_deref(), document.current_revision_id.as_deref()) {
        crate::translation_queue::revise_pdf_checkpoint(db, &document.id, previous, revision, first_changed)?;
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
        blocks: Vec::new(),
        refresh_pending: file_backed && should_refresh(document, page),
        has_ocr: false,
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
    result.has_ocr = has_ocr_lines(document, page);
    result.state = if result.has_ocr {
        PdfReadPageState::Recognized
    } else {
        PdfReadPageState::TextLayer
    };
    result.refresh_pending |= file_backed && completed.binary_search(&page).is_err()
        && can_recover_text_layer(document, page, content);
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

fn ocr_page_line_bytes(line: &ocr::OcrLine) -> usize {
    line.reading_text().len() + 1 + usize::from(line.block_start)
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

#[cfg(test)]
mod drop_cap_tests {
    use super::*;

    #[test]
    fn canonical_ocr_rebases_background_progress_without_restarting_the_book() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.db")).unwrap();
        let original = root.path().join("library.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut pages = vec![String::from("Le bateau arrive."); 12];
        pages[8].clear();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "library.pdf", &original, "fr", pages, vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let previous = document.current_revision_id.as_deref().unwrap();
        let page = 11;
        let old_start = document.pdf_page_starts.as_ref().unwrap()[page].unwrap();
        let card = store.save_selected_word(crate::Vocabulary {
            id: String::new(), word: "bateau".into(), document_id: document.id.clone(),
            language: "fr".into(), target_language: "uk".into(), translation: "човен".into(),
            context: String::new(), forward: Default::default(), reverse: Default::default(),
            analysis: None, expression: None, sense: None, accepted_translations: Vec::new(),
        }, crate::dictionary_transfer::DictionarySelection {
            revision_id: previous.to_owned(), start_utf16: old_start + 3, reconstructed_text: None,
        }).unwrap();
        for (target, cursor) in [("uk", 12), ("en", 3)] {
            store.connection.execute(
                "INSERT INTO background_translation_progress
                 (document_id,target,revision_id,cursor,incomplete,resume_at,ocr_generation,route_identity)
                 VALUES (?1,?2,?3,?4,1,NULL,5,'installed route')",
                params![document.id, target, previous, cursor],
            ).unwrap();
        }
        let revised = store.cache_pdf_ocr_page(&document.id, 8, vec![ocr::OcrLine {
            rotation: None,
            block_start: false,
            page: 8, text: "Le marin part.".into(), x: 0.1, y: 0.1, width: 0.5, height: 0.05,
            confidence: None, correction: None, words: Vec::new(), source_span: None,
        }]).unwrap();
        let revision = revised.current_revision_id.as_deref().unwrap();
        assert_ne!(previous, revision);
        let new_start = revised.pdf_page_starts.as_ref().unwrap()[page].unwrap();
        assert!(new_start > old_start);
        let links = serde_json::to_value(store.dictionary_source_locations(&card.id).unwrap()).unwrap();
        assert_eq!(links.as_array().unwrap().len(), 1);
        assert_eq!(links[0]["selection"]["startUtf16"], new_start + 3);
        assert_eq!(links[0]["revisionId"], revision);
        for (target, expected) in [("uk", 7), ("en", 3)] {
            let saved: (String, u16, bool, i64, u32, String) = store.connection.query_row(
                "SELECT revision_id,cursor,incomplete,resume_at,ocr_generation,route_identity
                 FROM background_translation_progress WHERE document_id=?1 AND target=?2",
                params![document.id, target],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            ).unwrap();
            assert_eq!(saved, (revision.to_owned(), expected, true, 0, 6, "installed route".into()));
        }
    }

    #[test]
    fn pdf_layout_repairs_preserve_words_and_create_independent_blocks() {
        let before = "Left choice\nOR Right choice\ncontinues here.\ncontinues there.";
        let after = "Left choice\ncontinues here.\n\nOR\n\nRight choice\ncontinues there.";
        assert!(valid_pdf_order_edit(before, after));
        assert!(valid_pdf_order_edit(&before.replace("OR Right", "OR\nRight"), after));
        assert!(valid_pdf_order_edit("Left choice Right choice\ncontinues here. continues there.",
            "Left choice\ncontinues here.\n\nRight choice\ncontinues there."));
        assert!(!valid_pdf_order_edit(before, &after.replace("there", "elsewhere")));
        assert!(valid_pdf_order_edit(" ", "\n\n"));
        assert!(!valid_pdf_order_edit("text", "\n\n"));
        assert!(valid_pdf_order_edit("78", "⁷⁸"));
        assert!(!valid_pdf_order_edit("78", "⁷⁹"));
        let sentences = crate::segment_pdf_with_titles(after, "fixture");
        let segments = sentences.iter().map(|sentence| sentence.text.as_str()).collect::<Vec<_>>();
        assert_eq!(segments, ["Left choice\ncontinues here.", "OR", "Right choice\ncontinues there."]);
    }

    #[test]
    fn empty_detached_ocr_retry_leaves_the_current_text_and_its_language() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.db")).unwrap();
        let original = root.path().join("library.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "library.pdf", &original, "fr",
            vec!["Texte natif.".into(), String::new()], vec![],
        ).unwrap();
        let id = store.import_pdf_file(prepared).unwrap().id;
        let line = |text: &str| ocr::OcrLine {
            page: 1, text: text.into(), x: 0.1, y: 0.2, width: 0.6, height: 0.04,
            confidence: None, rotation: None, correction: None, block_start: false,
            words: Vec::new(), source_span: None,
        };
        pdf_detached_pages::write_ocr(&store.connection, &id, 1, "fr", &[line("Texte reconnu.")], 2).unwrap();
        store.set_pdf_reading_page(&id, 1).unwrap();
        store.set_document_language(&id, "en").unwrap();
        let claim = store.claim_pdf_ocr_page_for_window(&id, Some(1)).unwrap().unwrap();
        store.complete_pdf_ocr_page(&id, 1, vec![], Some(&claim.token)).unwrap();

        let window = store.pdf_read_window(&id, 1).unwrap();
        assert_eq!(window.pages[1].text, "Texte reconnu.");
        assert!(window.pages[1].refresh_pending);
        assert_eq!(pdf_detached_pages::source_language(&store.connection, &id, 1).unwrap(), Some(Some("fr".into())));
        assert!(store.claim_pdf_ocr_page_for_window(&id, Some(1)).unwrap().is_none());
        let retry = store.claim_pdf_ocr_page_with_retry(&id, Some(1), true).unwrap().unwrap();
        assert_ne!(retry.token, claim.token);
        store.complete_pdf_ocr_page(&id, 1, vec![line("Recognized text.")], Some(&retry.token)).unwrap();
        let window = store.pdf_read_window(&id, 1).unwrap();
        assert_eq!(window.pages[1].text, "Recognized text.");
        assert!(!window.pages[1].refresh_pending);
        assert_eq!(store.pdf_reading_page(&id).unwrap(), Some(1));
    }

    #[test]
    fn stored_pdf_order_repair_preserves_canonical_and_detached_annotations() {
        use sha2::{Digest, Sha256};

        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("library.db")).unwrap();
        let original = root.path().join("library.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let old = "Il est mort\n  Naples\nà\noù nous avons marché avec un marin. Le marin repart.";
        let edits = [PdfTextOrderEdit {
            start_utf16: "Il est mort".len(),
            end_utf16: "Il est mort\n  Naples\nà".encode_utf16().count(),
            replacement: " à Naples".into(),
        }];
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "library.pdf", &original, "fr", vec![old.into(), "".into()], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let revision = document.current_revision_id.as_deref().unwrap();
        let sentence = &document.sentences[0];
        store.set_sentence_note(&document.id, &sentence.id, "keep this note").unwrap();
        store.set_bookmark(&document.id, &sentence.id, true).unwrap();
        let mut save = |word: &str| store.save_selected_word(crate::Vocabulary {
            id: String::new(), word: word.into(), document_id: document.id.clone(),
            language: "fr".into(), target_language: "uk".into(), translation: "переклад".into(),
            context: String::new(), forward: Default::default(), reverse: Default::default(),
            analysis: None, expression: None, sense: None, accepted_translations: Vec::new(),
        }, crate::dictionary_transfer::DictionarySelection {
            revision_id: revision.to_owned(),
            start_utf16: old[..old.rfind(word).unwrap()].encode_utf16().count(),
            reconstructed_text: None,
        }).unwrap();
        let unchanged = save("marin");
        let moved_inside_edit = save("Naples");
        assert!(store.repair_stored_pdf_text_order(&document.id, 0, revision, old, &edits).unwrap());
        let updated = store.translation_document(&document.id).unwrap();
        let sentence = &updated.sentences[0];
        assert!(sentence.text.contains("mort à Naples"));
        assert_eq!(updated.notes[&sentence.id], "keep this note");
        assert!(updated.bookmarks.contains(&sentence.id));
        let links = serde_json::to_value(store.dictionary_source_locations(&unchanged.id).unwrap()).unwrap();
        assert_eq!(links.as_array().unwrap().len(), 1);
        assert_eq!(links[0]["sentenceId"], updated.sentences[1].id);
        assert_eq!(links[0]["selection"]["startUtf16"],
            updated.text[..updated.text.rfind("marin").unwrap()].encode_utf16().count());
        assert!(store.dictionary_source_locations(&moved_inside_edit.id).unwrap().is_empty());
        assert!(!store.repair_stored_pdf_text_order(&document.id, 0, revision, old, &edits).unwrap());

        pdf_detached_pages::write_text_layer(&store.connection, &document.id, 1, old).unwrap();
        let page = store.detached_pdf_sentences(&document.id, 1).unwrap().unwrap();
        store.select_detached_pdf_sentence(&document.id, 1, Some(1), None).unwrap();
        let spoken = &page.sentences[1];
        store.set_detached_pdf_speech_resume(&document.id, 1, &spoken.id, Some(crate::SpeechResume {
            sentence_id: spoken.id.clone(),
            text_hash: format!("{:x}", Sha256::digest(spoken.text.as_bytes())),
            offset_utf16: 3,
            model: None,
            boundary_text: None,
        })).unwrap();
        store.update_detached_pdf_annotation(
            &document.id, 1, &page.sentences[1].id,
            crate::DetachedPdfAnnotationChange::Note { text: "keep detached".into() },
        ).unwrap();
        let revision = updated.current_revision_id.as_deref().unwrap();
        assert!(store.repair_stored_pdf_text_order(&document.id, 1, revision, old, &edits).unwrap());
        let page = store.detached_pdf_sentences(&document.id, 1).unwrap().unwrap();
        assert!(page.text.contains("mort à Naples"));
        assert_eq!(page.saved, 1);
        let detached = serde_json::to_value(store.detached_pdf_sentence(&document.id, 1, None).unwrap()).unwrap();
        assert_eq!(detached["note"], "keep detached");
        assert_eq!(detached["speechResume"]["offsetUtf16"], 3);
        assert_eq!(detached["speechResume"]["sentenceId"], page.sentences[1].id);
    }

    #[test]
    fn repairs_drop_cap_with_geometry_and_unique_text_layer_context() {
        let reference = "Le 15 février 1815, à Marseille.\nentre dans le port. Il revient.";
        let make_line = |text: &str, x, y, height| ocr::OcrLine {
            rotation: None,
            block_start: false,
            page: 0, text: text.to_owned(), x, y, width: 0.7, height,
            confidence: None, correction: None, words: vec![], source_span: None,
        };
        let mut lines = [
            make_line("© 15 fevrier 1815, à Marseille.", 0.117, 0.264, 0.050),
            make_line("Jentre dans le port. Il revient.", 0.148, 0.303, 0.022),
        ];
        repair_ocr_drop_caps(reference, &mut lines);
        assert_eq!(lines[0].reading_text(), "Le 15 fevrier 1815, à Marseille.");
        assert_eq!(lines[1].reading_text(), "entre dans le port. Il revient.");
        assert!(lines[0].text.starts_with('©'));
        assert!(reconcile_ocr_with_layer(reference, &mut lines));
        assert_eq!(lines[0].reading_text(), "Le 15 février 1815, à Marseille.");

        lines[0].correction = None;
        lines[0].height = 0.024;
        lines[1].correction = None;
        repair_ocr_drop_caps(reference, &mut lines);
        assert!(lines.iter().all(|line| line.correction.is_none()));
    }
}

#[cfg(test)]
mod note_punctuation_tests {
    use super::*;

    #[test]
    fn confirmed_footnote_stops_splitting_sentences_and_preserves_real_questions() {
        let layer = "Il monte dans une chaloupe2 pour rejoindre son bateau.\n\
            Ce n’est pas le capitaine3 qui dirige la manœuvre.\n\
            Avez-vous vu cette grande chaloupe2 ?\n\
            Parlez-vous souvent de cette autre chaloupe ?\n\
            2 Chaloupe : petit bateau non couvert.\n\
            3 Capitaine : personne qui dirige un bateau.";
        let mut lines: Vec<_> = [
            "Il monte dans une chaloupe?",
            "Ce n'est pas le capitaine' qui dirige la manœuvre.",
            "Avez-vous vu cette grande chaloupe?",
            "Parlez-vous souvent de cette autre chaloupe?",
        ].into_iter().map(|text| ocr::OcrLine {
            rotation: None,
            block_start: false,
            page: 0, text: text.to_owned(), x: 0.1, y: 0.2, width: 0.7, height: 0.02,
            confidence: None, correction: None, words: vec![], source_span: None,
        }).collect();
        remove_spurious_ocr_note_punctuation(layer, &mut lines);
        assert_eq!(lines[0].reading_text(), "Il monte dans une chaloupe");
        assert_eq!(lines[1].reading_text(), "Ce n'est pas le capitaine qui dirige la manœuvre.");
        assert!(lines[2].reading_text().ends_with('?'));
        assert!(lines[3].reading_text().ends_with('?'));
        assert_eq!(lines[0].text, "Il monte dans une chaloupe?");
    }
}
