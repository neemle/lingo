use super::{annotations, CachedSentences};
use crate::translation::{CacheRoutes, ParagraphTranslation};
use crate::{document_layout, Document, Result, Sentence, Store};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetachedPdfTranslation {
    pub(super) text: String,
    pub(super) pivot_language: Option<String>,
    pub(super) source_text: Option<String>,
    pub(super) start_position: usize,
}

impl Store {
    pub fn detached_pdf_translation(
        &mut self,
        id: &str,
        page: u16,
        sentence_id: &str,
        target: &str,
        cached_only: bool,
        background: bool,
    ) -> Result<Option<DetachedPdfTranslation>> {
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
        let reviewed = annotations::reviewed_for_page(&self.connection, id, page, target)?;
        let document = page_document(text, sentences, language, target, reviewed);
        if cached_only {
            let routes = self.detached_translation_routes(&document.language, target)?;
            return cached_page_translation(self, &routes, &document, selected, target);
        }
        let translated =
            self.translate_document_paragraph(&document, selected, target, background)?;
        Ok(active_translation(&document, selected, translated))
    }
}

pub(super) fn page_document(
    text: String,
    sentences: Vec<Sentence>,
    language: String,
    target: &str,
    mut reviewed: BTreeMap<usize, String>,
) -> Document {
    let layout = document_layout::from_transcript(text, None);
    let mut reviewed_translations = BTreeMap::new();
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
    Document {
        text: layout.text,
        blocks: layout.blocks,
        sentences,
        language,
        format: "pdf".into(),
        reviewed_translations,
        ..Document::default()
    }
}

pub(super) fn cached_page_translation(
    store: &Store,
    routes: &CacheRoutes,
    document: &Document,
    selected: usize,
    target: &str,
) -> Result<Option<DetachedPdfTranslation>> {
    let original = &document.sentences[selected];
    if document
        .reviewed_translations
        .get(target)
        .is_some_and(|items| items.contains_key(&original.id))
    {
        return Ok(None);
    }
    let cached = routes.cached_active(store, document, selected, target)?;
    if let Some(active) = active_translation(document, selected, cached.paragraph) {
        return Ok(Some(active));
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
            ));
        }
    }
    Ok(routes
        .cached_standalone(store, &original.text, target)?
        .map(|(text, pivot_language)| DetachedPdfTranslation {
            text,
            pivot_language,
            source_text: None,
            start_position: selected,
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
