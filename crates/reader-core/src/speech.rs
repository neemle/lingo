use crate::{pdf_detached_pages::word_break, pdf_reading_position, resources::InferencePriority, tts::SpeechAudio, Document, Result, Sentence, Store};
use rusqlite::OptionalExtension;
use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub enum ModelSpeechEngine {
    PiperUkrainian,
    Supertonic,
}

impl ModelSpeechEngine {
    pub fn from_id(engine: &str) -> Option<Self> {
        match engine {
            "piper-uk" => Some(Self::PiperUkrainian),
            "supertonic-3" => Some(Self::Supertonic),
            _ => None,
        }
    }
}

pub(crate) struct SpeechSource {
    pub language: String,
    pub text: String,
}

pub(crate) struct RenderedSpeech {
    pub audio: SpeechAudio,
    pub cached: bool,
}

/// Prepared speech engine adapter. The job owns the text and model selection.
pub(crate) trait SpeechAdapter {
    fn render(self: Box<Self>, priority: InferencePriority, canceled: Option<&dyn Fn() -> bool>) -> Result<RenderedSpeech>;
}

pub struct PreparedSpeech {
    sentence_id: String,
    adapter: Box<dyn SpeechAdapter>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechPrefetch {
    pub sentence_id: String,
    pub cached: bool,
}

impl PreparedSpeech {
    pub fn run(self) -> Result<SpeechAudio> {
        self.run_cancellable(None)
    }

    pub fn run_cancellable(self, canceled: Option<&dyn Fn() -> bool>) -> Result<SpeechAudio> {
        self.adapter.render(InferencePriority::Foreground, canceled).map(|result| result.audio)
    }

    pub fn prefetch(self) -> Result<SpeechPrefetch> {
        self.prefetch_cancellable(None)
    }

    pub fn prefetch_cancellable(self, canceled: Option<&dyn Fn() -> bool>) -> Result<SpeechPrefetch> {
        let result = self.adapter.render(InferencePriority::Background, canceled)?;
        Ok(SpeechPrefetch { sentence_id: self.sentence_id, cached: result.cached })
    }
}

impl Store {
    pub fn prepare_model_speech(
        &self, document_id: &str, sentence_id: &str,
        engine: ModelSpeechEngine, speaker: u32, rate: f32,
    ) -> Result<PreparedSpeech> {
        self.prepare_speech_at(document_id, sentence_id, engine, speaker, rate, false)?
            .ok_or_else(|| "Речення для озвучення не знайдено".into())
    }

    pub fn prepare_speech_text(
        &self, text: &str, language: &str, engine: ModelSpeechEngine, speaker: u32, rate: f32,
    ) -> Result<PreparedSpeech> {
        crate::valid_language(language)?;
        if text.len() > 4096 || text.trim().is_empty() ||
            text.chars().any(|ch| ch.is_control() && !matches!(ch, '\r' | '\n' | '\t'))
        {
            return Err("Текст для озвучення порожній, задовгий або містить керівні символи.".into());
        }
        let source = SpeechSource { language: language.to_owned(), text: text.trim().to_owned() };
        self.prepare_speech_source(String::new(), source, engine, speaker, rate)
    }

    pub fn prepare_speech_prefetch(
        &self, document_id: &str, current_sentence_id: &str,
        engine: ModelSpeechEngine, speaker: u32, rate: f32,
    ) -> Result<Option<PreparedSpeech>> {
        self.prepare_speech_at(document_id, current_sentence_id, engine, speaker, rate, true)
    }

    fn prepare_speech_at(
        &self, document_id: &str, sentence_id: &str,
        engine: ModelSpeechEngine, speaker: u32, rate: f32, next: bool,
    ) -> Result<Option<PreparedSpeech>> {
        let Some((sentence_id, source)) = self.speech_source(document_id, sentence_id, next)? else {
            return Ok(None);
        };
        self.prepare_speech_source(sentence_id, source, engine, speaker, rate).map(Some)
    }

