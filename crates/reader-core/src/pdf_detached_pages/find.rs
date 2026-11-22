use super::database_error;
use crate::{Result, Store};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

const MAX_SCAN_PAGES: usize = 16;
const MAX_RESULTS: usize = 30;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DetachedPdfFindCursor {
    pub page: u16,
    pub position: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfFindHit {
    page: u16,
    position: usize,
    sentence_id: String,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfFindPage {
    matches: Vec<DetachedPdfFindHit>,
    next: Option<DetachedPdfFindCursor>,
}

impl Store {
    pub fn find_detached_pdf_text(
        &self, id: &str, query: &str, start: DetachedPdfFindCursor, limit: usize,
    ) -> Result<DetachedPdfFindPage> {
        if query.len() > 1024 || !(1..=MAX_RESULTS).contains(&limit) {
            return Err("Некоректні параметри пошуку у PDF.".into());
        }

        let mut result = DetachedPdfFindPage { matches: Vec::with_capacity(limit), next: None };
        let needle = search_text(query.trim());
        if needle.is_empty() { return Ok(result); }
        let mut statement = self.connection.prepare(
            "SELECT page,text FROM pdf_detached_pages WHERE document_id=?1 AND page>=?2 ORDER BY page LIMIT ?3",
        ).map_err(database_error)?;
        let mut pages = statement.query(params![id, start.page, MAX_SCAN_PAGES + 1]).map_err(database_error)?;
        let mut scanned = 0;
        while let Some(row) = pages.next().map_err(database_error)? {
            let page: u16 = row.get(0).map_err(database_error)?;
            let first_position = if page == start.page { start.position } else { 0 };
            if scanned == MAX_SCAN_PAGES {
                result.next = Some(DetachedPdfFindCursor { page, position: first_position });
                break;
            }
            scanned += 1;

            let text: String = row.get(1).map_err(database_error)?;
            let prefix = super::sentence_prefix(id, page, &super::page_revision(&text));
            for (position, sentence) in crate::segment(&text, &prefix).into_iter().enumerate().skip(first_position) {
                let normalized = search_text(&sentence.text);
                let Some(offset) = normalized.find(&needle) else { continue; };
                if result.matches.len() == limit {
                    result.next = Some(DetachedPdfFindCursor { page, position });
                    return Ok(result);
                }
                let text = if sentence.text.chars().take(513).count() <= 512 {
                    sentence.text
                } else {
                    let excerpt: String = normalized[offset..].chars().take(512).collect();
                    if offset == 0 { excerpt } else { format!("…{excerpt}") }
                };
                result.matches.push(DetachedPdfFindHit { page, position, sentence_id: sentence.id, text });
            }
        }
        Ok(result)
    }
}

fn search_text(value: &str) -> String {
    value.nfc().map(|ch| match ch {
        '’' | 'ʼ' => '\'',
        ch => ch,
    }).flat_map(char::to_lowercase).collect()
}
