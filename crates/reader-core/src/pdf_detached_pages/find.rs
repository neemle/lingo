use super::{database_error, translation};
use crate::{Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

const MAX_SCAN_PAGES: usize = 16;
const MAX_TRANSLATION_SCAN_SENTENCES: usize = 128;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pivot_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_text: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfFindPage {
    matches: Vec<DetachedPdfFindHit>,
    next: Option<DetachedPdfFindCursor>,
}

impl Store {
    pub fn find_detached_pdf_text(
        &self, id: &str, query: &str, start: DetachedPdfFindCursor, limit: usize, target: Option<&str>,
    ) -> Result<DetachedPdfFindPage> {
        if query.len() > 1024 || !(1..=MAX_RESULTS).contains(&limit) {
            return Err("Некоректні параметри пошуку у PDF.".into());
        }

        let mut result = DetachedPdfFindPage { matches: Vec::with_capacity(limit), next: None };
        let needle = search_text(query.trim());
        if needle.is_empty() { return Ok(result); }
        let routes = if let Some(target) = target {
            crate::valid_language(target)?;
            let source: Option<String> = self.connection.query_row(
                "SELECT json_extract(body, '$.language') FROM documents WHERE id=?1
                 AND EXISTS (SELECT 1 FROM pdf_detached_pages WHERE document_id=?1 AND page>=?2)",
                params![id, start.page], |row| row.get(0),
            ).optional().map_err(database_error)?;
            let Some(source) = source else { return Ok(result); };
            if source == target { return Ok(result); }
            let resolved = self.translation_cache_routes(&source, target)?;
            Some((target, source, resolved))
        } else {
            None
        };
        let mut statement = self.connection.prepare(
            "SELECT page,text,soft_lines FROM pdf_detached_pages WHERE document_id=?1 AND page>=?2 ORDER BY page LIMIT ?3",
        ).map_err(database_error)?;
        let mut pages = statement.query(params![id, start.page, MAX_SCAN_PAGES as u32 + 1]).map_err(database_error)?;
        let mut scanned = 0;
        let mut scanned_sentences = 0;
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
            let sentences = crate::pdf_sentences::segment(&text, &prefix, row.get(2).map_err(database_error)?);
            let (projection, original) = if let Some((target, source, _)) = &routes {
                (Some(self.project_pdf_translation_page(id, page, target, text, sentences, source.clone())?), Vec::new())
            } else {
                (None, sentences)
            };
            let sentences = projection.as_ref().map_or(original.as_slice(), translation::PageProjection::physical_sentences);
            for (position, sentence) in sentences.iter().enumerate().skip(first_position) {
                let (text, pivot_language, reviewed, source_text) =
                    if let (Some((target, _, routes)), Some(projection)) = (&routes, &projection) {
                        if scanned_sentences == MAX_TRANSLATION_SCAN_SENTENCES {
                            result.next = Some(DetachedPdfFindCursor { page, position });
                            return Ok(result);
                        }
                        scanned_sentences += 1;
                        if let Some(text) = projection.reviewed(&sentence.id, target) {
                            (text.to_owned(), None, Some(true), None)
                        } else if let Some(hit) = translation::cached_page_translation(self, routes, projection, position, target)? {
                            if hit.start_position != position { continue; }
                            (hit.text, hit.pivot_language, Some(false), hit.source_text)
                        } else {
                            continue;
                        }
                    } else {
                        (sentence.text.clone(), None, None, None)
                    };
                let normalized = search_text(&text);
                let Some(offset) = normalized.find(&needle) else { continue; };
                if result.matches.len() == limit {
                    result.next = Some(DetachedPdfFindCursor { page, position });
                    return Ok(result);
                }
                let text = if reviewed.is_some() || text.chars().take(513).count() <= 512 {
                    text
                } else {
                    let excerpt: String = normalized[offset..].chars().take(512).collect();
                    if offset == 0 { excerpt } else { format!("…{excerpt}") }
                };
                result.matches.push(DetachedPdfFindHit {
                    page, position, sentence_id: sentence.id.clone(), text, pivot_language, reviewed, source_text,
                });
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
