use super::database_error;
use crate::{Result, Sentence, SpeechResume, Store};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ArchivedSpeechResume {
    position: usize,
    resume: SpeechResume,
}

impl ArchivedSpeechResume {
    pub(super) fn for_sentences(
        &self,
        sentences: &[Sentence],
        language: &str,
    ) -> Option<SpeechResume> {
        let sentence = sentences.get(self.position)?;
        if !valid(&self.resume, sentence, language) {
            return None;
        }
        let mut resume = self.resume.clone();
        resume.sentence_id.clone_from(&sentence.id);
        Some(resume)
    }
}

impl Store {
    pub fn set_detached_pdf_speech_resume(
        &mut self,
        id: &str,
        page: u16,
        sentence_id: &str,
        resume: Option<SpeechResume>,
    ) -> Result<()> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        if let Some(resume) = resume {
            let current = self
                .detached_pdf_sentences(id, page)?
                .ok_or("Текст сторінки PDF недоступний.")?;
            let position = current.saved.min(current.sentences.len() - 1);
            let sentence = &current.sentences[position];
            if sentence.id != sentence_id
                || resume.sentence_id != sentence_id
                || !valid(&resume, sentence, &current.language)
            {
                return Err("Позиція озвучення не відповідає поточному реченню PDF.".into());
            }
            write(&tx, id, page, &ArchivedSpeechResume { position, resume })?;
        } else {
            tx.execute(
                "DELETE FROM pdf_detached_speech_resume WHERE document_id=?1 AND page=?2
                 AND json_extract(body, '$.resume.sentenceId')=?3",
                params![id, page, sentence_id],
            )
            .map_err(database_error)?;
        }
        tx.commit().map_err(database_error)
    }
}

pub(super) fn read(db: &Connection, id: &str, page: u16) -> Result<Option<ArchivedSpeechResume>> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM pdf_detached_speech_resume WHERE document_id=?1 AND page=?2",
            params![id, page],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    body.as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(database_error)
}

pub(super) fn write(
    db: &Connection,
    id: &str,
    page: u16,
    checkpoint: &ArchivedSpeechResume,
) -> Result<()> {
    let body = serde_json::to_string(checkpoint).map_err(database_error)?;
    db.execute(
        "INSERT INTO pdf_detached_speech_resume VALUES (?1,?2,?3)
         ON CONFLICT(document_id,page) DO UPDATE SET body=excluded.body WHERE body!=excluded.body",
        params![id, page, body],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(super) fn valid(resume: &SpeechResume, sentence: &Sentence, language: &str) -> bool {
    resume
        .model
        .as_ref()
        .is_none_or(|model| model.language == language && resume.offset_utf16 == 0)
        && resume.matches(&sentence.text, &sentence.text)
}

pub(super) fn rebind(
    checkpoint: ArchivedSpeechResume,
    sentences: &[Sentence],
    language: &str,
) -> Option<ArchivedSpeechResume> {
    let resume = checkpoint.for_sentences(sentences, language)?;
    Some(ArchivedSpeechResume {
        resume,
        ..checkpoint
    })
}

pub(super) fn matches_original(checkpoint: &ArchivedSpeechResume, sentences: &[Sentence]) -> bool {
    sentences
        .get(checkpoint.position)
        .is_some_and(|sentence| sentence.id == checkpoint.resume.sentence_id)
}
