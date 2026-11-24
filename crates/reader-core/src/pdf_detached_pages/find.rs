use super::{database_error, translation};
use crate::translation::{normalize_translation_search as search_text, PageBoundaries, TranslationSearchFragment, UnitGrouping};
use crate::{Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pdf_fragment: Option<TranslationSearchFragment>,
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
            "SELECT p.page,p.text,p.soft_lines,json_extract(d.body,'$.language')
             FROM pdf_detached_pages p JOIN documents d ON d.id=p.document_id
             WHERE p.document_id=?1 AND p.page>=?2 ORDER BY p.page LIMIT ?3",
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
            let source_document = crate::Document { language: row.get(3).map_err(database_error)?, ..Default::default() };
            let prefix = super::sentence_prefix(id, page, &super::page_revision(&text));
            let sentences = super::Layout::from_storage(row.get(2).map_err(database_error)?).segment(&text, &prefix);
            let local_start = sentences.first().map_or(0, |sentence| sentence.start_utf16);
            let (projection, original) = if let Some((target, source, _)) = &routes {
                (Some(self.project_pdf_translation_page(id, page, target, text, sentences, source.clone())?), Vec::new())
            } else {
                (None, sentences)
            };
            let sentences = projection.as_ref().map_or(original.as_slice(), translation::PageProjection::physical_sentences);
            let projected_offset = sentences.first().map_or(0, |sentence| sentence.start_utf16 - local_start);
            let layout = projection.as_ref().zip(routes.as_ref())
                .and_then(|(projection, (target, _, _))| projection.ocr_search_projection(target));
            for (position, sentence) in sentences.iter().enumerate().skip(first_position) {
                let mut pdf_fragment = None;
                let (text, pivot_language, reviewed, source_text) =
                    if let (Some((target, _, routes)), Some(projection)) = (&routes, &projection) {
                        if scanned_sentences == MAX_TRANSLATION_SCAN_SENTENCES {
                            result.next = Some(DetachedPdfFindCursor { page, position });
                            return Ok(result);
                        }
                        scanned_sentences += 1;
                        let split = layout.as_ref().and_then(|layout| {
                            let first = layout.sentences.partition_point(|part| part.end_utf16 <= sentence.start_utf16);
                            let last = layout.sentences.partition_point(|part| part.start_utf16 < sentence.end_utf16);
                            (last > first + 1).then_some((layout, first..last))
                        });
                        let has_fragments = split.is_some();
                        let mut matched = None;
                        if let Some((layout, fragments)) = split {
                            for index in fragments {
                                let Some(row) = routes.cached_pdf_row(
                                    self, layout, index, target, PageBoundaries::Projected, UnitGrouping::Sentence,
                                )? else { continue; };
                                let Some(text) = row.translation else { continue; };
                                if !search_text(&text).contains(&needle) { continue; }
                                let part = &layout.sentences[index];
                                pdf_fragment = Some(TranslationSearchFragment {
                                    page,
                                    start_utf16: part.start_utf16 - projected_offset,
                                    end_utf16: part.end_utf16 - projected_offset,
                                    pivot_language: row.pivot_language,
                                    reflow_only: false,
                                });
                                matched = Some(text);
                                break;
                            }
                        }
                        if let Some(text) = matched {
                            (text, None, None, None)
                        } else if let Some(text) = projection.reviewed(&sentence.id, target) {
                            (text.to_owned(), None, Some(true), None)
                        } else if let Some(hit) = translation::cached_page_translation(self, routes, projection, position, target, true)? {
                            if hit.start_position != position { continue; }
                            if has_fragments {
                                pdf_fragment = Some(TranslationSearchFragment {
                                    page, start_utf16: sentence.start_utf16 - projected_offset,
                                    end_utf16: sentence.end_utf16 - projected_offset,
                                    pivot_language: hit.pivot_language.clone(), reflow_only: true,
                                });
                            }
                            (hit.text, hit.pivot_language, Some(false), hit.source_text)
                        } else {
                            continue;
                        }
                    } else {
                        (sentence.text.clone(), None, None, None)
                    };
                let mut normalized = search_text(&text);
                let mut offset = normalized.find(&needle);
                if offset.is_none() && target.is_none() && text.contains('\n') && text.contains(['-', '\u{00ad}', '\u{2010}']) {
                    normalized = search_text(&crate::translation::normalize_ocr_input(&source_document, &text, 0));
                    offset = normalized.find(&needle);
                }
                let Some(offset) = offset else { continue; };
                if result.matches.len() == limit {
                    result.next = Some(DetachedPdfFindCursor { page, position });
                    return Ok(result);
                }
                let text = if reviewed.is_none() && pdf_fragment.is_none() && text.chars().take(513).count() > 512 {
                    let excerpt: String = normalized[offset..].chars().take(512).collect();
                    if offset == 0 { excerpt } else { format!("…{excerpt}") }
                } else {
                    text
                };
                result.matches.push(DetachedPdfFindHit {
                    page, position, sentence_id: sentence.id.clone(), text, pivot_language, reviewed, source_text, pdf_fragment,
                });
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_wrapped_phrase_in_detached_page_with_original_sentence_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("reader.sqlite")).unwrap();
        let path = directory.path().join("source.pdf");
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "source.pdf", &path, "fr", vec![String::new(); 3], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let text = "L’e\u{301}quipage attend\n  sur le quai des marchan-\n  dises. Est-\nil arrivé ?";
        super::super::write_text_layer(&store.connection, &document.id, 2, text).unwrap();
        let cursor = DetachedPdfFindCursor { page: 0, position: 0 };
        let found = store.find_detached_pdf_text(&document.id, "L'équipage attend sur", cursor, 1, None).unwrap();
        let cached = store.detached_pdf_sentences(&document.id, 2).unwrap().unwrap();
        assert_eq!(found.matches.len(), 1);
        let hit = &found.matches[0];
        assert_eq!((hit.page, hit.position), (2, 0));
        assert_eq!(hit.sentence_id, cached.sentences[0].id);
        assert_eq!(hit.text, cached.sentences[0].text);
        let found = store.find_detached_pdf_text(&document.id, "marchandises", cursor, 1, None).unwrap();
        assert_eq!(found.matches[0].sentence_id, cached.sentences[0].id);
        let found = store.find_detached_pdf_text(&document.id, "est-il", cursor, 1, None).unwrap();
        assert_eq!(found.matches[0].sentence_id, cached.sentences[1].id);
        assert!(store.find_detached_pdf_text(&document.id, "estil", cursor, 1, None).unwrap().matches.is_empty());
    }
}
