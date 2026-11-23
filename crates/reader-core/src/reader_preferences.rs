use crate::{valid_language, Result, Store};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReaderLayout {
    Automatic,
    Columns,
    Stacked,
    Source,
    Translation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PdfFit {
    PageWidth,
    TextWidth,
    WholePage,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ReaderPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<ReaderLayout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_translation_layout: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_fit_width: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_fit: Option<PdfFit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_show_original: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_translate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_translation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hy_auto_translation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuous_speech: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_rate_percent: Option<u16>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub translation_targets: BTreeMap<String, Option<String>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub speech_voices: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "field", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReaderPreferenceChange {
    Layout { value: ReaderLayout },
    PdfTranslationLayout { value: bool },
    PdfFitWidth { value: bool },
    PdfFit { value: PdfFit },
    PdfShowOriginal { value: bool },
    AutoTranslate { value: bool },
    BackgroundTranslation { value: bool },
    HyAutoTranslation { value: bool },
    ContinuousSpeech { value: bool },
    SpeechRatePercent { value: u16 },
    SpeechPlayback {
        #[serde(rename = "ratePercent")]
        rate_percent: u16,
        continuous: bool,
    },
    TranslationTarget { source: String, target: Option<String> },
    SpeechVoice {
        language: String,
        #[serde(rename = "voiceId")]
        voice_id: Option<String>,
    },
}

impl ReaderPreferences {
    pub(crate) fn is_empty(&self) -> bool {
        self.layout.is_none()
            && self.pdf_translation_layout.is_none()
            && self.pdf_fit_width.is_none()
            && self.pdf_fit.is_none()
            && self.pdf_show_original.is_none()
            && self.auto_translate.is_none()
            && self.background_translation.is_none()
            && self.hy_auto_translation.is_none()
            && self.continuous_speech.is_none()
            && self.speech_rate_percent.is_none()
            && self.translation_targets.is_empty()
            && self.speech_voices.is_empty()
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.speech_rate_percent.is_some_and(|rate| !(40..=200).contains(&rate)) {
            return Err("Швидкість озвучення має бути від 40 до 200 відсотків.".into());
        }
        for (source, target) in &self.translation_targets {
            valid_language(source)?;
            if let Some(target) = target {
                valid_language(target)?;
                if source == target {
                    return Err("Мова перекладу має відрізнятися від мови джерела.".into());
                }
            }
        }
        for (language, voice) in &self.speech_voices {
            valid_language(language)?;
            if voice.as_ref().is_some_and(|voice| {
                voice.trim().is_empty() || voice.len() > 512 || voice.chars().any(char::is_control)
            }) {
                return Err("Некоректний ідентифікатор голосу.".into());
            }
        }
        Ok(())
    }

    fn apply(&mut self, change: ReaderPreferenceChange) {
        match change {
            ReaderPreferenceChange::Layout { value } => self.layout = Some(value),
            ReaderPreferenceChange::PdfTranslationLayout { value } => self.pdf_translation_layout = Some(value),
            ReaderPreferenceChange::PdfFitWidth { value } => {
                self.pdf_fit_width = Some(value);
                self.pdf_fit = None;
            }
            ReaderPreferenceChange::PdfFit { value } => {
                self.pdf_fit = Some(value);
                self.pdf_fit_width = None;
            }
            ReaderPreferenceChange::PdfShowOriginal { value } => self.pdf_show_original = Some(value),
            ReaderPreferenceChange::AutoTranslate { value } => self.auto_translate = Some(value),
            ReaderPreferenceChange::BackgroundTranslation { value } => self.background_translation = Some(value),
            ReaderPreferenceChange::HyAutoTranslation { value } => self.hy_auto_translation = Some(value),
            ReaderPreferenceChange::ContinuousSpeech { value } => self.continuous_speech = Some(value),
            ReaderPreferenceChange::SpeechRatePercent { value } => self.speech_rate_percent = Some(value),
            ReaderPreferenceChange::SpeechPlayback { rate_percent, continuous } => {
                self.speech_rate_percent = Some(rate_percent);
                self.continuous_speech = Some(continuous);
            }
            ReaderPreferenceChange::TranslationTarget { source, target } => {
                self.translation_targets.insert(source, target);
            }
            ReaderPreferenceChange::SpeechVoice { language, voice_id } => {
                self.speech_voices.insert(language, voice_id);
            }
        }
    }
}

pub(crate) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS reader_preferences (
            id INTEGER PRIMARY KEY CHECK(id=1),
            body TEXT NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    )
    .map_err(database_error)
}

pub(crate) fn read(db: &Connection) -> Result<ReaderPreferences> {
    let body: Option<String> = db.query_row(
        "SELECT body FROM reader_preferences WHERE id=1",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(database_error)?;
    match body {
        Some(body) => serde_json::from_str(&body).map_err(database_error),
        None => Ok(ReaderPreferences::default()),
    }
}

fn write(db: &Connection, preferences: &ReaderPreferences) -> Result<()> {
    let body = serde_json::to_string(preferences).map_err(database_error)?;
    db.execute(
        "INSERT INTO reader_preferences (id,body,updated_at) VALUES (1,?1,?2)
         ON CONFLICT(id) DO UPDATE SET body=excluded.body,updated_at=excluded.updated_at
         WHERE reader_preferences.body<>excluded.body",
        params![body, crate::now()],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(crate) fn disable_background_translation(db: &Connection) -> Result<()> {
    let mut preferences = read(db)?;
    if preferences.background_translation == Some(true) {
        preferences.background_translation = Some(false);
        write(db, &preferences)?;
    }
    Ok(())
}

pub(crate) fn restore(db: &Connection, incoming: ReaderPreferences) -> Result<()> {
    if incoming.is_empty() {
        return Ok(());
    }
    incoming.validate()?;
    let mut local = read(db)?;
    local.layout = local.layout.or(incoming.layout);
    local.pdf_translation_layout = local.pdf_translation_layout.or(incoming.pdf_translation_layout);
    if local.pdf_fit.is_none() && local.pdf_fit_width.is_none() {
        local.pdf_fit = incoming.pdf_fit;
        local.pdf_fit_width = incoming.pdf_fit_width;
    }
    local.pdf_show_original = local.pdf_show_original.or(incoming.pdf_show_original);
    local.auto_translate = local.auto_translate.or(incoming.auto_translate);
    local.background_translation = local.background_translation.or(incoming.background_translation);
    local.continuous_speech = local.continuous_speech.or(incoming.continuous_speech);
    local.speech_rate_percent = local.speech_rate_percent.or(incoming.speech_rate_percent);
    for (source, target) in incoming.translation_targets {
        local.translation_targets.entry(source).or_insert(target);
    }
    for (language, voice) in incoming.speech_voices {
        local.speech_voices.entry(language).or_insert(voice);
    }
    write(db, &local)
}

impl Store {
    pub fn reader_preferences(&self) -> Result<ReaderPreferences> {
        read(&self.connection)
    }

    pub fn merge_reader_preferences(&mut self, incoming: ReaderPreferences) -> Result<ReaderPreferences> {
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        restore(&tx, incoming)?;
        let merged = read(&tx)?;
        tx.commit().map_err(database_error)?;
        Ok(merged)
    }

    pub fn update_reader_preference(&mut self, change: ReaderPreferenceChange) -> Result<ReaderPreferences> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let mut current = read(&tx)?;
        current.apply(change);
        current.validate()?;
        write(&tx, &current)?;
        tx.commit().map_err(database_error)?;
        Ok(current)
    }
}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка збереження налаштувань читача. {error}")
}
