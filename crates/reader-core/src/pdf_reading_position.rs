//! Physical reading position for PDFs, independent of sentence and OCR state.

use crate::{Document, Result, Store};
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
            if !contains_page(document, page) {
                return Err("Сторінку PDF не знайдено".into());
            }
            tx.execute(
                "INSERT INTO pdf_reading_positions (document_id,page) VALUES (?1,?2)
                 ON CONFLICT(document_id) DO UPDATE SET page=excluded.page
                 WHERE pdf_reading_positions.page<>excluded.page",
                params![id, page],
            )
            .map_err(database_error)?;
            Ok(false)
        })
    }
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
