use crate::{resources::InferencePriority, tts::SpeechAudio, Document, Result, Store};
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
        let Some((sentence_id, source)) = self.detached_pdf_speech_source(document_id, page, sentence_id, next)? else {
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
        let body: String = self.connection.query_row(
            "SELECT body FROM documents WHERE id=?1", [document_id], |row| row.get(0),
        ).optional().map_err(|error| error.to_string())?
            .ok_or("Документ для озвучення не знайдено")?;
        let document: Document = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        let index = document.sentences.iter().position(|sentence| sentence.id == sentence_id)
            .ok_or("Речення для озвучення не знайдено")?;
        Ok(document.sentences.get(index + usize::from(next)).map(|sentence| {
            (sentence.id.clone(), SpeechSource {
                language: document.language.clone(),
                text: document.speech_text(sentence).to_owned(),
            })
        }))
    }
}
