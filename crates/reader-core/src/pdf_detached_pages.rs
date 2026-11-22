use crate::{ocr, Result, Sentence, Store};
use serde::Serialize;
use sha2::{Digest, Sha256};
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
        );
        CREATE TABLE IF NOT EXISTS pdf_detached_positions (
            document_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            sentence INTEGER NOT NULL CHECK(sentence >= 0),
            PRIMARY KEY(document_id,page),
            FOREIGN KEY(document_id,page) REFERENCES pdf_detached_pages(document_id,page) ON DELETE CASCADE
        );",
    )
    .map_err(database_error)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfSentence {
    pub sentence: Sentence,
    pub position: usize,
    pub total: usize,
    pub language: String,
}

impl Store {
    pub fn detached_pdf_sentence(
        &self, id: &str, page: u16, requested: Option<usize>,
    ) -> Result<Option<DetachedPdfSentence>> {
        let db = &self.connection;
        let result: Option<(String, String, usize)> = db.query_row(
            "SELECT p.text, json_extract(d.body, '$.language'), coalesce(c.sentence,0)
             FROM pdf_detached_pages p JOIN documents d ON d.id=p.document_id
             LEFT JOIN pdf_detached_positions c ON c.document_id=p.document_id AND c.page=p.page
             WHERE p.document_id=?1 AND p.page=?2",
            params![id, page],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).optional().map_err(database_error)?;
        let Some((text, language, saved)) = result else { return Ok(None); };
        let revision = format!("{id}:{page}:{:x}", Sha256::digest(text.as_bytes()));
        let sentences = crate::segment(&text, &revision);
        if sentences.is_empty() { return Ok(None); }
        let total = sentences.len();
        let position = requested.unwrap_or(saved).min(total - 1);
        if requested.is_some() {
            db.execute(
                "INSERT INTO pdf_detached_positions (document_id,page,sentence)
                 SELECT document_id,page,?3 FROM pdf_detached_pages WHERE document_id=?1 AND page=?2
                 ON CONFLICT(document_id,page) DO UPDATE SET sentence=excluded.sentence",
                params![id, page, position],
            ).map_err(database_error)?;
        }
        let sentence = sentences.into_iter().nth(position).ok_or("Речення PDF не знайдено.")?;
        Ok(Some(DetachedPdfSentence { sentence, position, total, language }))
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

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка кешу сторінки PDF. {error}")
}
