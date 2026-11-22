use super::{database_error, DetachedPdfSentence};
use crate::{Result, Store};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum DetachedPdfAnnotationChange {
    Bookmark { saved: bool },
    Note { text: String },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ArchivedAnnotation {
    pub start_utf16: usize,
    pub bookmarked: bool,
    pub note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfAnnotationItem {
    position: usize,
    text: String,
    bookmarked: bool,
    note: Option<String>,
}

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS pdf_detached_annotations (
            document_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            start_utf16 INTEGER NOT NULL CHECK(start_utf16 >= 0),
            bookmarked INTEGER NOT NULL CHECK(bookmarked IN (0,1)),
            note TEXT,
            PRIMARY KEY(document_id,page,start_utf16),
            FOREIGN KEY(document_id,page) REFERENCES pdf_detached_pages(document_id,page) ON DELETE CASCADE
        );",
    )
    .map_err(database_error)
}

pub(crate) fn has_annotations(db: &Connection, id: &str, page: u16) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pdf_detached_annotations WHERE document_id=?1 AND page=?2)",
        params![id, page],
        |row| row.get(0),
    ).map_err(database_error)
}

pub(super) fn read(
    db: &Connection, id: &str, page: u16, start: usize,
) -> Result<(bool, Option<String>)> {
    db.query_row(
        "SELECT bookmarked,note FROM pdf_detached_annotations
         WHERE document_id=?1 AND page=?2 AND start_utf16=?3",
        params![id, page, start],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map(|result| result.unwrap_or_default()).map_err(database_error)
}

pub(crate) fn validate_note(note: &str) -> Result<()> {
    if note.len() > 4000 || note.contains('\0') {
        return Err("Нотатка має містити не більш як 4 КБ тексту.".into());
    }
    Ok(())
}

pub(super) fn list(db: &Connection, id: &str, page: u16) -> Result<Vec<ArchivedAnnotation>> {
    let mut statement = db.prepare(
        "SELECT start_utf16,bookmarked,note FROM pdf_detached_annotations
         WHERE document_id=?1 AND page=?2 ORDER BY start_utf16",
    ).map_err(database_error)?;
    let rows = statement.query_map(params![id, page], |row| Ok(ArchivedAnnotation {
        start_utf16: row.get(0)?,
        bookmarked: row.get(1)?,
        note: row.get(2)?,
    })).map_err(database_error)?;
    rows.collect::<rusqlite::Result<_>>().map_err(database_error)
}

pub(super) fn merge(db: &Connection, id: &str, page: u16, annotations: &[ArchivedAnnotation]) -> Result<()> {
    for item in annotations {
        db.execute(
            "INSERT INTO pdf_detached_annotations (document_id,page,start_utf16,bookmarked,note)
             VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(document_id,page,start_utf16) DO UPDATE SET
                bookmarked=max(bookmarked,excluded.bookmarked),
                note=coalesce(note,excluded.note)",
            params![id, page, item.start_utf16, item.bookmarked, item.note],
        ).map_err(database_error)?;
    }
    Ok(())
}

impl Store {
    pub fn update_detached_pdf_annotation(
        &self, id: &str, page: u16, sentence_id: &str, change: DetachedPdfAnnotationChange,
    ) -> Result<DetachedPdfSentence> {
        let tx = self.connection.unchecked_transaction().map_err(database_error)?;
        let mut current = self.detached_pdf_sentence(id, page, None)?
            .filter(|current| current.sentence.id == sentence_id)
            .ok_or("Речення сторінки PDF змінилося.")?;
        match change {
            DetachedPdfAnnotationChange::Bookmark { saved } => current.bookmarked = saved,
            DetachedPdfAnnotationChange::Note { text } => {
                let text = text.trim();
                validate_note(text)?;
                current.note = (!text.is_empty()).then(|| text.to_owned());
            }
        }
        let start = current.sentence.start_utf16;
        if !current.bookmarked && current.note.is_none() {
            tx.execute(
                "DELETE FROM pdf_detached_annotations WHERE document_id=?1 AND page=?2 AND start_utf16=?3",
                params![id, page, start],
            ).map_err(database_error)?;
        } else {
            tx.execute(
                "INSERT INTO pdf_detached_annotations (document_id,page,start_utf16,bookmarked,note)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(document_id,page,start_utf16) DO UPDATE SET
                    bookmarked=excluded.bookmarked,note=excluded.note",
                params![id, page, start, current.bookmarked, current.note],
            ).map_err(database_error)?;
        }
        tx.commit().map_err(database_error)?;
        Ok(current)
    }

    pub fn detached_pdf_annotations(&self, id: &str, page: u16) -> Result<Vec<DetachedPdfAnnotationItem>> {
        let Some(cached) = self.detached_pdf_sentences(id, page)? else { return Ok(Vec::new()); };
        let items = list(&self.connection, id, page)?;
        Ok(items.into_iter().filter_map(|item| {
            let position = cached.sentences.binary_search_by_key(&item.start_utf16, |sentence| sentence.start_utf16)
                .ok()?;
            Some(DetachedPdfAnnotationItem {
                position,
                text: cached.sentences[position].text.clone(),
                bookmarked: item.bookmarked,
                note: item.note,
            })
        }).collect())
    }
}
