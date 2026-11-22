use crate::{card_learning::answer_key, dictionary_transfer, Result, Store};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<PracticeCollection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeCollection {
    pub document_id: String,
    pub source_hash: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u32>,
}

impl Default for PracticeProgress {
    fn default() -> Self {
        Self {
            revision: 0,
            direction: "forward".into(),
            mode: 0,
            question: None,
            collection: None,
        }
    }
}

impl PracticeProgress {
    fn validate(&self) -> Result<()> {
        if !["forward", "reverse"].contains(&self.direction.as_str()) || self.mode > 2 {
            return Err("Некоректні налаштування практики.".into());
        }
        if self.collection.as_ref().is_some_and(|collection| {
            collection.document_id.trim().is_empty() || collection.document_id.len() > 256
        }) {
            return Err("Некоректна добірка для практики.".into());
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
                || question.elapsed_ms.is_some_and(|elapsed| {
                    !question.revealed || elapsed > crate::review_schedule::MAX_ELAPSED_MS
                })
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
    body.map(|body| {
        let mut progress = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        align_question(connection, &mut progress)?;
        Ok(progress)
    })
    .transpose()
}

fn align_question(connection: &Connection, progress: &mut PracticeProgress) -> Result<()> {
    if let Some(collection) = &mut progress.collection {
        let (id, hash) = dictionary_transfer::resolve_collection(
            connection,
            &collection.document_id,
            collection.source_hash.as_deref(),
        )?;
        collection.document_id = id;
        collection.source_hash = hash;
    }

    let Some(question) = &mut progress.question else {
        return Ok(());
    };
    let document_id = progress
        .collection
        .as_ref()
        .map(|collection| collection.document_id.as_str());
    let card =
        dictionary_transfer::collection_cards(connection, document_id, Some(&question.card_id))?
            .pop()
            .ok_or(if document_id.is_some() {
                "Словникова картка не входить до вибраної добірки."
            } else {
                "Словникову картку не знайдено."
            })?;
    let reverse = progress.direction == "reverse";
    question.answers = card.practice_answers(reverse);
    question.prompt = if reverse { card.translation } else { card.word };
    let correct_options = question
        .options
        .iter()
        .filter(|option| {
            let key = answer_key(option);
            question
                .answers
                .iter()
                .any(|answer| answer_key(answer) == key)
        })
        .count();
    if correct_options != 1 {
        question.options.clear();
    }
    Ok(())
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
        if progress
            .question
            .as_ref()
            .and_then(|question| question.given_answer.as_deref())
            .is_some_and(|answer| !crate::review_schedule::valid_answer(answer))
        {
            return Err("Відповідь перевищує 2000 байтів.".into());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        assert_revision(progress.revision, read(&tx)?.unwrap_or_default().revision)?;
        align_question(&tx, &mut progress)?;
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
        Self::record_review(
            &tx,
            &question.card_id,
            &progress.direction,
            grade,
            // A question persisted by an older build may contain an unbounded answer.
            question
                .given_answer
                .as_deref()
                .filter(|answer| crate::review_schedule::valid_answer(answer)),
            question.elapsed_ms,
        )?;
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
) -> Result<bool> {
    incoming.validate()?;
    if read(connection)?.is_some() {
        return Ok(false);
    }
    if let Some(question) = &mut incoming.question {
        question.card_id = card_ids
            .get(&question.card_id)
            .ok_or("В архіві немає картки для практики.")?
            .clone();
    }
    align_question(connection, &mut incoming)?;
    incoming.revision = 0;
    incoming.advance()?;
    write(connection, &incoming)?;
    Ok(true)
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
                sense: None,
                accepted_translations: Vec::new(),
            })
            .unwrap();
        let card_id = card.id.clone();
        let options = vec!["merci".into(), "bonjour".into(), "salut".into()];
        let mut progress = store
            .set_practice(PracticeProgress {
                revision: 0,
                direction: "reverse".into(),
                mode: 2,
                collection: None,
                question: Some(SavedQuestion {
                    card_id: card.id,
                    prompt: card.translation,
                    answers: vec![card.word],
                    options: options.clone(),
                    revealed: false,
                    given_answer: None,
                    elapsed_ms: None,
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
        question.elapsed_ms = Some(1250);
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
        let completed = store.export().unwrap();
        assert_eq!(completed.reviews.len(), 1);
        let review = store
            .vocabulary_review_history(&card_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(review.answer.as_deref(), Some("merci"));
        assert_eq!(review.elapsed_ms, Some(1250));

        let mut full = Store::open(&root.path().join("completed.sqlite")).unwrap();
        full.restore(completed).unwrap();
        let restored = full
            .vocabulary_review_history(&card_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(restored.answer, review.answer);
        assert_eq!(restored.elapsed_ms, review.elapsed_ms);

        let detailed = store.export_dictionary(None).unwrap();
        let origin = detailed.reviews[0].id.clone();
        let mut legacy = detailed.clone();
        legacy.reviews[0].event.answer = None;
        legacy.reviews[0].event.elapsed_ms = None;
        let mut dictionary = Store::open(&root.path().join("dictionary.sqlite")).unwrap();
        assert_eq!(
            dictionary
                .import_dictionary(legacy.clone())
                .unwrap()
                .added_reviews,
            1
        );
        assert_eq!(
            dictionary
                .import_dictionary(detailed)
                .unwrap()
                .added_reviews,
            0
        );
        assert_eq!(
            dictionary.import_dictionary(legacy).unwrap().added_reviews,
            0
        );
        let upgraded = dictionary.export_dictionary(None).unwrap();
        assert_eq!(upgraded.reviews[0].id, origin);
        assert_eq!(upgraded.reviews[0].event.answer, review.answer);
        assert_eq!(upgraded.reviews[0].event.elapsed_ms, review.elapsed_ms);

        let mut clean = Store::open(&root.path().join("restored.sqlite")).unwrap();
        clean.restore(archive.clone()).unwrap();
        assert_eq!(clean.practice().unwrap().question.unwrap().options, options);
        store.restore(archive).unwrap();
        assert!(store.practice().unwrap().question.is_none());
    }
}
