use super::{annotations, database_error, word_break, CachedSentences};
use crate::translation::{
    pdf_regeneration_input, sentence_input, CacheRoutes, PageBoundaries, ParagraphTranslation,
    ParagraphUnit, TranslationBatchError, UnitGrouping,
};
use crate::{document_layout, ocr, Document, Result, Sentence, Store};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PdfTranslationSource {
    language: String,
    revision_id: Option<String>,
    detached: bool,
}

impl PdfTranslationSource {
    fn matches(&self, language: &str, revision_id: Option<&str>, detached: bool) -> bool {
        self.language == language && self.revision_id.as_deref() == revision_id && self.detached == detached
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PdfSentenceRefresh {
    offset_utf16: usize,
    #[serde(default)]
    anchor: Option<PdfSentenceRefreshAnchor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PdfSentenceRefreshAnchor {
    sentence_id: String,
    source_language: String,
    revision_id: Option<String>,
}

impl PdfSentenceRefresh {
    pub fn at_offset(offset_utf16: usize) -> Self {
        Self { offset_utf16, anchor: None }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfTranslation {
    pub(super) text: String,
    pub(super) pivot_language: Option<String>,
    pub(super) source_text: Option<String>,
    pub(super) start_position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) direct_model_revision: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) legacy_group: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    layout_start_utf16: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    layout_end_utf16: Option<usize>,
    pub(crate) source_sentence_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_block_index: Option<usize>,
    pub(crate) text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pivot_language: Option<String>,
    legacy_group: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    direct_model_revision: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    reviewed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
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

    pub(super) fn ocr_search_projection(&self, target: &str) -> Option<Document> {
        ocr_search_projection(&self.document, self.physical.clone(), None, target)
    }

    fn localize(&self, mut result: DetachedPdfTranslation) -> DetachedPdfTranslation {
        result.start_position = result.start_position.saturating_sub(self.physical.start);
        result
    }
}

impl Store {
    pub(crate) fn warm_pdf_reflow_page(
        &mut self, document: &Document, page: u16, target: &str, canceled: &dyn Fn() -> bool,
    ) -> std::result::Result<Vec<ParagraphUnit>, TranslationBatchError> {
        if let Some(cached) = self.detached_pdf_sentences(&document.id, page)? {
            let projection = self.project_pdf_translation_page(
                &document.id, page, target, cached.text, cached.sentences, cached.language)?;
            return self.translate_document_page(
                &projection.document, projection.physical, target, PageBoundaries::Projected, true, canceled);
        }
        let Some((start, end)) = crate::pdf_reading_position::page_bounds(document, page) else {
            return Ok(Vec::new());
        };
        let physical = document.sentences.partition_point(|sentence| sentence.end_utf16 <= start)
            ..document.sentences.partition_point(|sentence| sentence.start_utf16 < end);
        self.translate_document_page(document, physical, target, PageBoundaries::Canonical, true, canceled)
    }

    pub fn pdf_page_translation(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool,
    ) -> Result<Vec<PdfPageTranslationRow>> {
        self.pdf_page_translation_with_priority(id, page, target, cached_only, false)
    }

    pub fn pdf_page_translation_with_priority(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
    ) -> Result<Vec<PdfPageTranslationRow>> {
        self.pdf_page_translation_until_canceled(id, page, target, cached_only, background, None, None, &|| false)
    }

    pub fn pdf_page_translation_until_canceled(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
        refresh: Option<&PdfSentenceRefresh>,
        expected: Option<&PdfTranslationSource>,
        canceled: &dyn Fn() -> bool,
    ) -> Result<Vec<PdfPageTranslationRow>> {
        match self.pdf_page_translation_result_interruptible(id, page, target, cached_only, background, refresh, expected, canceled) {
            Err(TranslationBatchError::Deferred { .. }) if background && !cached_only && !canceled() =>
                self.pdf_page_translation_result_interruptible(id, page, target, true, background, None, expected, canceled)
                    .map_err(String::from),
            result => result.map_err(String::from),
        }
    }

    pub(crate) fn pdf_page_translation_result(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
    ) -> std::result::Result<Vec<PdfPageTranslationRow>, TranslationBatchError> {
        self.pdf_page_translation_result_interruptible(id, page, target, cached_only, background, None, None, &|| false)
    }

    pub(crate) fn pdf_page_translation_result_interruptible(
        &mut self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
        refresh: Option<&PdfSentenceRefresh>,
        expected: Option<&PdfTranslationSource>,
        canceled: &dyn Fn() -> bool,
    ) -> std::result::Result<Vec<PdfPageTranslationRow>, TranslationBatchError> {
        const CANCELED: &str = "Переклад сторінки скасовано.";
        if canceled() {
            return Err(TranslationBatchError::deferred(CANCELED.into()));
        }
        if refresh.is_some() && (cached_only || background) {
            return Err(TranslationBatchError::Failed("Повторний переклад потребує активного запиту.".into()));
        }
        let missing_selection = || "Виділене речення змінилося. Виберіть його ще раз.".to_owned();
        let changed_source = || TranslationBatchError::Failed("Текст сторінки змінився. Відкрийте сторінку знову.".into());
        let (mut document, physical, bounds, projected_offset, boundaries, revision) = if let Some(cached) = self.detached_pdf_sentences(id, page)? {
            if expected.is_some_and(|source| !source.matches(&cached.language, Some(&cached.revision), true)) {
                return Err(changed_source());
            }
            let local_start = cached.sentences.first().map_or(0, |sentence| sentence.start_utf16);
            let projection = self.project_pdf_translation_page(id, page, target, cached.text, cached.sentences, cached.language)?;
            let offset = projection.physical_sentences().first().map_or(0, |sentence| sentence.start_utf16 - local_start);
            (projection.document, projection.physical, None, offset, PageBoundaries::Projected, Some(cached.revision))
        } else {
            let document = self.translation_document(id)?;
            if expected.is_some_and(|source| !source.matches(&document.language, document.current_revision_id.as_deref(), false)) {
                return Err(changed_source());
            }
            let bounds = if document.format == "pdf" {
                crate::pdf_reading_position::page_bounds(&document, page)
            } else {
                (page == 0 && document.ocr_lines.as_ref().is_some_and(|lines| !lines.is_empty()))
                    .then(|| (0, document.text.encode_utf16().count()))
            };
            let Some((start, end)) = bounds else {
                return if refresh.is_some() { Err(missing_selection().into()) } else { Ok(Vec::new()) };
            };
            let first = document.sentences.partition_point(|s| s.end_utf16 <= start);
            let last = document.sentences.partition_point(|s| s.start_utf16 < end);
            let revision = document.current_revision_id.clone();
            (document, first..last, Some((start, end)), 0, PageBoundaries::Canonical, revision)
        };
        if document.language == target {
            return if refresh.is_some() { Err(missing_selection().into()) } else { Ok(Vec::new()) };
        }
        let original: Vec<_> = document.sentences[physical.clone()].iter()
            .map(|sentence| (sentence.start_utf16..sentence.end_utf16, sentence.id.clone())).collect();
        let selected = refresh.map(|request| {
            let index = pdf_refresh_sentence(&original, request.offset_utf16, projected_offset, bounds)
                .ok_or_else(missing_selection)?;
            if request.anchor.as_ref().is_some_and(|anchor| anchor.sentence_id != original[index].1 ||
                anchor.source_language != document.language || anchor.revision_id != revision)
            {
                return Err(missing_selection());
            }
            Ok(index)
        }).transpose()?;
        let mut reflow_input = None;
        let physical = if let Some(units) = split_ocr_sentences(&document, physical.clone(), bounds.map(|_| page), target) {
            if let Some(index) = selected.filter(|&index| !units.iter().any(|unit| unit.id == original[index].1)) {
                reflow_input = Some(pdf_regeneration_input(self, &document, physical.start + index, target, boundaries)?);
            }
            let resized = physical.start..physical.start + units.len();
            document.sentences.splice(physical, units);
            resized
        } else {
            physical
        };
        let offsets: Vec<_> = document.sentences[physical.clone()].iter().map(|sentence| {
            bounds.map_or_else(|| (sentence.start_utf16 - projected_offset, sentence.end_utf16 - projected_offset),
                |(start, end)| (sentence.start_utf16.max(start), sentence.end_utf16.min(end)))
        }).collect();
        let indices: std::collections::HashMap<_, _> = document.sentences[physical.clone()].iter()
            .enumerate().map(|(i, sentence)| (sentence.id.as_str(), i)).collect();
        let page_range = |sentence_ids: &[String]| {
            let mut local = sentence_ids.iter().filter_map(|id| indices.get(id.as_str()).copied());
            let first = local.next()?;
            Some((first, local.last().unwrap_or(first)))
        };
        let block_starts = pdf_layout_block_starts(&document, &document.sentences[physical.clone()]);
        let row = |first: usize, last: usize, unit: ParagraphUnit, legacy_group| {
            let source = &document.sentences[physical.start + first];
            let end = &document.sentences[physical.start + last];
            let (start, stop) = (offsets[first].0, offsets[last].1);
            let visual = (!unit.reviewed).then(|| translation_layout_span(&document, source, end)).flatten()
                .map(|range| (range.start.saturating_sub(projected_offset), range.end.saturating_sub(projected_offset)))
                .and_then(|(left, right)| {
                    let (left, right) = (left.max(start), right.min(stop));
                    (left < right).then_some((left, right))
                });
            PdfPageTranslationRow {
                start_utf16: start, end_utf16: stop,
                layout_start_utf16: visual.map(|(left, _)| left).filter(|&left| left > start),
                layout_end_utf16: visual.map(|(_, right)| right).filter(|&right| right < stop),
                source_sentence_id: original[original.partition_point(|(span, _)| span.end <= source.start_utf16)].1.clone(),
                source_block_index: source_block_index(&document, &block_starts, source, end),
                text: unit.translation, source_text: unit.pdf_source, pivot_language: unit.pivot_language,
                direct_model_revision: unit.direct_model_revision, reviewed: unit.reviewed,
                legacy_group, failure: unit.translation_failure,
            }
        };
        let mut generation_error = None;
        let mut failures = BTreeMap::new();
        if let Some(index) = selected {
            let selected = pdf_refresh_range(&original[index].0, &offsets, projected_offset)
                .ok_or_else(missing_selection)?;
            let routes = self.translation_cache_routes_for_display(&document.language, target, true)?;
            let positions = routes.pdf_refresh_positions(self, &document, physical.clone(),
                physical.start + selected.start..physical.start + selected.end, target, boundaries)?;
            self.regenerate_document_pdf_sentences(&document, positions, reflow_input, target, boundaries, canceled)?;
        } else if !cached_only {
            match self.translate_document_page(&document, physical.clone(), target, boundaries, background, canceled) {
                // The scheduler needs to see unfinished units even when the
                // reader can display a legacy cached group over the same text.
                Ok(generated) if background ||
                    (generated.iter().any(|unit| !unit.reviewed && unit.translation.is_some())
                        && generated.iter().all(|unit| unit.translation.is_some())) =>
                {
                    return Ok(generated.into_iter().filter_map(|unit| {
                        let (first, last) = page_range(&unit.sentence_ids)?;
                        Some(row(first, last, unit, false))
                    }).collect());
                }
                Ok(generated) => {
                    for unit in generated {
                        if let Some(((first, last), failure)) = page_range(&unit.sentence_ids).zip(unit.translation_failure) {
                            for local in first..=last {
                                failures.insert(local, failure.clone());
                            }
                        }
                    }
                }
                Err(error) if background || canceled() =>
                    return Err(error),
                // Earlier batches may already be in SQLite, alongside translations from prior routes.
                Err(error) => generation_error = Some(error),
            }
        }
        // Saved pivot rows are readable while the direct model works, but they
        // do not count as completed rows for the background worker.
        let routes = self.translation_cache_routes_for_display(&document.language, target, !background)?;
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
                let legacy_group = grouping == UnitGrouping::LegacyContext && first < last;
                rows.insert(first, row(first, last, unit, legacy_group));
            }
        }
        if rows.is_empty() {
            if let Some(error) = generation_error { return Err(error); }
            if let Some(failure) = failures.values().next() { return Err(failure.clone().into()); }
        }
        for (local, sentence) in document.sentences[physical.clone()].iter().enumerate() {
            if !covered[local] && !document_layout::skip_in_translation(&document, sentence) {
                let unit = ParagraphUnit {
                    sentence_ids: Vec::new(), translation: None, reviewed: false, pivot_language: None,
                    direct_model_revision: None, pdf_source: Some(sentence.text.clone()),
                    translation_failure: failures.remove(&local), alignment: None,
                };
                rows.insert(local, row(local, local, unit, false));
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
        let (projection, selected) = self.detached_page_projection(id, page, sentence_id, target, cached_only)?;
        if cached_only {
            let routes = self.translation_cache_routes_for_display(&projection.document.language, target, !background)?;
            return cached_page_translation(self, &routes, &projection, selected, target);
        }
        let position = projection.physical.start + selected;
        let translated =
            self.translate_document_paragraph(&projection.document, position, target, background, PageBoundaries::Projected)?;
        if let Some(active) = active_translation(&projection.document, position, translated) {
            return Ok(Some(projection.localize(active)));
        }
        let routes = self.translation_cache_routes(&projection.document.language, target)?;
        cached_page_translation(self, &routes, &projection, selected, target)
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
        let (projection, selected) = self.detached_page_projection(id, page, sentence_id, target, cached_only)?;
        let document = &projection.document;
        let position = projection.physical.start + selected;
        let routes = self.translation_cache_routes_for_display(&document.language, target, !background)?;
        let paragraph = if cached_only {
            routes.cached_window(self, document, position, target, PageBoundaries::Projected)?.paragraph
        } else {
            let mut generated = self.translate_document_paragraph(document, position, target, background, PageBoundaries::Projected)?;
            if !background && generated.units.iter().any(|unit| unit.translation.is_none()) {
                // A partial direct result should not clear saved neighboring translations.
                // Only fill rows with exactly the same source sentences.
                let mut saved = routes.cached_window(self, document, position, target, PageBoundaries::Projected)?.paragraph.units;
                for unit in generated.units.iter_mut().filter(|unit| unit.translation.is_none()) {
                    if let Some(index) = saved.iter().position(|candidate| {
                        candidate.translation.is_some() && candidate.sentence_ids == unit.sentence_ids
                    }) {
                        *unit = saved.swap_remove(index);
                    }
                }
            }
            generated
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
            None => cached_page_translation(self, &routes, &projection, selected, target)?,
        };
        Ok(DetachedPdfPassage { active, rows })
    }

    fn detached_page_projection(
        &self, id: &str, page: u16, sentence_id: &str, target: &str, cached_only: bool,
    ) -> Result<(PageProjection, usize)> {
        let snapshot = self.connection.unchecked_transaction().map_err(database_error)?;
        let CachedSentences {
            language,
            text,
            saved,
            sentences,
            ..
        } = self
            .detached_pdf_sentences(id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let selected = if cached_only {
            sentences.iter().position(|sentence| sentence.id == sentence_id)
                .ok_or("Речення сторінки PDF змінилося.")?
        } else {
            let selected = saved.min(sentences.len() - 1);
            if sentences[selected].id != sentence_id {
                return Err("Позиція читання сторінки PDF змінилася.".into());
            }
            selected
        };
        let projection = self.project_pdf_translation_page(id, page, target, text, sentences, language)?;
        snapshot.commit().map_err(database_error)?;
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

        let lines = self.detached_pdf_ocr(id, page)?.map(|ocr| ocr.lines);
        let local_layout = document_layout::detached_pdf_document(text, &language, lines);
        let body_end = word_break::last_detached_body_sentence(&local_layout, &sentences);
        let (mut previous, mut next) = word_break::neighboring_fragments(
            self, id, page, Some(target), sentences.first(), body_end.as_deref(),
        )?;
        if let Some(items) = reviewed_translations.get(target) {
            if sentences
                .first()
                .is_some_and(|sentence| items.contains_key(&sentence.id))
            {
                previous = None;
            }
            if body_end.as_deref()
                .is_some_and(|sentence| items.contains_key(&sentence.id))
            {
                next = None;
            }
        }
        let physical_start = usize::from(previous.is_some());
        let physical = physical_start..physical_start + sentences.len();
        let layout = if previous.is_some() || next.is_some() {
            let Document { text, ocr_lines: lines, .. } = local_layout;
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
            local_layout
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

fn pdf_refresh_sentence(
    original: &[(Range<usize>, String)], offset: usize, projection: usize,
    bounds: Option<(usize, usize)>,
) -> Option<usize> {
    let projected = offset.checked_add(projection)?;
    if bounds.is_some_and(|(start, end)| !(start..end).contains(&offset)) { return None; }
    let index = original.partition_point(|(span, _)| span.end <= projected);
    original.get(index).filter(|(span, _)| span.contains(&projected)).map(|_| index)
}

fn pdf_refresh_range(span: &Range<usize>, offsets: &[(usize, usize)], projection: usize) -> Option<Range<usize>> {
    let start = offsets.partition_point(|&(_, end)| end <= span.start.saturating_sub(projection));
    let end = offsets.partition_point(|&(start, _)| start < span.end - projection);
    (start < end).then_some(start..end)
}

pub(crate) fn canonical_ocr_search_projection(document: &Document, page: u16, target: &str) -> Option<Document> {
    let (start, end) = crate::pdf_reading_position::page_bounds(document, page)?;
    let physical = document.sentences.partition_point(|s| s.end_utf16 <= start)
        ..document.sentences.partition_point(|s| s.start_utf16 < end);
    ocr_search_projection(document, physical, Some(page), target)
}

fn ocr_search_projection(document: &Document, physical: Range<usize>, page: Option<u16>, target: &str) -> Option<Document> {
    let units = split_ocr_sentences(document, physical.clone(), page, target)?;
    let mut projection = document.clone();
    projection.sentences.splice(physical, units);
    Some(projection)
}

// A reading sentence can span several independent visual objects on a cover or
// worksheet. Split only the translation projection, preserving reading and note IDs.
fn split_ocr_sentences(
    document: &Document, physical: Range<usize>, page: Option<u16>, target: &str,
) -> Option<Vec<Sentence>> {
    let lines = document.ocr_lines.as_deref()?;
    let mut cuts = Vec::new();
    for pair in lines.windows(2) {
        let [left, right] = pair else { continue; };
        if left.page != right.page || page.is_some_and(|page| left.page != page) { continue; }
        let Some((left_span, right_span)) = left.source_span.zip(right.source_span) else { continue; };
        if left_span.end_utf8 > right_span.start_utf8 || same_ocr_flow(left, right) { continue; }
        if document.text.is_char_boundary(right_span.start_utf8) {
            cuts.push(right_span.start_utf8);
        }
    }
    if cuts.is_empty() { return None; }
    cuts.sort_unstable();
    cuts.dedup();

    let reviewed = document.reviewed_translations.get(target);
    let mut units = Vec::with_capacity(physical.len());
    for sentence in &document.sentences[physical.clone()] {
        let first = cuts.partition_point(|&cut| cut <= sentence.start_utf8);
        let last = cuts.partition_point(|&cut| cut < sentence.end_utf8);
        if first == last || reviewed.is_some_and(|items| items.contains_key(&sentence.id)) {
            units.push(sentence.clone());
            continue;
        }
        let mut start = sentence.start_utf8;
        let mut start_utf16 = sentence.start_utf16;
        for end in cuts[first..last].iter().copied().chain(std::iter::once(sentence.end_utf8)) {
            let part = &document.text[start..end];
            let text = part.trim();
            if !text.is_empty() {
                let leading = part.len() - part.trim_start().len();
                let part_start = start + leading;
                let part_start_utf16 = start_utf16 + part[..leading].encode_utf16().count();
                units.push(Sentence {
                    id: format!("{}{marker}{part_start}", sentence.id,
                        marker = crate::translation::PDF_LAYOUT_FRAGMENT_MARKER),
                    text: text.to_owned(),
                    start_utf8: part_start,
                    end_utf8: part_start + text.len(),
                    start_utf16: part_start_utf16,
                    end_utf16: part_start_utf16 + text.encode_utf16().count(),
                });
            }
            start_utf16 += part.encode_utf16().count();
            start = end;
        }
    }
    (units.len() > physical.len()).then_some(units)
}

fn same_ocr_flow(left: &ocr::OcrLine, right: &ocr::OcrLine) -> bool {
    let rotation = ocr::line_rotation(left).unwrap_or_default();
    if rotation != ocr::line_rotation(right).unwrap_or_default() {
        return false;
    }
    let (left_x, left_y, left_width, left_height) =
        ocr::reading_box((left.x, left.y, left.width, left.height), rotation);
    let (right_x, right_y, right_width, right_height) =
        ocr::reading_box((right.x, right.y, right.width, right.height), rotation);
    if left_height > 2.0 * left_width || right_height > 2.0 * right_width {
        return false;
    }
    let overlap = (left_x + left_width).min(right_x + right_width) - left_x.max(right_x);
    let line_height = left_height.min(right_height);
    let line_gap = right_y - left_y - left_height;
    let wrapped = overlap >= 0.28 * left_width.min(right_width) && right_y > left_y && line_gap <= line_height;
    let inline = (right_y - left_y).abs() <= 0.45 * line_height && right_x >= left_x
        && right_x - left_x - left_width <= 0.9 * line_height;
    wrapped || inline
}

fn pdf_layout_block_starts(document: &Document, page: &[Sentence]) -> Vec<usize> {
    let Some((first, last)) = page.first().zip(page.last()) else { return Vec::new(); };
    let line_start = document.text.get(..first.start_utf8).and_then(|before| before.rfind('\n'))
        .map_or(0, |newline| newline + 1);
    let mut starts: Vec<_> = document.blocks.iter().map(|block| block.start_utf8).collect();
    if let Some(text) = document.text.get(line_start..last.end_utf8) {
        starts.extend(document_layout::pdf_list_label_ranges(document, text, line_start)
            .map(|range| line_start + range.start));
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

fn translation_layout_span(document: &Document, first: &Sentence, last: &Sentence) -> Option<Range<usize>> {
    let source = document.text.get(first.start_utf8..last.end_utf8)?;
    let masked = document_layout::mask_pdf_translation_input(document, source, first.start_utf8)?;
    let body = masked.trim();
    if body.is_empty() { return None; }
    let leading = &masked[..masked.len() - masked.trim_start().len()];
    let start = first.start_utf16 + leading.encode_utf16().count();
    Some(start..start + body.encode_utf16().count())
}

fn source_block_index(document: &Document, starts: &[usize], first: &Sentence, last: &Sentence) -> Option<usize> {
    let first_letter = first.start_utf8 + first.text.find(char::is_alphanumeric)?;
    let last_letter = last.start_utf8 + last.text.rfind(char::is_alphanumeric)?;
    let block = document.blocks.partition_point(|block| block.start_utf8 <= first_letter).checked_sub(1)?;
    let boundary = starts.partition_point(|start| *start <= first_letter).checked_sub(1)?;
    let end = document.blocks[block].end_utf8.min(starts.get(boundary + 1).copied().unwrap_or(usize::MAX));
    if last_letter >= end {
        // Segmentation can attach the next printed item marker to the preceding sentence.
        // Only the marker itself may run past the layout boundary.
        let suffix = document.text.get(end..last.end_utf8)?;
        let marker = document_layout::pdf_list_label_ranges(document, suffix, end).next()?;
        if suffix[..marker.start].chars().any(char::is_alphanumeric) ||
            suffix[marker.end..].chars().any(char::is_alphanumeric) {
            return None;
        }
    }
    Some(boundary)
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
    fn pdf_translation_segments_wrapped_ocr_in_reading_coordinates() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let text = "Le bateau revient au port.";
        let mut document = store.import("reader.txt", text.as_bytes(), "fr").unwrap();
        let line = |start, end, bounds, rotation| {
            let (x, y, width, height) = ocr::reading_box(bounds, (360 - rotation) % 360);
            ocr::OcrLine {
                page: 0, text: text[start..end].into(), x, y, width, height,
                confidence: None, correction: None, words: Vec::new(), rotation: Some(rotation),
                block_start: false, source_span: Some(ocr::OcrSpan {
                    start_utf8: start, end_utf8: end, start_utf16: start, end_utf16: end,
                }),
            }
        };
        let wrap = (0.1, 0.14, 0.32, 0.03);
        let next_column = (0.6, 0.14, 0.3, 0.03);
        for angle in [0, 90, 180, 270] {
            let first = line(0, 9, (0.1, 0.10, 0.4, 0.03), angle);
            let second = line(10, text.len(), wrap, angle);
            document.ocr_lines = Some(vec![first.clone(), second]);
            assert!(super::split_ocr_sentences(&document, 0..1, Some(0), "uk").is_none(), "{angle}");

            document.ocr_lines = Some(vec![first.clone(), line(10, text.len(), next_column, angle)]);
            assert!(super::split_ocr_sentences(&document, 0..1, Some(0), "uk").is_some(), "{angle}");

            document.ocr_lines = Some(vec![first, line(10, text.len(), wrap, (angle + 180) % 360)]);
            let split = super::split_ocr_sentences(&document, 0..1, Some(0), "uk").unwrap();
            assert_eq!(split.iter().map(|part| part.text.as_str()).collect::<Vec<_>>(), ["Le bateau", "revient au port."]);
        }
        assert_eq!(document.sentences[0].text, text);
    }

    #[test]
    fn pdf_refresh_keeps_every_fragment_of_the_selected_sentence_on_the_current_page() {
        let original = [(20..31, "before".to_owned()), (32..70, "selected".to_owned()),
            (71..91, "after".to_owned())];
        let offsets = [(20, 31), (32, 48), (49, 59), (60, 70), (71, 91)];
        assert_eq!(pdf_refresh_sentence(&original, 54, 0, Some((20, 91))), Some(1));
        assert_eq!(pdf_refresh_range(&original[1].0, &offsets, 0), Some(1..4));

        let detached = [(12, 28), (29, 39), (40, 50), (51, 71)];
        assert_eq!(pdf_refresh_sentence(&original[1..], 30, 20, None), Some(0));
        assert_eq!(pdf_refresh_range(&original[1].0, &detached, 20), Some(0..3));

        let next_page = [(49, 59), (60, 70), (71, 91)];
        assert_eq!(pdf_refresh_sentence(&original[1..], 54, 0, Some((49, 91))), Some(0));
        assert_eq!(pdf_refresh_range(&original[1].0, &next_page, 0), Some(0..2));
        assert_eq!(pdf_refresh_sentence(&original[1..], 32, 0, Some((49, 91))), None);
    }

    #[test]
    fn cover_translation_separates_labels_and_keeps_wrapped_titles_together() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let line = |text: &str, x, y, width, height| ocr::OcrLine {
            rotation: None,
            page: 0, text: text.into(), x, y, width, height,
            confidence: None, correction: None, block_start: false, words: Vec::new(), source_span: None,
        };
        let document = store.import_scanned_pdf("cover.pdf", b"%PDF-1.4\n", "fr", vec![
            line("Lire en français", 0.17, 0.04, 0.25, 0.02),
            line("Collection", 0.08, 0.06, 0.03, 0.09),
            line("Le Secret de", 0.49, 0.15, 0.43, 0.04),
            line("la maison", 0.46, 0.21, 0.46, 0.04),
        ], 1).unwrap();
        let rows = store.pdf_page_translation(&document.id, 0, "uk", true).unwrap();
        assert_eq!(rows.iter().map(|row| row.source_text.as_deref()).collect::<Vec<_>>(),
            [Some("Lire en français"), Some("Collection"), Some("Le Secret de\nla maison")]);
        for row in rows {
            assert!(document.sentences.iter().any(|sentence| sentence.id == row.source_sentence_id
                && sentence.start_utf16 <= row.start_utf16 && row.end_utf16 <= sentence.end_utf16));
        }
    }

    #[test]
    fn pdf_overlay_excludes_the_printed_number_but_keeps_the_sentence_anchor() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let line = ocr::OcrLine {
            rotation: None, page: 0, text: "11. préparez votre essai.".into(),
            x: 0.1, y: 0.2, width: 0.42, height: 0.02, confidence: None,
            correction: None, block_start: false, source_span: None,
            words: [("11.", 0, 0.1, 0.05), ("préparez", 4, 0.19, 0.12),
                ("votre", 13, 0.32, 0.08), ("essai.", 19, 0.42, 0.10)]
                .into_iter().map(|(text, start, x, width)| ocr::OcrWord {
                    text: text.into(), x, y: 0.2, width, height: 0.02,
                    raw_start_utf16: Some(start), source_span: None,
                }).collect(),
        };
        let document = store.import_scanned_pdf("exercise.pdf", b"%PDF-1.4\n", "fr", vec![line], 1).unwrap();
        let rows = store.pdf_page_translation(&document.id, 0, "uk", true).unwrap();
        let [row] = rows.as_slice() else { panic!("expected one numbered exercise"); };
        assert_eq!(row.source_sentence_id, document.sentences[0].id);
        assert_eq!(row.start_utf16, 0);
        assert_eq!(row.layout_start_utf16, Some(4));
        assert_eq!(row.layout_end_utf16, None);

        let spans = serde_json::from_value::<Vec<crate::pdf_translation_layout::SourceRow>>(serde_json::json!([{
            "startUtf16": row.layout_start_utf16.unwrap(), "endUtf16": row.end_utf16,
            "blockIndex": row.source_block_index, "pending": false,
        }])).unwrap();
        let styles = serde_json::from_value::<Vec<crate::pdf_translation_layout::SourceStyleAnchor>>(serde_json::json!([{
            "startUtf16": 0, "style": {"weight": 700, "italic": false, "maskSafe": false, "background": [1., 1., 1., 1.]},
        }])).unwrap();
        let layout = crate::pdf_translation_layout::project(&spans, &document.ocr_lines.unwrap(), 0, &styles, true).unwrap();
        let layout = serde_json::to_value(layout).unwrap();
        assert_eq!(layout["overflow"], serde_json::json!([]));
        assert_eq!(layout["regions"][0]["bounds"]["x"].as_f64(), Some(0.19));
    }

    #[test]
    fn scanned_translation_rows_keep_heading_body_and_caption_blocks() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let line = |text: &str, y, height| ocr::OcrLine {
            rotation: None,
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
    fn projected_pdf_translation_continues_ocr_body_before_a_trailing_note() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let source = root.path().join("mixed.pdf");
        std::fs::write(&source, b"%PDF-1.4\n").unwrap();
        let prepared = file_originals::prepare_pdf_import(store.pdf_original_root(), "mixed.pdf", &source, "fr",
            vec![String::new(), "naire depuis 1815.".into()], vec![]).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let line = |text: &str, y, height| ocr::OcrLine {
            page: 0, rotation: None, block_start: false, text: text.into(), x: 0.1,
            y, width: 0.8, height, confidence: None, correction: None, words: vec![], source_span: None,
        };
        pdf_detached_pages::write_ocr(&store.connection, &document.id, 0, "fr", &[
            line("Le bateau arrive.", 0.10, 0.04),
            line("La mer est calme.", 0.15, 0.04),
            line("Depuis le quai¹, un voyage extraordi-", 0.20, 0.04),
            line("1 Quai : bord du port.", 0.80, 0.02),
        ], 2).unwrap();
        let cached = store.detached_pdf_sentences(&document.id, 0).unwrap().unwrap();
        let body = cached.sentences.iter().find(|sentence| sentence.text.contains("extraordi-")).unwrap();
        let expected_id = body.id.clone();
        let projection = store.project_pdf_translation_page(&document.id, 0, "uk", cached.text,
            cached.sentences, cached.language).unwrap();
        let index = projection.document.sentences.iter().position(|sentence| sentence.id == expected_id).unwrap();
        let (text, ids) = sentence_source(&store, &projection.document, index, "uk", PageBoundaries::Projected).unwrap();
        assert_eq!(text, "Depuis le quai , un voyage extraordinaire depuis 1815.");
        assert_eq!(ids, [expected_id]);
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
            rotation: None,
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
    let cached = routes.cached_active(store, document, selected, target)?;
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
                aligned.route.direct_model_revision().map(str::to_owned),
            )
            .map(|value| projection.localize(value)));
        }
    }
    let standalone = crate::translation::normalize_pdf_translation_input(
        document, &original.text, original.start_utf8, original.start_utf16,
    );
    let standalone = crate::translation::prepare_translation_input(&document.language, target, standalone);
    Ok(routes
        .cached_sentence(store, &standalone, target)?
        .and_then(|aligned| Some(DetachedPdfTranslation {
            text: aligned.segments.into_iter().next()?,
            pivot_language: aligned.route.pivot_language().map(str::to_owned),
            direct_model_revision: aligned.route.direct_model_revision().map(str::to_owned),
            source_text: None,
            start_position: local_position,
            legacy_group: false,
        })))
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
    let mut result = machine_result(
        document,
        selected,
        &unit.sentence_ids,
        unit.translation?,
        unit.pivot_language,
        unit.direct_model_revision,
    )?;
    result.legacy_group = paragraph.legacy_group;
    Some(result)
}

fn machine_result(
    document: &Document,
    selected: usize,
    ids: &[String],
    text: String,
    pivot_language: Option<String>,
    direct_model_revision: Option<String>,
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
        direct_model_revision,
        source_text,
        start_position,
        legacy_group: false,
    })
}
