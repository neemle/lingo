use super::WordAnalysis;
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(super) enum GrammarCue {
    FrenchDeterminerGender {
        gender: &'static str,
        tag: &'static str,
    },
    UkrainianGenitive,
    UkrainianInfinitive,
    EnglishDeterminerNoun,
}

impl GrammarCue {
    // Only apply local agreement/government where an attested reading agrees.
    // For French un/une see University of Texas, Tex's French Grammar, det4.
    pub(super) fn infer(
        language: &str,
        previous: Option<&str>,
        readings: &[WordAnalysis],
    ) -> Option<Self> {
        let cue = match (language, previous?) {
            ("fr", "un") => Self::FrenchDeterminerGender {
                gender: "Masc",
                tag: "masculine",
            },
            ("fr", "une") => Self::FrenchDeterminerGender {
                gender: "Fem",
                tag: "feminine",
            },
            (
                "uk",
                "біля" | "без" | "від" | "для" | "до" | "після" | "поблизу" | "серед" | "щодо",
            ) => Self::UkrainianGenitive,
            (
                "uk",
                "хочу" | "хочеш" | "хоче" | "хочемо" | "хочете" | "хочуть" | "хотів" | "хотіла"
                | "хотіло" | "хотіли",
            ) if readings
                .iter()
                .filter(|analysis| is_nominal(&analysis.part_of_speech))
                .all(|analysis| {
                    has_feature(&analysis.features, "Case", None)
                        && !["Acc", "Gen"]
                            .iter()
                            .any(|case| has_feature(&analysis.features, "Case", Some(case)))
                }) =>
            {
                Self::UkrainianInfinitive
            }
            // The article could introduce an adjective before the actual noun.
            ("en", "the" | "a" | "an")
                if !readings
                    .iter()
                    .any(|analysis| analysis.part_of_speech == "ADJ") =>
            {
                Self::EnglishDeterminerNoun
            }
            _ => return None,
        };
        readings
            .iter()
            .any(|analysis| cue.supports(analysis))
            .then_some(cue)
    }

    fn supports(self, analysis: &WordAnalysis) -> bool {
        let pos = analysis.part_of_speech.as_str();
        match self {
            Self::FrenchDeterminerGender { gender, .. } => {
                matches!(pos, "NOUN" | "PROPN" | "ADJ")
                    && has_feature(&analysis.features, "Gender", Some(gender))
                    && has_feature(&analysis.features, "Number", Some("Sing"))
            }
            Self::UkrainianGenitive => {
                is_nominal(pos) && has_feature(&analysis.features, "Case", Some("Gen"))
            }
            Self::UkrainianInfinitive => {
                pos == "VERB" && has_feature(&analysis.features, "VerbForm", Some("Inf"))
            }
            Self::EnglishDeterminerNoun => matches!(pos, "NOUN" | "PROPN"),
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::FrenchDeterminerGender { .. } => "frenchDeterminerGender",
            Self::UkrainianGenitive => "ukrainianGenitivePreposition",
            Self::UkrainianInfinitive => "ukrainianInfinitiveComplement",
            Self::EnglishDeterminerNoun => "englishDeterminerNoun",
        }
    }

    pub(super) fn lexical_priority(self, tags: &[String], agrees: bool) -> u8 {
        if !agrees {
            return 0;
        }
        if let Self::FrenchDeterminerGender { tag, .. } = self {
            let known_gender = tags
                .iter()
                .any(|value| matches!(value.as_str(), "masculine" | "feminine"));
            if tags.iter().any(|value| value == tag) {
                return 2;
            }
            if known_gender {
                return 0;
            }
        }
        1
    }
}

pub(super) fn order_readings(readings: &mut Vec<WordAnalysis>, cue: Option<GrammarCue>) {
    if let Some(cue) = cue {
        for reading in readings.iter_mut() {
            reading.grammar_evidence = cue.supports(reading).then_some(cue.code());
        }
        // The input already has a stable order by measured corpus evidence.
        readings.sort_by_key(|reading| reading.grammar_evidence.is_none());
    }
    let mut seen = BTreeSet::new();
    let mut variants = Vec::new();
    // Preserve one representative per lemma/POS before displaying variant morphology.
    for reading in std::mem::take(readings) {
        if seen.insert((reading.lemma.clone(), reading.part_of_speech.clone())) {
            readings.push(reading);
        } else {
            variants.push(reading);
        }
    }
    readings.extend(variants);
    readings.truncate(5);
}

fn has_feature(features: &str, name: &str, expected: Option<&str>) -> bool {
    features.split('|').any(|feature| {
        feature.split_once('=').is_some_and(|(key, values)| {
            key == name
                && expected.is_none_or(|expected| values.split(',').any(|value| value == expected))
        })
    })
}

fn is_nominal(pos: &str) -> bool {
    matches!(pos, "NOUN" | "PROPN" | "ADJ" | "DET" | "NUM" | "PRON")
}
