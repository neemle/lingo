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
            return vec![self.word.clone()];
        }
        std::iter::once(self.translation.clone())
            .chain(self.accepted_translations.iter().cloned())
            .collect()
    }
}

impl Store {
    pub fn set_vocabulary_variants(
        &mut self,
        id: &str,
        accepted_translations: Vec<String>,
    ) -> Result<Vocabulary> {
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
        card.accepted_translations = prepare_variants(&card.translation, accepted_translations)?;
        let body = serde_json::to_string(&card).map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE vocabulary SET body=?1 WHERE id=?2",
            params![body, id],
        )
        .map_err(|error| error.to_string())?;
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
                }),
            })
            .unwrap()
            .question
            .unwrap();
        assert_eq!(question.answers, ["адвокат", "правник", "юрист"]);
        assert!(question.options.is_empty());
        assert_eq!(question.given_answer.as_deref(), Some("авокадо"));

        store.review(&lawyer.id, "forward", 2).unwrap();
        let mut standalone = Store::open(&root.path().join("standalone.db")).unwrap();
        for _ in 0..2 {
            standalone
                .import_dictionary(store.export_dictionary(None).unwrap())
                .unwrap();
        }
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
