use crate::{card_learning::answer_key, dictionary_transfer, Result, Store, Vocabulary};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeCollection {
    pub document_id: String,
    pub source_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PracticeSettings {
    direction: String,
    mode: u8,
    collection: Option<PracticeCollection>,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeQueue {
    pub progress: PracticeProgress,
    pub due_count: usize,
    pub total_cards: usize,
    pub matches_dictionary: Option<bool>,
    pub context: Option<String>,
}

impl SavedQuestion {
    fn dictionary_match(&self) -> Option<bool> {
        let answer = answer_key(self.given_answer.as_deref()?);
        Some(self.answers.iter().any(|candidate| answer_key(candidate) == answer))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeAnswer {
    pub progress: PracticeProgress,
    pub matches_dictionary: Option<bool>,
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
    question.answers = question_answers(connection, &card, reverse)?;
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

pub(crate) fn adjust_after_library_change(
    connection: &Connection,
    card_id: Option<&str>,
    check_collection: bool,
) -> Result<()> {
    let body: Option<String> = connection
        .query_row(
            "SELECT body FROM practice WHERE id=1 AND
             (json_extract(body, '$.question.cardId')=?1 OR (?2 AND json_type(body, '$.collection')='object'))",
            rusqlite::params![card_id, check_collection],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some(body) = body else {
        return Ok(());
    };
    let mut progress: PracticeProgress =
        serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let question_removed = progress
        .question
        .as_ref()
        .is_some_and(|question| Some(question.card_id.as_str()) == card_id);
    if question_removed {
        progress.question = None;
    }
    let mut collection_removed = false;
    if let Some(collection) = progress.collection.as_ref().filter(|_| check_collection) {
        if dictionary_transfer::find_collection(
            connection,
            &collection.document_id,
            collection.source_hash.as_deref(),
        )?
        .is_none()
        {
            progress.collection = None;
            collection_removed = true;
        }
    }
    if question_removed || collection_removed {
        progress.advance()?;
        write(connection, &progress)?;
    }
    Ok(())
}

fn assert_revision(expected: u64, actual: u64) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err("Практику вже оновлено. Відкрийте поточне запитання.".into())
    }
}

fn prompt(card: &Vocabulary, reverse: bool) -> &str {
    if reverse {
        &card.translation
    } else {
        &card.word
    }
}

fn matches_prompt(card: &Vocabulary, reverse: bool, normalized: &str) -> bool {
    answer_key(prompt(card, reverse)) == normalized
        || reverse && card.accepted_translations.iter().any(|variant| answer_key(variant) == normalized)
}

fn question_answers(db: &Connection, card: &Vocabulary, reverse: bool) -> Result<Vec<String>> {
    let mut answers = card.practice_answers(reverse);
    if !reverse {
        return Ok(answers);
    }
    let key = answer_key(&card.translation);
    let mut seen: BTreeSet<_> = answers.iter().map(|answer| answer_key(answer)).collect();
    let mut query = db.prepare(
        "SELECT body FROM vocabulary WHERE id<>?1
         AND json_extract(body, '$.language')=?2 AND json_extract(body, '$.targetLanguage')=?3
         ORDER BY id",
    ).map_err(|error| error.to_string())?;
    let rows = query.query_map(params![card.id, card.language, card.target_language], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    for body in rows {
        let other: Vocabulary = serde_json::from_str(&body.map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        if matches_prompt(&other, true, &key) {
            answers.extend(other.practice_answers(true).into_iter().filter(|answer| seen.insert(answer_key(answer))));
        }
    }
    Ok(answers)
}

fn choices(card: &Vocabulary, candidates: &[Vocabulary], reverse: bool) -> Vec<String> {
    let same_pair = |other: &&Vocabulary| {
        card.language == other.language && card.target_language == other.target_language
    };
    let key = answer_key(prompt(card, reverse));
    let could_answer = |other: &&Vocabulary| matches_prompt(other, reverse, &key);
    // Another sense with the same prompt may also be a valid answer.
    let mut excluded: BTreeSet<_> = candidates
        .iter()
        .filter(same_pair)
        .filter(could_answer)
        .flat_map(|other| other.practice_answers(reverse))
        .map(|answer| answer_key(&answer))
        .collect();
    let correct = if reverse { &card.word } else { &card.translation };
    excluded.insert(answer_key(correct));
    let mut options: Vec<_> = candidates
        .iter()
        .filter(same_pair)
        .filter(|other| !could_answer(other))
        .map(|other| if reverse { &other.word } else { &other.translation })
        .filter(|answer| !answer.trim().is_empty() && excluded.insert(answer_key(answer)))
        .take(3)
        .cloned()
        .collect();
    if options.len() < 2 {
        return Vec::new();
    }
    options.push(correct.clone());
    options.sort_by_cached_key(|_| uuid::Uuid::new_v4());
    options
}

impl Store {
    pub fn practice(&self) -> Result<PracticeProgress> {
        Ok(read(&self.connection)?.unwrap_or_default())
    }

    /// Select a due card in the saved direction and collection, or resume the open question.
    pub fn next_practice_question(&mut self, revision: u64) -> Result<PracticeQueue> {
        self.practice_queue(revision, None)
    }

    pub fn practice_queue(&mut self, revision: u64, settings: Option<PracticeSettings>) -> Result<PracticeQueue> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut progress = read(&tx)?.unwrap_or_default();
        assert_revision(revision, progress.revision)?;
        let mut changed = false;
        if let Some(settings) = settings {
            let mut proposed = PracticeProgress {
                revision, direction: settings.direction, mode: settings.mode,
                question: None, collection: settings.collection,
            };
            proposed.validate()?;
            align_question(&tx, &mut proposed)?;
            if proposed.direction != progress.direction || proposed.mode != progress.mode
                || proposed.collection != progress.collection
            {
                progress = proposed;
                changed = true;
            }
        }
        let collection = progress
            .collection
            .as_ref()
            .map(|scope| scope.document_id.as_str());
        let mut cards = dictionary_transfer::collection_cards(&tx, collection, None)?;
        let reverse = progress.direction == "reverse";
        let due_at = |card: &Vocabulary| {
            if reverse { card.reverse.due_at } else { card.forward.due_at }
        };
        let now = crate::now();
        let due = cards.iter().filter(|card| due_at(card) <= now);
        let due_count = due.clone().count();
        if progress.question.is_none() {
            // Oldest scheduled reviews come before new cards in this direction.
            if let Some(card) = due.min_by_key(|card| (due_at(card) == 0, due_at(card))) {
                let options = if progress.mode == 2 {
                    if collection.is_some() {
                        let vocabulary = dictionary_transfer::collection_cards(&tx, None, None)?;
                        choices(card, &vocabulary, reverse)
                    } else {
                        choices(card, &cards, reverse)
                    }
                } else {
                    Vec::new()
                };
                progress.question = Some(SavedQuestion {
                    card_id: card.id.clone(),
                    prompt: prompt(card, reverse).to_owned(),
                    answers: question_answers(&tx, card, reverse)?,
                    options,
                    revealed: false,
                    given_answer: None,
                    elapsed_ms: None,
                });
                changed = true;
            }
        }
        let mut context = None;
        if !reverse {
            if let Some(card) = progress.question.as_ref()
                .and_then(|question| cards.iter_mut().find(|card| card.id == question.card_id))
            {
                if let Some(id) = collection {
                    dictionary_transfer::apply_collection_contexts(&tx, id, std::slice::from_mut(card))?;
                }
                if !card.context.trim().is_empty() {
                    context = Some(card.context.clone());
                }
            }
        }
        if changed {
            progress.advance()?;
            write(&tx, &progress)?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(PracticeQueue {
            matches_dictionary: progress.question.as_ref().and_then(SavedQuestion::dictionary_match),
            progress,
            due_count,
            total_cards: cards.len(),
            context,
        })
    }

    /// Reveal and persist the answer once for the current question.
    pub fn answer_practice_question(
        &mut self,
        revision: u64,
        answer: Option<&str>,
        elapsed_ms: Option<u32>,
    ) -> Result<PracticeAnswer> {
        let answer = answer.map(str::trim);
        if answer.is_some_and(|text| text.is_empty() || !crate::review_schedule::valid_answer(text))
            || elapsed_ms.is_some_and(|ms| ms > crate::review_schedule::MAX_ELAPSED_MS)
        {
            return Err("Некоректна відповідь або час відповіді.".into());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let mut progress = read(&tx)?.unwrap_or_default();
        assert_revision(revision, progress.revision)?;
        let question = progress
            .question
            .as_mut()
            .filter(|question| !question.revealed)
            .ok_or("Немає відкритого запитання.")?;
        let normalized = answer.map(answer_key);
        let invalid_choice = !question.options.is_empty()
            && !question.options.iter().any(|option| {
                normalized.as_ref().is_some_and(|value| answer_key(option) == *value)
            });
        if progress.mode != 0 && normalized.is_none() || invalid_choice {
            return Err("Введіть відповідь або виберіть один із варіантів.".into());
        }
        question.revealed = true;
        question.given_answer = answer.map(str::to_owned);
        question.elapsed_ms = elapsed_ms;
        let matches_dictionary = question.dictionary_match();
        progress.advance()?;
        write(&tx, &progress)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(PracticeAnswer {
            progress,
            matches_dictionary,
        })
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
    fn reverse_practice_accepts_and_retains_the_observed_form() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let mut store = Store::open(&path).unwrap();
        let card = store.save_word(Vocabulary {
            id: String::new(), word: "livres".into(), translation: "книга".into(),
            language: "fr".into(), target_language: "uk".into(),
            context: "Elle lit des livres.".into(), document_id: String::new(),
            forward: ReviewState::default(), reverse: ReviewState::default(),
            analysis: Some(Box::new(crate::LemmaSnapshot {
                lemma: "livre".into(), surface: "livres".into(), part_of_speech: "NOUN".into(),
                features: "Number=Plur".into(), source: "fixture".into(), source_version: "1".into(),
                context: None, document_id: None,
            })),
            expression: None, sense: None, accepted_translations: Vec::new(),
        }).unwrap();
        let vocabulary = [card.clone(), Vocabulary { word: "livres".into(), translation: "томи".into(), ..card.clone() },
            Vocabulary { word: "bateau".into(), translation: "човен".into(), ..card.clone() },
            Vocabulary { word: "port".into(), translation: "порт".into(), ..card.clone() }];
        let options = choices(&card, &vocabulary, true);
        assert_eq!(options.len(), 3);
        assert!(!options.iter().any(|option| option == "livres"));

        let progress = store.practice_queue(0, Some(PracticeSettings {
            direction: "reverse".into(), mode: 1, collection: None,
        })).unwrap().progress;
        assert_eq!(progress.question.unwrap().answers, ["livre", "livres"]);
        drop(store);
        let mut store = Store::open(&path).unwrap();
        let answer = store.answer_practice_question(progress.revision, Some("LIVRES"), None).unwrap();
        assert_eq!(answer.matches_dictionary, Some(true));
        store.grade_practice(answer.progress.revision, 2).unwrap();
        let saved = store.snapshot().unwrap().vocabulary.pop().unwrap();
        assert_eq!(saved.forward.reviews, 0);
        assert_eq!(saved.reverse.reviews, 1);
    }

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