    pub fn prepare_detached_pdf_speech(
        &self, document_id: &str, address: (u16, &str),
        engine: ModelSpeechEngine, speaker: u32, rate: f32, next: bool,
    ) -> Result<Option<PreparedSpeech>> {
        let (page, sentence_id) = address;
        let request = self.detached_pdf_speech_source(document_id, page, sentence_id, next)?;
        let request = match (request, next, page.checked_add(1)) {
            (None, true, Some(next_page)) => self.first_speech_on_pdf_page(document_id, next_page, None)?,
            (request, _, _) => request,
        };
        let Some((sentence_id, source)) = request else {
            return Ok(None);
        };
        self.prepare_speech_source(sentence_id, source, engine, speaker, rate).map(Some)
    }

    fn prepare_speech_source(
        &self, sentence_id: String, source: SpeechSource, engine: ModelSpeechEngine, speaker: u32, rate: f32,
    ) -> Result<PreparedSpeech> {
        let adapter: Box<dyn SpeechAdapter> = match engine {
            ModelSpeechEngine::PiperUkrainian => Box::new(self.prepare_piper_source(source, speaker, rate)?),
            ModelSpeechEngine::Supertonic => Box::new(self.prepare_supertonic_source(source, speaker, rate)?),
        };
        Ok(PreparedSpeech { sentence_id, adapter })
    }

    pub(crate) fn speech_source(
        &self, document_id: &str, sentence_id: &str, next: bool,
    ) -> Result<Option<(String, SpeechSource)>> {
        let document = self.speech_document(document_id)?;
        let index = document.sentences.iter().position(|sentence| sentence.id == sentence_id)
            .ok_or("Речення для озвучення не знайдено")?;
        let candidate = if next {
            document.sentences.iter().skip(index + 1).find(|sentence|
                !crate::document_layout::sentence_in_footnote(&document.blocks, sentence)
                    && has_narration_text(document.speech_text(sentence)))
        } else {
            document.sentences.get(index)
        };
        if next && document.format == "pdf" {
            let sentence = &document.sentences[index];
            let page = self.pdf_reading_page(document_id)?
                .filter(|&page| pdf_reading_position::page_bounds(&document, page)
                    .is_some_and(|(start, end)| sentence.start_utf16 < end && sentence.end_utf16 > start))
                .or_else(|| {
                    document.pdf_page_starts.as_ref()?.iter().enumerate().rev()
                        .find(|(_, start)| start.is_some_and(|start| start <= sentence.start_utf16))
                        .and_then(|(page, _)| u16::try_from(page).ok())
                });
            if let Some((page, (_, end))) = page.and_then(|page|
                pdf_reading_position::page_bounds(&document, page).map(|bounds| (page, bounds)))
            {
                if candidate.is_none_or(|candidate| candidate.start_utf16 >= end) {
                    let Some(next_page) = page.checked_add(1) else { return Ok(None); };
                    return self.first_speech_on_pdf_page(document_id, next_page, Some((&document, index + 1)));
                }
            }
        }
        candidate.map(|sentence| self.canonical_speech(&document, sentence)).transpose()
            .map(|speech| speech.filter(|(_, source)| has_narration_text(&source.text)))
    }

    fn first_speech_on_pdf_page(
        &self, document_id: &str, page: u16, canonical: Option<(&Document, usize)>,
    ) -> Result<Option<(String, SpeechSource)>> {
        if let Some((document, start)) = canonical {
            if pdf_reading_position::page_bounds(document, page).is_some() {
                return self.first_canonical_on_page(document, page, start);
            }
        }
        let detached = self.first_detached_pdf_speech_source(document_id, page)?;
        if detached.is_some() || canonical.is_some() {
            return Ok(detached);
        }
        let document = self.speech_document(document_id)?;
        self.first_canonical_on_page(&document, page, 0)
    }

    pub(crate) fn speech_document(&self, document_id: &str) -> Result<Document> {
        let body: String = self.connection.query_row(
            "SELECT body FROM documents WHERE id=?1", [document_id], |row| row.get(0),
        ).optional().map_err(|error| error.to_string())?
            .ok_or("Документ для озвучення не знайдено")?;
        let mut document = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        crate::document_layout::reflow_ocr_paragraphs(&mut document);
        Ok(document)
    }
    pub fn speech_narration(&self, document_id: &str, sentence_id: &str) -> Result<Option<String>> {
        let document = self.speech_document(document_id)?;
        let Some(sentence) = document.sentences.iter().find(|sentence| sentence.id == sentence_id) else {
            return Ok(None);
        };
        self.speech_narration_for_sentence(&document, sentence)
    }

