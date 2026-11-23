use crate::{valid_language, Result, Store, Vocabulary};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

const MAX_VARIANTS: usize = 32;
const MAX_TRANSLATION_BYTES: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SenseSnapshot {
    pub id: String,
    pub lemma: String,
    pub surface: String,
    pub part_of_speech: String,
    pub gloss: String,
    pub gloss_language: String,
    pub source_name: String,
    pub source_url: String,
    pub source_license: String,
    pub source_revision: String,
}

pub(crate) fn sense_id(language: &str, lemma: &str, part_of_speech: &str, gloss: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{language}\0{lemma}\0{part_of_speech}\0{gloss}").as_bytes())
    )
}

impl Vocabulary {
    pub(crate) fn validate_learning(&self) -> Result<()> {
        if let Some(sense) = &self.sense {
            valid_language(&sense.gloss_language)?;
            if sense.id
                != sense_id(
                    &self.language,
                    &sense.lemma,
                    &sense.part_of_speech,
                    &sense.gloss,
                )
                || self.word != sense.lemma
                || self.expression.is_some()
                || self.analysis.as_ref().is_some_and(|analysis| {
                    crate::knowledge::word_key(&analysis.lemma)
                        != crate::knowledge::word_key(&sense.lemma)
                        || analysis.part_of_speech != sense.part_of_speech
                })
                || !sense
                    .source_url
                    .starts_with("https://en.wiktionary.org/wiki/")
            {
                return Err("Некоректне словникове значення картки".into());
            }
            for (value, limit) in [
                (&sense.lemma, 500),
                (&sense.surface, 500),
                (&sense.part_of_speech, 40),
                (&sense.gloss, 2000),
                (&sense.source_name, 160),
                (&sense.source_url, 2048),
                (&sense.source_license, 100),
                (&sense.source_revision, 120),
            ] {
                if value.trim().is_empty()
                    || value.len() > limit
                    || value.chars().any(char::is_control)
                {
                    return Err("Некоректне джерело словникового значення".into());
                }
            }
        }
        prepare_variants(
            &self.translation,
            self.accepted_translations.iter().cloned(),
        )?;
        Ok(())
    }

    pub(crate) fn merge_accepted_translations(&mut self, incoming: &Vocabulary) -> Result<()> {
        let mut candidates = self.accepted_translations.clone();
        if self.sense.is_some() && incoming.sense.is_some() {
            candidates.push(incoming.translation.clone());
        }
        candidates.extend(incoming.accepted_translations.iter().cloned());
        self.accepted_translations = prepare_variants(&self.translation, candidates)?;
        Ok(())
    }

    pub(crate) fn practice_answers(&self, reverse: bool) -> Vec<String> {
        if reverse {
            let mut answers = vec![self.word.clone()];
            let mut seen = BTreeSet::from([answer_key(&self.word)]);
            let forms = [
                self.sense.as_ref().map(|sense| sense.surface.as_str()),
                self.analysis.as_ref().map(|analysis| analysis.surface.as_str()),
                self.expression.as_ref().map(|expression| expression.surface.as_str()),
            ];
            for form in forms.into_iter().flatten().map(str::trim) {
                if crate::review_schedule::valid_answer(form) && seen.insert(answer_key(form)) {
                    answers.push(form.to_owned());
                }
            }
            return answers;
        }
        std::iter::once(self.translation.clone())
            .chain(self.accepted_translations.iter().cloned())
            .collect()
    }
}

impl Store {
    pub fn delete_vocabulary_card(&mut self, id: &str) -> Result<bool> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let removed = tx
            .execute("DELETE FROM vocabulary WHERE id=?1", [id])
            .map_err(|error| error.to_string())?;
        if removed == 0 {
            return Ok(false);
        }
        tx.execute("DELETE FROM review_events WHERE entry_id=?1", [id])
            .map_err(|error| error.to_string())?;
        crate::practice::adjust_after_library_change(&tx, Some(id), true)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(true)
    }

    pub fn set_vocabulary_variants(
        &mut self,
        id: &str,
        accepted_translations: Vec<String>,
    ) -> Result<Vocabulary> {
        self.edit_vocabulary_translation(id, None, accepted_translations)
    }

    pub fn edit_vocabulary_translation(
        &mut self,
        id: &str,
        translation: Option<&str>,
        accepted_translations: Vec<String>,
    ) -> Result<Vocabulary> {
        let translation = translation.map(str::trim);
        if translation.is_some_and(|text| {
            text.is_empty()
                || text.len() > MAX_TRANSLATION_BYTES
                || text
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        }) {
            return Err(
                "Основний переклад має бути одним непорожнім рядком до 2000 байтів.".into(),
            );
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let body: String = tx
            .query_row("SELECT body FROM vocabulary WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or("Словникову картку не знайдено.")?;
        let mut card: Vocabulary =
            serde_json::from_str(&body).map_err(|error| error.to_string())?;
        let word_key =
            if let Some(updated) = translation.filter(|updated| *updated != card.translation) {
                card.translation = updated.to_owned();
                let key = card.word_key();
                let duplicate: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM vocabulary WHERE word_key=?1 AND id<>?2)",
                        params![key, id],
                        |row| row.get(0),
                    )
                    .map_err(|error| error.to_string())?;
                if duplicate {
                    return Err("Картка з таким перекладом уже існує.".into());
                }
                Some(key)
            } else {
                None
            };
        card.accepted_translations = prepare_variants(&card.translation, accepted_translations)?;
        let body = serde_json::to_string(&card).map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE vocabulary SET word_key=COALESCE(?1, word_key), body=?2 WHERE id=?3",
            params![word_key, body, id],
        )
        .map_err(|error| error.to_string())?;
        if word_key.is_some() {
            crate::practice::adjust_after_library_change(&tx, Some(id), false)?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(card)
    }
}

