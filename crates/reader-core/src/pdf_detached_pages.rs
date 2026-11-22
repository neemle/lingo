use crate::{ocr, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_detached_pages (
            document_id TEXT NOT NULL REFERENCES file_originals(document_id) ON DELETE CASCADE,
            page INTEGER NOT NULL CHECK(page >= 0 AND page < 65535),
            ocr_language TEXT,
            text TEXT NOT NULL,
            lines TEXT,
            PRIMARY KEY(document_id, page)
        );",
    )
    .map_err(database_error)
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

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка кешу сторінки PDF. {error}")
}
