use super::{annotations, word_break, CachedSentences};
use crate::translation::{sentence_input, CacheRoutes, PageBoundaries, ParagraphTranslation};
use crate::{document_layout, Document, Result, Sentence, Store};
use serde::Serialize;
use std::{collections::BTreeMap, ops::Range};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfTranslation {
    pub(super) text: String,
    pub(super) pivot_language: Option<String>,
    pub(super) source_text: Option<String>,
    pub(super) start_position: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfPassage {
    active: Option<DetachedPdfTranslation>,
    rows: Vec<DetachedPdfPassageRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DetachedPdfPassageRow {
    position: usize,
    sentence_count: usize,
    text: String,
}

pub(super) struct PageProjection {
    document: Document,
    physical: Range<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfPageTranslationRow {
    start_utf16: usize,
    end_utf16: usize,
    text: Option<String>,
}

impl PageProjection {
    pub(super) fn physical_sentences(&self) -> &[Sentence] {
        &self.document.sentences[self.physical.clone()]
    }

    pub(super) fn reviewed(&self, sentence_id: &str, target: &str) -> Option<&str> {
        self.document
            .reviewed_translations
            .get(target)?
            .get(sentence_id)
            .map(String::as_str)
    }

    fn localize(&self, mut result: DetachedPdfTranslation) -> DetachedPdfTranslation {
        result.start_position = result.start_position.saturating_sub(self.physical.start);
        result
    }
}

impl Store {
    pub fn pdf_page_translation(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool,
    ) -> Result<Vec<PdfPageTranslationRow>> {
        let (document, physical, offsets, boundaries) = if let Some(cached) = self.detached_pdf_sentences(id, page)? {
            let spans = cached.sentences.iter().map(|s| (s.start_utf16, s.end_utf16)).collect::<Vec<_>>();
            let projection = self.project_pdf_translation_page(id, page, target, cached.text, cached.sentences, cached.language)?;
            (projection.document, projection.physical, spans, PageBoundaries::Projected)
        } else {
            let document = self.translation_document(id)?;
            let Some((start, end)) = crate::pdf_reading_position::page_bounds(&document, page) else { return Ok(Vec::new()); };
            let first = document.sentences.partition_point(|s| s.end_utf16 <= start);
            let last = document.sentences.partition_point(|s| s.start_utf16 < end);
            let spans = document.sentences[first..last].iter()
                .map(|s| (s.start_utf16.max(start), s.end_utf16.min(end))).collect();
            (document, first..last, spans, PageBoundaries::Canonical)
        };
        if document.language == target { return Ok(Vec::new()); }
        let indices: std::collections::HashMap<_, _> = document.sentences[physical.clone()].iter()
            .enumerate().map(|(i, sentence)| (sentence.id.as_str(), i)).collect();
        let page_range = |unit: &crate::translation::ParagraphUnit| {
            let mut local = unit.sentence_ids.iter().filter_map(|id| indices.get(id.as_str()).copied());
            let first = local.next()?;
            Some((first, local.last().unwrap_or(first)))
        };
        if !cached_only {
            let generated = self.translate_document_page(&document, physical.clone(), target, boundaries)?;
            if generated.iter().any(|unit| !unit.reviewed && unit.translation.is_some()) {
                return Ok(generated.into_iter().filter_map(|unit| {
                    let (first, last) = page_range(&unit)?;
                    Some(PdfPageTranslationRow {
                        start_utf16: offsets[first].0, end_utf16: offsets[last].1, text: unit.translation,
                    })
                }).collect());
            }
        }
        let routes = self.translation_cache_routes(&document.language, target)?;
        let mut covered = vec![false; offsets.len()];
        let mut rows = BTreeMap::<usize, PdfPageTranslationRow>::new();
        for position in physical.clone() {
            if covered[position - physical.start] { continue; }
            let sentence = &document.sentences[position];
            if document_layout::sentence_in_footnote(&document.blocks, sentence) { continue; }
            let cached = routes.cached_window(self, &document, position, target, boundaries)?;
            for unit in cached.paragraph.units {
                let Some((first, last)) = page_range(&unit) else { continue; };
                let text = unit.translation.or_else(|| cached.sentence.as_ref()
                    .filter(|_| first == last && first == position - physical.start)
                    .and_then(|result| result.segments.first().cloned()));
                if text.is_some() { covered[first..=last].fill(true); }
                let row = rows.entry(first).or_insert(PdfPageTranslationRow {
                    start_utf16: offsets[first].0, end_utf16: offsets[last].1, text: None,
                });
                if row.text.is_none() && text.is_some() { row.text = text; }
            }
        }
        Ok(rows.into_values().collect())
    }

    pub fn detached_pdf_translation(
        &mut self,
        id: &str,
        page: u16,
        sentence_id: &str,
        target: &str,
        cached_only: bool,
        background: bool,
    ) -> Result<Option<DetachedPdfTranslation>> {
        let (projection, selected) = self.detached_page_projection(id, page, sentence_id, target)?;
        if cached_only {
            let routes = self.translation_cache_routes(&projection.document.language, target)?;
            return cached_page_translation(self, &routes, &projection, selected, target, false);
        }
        let position = projection.physical.start + selected;
        let translated =
            self.translate_document_paragraph(&projection.document, position, target, background, PageBoundaries::Projected)?;
        if let Some(active) = active_translation(&projection.document, position, translated) {
            return Ok(Some(projection.localize(active)));
        }
        let routes = self.translation_cache_routes(&projection.document.language, target)?;
        cached_page_translation(self, &routes, &projection, selected, target, false)
    }

    pub fn detached_pdf_passage(
        &mut self,
        id: &str,
        page: u16,
        sentence_id: &str,
        target: &str,
        cached_only: bool,
        background: bool,
    ) -> Result<DetachedPdfPassage> {
        let (projection, selected) = self.detached_page_projection(id, page, sentence_id, target)?;
        let document = &projection.document;
        let position = projection.physical.start + selected;
        let routes = self.translation_cache_routes(&document.language, target)?;
        let paragraph = if cached_only {
            routes.cached_window(self, document, position, target, PageBoundaries::Projected)?.paragraph
        } else {
            self.translate_document_paragraph(document, position, target, background, PageBoundaries::Projected)?
        };
        let mut rows = Vec::new();
        for unit in &paragraph.units {
            let mut positions = projection.physical_sentences().iter().enumerate()
                .filter(|(_, sentence)| unit.sentence_ids.contains(&sentence.id))
                .map(|(index, _)| index);
            if let (Some(first), Some(text)) = (positions.next(), &unit.translation) {
                rows.push(DetachedPdfPassageRow {
                    position: first,
                    sentence_count: 1 + positions.count(),
                    text: text.clone(),
                });
            }
        }
        rows.sort_by_key(|row| row.position);
        let active = active_translation(document, position, paragraph)
            .map(|result| projection.localize(result));
        let active = match active {
            Some(active) => Some(active),
            None => cached_page_translation(self, &routes, &projection, selected, target, false)?,
        };
        Ok(DetachedPdfPassage { active, rows })
    }

    fn detached_page_projection(
        &self, id: &str, page: u16, sentence_id: &str, target: &str,
    ) -> Result<(PageProjection, usize)> {
        let CachedSentences {
            language,
            text,
            saved,
            sentences,
            ..
        } = self
            .detached_pdf_sentences(id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let selected = saved.min(sentences.len() - 1);
        if sentences[selected].id != sentence_id {
            return Err("Позиція читання сторінки PDF змінилася.".into());
        }
        let projection = self.project_pdf_translation_page(id, page, target, text, sentences, language)?;
        Ok((projection, selected))
    }

    pub(crate) fn detached_sentence_input(
        &self, id: &str, page: u16, sentence_id: &str, target: &str,
    ) -> Result<(String, String)> {
        let CachedSentences { language, text, saved, sentences, .. } = self
            .detached_pdf_sentences(id, page)?.ok_or("Текст сторінки PDF недоступний.")?;
        let selected = saved.min(sentences.len() - 1);
        if sentences[selected].id != sentence_id {
            return Err("Позиція читання сторінки PDF змінилася.".into());
        }
        let projection = self.project_pdf_translation_page(id, page, target, text, sentences, language)?;
        let position = projection.physical.start + selected;
        let (input, _) = sentence_input(self, &projection.document, position, target, PageBoundaries::Projected)?;
        Ok((projection.document.language, input))
    }

    pub(super) fn project_pdf_translation_page(
        &self,
        id: &str,
        page: u16,
        target: &str,
        text: String,
        mut sentences: Vec<Sentence>,
        language: String,
    ) -> Result<PageProjection> {
        let mut reviewed = annotations::reviewed_for_page(&self.connection, id, page, target)?;
        let mut reviewed_translations: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        if !reviewed.is_empty() {
            let values = sentences
                .iter()
                .filter_map(|sentence| {
                    reviewed
                        .remove(&sentence.start_utf16)
                        .map(|text| (sentence.id.clone(), text))
                })
                .collect();
            reviewed_translations.insert(target.to_owned(), values);
        }

        let (mut previous, mut next) = word_break::neighboring_fragments(
            self, id, page, Some(target), sentences.first(), sentences.last(),
        )?;
        if let Some(items) = reviewed_translations.get(target) {
            if sentences
                .first()
                .is_some_and(|sentence| items.contains_key(&sentence.id))
            {
                previous = None;
            }
            if sentences
                .last()
                .is_some_and(|sentence| items.contains_key(&sentence.id))
            {
                next = None;
            }
        }
        let physical_start = usize::from(previous.is_some());
        let physical = physical_start..physical_start + sentences.len();
        let (text, page_starts) = if previous.is_some() || next.is_some() {
            let (joined, starts) =
                include_neighbors(id, page, &text, &mut sentences, previous, next);
            (joined, Some(starts))
        } else {
            (text, None)
        };
        let layout = if let Some(starts) = page_starts {
            let extracted = document_layout::from_transcript(text, Some(starts));
            Document { text: extracted.text, blocks: extracted.blocks, pdf_page_starts: extracted.pdf_page_starts, ..Document::default() }
        } else {
            document_layout::detached_pdf_document(text, &language,
                self.detached_pdf_ocr(id, page)?.map(|ocr| ocr.lines))
        };
        let document = Document {
            id: id.to_owned(),
            sentences,
            language,
            format: "pdf".into(),
            reviewed_translations,
            ..layout
        };
        Ok(PageProjection { document, physical })
    }
}

fn include_neighbors(
    id: &str,
    page: u16,
    text: &str,
    sentences: &mut Vec<Sentence>,
    previous: Option<String>,
    next: Option<String>,
) -> (String, Vec<Option<usize>>) {
    let center_end = if next.is_some() {
        sentences
            .last()
            .map_or(text.len(), |sentence| sentence.end_utf8)
    } else {
        text.len()
    };
    let mut starts = vec![Some(0)];
    let mut joined = String::with_capacity(
        text.len()
            + previous.as_ref().map_or(0, |part| part.len() + 2)
            + next.as_ref().map_or(0, |part| part.len() + 2),
    );
    let mut center_byte = 0;
    if let Some(previous) = previous {
        center_byte = sentences[0].start_utf8;
        let center_utf16 = sentences[0].start_utf16;
        let prefix = word_break::boundary_sentence(&previous, id, page - 1);
        let shift_bytes = previous.len() + 2;
        let shift_utf16 = prefix.end_utf16 + 2;
        for sentence in sentences.iter_mut() {
            sentence.start_utf8 = sentence.start_utf8 - center_byte + shift_bytes;
            sentence.end_utf8 = sentence.end_utf8 - center_byte + shift_bytes;
            sentence.start_utf16 = sentence.start_utf16 - center_utf16 + shift_utf16;
            sentence.end_utf16 = sentence.end_utf16 - center_utf16 + shift_utf16;
        }
        joined.push_str(&previous);
        joined.push_str("\n\n");
        starts.push(Some(shift_utf16));
        sentences.insert(0, prefix);
    }

    if let Some(next) = next {
        let last = &sentences[sentences.len() - 1];
        joined.push_str(&text[center_byte..center_end]);
        joined.push_str("\n\n");
        let mut suffix = word_break::boundary_sentence(&next, id, page + 1);
        suffix.start_utf8 = joined.len();
        suffix.end_utf8 += suffix.start_utf8;
        suffix.start_utf16 = last.end_utf16 + 2;
        suffix.end_utf16 += suffix.start_utf16;
        starts.push(Some(suffix.start_utf16));
        joined.push_str(&next);
        sentences.push(suffix);
    } else {
        joined.push_str(&text[center_byte..]);
    }
    (joined, starts)
}

pub(super) fn cached_page_translation(
    store: &Store,
    routes: &CacheRoutes,
    projection: &PageProjection,
    selected: usize,
    target: &str,
    for_search: bool,
) -> Result<Option<DetachedPdfTranslation>> {
    let document = &projection.document;
    let local_position = selected;
    let selected = projection.physical.start + selected;
    let original = &document.sentences[selected];
    if document_layout::sentence_in_footnote(&document.blocks, original) {
        return Ok(None);
    }
    if document
        .reviewed_translations
        .get(target)
        .is_some_and(|items| items.contains_key(&original.id))
    {
        return Ok(None);
    }
    let cached = routes.cached_active(store, document, selected, target, for_search)?;
    if let Some(active) = active_translation(document, selected, cached.paragraph) {
        return Ok(Some(projection.localize(active)));
    }
    if let Some(aligned) = cached.sentence {
        if let Some((text, ids)) = aligned.segments.into_iter().next().zip(
            aligned
                .alignment
                .map(|alignment| alignment.source_sentence_ids),
        ) {
            return Ok(machine_result(
                document,
                selected,
                &ids,
                text,
                aligned.route.pivot_language().map(str::to_owned),
            )
            .map(|value| projection.localize(value)));
        }
    }
    let standalone = crate::translation::normalize_pdf_translation_input(
        document, &original.text, original.start_utf8, original.start_utf16,
    );
    Ok(routes
        .cached_standalone(store, &standalone, target)?
        .map(|(text, pivot_language)| DetachedPdfTranslation {
            text,
            pivot_language,
            source_text: None,
            start_position: local_position,
        }))
}

fn active_translation(
    document: &Document,
    selected: usize,
    paragraph: ParagraphTranslation,
) -> Option<DetachedPdfTranslation> {
    let id = &document.sentences[selected].id;
    let unit = paragraph
        .units
        .into_iter()
        .find(|unit| !unit.reviewed && unit.sentence_ids.contains(id))?;
    machine_result(
        document,
        selected,
        &unit.sentence_ids,
        unit.translation?,
        paragraph.pivot_language,
    )
}

fn machine_result(
    document: &Document,
    selected: usize,
    ids: &[String],
    text: String,
    pivot_language: Option<String>,
) -> Option<DetachedPdfTranslation> {
    let id = &document.sentences[selected].id;
    let start_position = selected.checked_sub(ids.iter().position(|candidate| candidate == id)?)?;
    let source_text = if ids.len() > 1 {
        let first = &document.sentences[start_position];
        let last = document.sentences.get(start_position + ids.len() - 1)?;
        Some(
            document
                .text
                .get(first.start_utf8..last.end_utf8)?
                .to_owned(),
        )
    } else {
        None
    };
    Some(DetachedPdfTranslation {
        text,
        pivot_language,
        source_text,
        start_position,
    })
}
