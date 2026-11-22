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
    fn render(self: Box<Self>, priority: InferencePriority) -> Result<RenderedSpeech>;
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
        self.adapter.render(InferencePriority::Foreground).map(|result| result.audio)
    }

    pub fn prefetch(self) -> Result<SpeechPrefetch> {
        let result = self.adapter.render(InferencePriority::Background)?;
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
                !crate::document_layout::sentence_in_footnote(&document.blocks, sentence))
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
            .map(|speech| speech.filter(|(_, source)| !source.text.is_empty()))
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

    fn speech_document(&self, document_id: &str) -> Result<Document> {
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
        if crate::document_layout::sentence_in_footnote(&document.blocks, sentence) {
            return Ok(Some(String::new()));
        }
        word_break::canonical_narration(&self.connection, &document, sentence)
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
        Ok(without_pdf_note_markers(&document, sentence, narration.as_deref().unwrap_or(&sentence.text)))
    }

    fn first_canonical_on_page(
        &self, document: &Document, page: u16, start: usize,
    ) -> Result<Option<(String, SpeechSource)>> {
        let Some((left, right)) = pdf_reading_position::page_bounds(document, page) else { return Ok(None); };
        let sentences = &document.sentences[start..];
        let index = sentences.partition_point(|sentence| sentence.end_utf16 <= left);
        for sentence in sentences[index..].iter().take_while(|sentence| sentence.start_utf16 < right) {
            let speech = self.canonical_speech(document, sentence)?;
            if !speech.1.text.is_empty() {
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
                without_pdf_note_markers(document, sentence, &narration).unwrap_or(narration)
            }
        };
        Ok((sentence.id.clone(), SpeechSource { language: document.language.clone(), text }))
    }
}


/// Blanks confirmed references in the reading input while preserving UTF-16 offsets.
pub(crate) fn without_pdf_note_markers(document: &Document, sentence: &Sentence, text: &str) -> Option<String> {
    if document.format != "pdf" || !text.chars().any(char::is_numeric) {
        return None;
    }
    let block = document.blocks.partition_point(|block| block.start_utf8 <= sentence.start_utf8);
    let page = document.blocks.get(block.checked_sub(1)?)?.page?;
    let mut numbers = document.blocks.iter().filter(|block| block.footnote && block.page == Some(page))
        .filter_map(|block| {
            let note = document.text.get(block.start_utf8..block.end_utf8)?;
            note.split_whitespace().next()?.parse::<u32>().ok()
        }).peekable();
    numbers.peek()?;
    let numbers: Vec<_> = numbers.collect();

    let mut clean = String::with_capacity(text.len());
    let mut previous_letter_count = 0;
    let mut lower_word = true;
    let mut chars = text.chars().peekable();
    let mut changed = false;
    while let Some(ch) = chars.next() {
        if let Some((digit, superscript)) = note_digit(ch) {
            let mut run = String::from(ch);
            let mut number = digit;
            while let Some(next) = chars.peek().and_then(|&ch| note_digit(ch).filter(|&(_, sup)| sup == superscript)) {
                run.push(chars.next().expect("peeked digit"));
                number = number.saturating_mul(10).saturating_add(next.0);
            }
            let follows_word = previous_letter_count > 0 &&
                (superscript || previous_letter_count >= 3 && lower_word);
            if follows_word && chars.peek().is_none_or(|ch| !ch.is_alphanumeric()) && numbers.contains(&number) {
                clean.extend(std::iter::repeat_n(' ', run.encode_utf16().count()));
                changed = true;
            } else {
                clean.push_str(&run);
            }
        } else {
            clean.push(ch);
        }
        if ch.is_alphabetic() {
            previous_letter_count += 1;
            lower_word &= ch.is_lowercase();
        } else {
            previous_letter_count = 0;
            lower_word = true;
        }
    }
    changed.then_some(clean)
}

fn note_digit(ch: char) -> Option<(u32, bool)> {
    if let Some(digit) = ch.to_digit(10) {
        return Some((digit, false));
    }
    "⁰¹²³⁴⁵⁶⁷⁸⁹".chars().position(|digit| digit == ch).map(|index| (index as u32, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_same_page_footnote_markers_are_silent_with_original_offsets() {
        let source = "Le marin voit les mâts1 et le quai², au niveau B2 en 1815.";
        let text = format!("{source}\n1 Mât : pièce du navire.\n2 Quai : bord du port.");
        let document = crate::document_layout::detached_pdf_document(text, None);
        let sentence = crate::segment_pdf_with_titles(source, "pdf").remove(0);
        let pronunciation = without_pdf_note_markers(&document, &sentence, source).unwrap();
        assert_eq!(pronunciation, "Le marin voit les mâts  et le quai , au niveau B2 en 1815.");
        assert_eq!(pronunciation.encode_utf16().count(), source.encode_utf16().count());
        let unrelated = "Le marin voit B2 et les mâts3.";
        assert!(without_pdf_note_markers(&document, &sentence, unrelated).is_none());
    }
}