    pub(crate) fn speech_narration_for_sentence(&self, document: &Document, sentence: &Sentence) -> Result<Option<String>> {
        if crate::document_layout::sentence_in_footnote(&document.blocks, sentence) {
            return Ok(Some(String::new()));
        }
        word_break::canonical_narration(&self.connection, document, sentence)
    }

    pub fn speech_pronunciation(&self, document_id: &str, sentence_id: &str) -> Result<Option<String>> {
        let document = self.speech_document(document_id)?;
        let Some(sentence) = document.sentences.iter().find(|sentence| sentence.id == sentence_id) else {
            return Ok(None);
        };
        if document.speech_text_overrides.contains_key(sentence_id) ||
            crate::document_layout::sentence_in_footnote(&document.blocks, sentence)
        {
            return Ok(None);
        }
        let narration = word_break::canonical_narration(&self.connection, &document, sentence)?;
        Ok(pdf_narration(&document, sentence, narration.as_deref().unwrap_or(&sentence.text)))
    }

    fn first_canonical_on_page(
        &self, document: &Document, page: u16, start: usize,
    ) -> Result<Option<(String, SpeechSource)>> {
        let Some((left, right)) = pdf_reading_position::page_bounds(document, page) else { return Ok(None); };
        let sentences = &document.sentences[start..];
        let index = sentences.partition_point(|sentence| sentence.end_utf16 <= left);
        for sentence in sentences[index..].iter().take_while(|sentence| sentence.start_utf16 < right) {
            let speech = self.canonical_speech(document, sentence)?;
            if has_narration_text(&speech.1.text) {
                return Ok(Some(speech));
            }
        }
        Ok(None)
    }