pub(crate) fn prepare_variants(
    primary: &str,
    candidates: impl IntoIterator<Item = String>,
) -> Result<Vec<String>> {
    let mut seen = BTreeSet::from([answer_key(primary)]);
    let mut variants = Vec::new();
    for candidate in candidates {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        if candidate.len() > MAX_TRANSLATION_BYTES || candidate.chars().any(char::is_control) {
            return Err("Варіант перекладу має бути одним рядком до 2000 байтів.".into());
        }
        if seen.insert(answer_key(candidate)) {
            if variants.len() == MAX_VARIANTS {
                return Err("До картки можна додати не більше 32 варіантів.".into());
            }
            variants.push(candidate.to_owned());
        }
    }
    Ok(variants)
}

pub(crate) fn answer_key(value: &str) -> String {
    let folded: String = value
        .nfc()
        .map(|character| match character {
            '’' | 'ʼ' => '\'',
            value => value,
        })
        .flat_map(char::to_lowercase)
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::practice::{PracticeCollection, PracticeProgress, SavedQuestion};
    use crate::ReviewState;

    #[test]
    fn deleting_cards_cleans_history_and_only_affected_practice() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reader.db");
        let mut store = Store::open(&path).unwrap();
        let document = store.import("fixture.txt", b"Le livre", "fr").unwrap();
        let entry = |word: &str, source: &str| Vocabulary {
            id: String::new(),
            word: word.into(),
            translation: "книга".into(),
            language: "fr".into(),
            target_language: "uk".into(),
            context: String::new(),
            document_id: source.into(),
            forward: ReviewState::default(),
            reverse: ReviewState::default(),
            analysis: None,
            expression: None,
            sense: None,
            accepted_translations: Vec::new(),
        };
        let active = store.save_word(entry("livre", "legacy-source")).unwrap();
        let other = store.save_word(entry("ouvrage", &document.id)).unwrap();
        let linked: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM dictionary_card_sources WHERE card_id=?1 AND document_id=?2)",
                rusqlite::params![other.id, document.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(linked);
        for card in [&active, &other] {
            store.review(&card.id, "reverse", 2).unwrap();
        }
        store.connection.execute(
            "INSERT INTO dictionary_review_origins(origin,event_id) SELECT 'fixture',id FROM review_events WHERE entry_id=?1",
            [&other.id],
        ).unwrap();
        let progress = store
            .set_practice(PracticeProgress {
                revision: 0,
                direction: "reverse".into(),
                mode: 1,
                question: Some(SavedQuestion {
                    card_id: active.id.clone(),
                    prompt: active.translation.clone(),
                    answers: vec![active.word.clone()],
                    options: Vec::new(),
                    revealed: true,
                    given_answer: Some(active.word.clone()),
                    elapsed_ms: Some(500),
                }),
                collection: Some(PracticeCollection {
                    document_id: "legacy-source".into(),
                    source_hash: None,
                }),
            })
            .unwrap();

        assert!(store.delete_vocabulary_card(&other.id).unwrap());
        assert!(store
            .vocabulary_review_history(&other.id)
            .unwrap()
            .is_empty());
        assert_eq!(
            store.vocabulary_review_history(&active.id).unwrap().len(),
            1
        );
        for sql in [
            "SELECT count(*) FROM dictionary_review_origins",
            "SELECT count(*) FROM dictionary_card_sources",
        ] {
            assert_eq!(
                store
                    .connection
                    .query_row(sql, [], |row| row.get::<_, u32>(0))
                    .unwrap(),
                0
            );
        }
        let unchanged = store.practice().unwrap();
        assert_eq!(unchanged.revision, progress.revision);
        assert_eq!(unchanged.question.unwrap().card_id, active.id);

        assert!(store.delete_vocabulary_card(&active.id).unwrap());
        assert!(!store.delete_vocabulary_card(&active.id).unwrap());
        assert!(store
            .vocabulary_review_history(&active.id)
            .unwrap()
            .is_empty());
        drop(store);
        let store = Store::open(&path).unwrap();
        let reset = store.practice().unwrap();
        assert_eq!(reset.revision, progress.revision + 1);
        assert_eq!(reset.direction, progress.direction);
        assert_eq!(reset.mode, progress.mode);
        assert!(reset.question.is_none() && reset.collection.is_none());
        assert!(store.snapshot().unwrap().vocabulary.is_empty());
    }

    #[test]
    fn correcting_translation_keeps_card_and_replays_old_archive_without_reverting() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("reader.db")).unwrap();
        let entry = |translation: &str| Vocabulary {
            id: String::new(),
            word: "fenêtre".into(),
            translation: translation.into(),
            language: "fr".into(),
            target_language: "uk".into(),
            context: String::new(),
            document_id: String::new(),
            forward: crate::ReviewState::default(),
            reverse: crate::ReviewState::default(),
            analysis: None,
            expression: None,
            sense: None,
            accepted_translations: Vec::new(),
        };
        let card = store.save_word(entry("двері")).unwrap();
        store.review(&card.id, "forward", 2).unwrap();
        let practice = store
            .set_practice(PracticeProgress {
                revision: 0,
                direction: "reverse".into(),
                mode: 0,
                question: Some(SavedQuestion {
                    card_id: card.id.clone(),
                    prompt: card.translation.clone(),
                    answers: vec![card.word.clone()],
                    options: vec![],
                    revealed: true,
                    given_answer: None,
                    elapsed_ms: None,
                }),
                collection: None,
            })
            .unwrap();
        let old_archive = store.export_dictionary(None).unwrap();

        let corrected = store
            .edit_vocabulary_translation(
                &card.id,
                Some("  вікно  "),
                vec!["ВІКНО".into(), "віконце".into()],
            )
            .unwrap();
        assert_eq!(corrected.id, card.id);
        assert_eq!(corrected.translation, "вікно");
        assert_eq!(corrected.accepted_translations, ["віконце"]);
        assert_eq!(corrected.forward.reviews, 1);
        let updated = store.practice().unwrap();
        assert!(updated.question.is_none());
        assert_eq!(updated.revision, practice.revision + 1);
        assert_eq!(updated.direction, practice.direction);

        let other = store.save_word(entry("двері")).unwrap();
        assert_ne!(other.id, card.id);
        let error = store
            .edit_vocabulary_translation(&card.id, Some("двері"), vec![])
            .unwrap_err();
        assert_eq!(error, "Картка з таким перекладом уже існує.");
        let restored = store.import_dictionary(old_archive).unwrap();
        assert_eq!(restored.added_cards, 0);
        assert_eq!(restored.added_reviews, 0);
        let cards = store.snapshot().unwrap().vocabulary;
        let current = cards.iter().find(|item| item.id == card.id).unwrap();
        assert_eq!(current.translation, "вікно");
        assert_eq!(current.accepted_translations, ["віконце"]);
        assert_eq!(current.forward.reviews, 1);
        assert_eq!(store.vocabulary_review_history(&other.id).unwrap().len(), 0);
    }

    #[test]
    fn vocabulary_sense_and_approved_variants_survive_practice_and_transfer() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(&root.path().join("original.db")).unwrap();
        let first = store
            .import("first.txt", b"Il connait des avocats.", "fr")
            .unwrap();
        let second = store
            .import("second.txt", b"Elle parle aux avocats.", "fr")
            .unwrap();
        let entry = |gloss: &str, translation: &str, document: &crate::Document| Vocabulary {
            id: String::new(),
            word: "avocats".into(),
            translation: translation.into(),
            language: "fr".into(),
            target_language: "uk".into(),
            context: document.text.clone(),
            document_id: document.id.clone(),
            forward: ReviewState::default(),
            reverse: ReviewState::default(),
            analysis: None,
            expression: None,
            sense: Some(Box::new(SenseSnapshot {
                id: sense_id("fr", "avocat", "NOUN", gloss),
                lemma: "avocat".into(),
                surface: "avocats".into(),
                part_of_speech: "NOUN".into(),
                gloss: gloss.into(),
                gloss_language: "en".into(),
                source_name: "English Wiktionary via Kaikki/Wiktextract".into(),
                source_url: "https://en.wiktionary.org/wiki/avocat#French".into(),
                source_license: "CC BY-SA 4.0".into(),
                source_revision: "fixture".into(),
            })),
            accepted_translations: Vec::new(),
        };
        let lawyer = store.save_word(entry("lawyer", "адвокат", &first)).unwrap();
        let fruit = store
            .save_word(entry("avocado", "авокадо", &first))
            .unwrap();
        let mut legacy = entry("lawyer", "адвокат", &first);
        legacy.sense = None;
        legacy.word = "avocat".into();
        let legacy = store.save_word(legacy).unwrap();
        assert_ne!(lawyer.id, fruit.id);
        assert_ne!(lawyer.id, legacy.id);
        assert_eq!(lawyer.sense.as_ref().unwrap().surface, "avocats");

        let lawyer = store
            .set_vocabulary_variants(&lawyer.id, vec![" правник ".into(), "ПРАВНИК".into()])
            .unwrap();
        let repeated = store.save_word(entry("lawyer", "юрист", &second)).unwrap();
        assert_eq!(repeated.id, lawyer.id);
        assert_eq!(repeated.accepted_translations, ["правник", "юрист"]);
        let collection = store.dictionary_collection(Some(&second.id)).unwrap();
        assert_eq!(collection.len(), 1);
        assert_eq!(collection[0].id, lawyer.id);

        let question = store
            .set_practice(PracticeProgress {
                revision: 0,
                direction: "forward".into(),
                mode: 2,
                collection: Some(PracticeCollection {
                    document_id: second.id.clone(),
                    source_hash: None,
                }),
                question: Some(SavedQuestion {
                    card_id: lawyer.id.clone(),
                    prompt: lawyer.word.clone(),
                    answers: vec![lawyer.translation.clone(), fruit.translation.clone()],
                    options: vec![lawyer.translation.clone(), "правник".into(), "друге".into()],
                    revealed: true,
                    given_answer: Some("авокадо".into()),
                    elapsed_ms: None,
                }),
            })
            .unwrap()
            .question
            .unwrap();
        assert_eq!(question.answers, ["адвокат", "правник", "юрист"]);
        assert!(question.options.is_empty());
        assert_eq!(question.given_answer.as_deref(), Some("авокадо"));

        store.review(&lawyer.id, "forward", 2).unwrap();
        let archive = store.export_dictionary(None).unwrap();
        let json = serde_json::to_value(&archive).unwrap();
        assert!(json.get("practice").is_some());
        let filtered = store.export_dictionary(Some(&second.id)).unwrap();
        let legacy_json = serde_json::to_value(filtered).unwrap();
        assert!(legacy_json.get("practice").is_none());
        let mut standalone = Store::open(&root.path().join("standalone.db")).unwrap();
        assert!(
            !standalone
                .import_dictionary(serde_json::from_value(legacy_json).unwrap())
                .unwrap()
                .restored_practice
        );
        assert!(
            standalone
                .import_dictionary(archive.clone())
                .unwrap()
                .restored_practice
        );
        assert!(
            !standalone
                .import_dictionary(archive.clone())
                .unwrap()
                .restored_practice
        );
        assert_eq!(
            standalone.practice().unwrap().question.unwrap().card_id,
            lawyer.id
        );

        let mut duplicate = Store::open(&root.path().join("duplicate.db")).unwrap();
        let local_document = duplicate
            .import("local.txt", b"Elle parle aux avocats.", "fr")
            .unwrap();
        let local_card = duplicate
            .save_word(entry("lawyer", "адвокат", &local_document))
            .unwrap();
        assert_ne!(local_card.id, lawyer.id);
        assert_ne!(local_document.id, second.id);
        assert!(
            duplicate
                .import_dictionary(archive)
                .unwrap()
                .restored_practice
        );
        let resumed = duplicate.practice().unwrap();
        assert_eq!(resumed.collection.unwrap().document_id, local_document.id);
        let resumed = resumed.question.unwrap();
        assert_eq!(resumed.card_id, local_card.id);
        assert_eq!(resumed.given_answer.as_deref(), Some("авокадо"));
        let entries = standalone.dictionary_collection(None).unwrap();
        assert_eq!(entries.len(), 3);
        let lawyer = entries.iter().find(|card| card.id == lawyer.id).unwrap();
        assert_eq!(lawyer.accepted_translations, ["правник", "юрист"]);
        assert_eq!(lawyer.forward.reviews, 1);
        let fruit = entries.iter().find(|card| card.id == fruit.id).unwrap();
        assert_eq!(fruit.sense.as_ref().unwrap().gloss, "avocado");
        assert!(fruit.accepted_translations.is_empty());
        assert_eq!(fruit.forward.reviews, 0);

        let mut restored = Store::open(&root.path().join("restore.db")).unwrap();
        restored.restore(store.export().unwrap()).unwrap();
        let practice = restored.practice().unwrap();
        assert_eq!(
            practice.question.unwrap().answers,
            ["адвокат", "правник", "юрист"]
        );
        assert_eq!(
            restored.dictionary_collection(Some(&second.id)).unwrap()[0].id,
            lawyer.id
        );
    }
}
