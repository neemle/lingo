use crate::{Result, Store};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeProgress {
    pub revision: u64,
    pub direction: String,
    pub mode: u8,
    pub question: Option<SavedQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuestion {
    pub card_id: String,
    pub prompt: String,
    pub answers: Vec<String>,
    pub options: Vec<String>,
    pub revealed: bool,
    pub given_answer: Option<String>,
}

impl Default for PracticeProgress {
    fn default() -> Self {
        Self {
            revision: 0,
            direction: "forward".into(),
            mode: 0,
            question: None,
        }
    }
}

impl PracticeProgress {
    fn validate(&self) -> Result<()> {
        if !["forward", "reverse"].contains(&self.direction.as_str()) || self.mode > 2 {
            return Err("Некоректні налаштування практики.".into());
        }
        if let Some(question) = &self.question {
            if question.card_id.is_empty()
                || question.prompt.trim().is_empty()
                || question.answers.is_empty()
                || question
                    .answers
                    .iter()
                    .any(|answer| answer.trim().is_empty())
                || question.options.len() > 4
                || (self.mode != 2 && !question.options.is_empty())
                || question
                    .given_answer
                    .as_ref()
                    .is_some_and(|answer| !question.revealed || answer.trim().is_empty())
            {
                return Err("Некоректне запитання практики.".into());
            }
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<()> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("Надто багато змін практики.")?;
        Ok(())
    }
}

pub(crate) fn read(connection: &Connection) -> Result<Option<PracticeProgress>> {
    let body: Option<String> = connection
        .query_row("SELECT body FROM practice WHERE id=1", [], |row| row.get(0))
        .optional()
        .map_err(|error| error.to_string())?;
    body.as_deref()
        .map(|body| serde_json::from_str(body).map_err(|error| error.to_string()))
        .transpose()
}

fn write(connection: &Connection, progress: &PracticeProgress) -> Result<()> {
    let body = serde_json::to_string(progress).map_err(|error| error.to_string())?;
    connection.execute("INSERT INTO practice(id,body) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body", [body])
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn assert_revision(expected: u64, actual: u64) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err("Практику вже оновлено. Відкрийте поточне запитання.".into())
    }
}

impl Store {
    pub fn practice(&self) -> Result<PracticeProgress> {
        Ok(read(&self.connection)?.unwrap_or_default())
    }

    pub fn set_practice(&mut self, mut progress: PracticeProgress) -> Result<PracticeProgress> {
        progress.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        assert_revision(progress.revision, read(&tx)?.unwrap_or_default().revision)?;
        if let Some(question) = &progress.question {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM vocabulary WHERE id=?1)",
                    [&question.card_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if !exists {
                return Err("Словникову картку не знайдено.".into());
            }
        }
        progress.advance()?;
        write(&tx, &progress)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(progress)
    }

    pub fn grade_practice(&mut self, revision: u64, grade: u8) -> Result<PracticeProgress> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut progress = read(&tx)?.unwrap_or_default();
        assert_revision(revision, progress.revision)?;
        let question = progress
            .question
            .take()
            .filter(|question| question.revealed)
            .ok_or("Спочатку відкрийте відповідь.")?;
        Self::record_review(&tx, &question.card_id, &progress.direction, grade)?;
        progress.advance()?;
        write(&tx, &progress)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(progress)
    }
}

pub(crate) fn restore_if_fresh(
    connection: &Connection,
    mut incoming: PracticeProgress,
    card_ids: &BTreeMap<String, String>,
) -> Result<()> {
    incoming.validate()?;
    if read(connection)?.is_some() {
        return Ok(());
    }
    if let Some(question) = &mut incoming.question {
        question.card_id = card_ids
            .get(&question.card_id)
            .ok_or("В архіві немає картки для практики.")?
            .clone();
    }
    incoming.revision = 0;
    incoming.advance()?;
    write(connection, &incoming)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReviewState, Vocabulary};

    #[test]
    fn restart_keeps_choice_order_and_consumes_grade_once() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.sqlite");
        let mut store = Store::open(&path).unwrap();
        let card = store
            .save_word(Vocabulary {
                id: String::new(),
                word: "bonjour".into(),
                translation: "добрий день".into(),
                language: "fr".into(),
                target_language: "uk".into(),
                context: String::new(),
                document_id: String::new(),
                forward: ReviewState::default(),
                reverse: ReviewState::default(),
                analysis: None,
                expression: None,
            })
            .unwrap();
        let options = vec!["merci".into(), "bonjour".into(), "salut".into()];
        let mut progress = store
            .set_practice(PracticeProgress {
                revision: 0,
                direction: "reverse".into(),
                mode: 2,
                question: Some(SavedQuestion {
                    card_id: card.id,
                    prompt: card.translation,
                    answers: vec![card.word],
                    options: options.clone(),
                    revealed: false,
                    given_answer: None,
                }),
            })
            .unwrap();
        assert!(store.grade_practice(progress.revision, 2).is_err());
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let unseen = store.practice().unwrap();
        assert_eq!(unseen.question.unwrap().options, options);
        let question = progress.question.as_mut().unwrap();
        question.revealed = true;
        question.given_answer = Some("merci".into());
        let progress = store.set_practice(progress).unwrap();
        let archive = store.export().unwrap();
        drop(store);

        let mut store = Store::open(&path).unwrap();
        let mut other_window = Store::open(&path).unwrap();
        assert_eq!(
            store
                .practice()
                .unwrap()
                .question
                .unwrap()
                .given_answer
                .as_deref(),
            Some("merci")
        );
        assert!(store.grade_practice(progress.revision, 99).is_err());
        assert_eq!(store.practice().unwrap().revision, progress.revision);
        assert!(store
            .grade_practice(progress.revision, 2)
            .unwrap()
            .question
            .is_none());
        assert!(other_window.grade_practice(progress.revision, 2).is_err());
        let card = &store.snapshot().unwrap().vocabulary[0];
        assert_eq!(card.forward.reviews, 0);
        assert_eq!(card.reverse.reviews, 1);
        assert_eq!(store.export().unwrap().reviews.len(), 1);

        let mut clean = Store::open(&root.path().join("restored.sqlite")).unwrap();
        clean.restore(archive.clone()).unwrap();
        assert_eq!(clean.practice().unwrap().question.unwrap().options, options);
        store.restore(archive).unwrap();
        assert!(store.practice().unwrap().question.is_none());
    }
}