    fn canonical_speech(&self, document: &Document, sentence: &Sentence) -> Result<(String, SpeechSource)> {
        let text = if crate::document_layout::sentence_in_footnote(&document.blocks, sentence) {
            String::new()
        } else {
            let narration = word_break::canonical_narration(&self.connection, document, sentence)?
                .unwrap_or_else(|| document.speech_text(sentence).to_owned());
            if document.speech_text_overrides.contains_key(&sentence.id) {
                narration
            } else {
                pdf_narration(document, sentence, &narration).unwrap_or(narration)
            }
        };
        Ok((sentence.id.clone(), SpeechSource { language: document.language.clone(), text }))
    }
}

pub(crate) fn has_narration_text(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

pub(crate) fn same_word_source(source: &str, spoken: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    use unicode_segmentation::UnicodeSegmentation;

    fn without_stress(element: &str) -> &str {
        if element.starts_with(|ch| matches!(ch, '\u{0400}'..='\u{052f}')) {
            element.trim_end_matches('\u{0301}')
        } else {
            element
        }
    }

    let mut spoken = spoken.graphemes(true);
    source.graphemes(true).all(|original| {
        spoken.next().is_some_and(|element| without_stress(original).nfc().eq(without_stress(element).nfc()))
    }) && spoken.next().is_none()
}


pub(crate) fn pdf_narration(document: &Document, sentence: &Sentence, text: &str) -> Option<String> {
    let masked = without_pdf_note_markers(document, sentence, text);
    let narration = masked.as_deref().unwrap_or(text);
    if (document.format == "pdf" || document.ocr_lines.is_some()) && narration.contains('\n') {
        Some(crate::translation::normalize_ocr_input(document, narration, sentence.start_utf16))
    } else {
        masked
    }
}

/// Blanks confirmed references in the reading input while preserving UTF-16 offsets.
pub(crate) fn without_pdf_note_markers(document: &Document, sentence: &Sentence, text: &str) -> Option<String> {
    let start = text.find(&sentence.text)?;
    let cleaned = crate::document_layout::mask_pdf_non_text(document, &sentence.text, sentence.start_utf8)?;
    let mut out = text.to_owned();
    out.replace_range(start..start + sentence.text.len(), &cleaned);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_word_checkpoint_survives_restart_with_a_stress_edit() {
        use sha2::{Digest, Sha256};

        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("reader.db");
        let original = root.path().join("scan.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut store = Store::open(&database).unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "uk", vec![String::new()], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        crate::pdf_detached_pages::write_text_layer(&store.connection, &document.id, 0,
            "Біля вікна росте дерево.").unwrap();
        let sentence = store.detached_pdf_sentence(&document.id, 0, None).unwrap().unwrap().sentence;
        let edit = |store: &mut Store, text: &str| {
            store.update_detached_pdf_annotation(&document.id, 0, &sentence.id,
                crate::DetachedPdfAnnotationChange::SpeechText { text: text.into() }).unwrap();
        };
        let save = |store: &mut Store, spoken: &str| {
            store.set_detached_pdf_speech_resume(&document.id, 0, &sentence.id, Some(crate::SpeechResume {
                sentence_id: sentence.id.clone(), text_hash: format!("{:x}", Sha256::digest(spoken.as_bytes())),
                offset_utf16: 5, model: None, boundary_text: None,
            }))
        };
        let stressed = "Біля вікна́ росте дерево.";
        edit(&mut store, stressed);
        save(&mut store, stressed).unwrap();
        drop(store);

        let mut store = Store::open(&database).unwrap();
        let current = store.detached_pdf_sentence(&document.id, 0, None).unwrap().unwrap();
        assert_eq!(current.speech_resume.unwrap().offset_utf16, 5);
        let rewritten = "Біля стіни росте дерево.";
        edit(&mut store, rewritten);
        assert!(save(&mut store, rewritten).is_err());
        assert!(store.detached_pdf_sentence(&document.id, 0, None).unwrap().unwrap().speech_resume.is_none());
    }

    #[test]
    fn narration_skips_detached_dialogue_dashes_inside_and_across_pages() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("book.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "book.pdf", &original, "fr",
            vec!["CHAPITRE 1\n–\n\nBravo, mon jeune ami !".into(), String::new()], vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        let dash = document.sentences.iter().position(|sentence| sentence.text == "–").unwrap();
        assert!(store.speech_source(&document.id, &document.sentences[dash].id, false).unwrap().is_none());
        let (_, spoken) = store.speech_source(&document.id, &document.sentences[dash - 1].id, true)
            .unwrap().unwrap();
        assert_eq!(spoken.text, "Bravo, mon jeune ami !");

        crate::pdf_detached_pages::write_text_layer(&store.connection, &document.id, 1,
            "–\n\nOui, monsieur.").unwrap();
        let (_, spoken) = store.speech_source(&document.id, &document.sentences[dash + 1].id, true)
            .unwrap().unwrap();
        assert_eq!(spoken.text, "Oui, monsieur.");
    }

    #[test]
    fn narration_joins_pdf_wraps_but_keeps_french_inversion() {
        let input = "Les marchan-\ndises sont ici. Est-\nelle au port ?";
        let document = crate::document_layout::detached_pdf_document(input.into(), "fr", None);
        let sentences = crate::segment_pdf_with_titles(input, "pdf");
        let narrate = |sentence: &Sentence| pdf_narration(&document, sentence, &sentence.text).unwrap();
        assert_eq!(narrate(&sentences[0]), "Les marchandises sont ici.");
        assert_eq!(narrate(&sentences[1]), "Est-elle au port ?");
    }

    #[test]
    fn only_same_page_footnote_markers_are_silent_with_original_offsets() {
        let source = "Le marin voit les mâts1 et le quai², au niveau B2 en 1815.";
        let text = format!("{source}\n1 Mât : pièce du navire.\n2 Quai : bord du port.");
        let document = crate::document_layout::detached_pdf_document(text, "fr", None);
        let sentence = crate::segment_pdf_with_titles(source, "pdf").remove(0);
        let pronunciation = without_pdf_note_markers(&document, &sentence, source).unwrap();
        assert_eq!(pronunciation, "Le marin voit les mâts  et le quai , au niveau B2 en 1815.");
        assert_eq!(pronunciation.encode_utf16().count(), source.encode_utf16().count());
        let unrelated = "Le marin voit B2 et les mâts3.";
        assert!(without_pdf_note_markers(&document, &sentence, unrelated).is_none());
    }
}
