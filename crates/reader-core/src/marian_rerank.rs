use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Feature {
    Word(String),
    Pair(String, String),
}

fn features(text: &str) -> BTreeSet<Feature> {
    let words: Vec<String> = text
        .unicode_words()
        .map(|word| word.nfkc().flat_map(char::to_lowercase).collect())
        .collect();
    words
        .iter()
        .cloned()
        .map(Feature::Word)
        .chain(
            words
                .windows(2)
                .map(|pair| Feature::Pair(pair[0].clone(), pair[1].clone())),
        )
        .collect()
}

pub(super) fn supported_reference(candidates: &[String], reference: &str) -> bool {
    let choices: Vec<_> = candidates.iter().map(|text| features(text)).collect();
    features(reference).iter().any(|feature| {
        let count = choices
            .iter()
            .filter(|choice| choice.contains(feature))
            .count();
        count > 0 && count < choices.len()
    })
}

/// Select an already-contextual hypothesis; independent references are evidence only.
pub(super) fn choose(candidates: &[String], references: &[String]) -> usize {
    if !(2..=8).contains(&candidates.len()) || !(2..=3).contains(&references.len()) {
        return 0;
    }
    let choices: Vec<_> = candidates.iter().map(|text| features(text)).collect();
    let references: Vec<_> = references.iter().map(|text| features(text)).collect();
    let mut scores = Vec::with_capacity(choices.len());
    for (index, choice) in choices.iter().enumerate() {
        let mut total = 0.0;
        let mut minimum = 1.0f64;
        let mut supported_pairs = 0;
        for reference in &references {
            let mut possible = 0.0;
            let mut matched = 0.0;
            for feature in reference {
                let count = choices
                    .iter()
                    .filter(|candidate| candidate.contains(feature))
                    .count();
                let weight = ((choices.len() + 1) as f64 / (count + 1) as f64).ln()
                    * if matches!(feature, Feature::Pair(..)) {
                        2.0
                    } else {
                        1.0
                    };
                possible += weight;
                if choice.contains(feature) {
                    matched += weight;
                    if weight > 0.0 && matches!(feature, Feature::Pair(..)) {
                        supported_pairs += 1;
                    }
                }
            }
            if possible <= f64::EPSILON {
                return 0;
            }
            let coverage = matched / possible;
            total += coverage;
            minimum = minimum.min(coverage);
        }
        scores.push((
            index,
            total / references.len() as f64,
            minimum,
            supported_pairs,
        ));
    }
    let baseline = scores[0].1;
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let best = scores[0];
    if best.0 != 0
        && best.1 >= baseline + 0.20
        && best.1 >= scores[1].1 + 0.025
        && best.2 >= 0.10
        && best.3 >= 2
    {
        best.0
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_promotes_a_contextual_candidate_with_balanced_discriminating_evidence() {
        let candidates = [
            "On lit un livre, le soleil s'assoie.",
            "Nous lisons un livre, le soleil s'assoie.",
            "On lit un livre, le soleil se couche.",
            "Nous lisons le livre, le soleil s'assoie.",
        ]
        .map(str::to_owned);
        let references = ["Nous lisons un livre.", "Le soleil se couche."].map(str::to_owned);
        assert_eq!(choose(&candidates, &references), 2);
        assert!(!supported_reference(&candidates, "Le soleil s'assombrit."));
        assert_eq!(
            choose(
                &candidates,
                &[references[0].clone(), "Une autre observation.".into()]
            ),
            0
        );
    }
    #[test]
    fn rain_control_retains_both_events_from_a_contextual_alternative() {
        let candidates = [
            "Elle a pris son parapluie, il a plu.",
            "Elle a pris un parapluie, il a plu.",
            "Elle a pris son parapluie, il a commencé à pleuvoir.",
            "Elle a pris un parapluie, il a commencé à pleuvoir.",
        ]
        .map(str::to_owned);
        assert_eq!(
            choose(
                &candidates,
                &[
                    "Elle a pris un parapluie.".into(),
                    "Il a commencé à pleuvoir.".into()
                ]
            ),
            3
        );
    }

    #[test]
    fn tied_or_nondiscriminating_references_leave_model_order_unchanged() {
        let candidates = ["Il pleut.", "Il pleut !"].map(str::to_owned);
        assert_eq!(
            choose(&candidates, &["Il pleut.".into(), "Il pleut.".into()]),
            0
        );
    }
}
