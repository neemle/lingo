use super::{annotations, word_break, CachedSentences};
use crate::translation::{
    sentence_input, CacheRoutes, PageBoundaries, ParagraphTranslation, TranslationBatchError, UnitGrouping,
};
use crate::{document_layout, ocr, Document, Result, Sentence, Store};
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
    source_sentence_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_block_index: Option<usize>,
    pub(crate) text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pivot_language: Option<String>,
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
        self.pdf_page_translation_with_priority(id, page, target, cached_only, false)
    }

    pub fn pdf_page_translation_with_priority(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
    ) -> Result<Vec<PdfPageTranslationRow>> {
        self.pdf_page_translation_until_canceled(id, page, target, cached_only, background, &|| false)
    }

    pub(crate) fn pdf_page_translation_until_canceled(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
        canceled: &dyn Fn() -> bool,
    ) -> Result<Vec<PdfPageTranslationRow>> {
        match self.pdf_page_translation_result_interruptible(id, page, target, cached_only, background, canceled) {
            Err(TranslationBatchError::Deferred { .. }) if background && !cached_only && !canceled() =>
                self.pdf_page_translation_result(id, page, target, true, background).map_err(String::from),
            result => result.map_err(String::from),
        }
    }

    pub(crate) fn pdf_page_translation_result(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
    ) -> std::result::Result<Vec<PdfPageTranslationRow>, TranslationBatchError> {
        self.pdf_page_translation_result_interruptible(id, page, target, cached_only, background, &|| false)
    }

    pub(crate) fn pdf_page_translation_result_interruptible(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
        canceled: &dyn Fn() -> bool,
    ) -> std::result::Result<Vec<PdfPageTranslationRow>, TranslationBatchError> {
        const CANCELED: &str = "Переклад сторінки скасовано.";
        if canceled() {
            return Err(TranslationBatchError::deferred(CANCELED.into()));
        }
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
        let page_range = |sentence_ids: &[String]| {
            let mut local = sentence_ids.iter().filter_map(|id| indices.get(id.as_str()).copied());
            let first = local.next()?;
            Some((first, local.last().unwrap_or(first)))
        };
        let row = |first: usize, last: usize, text, source_text, pivot_language| {
            let source = &document.sentences[physical.start + first];
            let end = &document.sentences[physical.start + last];
            PdfPageTranslationRow {
                start_utf16: offsets[first].0, end_utf16: offsets[last].1,
                source_sentence_id: source.id.clone(),
                source_block_index: source_block_index(&document, source, end),
                text, source_text, pivot_language,
            }
        };
        let mut generation_error = None;
        if !cached_only {
            match self.translate_document_page(&document, physical.clone(), target, boundaries, background, canceled) {
                Ok(generated) if generated.iter().any(|unit| !unit.reviewed && unit.translation.is_some())
                    && generated.iter().all(|unit| unit.translation.is_some()) =>
                {
                    return Ok(generated.into_iter().filter_map(|unit| {
                        let (first, last) = page_range(&unit.sentence_ids)?;
                        Some(row(first, last, unit.translation, unit.pdf_source, unit.pivot_language))
                    }).collect());
                }
                Ok(_) => {}
                Err(error) if canceled() || (background && matches!(&error, TranslationBatchError::Deferred { .. })) =>
                    return Err(error),
                // Earlier batches may already be in SQLite, alongside translations from prior routes.
                Err(error) => generation_error = Some(error),
            }
        }
        let routes = self.translation_cache_routes(&document.language, target)?;
        let mut covered = vec![false; offsets.len()];
        let mut rows = BTreeMap::<usize, PdfPageTranslationRow>::new();
        // Older cached groups can fill gaps after all individually aligned rows are reserved.
        for grouping in [UnitGrouping::Sentence, UnitGrouping::LegacyContext] {
            for position in physical.clone() {
                if canceled() {
                    return Err(TranslationBatchError::deferred(CANCELED.into()));
                }
                if covered[position - physical.start] { continue; }
                let sentence = &document.sentences[position];
                if document_layout::skip_in_translation(&document, sentence) { continue; }
                let Some(unit) = routes.cached_pdf_row(self, &document, position, target, boundaries, grouping)?
                    else { continue; };
                let Some((first, last)) = page_range(&unit.sentence_ids) else { continue; };
                if covered[first..=last].contains(&true) { continue; }
                covered[first..=last].fill(true);
                rows.insert(first, row(first, last, unit.translation, unit.pdf_source, unit.pivot_language));
            }
        }
        if let Some(error) = generation_error.filter(|_| rows.is_empty()) { return Err(error); }
        for (local, sentence) in document.sentences[physical.clone()].iter().enumerate() {
            if !covered[local] && !document_layout::skip_in_translation(&document, sentence) {
                rows.insert(local, row(local, local, None, Some(sentence.text.clone()), None));
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
        let lines = self.detached_pdf_ocr(id, page)?.map(|ocr| ocr.lines);
        let layout = if previous.is_some() || next.is_some() {
            let (joined, starts, center) = include_neighbors(id, page, &text, &mut sentences, previous, next);
            let local = (center.clone(), text[..center.start].encode_utf16().count());
            let projected = if physical_start == 0 { (0, 0) } else {
                let first = &sentences[physical_start];
                (first.start_utf8, first.start_utf16)
            };
            let mut layout = Document {
                text: joined,
                format: "pdf".into(),
                pdf_page_starts: Some(starts),
                ocr_lines: lines.map(|lines| project_ocr_lines(lines, local, projected, physical_start as u16)),
                ..Document::default()
            };
            document_layout::reflow_ocr_paragraphs(&mut layout);
            layout
        } else {
            document_layout::detached_pdf_document(text, &language, lines)
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

fn source_block_index(document: &Document, first: &Sentence, last: &Sentence) -> Option<usize> {
    let first_letter = first.start_utf8 + first.text.find(char::is_alphanumeric)?;
    let last_letter = last.start_utf8 + last.text.rfind(char::is_alphanumeric)?;
    let index = document.blocks.partition_point(|block| block.start_utf8 <= first_letter).checked_sub(1)?;
    (last_letter < document.blocks[index].end_utf8).then_some(index)
}

fn project_ocr_lines(
    lines: Vec<ocr::OcrLine>, (center, center_utf16): (Range<usize>, usize),
    (start_utf8, start_utf16): (usize, usize), virtual_page: u16,
) -> Vec<ocr::OcrLine> {
    let shift = |span: ocr::OcrSpan| {
        if span.start_utf8 < center.start || span.end_utf8 > center.end || span.start_utf16 < center_utf16 {
            return None;
        }
        Some(ocr::OcrSpan {
            start_utf8: span.start_utf8 - center.start + start_utf8,
            end_utf8: span.end_utf8 - center.start + start_utf8,
            start_utf16: span.start_utf16 - center_utf16 + start_utf16,
            end_utf16: span.end_utf16 - center_utf16 + start_utf16,
        })
    };
    lines.into_iter().filter_map(|mut line| {
        line.source_span = Some(shift(line.source_span?)?);
        line.page = virtual_page;
        for word in &mut line.words {
            word.source_span = word.source_span.and_then(shift);
        }
        Some(line)
    }).collect()
}

fn include_neighbors(
    id: &str,
    page: u16,
    text: &str,
    sentences: &mut Vec<Sentence>,
    previous: Option<String>,
    next: Option<String>,
) -> (String, Vec<Option<usize>>, Range<usize>) {
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
    (joined, starts, center_byte..center_end)
}

#[cfg(test)]
mod tests {
    use crate::translation::sentence_source;
    use super::*;
    use crate::{file_originals, pdf_detached_pages};

    #[test]
    fn scanned_translation_rows_keep_heading_body_and_caption_blocks() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let line = |text: &str, y, height| ocr::OcrLine {
            page: 0, text: text.into(), x: 0.1, y, width: 0.8, height,
            confidence: None, correction: None, block_start: false, words: Vec::new(), source_span: None,
        };
        let document = store.import_scanned_pdf("scan.pdf", b"%PDF-1.4\n", "fr", vec![
            line("Un capitaine de vingt ans", 0.1, 0.045),
            line("Le marin entre.", 0.16, 0.025),
            line("Il attend à quai.", 0.19, 0.025),
            line("Illustration du port", 0.55, 0.025),
        ], 1).unwrap();
        let rows = store.pdf_page_translation(&document.id, 0, "uk", true).unwrap();
        assert_eq!(rows.iter().map(|row| row.source_block_index).collect::<Vec<_>>(),
            [Some(0), Some(1), Some(1), Some(2)]);
        for (row, sentence) in rows.iter().zip(&document.sentences) {
            assert_eq!(row.source_sentence_id, sentence.id);
            assert_eq!((row.start_utf16, row.end_utf16), (sentence.start_utf16, sentence.end_utf16));
        }
    }

    #[test]
    fn projected_page_joins_a_split_word_without_translating_small_glossary_notes() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let source = root.path().join("scan.pdf");
        std::fs::write(&source, b"%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &source, "fr", vec![String::new(); 2], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let line = |page, text: &str, y, height| ocr::OcrLine {
            block_start: false,
            page, text: text.into(), x: 0.1, y, width: 0.8, height,
            confidence: None, correction: None, words: vec![], source_span: None,
        };
        pdf_detached_pages::write_ocr(&store.connection, &document.id, 0, "fr", &[
            line(0, "Les marins partirent.", 0.1, 0.04),
            line(0, "La mer était calme.", 0.15, 0.04),
            line(0, "Le vent soufflait.", 0.2, 0.04),
            line(0, "C'était un voyage extraordi-", 0.25, 0.04),
            line(0, "Marin : métier de la mer.", 0.8, 0.015),
            line(0, "Mer : grande étendue d'eau.", 0.82, 0.015),
            line(0, "Voyage : déplacement lointain.", 0.84, 0.015),
        ], 2).unwrap();
        pdf_detached_pages::write_ocr(&store.connection, &document.id, 1, "fr", &[
            line(1, "naire. Le marin s'approcha du quai.", 0.1, 0.04),
            line(1, "Le vent soufflait fort.", 0.15, 0.04),
            line(1, "Les mouettes criaient.", 0.2, 0.04),
            line(1, "Il attendit le bateau.", 0.25, 0.04),
            line(1, "Quai : lieu où les bateaux accostent.", 0.8, 0.015),
            line(1, "Vent : mouvement de l'air.", 0.82, 0.015),
            line(1, "Mouette : oiseau de mer.", 0.84, 0.015),
        ], 2).unwrap();

        let cached = store.detached_pdf_sentences(&document.id, 1).unwrap().unwrap();
        let glossary_start = cached.text.find("Quai :").unwrap();
        let glossary_start = cached.text[..glossary_start].encode_utf16().count();
        let projection = store.project_pdf_translation_page(
            &document.id, 1, "uk", cached.text, cached.sentences.clone(), cached.language,
        ).unwrap();
        assert_eq!(projection.physical.start, 1);
        let (joined, _) = sentence_source(&store, &projection.document, projection.physical.start,
            "uk", PageBoundaries::Projected).unwrap();
        assert!(joined.contains("extraordinaire."), "{joined}");
        assert!(!joined.contains("métier"));
        assert_eq!(projection.document.blocks.iter().filter(|block| block.footnote).count(), 3);
        let rows = store.pdf_page_translation(&document.id, 1, "uk", true).unwrap();
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| row.end_utf16 <= glossary_start));
        for row in &rows {
            let sentence = cached.sentences.iter().find(|sentence| sentence.id == row.source_sentence_id).unwrap();
            assert_eq!(row.start_utf16, sentence.start_utf16);
            assert_eq!(row.source_text.as_deref(), Some(sentence.text.as_str()));
        }
        let row = &rows[0];
        assert!(store.select_detached_pdf_sentence_at(
            &document.id, 1, None, Some("stale-id"), Some(row.start_utf16),
        ).is_err());
        let selected = store.select_detached_pdf_sentence_at(
            &document.id, 1, None, Some(&row.source_sentence_id), Some(row.start_utf16),
        ).unwrap().unwrap();
        assert_eq!(selected.sentence.id, row.source_sentence_id);

        let left = store.detached_pdf_sentences(&document.id, 0).unwrap().unwrap();
        let dangling = left.sentences.iter().position(|sentence| sentence.text.contains("extraordi-")).unwrap();
        let spoken = store.detached_pdf_sentence(&document.id, 0, Some(dangling)).unwrap().unwrap();
        assert_eq!(spoken.narration_text.as_deref(), Some("C'était un voyage extraordinaire."));
        let continuation = store.detached_pdf_sentence(&document.id, 1, Some(0)).unwrap().unwrap();
        assert!(continuation.narration_text.as_deref().is_some_and(|text| !text.starts_with("naire")));
    }
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
